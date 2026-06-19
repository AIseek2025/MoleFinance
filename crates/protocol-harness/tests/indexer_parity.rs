//! Indexer parity: chain `withdrawable` matches indexer's `equity()`
//! after we explicitly sync to the close price first.
//!
//! `clearing_core::close_position` internally re-syncs to the caller's
//! envelope BEFORE computing `withdrawable`, so to compare the indexer's
//! pre-close view fairly we must (1) sync, (2) capture indexer equity,
//! (3) close. The close's internal sync then becomes a no-op price-wise.
//!
//! ## Status (wave 4)
//!
//! 1. **No-rotation** (`run_drift_seed` with ≤±2% syncs): chain and
//!    indexer agree to single-digit raw units (pure floor accumulation).
//! 2. **With rotations + claims + multi-position buckets**
//!    (`aggregate_chain_payouts_match_indexer_with_rotations`):
//!    aggregate drift drops to single-ppb ratios after the harness
//!    adopted Solana tx-revert semantics on every entry point. The
//!    earlier ~0.7 % drift was a host-side artefact of close_position
//!    mutating the sub_pool mid-way, then erroring with
//!    `WithdrawableZero` and discarding the mutation events — so the
//!    chain bucket disappeared while the indexer never saw the
//!    corresponding burn. With the snapshot/restore wrapper in
//!    `Harness::close` (and friends), failed calls revert sub_pool +
//!    position to their pre-call state, just like a Solana tx revert.

use clearing_core::{Direction, MarketParams, PriceEnvelope};
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

#[test]
fn sync_then_close_matches_indexer_within_bounds_two_traders() {
    // Two traders, opposite sides, three price levels, exact match.
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let alice = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let bob = h
        .open(0, Direction::Short, 100_000_000, envelope(entry, 2))
        .unwrap();

    let p1 = entry + entry / 50;
    h.sync(0, envelope(p1, 3)).unwrap();
    let alice_pre = h.indexer().position(alice.position_id).unwrap().equity();
    let bob_pre = h.indexer().position(bob.position_id).unwrap().equity();

    let alice_close = h.close(alice.position_id, envelope(p1, 4)).unwrap();
    let bob_close = h.close(bob.position_id, envelope(p1, 5)).unwrap();
    h.check_invariants().unwrap();

    assert_eq!(
        alice_pre, alice_close.withdrawable,
        "alice indexer pre-close equity != withdrawable"
    );
    assert_eq!(
        bob_pre, bob_close.withdrawable,
        "bob indexer pre-close equity != withdrawable"
    );
}

#[test]
fn random_sync_then_close_indexer_drift_is_bounded() {
    for seed in [0u64, 1, 7] {
        run_drift_seed(seed);
    }
}

fn run_drift_seed(seed: u64) {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let market = MarketParams::sample();

    let mut h = Harness::new(market);
    for sp_id in 0..2u32 {
        h.add_sub_pool(sp_id, 100 * PRICE_SCALE, 0);
    }

    let mut prices = [100 * PRICE_SCALE; 2];
    let mut live: Vec<(u64, u32)> = Vec::new();
    let mut max_drift: u128 = 0;

    let mut slot: u64 = 1;
    for _ in 0..400u32 {
        let action = rng.gen_range(0u32..100);
        match action {
            0..=39 => {
                let sp = rng.gen_range(0..2u32);
                let dir = if rng.gen_bool(0.5) {
                    Direction::Long
                } else {
                    Direction::Short
                };
                let stake = rng.gen_range(50_000_000u64..120_000_000);
                let p = prices[sp as usize];
                if let Ok(open) = h.open(sp, dir, stake, envelope(p, slot)) {
                    live.push((open.position_id, sp));
                }
            }
            40..=69 => {
                if !live.is_empty() {
                    let idx = rng.gen_range(0..live.len());
                    let (pid, sp) = live.swap_remove(idx);
                    let p = prices[sp as usize];
                    // Sync first, then capture indexer view, then close.
                    let _ = h.sync(sp, envelope(p, slot));
                    let pre = h
                        .indexer()
                        .position(pid)
                        .map(|v| v.equity())
                        .unwrap_or(0);
                    match h.close(pid, envelope(p, slot)) {
                        Ok(c) => {
                            let drift = pre.abs_diff(c.withdrawable);
                            if drift > max_drift {
                                max_drift = drift;
                            }
                        }
                        Err(HarnessError::Clearing(
                            clearing_core::ClearingError::WithdrawableZero,
                        )) => {
                            h.force_close(pid, envelope(p, slot), true).unwrap();
                        }
                        Err(other) => panic!("seed {} close error: {:?}", seed, other),
                    }
                }
            }
            _ => {
                let sp = rng.gen_range(0..2u32);
                let p = prices[sp as usize];
                // Keep price moves tame: ±2% per sync, no rotations expected.
                let bps: i64 = rng.gen_range(-200..=200);
                let delta = (p as i128 * bps as i128 / 10_000) as i64;
                let p_next = (p as i128 + delta as i128).max(1) as u64;
                if p_next != p {
                    let _ = h.sync(sp, envelope(p_next, slot));
                    prices[sp as usize] = p_next;
                }
            }
        }
        h.check_invariants().unwrap();
        slot += 1;
    }

    // Drift bound: the indexer aggregates a per-position floor remainder
    // on every PoolSync that touches the position. With ~400 ops and
    // ~100 distinct positions and ±2% sync caps (no rotations), an
    // empirical bound of 1024 raw units (i.e. ~1e-6 of the smallest
    // stake) leaves comfortable headroom.
    assert!(
        max_drift <= 1024,
        "seed {}: indexer drift = {}",
        seed,
        max_drift
    );
}

/// Aggregate parity: across many traders, many sub pools, and big
/// price moves that DO trigger rotations, the **sum** of indexer
/// pre-close equities matches the **sum** of chain payouts within a
/// tight relative bound.
///
/// Wave-4 fix (atomic Solana-style tx revert in the harness) brings
/// observed signed drift to a few hundred raw units across ~10^10
/// total deposits, i.e. < 1 ppm. We pin the bound at 1 ppm to catch
/// any regression; with floor-rounding noise this leaves ample
/// headroom but immediately fails on any new reintroduced systematic
/// bias. Conservation on chain remains the binding correctness
/// invariant (`total_deposits == withdrawn + vault + fee_vault`).
#[test]
fn aggregate_chain_payouts_match_indexer_with_rotations() {
    for seed in [0u64, 7, 42] {
        run_aggregate_parity(seed);
    }
}

fn run_aggregate_parity(seed: u64) {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000; // ±500% — exercise rotations.

    let mut h = Harness::new(market);
    for sp_id in 0..3u32 {
        h.add_sub_pool(sp_id, 100 * PRICE_SCALE, 0);
    }

    let mut prices = [100 * PRICE_SCALE; 3];
    let mut live: Vec<(u64, u32)> = Vec::new();
    let mut sum_indexer_pre_equity: u128 = 0;
    let mut sum_chain_withdrawable: u128 = 0;

    let mut slot: u64 = 1;
    for _ in 0..500u32 {
        let action = rng.gen_range(0u32..100);
        match action {
            0..=39 => {
                let sp = rng.gen_range(0..3u32);
                let dir = if rng.gen_bool(0.5) {
                    Direction::Long
                } else {
                    Direction::Short
                };
                let stake = rng.gen_range(50_000_000u64..150_000_000);
                let p = prices[sp as usize];
                if let Ok(open) = h.open(sp, dir, stake, envelope(p, slot)) {
                    live.push((open.position_id, sp));
                }
            }
            40..=69 => {
                if !live.is_empty() {
                    let idx = rng.gen_range(0..live.len());
                    let (pid, sp) = live.swap_remove(idx);
                    let p = prices[sp as usize];
                    let _ = h.sync(sp, envelope(p, slot));
                    let pre = h
                        .indexer()
                        .position(pid)
                        .map(|v| v.equity())
                        .unwrap_or(0);
                    match h.close(pid, envelope(p, slot)) {
                        Ok(c) => {
                            sum_indexer_pre_equity += pre;
                            sum_chain_withdrawable += c.withdrawable;
                        }
                        Err(HarnessError::Clearing(
                            clearing_core::ClearingError::WithdrawableZero,
                        )) => {
                            h.force_close(pid, envelope(p, slot), true).unwrap();
                        }
                        Err(other) => panic!("seed {}: {:?}", seed, other),
                    }
                }
            }
            _ => {
                let sp = rng.gen_range(0..3u32);
                let p = prices[sp as usize];
                // ±5% steps to provoke rotations.
                let bps: i64 = rng.gen_range(-500..=500);
                let delta = (p as i128 * bps as i128 / 10_000) as i64;
                let p_next = (p as i128 + delta as i128).max(1) as u64;
                if p_next != p {
                    let _ = h.sync(sp, envelope(p_next, slot));
                    prices[sp as usize] = p_next;
                }
            }
        }
        h.check_invariants().unwrap();
        slot += 1;
    }

    while let Some((pid, sp)) = live.pop() {
        let p = prices[sp as usize];
        let _ = h.sync(sp, envelope(p, slot));
        let pre = h
            .indexer()
            .position(pid)
            .map(|v| v.equity())
            .unwrap_or(0);
        match h.close(pid, envelope(p, slot)) {
            Ok(c) => {
                sum_indexer_pre_equity += pre;
                sum_chain_withdrawable += c.withdrawable;
            }
            Err(HarnessError::Clearing(clearing_core::ClearingError::WithdrawableZero)) => {
                h.force_close(pid, envelope(p, slot), true).unwrap();
            }
            Err(other) => panic!("seed {}: drain {:?}", seed, other),
        }
        h.check_invariants().unwrap();
        slot += 1;
    }

    let s = h.summary();
    let drift = sum_indexer_pre_equity.abs_diff(sum_chain_withdrawable);
    // 1 ppm of total deposits, with a 1024-raw-unit absolute floor so
    // tiny workloads don't trip on pure rounding noise.
    let bound = (s.total_deposits / 1_000_000).max(1024);
    assert!(
        drift <= bound,
        "seed {}: aggregate drift {} > bound {} (deposits={}, indexer_sum={}, chain_sum={})",
        seed, drift, bound, s.total_deposits, sum_indexer_pre_equity, sum_chain_withdrawable
    );

    // Conservation must always hold (chain is source of truth).
    assert_eq!(
        s.total_deposits,
        s.total_withdrawals + s.vault_balance + s.fee_vault_balance
    );
}
