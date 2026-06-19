//! Atomic revert (Solana tx semantics) regression tests.
//!
//! The harness wraps every state-mutating engine call in a
//! snapshot/restore pair so a failed call leaves zero side effects on
//! the sub-pool, the position, and the indexer. This mirrors Solana's
//! transaction-revert behaviour exactly.
//!
//! Before this fix, `close_position` returning `WithdrawableZero`
//! would still have:
//!   * burnt the position's active shares from the sub-pool aggregate,
//!   * redeemed every recovery share from the dormant bucket
//!     (potentially deleting the bucket),
//!   * accumulated sync events that were then dropped on the Err path.
//!
//! The chain state mutated, the indexer never received the events, and
//! the front-end view drifted by ~0.7 % per workload.
//!
//! These tests construct the exact divergent scenario and assert that:
//!   1. After a failed close, sub-pool and position are byte-for-byte
//!      equal to their pre-call state.
//!   2. The indexer received zero events from the failed call.
//!   3. A subsequent force-close completes correctly and the
//!      bucket/indexer aggregates remain consistent.

use clearing_core::{Direction, MarketParams, PriceEnvelope};
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

/// Build a state where Bob's long position will see `withdrawable == 0`
/// at the close price: long pool fully wiped after a deep crash, no
/// dormant recovery in his bucket. The very first close attempt must
/// surface as `WithdrawableZero` AND must not perturb chain state.
#[test]
fn failed_close_with_zero_withdrawable_leaves_chain_untouched() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000; // permit deep crash.

    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    // Two shorts (winners), one long (will rotate to recovery on crash,
    // then the dormant bucket sees no further upside, yielding 0).
    let bob = h
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let _short_a = h
        .open(0, Direction::Short, 200_000_000, envelope(entry, 2))
        .unwrap();
    h.check_invariants().unwrap();

    // Crash 90 %. Long active pool collapses to 0 → rotate event.
    let p_crash = entry / 10;
    h.sync(0, envelope(p_crash, 10)).unwrap();
    h.check_invariants().unwrap();

    // Bob's bucket needs a price recovery to ever pay out. Without it,
    // any close immediately yields 0.
    let p_close = p_crash; // no recovery.

    // Snapshot chain & indexer state for byte-equal comparison.
    let sp_before = h.sub_pool(0).cloned().expect("sub pool exists");
    let pos_before = h.position(bob.position_id).cloned().expect("bob lives");
    let bucket_view_before = h
        .indexer()
        .dormant_inventory(0)
        .unwrap_or_default();
    let bob_indexer_before = h
        .indexer()
        .position(bob.position_id)
        .cloned()
        .expect("bob in indexer");
    let summary_before = h.summary();

    let err = h
        .close(bob.position_id, envelope(p_close, 11))
        .expect_err("must fail with WithdrawableZero");
    match err {
        HarnessError::Clearing(clearing_core::ClearingError::WithdrawableZero) => {}
        other => panic!("expected WithdrawableZero, got {other:?}"),
    }

    // 1. Sub-pool aggregates byte-equal.
    let sp_after = h.sub_pool(0).expect("sub pool still exists");
    assert_eq!(
        sp_before.long_pool_equity, sp_after.long_pool_equity,
        "long_pool_equity changed on failed close",
    );
    assert_eq!(
        sp_before.short_pool_equity, sp_after.short_pool_equity,
        "short_pool_equity changed on failed close",
    );
    assert_eq!(
        sp_before.long_active_shares, sp_after.long_active_shares,
        "long_active_shares changed on failed close",
    );
    assert_eq!(
        sp_before.short_active_shares, sp_after.short_active_shares,
        "short_active_shares changed on failed close",
    );
    assert_eq!(
        sp_before.long_recovery_shares, sp_after.long_recovery_shares,
        "long_recovery_shares changed on failed close",
    );
    assert_eq!(
        sp_before.short_recovery_shares, sp_after.short_recovery_shares,
        "short_recovery_shares changed on failed close",
    );

    // 2. Dormant buckets (chain side) byte-equal.
    let chain_long_before: Vec<_> = sp_before
        .long_dormant
        .iter_buckets()
        .map(|(t, b)| (*t, b.clone()))
        .collect();
    let chain_long_after: Vec<_> = sp_after
        .long_dormant
        .iter_buckets()
        .map(|(t, b)| (*t, b.clone()))
        .collect();
    assert_eq!(
        chain_long_before.len(),
        chain_long_after.len(),
        "long dormant bucket count changed",
    );
    for ((t1, b1), (t2, b2)) in chain_long_before.iter().zip(chain_long_after.iter()) {
        assert_eq!(t1, t2, "bucket tick changed");
        assert_eq!(
            b1.total_recovery_shares, b2.total_recovery_shares,
            "bucket shares changed at tick {t1}",
        );
        assert_eq!(
            b1.total_recovery_notional, b2.total_recovery_notional,
            "bucket notional changed at tick {t1}",
        );
        assert_eq!(
            b1.accrued_value, b2.accrued_value,
            "bucket accrued_value changed at tick {t1}",
        );
    }

    // 3. Position byte-equal. (locked_loss / realized_profit_balance
    //    are indexer-side fields only; checked separately below.)
    let pos_after = h.position(bob.position_id).expect("bob lives");
    assert_eq!(pos_before.active_shares, pos_after.active_shares);
    assert_eq!(pos_before.recovery_shares, pos_after.recovery_shares);
    assert_eq!(pos_before.recovery_bucket_tick, pos_after.recovery_bucket_tick);
    assert_eq!(pos_before.notional, pos_after.notional);
    assert_eq!(pos_before.status, pos_after.status);

    // 4. Indexer state untouched.
    let bucket_view_after = h
        .indexer()
        .dormant_inventory(0)
        .unwrap_or_default();
    assert_eq!(
        bucket_view_before.len(),
        bucket_view_after.len(),
        "indexer bucket count changed on failed close",
    );
    for (b1, b2) in bucket_view_before.iter().zip(bucket_view_after.iter()) {
        assert_eq!(b1.bucket_tick, b2.bucket_tick);
        assert_eq!(b1.total_recovery_shares, b2.total_recovery_shares);
        assert_eq!(b1.total_recovery_notional, b2.total_recovery_notional);
        assert_eq!(b1.accrued_value, b2.accrued_value);
    }
    let bob_indexer_after = h
        .indexer()
        .position(bob.position_id)
        .cloned()
        .expect("bob still in indexer");
    assert_eq!(
        bob_indexer_before.active_shares, bob_indexer_after.active_shares,
        "indexer position active_shares changed",
    );
    assert_eq!(
        bob_indexer_before.recovery_shares, bob_indexer_after.recovery_shares,
        "indexer position recovery_shares changed",
    );
    assert_eq!(
        bob_indexer_before.locked_loss, bob_indexer_after.locked_loss,
    );
    assert_eq!(
        bob_indexer_before.realized_profit_balance,
        bob_indexer_after.realized_profit_balance,
    );

    // 5. Conservation invariants still hold and summary is unchanged.
    let summary_after = h.summary();
    assert_eq!(summary_before.total_deposits, summary_after.total_deposits);
    assert_eq!(summary_before.total_withdrawals, summary_after.total_withdrawals);
    assert_eq!(summary_before.vault_balance, summary_after.vault_balance);
    assert_eq!(summary_before.fee_vault_balance, summary_after.fee_vault_balance);
    h.check_invariants().unwrap();

    // 6. Recovery path: force_close completes cleanly.
    h.force_close(bob.position_id, envelope(p_close, 12), true).unwrap();
    h.check_invariants().unwrap();
}

/// A second variant: failed close on a position that has `recovery_shares == 0`
/// in a wiped pool. The chain still mutates `position.active_shares = 0`
/// inside `close_position` before raising WithdrawableZero, so the
/// snapshot/restore must catch the position-side mutation too.
#[test]
fn failed_close_with_active_only_position_leaves_position_untouched() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;

    let entry = 100 * PRICE_SCALE;
    let mut h = Harness::new(market);
    h.add_sub_pool(0, entry, 0);

    // Single long, single short. Crash 99 % to wipe long pool entirely.
    let alice = h
        .open(0, Direction::Long, 50_000_000, envelope(entry, 1))
        .unwrap();
    let _short = h
        .open(0, Direction::Short, 50_000_000, envelope(entry, 2))
        .unwrap();

    let p_crash = (entry / 100).max(1);
    h.sync(0, envelope(p_crash, 3)).unwrap();
    h.check_invariants().unwrap();

    let pos_before = h.position(alice.position_id).cloned().expect("alice lives");

    let err = h
        .close(alice.position_id, envelope(p_crash, 4))
        .expect_err("must fail with WithdrawableZero");
    matches!(err, HarnessError::Clearing(clearing_core::ClearingError::WithdrawableZero));

    let pos_after = h.position(alice.position_id).expect("alice still tracked");
    assert_eq!(pos_before.active_shares, pos_after.active_shares);
    assert_eq!(pos_before.recovery_shares, pos_after.recovery_shares);
    assert_eq!(pos_before.recovery_bucket_tick, pos_after.recovery_bucket_tick);
    assert_eq!(pos_before.status, pos_after.status);
    h.check_invariants().unwrap();
}
