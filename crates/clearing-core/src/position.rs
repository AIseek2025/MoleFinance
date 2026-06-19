//! User-facing position struct.
//!
//! Mirrors `Docs/Planning/18-shares模型实现细则与边界条件.md` §7.

use crate::types::Direction;

/// Position lifecycle. The on-chain encoding folds `Dormant` into the
/// `Open` state with a `dormant: bool` flag; here we keep them as separate
/// host-side enum variants for readability while using helper accessors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionStatus {
    /// Position is active and immediately settleable.
    Open,
    /// Position is open but currently dormant (zero-value, holding only
    /// recovery shares).
    Dormant,
    /// Position has been finalized (closed or forfeited).
    Closed,
}

/// On-chain position.
#[derive(Debug, Clone)]
pub struct Position {
    /// Owner public-key surrogate (not used by the math, kept for parity with
    /// the Solana account model).
    pub owner: [u8; 32],
    /// Sub pool index this position belongs to.
    pub sub_pool_id: u32,
    /// Position id, unique per (market, owner).
    pub position_id: u64,

    /// Long or short.
    pub direction: Direction,
    /// Status flag.
    pub status: PositionStatus,

    /// Initial principal (after open fee).
    pub principal: u64,
    /// Notional = principal * leverage (raw token units).
    pub notional: u128,

    /// Active shares currently held (zero after rotate-to-recovery).
    pub active_shares: u128,
    /// Recovery shares held (non-zero only after rotate or partial recovery).
    pub recovery_shares: u128,
    /// Tick id of the recovery bucket the recovery shares live in.
    pub recovery_bucket_tick: Option<i64>,
    /// Anchor price at which equity went to zero (raw `PRICE_SCALE` units).
    pub zero_price: u64,

    /// Entry price.
    pub entry_price: u64,
    /// Last subpool sync slot observed by this position.
    pub last_sync_slot: u64,

    /// Slot at which the position was opened.
    pub opened_at_slot: u64,
    /// Last update slot.
    pub updated_at_slot: u64,
    /// Slot at which the position was closed (0 while open).
    pub closed_at_slot: u64,
    /// Schema version this position was opened against.
    pub schema_version: u16,

    /// Active generation observed at open time. Used for lazy migration.
    pub active_generation: u64,
}

impl Position {
    /// Returns `true` if this position holds any active or recovery claim.
    pub fn has_outstanding_claim(&self) -> bool {
        self.active_shares > 0 || self.recovery_shares > 0
    }

    /// Returns `true` when the position currently maps to active shares only.
    pub fn is_active_only(&self) -> bool {
        self.active_shares > 0 && self.recovery_shares == 0
    }
}
