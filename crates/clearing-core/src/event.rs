//! Structured engine events.
//!
//! Every state-mutating instruction in [`crate::engine`] produces a
//! sequence of [`EngineEvent`]s. The on-chain Solana program serializes
//! these into Anchor `emit_cpi!` events; the host-side `indexer` crate
//! replays them to reconstruct a per-position view (whitepaper §3
//! `locked_loss` / `realized_profit_balance`).
//!
//! Event ordering matters: within a single instruction, events are
//! emitted in causal order (sync_pool first, then the user-facing op,
//! then any post-state rotation events).

use crate::types::Direction;

/// Engine-emitted event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    /// Sync produced a non-zero transfer between directional pools and/or
    /// recovery accrual. Fields capture the **before** snapshot needed for
    /// downstream attribution.
    PoolSync(PoolSyncEvent),
    /// New position opened.
    PositionOpened(PositionOpenedEvent),
    /// Position closed normally; user received `withdrawable`.
    PositionClosed(PositionClosedEvent),
    /// Force-close on a zero-value position; user forfeited recovery.
    PositionForceClosed(PositionForceClosedEvent),
    /// Recovery shares redeemed without closing.
    DormantRecoveryClaimed(DormantRecoveryClaimedEvent),
    /// Active series rotated to recovery (pool zeroed).
    ActiveRotatedToRecovery(ActiveRotatedToRecoveryEvent),
    /// Dust harvested to fee vault.
    DustHarvested(DustHarvestedEvent),
    /// Lazy ledger replay applied a batch of pending events to a single
    /// dormant bucket. Emitted by
    /// [`crate::engine::pre_sync_dormant_bucket`]. The indexer uses
    /// `(direction, bucket_tick, post_accrued_value)` to converge its
    /// per-position projection without re-reading the bucket.
    DormantBucketPendingApplied(DormantBucketPendingAppliedEvent),
}

/// Snapshot of a `sync_pool` transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSyncEvent {
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Price before sync.
    pub p_last: u64,
    /// Price after sync.
    pub p_now: u64,
    /// Slot of the sync.
    pub slot: u64,

    /// Long active shares **before** the sync.
    pub long_active_shares_before: u128,
    /// Short active shares **before** the sync.
    pub short_active_shares_before: u128,
    /// Long active notional **before** the sync.
    pub long_active_notional_before: u128,
    /// Short active notional **before** the sync.
    pub short_active_notional_before: u128,
    /// Long pool equity **before** the sync.
    pub long_pool_equity_before: u128,
    /// Short pool equity **before** the sync.
    pub short_pool_equity_before: u128,

    /// Funds that flowed into long active pool.
    pub long_to_pool_inflow: u128,
    /// Funds that flowed into short active pool.
    pub short_to_pool_inflow: u128,
    /// Funds that flowed into long-side recovery accrual.
    pub long_to_recovery_inflow: u128,
    /// Funds that flowed into short-side recovery accrual.
    pub short_to_recovery_inflow: u128,
    /// Long-side residual that fell back to dust.
    pub long_residual_to_dust: u128,
    /// Short-side residual that fell back to dust.
    pub short_residual_to_dust: u128,
}

/// `open_position` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionOpenedEvent {
    /// Logical position id (caller-assigned, unique per sub pool).
    pub position_id: u64,
    /// Sub pool id the new position lives in.
    pub sub_pool_id: u32,
    /// Long or short.
    pub direction: Direction,
    /// Principal credited to the pool (post fee).
    pub principal: u64,
    /// Notional added to the active pool.
    pub notional: u128,
    /// Active shares minted to the user.
    pub shares_minted: u128,
    /// Effective principal accounted into the pool (after dust split).
    pub principal_into_pool: u128,
    /// Dust diverted from this open into `subpool.<dir>_dust`.
    pub dust: u128,
    /// Open fee charged.
    pub open_fee: u64,
    /// Entry price.
    pub entry_price: u64,
    /// Active generation observed at open.
    pub active_generation: u64,
    /// Slot of the open.
    pub slot: u64,
}

/// `close_position` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionClosedEvent {
    /// Logical position id.
    pub position_id: u64,
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Long or short.
    pub direction: Direction,
    /// Active shares burned.
    pub active_shares_burned: u128,
    /// Recovery shares burned.
    pub recovery_shares_burned: u128,
    /// **Notional** removed from the dormant bucket aggregate alongside
    /// `recovery_shares_burned`. Equals `mul_div_floor(bucket.total_recovery_notional,
    /// recovery_shares_burned, bucket.total_recovery_shares)` at the
    /// moment of close. Indexers MUST subtract this from their bucket
    /// view's `total_recovery_notional`; otherwise subsequent recovery
    /// distributions over-weight this bucket and under-allocate to
    /// other buckets, producing systematic per-position drift.
    pub recovery_notional_burned: u128,
    /// Funds released from the active pool.
    pub active_value: u128,
    /// Funds released from recovery accrual.
    pub recovery_value: u128,
    /// Total tokens transferred to the user.
    pub withdrawable: u128,
    /// Notional removed from the active pool.
    pub notional_removed: u128,
    /// Slot of the close.
    pub slot: u64,
}

/// `force_close_zero_value_position` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionForceClosedEvent {
    /// Logical position id.
    pub position_id: u64,
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Long or short.
    pub direction: Direction,
    /// Active shares burned (no value released).
    pub active_shares_burned: u128,
    /// Recovery shares burned and forfeited.
    pub recovery_shares_burned: u128,
    /// Notional removed from the dormant bucket aggregate alongside
    /// `recovery_shares_burned`. See [`PositionClosedEvent::recovery_notional_burned`].
    pub recovery_notional_burned: u128,
    /// Recovery accrual that was redirected to dust on forfeit.
    pub forfeited_recovery_value: u128,
    /// Notional removed from the active pool.
    pub notional_removed: u128,
    /// Slot of the force-close.
    pub slot: u64,
}

/// `claim_dormant_recovery` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DormantRecoveryClaimedEvent {
    /// Logical position id.
    pub position_id: u64,
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Long or short.
    pub direction: Direction,
    /// Bucket tick the recovery shares lived in.
    pub bucket_tick: i64,
    /// Recovery shares burned.
    pub recovery_shares_burned: u128,
    /// Notional removed from the dormant bucket aggregate alongside
    /// `recovery_shares_burned`. See [`PositionClosedEvent::recovery_notional_burned`].
    pub recovery_notional_burned: u128,
    /// Tokens released to the user.
    pub redeemable: u128,
    /// Slot of the claim.
    pub slot: u64,
}

/// Active-to-recovery rotation event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRotatedToRecoveryEvent {
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Long or short.
    pub direction: Direction,
    /// Bucket tick created or merged into.
    pub bucket_tick: i64,
    /// Anchor price stamped on the bucket.
    pub anchor_price: u64,
    /// Active shares migrated to recovery.
    pub migrated_shares: u128,
    /// Active notional migrated to recovery.
    pub migrated_notional: u128,
    /// Generation that just ended.
    pub generation_just_ended: u64,
    /// Slot of the rotation.
    pub slot: u64,
}

/// `pre_sync_dormant_bucket` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DormantBucketPendingAppliedEvent {
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Long or short.
    pub direction: Direction,
    /// Tick of the bucket whose pending was applied.
    pub bucket_tick: i64,
    /// Number of ledger entries applied during this call (each entry is
    /// either a no-op for the bucket or adds a positive share to its
    /// `accrued_value`).
    pub events_applied: u64,
    /// `accrued_value` of the bucket immediately before the apply.
    pub pre_accrued_value: u128,
    /// `accrued_value` of the bucket immediately after the apply.
    pub post_accrued_value: u128,
    /// `last_applied_index` after the apply (== `next_event_index` once
    /// fully caught up).
    pub last_applied_index: u64,
    /// Slot at which the apply was triggered.
    pub slot: u64,
}

/// Dust harvest event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DustHarvestedEvent {
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Long or short.
    pub direction: Direction,
    /// Amount swept to the protocol fee vault.
    pub amount: u128,
}
