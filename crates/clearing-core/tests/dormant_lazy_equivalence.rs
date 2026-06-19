//! Strong equivalence between the eager and lazy [`DormantStore`]
//! paths.
//!
//! For any sequence of `(insert_or_merge | distribute | redeem)`
//! operations, we drive two parallel stores:
//!
//! - `eager`: every distribute updates every activated bucket
//!   immediately (the host-side reference today).
//! - `lazy`: every distribute only appends a `DistEntry`; per-bucket
//!   `accrued_value` is updated when the bucket is touched
//!   (`redeem` / `apply_pending_to_bucket`).
//!
//! The claim — formalised by these property tests — is that after
//! draining all pending events on the lazy store
//! (`apply_pending_to_all`), the two stores are observationally
//! identical:
//!
//! - Same `accrued_value_total`.
//! - Same `bucket_count`.
//! - For every tick, identical `(accrued_value, total_recovery_shares,
//!   total_recovery_notional, anchor_price, position_count)`.
//! - Same `total_outstanding_claim_at(p)` at any probe price.
//!
//! On top of that we test that interleaved `compact_ledger` calls do
//! not break equivalence, even when compaction happens after some
//! buckets have been freshly created and have higher
//! `last_applied_index` than the floor of the live window.

use clearing_core::{Direction, DormantBucket, DormantStore};
use molemath::PRICE_SCALE;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

const MAX_BUCKETS: u32 = 1024;

/// Snapshot used for byte-for-byte comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoreSnapshot {
    accrued_value_total: u128,
    bucket_count: u32,
    buckets: Vec<(i64, BucketSnapshot)>,
    next_event_index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BucketSnapshot {
    direction_is_long: bool,
    zero_price_tick: i64,
    anchor_price: u64,
    total_recovery_shares: u128,
    total_recovery_notional: u128,
    accrued_value: u128,
    position_count: u64,
}

fn snapshot_bucket(b: &DormantBucket) -> BucketSnapshot {
    BucketSnapshot {
        direction_is_long: matches!(b.direction, Direction::Long),
        zero_price_tick: b.zero_price_tick,
        anchor_price: b.anchor_price,
        total_recovery_shares: b.total_recovery_shares,
        total_recovery_notional: b.total_recovery_notional,
        accrued_value: b.accrued_value,
        position_count: b.position_count,
    }
}

fn snapshot(store: &DormantStore) -> StoreSnapshot {
    let mut buckets: Vec<(i64, BucketSnapshot)> = store
        .iter_buckets()
        .map(|(k, b)| (*k, snapshot_bucket(b)))
        .collect();
    buckets.sort_by_key(|(k, _)| *k);
    StoreSnapshot {
        accrued_value_total: store.accrued_value_total(),
        bucket_count: store.bucket_count(),
        buckets,
        next_event_index: store.next_event_index(),
    }
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Insert {
        tick: i64,
        anchor_price: u64,
        added_shares: u128,
        added_notional: u128,
    },
    Distribute {
        p_now: u64,
        total_alloc: u128,
    },
    Redeem {
        tick: i64,
        shares_to_burn: u128,
    },
    Compact,
    /// Force a probe of `total_outstanding_claim_at`. Eager and lazy
    /// must agree at this price after the lazy store materialises any
    /// pending state for activated buckets.
    Probe {
        p_now: u64,
    },
}

fn random_op(rng: &mut ChaCha20Rng, direction: Direction) -> Op {
    match rng.gen_range(0u32..100) {
        0..=29 => {
            // Insert / merge (60% of opens are merges into the same set
            // of ticks, so anchors stay sane).
            let tick = rng.gen_range(0i64..16);
            let anchor_price = match direction {
                Direction::Long => 90 * PRICE_SCALE + tick as u64 * PRICE_SCALE / 4,
                Direction::Short => 110 * PRICE_SCALE - tick as u64 * PRICE_SCALE / 4,
            };
            let added_shares = rng.gen_range(1_000_000u128..50_000_000);
            let added_notional = added_shares * 10;
            Op::Insert {
                tick,
                anchor_price,
                added_shares,
                added_notional,
            }
        }
        30..=69 => {
            // Distribute at a price within the activation band.
            let p_now = match direction {
                Direction::Long => rng.gen_range(80 * PRICE_SCALE..120 * PRICE_SCALE),
                Direction::Short => rng.gen_range(80 * PRICE_SCALE..120 * PRICE_SCALE),
            };
            let total_alloc = rng.gen_range(0u128..50_000_000);
            Op::Distribute { p_now, total_alloc }
        }
        70..=84 => {
            let tick = rng.gen_range(0i64..16);
            let shares_to_burn = rng.gen_range(0u128..1_000_000);
            Op::Redeem {
                tick,
                shares_to_burn,
            }
        }
        85..=92 => Op::Compact,
        _ => {
            let p_now = rng.gen_range(60 * PRICE_SCALE..140 * PRICE_SCALE);
            Op::Probe { p_now }
        }
    }
}

/// Apply one op to both stores. Eager uses `distribute`; lazy uses
/// `distribute_lazy`. Other ops route identically.
///
/// Returns `Some(eager_outstanding, lazy_outstanding)` when the op was
/// a `Probe`, so the caller can assert agreement.
fn step(
    eager: &mut DormantStore,
    lazy: &mut DormantStore,
    op: Op,
) -> Option<(u128, u128)> {
    match op {
        Op::Insert {
            tick,
            anchor_price,
            added_shares,
            added_notional,
        } => {
            let _ = eager.insert_or_merge(
                tick,
                anchor_price,
                added_shares,
                added_notional,
                1,
                MAX_BUCKETS,
            );
            let _ = lazy.insert_or_merge(
                tick,
                anchor_price,
                added_shares,
                added_notional,
                1,
                MAX_BUCKETS,
            );
        }
        Op::Distribute { p_now, total_alloc } => {
            // Equivalence under unbounded ring: pass u32::MAX so the
            // capacity guard in distribute / distribute_lazy never
            // fires. The bounded-ring back-pressure is exercised in
            // `tests/ledger_capacity.rs`.
            let _ = eager.distribute(p_now, total_alloc, u32::MAX);
            let _ = lazy.distribute_lazy(p_now, total_alloc, u32::MAX);
        }
        Op::Redeem {
            tick,
            shares_to_burn,
        } => {
            // For redeem to be observably comparable, we need to
            // ensure we're burning the same number of shares from
            // both stores; cap by the smaller bucket's available.
            let cap_shares = match (eager.get(tick), lazy.get(tick)) {
                (Some(a), Some(b)) => a.total_recovery_shares.min(b.total_recovery_shares),
                _ => 0,
            };
            let actual = shares_to_burn.min(cap_shares);
            if actual == 0 {
                return None;
            }
            let _ = eager.redeem(tick, actual);
            let _ = lazy.redeem(tick, actual);
        }
        Op::Compact => {
            // Compaction needs every bucket up-to-date in lazy; bring
            // it level first so the GC watermark is meaningful.
            lazy.apply_pending_to_all().unwrap();
            let _ = eager.compact_ledger();
            let _ = lazy.compact_ledger();
        }
        Op::Probe { p_now } => {
            let eager_oc = eager.total_outstanding_claim_at(p_now).unwrap();
            // Lazy must materialise pending before quoting outstanding.
            lazy.apply_pending_to_all().unwrap();
            let lazy_oc = lazy.total_outstanding_claim_at(p_now).unwrap();
            return Some((eager_oc, lazy_oc));
        }
    }
    None
}

fn run_random_walk_for_direction(direction: Direction, seed: u64, n_ops: usize) {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let mut eager = DormantStore::new(direction);
    let mut lazy = DormantStore::new(direction);

    for step_idx in 0..n_ops {
        let op = random_op(&mut rng, direction);
        if let Some((eo, lo)) = step(&mut eager, &mut lazy, op) {
            assert_eq!(
                eo, lo,
                "seed {} step {}: outstanding mismatch eager={} lazy={}",
                seed, step_idx, eo, lo
            );
        }

        // Internal invariants always hold.
        eager.check_invariants().unwrap();
        lazy.check_invariants().unwrap();
    }

    // Drain lazy and compare full snapshots.
    lazy.apply_pending_to_all().unwrap();
    let snap_eager = snapshot(&eager);
    let snap_lazy = snapshot(&lazy);
    assert_eq!(
        snap_eager.accrued_value_total, snap_lazy.accrued_value_total,
        "seed {}: accrued_value_total mismatch",
        seed
    );
    assert_eq!(
        snap_eager.bucket_count, snap_lazy.bucket_count,
        "seed {}: bucket_count mismatch",
        seed
    );
    assert_eq!(
        snap_eager.next_event_index, snap_lazy.next_event_index,
        "seed {}: next_event_index mismatch",
        seed
    );
    assert_eq!(
        snap_eager.buckets, snap_lazy.buckets,
        "seed {}: per-bucket state diverged",
        seed
    );
}

#[test]
fn lazy_eager_equivalence_long_random_walk_400_ops() {
    for seed in [1u64, 17, 42, 99, 1024] {
        run_random_walk_for_direction(Direction::Long, seed, 400);
    }
}

#[test]
fn lazy_eager_equivalence_short_random_walk_400_ops() {
    for seed in [3u64, 19, 43, 100, 2048] {
        run_random_walk_for_direction(Direction::Short, seed, 400);
    }
}

#[test]
fn lazy_eager_equivalence_with_aggressive_compaction() {
    // Compaction-heavy: half the ops are compactions. Forces the GC
    // watermark to chase the lazy store closely; we still demand byte
    // equality at the end.
    for seed in [5u64, 25, 125] {
        let direction = Direction::Long;
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let mut eager = DormantStore::new(direction);
        let mut lazy = DormantStore::new(direction);
        for _ in 0..600 {
            let mut op = random_op(&mut rng, direction);
            // Bias toward compaction.
            if rng.gen_range(0u32..2) == 0 {
                op = Op::Compact;
            }
            if let Some((eo, lo)) = step(&mut eager, &mut lazy, op) {
                assert_eq!(eo, lo);
            }
            eager.check_invariants().unwrap();
            lazy.check_invariants().unwrap();
        }
        lazy.apply_pending_to_all().unwrap();
        assert_eq!(snapshot(&eager), snapshot(&lazy), "seed {}", seed);
    }
}

#[test]
fn lazy_distribute_does_not_touch_buckets_until_apply() {
    let direction = Direction::Long;
    let mut store = DormantStore::new(direction);
    store
        .insert_or_merge(0, 90 * PRICE_SCALE, 1_000_000, 10_000_000, 1, 100)
        .unwrap();
    store
        .insert_or_merge(1, 95 * PRICE_SCALE, 1_000_000, 10_000_000, 1, 100)
        .unwrap();

    // Snapshot accrued before distribute_lazy.
    let accrued_before = store.get(0).unwrap().accrued_value;
    let accrued_total_before = store.accrued_value_total();
    assert_eq!(accrued_before, 0);
    assert_eq!(accrued_total_before, 0);

    let receipt = store
        .distribute_lazy(100 * PRICE_SCALE, 1_000_000, u32::MAX)
        .unwrap();
    assert!(receipt.allocated > 0);

    // Buckets were NOT eagerly mutated; aggregate sum stays at 0.
    assert_eq!(store.get(0).unwrap().accrued_value, 0);
    assert_eq!(store.get(1).unwrap().accrued_value, 0);
    assert_eq!(store.accrued_value_total(), 0);
    // The ledger gained one entry though.
    assert_eq!(store.ledger().len(), 1);

    // Touch one bucket: only that bucket materialises.
    store.apply_pending_to_bucket(0).unwrap();
    assert!(store.get(0).unwrap().accrued_value > 0);
    assert_eq!(store.get(1).unwrap().accrued_value, 0);

    // Touch the other; total now matches the receipt's allocated.
    store.apply_pending_to_bucket(1).unwrap();
    assert_eq!(
        store.get(0).unwrap().accrued_value + store.get(1).unwrap().accrued_value,
        receipt.allocated
    );
    assert_eq!(store.accrued_value_total(), receipt.allocated);
}
