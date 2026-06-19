//! Property tests for the clearing engine.
//!
//! These cover the three load-bearing guarantees of the shares model:
//!
//! 1. **Conservation**: total tokens flowing in == total tokens flowing out
//!    (modulo bounded protocol-favorable dust).
//! 2. **No phantom growth**: `recovery_shares` cannot accrue value without
//!    counterparty losses.
//! 3. **Ratio stability**: same-direction opens preserve `pool_equity /
//!    active_shares` (modulo rounding).

use std::sync::atomic::{AtomicU64, Ordering};

use clearing_core::{
    close_position, force_close_zero_value_position, open_position as engine_open, sync_pool,
    ClearingError, ClearingResult, Direction, MarketParams, OpenOutcome, Position, PriceEnvelope,
    SubPool,
};
use molemath::PRICE_SCALE;
use proptest::prelude::*;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

static POSITION_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn open_position(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    envelope: PriceEnvelope,
    direction: Direction,
    gross_amount: u64,
) -> ClearingResult<(Position, OpenOutcome)> {
    let id = POSITION_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
    engine_open(market, sub_pool, envelope, direction, gross_amount, id)
}

fn envelope(p: u64, slot: u64) -> PriceEnvelope {
    PriceEnvelope {
        p_now: p,
        slot,
        expected_min: p,
        expected_max: p,
    }
}

#[test]
fn ratio_invariant_with_two_long_opens() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    let stake_a = 100_000_000u64;
    let stake_b = 250_000_000u64;

    let (mut a, _) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        stake_a,
    )
    .unwrap();
    let _ = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Long,
        stake_b,
    )
    .unwrap();

    // Without any price move, pool_equity == sum_principals == active_shares.
    assert_eq!(sub_pool.long_pool_equity, (stake_a + stake_b) as u128);
    assert_eq!(sub_pool.long_active_shares, (stake_a + stake_b) as u128);

    // Closing A returns its full principal (no PnL).
    let close_a = close_position(&market, &mut sub_pool, envelope(entry, 3), &mut a).unwrap();
    assert_eq!(close_a.withdrawable, stake_a as u128);
    // The ratio is preserved.
    assert_eq!(sub_pool.long_pool_equity, stake_b as u128);
    assert_eq!(sub_pool.long_active_shares, stake_b as u128);
}

#[test]
fn recovery_shares_do_not_grow_without_new_loss() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    let stake = 100_000_000u64;
    let _ = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        stake,
    )
    .unwrap();
    let _ = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Short,
        stake,
    )
    .unwrap();

    // Crash long.
    let p1 = entry * 7 / 10; // -30 %
    sync_pool(&market, &mut sub_pool, envelope(p1, 3)).unwrap();
    assert_eq!(sub_pool.long_pool_equity, 0);
    assert!(sub_pool.long_recovery_shares > 0);
    let initial_long_recovery_value = sub_pool.long_dormant.accrued_value_total();

    // Now drift sideways: a series of micro-syncs that net to no movement.
    sync_pool(&market, &mut sub_pool, envelope(p1, 4)).unwrap();
    sync_pool(&market, &mut sub_pool, envelope(p1, 5)).unwrap();

    // No new counterparty loss => no recovery accrual.
    assert_eq!(
        sub_pool.long_dormant.accrued_value_total(),
        initial_long_recovery_value,
    );
}

/// Random walk simulation. After every step we verify that:
///   sum(opens deposited) == sum(close withdrawals)
///                          + sub_pool.long_pool_equity
///                          + sub_pool.short_pool_equity
///                          + sum(dust)
///                          + sum(recovery_accrued)
///                          + sum(open active principal still alive)
fn run_conservation_scenario(seed: u64, steps: usize) {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 5_000; // 50 %
    let mut p = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, p, 0);

    let mut deposited: u128 = 0;
    let mut withdrawn: u128 = 0;
    let mut positions: Vec<clearing_core::Position> = Vec::new();

    for slot in 1..=steps as u64 {
        let action = rng.gen_range(0..10);
        match action {
            0..=4 => {
                // Open a position.
                let direction = if rng.gen_bool(0.5) {
                    Direction::Long
                } else {
                    Direction::Short
                };
                let principal = rng.gen_range(market.min_margin..(10 * market.min_margin));
                let env = envelope(p, slot);
                match open_position(&market, &mut sub_pool, env, direction, principal) {
                    Ok((pos, _)) => {
                        deposited += principal as u128;
                        positions.push(pos);
                    }
                    Err(ClearingError::DilutionRiskTooHigh)
                    | Err(ClearingError::SharesMintedTooSmall) => {}
                    Err(e) => panic!("unexpected open error {:?}", e),
                }
            }
            5..=7 => {
                // Close a random position if any.
                if positions.is_empty() {
                    continue;
                }
                let idx = rng.gen_range(0..positions.len());
                let mut pos = positions.swap_remove(idx);
                let env = envelope(p, slot);
                match close_position(&market, &mut sub_pool, env, &mut pos) {
                    Ok(out) => {
                        withdrawn += out.withdrawable;
                    }
                    Err(ClearingError::WithdrawableZero) => {
                        // Force close without funds.
                        let _ = force_close_zero_value_position(
                            &market,
                            &mut sub_pool,
                            env,
                            &mut pos,
                            true,
                        )
                        .unwrap();
                    }
                    Err(e) => panic!("unexpected close error {:?}", e),
                }
            }
            8..=9 => {
                // Price move.
                let delta_bps: i32 = rng.gen_range(-500..=500);
                let candidate = if delta_bps >= 0 {
                    p.saturating_add(p / 10_000 * delta_bps as u64)
                } else {
                    p.saturating_sub(p / 10_000 * (-delta_bps) as u64)
                };
                if candidate == 0 {
                    continue;
                }
                p = candidate;
                let env = envelope(p, slot);
                match sync_pool(&market, &mut sub_pool, env) {
                    Ok(_) => {}
                    Err(ClearingError::PriceMoveTooLarge) => {}
                    Err(e) => panic!("unexpected sync error {:?}", e),
                }
            }
            _ => unreachable!(),
        }
    }

    // Drain remaining positions at the final price.
    let final_slot = steps as u64 + 1;
    let env = envelope(p, final_slot);
    while let Some(mut pos) = positions.pop() {
        match close_position(&market, &mut sub_pool, env, &mut pos) {
            Ok(out) => {
                withdrawn += out.withdrawable;
            }
            Err(ClearingError::WithdrawableZero) => {
                let _ =
                    force_close_zero_value_position(&market, &mut sub_pool, env, &mut pos, true)
                        .unwrap();
            }
            Err(e) => panic!("drain close error {:?}", e),
        }
    }

    let leftover_pool = sub_pool.long_pool_equity + sub_pool.short_pool_equity;
    let leftover_dust = sub_pool.long_dust + sub_pool.short_dust;
    let leftover_recovery =
        sub_pool.long_dormant.accrued_value_total() + sub_pool.short_dormant.accrued_value_total();

    let total_out = withdrawn + leftover_pool + leftover_dust + leftover_recovery;

    // Conservation must hold exactly: floor rounding moves units into dust,
    // never into withdrawable.
    assert!(
        total_out == deposited,
        "seed {} steps {}: deposited={} total_out={} (pool={}, dust={}, recovery={}, withdrawn={})",
        seed,
        steps,
        deposited,
        total_out,
        leftover_pool,
        leftover_dust,
        leftover_recovery,
        withdrawn,
    );
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        ..ProptestConfig::default()
    })]

    #[test]
    fn prop_conservation_under_random_walk(seed in 0u64..10_000, steps in 50usize..150) {
        run_conservation_scenario(seed, steps);
    }
}

#[test]
fn dust_round_trips_into_protocol() {
    run_conservation_scenario(42, 200);
    run_conservation_scenario(7, 80);
    run_conservation_scenario(13, 300);
}
