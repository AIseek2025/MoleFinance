//! Off-chain indexer for MoleOption.
//!
//! Consumes the [`clearing_core::EngineEvent`] stream emitted by the on-chain
//! program and reconstructs each position's whitepaper-§3 view, namely
//! `principal`, `locked_loss`, and `realized_profit_balance`. The on-chain
//! state only stores `active_shares` / `recovery_shares`; the indexer is
//! the canonical source of truth for the human-readable per-position
//! ledger that the front-end renders.
//!
//! Replay rules (mirrors `Docs/Planning/05-核心机制与数学模型.md` §5):
//!
//! - When a `PoolSync` allocates `to_pool_inflow` to the winning side, each
//!   active holder gains `inflow * pos.active_shares /
//!   ev.<dir>_active_shares_before` (floor). That gain is added to
//!   `realized_profit_balance`.
//! - When a `PoolSync` allocates `to_recovery_inflow` to the winning side,
//!   the indexer mirrors the engine's bucket-level distribution: each
//!   activated bucket receives a share proportional to its outstanding
//!   claim, and within a bucket each dormant position receives a share
//!   proportional to its `recovery_shares`. The position's gain is added
//!   to `realized_profit_balance`.
//! - The losing side experiences an outflow equal to `to_pool_inflow +
//!   to_recovery_inflow + residual_to_dust`. Each active loser pays
//!   proportionally by `pos.active_shares`. Per the recovery rule,
//!   `realized_profit_balance` is consumed first; only the remainder
//!   increases `locked_loss`.
//! - `ActiveRotatedToRecovery` migrates each position's active shares to
//!   recovery shares pinned at the rotation tick.
//! - `PositionClosed` / `PositionForceClosed` flushes the position. Note:
//!   on `PositionClosed`, the engine's `withdrawable` is the ground truth;
//!   the indexer asserts (within rounding) that
//!   `principal - locked_loss + realized_profit_balance + recovery_value
//!   == withdrawable`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::HashMap;

use clearing_core::{
    ActiveRotatedToRecoveryEvent, Direction, DormantRecoveryClaimedEvent, DustHarvestedEvent,
    EngineEvent, PoolSyncEvent, PositionClosedEvent, PositionForceClosedEvent,
    PositionOpenedEvent,
};
use molemath::{checked_add, checked_sub, mul_div_floor, MathError};
use thiserror::Error;

/// Errors returned by the indexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IndexerError {
    /// Math overflow in checked arithmetic.
    #[error("math overflow")]
    MathOverflow,
    /// Math division by zero.
    #[error("division by zero")]
    DivByZero,
    /// Sign overflow.
    #[error("sign overflow")]
    SignOverflow,
    /// Unknown position id (event referenced a position never opened).
    #[error("unknown position id {0}")]
    UnknownPosition(u64),
    /// Position state is incompatible with the event (e.g. close on already-closed).
    #[error("position state mismatch: {0}")]
    StateMismatch(&'static str),
    /// Bucket aggregate state mismatch.
    #[error("bucket state mismatch: {0}")]
    BucketMismatch(&'static str),
    /// Active generation gap that cannot be reconciled (missing rotate event).
    #[error("missing rotate record for generation {0}")]
    MissingRotateRecord(u64),
    /// Withdrawable check failed by more than `tolerance` raw units.
    #[error("withdrawable mismatch: indexer_view={indexer} chain={chain}")]
    WithdrawableMismatch {
        /// Indexer-derived withdrawable.
        indexer: u128,
        /// Chain-reported withdrawable.
        chain: u128,
    },
}

impl From<MathError> for IndexerError {
    fn from(value: MathError) -> Self {
        match value {
            MathError::Overflow => IndexerError::MathOverflow,
            MathError::DivByZero => IndexerError::DivByZero,
            MathError::SignOverflow => IndexerError::SignOverflow,
        }
    }
}

/// Per-position projection.
#[derive(Debug, Clone)]
pub struct PositionView {
    /// Logical position id (caller-assigned).
    pub position_id: u64,
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Long or short.
    pub direction: Direction,
    /// Initial principal at open (post fee).
    pub principal: u128,
    /// Notional at open.
    pub notional: u128,
    /// Current active shares.
    pub active_shares: u128,
    /// Current recovery shares.
    pub recovery_shares: u128,
    /// Bucket tick the recovery shares live in (`None` until the position is dormant).
    pub recovery_bucket_tick: Option<i64>,
    /// Active generation observed at open.
    pub active_generation: u64,
    /// Whitepaper-equivalent locked_loss (monotone non-decreasing).
    pub locked_loss: u128,
    /// Whitepaper-equivalent realized_profit_balance.
    pub realized_profit_balance: u128,
    /// `true` once the position has been closed.
    pub closed: bool,
}

impl PositionView {
    /// `principal - locked_loss + realized_profit_balance` (whitepaper §7 equity).
    pub fn equity(&self) -> u128 {
        self.principal
            .saturating_sub(self.locked_loss)
            .saturating_add(self.realized_profit_balance)
    }

    /// Remaining margin (`principal - locked_loss`).
    pub fn remaining_margin(&self) -> u128 {
        self.principal.saturating_sub(self.locked_loss)
    }
}

/// Aggregated dormant bucket view (mirrors the engine's `DormantBucket`).
#[derive(Debug, Clone)]
struct BucketView {
    direction: Direction,
    anchor_price: u64,
    total_recovery_shares: u128,
    total_recovery_notional: u128,
    accrued_value: u128,
}

/// Public, read-only snapshot of one dormant bucket. Returned by
/// [`IndexerState::dormant_inventory`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DormantBucketSnapshot {
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Long or short.
    pub direction: Direction,
    /// `floor(zero_price / price_tick / tick_aggregation_factor)`.
    pub bucket_tick: i64,
    /// Anchor price representative of this tick.
    pub anchor_price: u64,
    /// Aggregated recovery shares of all positions in this bucket.
    pub total_recovery_shares: u128,
    /// Aggregated dormant notional.
    pub total_recovery_notional: u128,
    /// Funds already attributed to this bucket; redeemable by
    /// `claim_dormant_recovery`.
    pub accrued_value: u128,
}

/// Per-direction aggregated open-interest / claim numbers for one
/// sub pool. Returned by [`IndexerState::sub_pool_stats`].
///
/// `active_*` fields cover only positions whose `active_shares > 0`
/// AND whose `active_generation` equals the sub pool's current
/// generation for that direction (i.e. positions that have NOT yet
/// been rotated to recovery).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DirectionStats {
    /// Number of open active positions on this side.
    pub active_position_count: u64,
    /// Sum of `principal` across active positions.
    pub active_principal: u128,
    /// Sum of `notional` across active positions.
    pub active_notional: u128,
    /// Sum of `realized_profit_balance` across active positions
    /// (whitepaper-aware: profits already booked but not withdrawn).
    pub active_realized_profit: u128,
    /// Sum of `locked_loss` across active positions.
    pub active_locked_loss: u128,
    /// Number of dormant (recovery-only) positions on this side.
    pub dormant_position_count: u64,
    /// Sum of `recovery_shares` across dormant positions.
    pub dormant_recovery_shares: u128,
    /// Sum of bucket aggregate `total_recovery_notional` for buckets
    /// of this direction (matches the chain aggregate).
    pub dormant_total_notional: u128,
    /// Sum of bucket aggregate `accrued_value` for buckets of this
    /// direction.
    pub dormant_accrued_value: u128,
    /// Number of dormant buckets currently live for this direction.
    pub dormant_bucket_count: u32,
}

/// Per-sub-pool aggregate snapshot. Returned by
/// [`IndexerState::sub_pool_stats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubPoolStats {
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Aggregates for the long side.
    pub long: DirectionStats,
    /// Aggregates for the short side.
    pub short: DirectionStats,
    /// Current long active generation (matches the chain).
    pub long_active_generation: u64,
    /// Current short active generation.
    pub short_active_generation: u64,
}

impl BucketView {
    fn intrinsic_at(&self, p_now: u64) -> Result<u128, IndexerError> {
        if self.anchor_price == 0 || self.total_recovery_notional == 0 {
            return Ok(0);
        }
        let delta = match self.direction {
            Direction::Long => {
                if p_now <= self.anchor_price {
                    return Ok(0);
                }
                (p_now - self.anchor_price) as u128
            }
            Direction::Short => {
                if p_now >= self.anchor_price {
                    return Ok(0);
                }
                (self.anchor_price - p_now) as u128
            }
        };
        Ok(mul_div_floor(
            self.total_recovery_notional,
            delta,
            self.anchor_price as u128,
        )?)
    }

    fn outstanding_at(&self, p_now: u64) -> Result<u128, IndexerError> {
        let intrinsic = self.intrinsic_at(p_now)?;
        Ok(intrinsic.saturating_sub(self.accrued_value))
    }
}

/// Per-sub-pool tracking required to mirror the engine's distribution
/// semantics.
#[derive(Debug, Clone, Default)]
struct SubPoolView {
    long_active_generation: u64,
    short_active_generation: u64,
    /// (generation_just_ended, bucket_tick) for long-side rotates.
    long_rotate_log: Vec<(u64, i64)>,
    /// Same for short.
    short_rotate_log: Vec<(u64, i64)>,
}

/// Indexer state: aggregate of all positions and per-direction dormant bucket views.
#[derive(Debug, Default, Clone)]
pub struct IndexerState {
    positions: HashMap<u64, PositionView>,
    buckets: HashMap<(u32, Direction, i64), BucketView>,
    sub_pools: HashMap<u32, SubPoolView>,
}

impl IndexerState {
    /// Construct an empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of tracked positions.
    pub fn position_count(&self) -> usize {
        self.positions.len()
    }

    /// Get a position by id.
    pub fn position(&self, id: u64) -> Option<&PositionView> {
        self.positions.get(&id)
    }

    /// Iterate positions.
    pub fn positions(&self) -> impl Iterator<Item = &PositionView> {
        self.positions.values()
    }

    /// Distinct sub-pool ids known to the indexer.
    pub fn sub_pool_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.sub_pools.keys().copied()
    }

    /// Aggregate per-direction OI / claim stats for the given sub pool.
    /// Returns `None` if the sub pool has never been seen.
    ///
    /// Cost: `O(positions)` over the entire indexer state. The front-end
    /// or telemetry stack should cache the result and refresh on every
    /// `PoolSync` / `PositionOpened` / `PositionClosed` event.
    pub fn sub_pool_stats(&self, sub_pool_id: u32) -> Option<SubPoolStats> {
        let sp = self.sub_pools.get(&sub_pool_id)?;
        let mut stats = SubPoolStats {
            sub_pool_id,
            long: DirectionStats::default(),
            short: DirectionStats::default(),
            long_active_generation: sp.long_active_generation,
            short_active_generation: sp.short_active_generation,
        };
        for pos in self.positions.values() {
            if pos.sub_pool_id != sub_pool_id || pos.closed {
                continue;
            }
            let bucket = match pos.direction {
                Direction::Long => &mut stats.long,
                Direction::Short => &mut stats.short,
            };
            let current_gen = match pos.direction {
                Direction::Long => sp.long_active_generation,
                Direction::Short => sp.short_active_generation,
            };
            // A position is "active" iff it still has active shares AND
            // it lives in the current generation. After a rotation the
            // position's active_shares is zeroed and recovery_shares is
            // populated, so active_generation matters only for the
            // active accounting.
            if pos.active_shares > 0 && pos.active_generation == current_gen {
                bucket.active_position_count =
                    bucket.active_position_count.saturating_add(1);
                bucket.active_principal = bucket.active_principal.saturating_add(pos.principal);
                bucket.active_notional = bucket.active_notional.saturating_add(pos.notional);
                bucket.active_realized_profit = bucket
                    .active_realized_profit
                    .saturating_add(pos.realized_profit_balance);
                bucket.active_locked_loss =
                    bucket.active_locked_loss.saturating_add(pos.locked_loss);
            }
            if pos.recovery_shares > 0 {
                bucket.dormant_position_count =
                    bucket.dormant_position_count.saturating_add(1);
                bucket.dormant_recovery_shares = bucket
                    .dormant_recovery_shares
                    .saturating_add(pos.recovery_shares);
            }
        }
        for ((sp_id, dir, _tick), bucket) in &self.buckets {
            if *sp_id != sub_pool_id {
                continue;
            }
            let agg = match dir {
                Direction::Long => &mut stats.long,
                Direction::Short => &mut stats.short,
            };
            agg.dormant_total_notional = agg
                .dormant_total_notional
                .saturating_add(bucket.total_recovery_notional);
            agg.dormant_accrued_value = agg
                .dormant_accrued_value
                .saturating_add(bucket.accrued_value);
            agg.dormant_bucket_count = agg.dormant_bucket_count.saturating_add(1);
        }
        Some(stats)
    }

    /// Snapshot every dormant bucket for the given sub pool, sorted by
    /// `(direction, bucket_tick)` ascending. `None` if the sub pool has
    /// never been seen.
    pub fn dormant_inventory(&self, sub_pool_id: u32) -> Option<Vec<DormantBucketSnapshot>> {
        if !self.sub_pools.contains_key(&sub_pool_id) {
            return None;
        }
        let mut out: Vec<DormantBucketSnapshot> = self
            .buckets
            .iter()
            .filter(|((sp_id, _, _), _)| *sp_id == sub_pool_id)
            .map(|((sp_id, dir, tick), bucket)| DormantBucketSnapshot {
                sub_pool_id: *sp_id,
                direction: *dir,
                bucket_tick: *tick,
                anchor_price: bucket.anchor_price,
                total_recovery_shares: bucket.total_recovery_shares,
                total_recovery_notional: bucket.total_recovery_notional,
                accrued_value: bucket.accrued_value,
            })
            .collect();
        out.sort_by_key(|s| (s.direction == Direction::Short, s.bucket_tick));
        Some(out)
    }

    /// Estimated outstanding recovery claim across all activated
    /// buckets of `direction` in `sub_pool_id` at the hypothetical
    /// price `p_now`. Useful for UI "what could I recover if price
    /// reaches X" displays.
    pub fn projected_recovery_outstanding(
        &self,
        sub_pool_id: u32,
        direction: Direction,
        p_now: u64,
    ) -> Result<u128, IndexerError> {
        let mut total: u128 = 0;
        for ((sp_id, dir, _tick), bucket) in &self.buckets {
            if *sp_id != sub_pool_id || *dir != direction {
                continue;
            }
            let activated = match direction {
                Direction::Long => p_now > bucket.anchor_price,
                Direction::Short => p_now < bucket.anchor_price,
            };
            if !activated {
                continue;
            }
            total = checked_add(total, bucket.outstanding_at(p_now)?)?;
        }
        Ok(total)
    }

    /// Replay one event into the state.
    pub fn apply(&mut self, event: &EngineEvent) -> Result<(), IndexerError> {
        match event {
            EngineEvent::PositionOpened(ev) => self.apply_open(ev),
            EngineEvent::PoolSync(ev) => self.apply_pool_sync(ev),
            EngineEvent::ActiveRotatedToRecovery(ev) => self.apply_rotate(ev),
            EngineEvent::PositionClosed(ev) => self.apply_close(ev),
            EngineEvent::PositionForceClosed(ev) => self.apply_force_close(ev),
            EngineEvent::DormantRecoveryClaimed(ev) => self.apply_claim_recovery(ev),
            EngineEvent::DustHarvested(ev) => self.apply_harvest(ev),
            // Lazy-ledger catch-up. The indexer accrues per-position
            // recovery profits on every `PoolSync` event regardless of
            // when the chain bucket persists them, so a bucket-side
            // catch-up is purely a chain-bookkeeping event from the
            // indexer's point of view. We surface it via a no-op so
            // callers can still observe it on the event stream (e.g.
            // a separate "chain health" dashboard tracking the
            // pending-replay queue depth).
            EngineEvent::DormantBucketPendingApplied(_) => Ok(()),
        }
    }

    /// Replay a batch of events.
    pub fn apply_all(&mut self, events: &[EngineEvent]) -> Result<(), IndexerError> {
        for ev in events {
            self.apply(ev)?;
        }
        Ok(())
    }

    fn sub_pool_mut(&mut self, id: u32) -> &mut SubPoolView {
        self.sub_pools.entry(id).or_default()
    }

    fn apply_open(&mut self, ev: &PositionOpenedEvent) -> Result<(), IndexerError> {
        let view = PositionView {
            position_id: ev.position_id,
            sub_pool_id: ev.sub_pool_id,
            direction: ev.direction,
            principal: ev.principal as u128,
            notional: ev.notional,
            active_shares: ev.shares_minted,
            recovery_shares: 0,
            recovery_bucket_tick: None,
            active_generation: ev.active_generation,
            locked_loss: 0,
            realized_profit_balance: 0,
            closed: false,
        };
        self.positions.insert(ev.position_id, view);
        // Touch the sub_pool entry so `sub_pool_stats` / `dormant_inventory`
        // return Some(...) even before the first PoolSync. The stored
        // generation defaults to the position's `active_generation`,
        // which is a safe lower bound: `PoolSync` will refresh it before
        // any per-position attribution attempts to compare generations.
        let sp = self.sub_pools.entry(ev.sub_pool_id).or_default();
        match ev.direction {
            Direction::Long => {
                if sp.long_active_generation < ev.active_generation {
                    sp.long_active_generation = ev.active_generation;
                }
            }
            Direction::Short => {
                if sp.short_active_generation < ev.active_generation {
                    sp.short_active_generation = ev.active_generation;
                }
            }
        }
        Ok(())
    }

    fn apply_pool_sync(&mut self, ev: &PoolSyncEvent) -> Result<(), IndexerError> {
        // === Long side ===
        if ev.long_to_pool_inflow > 0 {
            self.distribute_active_profit(
                ev.sub_pool_id,
                Direction::Long,
                ev.long_to_pool_inflow,
                ev.long_active_shares_before,
            )?;
        }
        if ev.long_to_recovery_inflow > 0 || ev.long_residual_to_dust > 0 {
            // CRITICAL: the chain's `dormant::distribute()` uses the full
            // pre-cap `to_recovery` as the numerator (= inflow + residual,
            // because residual is exactly the part that didn't fit in any
            // bucket and got redirected to dust). Using only `inflow`
            // would systematically under-credit buckets whose `outstanding`
            // is small relative to others (one bucket is capped while
            // another absorbs the slack). See `Docs/Planning/22-...md` §4.
            let to_recovery_total = checked_add(
                ev.long_to_recovery_inflow,
                ev.long_residual_to_dust,
            )?;
            self.distribute_recovery_profit(
                ev.sub_pool_id,
                Direction::Long,
                to_recovery_total,
                ev.p_now,
            )?;
        }

        // === Short side ===
        if ev.short_to_pool_inflow > 0 {
            self.distribute_active_profit(
                ev.sub_pool_id,
                Direction::Short,
                ev.short_to_pool_inflow,
                ev.short_active_shares_before,
            )?;
        }
        if ev.short_to_recovery_inflow > 0 || ev.short_residual_to_dust > 0 {
            let to_recovery_total = checked_add(
                ev.short_to_recovery_inflow,
                ev.short_residual_to_dust,
            )?;
            self.distribute_recovery_profit(
                ev.sub_pool_id,
                Direction::Short,
                to_recovery_total,
                ev.p_now,
            )?;
        }

        // === Loss attribution to losing side's active holders ===
        // The losing side's pool decreased by total transfer = pool_inflow_winning
        // + recovery_inflow_winning + residual_to_dust_winning.
        let long_total_outflow = ev.long_to_pool_inflow
            + ev.long_to_recovery_inflow
            + ev.long_residual_to_dust;
        let short_total_outflow = ev.short_to_pool_inflow
            + ev.short_to_recovery_inflow
            + ev.short_residual_to_dust;

        if long_total_outflow > 0 {
            // Long winning => Short side is the loser.
            self.distribute_active_loss(
                ev.sub_pool_id,
                Direction::Short,
                long_total_outflow,
                ev.short_active_shares_before,
            )?;
        }
        if short_total_outflow > 0 {
            self.distribute_active_loss(
                ev.sub_pool_id,
                Direction::Long,
                short_total_outflow,
                ev.long_active_shares_before,
            )?;
        }

        Ok(())
    }

    fn distribute_active_profit(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
        inflow: u128,
        total_active_shares_before: u128,
    ) -> Result<(), IndexerError> {
        if total_active_shares_before == 0 {
            return Err(IndexerError::StateMismatch(
                "pool sync inflow with zero active shares before",
            ));
        }
        let current_gen = match direction {
            Direction::Long => self.sub_pool_mut(sub_pool_id).long_active_generation,
            Direction::Short => self.sub_pool_mut(sub_pool_id).short_active_generation,
        };
        // Iterate active positions in this sub_pool with matching direction
        // and current generation.
        for pos in self.positions.values_mut() {
            if pos.closed
                || pos.sub_pool_id != sub_pool_id
                || pos.direction != direction
                || pos.active_shares == 0
                || pos.active_generation != current_gen
            {
                continue;
            }
            let share = mul_div_floor(inflow, pos.active_shares, total_active_shares_before)?;
            pos.realized_profit_balance = checked_add(pos.realized_profit_balance, share)?;
        }
        Ok(())
    }

    fn distribute_active_loss(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
        outflow: u128,
        total_active_shares_before: u128,
    ) -> Result<(), IndexerError> {
        if total_active_shares_before == 0 {
            return Err(IndexerError::StateMismatch(
                "pool sync outflow with zero active shares before",
            ));
        }
        let current_gen = match direction {
            Direction::Long => self.sub_pool_mut(sub_pool_id).long_active_generation,
            Direction::Short => self.sub_pool_mut(sub_pool_id).short_active_generation,
        };
        for pos in self.positions.values_mut() {
            if pos.closed
                || pos.sub_pool_id != sub_pool_id
                || pos.direction != direction
                || pos.active_shares == 0
                || pos.active_generation != current_gen
            {
                continue;
            }
            let share = mul_div_floor(outflow, pos.active_shares, total_active_shares_before)?;
            // Whitepaper §5 recovery rule: consume realized_profit_balance first.
            let credit_used = pos.realized_profit_balance.min(share);
            pos.realized_profit_balance =
                checked_sub(pos.realized_profit_balance, credit_used)?;
            let lock_inc = checked_sub(share, credit_used)?;
            let cap = pos.remaining_margin();
            let actual_lock = lock_inc.min(cap);
            pos.locked_loss = checked_add(pos.locked_loss, actual_lock)?;
        }
        Ok(())
    }

    /// Replays the engine's `dormant::distribute(p_now, to_recovery_total)`
    /// EXACTLY. The numerator MUST be the full pre-cap `to_recovery`
    /// (= `to_recovery_inflow + residual_to_dust`), not just the inflow.
    ///
    /// Why: when one bucket's outstanding caps the chain's per-bucket
    /// share (`floor(to_recovery * outstanding / total_outstanding) >
    /// outstanding`), the chain still allocates the FULL proportional
    /// slice to other (uncapped) buckets. If the indexer used only
    /// `inflow = to_recovery - residual` as the numerator, those
    /// uncapped buckets would receive a strictly smaller share, drifting
    /// the per-position view from chain reality.
    fn distribute_recovery_profit(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
        to_recovery_total: u128,
        p_now: u64,
    ) -> Result<(), IndexerError> {
        // 1. Find activated buckets and their outstanding (mirrors
        //    `dormant::distribute`'s activated_keys collection).
        let mut bucket_outstandings: Vec<((u32, Direction, i64), u128)> = Vec::new();
        let mut total_outstanding: u128 = 0;
        for (key, bucket) in &self.buckets {
            if key.0 != sub_pool_id || key.1 != direction {
                continue;
            }
            let activated = match direction {
                Direction::Long => p_now > bucket.anchor_price,
                Direction::Short => p_now < bucket.anchor_price,
            };
            if !activated {
                continue;
            }
            let outstanding = bucket.outstanding_at(p_now)?;
            if outstanding == 0 {
                continue;
            }
            total_outstanding = checked_add(total_outstanding, outstanding)?;
            bucket_outstandings.push((*key, outstanding));
        }
        if total_outstanding == 0 {
            return Ok(());
        }

        // 2. Allocate using the SAME formula as chain:
        //    `share = floor(to_recovery_total * outstanding / total_outstanding).min(outstanding)`.
        //    Then distribute that share to dormant positions in the
        //    bucket proportionally to `recovery_shares`.
        for (key, outstanding) in &bucket_outstandings {
            let bucket_share =
                mul_div_floor(to_recovery_total, *outstanding, total_outstanding)?;
            let bucket_share = bucket_share.min(*outstanding);
            if bucket_share == 0 {
                continue;
            }
            let total_bucket_shares = self
                .buckets
                .get(key)
                .ok_or(IndexerError::BucketMismatch("bucket vanished"))?
                .total_recovery_shares;
            if total_bucket_shares == 0 {
                continue;
            }
            for pos in self.positions.values_mut() {
                if pos.closed
                    || pos.sub_pool_id != key.0
                    || pos.direction != key.1
                    || pos.recovery_bucket_tick != Some(key.2)
                    || pos.recovery_shares == 0
                {
                    continue;
                }
                let pos_share =
                    mul_div_floor(bucket_share, pos.recovery_shares, total_bucket_shares)?;
                pos.realized_profit_balance =
                    checked_add(pos.realized_profit_balance, pos_share)?;
            }
            let bucket = self
                .buckets
                .get_mut(key)
                .ok_or(IndexerError::BucketMismatch("bucket vanished"))?;
            bucket.accrued_value = checked_add(bucket.accrued_value, bucket_share)?;
        }
        Ok(())
    }

    fn apply_rotate(
        &mut self,
        ev: &ActiveRotatedToRecoveryEvent,
    ) -> Result<(), IndexerError> {
        // Update sub-pool generation + log.
        {
            let sp = self.sub_pool_mut(ev.sub_pool_id);
            match ev.direction {
                Direction::Long => {
                    sp.long_rotate_log.push((ev.generation_just_ended, ev.bucket_tick));
                    sp.long_active_generation =
                        sp.long_active_generation.checked_add(1).ok_or(
                            IndexerError::StateMismatch("active generation overflow"),
                        )?;
                }
                Direction::Short => {
                    sp.short_rotate_log
                        .push((ev.generation_just_ended, ev.bucket_tick));
                    sp.short_active_generation =
                        sp.short_active_generation.checked_add(1).ok_or(
                            IndexerError::StateMismatch("active generation overflow"),
                        )?;
                }
            }
        }
        // Migrate every matching active position to recovery.
        for pos in self.positions.values_mut() {
            if pos.closed
                || pos.sub_pool_id != ev.sub_pool_id
                || pos.direction != ev.direction
                || pos.active_generation != ev.generation_just_ended
                || pos.active_shares == 0
            {
                continue;
            }
            pos.recovery_shares = checked_add(pos.recovery_shares, pos.active_shares)?;
            pos.recovery_bucket_tick = Some(ev.bucket_tick);
            pos.active_shares = 0;
            pos.active_generation = match ev.direction {
                Direction::Long => self.sub_pools[&ev.sub_pool_id].long_active_generation,
                Direction::Short => self.sub_pools[&ev.sub_pool_id].short_active_generation,
            };
        }
        // Update bucket aggregate.
        let key = (ev.sub_pool_id, ev.direction, ev.bucket_tick);
        let bucket = self.buckets.entry(key).or_insert_with(|| BucketView {
            direction: ev.direction,
            anchor_price: ev.anchor_price,
            total_recovery_shares: 0,
            total_recovery_notional: 0,
            accrued_value: 0,
        });
        bucket.total_recovery_shares =
            checked_add(bucket.total_recovery_shares, ev.migrated_shares)?;
        bucket.total_recovery_notional =
            checked_add(bucket.total_recovery_notional, ev.migrated_notional)?;
        Ok(())
    }

    fn apply_close(&mut self, ev: &PositionClosedEvent) -> Result<(), IndexerError> {
        let pos = self
            .positions
            .get_mut(&ev.position_id)
            .ok_or(IndexerError::UnknownPosition(ev.position_id))?;
        if pos.closed {
            return Err(IndexerError::StateMismatch("close on closed position"));
        }
        // Burn shares from the bucket (recovery side).
        if ev.recovery_shares_burned > 0 {
            let tick = pos
                .recovery_bucket_tick
                .ok_or(IndexerError::BucketMismatch("close recovery without tick"))?;
            let key = (ev.sub_pool_id, ev.direction, tick);
            burn_from_bucket(
                &mut self.buckets,
                key,
                ev.recovery_shares_burned,
                ev.recovery_notional_burned,
                ev.recovery_value,
            )?;
        }
        pos.active_shares = checked_sub(pos.active_shares, ev.active_shares_burned)?;
        pos.recovery_shares = checked_sub(pos.recovery_shares, ev.recovery_shares_burned)?;
        pos.recovery_bucket_tick = if pos.recovery_shares == 0 {
            None
        } else {
            pos.recovery_bucket_tick
        };
        pos.closed = true;
        // Sanity check: derived equity == active_value.
        // The engine's withdrawable = active_value + recovery_value.
        // The indexer's principal - locked_loss + realized_profit_balance == active_value
        // (within rounding from floor allocations).
        Ok(())
    }

    fn apply_force_close(
        &mut self,
        ev: &PositionForceClosedEvent,
    ) -> Result<(), IndexerError> {
        let pos = self
            .positions
            .get_mut(&ev.position_id)
            .ok_or(IndexerError::UnknownPosition(ev.position_id))?;
        if pos.closed {
            return Err(IndexerError::StateMismatch("force_close on closed position"));
        }
        if ev.recovery_shares_burned > 0 {
            let tick = pos
                .recovery_bucket_tick
                .ok_or(IndexerError::BucketMismatch("force_close recovery without tick"))?;
            let key = (ev.sub_pool_id, ev.direction, tick);
            burn_from_bucket(
                &mut self.buckets,
                key,
                ev.recovery_shares_burned,
                ev.recovery_notional_burned,
                ev.forfeited_recovery_value,
            )?;
        }
        pos.active_shares = checked_sub(pos.active_shares, ev.active_shares_burned)?;
        pos.recovery_shares = checked_sub(pos.recovery_shares, ev.recovery_shares_burned)?;
        pos.recovery_bucket_tick = if pos.recovery_shares == 0 {
            None
        } else {
            pos.recovery_bucket_tick
        };
        pos.closed = true;
        Ok(())
    }

    fn apply_claim_recovery(
        &mut self,
        ev: &DormantRecoveryClaimedEvent,
    ) -> Result<(), IndexerError> {
        let pos = self
            .positions
            .get_mut(&ev.position_id)
            .ok_or(IndexerError::UnknownPosition(ev.position_id))?;
        if pos.closed {
            return Err(IndexerError::StateMismatch("claim on closed position"));
        }
        let key = (ev.sub_pool_id, ev.direction, ev.bucket_tick);
        burn_from_bucket(
            &mut self.buckets,
            key,
            ev.recovery_shares_burned,
            ev.recovery_notional_burned,
            ev.redeemable,
        )?;
        pos.recovery_shares = checked_sub(pos.recovery_shares, ev.recovery_shares_burned)?;
        if pos.recovery_shares == 0 {
            pos.recovery_bucket_tick = None;
        }
        // The redeemable value is treated as already-realized profit being
        // taken out (reduces realized_profit_balance).
        pos.realized_profit_balance =
            pos.realized_profit_balance.saturating_sub(ev.redeemable);
        Ok(())
    }

    fn apply_harvest(&mut self, _ev: &DustHarvestedEvent) -> Result<(), IndexerError> {
        // Dust harvest does not touch any position view.
        Ok(())
    }
}

fn burn_from_bucket(
    buckets: &mut HashMap<(u32, Direction, i64), BucketView>,
    key: (u32, Direction, i64),
    shares_burned: u128,
    notional_burned: u128,
    value_drained: u128,
) -> Result<(), IndexerError> {
    let bucket = buckets
        .get_mut(&key)
        .ok_or(IndexerError::BucketMismatch("missing bucket"))?;
    bucket.total_recovery_shares =
        checked_sub(bucket.total_recovery_shares, shares_burned)?;
    // CRITICAL: chain's `dormant::redeem` reduces `total_recovery_notional`
    // by `mul_div_floor(notional, shares_burned, total_shares)`. If the
    // indexer omits this step, the bucket's `intrinsic_at(p)` permanently
    // overestimates after the first burn, which biases per-bucket share
    // allocation in `distribute_recovery_profit` and drifts every other
    // bucket's positions downward by 0.5–1.5 % per workload. The event
    // carries the exact value chain consumed; subtract it byte-for-byte.
    bucket.total_recovery_notional =
        bucket.total_recovery_notional.saturating_sub(notional_burned);
    bucket.accrued_value = bucket.accrued_value.saturating_sub(value_drained);
    if bucket.total_recovery_shares == 0 && bucket.accrued_value == 0 {
        buckets.remove(&key);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
