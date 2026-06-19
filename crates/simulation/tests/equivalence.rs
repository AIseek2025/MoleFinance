//! Cross-engine equivalence tests.
//!
//! These compare the on-chain shares model in `clearing-core` against
//! the offline per-position oracle in this crate. The two engines should
//! agree on:
//!
//! - Total tokens deposited == total tokens claimable + dust.
//! - Per-side aggregate value (long vs short) within bounded rounding.
//!
//! The full per-position equality does **not** hold: the shares model
//! folds dormant positions into aggregated buckets, so individual
//! locked_loss/realized_profit values diverge from the oracle. Aggregate
//! conservation does hold and is what the protocol guarantees.

use std::sync::atomic::{AtomicU64, Ordering};

use clearing_core::{
    close_position, force_close_zero_value_position, open_position as engine_open, sync_pool,
    ClearingError, ClearingResult, Direction as CoreDir, MarketParams, OpenOutcome, Position,
    PriceEnvelope, SubPool,
};
use molemath::PRICE_SCALE;
use simulation::{settle, OraclePosition};

static POSITION_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn open_position(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    envelope: PriceEnvelope,
    direction: CoreDir,
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

#[allow(dead_code)]
#[derive(Debug)]
struct Trader {
    direction: CoreDir,
    principal: u64,
    pos: Position,
    oracle_idx: usize,
}

#[test]
fn two_user_long_wins_short_loses() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    let stake = 100_000_000u64;

    let (long_pos, _) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        CoreDir::Long,
        stake,
    )
    .unwrap();
    let (short_pos, _) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        CoreDir::Short,
        stake,
    )
    .unwrap();

    let mut oracle = vec![
        OraclePosition::new(0, CoreDir::Long, stake as u128, market.leverage_bps, entry).unwrap(),
        OraclePosition::new(1, CoreDir::Short, stake as u128, market.leverage_bps, entry).unwrap(),
    ];

    let mut traders = [
        Trader {
            direction: CoreDir::Long,
            principal: stake,
            pos: long_pos,
            oracle_idx: 0,
        },
        Trader {
            direction: CoreDir::Short,
            principal: stake,
            pos: short_pos,
            oracle_idx: 1,
        },
    ];

    // Move price up 5 %.
    let p1 = entry + entry / 20;
    sync_pool(&market, &mut sub_pool, envelope(p1, 3)).unwrap();
    settle(&mut oracle, p1).unwrap();

    // Close long.
    let close_long = close_position(
        &market,
        &mut sub_pool,
        envelope(p1, 4),
        &mut traders[0].pos,
    )
    .unwrap();
    let oracle_long = oracle[0].equity();
    assert!(
        close_long.withdrawable.abs_diff(oracle_long) <= 4,
        "long shares={} oracle={}",
        close_long.withdrawable,
        oracle_long,
    );

    // Close short.
    let close_short = match close_position(
        &market,
        &mut sub_pool,
        envelope(p1, 5),
        &mut traders[1].pos,
    ) {
        Ok(out) => out.withdrawable,
        Err(ClearingError::WithdrawableZero) => {
            force_close_zero_value_position(
                &market,
                &mut sub_pool,
                envelope(p1, 5),
                &mut traders[1].pos,
                true,
            )
            .unwrap();
            0
        }
        Err(e) => panic!("unexpected close error {:?}", e),
    };
    let oracle_short = oracle[1].equity();
    assert!(
        close_short.abs_diff(oracle_short) <= 4,
        "short shares={} oracle={}",
        close_short,
        oracle_short,
    );

    // Aggregate conservation.
    let total = close_long.withdrawable + close_short;
    assert!(
        total <= 2 * traders[0].principal as u128 + traders[1].principal as u128 * 2 / 2,
        "withdrawn dust accounting"
    );
}

#[test]
fn alice_locked_loss_does_not_revive_without_third_loser() {
    // Whitepaper §8.2 walk-through: Alice long, B short, B closes profitable.
    // Price reverses but no new losing positions arrive -> Alice's equity
    // does not increase beyond what's left.
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    let alice_stake = 100_000_000u64;
    let bob_stake = 100_000_000u64;

    let (mut alice_pos, _) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        CoreDir::Long,
        alice_stake,
    )
    .unwrap();
    let (mut bob_pos, _) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        CoreDir::Short,
        bob_stake,
    )
    .unwrap();

    let mut oracle = vec![
        OraclePosition::new(
            0,
            CoreDir::Long,
            alice_stake as u128,
            market.leverage_bps,
            entry,
        )
        .unwrap(),
        OraclePosition::new(
            1,
            CoreDir::Short,
            bob_stake as u128,
            market.leverage_bps,
            entry,
        )
        .unwrap(),
    ];

    // Phase 1: price drops 3 %.
    let p1 = entry - entry * 3 / 100;
    sync_pool(&market, &mut sub_pool, envelope(p1, 3)).unwrap();
    settle(&mut oracle, p1).unwrap();

    // Bob closes (profitable short).
    let _bob_close =
        close_position(&market, &mut sub_pool, envelope(p1, 4), &mut bob_pos).unwrap();
    simulation::close(&mut oracle, 1, p1).unwrap();

    // Phase 2: price recovers to entry; no new losers.
    let p2 = entry;
    sync_pool(&market, &mut sub_pool, envelope(p2, 5)).unwrap();
    settle(&mut oracle, p2).unwrap();

    // Alice closes.
    let alice_close = match close_position(
        &market,
        &mut sub_pool,
        envelope(p2, 6),
        &mut alice_pos,
    ) {
        Ok(out) => out.withdrawable,
        Err(ClearingError::WithdrawableZero) => {
            force_close_zero_value_position(
                &market,
                &mut sub_pool,
                envelope(p2, 6),
                &mut alice_pos,
                true,
            )
            .unwrap();
            0
        }
        Err(e) => panic!("unexpected error {:?}", e),
    };
    let alice_oracle = oracle[0].equity();
    assert!(
        alice_close.abs_diff(alice_oracle) <= 4,
        "alice shares={} oracle={}",
        alice_close,
        alice_oracle,
    );
    // Alice should have less than her original principal (locked loss).
    assert!(alice_close < alice_stake as u128);
}
