//! On-chain account layouts.
//!
//! Mirrors `Docs/Planning/07-智能合约设计.md` and
//! `Docs/Planning/18-shares模型实现细则与边界条件.md`.

use anchor_lang::prelude::*;

/// Global protocol configuration.
#[account]
pub struct GlobalConfig {
    pub admin_authority: Pubkey,
    pub emergency_authority: Pubkey,
    pub protocol_treasury: Pubkey,
    pub upgrade_authority: Pubkey,
    pub paused_globally: bool,
    pub schema_version: u16,
    pub bump: u8,
}

impl GlobalConfig {
    pub const LEN: usize = 8 + 32 * 4 + 1 + 2 + 1;
}

/// One Market = (symbol, collateral, leverage).
#[account]
pub struct Market {
    pub global_config: Pubkey,
    pub symbol: [u8; 16],
    pub collateral_mint: Pubkey,
    pub vault: Pubkey,
    pub fee_vault: Pubkey,
    pub oracle_price_feed: Pubkey,
    pub oracle_program_id: Pubkey,

    pub leverage_bps: u32,

    pub min_margin: u64,
    pub max_margin_per_position: u64,
    pub max_total_principal: u128,
    pub max_total_notional: u128,
    pub current_total_principal: u128,
    pub current_total_notional: u128,

    pub open_fee_bps: u16,

    pub max_oracle_age_seconds: i64,
    pub max_oracle_age_slots: u64,
    pub max_confidence_bps: u16,
    pub max_price_move_bps_per_sync: u32,

    pub price_tick: u64,
    pub tick_aggregation_factor: u32,
    pub max_dormant_bucket_count_per_direction: u32,

    pub dilution_safety_bps: u32,
    pub max_idle_slots: u64,

    pub paused: bool,
    pub frozen_new_position: bool,
    pub schema_version: u16,

    pub sub_pool_count: u32,

    /// 0 = Eager (sync_pool touches every bucket),
    /// 1 = Lazy (sync_pool only appends a ledger entry; users / keepers
    /// catch buckets up via `pre_sync_dormant_bucket`).
    /// See `Docs/Planning/21-Dormant存储与CU预算.md` for trade-offs.
    pub dormant_distribute_mode: u8,
    /// Maximum number of pending ledger events any single
    /// `pre_sync_dormant_bucket` call may apply. Bounds per-tx CU.
    pub max_pending_apply_per_tx: u32,
    /// Hard cap on the size of each per-direction distribution ledger
    /// ring buffer. See `clearing_core::MarketParams::max_distribution_ledger_size`.
    pub max_distribution_ledger_size: u32,

    pub bump: u8,
    pub _pad: [u8; 2],
}

impl Market {
    pub const LEN: usize = 8 + 32 * 6 + 16 + 4 + 8 * 4 + 16 * 4 + 2 + 8 + 8 + 2 + 4 + 8 + 4 + 4 + 4
        + 8 + 1 + 1 + 2 + 4 + 1 + 4 + 4 + 1 + 2;
}

/// Per-shard mutable state. There are `market.sub_pool_count` instances.
#[account]
pub struct SubPool {
    pub market: Pubkey,
    pub sub_pool_id: u32,

    pub long_pool_equity: u128,
    pub short_pool_equity: u128,

    pub long_active_shares: u128,
    pub short_active_shares: u128,
    pub long_recovery_shares: u128,
    pub short_recovery_shares: u128,

    pub long_active_notional: u128,
    pub short_active_notional: u128,

    pub long_active_generation: u64,
    pub short_active_generation: u64,

    pub last_price: u64,
    pub last_sync_slot: u64,

    pub long_dust: u128,
    pub short_dust: u128,

    pub long_dormant_bucket_count: u32,
    pub short_dormant_bucket_count: u32,

    pub bump: u8,
    pub _pad: [u8; 7],
}

impl SubPool {
    pub const LEN: usize =
        8 + 32 + 4 + 16 * 8 + 8 * 4 + 16 * 2 + 4 * 2 + 1 + 7;
}

/// One dormant bucket aggregates positions that hit zero at the same
/// `zero_price_tick`.
///
/// Mirrors `clearing_core::DormantBucket` byte-for-byte (except for
/// the Anchor `bump` and discriminator). The bridge between this
/// account and the host engine type is implemented by
/// [`pack_into_record`](DormantBucket::pack_into_record) /
/// [`apply_record`](DormantBucket::apply_record); see
/// `Docs/Planning/23-on-chain-dormant-bridge.md` for the per-
/// instruction account-list contract.
#[account]
pub struct DormantBucket {
    pub sub_pool: Pubkey,
    pub direction_is_long: bool,
    pub zero_price_tick: i64,
    pub anchor_price: u64,
    pub total_recovery_shares: u128,
    pub total_recovery_notional: u128,
    pub accrued_value: u128,
    pub position_count: u64,
    /// Absolute index of the last `DistributionLedger` entry applied
    /// to `accrued_value`. Always lies in
    /// `[ledger.gc_offset, ledger.next_event_index]`. Lazy-mode
    /// `pre_sync_dormant_bucket` advances this monotonically.
    pub last_applied_index: u64,
    pub bump: u8,
    pub _pad: [u8; 6],
}

impl DormantBucket {
    /// 8 (anchor disc) + 32 (sub_pool) + 1 (direction) + 8 (tick)
    /// + 8 (anchor) + 16*3 (shares/notional/accrued) + 8 (pos_count)
    /// + 8 (last_applied_index) + 1 (bump) + 6 (pad) = 120
    pub const LEN: usize = 8 + 32 + 1 + 8 + 8 + 16 * 3 + 8 + 8 + 1 + 6;
}

/// Per-direction distribution ledger ring buffer.
///
/// One per `(sub_pool, direction)`. The Anchor wrapper stores up to
/// `Market::max_distribution_ledger_size` entries inline. The host
/// engine reads/writes these via
/// `clearing_core::onchain::OnChainLedger`; the property tests in
/// `crates/clearing-core/tests/onchain_layout.rs` pin the byte
/// equivalence.
///
/// The on-chain account is preallocated at the maximum size to avoid
/// runtime `realloc`. Capacity is enforced by
/// `clearing_core::ClearingError::LedgerCapacityExceeded` and the
/// init handler rejects `max_distribution_ledger_size == 0`.
#[account]
pub struct DistributionLedger {
    pub sub_pool: Pubkey,
    pub direction_is_long: bool,
    /// Hard cap on `entries.len()`. Mirrors `Market::max_distribution_ledger_size`.
    pub max_entries: u32,
    /// Number of GC'd events at the front of the logical ring.
    pub gc_offset: u64,
    /// Absolute index of the next event to be appended.
    /// Equal to `gc_offset + entry_count`.
    pub next_event_index: u64,
    /// Cached `accrued_value` sum across this direction's buckets.
    pub accrued_value_total: u128,
    /// Lazy-mode in-flight allocations (wave 5.5): funds the engine
    /// routed out of `pool_equity` via `distribute_lazy` that no
    /// bucket has yet pulled into its `accrued_value` via
    /// `apply_pending_to_bucket`. Required for vault decomposition
    /// to balance under lazy mode at every step. Always `0` in
    /// eager mode. Mirrors
    /// `clearing_core::onchain::OnChainLedger::pending_distribution_total`.
    pub pending_distribution_total: u128,
    /// Live entry count.
    pub entry_count: u32,
    /// Live entries (logical view; on chain stored inline up to
    /// `max_entries` slots).
    pub entries: Vec<DistEntryPacked>,
    pub bump: u8,
    pub _pad: [u8; 7],
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default)]
pub struct DistEntryPacked {
    pub event_index: u64,
    pub p_at_event: u64,
    pub total_outstanding_at_event: u128,
    pub total_alloc_input: u128,
    pub allocated_sum_observed: u128,
}

impl DistributionLedger {
    /// Per-entry size: 8 + 8 + 16 * 3 = 64 bytes.
    pub const ENTRY_LEN: usize = 8 + 8 + 16 * 3;

    /// Header size: 8 (disc) + 32 (sub_pool) + 1 (dir) + 4 (max_entries)
    /// plus 8 (gc_offset) + 8 (next_event_index) + 16 (accrued_total)
    /// plus 16 (pending_distribution_total, added in wave 5.5)
    /// plus 4 (entry_count) + 4 (Vec length prefix) + 1 (bump)
    /// plus 7 (pad) = 109 bytes.
    pub const HEADER_LEN: usize = 8 + 32 + 1 + 4 + 8 + 8 + 16 + 16 + 4 + 4 + 1 + 7;

    /// Total account size for a ledger sized at `max_entries`.
    pub fn account_size(max_entries: u32) -> usize {
        Self::HEADER_LEN + (max_entries as usize) * Self::ENTRY_LEN
    }
}

/// Per-rotate event log entry. Stored in a ring buffer per direction. Used
/// for lazy migration of stale positions.
#[account]
pub struct RotateLog {
    pub sub_pool: Pubkey,
    pub direction_is_long: bool,
    pub head: u32,
    pub len: u32,
    pub records: [RotateRecordPacked; 64],
    pub bump: u8,
    pub _pad: [u8; 6],
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default)]
pub struct RotateRecordPacked {
    pub generation_just_ended: u64,
    pub bucket_tick: i64,
    pub anchor_price: u64,
}

impl RotateLog {
    pub const RECORD_LEN: usize = 8 + 8 + 8;
    pub const RING_CAPACITY: usize = 64;
    pub const LEN: usize =
        8 + 32 + 1 + 4 + 4 + Self::RECORD_LEN * Self::RING_CAPACITY + 1 + 6;
}

/// User-owned position.
#[account]
pub struct Position {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub sub_pool: Pubkey,
    pub position_id: u64,

    pub direction_is_long: bool,
    pub status: u8, // 0 = Open, 1 = Dormant, 2 = Closed

    pub principal: u64,
    pub leverage_bps: u32,
    pub notional: u128,

    pub active_shares: u128,
    pub recovery_shares: u128,
    pub recovery_bucket_tick: i64,
    pub has_recovery_bucket: bool,
    pub zero_price: u64,

    pub entry_price: u64,
    pub last_sync_slot: u64,
    pub active_generation: u64,

    pub opened_at: i64,
    pub updated_at: i64,
    pub closed_at: i64,
    pub schema_version: u16,
    pub bump: u8,
    pub _pad: [u8; 5],
}

impl Position {
    pub const LEN: usize = 8 + 32 * 3 + 8 + 1 + 1 + 8 + 4 + 16 + 16 * 2 + 8 + 1 + 8 + 8 + 8 + 8
        + 8 * 3 + 2 + 1 + 5;
}

/// **Wave 15.** Per-market keeper-leader lock PDA.
///
/// Mirrors `keeper_decoder::leader_lock::KeeperLeaderLock` byte-for-
/// byte. The 49-byte body is fixed-size by design (we deliberately
/// avoided `Option<Pubkey>` because Borsh's option encoding is
/// variable-length, which is incompatible with Anchor's `space =
/// LEN` model). The host-side `chain_mirror::leader_lock::tests::
/// borsh_derive_matches_hand_rolled_layout` test runs both a held
/// and an unowned form through Borsh and asserts they're 49 bytes
/// each.
///
/// Layout (after the 8-byte Anchor discriminator):
///   `has_leader[1] ++ current_leader[32] ++ last_heartbeat_slot[8]
///    ++ takeover_threshold_slots[8]` = 49 bytes.
///
/// PDA seeds: `[b"keeper_leader_lock", market.key().as_ref()]`. Each
/// market gets its own lock; cross-market keeper races are not the
/// design goal (§24 operator runbook §3.4 — one bot per market).
#[account]
pub struct KeeperLeaderLock {
    /// `true` iff the lock currently records a leader. `false`
    /// before the very first heartbeat AND after a graceful release.
    pub has_leader: bool,
    /// 32-byte pubkey of the keeper currently holding the lock.
    /// All-zero when `has_leader == false`.
    pub current_leader: [u8; 32],
    /// Slot stamp of the most recent successful heartbeat by the
    /// current leader. Monotonically non-decreasing.
    pub last_heartbeat_slot: u64,
    /// How many slots can elapse before any keeper is allowed to
    /// take over. Wave 15 default ≈ 30 s = 75 slots.
    pub takeover_threshold_slots: u64,
}

impl KeeperLeaderLock {
    /// Anchor disc (8) + body (49) = 57 bytes. Anchor pads to 8-byte
    /// boundary, so the realised on-chain account size is 64 bytes.
    pub const LEN: usize = 8 + 1 + 32 + 8 + 8;

    /// Wave-15 default: 75 slots ≈ 30 s of staleness before any
    /// other keeper may take over. Mirrors
    /// `keeper_decoder::leader_lock::tests::TAKEOVER`.
    pub const DEFAULT_TAKEOVER_THRESHOLD_SLOTS: u64 = 75;
}
