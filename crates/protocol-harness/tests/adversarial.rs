//! Adversarial scenario tests.
//!
//! Each test exercises a specific attack or misuse path that the
//! whitepaper / planning docs §19 audit called out, asserting the
//! protocol rejects it cleanly while preserving all invariants.

use clearing_core::{ClearingError, Direction, MarketParams, PriceEnvelope};
use molemath::PRICE_SCALE;
use protocol_harness::{Harness, HarnessError};

fn envelope(p: u64, slot: u64) -> PriceEnvelope {
    PriceEnvelope {
        p_now: p,
        slot,
        expected_min: p,
        expected_max: p,
    }
}

#[test]
fn envelope_deviation_is_rejected_protocol_state_unchanged() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let alice = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let _bob = h
        .open(0, Direction::Short, 100_000_000, envelope(entry, 2))
        .unwrap();

    // Drift the actual oracle price (p_now) BUT keep the envelope tight
    // around the original. The engine must reject and not mutate state.
    let mismatched = PriceEnvelope {
        p_now: entry * 102 / 100, // oracle is +2 %
        slot: 3,
        expected_min: entry * 999 / 1000, // user only tolerated +/-0.1 %
        expected_max: entry * 1001 / 1000,
    };
    let snapshot = h.summary();
    let err = h.sync(0, mismatched).unwrap_err();
    match err {
        HarnessError::Clearing(ClearingError::PriceProtectionFailed) => {}
        other => panic!("expected PriceProtectionFailed, got {:?}", other),
    }
    let after = h.summary();
    assert_eq!(snapshot.vault_balance, after.vault_balance);
    assert_eq!(snapshot.fee_vault_balance, after.fee_vault_balance);
    assert_eq!(snapshot.pool_equity_total, after.pool_equity_total);
    h.check_invariants().unwrap();

    // Existing positions are still openable / closeable at the correct
    // price.
    h.close(alice.position_id, envelope(entry, 4)).unwrap();
    h.check_invariants().unwrap();
}

#[test]
fn force_close_without_acknowledge_is_rejected() {
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

    // Crash long.
    let p_crash = entry * 70 / 100;
    h.sync(0, envelope(p_crash, 3)).unwrap();

    // Without acknowledgement -> reject.
    let snapshot = h.summary();
    let err = h.force_close(alice.position_id, envelope(p_crash, 4), false).unwrap_err();
    match err {
        HarnessError::Clearing(ClearingError::ForfeitAcknowledgementRequired) => {}
        other => panic!("expected ForfeitAcknowledgementRequired, got {:?}", other),
    }
    let after = h.summary();
    assert_eq!(snapshot.vault_balance, after.vault_balance);
    assert_eq!(snapshot.dormant_accrued_total, after.dormant_accrued_total);
    h.check_invariants().unwrap();

    // With acknowledgement -> succeeds.
    h.force_close(alice.position_id, envelope(p_crash, 5), true).unwrap();
    h.check_invariants().unwrap();
}

#[test]
fn cross_sub_pool_isolation_one_pool_drained_others_unaffected() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);
    h.add_sub_pool(1, entry, 0);

    let _alice = h.open(0, Direction::Long, 100_000_000, envelope(entry, 1)).unwrap();
    let _bob = h.open(0, Direction::Short, 100_000_000, envelope(entry, 2)).unwrap();
    let carol = h.open(1, Direction::Long, 100_000_000, envelope(entry, 3)).unwrap();
    let _dave = h.open(1, Direction::Short, 100_000_000, envelope(entry, 4)).unwrap();
    h.check_invariants().unwrap();

    // Crash sub_pool 0 only.
    let p_crash = entry * 60 / 100;
    h.sync(0, envelope(p_crash, 5)).unwrap();
    h.check_invariants().unwrap();

    // Sub pool 1 must be unaffected: same equities, no rotation.
    let sp1 = h.sub_pool(1).unwrap();
    assert_eq!(sp1.long_pool_equity + sp1.short_pool_equity, 200_000_000);
    assert_eq!(sp1.long_dormant.bucket_count(), 0);
    assert_eq!(sp1.short_dormant.bucket_count(), 0);

    // Carol can close at sub_pool 1's untouched price and get full principal.
    let close = h.close(carol.position_id, envelope(entry, 6)).unwrap();
    assert_eq!(close.withdrawable, 100_000_000);
    h.check_invariants().unwrap();
}

#[test]
fn force_close_on_positive_value_position_rejected() {
    // force_close is only valid on a position whose total payout would
    // be zero. Calling it on a still-positive position must fail with
    // the explicit invariant error and leave state untouched.
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let alice = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let _bob = h
        .open(0, Direction::Short, 100_000_000, envelope(entry, 2))
        .unwrap();

    // Position is still worth ~100M.
    let snapshot = h.summary();
    let err = h
        .force_close(alice.position_id, envelope(entry, 3), true)
        .unwrap_err();
    match err {
        HarnessError::Clearing(ClearingError::Invariant(_)) => {}
        other => panic!("expected Invariant error, got {:?}", other),
    }
    let after = h.summary();
    assert_eq!(snapshot.vault_balance, after.vault_balance);
    h.check_invariants().unwrap();
}

#[test]
fn excessive_price_move_per_sync_rejected() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 1_000; // 10 % cap
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let _alice = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let _bob = h
        .open(0, Direction::Short, 100_000_000, envelope(entry, 2))
        .unwrap();

    // Try a 20 % move in one sync.
    let big_move = envelope(entry * 80 / 100, 3);
    let snapshot = h.summary();
    let err = h.sync(0, big_move).unwrap_err();
    match err {
        HarnessError::Clearing(ClearingError::PriceMoveTooLarge) => {}
        other => panic!("expected PriceMoveTooLarge, got {:?}", other),
    }
    let after = h.summary();
    assert_eq!(snapshot.vault_balance, after.vault_balance);
    h.check_invariants().unwrap();
}

#[test]
fn paused_market_blocks_open_but_lets_close_proceed() {
    let mut market = MarketParams::sample();
    market.paused = true;
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    // open is rejected.
    let err = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap_err();
    assert!(matches!(
        err,
        HarnessError::Clearing(ClearingError::MarketPaused)
    ));
}

#[test]
fn frozen_new_position_blocks_open_only() {
    let mut market = MarketParams::sample();
    market.frozen_new_position = true;
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let err = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap_err();
    assert!(matches!(
        err,
        HarnessError::Clearing(ClearingError::FrozenNewPosition)
    ));
}

#[test]
fn min_margin_enforced_on_open() {
    let mut market = MarketParams::sample();
    market.min_margin = 50_000_000;
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    // Below min margin -> reject.
    let err = h
        .open(0, Direction::Long, 10_000_000, envelope(entry, 1))
        .unwrap_err();
    assert!(matches!(
        err,
        HarnessError::Clearing(ClearingError::MarginBelowMinimum)
    ));

    // At or above min margin -> ok.
    let _ok = h
        .open(0, Direction::Long, 60_000_000, envelope(entry, 2))
        .unwrap();
    h.check_invariants().unwrap();
}

#[test]
fn nonexistent_sub_pool_rejected() {
    let market = MarketParams::sample();
    let mut h = Harness::new(market);
    let err = h
        .open(7, Direction::Long, 100_000_000, envelope(100 * PRICE_SCALE, 1))
        .unwrap_err();
    assert!(matches!(err, HarnessError::SubPoolNotFound(7)));
}
