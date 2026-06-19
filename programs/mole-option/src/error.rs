//! Program-level Anchor errors.
//!
//! These mirror `clearing_core::ClearingError` so we can convert at the
//! boundary; on-chain consumers see Anchor-style error codes.

use anchor_lang::prelude::*;
use clearing_core::ClearingError;

#[error_code]
pub enum ProgramError {
    #[msg("math overflow")]
    MathOverflow,
    #[msg("division by zero")]
    DivByZero,
    #[msg("sign overflow")]
    SignOverflow,
    #[msg("market is paused")]
    MarketPaused,
    #[msg("opening new positions is frozen for this market")]
    FrozenNewPosition,
    #[msg("invalid sub pool")]
    InvalidSubPool,
    #[msg("direction mismatch")]
    DirectionMismatch,
    #[msg("margin below minimum")]
    MarginBelowMinimum,
    #[msg("margin exceeds per-position cap")]
    MarginAboveMaximum,
    #[msg("market total principal cap exceeded")]
    TotalPrincipalCapExceeded,
    #[msg("market total notional cap exceeded")]
    TotalNotionalCapExceeded,
    #[msg("oracle price is zero")]
    OraclePriceZero,
    #[msg("oracle confidence interval too wide")]
    OracleConfidenceTooWide,
    #[msg("oracle price too stale")]
    OracleStale,
    #[msg("price protection failed")]
    PriceProtectionFailed,
    #[msg("single-step price move too large")]
    PriceMoveTooLarge,
    #[msg("shares minted too small")]
    SharesMintedTooSmall,
    #[msg("dilution risk too high")]
    DilutionRiskTooHigh,
    #[msg("position not open")]
    PositionNotOpen,
    #[msg("position is dormant")]
    PositionIsDormant,
    #[msg("withdrawable is zero, force-close required")]
    WithdrawableZero,
    #[msg("forfeit acknowledgement required")]
    ForfeitAcknowledgementRequired,
    #[msg("vault balance insufficient")]
    VaultInsufficient,
    #[msg("dormant bucket cap exceeded")]
    DormantBucketCapExceeded,
    #[msg("sub pool idle, sync required")]
    SubPoolStale,
    #[msg("schema version mismatch")]
    SchemaVersionMismatch,
    #[msg("invariant violation, market auto-paused")]
    Invariant,
    #[msg("unauthorized")]
    Unauthorized,
    #[msg("oracle account data unreadable")]
    OracleAccountUnreadable,
    #[msg("oracle account validation failed")]
    OracleValidationFailed,
    #[msg("oracle price falls outside the caller's price envelope")]
    OraclePriceOutsideEnvelope,
    #[msg("dormant pending budget exceeded; retry with a smaller batch")]
    DormantPendingBudgetExceeded,
    #[msg("dormant bucket not found at the given tick")]
    DormantBucketMissing,
    #[msg("invalid parameter")]
    InvalidParameter,
    #[msg("dormant bridge: a passed-through account does not match the expected (sub_pool, direction)")]
    DormantBridgeAccountMismatch,
    #[msg(
        "dormant bridge: engine produced more buckets than the caller pre-allocated PDA slots for; \
         retry with a fresh init_dormant_bucket PDA in remaining_accounts"
    )]
    DormantBridgeBucketSlotExhausted,
    #[msg("dormant bridge: distribution ledger account is required for this instruction but was not provided")]
    DormantBridgeLedgerMissing,
    #[msg("dormant ledger: ring buffer at capacity; retry after pre_sync_dormant_bucket")]
    LedgerCapacityExceeded,
    #[msg("dormant bucket still has live shares / accrued / position_count; cannot close")]
    DormantBucketStillLive,
    #[msg(
        "dormant bucket has not advanced its last_applied_index past the ledger head; \
         pre_sync the bucket before closing"
    )]
    DormantBucketHasPendingApply,
    /// **Wave 9.** Schema bump rejected: requested target version is
    /// not strictly greater than the current `market.schema_version`.
    /// Schema versions are monotonically increasing — bumping to the
    /// same or older value would let an attacker rewind the protocol
    /// past a fix.
    #[msg("schema bump must strictly increase market.schema_version")]
    SchemaBumpMustIncrease,
    /// **Wave 9.** `migrate_position` / `migrate_market` was invoked
    /// with a position/market whose `schema_version` is already at the
    /// target — migration would be a no-op and silently masks an
    /// off-chain bug. Caller must check before invoking.
    #[msg("schema migration: source already at target version")]
    SchemaMigrationNoop,
    /// **Wave 9.** Migration handler does not know how to upgrade from
    /// the source `schema_version` to the target. v1 only supports
    /// 1 → 1 (which is rejected as `SchemaMigrationNoop`); future
    /// versions will register concrete upgrade paths.
    #[msg("schema migration: no path registered for source -> target")]
    SchemaMigrationPathMissing,
    /// **Wave 15.** `keeper_leader_heartbeat` rejected because the
    /// lock is currently held fresh by another keeper. Caller should
    /// back off `slots_until_stale` slots before retrying.
    #[msg("keeper-leader lock is held fresh by another keeper")]
    KeeperLeaderHeldByOther,
    /// **Wave 15.** `keeper_leader_heartbeat` rejected because the
    /// caller's `observed_slot` is older than the lock's recorded
    /// `last_heartbeat_slot`. This is the wave-15 monotonicity
    /// guard: a malicious keeper cannot rewind the lock to extend
    /// its leadership after another keeper stamped a later slot.
    #[msg("keeper-leader heartbeat rejected: observed_slot < recorded slot")]
    KeeperLeaderClockSkew,
    /// **Wave 15.** `keeper_leader_release` rejected because the
    /// signer does not currently hold the lock.
    #[msg("keeper-leader release rejected: signer is not the current holder")]
    KeeperLeaderNotHolder,
    /// **Wave 15.** `keeper_leader_release` rejected because the
    /// lock has no leader to release. Idempotency-friendly: callers
    /// retrying release after a successful release see this rather
    /// than a state corruption.
    #[msg("keeper-leader release rejected: lock is unowned")]
    KeeperLeaderNotHeld,
    /// **Wave 15.** `keeper_leader_acquire` rejected because the
    /// lock is currently fresh — `acquire` is the strict
    /// "claim-stale" path, callers should use
    /// `keeper_leader_heartbeat` for the all-paths flow. Surfaces
    /// the on-chain gate that fails fresh-lock acquisitions before
    /// the takeover threshold elapses.
    #[msg("keeper-leader acquire rejected: lock is currently fresh")]
    KeeperLeaderAcquireWhileFresh,
}

impl From<ClearingError> for ProgramError {
    fn from(e: ClearingError) -> Self {
        match e {
            ClearingError::MathOverflow => ProgramError::MathOverflow,
            ClearingError::DivByZero => ProgramError::DivByZero,
            ClearingError::SignOverflow => ProgramError::SignOverflow,
            ClearingError::MarketPaused => ProgramError::MarketPaused,
            ClearingError::FrozenNewPosition => ProgramError::FrozenNewPosition,
            ClearingError::InvalidSubPool => ProgramError::InvalidSubPool,
            ClearingError::DirectionMismatch => ProgramError::DirectionMismatch,
            ClearingError::MarginBelowMinimum => ProgramError::MarginBelowMinimum,
            ClearingError::MarginAboveMaximum => ProgramError::MarginAboveMaximum,
            ClearingError::TotalPrincipalCapExceeded => ProgramError::TotalPrincipalCapExceeded,
            ClearingError::TotalNotionalCapExceeded => ProgramError::TotalNotionalCapExceeded,
            ClearingError::OraclePriceZero => ProgramError::OraclePriceZero,
            ClearingError::OracleConfidenceTooWide => ProgramError::OracleConfidenceTooWide,
            ClearingError::OracleStale => ProgramError::OracleStale,
            ClearingError::PriceProtectionFailed => ProgramError::PriceProtectionFailed,
            ClearingError::PriceMoveTooLarge => ProgramError::PriceMoveTooLarge,
            ClearingError::SharesMintedTooSmall => ProgramError::SharesMintedTooSmall,
            ClearingError::DilutionRiskTooHigh => ProgramError::DilutionRiskTooHigh,
            ClearingError::PositionNotOpen => ProgramError::PositionNotOpen,
            ClearingError::PositionIsDormant => ProgramError::PositionIsDormant,
            ClearingError::WithdrawableZero => ProgramError::WithdrawableZero,
            ClearingError::ForfeitAcknowledgementRequired => {
                ProgramError::ForfeitAcknowledgementRequired
            }
            ClearingError::VaultInsufficient => ProgramError::VaultInsufficient,
            ClearingError::DormantBucketCapExceeded => ProgramError::DormantBucketCapExceeded,
            ClearingError::DormantPendingBudgetExceeded => ProgramError::DormantPendingBudgetExceeded,
            ClearingError::DormantBucketMissing => ProgramError::DormantBucketMissing,
            ClearingError::SubPoolStale => ProgramError::SubPoolStale,
            ClearingError::SchemaVersionMismatch => ProgramError::SchemaVersionMismatch,
            ClearingError::LedgerCapacityExceeded => ProgramError::LedgerCapacityExceeded,
            ClearingError::Invariant(_) => ProgramError::Invariant,
        }
    }
}

/// Convenience wrapper used inside the program.
pub fn map_err(e: ClearingError) -> Error {
    let code: ProgramError = e.into();
    code.into()
}
