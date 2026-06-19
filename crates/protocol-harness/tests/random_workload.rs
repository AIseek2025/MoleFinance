//! Random multi-trader / multi-sub-pool workload.
//!
//! Each test seeds a deterministic RNG and runs ~1000 operations across
//! ~100 traders, asserting that the harness invariants hold after EVERY
//! operation:
//!
//! 1. **Conservation** — `total_deposits == total_withdrawals + vault + fee_vault`.
//! 2. **Vault decomposition** — `vault == sum_pool_equity + sum_dormant + sum_dust`.
//! 3. **No over-payment** — vault never goes negative.
//!
//! Failure modes the test would surface:
//! - any clearing-core path that leaks more value than its inflows,
//! - any vault accounting drift when the engine moves funds between
//!   pool / dormant / dust slots,
//! - any over-payment of `withdrawable` exceeding accumulated state.

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

/// A single live position in the random workload.
struct LiveTrader {
    position_id: u64,
    sub_pool: u32,
    #[allow(dead_code)]
    direction: Direction,
}

#[test]
fn random_workload_preserves_all_invariants_across_seeds() {
    for seed in [1u64, 7, 42, 1024] {
        run_random_workload(seed);
    }
}

fn run_random_workload(seed: u64) {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);

    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 5_000;

    let mut h = Harness::new(market);

    for sp_id in 0..3u32 {
        h.add_sub_pool(sp_id, 100 * PRICE_SCALE, 0);
    }
    h.check_invariants().unwrap();

    let mut prices = [100 * PRICE_SCALE; 3];
    let mut live: Vec<LiveTrader> = Vec::new();

    let mut slot: u64 = 1;
    let total_ops = 1_000;

    for op_idx in 0..total_ops {
        let total_op_attempts = op_idx + 1;
        let action = rng.gen_range(0u32..100);

        match action {
            0..=34 => {
                let sp = rng.gen_range(0..3u32);
                let direction = if rng.gen_bool(0.5) {
                    Direction::Long
                } else {
                    Direction::Short
                };
                let stake = rng.gen_range(50_000_000u64..200_000_000);
                let p = prices[sp as usize];
                if let Ok(open) = h.open(sp, direction, stake, envelope(p, slot)) {
                    live.push(LiveTrader {
                        position_id: open.position_id,
                        sub_pool: sp,
                        direction,
                    });
                }
            }
            35..=64 => {
                if !live.is_empty() {
                    let idx = rng.gen_range(0..live.len());
                    let trader = live.swap_remove(idx);
                    let p = prices[trader.sub_pool as usize];
                    match h.close(trader.position_id, envelope(p, slot)) {
                        Ok(_) => {}
                        Err(HarnessError::Clearing(
                            clearing_core::ClearingError::WithdrawableZero,
                        )) => {
                            h.force_close(trader.position_id, envelope(p, slot), true)
                                .unwrap();
                        }
                        Err(other) => {
                            panic!("seed {} close error: {:?}", seed, other);
                        }
                    }
                }
            }
            65..=89 => {
                let sp = rng.gen_range(0..3u32);
                let p = prices[sp as usize];
                let bps: i64 = rng.gen_range(-500..=500);
                let delta = (p as i128 * bps as i128 / 10_000) as i64;
                let p_next = (p as i128 + delta as i128).max(1) as u64;
                if p_next != p {
                    let _ = h.sync(sp, envelope(p_next, slot));
                    prices[sp as usize] = p_next;
                }
            }
            90..=94 => {
                if !live.is_empty() {
                    let idx = rng.gen_range(0..live.len());
                    let trader = &live[idx];
                    let p = prices[trader.sub_pool as usize];
                    let _ = h.claim_recovery(trader.position_id, envelope(p, slot));
                }
            }
            _ => {
                let sp = rng.gen_range(0..3u32);
                let dir = if rng.gen_bool(0.5) {
                    Direction::Long
                } else {
                    Direction::Short
                };
                let _ = h.harvest_dust(sp, dir);
            }
        }

        h.check_invariants()
            .unwrap_or_else(|e| panic!("seed {} op #{}: {:?}", seed, total_op_attempts, e));
        slot += 1;
    }

    while let Some(trader) = live.pop() {
        let p = prices[trader.sub_pool as usize];
        match h.close(trader.position_id, envelope(p, slot)) {
            Ok(_) => {}
            Err(HarnessError::Clearing(clearing_core::ClearingError::WithdrawableZero)) => {
                h.force_close(trader.position_id, envelope(p, slot), true)
                    .unwrap();
            }
            Err(other) => panic!("seed {} drain close error: {:?}", seed, other),
        }
        h.check_invariants().unwrap();
        slot += 1;
    }

    for sp in 0..3u32 {
        for dir in [Direction::Long, Direction::Short] {
            let _ = h.harvest_dust(sp, dir);
        }
    }
    h.check_invariants().unwrap();

    let s = h.summary();

    // Strong end-state invariants.
    assert_eq!(
        s.total_deposits,
        s.total_withdrawals + s.vault_balance + s.fee_vault_balance,
        "seed {}: conservation broken",
        seed
    );

    // After force-closing all zero-value positions, the only residual in
    // `vault_balance` is forfeited recovery accrual that nobody can claim
    // (it eventually becomes harvestable dust on the next sync that
    // accrues to that bucket). The protocol must NEVER pay out more than
    // total_deposits.
    assert!(
        s.total_withdrawals + s.fee_vault_balance <= s.total_deposits,
        "seed {}: protocol over-paid: deposits={} withdrawals={} fee_vault={}",
        seed,
        s.total_deposits,
        s.total_withdrawals,
        s.fee_vault_balance,
    );
}
