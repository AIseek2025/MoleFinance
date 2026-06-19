//! `MarketParams` (configuration) and `SubPool` (mutable per-shard state).
//!
//! Mirrors `Docs/Planning/18-shares模型实现细则与边界条件.md` §2.

use crate::dormant::DormantStore;
use crate::error::{ClearingError, ClearingResult};
use crate::types::Direction;

/// **Wave 8 — single source of truth for the on-chain schema version.**
///
/// Every funds-touching engine entrypoint asserts that `market.
/// schema_version` equals this constant before performing any work.
/// Position-bearing entrypoints additionally assert `position.
/// schema_version == market.schema_version`. Any mismatch returns
/// [`ClearingError::SchemaVersionMismatch`] and the on-chain handler
/// surfaces `ProgramError::SchemaVersionMismatch` so the user sees a
/// clean, actionable error instead of silent corruption.
///
/// **Upgrade playbook** (mirrors `Docs/Planning/16-治理与升级.md`):
///
/// 1. Land the new logic behind a feature gate; keep `SCHEMA_VERSION_
///    CURRENT` at the old value. Old positions and old market still
///    work.
/// 2. Ship a migration instruction (`migrate_position`,
///    `migrate_market`) that no-ops on the new schema and bumps the
///    schema_version field on the old schema in place.
/// 3. Bump `SCHEMA_VERSION_CURRENT` together with a Squads-multisig
///    governance tx that sets `market.schema_version =
///    SCHEMA_VERSION_CURRENT`. From this point old-schema positions
///    are rejected by the engine and MUST go through `migrate_
///    position` before they can close.
/// 4. Old-schema migration instructions stay deployed for one full
///    governance epoch as a fallback path.
pub const SCHEMA_VERSION_CURRENT: u16 = 1;

/// **Wave 8.** Validate that the given on-chain schema version
/// matches what the engine was compiled for. Used at every funds-
/// touching entrypoint so a stale upgrade or a forgotten migration
/// instruction can never silently corrupt state.
#[inline]
pub fn assert_schema_version(found: u16) -> ClearingResult<()> {
    if found != SCHEMA_VERSION_CURRENT {
        return Err(ClearingError::SchemaVersionMismatch);
    }
    Ok(())
}

/// Strategy used by [`crate::engine::sync_pool`] to push recovery
/// allocations into the per-direction dormant store.
///
/// On the host crate this only affects when `accrued_value` and
/// `last_applied_index` are written; the **observable end state** is
/// identical between modes once
/// [`DormantStore::apply_pending_to_bucket`] has caught every bucket up to
/// the ledger head. The choice matters on chain, where a single Solana
/// transaction can only load a bounded number of accounts: lazy mode lets
/// `sync_pool` skip non-activated buckets entirely, deferring per-bucket
/// writes to user-paid `pre_sync_dormant_bucket` instructions.
///
/// See `Docs/Planning/18-shares模型实现细则与边界条件.md` §10.3 for the
/// on-chain account layout each mode implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DistributeMode {
    /// Touch every dormant bucket — activated and inactive — at sync time.
    /// Activated buckets receive their proportional share immediately,
    /// inactive ones get their `last_applied_index` bumped so the lazy
    /// replay path stays a no-op for them. O(bucket_count) per sync.
    /// Used by the host reference and acceptable on chain when the total
    /// bucket count is small.
    #[default]
    Eager,
    /// Append a [`DistEntry`](crate::DistEntry) and update activated
    /// buckets only — non-activated buckets retain their existing
    /// `last_applied_index` and pay the catch-up cost lazily on their
    /// next interaction (e.g. `claim_dormant_recovery` or
    /// `pre_sync_dormant_bucket`). O(activated_bucket_count) per sync,
    /// independent of the total bucket count.
    Lazy,
}

/// Immutable per-market parameters. On chain these live inside the `Market`
/// account; here they are passed by reference to the engine.
///
/// All bps values use `BPS_SCALE = 10_000` (100 % == 10_000 bps).
#[derive(Debug, Clone)]
pub struct MarketParams {
    /// Leverage applied to principal to derive notional, in bps.
    /// `notional = principal * leverage_bps / 10_000`.
    pub leverage_bps: u32,
    /// Lower bound for principal, in raw collateral units.
    pub min_margin: u64,
    /// Per-position upper bound for principal.
    pub max_margin_per_position: u64,
    /// Upper bound for the sum of `principal` across the market.
    pub max_total_principal: u128,
    /// Upper bound for the sum of `notional` across the market.
    pub max_total_notional: u128,

    /// Open fee in bps charged on the gross deposited amount.
    pub open_fee_bps: u16,

    /// Maximum oracle price age allowed during sync, in seconds (host clock).
    pub max_oracle_age_seconds: i64,
    /// Maximum oracle confidence interval, in bps of price.
    pub max_confidence_bps: u16,
    /// Maximum allowed price move per sync, in bps. Sync is rejected
    /// (auto-pause on chain) if exceeded.
    pub max_price_move_bps_per_sync: u32,

    /// Quantization step for the oracle price (price_tick).
    pub price_tick: u64,
    /// `bucket_tick = floor(price / price_tick / tick_aggregation_factor)`.
    pub tick_aggregation_factor: u32,
    /// Hard cap on the number of dormant buckets per direction per subpool.
    pub max_dormant_bucket_count_per_direction: u32,
    /// Strategy used when `sync_pool` distributes a recovery transfer to
    /// the dormant buckets. See [`DistributeMode`] for the trade-off.
    pub dormant_distribute_mode: DistributeMode,
    /// Maximum number of pending ledger events any single
    /// `pre_sync_dormant_bucket` call may apply. Bounds the per-tx CU
    /// budget for lazy bucket catch-ups. The on-chain version returns
    /// [`crate::ClearingError::DormantPendingBudgetExceeded`] when this
    /// is exceeded; the keeper retries with a smaller batch until the
    /// bucket is caught up.
    pub max_pending_apply_per_tx: u32,

    /// Hard cap on the live size of the per-direction distribution
    /// ledger ring buffer. The lazy path appends one entry per sync
    /// that produces a recovery transfer; entries are GC'd by
    /// [`crate::DormantStore::compact_ledger`] once every live bucket
    /// has caught up to or past them. When a sync would push past
    /// this cap *and* compaction can free no entries (because some
    /// bucket has not been touched), the engine returns
    /// [`crate::ClearingError::LedgerCapacityExceeded`] so the keeper
    /// can drive `pre_sync_dormant_bucket` for the lagging bucket and
    /// then retry. The on-chain account is sized at
    /// `max_distribution_ledger_size * sizeof(DistEntry)`; making this
    /// too small causes spurious back-pressure, too large wastes rent.
    pub max_distribution_ledger_size: u32,

    /// Reverse dilution threshold: trigger zero-out when
    /// `pool_equity * dilution_safety_bps < total_shares * 10_000`.
    pub dilution_safety_bps: u32,

    /// Maximum slot gap before a sub-pool requires a forced sync.
    pub max_idle_slots: u64,

    /// Schema version of on-chain accounts.
    pub schema_version: u16,

    /// `paused == true` blocks all funds-sensitive instructions.
    pub paused: bool,
    /// `frozen_new_position == true` blocks `open_position` only.
    pub frozen_new_position: bool,
}

impl MarketParams {
    /// Reasonable defaults for tests and reference simulators.
    /// Production parameters are configured via on-chain governance per
    /// `Docs/Planning/08-杠杆交易场与风控设计.md`.
    pub fn sample() -> Self {
        Self {
            leverage_bps: 100_000, // 10x leverage
            min_margin: 10_000_000,
            max_margin_per_position: 1_000_000_000_000,
            max_total_principal: u128::MAX / 2,
            max_total_notional: u128::MAX / 2,
            open_fee_bps: 0,
            max_oracle_age_seconds: 30,
            max_confidence_bps: 50, // 0.5 %
            max_price_move_bps_per_sync: 2_000,
            price_tick: 1, // 1 unit of PRICE_SCALE
            tick_aggregation_factor: 1,
            max_dormant_bucket_count_per_direction: 1024,
            dormant_distribute_mode: DistributeMode::Eager,
            max_pending_apply_per_tx: 4096,
            // 64KB ring at 64 bytes/entry; well within Solana's 10 MB
            // account size, large enough to absorb tens of thousands of
            // syncs between keeper catch-ups in lazy mode.
            max_distribution_ledger_size: 1024,
            dilution_safety_bps: 1, // 1 bps
            max_idle_slots: 6_000,
            schema_version: SCHEMA_VERSION_CURRENT,
            paused: false,
            frozen_new_position: false,
        }
    }
}

/// Mutable per-subpool state. There is one [`SubPool`] per market shard.
#[derive(Debug, Clone)]
pub struct SubPool {
    /// SubPool identity (0..sub_pool_count).
    pub sub_pool_id: u32,

    /// Long-side directional pool equity, in raw collateral units.
    pub long_pool_equity: u128,
    /// Short-side directional pool equity, in raw collateral units.
    pub short_pool_equity: u128,

    /// Long-side active shares.
    pub long_active_shares: u128,
    /// Short-side active shares.
    pub short_active_shares: u128,
    /// Long-side recovery shares (sum across all dormant buckets).
    pub long_recovery_shares: u128,
    /// Short-side recovery shares.
    pub short_recovery_shares: u128,

    /// Long-side total active notional (excludes dormant).
    pub long_active_notional: u128,
    /// Short-side total active notional (excludes dormant).
    pub short_active_notional: u128,

    /// Long-side dormant store.
    pub long_dormant: DormantStore,
    /// Short-side dormant store.
    pub short_dormant: DormantStore,

    /// Last synced oracle price (raw, scaled by `PRICE_SCALE`).
    pub last_price: u64,
    /// Slot at which `last_price` was set.
    pub last_sync_slot: u64,

    /// Long-side dust accumulated by `mul_div_floor` rounding.
    pub long_dust: u128,
    /// Short-side dust.
    pub short_dust: u128,

    /// Per-bucket position counts for cap enforcement (long).
    pub long_dormant_bucket_count: u32,
    /// Per-bucket position counts for cap enforcement (short).
    pub short_dormant_bucket_count: u32,

    /// Generation counter for the long-side active series. Increments every
    /// time `rotate_active_to_recovery(Long, ...)` runs. Used to lazily
    /// migrate stale position shares.
    pub long_active_generation: u64,
    /// Same for short side.
    pub short_active_generation: u64,
    /// Rotate log: `(generation_just_ended, bucket_tick)` pairs for the long
    /// side. The on-chain implementation prunes drained entries; the host
    /// reference keeps them.
    pub long_rotate_log: Vec<RotateRecord>,
    /// Rotate log for the short side.
    pub short_rotate_log: Vec<RotateRecord>,
}

/// One rotate event recorded for lazy position migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotateRecord {
    /// The active generation that just ended.
    pub generation_just_ended: u64,
    /// Tick of the dormant bucket created or merged into during this rotate.
    pub bucket_tick: i64,
    /// Anchor price recorded at the rotate.
    pub anchor_price: u64,
}

impl SubPool {
    /// Construct a fresh subpool anchored at `last_price`.
    pub fn new(sub_pool_id: u32, last_price: u64, last_sync_slot: u64) -> Self {
        Self {
            sub_pool_id,
            long_pool_equity: 0,
            short_pool_equity: 0,
            long_active_shares: 0,
            short_active_shares: 0,
            long_recovery_shares: 0,
            short_recovery_shares: 0,
            long_active_notional: 0,
            short_active_notional: 0,
            long_dormant: DormantStore::new(Direction::Long),
            short_dormant: DormantStore::new(Direction::Short),
            last_price,
            last_sync_slot,
            long_dust: 0,
            short_dust: 0,
            long_dormant_bucket_count: 0,
            short_dormant_bucket_count: 0,
            long_active_generation: 0,
            short_active_generation: 0,
            long_rotate_log: Vec::new(),
            short_rotate_log: Vec::new(),
        }
    }

    /// Current active generation for the given direction.
    pub fn active_generation(&self, direction: Direction) -> u64 {
        match direction {
            Direction::Long => self.long_active_generation,
            Direction::Short => self.short_active_generation,
        }
    }

    /// Lookup a rotate record by `generation_just_ended` for the given
    /// direction. Returns `None` if no rotate happened for that generation.
    pub fn find_rotate_record(
        &self,
        direction: Direction,
        generation_just_ended: u64,
    ) -> Option<RotateRecord> {
        let log = match direction {
            Direction::Long => &self.long_rotate_log,
            Direction::Short => &self.short_rotate_log,
        };
        log.iter()
            .find(|r| r.generation_just_ended == generation_just_ended)
            .copied()
    }

    /// Append a rotate record for the given direction.
    pub fn push_rotate_record(&mut self, direction: Direction, record: RotateRecord) {
        match direction {
            Direction::Long => {
                self.long_rotate_log.push(record);
                self.long_active_generation = self
                    .long_active_generation
                    .checked_add(1)
                    .expect("active generation overflow");
            }
            Direction::Short => {
                self.short_rotate_log.push(record);
                self.short_active_generation = self
                    .short_active_generation
                    .checked_add(1)
                    .expect("active generation overflow");
            }
        }
    }

    /// Get directional pool equity.
    pub fn pool_equity(&self, direction: Direction) -> u128 {
        match direction {
            Direction::Long => self.long_pool_equity,
            Direction::Short => self.short_pool_equity,
        }
    }

    /// Mutable getter for directional pool equity.
    pub fn pool_equity_mut(&mut self, direction: Direction) -> &mut u128 {
        match direction {
            Direction::Long => &mut self.long_pool_equity,
            Direction::Short => &mut self.short_pool_equity,
        }
    }

    /// Get directional active shares.
    pub fn active_shares(&self, direction: Direction) -> u128 {
        match direction {
            Direction::Long => self.long_active_shares,
            Direction::Short => self.short_active_shares,
        }
    }

    /// Mutable getter for directional active shares.
    pub fn active_shares_mut(&mut self, direction: Direction) -> &mut u128 {
        match direction {
            Direction::Long => &mut self.long_active_shares,
            Direction::Short => &mut self.short_active_shares,
        }
    }

    /// Mutable getter for directional recovery shares.
    pub fn recovery_shares_mut(&mut self, direction: Direction) -> &mut u128 {
        match direction {
            Direction::Long => &mut self.long_recovery_shares,
            Direction::Short => &mut self.short_recovery_shares,
        }
    }

    /// Mutable getter for directional active notional.
    pub fn active_notional_mut(&mut self, direction: Direction) -> &mut u128 {
        match direction {
            Direction::Long => &mut self.long_active_notional,
            Direction::Short => &mut self.short_active_notional,
        }
    }

    /// Get directional active notional.
    pub fn active_notional(&self, direction: Direction) -> u128 {
        match direction {
            Direction::Long => self.long_active_notional,
            Direction::Short => self.short_active_notional,
        }
    }

    /// Mutable getter for directional dust.
    pub fn dust_mut(&mut self, direction: Direction) -> &mut u128 {
        match direction {
            Direction::Long => &mut self.long_dust,
            Direction::Short => &mut self.short_dust,
        }
    }

    /// Mutable getter for directional dormant store.
    pub fn dormant_mut(&mut self, direction: Direction) -> &mut DormantStore {
        match direction {
            Direction::Long => &mut self.long_dormant,
            Direction::Short => &mut self.short_dormant,
        }
    }

    /// Total redeemable claim across both sides at the current pool snapshot.
    pub fn total_redeemable(&self) -> u128 {
        self.long_pool_equity
            + self.short_pool_equity
            + self.long_dormant.accrued_value_total()
            + self.short_dormant.accrued_value_total()
    }
}
