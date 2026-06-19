//! Wave 7: lazy-mode keeper drain equivalence.
//!
//! The thesis of this test:
//!
//! > For any sequence of `(open, close, force_close, claim, sync,
//! > harvest)` ops, a lazy-mode runtime that interleaves
//! > `pre_sync_dormant_bucket` for every live bucket between every
//! > op produces a state byte-identical to an eager-mode runtime
//! > driven by the same op stream.
//!
//! Why this matters:
//!
//!   * Wave 5.5 fixed `distribute_eager()` to drain pending lazy
//!     allocations before allocating in eager mode. That fix only
//!     guards mixed-mode workloads.
//!   * Wave 6 / 6.5 wired the on-chain `pre_sync_dormant_bucket`
//!     instruction. In production, lazy-mode keepers are expected
//!     to call it between syncs to keep bucket `accrued_value`
//!     close to its eager equivalent.
//!   * If `apply_pending_to_bucket` ever drifts from the eager
//!     `distribute()` math — e.g. due to a refactor of the share
//!     calculation, an off-by-one in `last_applied_index`, or a
//!     missed pending entry on bucket activation — this test trips
//!     before the divergence reaches Solana.
//!
//! The op stream replicates `chain-mirror::harness_parity` for
//! coverage symmetry; the comparison is between two **independent**
//! `Harness` instances rather than harness-vs-chain-mirror.

use clearing_core::{ClearingError, Direction, MarketParams, Position, PriceEnvelope, SubPool};
use molemath::PRICE_SCALE;
use protocol_harness::{Harness, HarnessError};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

fn envelope(p: u64, slot: u64) -> PriceEnvelope {
    PriceEnvelope {
        p_now: p,
        slot,
        expected_min: p,
        expected_max: p,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StateSnapshot {
    vault_balance: u128,
    fee_vault_balance: u128,
    total_deposits: u128,
    total_withdrawals: u128,
    sub_pools: Vec<SubPoolAggregate>,
    positions: Vec<PositionAggregate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SubPoolAggregate {
    sub_pool_id: u32,
    long_pool_equity: u128,
    short_pool_equity: u128,
    long_active_shares: u128,
    short_active_shares: u128,
    long_recovery_shares: u128,
    short_recovery_shares: u128,
    long_active_notional: u128,
    short_active_notional: u128,
    long_dust: u128,
    short_dust: u128,
    long_active_generation: u64,
    short_active_generation: u64,
    long_dormant_bucket_count: u32,
    short_dormant_bucket_count: u32,
    last_price: u64,
    /// `(tick, anchor, shares, notional, accrued, pos_count)`.
    /// `last_applied_index` is intentionally **omitted** — it is the
    /// one observable that legitimately differs between eager and
    /// lazy+drain runs (eager's bucket sees no ledger so the index
    /// stays at its `next_event_index`-on-creation value, while
    /// lazy's bucket has walked every entry to catch up). All other
    /// fields are byte-equal.
    long_buckets: Vec<(i64, u64, u128, u128, u128, u64)>,
    short_buckets: Vec<(i64, u64, u128, u128, u128, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PositionAggregate {
    position_id: u64,
    direction_is_long: bool,
    status: u8,
    active_shares: u128,
    recovery_shares: u128,
    recovery_bucket_tick: Option<i64>,
    notional: u128,
    active_generation: u64,
    principal: u64,
    zero_price: u64,
}

fn snap_pos(p: &Position) -> PositionAggregate {
    PositionAggregate {
        position_id: p.position_id,
        direction_is_long: matches!(p.direction, Direction::Long),
        status: match p.status {
            clearing_core::PositionStatus::Open => 0,
            clearing_core::PositionStatus::Dormant => 1,
            clearing_core::PositionStatus::Closed => 2,
        },
        active_shares: p.active_shares,
        recovery_shares: p.recovery_shares,
        recovery_bucket_tick: p.recovery_bucket_tick,
        notional: p.notional,
        active_generation: p.active_generation,
        principal: p.principal,
        zero_price: p.zero_price,
    }
}

fn snap_sub_pool(sp: &SubPool) -> SubPoolAggregate {
    let mut long_buckets: Vec<_> = sp
        .long_dormant
        .iter_buckets()
        .map(|(t, b)| {
            (
                *t,
                b.anchor_price,
                b.total_recovery_shares,
                b.total_recovery_notional,
                b.accrued_value,
                b.position_count,
            )
        })
        .collect();
    long_buckets.sort_by_key(|x| x.0);
    let mut short_buckets: Vec<_> = sp
        .short_dormant
        .iter_buckets()
        .map(|(t, b)| {
            (
                *t,
                b.anchor_price,
                b.total_recovery_shares,
                b.total_recovery_notional,
                b.accrued_value,
                b.position_count,
            )
        })
        .collect();
    short_buckets.sort_by_key(|x| x.0);
    SubPoolAggregate {
        sub_pool_id: sp.sub_pool_id,
        long_pool_equity: sp.long_pool_equity,
        short_pool_equity: sp.short_pool_equity,
        long_active_shares: sp.long_active_shares,
        short_active_shares: sp.short_active_shares,
        long_recovery_shares: sp.long_recovery_shares,
        short_recovery_shares: sp.short_recovery_shares,
        long_active_notional: sp.long_active_notional,
        short_active_notional: sp.short_active_notional,
        long_dust: sp.long_dust,
        short_dust: sp.short_dust,
        long_active_generation: sp.long_active_generation,
        short_active_generation: sp.short_active_generation,
        long_dormant_bucket_count: sp.long_dormant_bucket_count,
        short_dormant_bucket_count: sp.short_dormant_bucket_count,
        last_price: sp.last_price,
        long_buckets,
        short_buckets,
    }
}

fn snapshot(h: &Harness, sub_pool_ids: &[u32], position_ids: &[u64]) -> StateSnapshot {
    let summary = h.summary();
    let mut sub_pools = Vec::new();
    for id in sub_pool_ids {
        if let Some(sp) = h.sub_pool(*id) {
            sub_pools.push(snap_sub_pool(sp));
        }
    }
    sub_pools.sort_by_key(|s| s.sub_pool_id);
    let mut positions = Vec::new();
    for pid in position_ids {
        if let Some(p) = h.position(*pid) {
            positions.push(snap_pos(p));
        }
    }
    positions.sort_by_key(|p| p.position_id);
    StateSnapshot {
        vault_balance: summary.vault_balance,
        fee_vault_balance: summary.fee_vault_balance,
        total_deposits: summary.total_deposits,
        total_withdrawals: summary.total_withdrawals,
        sub_pools,
        positions,
    }
}

/// Tag the engine error so we can compare result classifications
/// across modes without relying on `PartialEq` between full
/// `ClearingError` values (some embed unique strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClearingErrorTag {
    WithdrawableZero,
    PositionNotOpen,
    Other,
}

fn tag(e: &HarnessError) -> ClearingErrorTag {
    match e {
        HarnessError::Clearing(ClearingError::WithdrawableZero) => ClearingErrorTag::WithdrawableZero,
        HarnessError::Clearing(ClearingError::PositionNotOpen) => ClearingErrorTag::PositionNotOpen,
        _ => ClearingErrorTag::Other,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpResult {
    OkOpen { position_id: u64 },
    OkVoid,
    Err(ClearingErrorTag),
}

#[test]
fn lazy_drain_matches_eager_under_random_workload() {
    for seed in [4u64, 22, 137] {
        run_seed(seed);
    }
}

fn run_seed(seed: u64) {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;

    let mut market_eager = market.clone();
    market_eager.dormant_distribute_mode = clearing_core::DistributeMode::Eager;
    let mut market_lazy = market;
    market_lazy.dormant_distribute_mode = clearing_core::DistributeMode::Lazy;

    let mut h_eager = Harness::new(market_eager);
    let mut h_lazy = Harness::new(market_lazy);
    let sub_pool_ids = vec![0u32, 1];
    for sp_id in &sub_pool_ids {
        h_eager.add_sub_pool(*sp_id, 100 * PRICE_SCALE, 0);
        h_lazy.add_sub_pool(*sp_id, 100 * PRICE_SCALE, 0);
    }

    let mut all_pids: Vec<u64> = Vec::new();
    let mut live: Vec<(u64, u32)> = Vec::new();
    let mut prices = [100 * PRICE_SCALE; 2];

    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let mut slot: u64 = 1;
    for step in 0..400u32 {
        let action = rng.gen_range(0u32..100);
        let ctx_label = format!("seed={seed} step={step} action={action}");

        let (e_res, l_res) = match action {
            0..=39 => {
                let sp = rng.gen_range(0..2u32);
                let dir = if rng.gen_bool(0.5) {
                    Direction::Long
                } else {
                    Direction::Short
                };
                let stake = rng.gen_range(50_000_000u64..150_000_000);
                let p = prices[sp as usize];
                let e_r = h_eager.open(sp, dir, stake, envelope(p, slot));
                let l_r = h_lazy.open(sp, dir, stake, envelope(p, slot));
                let e_tag = match &e_r {
                    Ok(s) => OpResult::OkOpen { position_id: s.position_id },
                    Err(e) => OpResult::Err(tag(e)),
                };
                let l_tag = match &l_r {
                    Ok(s) => OpResult::OkOpen { position_id: s.position_id },
                    Err(e) => OpResult::Err(tag(e)),
                };
                if let OpResult::OkOpen { position_id } = e_tag {
                    live.push((position_id, sp));
                    all_pids.push(position_id);
                }
                (e_tag, l_tag)
            }
            40..=64 => {
                if live.is_empty() {
                    (OpResult::OkVoid, OpResult::OkVoid)
                } else {
                    let idx = rng.gen_range(0..live.len());
                    let (pid, sp) = live[idx];
                    let p = prices[sp as usize];
                    let e_r = h_eager.close(pid, envelope(p, slot));
                    let l_r = h_lazy.close(pid, envelope(p, slot));
                    let e_tag = match &e_r {
                        Ok(_) => {
                            live.swap_remove(idx);
                            OpResult::OkVoid
                        }
                        Err(e) => OpResult::Err(tag(e)),
                    };
                    let l_tag = match &l_r {
                        Ok(_) => OpResult::OkVoid,
                        Err(e) => OpResult::Err(tag(e)),
                    };
                    if matches!(e_tag, OpResult::Err(ClearingErrorTag::WithdrawableZero))
                        && matches!(l_tag, OpResult::Err(ClearingErrorTag::WithdrawableZero))
                    {
                        h_eager.force_close(pid, envelope(p, slot), true).unwrap();
                        h_lazy.force_close(pid, envelope(p, slot), true).unwrap();
                        live.swap_remove(idx);
                    }
                    (e_tag, l_tag)
                }
            }
            65..=74 => {
                if live.is_empty() {
                    (OpResult::OkVoid, OpResult::OkVoid)
                } else {
                    let (pid, sp) = live[rng.gen_range(0..live.len())];
                    let p = prices[sp as usize];
                    let e_r = h_eager.claim_recovery(pid, envelope(p, slot));
                    let l_r = h_lazy.claim_recovery(pid, envelope(p, slot));
                    let e_tag = match &e_r {
                        Ok(_) => OpResult::OkVoid,
                        Err(e) => OpResult::Err(tag(e)),
                    };
                    let l_tag = match &l_r {
                        Ok(_) => OpResult::OkVoid,
                        Err(e) => OpResult::Err(tag(e)),
                    };
                    (e_tag, l_tag)
                }
            }
            75..=79 => {
                let sp = rng.gen_range(0..2u32);
                let dir = if rng.gen_bool(0.5) {
                    Direction::Long
                } else {
                    Direction::Short
                };
                let e_r = h_eager.harvest_dust(sp, dir);
                let l_r = h_lazy.harvest_dust(sp, dir);
                let e_tag = match &e_r {
                    Ok(_) => OpResult::OkVoid,
                    Err(e) => OpResult::Err(tag(e)),
                };
                let l_tag = match &l_r {
                    Ok(_) => OpResult::OkVoid,
                    Err(e) => OpResult::Err(tag(e)),
                };
                (e_tag, l_tag)
            }
            _ => {
                let sp = rng.gen_range(0..2u32);
                let p = prices[sp as usize];
                let bps: i64 = rng.gen_range(-500..=500);
                let delta = (p as i128 * bps as i128 / 10_000) as i64;
                let p_next = (p as i128 + delta as i128).max(1) as u64;
                if p_next == p {
                    (OpResult::OkVoid, OpResult::OkVoid)
                } else {
                    let e_r = h_eager.sync(sp, envelope(p_next, slot));
                    let l_r = h_lazy.sync(sp, envelope(p_next, slot));
                    let e_tag = match &e_r {
                        Ok(_) => {
                            prices[sp as usize] = p_next;
                            OpResult::OkVoid
                        }
                        Err(e) => OpResult::Err(tag(e)),
                    };
                    let l_tag = match &l_r {
                        Ok(_) => OpResult::OkVoid,
                        Err(e) => OpResult::Err(tag(e)),
                    };
                    (e_tag, l_tag)
                }
            }
        };

        // 1. Both modes agree on Ok-vs-Err and the error class.
        assert_eq!(e_res, l_res, "{ctx_label}: result mismatch");

        // 2. Drain the lazy side's pending allocations across every
        //    live bucket of every sub-pool. After this, lazy's
        //    `pending_distribution_total` should be zero everywhere
        //    and per-bucket `accrued_value` should match the eager
        //    side. We deliberately drain BEFORE every assertion so
        //    transient lazy-side lag never trips the comparison.
        for sp in &sub_pool_ids {
            h_lazy.drain_all_buckets(*sp, slot).unwrap();
        }

        // 3. Snapshots must match byte-for-byte across every
        //    aggregate the trader / SPL accounting cares about. The
        //    one omitted observable is per-bucket
        //    `last_applied_index` (see `SubPoolAggregate::long_buckets`
        //    docstring).
        let snap_e = snapshot(&h_eager, &sub_pool_ids, &all_pids);
        let snap_l = snapshot(&h_lazy, &sub_pool_ids, &all_pids);
        if snap_e != snap_l {
            // Pretty-print the first divergence so failure messages
            // are actionable.
            for (i, (x, y)) in snap_e.sub_pools.iter().zip(snap_l.sub_pools.iter()).enumerate() {
                if x != y {
                    panic!(
                        "{ctx_label}: sub_pool[{i}] (id={}) diff:\n  eager = {:?}\n  lazy  = {:?}",
                        x.sub_pool_id, x, y
                    );
                }
            }
            for (i, (x, y)) in snap_e.positions.iter().zip(snap_l.positions.iter()).enumerate() {
                if x != y {
                    panic!(
                        "{ctx_label}: position[{i}] (id={}) diff:\n  eager = {:?}\n  lazy  = {:?}",
                        x.position_id, x, y
                    );
                }
            }
            assert_eq!(snap_e, snap_l, "{ctx_label}: snapshot diff (no field localised)");
        }

        // 4. Both runtimes still satisfy the four-term vault
        //    invariant. After draining, lazy's `dormant_pending_total`
        //    is zero, so the four-term sum collapses to the eager
        //    three-term sum — but the harness check verifies the
        //    full four-term identity unconditionally.
        h_eager.check_invariants().unwrap();
        h_lazy.check_invariants().unwrap();

        slot += 1;
    }
}
