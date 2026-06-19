//! Smoke tests for the harness. The full random-walk + adversarial
//! suites live under `tests/`.

use super::*;
use clearing_core::{Direction, MarketParams, PriceEnvelope};
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
fn open_close_round_trip_preserves_everything() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let s0 = h.summary();
    assert_eq!(s0.total_deposits, 0);
    assert_eq!(s0.vault_balance, 0);
    h.check_invariants().unwrap();

    let open = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    h.check_invariants().unwrap();
    let s1 = h.summary();
    assert_eq!(s1.total_deposits, 100_000_000);
    // Sample market has open_fee_bps = 0, so fee vault stays empty.
    assert_eq!(s1.fee_vault_balance, 0);
    assert_eq!(s1.vault_balance, 100_000_000);

    let close = h.close(open.position_id, envelope(entry, 2)).unwrap();
    h.check_invariants().unwrap();
    assert_eq!(close.withdrawable, 100_000_000);
    let s2 = h.summary();
    assert_eq!(s2.total_withdrawals, 100_000_000);
    assert_eq!(s2.vault_balance, 0);
    assert_eq!(s2.fee_vault_balance, 0);
}

#[test]
fn long_gain_short_loss_with_indexer_parity() {
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
    h.check_invariants().unwrap();

    let p1 = entry + entry / 100;
    h.sync(0, envelope(p1, 3)).unwrap();
    h.check_invariants().unwrap();

    let close_a = h.close(alice.position_id, envelope(p1, 4)).unwrap();
    h.check_invariants().unwrap();
    assert_eq!(close_a.indexer_pre_equity, close_a.withdrawable);
    let close_b = h.close(bob.position_id, envelope(p1, 5)).unwrap();
    h.check_invariants().unwrap();
    assert_eq!(close_b.indexer_pre_equity, close_b.withdrawable);

    let s = h.summary();
    // Conservation: total_deposits = total_withdrawals + state (modulo dust).
    let total_paid = s.total_withdrawals + s.vault_balance + s.fee_vault_balance;
    assert_eq!(total_paid, s.total_deposits);
    assert!(s.vault_balance <= 4); // residual dust only
}

#[test]
fn duplicate_position_id_collision_is_impossible_via_harness() {
    // The harness assigns ids monotonically, so two opens always get
    // different ids.
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let a = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let b = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 2))
        .unwrap();
    assert_ne!(a.position_id, b.position_id);
}

#[test]
fn close_unknown_position_errors() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let err = h.close(42, envelope(entry, 1)).unwrap_err();
    assert!(matches!(err, HarnessError::PositionNotFound(42)));
}

#[test]
fn close_twice_errors_on_second_call() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    let open = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    h.close(open.position_id, envelope(entry, 2)).unwrap();
    let err = h.close(open.position_id, envelope(entry, 3)).unwrap_err();
    assert!(matches!(
        err,
        HarnessError::Clearing(ClearingError::PositionNotOpen)
    ));
}

#[test]
fn rotate_then_force_close_keeps_invariants() {
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

    // Crash long.
    let p1 = entry * 7 / 10;
    h.sync(0, envelope(p1, 3)).unwrap();
    h.check_invariants().unwrap();

    h.force_close(alice.position_id, envelope(p1, 4), true).unwrap();
    h.check_invariants().unwrap();
}

#[test]
fn harvest_moves_dust_from_vault_to_fee_vault() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);
    // Inject some dust manually for the test.
    {
        let sp = h.sub_pools.get_mut(&0).unwrap();
        sp.long_dust = 1_234;
    }
    // Vault must reflect dust to keep invariants happy.
    h.vault_balance += 1_234;
    h.total_deposits += 1_234;
    h.check_invariants().unwrap();

    let amount = h.harvest_dust(0, Direction::Long).unwrap();
    assert_eq!(amount, 1_234);
    h.check_invariants().unwrap();
    let (vault, fee_vault) = h.balances();
    assert_eq!(vault, 0);
    assert_eq!(fee_vault, 1_234);
}
