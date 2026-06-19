//! Smoke tests for the indexer state machine.

use super::*;
use clearing_core::{
    close_position, open_position, sync_pool, Direction, MarketParams, PriceEnvelope, SubPool,
};
use molemath::PRICE_SCALE;

fn envelope(p: u64, slot: u64) -> PriceEnvelope {
    PriceEnvelope {
        p_now: p,
        slot,
        expected_min: p,
        expected_max: p,
    }
}

#[test]
fn open_close_roundtrip_zero_pnl_indexer_view_matches_chain() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);
    let mut indexer = IndexerState::new();

    let principal = 100_000_000u64;
    let (mut pos, open_outcome) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        principal,
        7,
    )
    .unwrap();
    indexer.apply_all(&open_outcome.events).unwrap();

    let close_outcome =
        close_position(&market, &mut sub_pool, envelope(entry, 2), &mut pos).unwrap();
    indexer.apply_all(&close_outcome.events).unwrap();

    let view = indexer.position(7).unwrap();
    assert!(view.closed);
    assert_eq!(view.locked_loss, 0);
    assert_eq!(view.realized_profit_balance, 0);
    // Withdrawable = principal - locked_loss + realized_profit_balance + recovery_value.
    let derived_withdrawable = view.equity();
    assert_eq!(derived_withdrawable, close_outcome.withdrawable);
}

#[test]
fn long_gain_short_loss_indexer_matches_per_position_oracle() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);
    let mut indexer = IndexerState::new();
    let stake = 100_000_000u64;

    let (mut alice, ev) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        stake,
        1,
    )
    .unwrap();
    indexer.apply_all(&ev.events).unwrap();
    let (mut bob, ev) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Short,
        stake,
        2,
    )
    .unwrap();
    indexer.apply_all(&ev.events).unwrap();

    let p1 = entry + entry / 100; // +1%
    let sync = sync_pool(&market, &mut sub_pool, envelope(p1, 3)).unwrap();
    indexer.apply_all(&sync.events).unwrap();

    // After +1% sync: Alice (long) gained, Bob (short) lost.
    let alice_view = indexer.position(1).unwrap();
    let bob_view = indexer.position(2).unwrap();
    assert!(alice_view.realized_profit_balance > 0);
    assert!(bob_view.locked_loss > 0);
    assert_eq!(bob_view.realized_profit_balance, 0);

    // Conservation at indexer level.
    let alice_equity = alice_view.equity();
    let bob_equity = bob_view.equity();
    let total = alice_equity + bob_equity;
    assert!(total <= 2 * stake as u128);
    assert!(2 * stake as u128 - total <= 4); // a few units of dust.

    // Alice closes -> withdrawable matches the indexer projection that was
    // captured BEFORE the close event mutated the bucket aggregate.
    let alice_pre_close_equity = alice_view.equity();
    let bob_pre_close_equity = bob_view.equity();
    let close_alice =
        close_position(&market, &mut sub_pool, envelope(p1, 4), &mut alice).unwrap();
    indexer.apply_all(&close_alice.events).unwrap();
    {
        let alice_view_after = indexer.position(1).unwrap();
        assert!(alice_view_after.closed);
    }
    assert_eq!(alice_pre_close_equity, close_alice.withdrawable);

    let close_bob = close_position(&market, &mut sub_pool, envelope(p1, 5), &mut bob).unwrap();
    indexer.apply_all(&close_bob.events).unwrap();
    {
        let bob_view_after = indexer.position(2).unwrap();
        assert!(bob_view_after.closed);
    }
    assert_eq!(bob_pre_close_equity, close_bob.withdrawable);
    assert!(close_bob.withdrawable < stake as u128);
}

#[test]
fn rotate_migrates_position_to_recovery_in_indexer() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);
    let mut indexer = IndexerState::new();
    let stake = 100_000_000u64;

    let (_long_pos, ev) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        stake,
        10,
    )
    .unwrap();
    indexer.apply_all(&ev.events).unwrap();
    let (_short_pos, ev) = open_position(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Short,
        stake,
        11,
    )
    .unwrap();
    indexer.apply_all(&ev.events).unwrap();

    // Crash long.
    let p1 = entry * 7 / 10;
    let sync = sync_pool(&market, &mut sub_pool, envelope(p1, 3)).unwrap();
    indexer.apply_all(&sync.events).unwrap();

    let long_view = indexer.position(10).unwrap();
    assert_eq!(long_view.active_shares, 0);
    assert!(long_view.recovery_shares > 0);
    assert!(long_view.recovery_bucket_tick.is_some());
    // Long's locked_loss equals the entire principal (zero pool).
    assert_eq!(long_view.locked_loss, stake as u128);
    assert_eq!(long_view.realized_profit_balance, 0);

    let short_view = indexer.position(11).unwrap();
    // Short's profit is bounded by long's principal.
    assert!(short_view.realized_profit_balance > 0);
    assert!(short_view.realized_profit_balance <= stake as u128);
    assert_eq!(short_view.locked_loss, 0);
}

#[test]
fn sub_pool_stats_aggregates_active_and_dormant() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);
    let mut indexer = IndexerState::new();
    let stake = 100_000_000u64;

    // alice long, bob short, both at entry.
    for (id, dir) in [(101u64, Direction::Long), (102u64, Direction::Short)] {
        let (_p, ev) = open_position(
            &market,
            &mut sub_pool,
            envelope(entry, id),
            dir,
            stake,
            id,
        )
        .unwrap();
        indexer.apply_all(&ev.events).unwrap();
    }

    // Stats before any sync: only active OI on both sides.
    let stats = indexer.sub_pool_stats(0).unwrap();
    assert_eq!(stats.long.active_position_count, 1);
    assert_eq!(stats.short.active_position_count, 1);
    assert_eq!(stats.long.active_principal, stake as u128);
    assert_eq!(stats.short.active_principal, stake as u128);
    assert_eq!(stats.long.dormant_bucket_count, 0);
    assert_eq!(stats.short.dormant_bucket_count, 0);

    // Crash long, creating a long dormant bucket.
    let crash = entry * 7 / 10;
    let sync = sync_pool(&market, &mut sub_pool, envelope(crash, 3)).unwrap();
    indexer.apply_all(&sync.events).unwrap();

    let stats = indexer.sub_pool_stats(0).unwrap();
    // Long is now dormant; short is still active.
    assert_eq!(stats.long.active_position_count, 0);
    assert_eq!(stats.short.active_position_count, 1);
    assert_eq!(stats.long.dormant_position_count, 1);
    assert_eq!(stats.long.dormant_bucket_count, 1);
    assert!(stats.long.dormant_recovery_shares > 0);
    assert!(stats.long.dormant_total_notional > 0);
    // The bucket has not yet received any recovery distributions, so
    // accrued_value is 0.
    assert_eq!(stats.long.dormant_accrued_value, 0);

    // Inventory snapshot has exactly one long bucket at the crash tick.
    let inventory = indexer.dormant_inventory(0).unwrap();
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].direction, Direction::Long);
    assert!(inventory[0].total_recovery_shares > 0);

    // Project recovery outstanding at a hypothetical higher price ($85)
    // -- the long bucket should be activated and report a non-zero
    // outstanding claim.
    let projected = indexer
        .projected_recovery_outstanding(0, Direction::Long, entry * 85 / 100)
        .unwrap();
    assert!(projected > 0, "long bucket activated at p=85 must have outstanding>0");

    // Project at a price BELOW the crash price (still below anchor)
    // -- bucket NOT activated, outstanding == 0.
    let projected_low = indexer
        .projected_recovery_outstanding(0, Direction::Long, entry * 65 / 100)
        .unwrap();
    assert_eq!(projected_low, 0);
}

#[test]
fn sub_pool_stats_returns_none_for_unknown_id() {
    let indexer = IndexerState::new();
    assert!(indexer.sub_pool_stats(99).is_none());
    assert!(indexer.dormant_inventory(99).is_none());
}
