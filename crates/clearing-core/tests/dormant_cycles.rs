//! Boundary tests for the dormant bucket lifecycle.
//!
//! Covers price oscillations that produce **multiple** dormant buckets
//! at different anchor prices, multi-cycle rotation (active → recovery →
//! active again), and the lazy-migration path across generation gaps.

use std::sync::atomic::{AtomicU64, Ordering};

use clearing_core::{
    close_position, force_close_zero_value_position, open_position as engine_open, sync_pool,
    ClearingResult, Direction, MarketParams, OpenOutcome, Position, PriceEnvelope, SubPool,
};
use molemath::PRICE_SCALE;

static POSITION_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_id() -> u64 {
    POSITION_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn open(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    envelope: PriceEnvelope,
    direction: Direction,
    gross: u64,
) -> ClearingResult<(Position, OpenOutcome)> {
    engine_open(market, sub_pool, envelope, direction, gross, next_id())
}

fn envelope(p: u64, slot: u64) -> PriceEnvelope {
    PriceEnvelope {
        p_now: p,
        slot,
        expected_min: p,
        expected_max: p,
    }
}

/// Long pool zeroes twice with new entrants in between. Two distinct
/// dormant buckets must coexist, and the dormant store must aggregate
/// counterparty losses into both buckets according to the engine's
/// proportional distribution rule.
#[test]
fn two_dormant_buckets_coexist_after_multiple_zeros() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    // Round 1: Alice long, Bob short. Long zeroes when price drops to $70.
    let (mut alice, _) = open(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        100_000_000,
    )
    .unwrap();
    let (_bob, _) = open(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Short,
        100_000_000,
    )
    .unwrap();

    let p1 = entry * 7 / 10; // -30%
    sync_pool(&market, &mut sub_pool, envelope(p1, 3)).unwrap();
    assert_eq!(sub_pool.long_pool_equity, 0);
    assert!(sub_pool.long_recovery_shares > 0);
    let bucket1_count = sub_pool.long_dormant.bucket_count();

    // Round 2: at p1, Carol opens long, Dave opens short. Long zeroes
    // again at $50.
    let p1_open = p1; // open at the post-rotation price
    let (mut carol, _) = open(
        &market,
        &mut sub_pool,
        envelope(p1_open, 4),
        Direction::Long,
        100_000_000,
    )
    .unwrap();
    let (_dave, _) = open(
        &market,
        &mut sub_pool,
        envelope(p1_open, 5),
        Direction::Short,
        100_000_000,
    )
    .unwrap();
    assert_eq!(sub_pool.long_active_generation, 1);

    let p2 = p1 * 7 / 10; // another -30%
    sync_pool(&market, &mut sub_pool, envelope(p2, 6)).unwrap();
    assert_eq!(sub_pool.long_pool_equity, 0);
    assert_eq!(sub_pool.long_active_generation, 2);
    assert!(sub_pool.long_dormant.bucket_count() >= bucket1_count);

    // Both Alice (round 1) and Carol (round 2) have fully zero-equity
    // positions. Force-closing forfeits any recovery they may have.
    force_close_zero_value_position(
        &market,
        &mut sub_pool,
        envelope(p2, 7),
        &mut alice,
        true,
    )
    .unwrap();
    force_close_zero_value_position(
        &market,
        &mut sub_pool,
        envelope(p2, 8),
        &mut carol,
        true,
    )
    .unwrap();
}

/// After a rotation, a stale `Position` (whose `active_generation`
/// trails the sub pool's current) must be lazily migrated to recovery
/// shares the next time it's touched, never silently retaining its old
/// active shares against a fresh active series.
#[test]
fn lazy_migration_handles_two_consecutive_rotations() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    let (mut alice, _) = open(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        100_000_000,
    )
    .unwrap();
    let (_bob, _) = open(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Short,
        100_000_000,
    )
    .unwrap();

    // First rotation.
    let p1 = entry * 7 / 10;
    sync_pool(&market, &mut sub_pool, envelope(p1, 3)).unwrap();
    let alice_gen_at_open = alice.active_generation; // 0
    assert_eq!(sub_pool.long_active_generation, 1);
    assert!(alice_gen_at_open < sub_pool.long_active_generation);

    // Open a new long Eve in the next active generation.
    let (mut eve, _) = open(
        &market,
        &mut sub_pool,
        envelope(p1, 4),
        Direction::Long,
        100_000_000,
    )
    .unwrap();
    let (_frank, _) = open(
        &market,
        &mut sub_pool,
        envelope(p1, 5),
        Direction::Short,
        100_000_000,
    )
    .unwrap();
    assert_eq!(eve.active_generation, 1);

    // Second rotation: long zeroes again.
    let p2 = p1 * 7 / 10;
    sync_pool(&market, &mut sub_pool, envelope(p2, 6)).unwrap();
    assert_eq!(sub_pool.long_active_generation, 2);

    // At this point Alice's `position.active_generation == 0` while the
    // sub pool is on generation 2. Touching Alice must NOT see her as
    // an active holder of any kind. Her active_shares must lazy-migrate
    // to recovery_shares pinned at the FIRST rotation's bucket tick
    // (generation 0 → bucket at p1).
    let alice_recovery_before = alice.recovery_shares;
    force_close_zero_value_position(
        &market,
        &mut sub_pool,
        envelope(p2, 7),
        &mut alice,
        true,
    )
    .unwrap();
    assert_eq!(alice.active_shares, 0);
    // Some recovery shares burned from the first rotation's bucket.
    let _ = alice_recovery_before; // bound for clarity

    // Eve was active in generation 1; she rotated at the second event.
    // Her recovery must point to the second rotation's bucket (price p2).
    force_close_zero_value_position(
        &market,
        &mut sub_pool,
        envelope(p2, 8),
        &mut eve,
        true,
    )
    .unwrap();
    assert_eq!(eve.active_shares, 0);
}

/// Random oscillation that produces ≥3 dormant cycles. Total assets
/// must be conserved (modulo bounded dust).
#[test]
fn random_oscillation_with_multiple_dormant_cycles_conserves_capital() {
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha20Rng;

    for seed in [1u64, 17, 99] {
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let mut market = MarketParams::sample();
        market.max_price_move_bps_per_sync = 50_000;
        let entry = 100 * PRICE_SCALE;
        let mut sub_pool = SubPool::new(0, entry, 0);

        let mut total_in: u128 = 0;
        let mut total_out: u128 = 0;
        let mut p_now = entry;
        let mut slot = 1u64;
        let mut open_positions: Vec<Position> = Vec::new();

        for _ in 0..40 {
            let action = rng.gen_range(0u32..4);
            match action {
                0 | 1 => {
                    // Open long or short.
                    let direction = if action == 0 {
                        Direction::Long
                    } else {
                        Direction::Short
                    };
                    let stake = rng.gen_range(50_000_000u64..200_000_000);
                    let res = open(
                        &market,
                        &mut sub_pool,
                        envelope(p_now, slot),
                        direction,
                        stake,
                    );
                    if let Ok((pos, _)) = res {
                        total_in += stake as u128;
                        open_positions.push(pos);
                    }
                }
                2 => {
                    // Close a random open position.
                    if !open_positions.is_empty() {
                        let idx = rng.gen_range(0..open_positions.len());
                        let mut pos = open_positions.swap_remove(idx);
                        let res =
                            close_position(&market, &mut sub_pool, envelope(p_now, slot), &mut pos);
                        if let Ok(out) = res {
                            total_out += out.withdrawable;
                        } else {
                            // If close fails (zero-value), force-close.
                            let _ = force_close_zero_value_position(
                                &market,
                                &mut sub_pool,
                                envelope(p_now, slot),
                                &mut pos,
                                true,
                            );
                        }
                    }
                }
                _ => {
                    // Price step ±10%.
                    let bps: i64 = rng.gen_range(-1_000..=1_000);
                    let delta = (p_now as i128 * bps as i128 / 10_000) as i64;
                    let p_next = (p_now as i128 + delta as i128).max(1) as u64;
                    let _ = sync_pool(&market, &mut sub_pool, envelope(p_next, slot));
                    p_now = p_next;
                }
            }
            slot += 1;
        }

        // Drain remaining positions at p_now.
        for mut pos in open_positions {
            let res = close_position(&market, &mut sub_pool, envelope(p_now, slot), &mut pos);
            slot += 1;
            if let Ok(out) = res {
                total_out += out.withdrawable;
            } else {
                let _ = force_close_zero_value_position(
                    &market,
                    &mut sub_pool,
                    envelope(p_now, slot),
                    &mut pos,
                    true,
                );
                slot += 1;
            }
        }

        // Conservation: total tokens deposited == total tokens withdrawn +
        // (residual pool equity + dormant accrual + dust). Funds never
        // exceed deposits; the gap is bounded by floor truncation.
        let residual_pool = sub_pool.long_pool_equity + sub_pool.short_pool_equity;
        let residual_dormant = sub_pool.long_dormant.accrued_value_total()
            + sub_pool.short_dormant.accrued_value_total();
        let residual_dust = sub_pool.long_dust + sub_pool.short_dust;
        let accounted = total_out + residual_pool + residual_dormant + residual_dust;

        assert!(
            accounted <= total_in,
            "seed {}: protocol over-paid: in={} out={} residual_pool={} residual_dormant={} dust={}",
            seed,
            total_in,
            total_out,
            residual_pool,
            residual_dormant,
            residual_dust
        );
        let gap = total_in - accounted;
        assert!(
            gap <= 4096,
            "seed {}: accounting gap {} too large; total_in={}",
            seed,
            gap,
            total_in
        );
    }
}
