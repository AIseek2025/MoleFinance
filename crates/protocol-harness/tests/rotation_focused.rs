//! Focused tests around the active->recovery rotation boundary.
//!
//! These tests deliberately push one side's active pool to zero and
//! observe the indexer's per-position view across the rotation, the
//! subsequent recovery accrual, and a final close. They exercise the
//! exact code path that the broader random-workload runs flagged.

use clearing_core::{Direction, MarketParams, PriceEnvelope};
use molemath::PRICE_SCALE;
use protocol_harness::Harness;

fn envelope(p: u64, slot: u64) -> PriceEnvelope {
    PriceEnvelope {
        p_now: p,
        slot,
        expected_min: p,
        expected_max: p,
    }
}

#[test]
fn long_rotation_then_recovery_indexer_tracks_chain() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;

    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let alice = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let _bob = h
        .open(0, Direction::Short, 100_000_000, envelope(entry, 2))
        .unwrap();
    h.check_invariants().unwrap();

    // Crash long: price drops 30%.
    let p_crash = entry * 70 / 100;
    h.sync(0, envelope(p_crash, 3)).unwrap();
    h.check_invariants().unwrap();

    // Long pool should be at zero, alice rotated to recovery.
    let sp = h.sub_pool(0).unwrap();
    assert_eq!(sp.long_pool_equity, 0, "long should be drained");
    let alice_view_after_crash = h.indexer().position(alice.position_id).unwrap().clone();

    // Reverse: price climbs back. Long-side recovery bucket activates.
    let p_recover = entry; // back to 100
    h.sync(0, envelope(p_recover, 4)).unwrap();
    h.check_invariants().unwrap();

    // Now sync to alice's close price (same as recover price - no further move).
    let alice_pre = h.indexer().position(alice.position_id).unwrap().equity();
    let close = h.close(alice.position_id, envelope(p_recover, 5)).unwrap();
    h.check_invariants().unwrap();

    eprintln!(
        "alice after crash: principal={} L={} R={} eq={}",
        alice_view_after_crash.principal,
        alice_view_after_crash.locked_loss,
        alice_view_after_crash.realized_profit_balance,
        alice_view_after_crash.equity()
    );
    eprintln!(
        "alice pre-close eq={} chain withdrawable={} drift={}",
        alice_pre,
        close.withdrawable,
        alice_pre.abs_diff(close.withdrawable)
    );

    // For a sole long opener after crash + full reversal:
    // - Chain bucket.accrued = total transfer from short during reversal.
    // - Indexer's realized_profit_balance for alice should equal that.
    // - locked_loss should already cover the original crash loss.
    // The drift should be at most a handful of units (floor accumulation).
    assert!(
        alice_pre.abs_diff(close.withdrawable) <= 4,
        "alice indexer drift after rotation+recovery: {}",
        alice_pre.abs_diff(close.withdrawable)
    );
}

#[test]
fn long_rotation_with_two_active_traders_then_recovery() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    // Two longs, one short — making the loser side multi-position.
    let alice = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let bob = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 2))
        .unwrap();
    let _short = h
        .open(0, Direction::Short, 200_000_000, envelope(entry, 3))
        .unwrap();
    h.check_invariants().unwrap();

    // Crash hard.
    let p_crash = entry * 70 / 100;
    h.sync(0, envelope(p_crash, 4)).unwrap();
    h.check_invariants().unwrap();

    // Recover.
    let p_recover = entry;
    h.sync(0, envelope(p_recover, 5)).unwrap();
    h.check_invariants().unwrap();

    let alice_pre = h.indexer().position(alice.position_id).unwrap().equity();
    let bob_pre = h.indexer().position(bob.position_id).unwrap().equity();

    let alice_close = h.close(alice.position_id, envelope(p_recover, 6)).unwrap();
    let bob_close = h.close(bob.position_id, envelope(p_recover, 7)).unwrap();
    h.check_invariants().unwrap();

    eprintln!(
        "alice pre={} chain={} drift={}",
        alice_pre,
        alice_close.withdrawable,
        alice_pre.abs_diff(alice_close.withdrawable)
    );
    eprintln!(
        "bob pre={} chain={} drift={}",
        bob_pre,
        bob_close.withdrawable,
        bob_pre.abs_diff(bob_close.withdrawable)
    );

    // Floor across two positions is at most ~1 per event.
    assert!(alice_pre.abs_diff(alice_close.withdrawable) <= 8);
    assert!(bob_pre.abs_diff(bob_close.withdrawable) <= 8);
}

#[test]
fn long_rotation_with_pre_rotation_loss_drift() {
    // Scenario: alice has accumulated locked_loss BEFORE the rotation,
    // i.e. she suffered a partial loss but the long pool didn't fully
    // zero. Then a second downward sync triggers rotation. Then price
    // reverses. The indexer's pre-rotation locked_loss must still be
    // consistent with the chain's per-share value at close.
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let alice = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let _short = h
        .open(0, Direction::Short, 100_000_000, envelope(entry, 2))
        .unwrap();
    h.check_invariants().unwrap();

    // Step 1: small drop — long takes a partial loss.
    let p1 = entry * 95 / 100;
    h.sync(0, envelope(p1, 3)).unwrap();
    h.check_invariants().unwrap();

    let sp = h.sub_pool(0).unwrap();
    assert!(sp.long_pool_equity > 0, "long pool should not be zero yet");

    // Step 2: drop further to zero out long.
    let p_crash = entry * 70 / 100;
    h.sync(0, envelope(p_crash, 4)).unwrap();
    h.check_invariants().unwrap();
    let sp = h.sub_pool(0).unwrap();
    assert_eq!(sp.long_pool_equity, 0);

    // Step 3: recover partially.
    let p_recover = entry * 90 / 100;
    h.sync(0, envelope(p_recover, 5)).unwrap();
    h.check_invariants().unwrap();

    // Step 4: close.
    let alice_pre = h.indexer().position(alice.position_id).unwrap().equity();
    let alice_close = h.close(alice.position_id, envelope(p_recover, 6)).unwrap();
    h.check_invariants().unwrap();

    eprintln!(
        "pre-rot-loss alice pre={} chain={} drift={}",
        alice_pre,
        alice_close.withdrawable,
        alice_pre.abs_diff(alice_close.withdrawable)
    );
    assert!(alice_pre.abs_diff(alice_close.withdrawable) <= 8);
}

#[test]
fn double_rotation_two_separate_buckets_then_close() {
    // Step 1: long crashes (rotation A creates bucket at p_A).
    // Step 2: price recovers (long-A bucket accrues), then a NEW long
    //         opens.
    // Step 3: long crashes again at a DIFFERENT price (rotation B
    //         creates bucket at p_B).
    // Step 4: price recovers (both buckets activate), then alice (in
    //         bucket A) closes.
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let alice = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let _short_a = h
        .open(0, Direction::Short, 100_000_000, envelope(entry, 2))
        .unwrap();
    h.check_invariants().unwrap();

    // Crash 1.
    let p_crash1 = entry * 70 / 100;
    h.sync(0, envelope(p_crash1, 3)).unwrap();
    h.check_invariants().unwrap();

    // New long enters at p1.
    let p1 = entry * 90 / 100;
    h.sync(0, envelope(p1, 4)).unwrap();
    h.check_invariants().unwrap();

    let _carol = h
        .open(0, Direction::Long, 100_000_000, envelope(p1, 5))
        .unwrap();
    let _short_b = h
        .open(0, Direction::Short, 100_000_000, envelope(p1, 6))
        .unwrap();
    h.check_invariants().unwrap();

    // Crash 2 (carol gets rotated into bucket B).
    let p_crash2 = p1 * 70 / 100;
    h.sync(0, envelope(p_crash2, 7)).unwrap();
    h.check_invariants().unwrap();

    // Big recovery.
    let p_recover = entry;
    h.sync(0, envelope(p_recover, 8)).unwrap();
    h.check_invariants().unwrap();

    // Alice closes.
    let alice_pre = h.indexer().position(alice.position_id).unwrap().equity();
    let alice_close = h.close(alice.position_id, envelope(p_recover, 9)).unwrap();
    h.check_invariants().unwrap();
    eprintln!(
        "double-rot alice pre={} chain={} drift={}",
        alice_pre,
        alice_close.withdrawable,
        alice_pre.abs_diff(alice_close.withdrawable)
    );
    assert!(
        alice_pre.abs_diff(alice_close.withdrawable) <= 16,
        "double-rotation drift: {}",
        alice_pre.abs_diff(alice_close.withdrawable)
    );
}

#[test]
fn multi_position_bucket_one_claims_then_others_recover_more() {
    // Two longs rotate together into the same bucket. One claims fully
    // after partial recovery. Then more recovery comes for the bucket
    // (now with the remaining position's shares only). Finally the
    // remaining position closes. The drift on the closing position
    // should still be tiny.
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let alice = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let bob = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 2))
        .unwrap();
    let _short = h
        .open(0, Direction::Short, 200_000_000, envelope(entry, 3))
        .unwrap();

    let p_crash = entry * 60 / 100;
    h.sync(0, envelope(p_crash, 4)).unwrap();
    h.check_invariants().unwrap();

    // Partial recovery: bucket accrues some.
    let p_recover_1 = entry * 80 / 100;
    h.sync(0, envelope(p_recover_1, 5)).unwrap();
    h.check_invariants().unwrap();

    // Alice claims her share.
    let claim = h
        .claim_recovery(alice.position_id, envelope(p_recover_1, 6))
        .unwrap();
    eprintln!("alice claim={}", claim.withdrawable);
    h.check_invariants().unwrap();

    // More recovery: now only bob is in the bucket.
    let p_recover_2 = entry;
    h.sync(0, envelope(p_recover_2, 7)).unwrap();
    h.check_invariants().unwrap();

    // Bob closes.
    let bob_pre = h.indexer().position(bob.position_id).unwrap().equity();
    let bob_close = h.close(bob.position_id, envelope(p_recover_2, 8)).unwrap();
    h.check_invariants().unwrap();
    eprintln!(
        "bob-after-alice-claim pre={} chain={} drift={}",
        bob_pre,
        bob_close.withdrawable,
        bob_pre.abs_diff(bob_close.withdrawable)
    );
    assert!(
        bob_pre.abs_diff(bob_close.withdrawable) <= 16,
        "post-claim multi-position drift: {}",
        bob_pre.abs_diff(bob_close.withdrawable)
    );
}

#[test]
fn rotation_with_intermediate_short_loss_long_recovery() {
    // The random-workload bug pattern: open new shorts AFTER the long
    // rotation, so the short side accumulates new principal that can
    // be transferred into the long-recovery bucket on the next reversal.
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let alice = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let _short_a = h
        .open(0, Direction::Short, 100_000_000, envelope(entry, 2))
        .unwrap();

    // Crash to zero out long.
    let p_crash = entry * 60 / 100;
    h.sync(0, envelope(p_crash, 3)).unwrap();
    h.check_invariants().unwrap();

    // New short opens AT the crash price (active short pool grows).
    let _short_b = h
        .open(0, Direction::Short, 100_000_000, envelope(p_crash, 4))
        .unwrap();
    h.check_invariants().unwrap();

    // Big recovery: long-recovery bucket now competes with active longs
    // (none) for the short outflow. All inflow goes to recovery.
    let p_recover = entry;
    h.sync(0, envelope(p_recover, 5)).unwrap();
    h.check_invariants().unwrap();

    let alice_pre = h.indexer().position(alice.position_id).unwrap().equity();
    let alice_close = h.close(alice.position_id, envelope(p_recover, 6)).unwrap();
    h.check_invariants().unwrap();

    eprintln!(
        "intermediate-short alice pre={} chain={} drift={}",
        alice_pre,
        alice_close.withdrawable,
        alice_pre.abs_diff(alice_close.withdrawable)
    );
    assert!(
        alice_pre.abs_diff(alice_close.withdrawable) <= 16,
        "intermediate-short drift: {}",
        alice_pre.abs_diff(alice_close.withdrawable)
    );
}

#[test]
fn rotation_with_new_long_after_recovery_starts() {
    // After rotation, recovery starts. THEN a new long opens. The new
    // long should start fresh (active_generation = post-rotation gen).
    // The old (rotated) alice should keep accruing recovery only.
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let alice = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let _short_a = h
        .open(0, Direction::Short, 100_000_000, envelope(entry, 2))
        .unwrap();

    let p_crash = entry * 60 / 100;
    h.sync(0, envelope(p_crash, 3)).unwrap();
    h.check_invariants().unwrap();

    // Partial recovery: bucket accrues, but pool also rebuilds.
    let p_partial = entry * 80 / 100;
    h.sync(0, envelope(p_partial, 4)).unwrap();
    h.check_invariants().unwrap();

    // New long enters fresh (post-rotation generation).
    let _new_long = h
        .open(0, Direction::Long, 100_000_000, envelope(p_partial, 5))
        .unwrap();
    h.check_invariants().unwrap();

    // Further recovery (price climbs to entry).
    let p_full_recover = entry;
    h.sync(0, envelope(p_full_recover, 6)).unwrap();
    h.check_invariants().unwrap();

    let alice_pre = h.indexer().position(alice.position_id).unwrap().equity();
    let alice_close = h.close(alice.position_id, envelope(p_full_recover, 7)).unwrap();
    h.check_invariants().unwrap();
    eprintln!(
        "new-long-after-rec alice pre={} chain={} drift={}",
        alice_pre,
        alice_close.withdrawable,
        alice_pre.abs_diff(alice_close.withdrawable)
    );
    assert!(
        alice_pre.abs_diff(alice_close.withdrawable) <= 16,
        "new-long-after-rec drift: {}",
        alice_pre.abs_diff(alice_close.withdrawable)
    );
}
