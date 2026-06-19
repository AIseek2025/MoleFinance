//! Byte-equivalence property tests for the on-chain dormant bridge.
//!
//! Drives a [`DormantStore`] through a long random op stream while
//! continuously round-tripping it through
//! [`pack_dormant_store`] and [`unpack_dormant_store`]. After every
//! op we assert:
//!
//! 1. **Pack/unpack/pack idempotence** — the second pack output is
//!    byte-identical to the first. Combined with bytemuck's POD
//!    layout (which the Anchor program will use for zero-copy), this
//!    proves the on-chain bytes stored in the account are unique
//!    given the engine state.
//!
//! 2. **Behavioural equivalence** — replaying the next op on an
//!    `unpack(pack(store))` clone yields the same observable state
//!    as running the same op on the original. The host engine's
//!    snapshot/restore (in `protocol-harness`) and the on-chain
//!    Solana program (which IS pack/unpack on every tx) MUST be
//!    indistinguishable downstream.
//!
//! These tests are the foundation contract phase 2 of the bridge
//! (Anchor wiring) builds on top of: any divergence between host and
//! on-chain bytes will trip a property test before the program is
//! built.

use clearing_core::{
    pack_dormant_store, unpack_dormant_store, ClearingError, Direction, DormantStore,
    OnChainBucketRecord, OnChainLedger,
};
use molemath::PRICE_SCALE;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

const MAX_BUCKETS: u32 = 64;
const MAX_LEDGER: u32 = 256;

#[derive(Debug, Clone, Copy)]
enum Op {
    Insert {
        tick: i64,
        anchor_price: u64,
        added_shares: u128,
        added_notional: u128,
    },
    DistributeEager {
        p_now: u64,
        total_alloc: u128,
    },
    DistributeLazy {
        p_now: u64,
        total_alloc: u128,
    },
    Redeem {
        tick: i64,
        shares_to_burn: u128,
    },
    ApplyPending {
        tick: i64,
    },
    Compact,
}

fn generate_op(rng: &mut ChaCha20Rng) -> Op {
    let r = rng.gen_range(0..100);
    if r < 25 {
        let tick = rng.gen_range(50..150) as i64;
        let anchor = (tick as u64).saturating_mul(PRICE_SCALE);
        Op::Insert {
            tick,
            anchor_price: anchor.max(PRICE_SCALE),
            added_shares: rng.gen_range(100..1_000_000),
            added_notional: rng.gen_range(10_000..100_000_000),
        }
    } else if r < 45 {
        Op::DistributeEager {
            p_now: (rng.gen_range(50u64..150u64)).saturating_mul(PRICE_SCALE),
            total_alloc: rng.gen_range(0..1_000_000),
        }
    } else if r < 65 {
        Op::DistributeLazy {
            p_now: (rng.gen_range(50u64..150u64)).saturating_mul(PRICE_SCALE),
            total_alloc: rng.gen_range(0..1_000_000),
        }
    } else if r < 80 {
        Op::Redeem {
            tick: rng.gen_range(50..150) as i64,
            shares_to_burn: rng.gen_range(1..200_000),
        }
    } else if r < 95 {
        Op::ApplyPending {
            tick: rng.gen_range(50..150) as i64,
        }
    } else {
        Op::Compact
    }
}

/// Returns Σ `entry.allocated_sum_observed` across the live ledger
/// window. Equal to the upper bound on `pending_distribution_total`.
fn live_alloc_sum(store: &DormantStore) -> u128 {
    store.ledger().iter().map(|e| e.allocated_sum_observed).sum()
}

/// Apply `op` to `store`. Best-effort; ops that the engine rejects
/// (capacity, missing bucket, redeem-too-much) are silently dropped.
/// Returns `true` if state was mutated, `false` otherwise. The
/// callers don't actually need the mutated bit; it's here for
/// future tightening.
fn apply_op(store: &mut DormantStore, op: Op) -> bool {
    match op {
        Op::Insert {
            tick,
            anchor_price,
            added_shares,
            added_notional,
        } => store
            .insert_or_merge(tick, anchor_price, added_shares, added_notional, 1, MAX_BUCKETS)
            .is_ok(),
        Op::DistributeEager { p_now, total_alloc } => {
            match store.distribute(p_now, total_alloc, MAX_LEDGER) {
                Ok(_) => true,
                Err(ClearingError::LedgerCapacityExceeded) => false,
                Err(_) => false,
            }
        }
        Op::DistributeLazy { p_now, total_alloc } => {
            match store.distribute_lazy(p_now, total_alloc, MAX_LEDGER) {
                Ok(_) => true,
                Err(ClearingError::LedgerCapacityExceeded) => false,
                Err(_) => false,
            }
        }
        Op::Redeem {
            tick,
            shares_to_burn,
        } => {
            // Cap by what's actually there to keep redeems
            // observably consistent across the original / unpacked
            // store. We must NOT branch on internal state in a way
            // the unpacked store wouldn't see — but `get(tick)` is a
            // public observable, so it's safe.
            let cap = store
                .get(tick)
                .map(|b| b.total_recovery_shares)
                .unwrap_or(0);
            let shares = shares_to_burn.min(cap);
            if shares == 0 {
                return false;
            }
            store.redeem(tick, shares).is_ok()
        }
        Op::ApplyPending { tick } => store.apply_pending_to_bucket(tick).is_ok(),
        Op::Compact => {
            store.compact_ledger();
            true
        }
    }
}

/// Snapshot of every observable that the engine reads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Observable {
    direction_is_long: bool,
    bucket_count: u32,
    accrued_value_total: u128,
    next_event_index: u64,
    ledger_gc_offset: u64,
    ledger_len: usize,
    buckets: Vec<(i64, u64, u128, u128, u128, u64, u64)>,
}

fn observe(store: &DormantStore) -> Observable {
    let buckets: Vec<_> = store
        .iter_buckets()
        .map(|(tick, b)| {
            (
                *tick,
                b.anchor_price,
                b.total_recovery_shares,
                b.total_recovery_notional,
                b.accrued_value,
                b.position_count,
                b.last_applied_index,
            )
        })
        .collect();
    Observable {
        direction_is_long: matches!(store.direction(), Direction::Long),
        bucket_count: store.bucket_count(),
        accrued_value_total: store.accrued_value_total(),
        next_event_index: store.next_event_index(),
        ledger_gc_offset: store.ledger_gc_offset(),
        ledger_len: store.ledger_len(),
        buckets,
    }
}

/// Pack twice and assert the second pack matches the first byte-for-byte.
fn assert_pack_idempotent(buckets: &[OnChainBucketRecord], ledger: &OnChainLedger) {
    let restored = unpack_dormant_store(buckets, ledger).unwrap();
    let (b2, l2) = pack_dormant_store(&restored, MAX_BUCKETS, MAX_LEDGER).unwrap();
    assert_eq!(buckets.len(), b2.len(), "bucket count differs after round-trip");
    for (i, (a, b)) in buckets.iter().zip(b2.iter()).enumerate() {
        assert_eq!(a, b, "bucket[{i}] differs after round-trip");
    }
    assert_eq!(ledger.direction, l2.direction);
    assert_eq!(ledger.max_entries, l2.max_entries);
    assert_eq!(ledger.gc_offset, l2.gc_offset);
    assert_eq!(ledger.next_event_index, l2.next_event_index);
    assert_eq!(ledger.accrued_value_total, l2.accrued_value_total);
    assert_eq!(ledger.entry_count, l2.entry_count);
    assert_eq!(ledger.entries.len(), l2.entries.len());
    for (i, (a, b)) in ledger.entries.iter().zip(l2.entries.iter()).enumerate() {
        assert_eq!(a, b, "ledger.entries[{i}] differs after round-trip");
    }
}

#[test]
fn pack_unpack_round_trip_under_random_ops() {
    for seed in 0..6u64 {
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let mut store = DormantStore::new(if seed % 2 == 0 {
            Direction::Long
        } else {
            Direction::Short
        });

        for step in 0..400 {
            let op = generate_op(&mut rng);
            let _ = apply_op(&mut store, op);
            // Engine-side pending-vs-live invariant. compact_ledger
            // only ever drops entries that every live bucket has
            // walked, and apply_pending_to_bucket decrements pending
            // by the per-bucket share each walk produces. So the
            // engine's `pending_distribution_total` must always be
            // bounded above by Σ live entry.allocated_sum_observed.
            assert!(
                store.pending_distribution_total() <= live_alloc_sum(&store),
                "pending exceeds live-window allocated_sum after op {:?}",
                op,
            );

            let (buckets, ledger) = pack_dormant_store(&store, MAX_BUCKETS, MAX_LEDGER)
                .expect("pack always succeeds within capacity");
            // Idempotence: pack(unpack(pack)) == pack.
            assert_pack_idempotent(&buckets, &ledger);

            // Observable equivalence: unpack(pack(store)) == store.
            let restored = unpack_dormant_store(&buckets, &ledger).unwrap();
            assert_eq!(
                observe(&store),
                observe(&restored),
                "seed={seed} step={step}: observe(restored) differs",
            );
        }
    }
}

#[test]
fn unpacked_store_processes_subsequent_ops_identically() {
    // Stronger guarantee: not only is the unpacked store
    // observationally identical immediately after unpack, but it
    // continues to behave identically under further ops. This pins
    // every internal cache (gc_offset, next_event_index,
    // last_applied_index per bucket, accrued_value_total) to be
    // perfectly restored.
    for seed in 0..4u64 {
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let mut original = DormantStore::new(if seed % 2 == 0 {
            Direction::Long
        } else {
            Direction::Short
        });
        // Warm up.
        for _ in 0..200 {
            let _ = apply_op(&mut original, generate_op(&mut rng));
        }
        let (buckets, ledger) = pack_dormant_store(&original, MAX_BUCKETS, MAX_LEDGER).unwrap();
        let mut clone = unpack_dormant_store(&buckets, &ledger).unwrap();
        assert_eq!(observe(&original), observe(&clone));

        // Drive the same ops on both for a long stretch.
        let mut rng2 = ChaCha20Rng::seed_from_u64(seed.wrapping_add(0xa55a));
        for step in 0..400 {
            let op = generate_op(&mut rng2);
            let a = apply_op(&mut original, op);
            let b = apply_op(&mut clone, op);
            assert_eq!(
                a, b,
                "seed={seed} step={step}: success/fail differ between original and unpacked",
            );
            assert_eq!(
                observe(&original),
                observe(&clone),
                "seed={seed} step={step}: observe(original) != observe(unpacked)",
            );
        }
    }
}

#[test]
fn pack_rejects_oversize_ledger() {
    let mut store = DormantStore::new(Direction::Long);
    store
        .insert_or_merge(80, 80 * PRICE_SCALE, 1_000, 100_000, 1, 1024)
        .unwrap();
    store
        .insert_or_merge(200, 200 * PRICE_SCALE, 1_000, 100_000, 1, 1024)
        .unwrap();
    let p = 100 * PRICE_SCALE;
    for _ in 0..10 {
        store.distribute_lazy(p, 100, u32::MAX).unwrap();
    }
    let err = pack_dormant_store(&store, 1024, 5).unwrap_err();
    assert_eq!(err, ClearingError::LedgerCapacityExceeded);
}

#[test]
fn unpack_rejects_corrupted_ledger() {
    let store = DormantStore::new(Direction::Long);
    let (buckets, mut ledger) = pack_dormant_store(&store, 1024, 1024).unwrap();
    ledger.entry_count = 5;
    let err = unpack_dormant_store(&buckets, &ledger).unwrap_err();
    assert_eq!(
        err,
        ClearingError::Invariant("entry_count != entries.len()")
    );
}

#[test]
fn unpack_rejects_mismatched_direction() {
    let store = DormantStore::new(Direction::Long);
    let (buckets, mut ledger) = pack_dormant_store(&store, 1024, 1024).unwrap();
    ledger.direction = 9;
    let err = unpack_dormant_store(&buckets, &ledger).unwrap_err();
    assert_eq!(err, ClearingError::Invariant("invalid direction byte"));
}
