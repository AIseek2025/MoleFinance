//! Runtime invariant checks.
//!
//! Mirrors `Docs/Planning/18-shares模型实现细则与边界条件.md` §11. These
//! checks must pass after every state-mutating instruction. Failure
//! triggers `ClearingError::Invariant(reason)` which on chain emits an
//! `InvariantViolation` event and forces auto-pause.

use crate::error::{ClearingError, ClearingResult};
use crate::market::SubPool;

/// Run all invariants for a single subpool.
pub fn check_subpool_invariants(sub_pool: &SubPool) -> ClearingResult<()> {
    inv2_active_shares_zero_iff_pool_zero(sub_pool, "long")?;
    inv2_active_shares_zero_iff_pool_zero_short(sub_pool)?;
    inv4_active_notional_nonneg(sub_pool)?;
    inv5_recovery_shares_match_buckets(sub_pool)?;
    inv6_dormant_ledger_consistency(sub_pool)?;
    Ok(())
}

fn inv2_active_shares_zero_iff_pool_zero(sub_pool: &SubPool, _label: &str) -> ClearingResult<()> {
    if (sub_pool.long_pool_equity == 0) != (sub_pool.long_active_shares == 0) {
        return Err(ClearingError::Invariant(
            "long pool equity / active shares zero-iff invariant",
        ));
    }
    Ok(())
}

fn inv2_active_shares_zero_iff_pool_zero_short(sub_pool: &SubPool) -> ClearingResult<()> {
    if (sub_pool.short_pool_equity == 0) != (sub_pool.short_active_shares == 0) {
        return Err(ClearingError::Invariant(
            "short pool equity / active shares zero-iff invariant",
        ));
    }
    Ok(())
}

fn inv4_active_notional_nonneg(_sub_pool: &SubPool) -> ClearingResult<()> {
    // `u128` cannot be negative; this is a structural invariant. Kept here
    // to mirror the documentation list and serve as a marker for the future
    // signed-arithmetic invariants (e.g. on PnL ledgers).
    Ok(())
}

fn inv5_recovery_shares_match_buckets(sub_pool: &SubPool) -> ClearingResult<()> {
    let long_total = sub_pool.long_dormant.total_recovery_shares();
    if long_total != sub_pool.long_recovery_shares {
        return Err(ClearingError::Invariant(
            "long recovery_shares does not equal sum of bucket recovery shares",
        ));
    }
    let short_total = sub_pool.short_dormant.total_recovery_shares();
    if short_total != sub_pool.short_recovery_shares {
        return Err(ClearingError::Invariant(
            "short recovery_shares does not equal sum of bucket recovery shares",
        ));
    }
    Ok(())
}

/// Inv6: every per-direction `DormantStore` is internally consistent —
/// `accrued_value_total == sum(bucket.accrued_value)`, no bucket points
/// past `next_event_index`, no bucket points into a GC'd window, and the
/// ledger length is consistent with `next_event_index - ledger_gc_offset`.
fn inv6_dormant_ledger_consistency(sub_pool: &SubPool) -> ClearingResult<()> {
    sub_pool.long_dormant.check_invariants()?;
    sub_pool.short_dormant.check_invariants()?;
    Ok(())
}
