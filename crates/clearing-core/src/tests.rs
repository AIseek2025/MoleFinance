//! Smoke tests for the clearing engine. The full property test suite lives
//! in `tests/` once the basic invariants pass.

use std::sync::atomic::{AtomicU64, Ordering};

use molemath::PRICE_SCALE;

use super::*;
use crate::event::EngineEvent;

/// Test-local id counter so each call to [`open_position`] in this file
/// gets a unique synthetic position id without forcing the caller to
/// thread an extra argument.
static POSITION_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_id() -> u64 {
    POSITION_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Trampoline around [`engine::open_position`] that auto-assigns a
/// monotonically increasing `position_id`.
fn open_position(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    envelope: PriceEnvelope,
    direction: Direction,
    gross_amount: u64,
) -> ClearingResult<(Position, OpenOutcome)> {
    crate::engine::open_position(
        market,
        sub_pool,
        envelope,
        direction,
        gross_amount,
        next_id(),
    )
}

use crate::error::ClearingResult;

fn envelope(p: u64, slot: u64) -> PriceEnvelope {
    PriceEnvelope {
        p_now: p,
        slot,
        expected_min: p,
        expected_max: p,
    }
}

#[test]
fn fresh_pool_first_sync_seeds_price() {
    // A pristine pool starts at last_price == 0. The first sync must
    // seed the price instead of dividing by zero in the move check.
    let market = MarketParams::sample();
    let mut sub_pool = SubPool::new(0, 0, 0);
    assert_eq!(sub_pool.last_price, 0);

    let seed = 100 * PRICE_SCALE;
    let out = crate::engine::sync_pool(&market, &mut sub_pool, envelope(seed, 7)).unwrap();
    assert_eq!(sub_pool.last_price, seed, "first sync should seed last_price");
    assert_eq!(sub_pool.last_sync_slot, 7);
    assert!(out.events.is_empty(), "seeding sync has nothing to distribute");

    // A subsequent in-band move is now subject to the normal move check.
    let next = seed + seed / 100; // +1%
    crate::engine::sync_pool(&market, &mut sub_pool, envelope(next, 8)).unwrap();
    assert_eq!(sub_pool.last_price, next);
}

#[test]
fn fresh_pool_first_open_succeeds() {
    // The very first open on a market routes through sync_pool; without
    // the seeding branch this would fail with DivByZero.
    let market = MarketParams::sample();
    let mut sub_pool = SubPool::new(0, 0, 0);
    let entry = 100 * PRICE_SCALE;
    let (_pos, _open) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        100_000_000u64,
    )
    .expect("first open on a fresh pool should succeed");
    assert_eq!(sub_pool.last_price, entry);
}

#[test]
fn open_close_round_trip_zero_pnl() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    let principal = 100_000_000u64; // 1 USD * 1e8

    let (mut pos, open) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        principal,
    )
    .unwrap();
    assert_eq!(open.shares_minted, principal as u128);
    assert_eq!(sub_pool.long_pool_equity, principal as u128);
    assert_eq!(sub_pool.long_active_shares, principal as u128);

    // Close at the same price -> zero PnL, recover principal.
    let close = close_position(&market, &mut sub_pool, envelope(entry, 2), &mut pos).unwrap();
    assert_eq!(close.withdrawable, principal as u128);
    assert_eq!(sub_pool.long_pool_equity, 0);
    assert_eq!(sub_pool.long_active_shares, 0);
}

#[test]
fn long_gain_short_loss_settles_via_sync() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    let stake = 100_000_000u64;
    let (mut long_pos, _) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        stake,
    )
    .unwrap();
    let (mut short_pos, _) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Short,
        stake,
    )
    .unwrap();

    let p1 = entry + entry / 100; // +1 %
    sync_pool(&market, &mut sub_pool, envelope(p1, 3)).unwrap();

    // Long pool grew, short pool shrank by the same amount (capped by short pool).
    assert!(sub_pool.long_pool_equity > stake as u128);
    assert!(sub_pool.short_pool_equity < stake as u128);
    assert_eq!(
        sub_pool.long_pool_equity + sub_pool.short_pool_equity,
        2 * stake as u128
    );

    // Long closes at a profit.
    let close_long =
        close_position(&market, &mut sub_pool, envelope(p1, 4), &mut long_pos).unwrap();
    assert!(close_long.withdrawable > stake as u128);

    // Short closes at a loss but with positive remaining principal.
    let close_short =
        close_position(&market, &mut sub_pool, envelope(p1, 5), &mut short_pos).unwrap();
    assert!(close_short.withdrawable < stake as u128);

    // Conservation: total paid out = 2 * stake (within rounding dust).
    let total = close_long.withdrawable + close_short.withdrawable;
    assert!(total <= 2 * stake as u128);
    assert!(2 * stake as u128 - total <= 4); // <= a few units of dust
}

#[test]
fn long_pool_zeroes_and_rotates_to_recovery() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000; // allow up to 500% per sync for test
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    let stake = 100_000_000u64;
    let (mut long_pos, _) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        stake,
    )
    .unwrap();
    let (_short_pos, _) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Short,
        stake,
    )
    .unwrap();

    // Price drops 30% with 10x leverage -> long would lose 300% notional; cap is short_pool_equity.
    let p1 = entry - entry * 30 / 100;
    sync_pool(&market, &mut sub_pool, envelope(p1, 3)).unwrap();

    // Long pool should be 0, active rotated to recovery.
    assert_eq!(sub_pool.long_pool_equity, 0);
    assert_eq!(sub_pool.long_active_shares, 0);
    assert!(sub_pool.long_recovery_shares > 0);
    assert_eq!(sub_pool.long_active_generation, 1);

    // Long position closes -> migrates to recovery and forfeits (no value).
    let res = force_close_zero_value_position(
        &market,
        &mut sub_pool,
        envelope(p1, 4),
        &mut long_pos,
        true,
    )
    .unwrap();
    assert!(res.recovery_shares_burned > 0 || res.active_shares_burned > 0);
    assert_eq!(long_pos.status, PositionStatus::Closed);
}

#[test]
fn dilution_safety_rejects_when_pool_too_small() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    // Contrived state: pool decayed to ~ 0.001 % of share count -> below 1 bps.
    sub_pool.long_pool_equity = 1; // 1 raw unit
    sub_pool.long_active_shares = 10_000_000; // 1e7 shares
    sub_pool.long_active_notional = 0;

    // dilution_safety = 1 bps -> require pool * 10_000 >= shares * 1.
    // 1 * 10_000 = 10_000, 10_000_000 * 1 = 10_000_000. 10_000 < 10_000_000 -> reject.
    let err = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        100_000_000,
    )
    .unwrap_err();
    assert_eq!(err, ClearingError::DilutionRiskTooHigh);
}

#[test]
fn open_emits_pool_sync_then_position_opened() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    let (_pos, outcome) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        100_000_000,
    )
    .unwrap();

    // Same-price sync emits no events; only PositionOpened follows.
    assert_eq!(outcome.events.len(), 1);
    assert!(matches!(outcome.events[0], EngineEvent::PositionOpened(_)));
}

#[test]
fn sync_with_price_change_emits_pool_sync() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    let stake = 100_000_000u64;
    open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        stake,
    )
    .unwrap();
    open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Short,
        stake,
    )
    .unwrap();

    let p1 = entry + entry / 100;
    let outcome = sync_pool(&market, &mut sub_pool, envelope(p1, 3)).unwrap();
    assert_eq!(outcome.events.len(), 1);
    let EngineEvent::PoolSync(ev) = &outcome.events[0] else {
        panic!("expected PoolSync");
    };
    assert_eq!(ev.p_last, entry);
    assert_eq!(ev.p_now, p1);
    assert!(ev.long_to_pool_inflow > 0);
    assert_eq!(ev.short_to_pool_inflow, 0);
    assert_eq!(ev.long_active_shares_before, stake as u128);
    assert_eq!(ev.short_active_shares_before, stake as u128);
}

#[test]
fn rotate_emits_active_rotated_event() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);
    let stake = 100_000_000u64;

    open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        stake,
    )
    .unwrap();
    open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Short,
        stake,
    )
    .unwrap();

    let p1 = entry * 7 / 10;
    let outcome = sync_pool(&market, &mut sub_pool, envelope(p1, 3)).unwrap();

    let has_rotate = outcome
        .events
        .iter()
        .any(|e| matches!(e, EngineEvent::ActiveRotatedToRecovery(_)));
    assert!(has_rotate, "expected ActiveRotatedToRecovery, got {:?}", outcome.events);
    assert!(outcome.long_rotated_to_recovery);
}

#[test]
fn force_close_emits_force_closed_event() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);
    let stake = 100_000_000u64;

    let (mut long_pos, _) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        stake,
    )
    .unwrap();
    open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Short,
        stake,
    )
    .unwrap();

    let p1 = entry * 7 / 10;
    sync_pool(&market, &mut sub_pool, envelope(p1, 3)).unwrap();

    let outcome = force_close_zero_value_position(
        &market,
        &mut sub_pool,
        envelope(p1, 4),
        &mut long_pos,
        true,
    )
    .unwrap();
    let has_fc = outcome
        .events
        .iter()
        .any(|e| matches!(e, EngineEvent::PositionForceClosed(_)));
    assert!(has_fc, "expected PositionForceClosed event");
}

#[test]
fn dust_harvest_emits_event() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);
    sub_pool.long_dust = 1_234;
    let outcome = harvest_dust(&market, &mut sub_pool, Direction::Long).unwrap();
    assert_eq!(outcome.amount, 1_234);
    assert_eq!(outcome.events.len(), 1);
    let EngineEvent::DustHarvested(ev) = &outcome.events[0] else {
        panic!("expected DustHarvested event");
    };
    assert_eq!(ev.amount, 1_234);
    assert_eq!(ev.direction, Direction::Long);
}

#[test]
fn dilution_safety_passes_at_steady_state() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    // pool_equity == active_shares -> ratio == 1, far above 1 bps threshold.
    let stake = 100_000_000u64;
    open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        stake,
    )
    .unwrap();
    open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Long,
        stake,
    )
    .unwrap();
    assert_eq!(sub_pool.long_pool_equity, 2 * stake as u128);
    assert_eq!(sub_pool.long_active_shares, 2 * stake as u128);
}

#[test]
fn shares_minted_zero_rejected() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    // Construct: pool has many funds, few shares -> shares_minted floor = 0.
    sub_pool.long_pool_equity = 1_000_000_000_000_000u128;
    sub_pool.long_active_shares = 1; // tiny share count
    sub_pool.long_active_notional = 0;

    // dilution_safety check first: pool * 1 >= shares * 10_000? 1e15 >= 10_000 yes.
    // mint = principal * 1 / 1e15 = floor(1e7 / 1e15) = 0.
    let err = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        market.min_margin,
    )
    .unwrap_err();
    assert_eq!(err, ClearingError::SharesMintedTooSmall);
}

#[test]
fn price_protection_rejects_out_of_band() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    let bad_envelope = PriceEnvelope {
        p_now: entry,
        slot: 1,
        expected_min: entry + 1, // entry < expected_min
        expected_max: entry + 100,
    };
    let err = sync_pool(&market, &mut sub_pool, bad_envelope).unwrap_err();
    assert_eq!(err, ClearingError::PriceProtectionFailed);
}

#[test]
fn excessive_price_move_rejected() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 1_000; // 10 %
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    let p1 = entry * 2; // 100 % jump
    let env = envelope(p1, 1);
    let err = sync_pool(&market, &mut sub_pool, env).unwrap_err();
    assert_eq!(err, ClearingError::PriceMoveTooLarge);
}

// ----- Wave-3: lazy distribute mode + pre_sync_dormant_bucket -----

/// Build a sub-pool with one rotated long bucket (anchor 80) carrying 10x
/// recovery shares, so the next downward sync can distribute to that bucket.
fn build_long_bucket_at_70_then_recover_at_80(
    market: &MarketParams,
) -> (SubPool, u64) {
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);
    let stake = 100_000_000u64;

    open_position(
        market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        stake,
    )
    .unwrap();
    open_position(
        market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Short,
        stake,
    )
    .unwrap();

    // p drops 30% -> long pool zeroes, rotates into a dormant bucket.
    let p_low = entry - entry * 30 / 100;
    sync_pool(market, &mut sub_pool, envelope(p_low, 3)).unwrap();
    assert_eq!(sub_pool.long_pool_equity, 0);
    assert!(sub_pool.long_recovery_shares > 0);

    (sub_pool, p_low)
}

#[test]
fn lazy_distribute_skips_inactive_bucket_until_pre_sync() {
    // Eager mode: every distribute bumps every bucket's last_applied_index.
    // Lazy mode: a bucket whose anchor is below current price (i.e. not
    // activated) should NOT be touched at all by sync_pool.
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    market.dormant_distribute_mode = DistributeMode::Lazy;

    let (mut sub_pool, _p_low) = build_long_bucket_at_70_then_recover_at_80(&market);
    // After rotate the bucket exists; it is anchored at the rotation
    // price (== p_low). When p moves further DOWN, the bucket stays
    // *deactivated* for the long side (long buckets need p > anchor).
    let bucket_tick = sub_pool.long_dormant.bucket_ticks()[0];
    let pre_last_applied = sub_pool.long_dormant.get(bucket_tick).unwrap().last_applied_index;
    let pre_event_index = sub_pool.long_dormant.next_event_index();

    // Push another sync where short side wins again -> ledger entry is
    // appended and inactive long bucket is skipped.
    let p_lower = sub_pool.last_price - sub_pool.last_price / 100; // -1 % more
    sync_pool(&market, &mut sub_pool, envelope(p_lower, 4)).unwrap();

    let post_event_index = sub_pool.long_dormant.next_event_index();
    let post_bucket = sub_pool.long_dormant.get(bucket_tick).unwrap();

    assert!(
        post_event_index >= pre_event_index,
        "ledger should not regress"
    );
    // Bucket's last_applied_index is UNCHANGED — lazy skipped it.
    assert_eq!(post_bucket.last_applied_index, pre_last_applied);
}

#[test]
fn pre_sync_dormant_bucket_emits_event_and_advances_index() {
    // Drive a full rotation -> sync upward -> long bucket gains a
    // ledger entry. After distribute_lazy the bucket is one entry behind
    // (the new entry was pushed AFTER it was caught up). pre_sync must
    // close that gap and emit the catch-up event.
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    market.dormant_distribute_mode = DistributeMode::Lazy;

    let (mut sub_pool, p_low) = build_long_bucket_at_70_then_recover_at_80(&market);
    let bucket_tick = sub_pool.long_dormant.bucket_ticks()[0];
    let bucket_anchor = sub_pool.long_dormant.get(bucket_tick).unwrap().anchor_price;

    // Sync upward above anchor — long bucket activates, distribute_lazy
    // pushes a single ledger entry, bucket lags by exactly 1.
    let p_up = bucket_anchor + bucket_anchor / 100; // +1 %
    sync_pool(&market, &mut sub_pool, envelope(p_low, 4)).unwrap(); // no-op same price
    sync_pool(&market, &mut sub_pool, envelope(p_up, 5)).unwrap();

    let pending_before = sub_pool.long_dormant.next_event_index()
        - sub_pool
            .long_dormant
            .get(bucket_tick)
            .unwrap()
            .last_applied_index;
    assert!(
        pending_before >= 1,
        "expected at least one pending entry, got {}",
        pending_before
    );

    let outcome =
        pre_sync_dormant_bucket(&market, &mut sub_pool, Direction::Long, bucket_tick, 6).unwrap();

    assert_eq!(outcome.events.len(), 1);
    assert!(matches!(
        outcome.events[0],
        EngineEvent::DormantBucketPendingApplied(_)
    ));
    assert_eq!(outcome.pending_remaining, 0);
    assert!(outcome.events_applied >= 1);
    assert_eq!(
        sub_pool.long_dormant.get(bucket_tick).unwrap().last_applied_index,
        sub_pool.long_dormant.next_event_index()
    );

    // Calling pre_sync again is a clean no-op.
    let again =
        pre_sync_dormant_bucket(&market, &mut sub_pool, Direction::Long, bucket_tick, 7).unwrap();
    assert_eq!(again.events_applied, 0);
    assert_eq!(again.pending_remaining, 0);
    assert_eq!(again.pre_accrued_value, again.post_accrued_value);
}

#[test]
fn pre_sync_dormant_bucket_rejects_when_pending_exceeds_budget() {
    // With max_pending_apply_per_tx == 0, any non-empty backlog must be
    // rejected. We don't need to engineer a multi-entry backlog; one
    // pending entry already exceeds a budget of zero.
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    market.dormant_distribute_mode = DistributeMode::Lazy;
    market.max_pending_apply_per_tx = 0;

    let (mut sub_pool, p_low) = build_long_bucket_at_70_then_recover_at_80(&market);
    let bucket_tick = sub_pool.long_dormant.bucket_ticks()[0];
    let bucket_anchor = sub_pool.long_dormant.get(bucket_tick).unwrap().anchor_price;

    // Generate at least one pending ledger entry on the long bucket.
    let p_up = bucket_anchor + bucket_anchor / 100;
    sync_pool(&market, &mut sub_pool, envelope(p_low, 4)).unwrap();
    sync_pool(&market, &mut sub_pool, envelope(p_up, 5)).unwrap();

    let pending = sub_pool.long_dormant.next_event_index()
        - sub_pool
            .long_dormant
            .get(bucket_tick)
            .unwrap()
            .last_applied_index;
    assert!(
        pending >= 1,
        "expected at least one pending entry to test budget, got {}",
        pending
    );

    let err =
        pre_sync_dormant_bucket(&market, &mut sub_pool, Direction::Long, bucket_tick, 6)
            .unwrap_err();
    assert_eq!(err, ClearingError::DormantPendingBudgetExceeded);
}

#[test]
fn pre_sync_dormant_bucket_missing_returns_error() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    let err = pre_sync_dormant_bucket(&market, &mut sub_pool, Direction::Long, 42, 1).unwrap_err();
    assert_eq!(err, ClearingError::DormantBucketMissing);
}
