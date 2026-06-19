//! Capacity-bounded distribution ledger: back-pressure and recovery.
//!
//! These tests pin two invariants on the on-chain ring buffer:
//!
//! 1. **Back-pressure on overflow**: when the live ledger window
//!    reaches `max_distribution_ledger_size` and no live bucket has
//!    advanced past the head, both `distribute` and `distribute_lazy`
//!    return `LedgerCapacityExceeded` *without* mutating any state.
//!    This is what protects the on-chain ring account from
//!    silently overwriting unapplied entries.
//!
//! 2. **Recovery via keeper-driven catch-up**: once any live bucket
//!    advances past the head — typically because the keeper called
//!    `apply_pending_to_bucket` for the lagging bucket — the next
//!    `distribute` call compacts the ring and proceeds normally.
//!    No funds are lost; only the sync that hit the overflow needs
//!    to be retried.
//!
//! Together these guarantee that the on-chain account never has to
//! grow, and that the system always has a clear unblocking action.

use clearing_core::{ClearingError, Direction, DormantStore};
use molemath::PRICE_SCALE;

const MAX_LEDGER: u32 = 8;

/// Bucket fixture: insert a long bucket at the given anchor.
/// Activates at p > anchor.
fn long_bucket(store: &mut DormantStore, anchor: u64, tick: i64) {
    store
        .insert_or_merge(tick, anchor, 1_000_000, 100_000_000, 1, 1024)
        .unwrap();
}

/// Saturate the ring by distributing only on a fast bucket; a lagging
/// bucket stays out-of-band so the watermark can't advance.
///
/// - Fast bucket anchor = `fast_anchor` (activates at p > fast_anchor).
/// - Lagging bucket anchor = `lagging_anchor` ≫ p; never activated.
///
/// Returns the saturated store + both ticks.
fn saturate_ring_lazy() -> (DormantStore, i64, i64) {
    let mut store = DormantStore::new(Direction::Long);
    let fast = 80i64;
    let lagging = 200i64;
    long_bucket(&mut store, 80 * PRICE_SCALE, fast);
    long_bucket(&mut store, 200 * PRICE_SCALE, lagging);
    let p_now = 100 * PRICE_SCALE; // activates fast only
    for _ in 0..MAX_LEDGER {
        store
            .distribute_lazy(p_now, 1_000, MAX_LEDGER)
            .expect("first MAX_LEDGER calls fit in ring");
    }
    assert_eq!(store.ledger_len() as u32, MAX_LEDGER);
    (store, fast, lagging)
}

#[test]
fn distribute_lazy_returns_capacity_error_when_ring_is_full() {
    let (mut store, _, _) = saturate_ring_lazy();
    let p_now = 100 * PRICE_SCALE;
    let err = store
        .distribute_lazy(p_now, 1_000, MAX_LEDGER)
        .expect_err("ring full, lagging bucket pinning watermark");
    assert_eq!(err, ClearingError::LedgerCapacityExceeded);
}

#[test]
fn distribute_eager_returns_capacity_error_when_ring_is_full() {
    // Eager-distribute advances *every* bucket's last_applied each
    // call, so a single-bucket eager scenario can never saturate.
    // We saturate via the lazy path (lagging-bucket pin) and then
    // attempt an eager distribute; back-pressure must still fire.
    let (mut store, _, _) = saturate_ring_lazy();
    let p_now = 100 * PRICE_SCALE;
    let err = store
        .distribute(p_now, 1_000, MAX_LEDGER)
        .expect_err("ring full, eager distribute must back-pressure");
    assert_eq!(err, ClearingError::LedgerCapacityExceeded);
}

#[test]
fn capacity_error_does_not_mutate_state() {
    let (mut store, fast, lagging) = saturate_ring_lazy();

    // Snapshot before the failing call.
    let snap_ledger_len = store.ledger_len();
    let snap_accrued_total = store.accrued_value_total();
    let snap_fast_last = store.get(fast).unwrap().last_applied_index;
    let snap_lagging_last = store.get(lagging).unwrap().last_applied_index;
    let snap_fast_accrued = store.get(fast).unwrap().accrued_value;

    // Should fail without side effects.
    let p_now = 100 * PRICE_SCALE;
    let _ = store
        .distribute_lazy(p_now, 1_000, MAX_LEDGER)
        .unwrap_err();

    assert_eq!(store.ledger_len(), snap_ledger_len);
    assert_eq!(store.accrued_value_total(), snap_accrued_total);
    assert_eq!(store.get(fast).unwrap().last_applied_index, snap_fast_last);
    assert_eq!(
        store.get(lagging).unwrap().last_applied_index,
        snap_lagging_last
    );
    assert_eq!(store.get(fast).unwrap().accrued_value, snap_fast_accrued);
}

#[test]
fn keeper_drains_lagging_bucket_then_distribute_succeeds() {
    let (mut store, fast, lagging) = saturate_ring_lazy();
    let p_now = 100 * PRICE_SCALE;

    // Confirm the back-pressure fires.
    let err = store
        .distribute_lazy(p_now, 1_000, MAX_LEDGER)
        .expect_err("ring saturated, expected back-pressure");
    assert_eq!(err, ClearingError::LedgerCapacityExceeded);

    // Keeper catches up the lagging bucket. The lazy bucket walks
    // every ledger entry — which are all no-ops for it because it's
    // never been activated — and advances its last_applied_index to
    // the head. fast also gets caught up so the watermark covers
    // everything.
    let applied = store.apply_pending_to_bucket(lagging).unwrap();
    assert_eq!(applied as u32, MAX_LEDGER, "all entries replayed for lagging");
    let _ = store.apply_pending_to_bucket(fast).unwrap();

    // Retry: distribute_lazy's compact-on-overflow path frees the
    // ring and the call succeeds.
    let receipt = store
        .distribute_lazy(p_now, 1_000, MAX_LEDGER)
        .expect("retry must succeed after keeper catch-up");
    assert!(receipt.allocated > 0 || receipt.residual > 0);
    assert!(
        store.ledger_len() <= MAX_LEDGER as usize,
        "ring must remain within capacity post-recovery"
    );
}

#[test]
fn capacity_zero_rejects_every_distribute() {
    // Defensive: max_distribution_ledger_size = 0 must reject all
    // distributes. The init handler enforces > 0 but a misconfigured
    // engine call MUST NOT silently bypass back-pressure.
    let mut store = DormantStore::new(Direction::Long);
    long_bucket(&mut store, 90 * PRICE_SCALE, 90);

    let p_above = 100 * PRICE_SCALE;
    let err = store.distribute(p_above, 100, 0).expect_err("cap = 0");
    assert_eq!(err, ClearingError::LedgerCapacityExceeded);
    let err = store.distribute_lazy(p_above, 100, 0).expect_err("cap = 0");
    assert_eq!(err, ClearingError::LedgerCapacityExceeded);
}
