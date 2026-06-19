//! Canonical error codes for the clearing engine.
//!
//! Mirrors the on-chain error list in `Docs/Planning/07-智能合约设计.md`.
//! Numeric ordering is stable so the Solana program can map 1-to-1.

use molemath::MathError;
use thiserror::Error;

/// Result alias used throughout the clearing engine.
pub type ClearingResult<T> = Result<T, ClearingError>;

/// All errors that can be returned by the clearing engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ClearingError {
    /// Numeric overflow in checked math.
    #[error("math overflow")]
    MathOverflow,
    /// Division by zero in math primitive.
    #[error("division by zero")]
    DivByZero,
    /// Signed conversion would lose information.
    #[error("sign overflow")]
    SignOverflow,

    /// Market is paused.
    #[error("market paused")]
    MarketPaused,
    /// Opening new positions is frozen for this market.
    #[error("market is frozen for new positions")]
    FrozenNewPosition,
    /// Sub pool index out of range or mismatched.
    #[error("invalid sub pool")]
    InvalidSubPool,
    /// Direction mismatch (e.g. closing a position on the wrong subpool side).
    #[error("direction mismatch")]
    DirectionMismatch,

    /// Principal less than `min_margin`.
    #[error("margin below minimum")]
    MarginBelowMinimum,
    /// Margin exceeds per-position cap.
    #[error("margin exceeds per-position cap")]
    MarginAboveMaximum,
    /// Total principal across the market would exceed cap.
    #[error("market total principal cap exceeded")]
    TotalPrincipalCapExceeded,
    /// Total notional across the market would exceed cap.
    #[error("market total notional cap exceeded")]
    TotalNotionalCapExceeded,

    /// Oracle price is zero.
    #[error("oracle price is zero")]
    OraclePriceZero,
    /// Oracle confidence interval too wide.
    #[error("oracle confidence interval too wide")]
    OracleConfidenceTooWide,
    /// Oracle price is stale.
    #[error("oracle price too stale")]
    OracleStale,
    /// Submitted price protection expectation was violated.
    #[error("price protection failed")]
    PriceProtectionFailed,
    /// Single-step price move exceeds `max_price_move_bps_per_sync`.
    #[error("single-step price move too large")]
    PriceMoveTooLarge,

    /// Newly minted shares would round to zero.
    #[error("shares minted too small")]
    SharesMintedTooSmall,
    /// Reverse dilution risk: `pool_equity * dilution_safety_bps < total_shares * 10_000`.
    #[error("dilution risk too high")]
    DilutionRiskTooHigh,

    /// Position is not in `Open` state.
    #[error("position not open")]
    PositionNotOpen,
    /// Position is dormant; use the appropriate flow.
    #[error("position is dormant")]
    PositionIsDormant,
    /// Withdrawable amount is zero; explicit forfeit required.
    #[error("withdrawable is zero, force-close required")]
    WithdrawableZero,
    /// Caller did not acknowledge forfeiting recovery shares.
    #[error("forfeit acknowledgement required")]
    ForfeitAcknowledgementRequired,
    /// Vault balance insufficient (should never happen if invariants hold).
    #[error("vault balance insufficient")]
    VaultInsufficient,

    /// Dormant bucket count for direction has exceeded cap.
    #[error("dormant bucket cap exceeded")]
    DormantBucketCapExceeded,
    /// `pre_sync_dormant_bucket` was asked to apply more pending events
    /// than `MarketParams::max_pending_apply_per_tx` allows. The keeper
    /// must retry with a smaller batch (i.e. call again later — every
    /// retry chips away at the backlog by `max_pending_apply_per_tx`).
    #[error("pre_sync_dormant_bucket pending budget exceeded")]
    DormantPendingBudgetExceeded,
    /// `pre_sync_dormant_bucket` was called for a tick that does not
    /// correspond to any live bucket.
    #[error("dormant bucket not found")]
    DormantBucketMissing,
    /// The on-chain distribution-ledger ring buffer is full and no
    /// entries can be GC'd because some live bucket's
    /// `last_applied_index` has not yet advanced. The keeper / user
    /// must drive `pre_sync_dormant_bucket` for the lagging bucket(s)
    /// so `compact_ledger` can free space, then retry.
    #[error("distribution ledger capacity exceeded; advance lagging buckets and retry")]
    LedgerCapacityExceeded,
    /// Sub-pool requires a sync (idle too long).
    #[error("sub pool idle, sync required")]
    SubPoolStale,

    /// Schema version mismatch (migration in progress).
    #[error("schema version mismatch")]
    SchemaVersionMismatch,

    /// Internal invariant violation. Triggers auto-pause on chain.
    #[error("invariant violation: {0}")]
    Invariant(&'static str),
}

impl From<MathError> for ClearingError {
    fn from(value: MathError) -> Self {
        match value {
            MathError::Overflow => ClearingError::MathOverflow,
            MathError::DivByZero => ClearingError::DivByZero,
            MathError::SignOverflow => ClearingError::SignOverflow,
        }
    }
}
