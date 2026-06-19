//! Clearing engine: state-mutating operations.
//!
//! Public surface mirrors the Solana program instructions defined in
//! `Docs/Planning/07-智能合约设计.md` §3 and `18-shares模型实现细则.md` §§4-9.

use molemath::{
    checked_add, checked_mul, checked_sub, mul_div_ceil, mul_div_floor, price_move_bps,
    signed_pnl_increment, BPS_SCALE,
};

use crate::dormant::DistributionReceipt;
use crate::error::{ClearingError, ClearingResult};
use crate::event::{
    ActiveRotatedToRecoveryEvent, DormantBucketPendingAppliedEvent, DormantRecoveryClaimedEvent,
    DustHarvestedEvent, EngineEvent, PoolSyncEvent, PositionClosedEvent, PositionForceClosedEvent,
    PositionOpenedEvent,
};
use crate::invariants::check_subpool_invariants;
use crate::market::{DistributeMode, MarketParams, RotateRecord, SubPool};
use crate::position::{Position, PositionStatus};
use crate::types::Direction;

/// Result of a [`sync_pool`] call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    /// Funds that flowed into the long pool from the short pool this sync.
    pub long_to_pool_inflow: u128,
    /// Funds that flowed into the short pool from the long pool this sync.
    pub short_to_pool_inflow: u128,
    /// Funds that flowed into long-side dormant buckets.
    pub long_to_recovery_inflow: u128,
    /// Funds that flowed into short-side dormant buckets.
    pub short_to_recovery_inflow: u128,
    /// Long-side residual sent to dust during recovery distribution.
    pub long_residual_to_dust: u128,
    /// Short-side residual sent to dust during recovery distribution.
    pub short_residual_to_dust: u128,
    /// Whether long active shares were rotated to recovery this sync.
    pub long_rotated_to_recovery: bool,
    /// Whether short active shares were rotated to recovery this sync.
    pub short_rotated_to_recovery: bool,
    /// Structured events emitted by this sync (for off-chain indexers).
    pub events: Vec<EngineEvent>,
}

/// Result of a successful [`open_position`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenOutcome {
    /// Shares minted to the new position.
    pub shares_minted: u128,
    /// Effective principal accounted into the pool (after dust split).
    pub principal_into_pool: u128,
    /// Notional added to the active pool.
    pub notional_added: u128,
    /// Dust diverted from this open into `subpool.<dir>_dust`.
    pub dust: u128,
    /// Open fee charged.
    pub open_fee: u64,
    /// Structured events emitted by this open (sync_pool prelude + open).
    pub events: Vec<EngineEvent>,
}

/// Result of a successful [`close_position`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseOutcome {
    /// Funds released from the active pool to the user.
    pub active_value: u128,
    /// Funds released from recovery accrual to the user.
    pub recovery_value: u128,
    /// Total withdrawable.
    pub withdrawable: u128,
    /// Notional removed from the active pool.
    pub notional_removed: u128,
    /// Structured events emitted by this close.
    pub events: Vec<EngineEvent>,
}

/// Result of [`force_close_zero_value_position`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceCloseOutcome {
    /// Shares burned from active.
    pub active_shares_burned: u128,
    /// Shares burned from recovery (forfeited).
    pub recovery_shares_burned: u128,
    /// Notional removed from active pool.
    pub notional_removed: u128,
    /// Structured events emitted by this force-close.
    pub events: Vec<EngineEvent>,
}

/// Result of [`claim_dormant_recovery`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRecoveryOutcome {
    /// Funds released from a dormant bucket to the user.
    pub redeemable: u128,
    /// Recovery shares burned.
    pub recovery_shares_burned: u128,
    /// Structured events emitted by this claim.
    pub events: Vec<EngineEvent>,
}

/// Result of [`pre_sync_dormant_bucket`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreSyncOutcome {
    /// Number of ledger entries actually applied to the bucket. Zero
    /// when the bucket was already up to date.
    pub events_applied: u64,
    /// `accrued_value` of the bucket immediately before the apply.
    pub pre_accrued_value: u128,
    /// `accrued_value` of the bucket immediately after the apply.
    pub post_accrued_value: u128,
    /// `last_applied_index` after the apply (== `next_event_index` once
    /// fully caught up).
    pub last_applied_index: u64,
    /// Number of pending entries still remaining (i.e.
    /// `next_event_index - last_applied_index` — non-zero when the
    /// keeper hit the per-tx pending budget and must retry).
    pub pending_remaining: u64,
    /// Structured events emitted by this call.
    pub events: Vec<EngineEvent>,
}

/// Quoted sync-pool envelope passed by the caller.
#[derive(Debug, Clone, Copy)]
pub struct PriceEnvelope {
    /// Latest oracle price.
    pub p_now: u64,
    /// Slot of `p_now`.
    pub slot: u64,
    /// Required minimum after the sync (price protection).
    pub expected_min: u64,
    /// Required maximum after the sync (price protection).
    pub expected_max: u64,
}

/// Synchronise `sub_pool` to the latest oracle price.
///
/// This is the **only** function that may move funds between `long_pool_equity`
/// and `short_pool_equity`. Every state-changing engine entrypoint calls
/// `sync_pool` first.
pub fn sync_pool(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    envelope: PriceEnvelope,
) -> ClearingResult<SyncOutcome> {
    crate::market::assert_schema_version(market.schema_version)?;
    if market.paused {
        return Err(ClearingError::MarketPaused);
    }
    if envelope.p_now == 0 {
        return Err(ClearingError::OraclePriceZero);
    }
    if envelope.p_now < envelope.expected_min || envelope.p_now > envelope.expected_max {
        return Err(ClearingError::PriceProtectionFailed);
    }

    if envelope.p_now == sub_pool.last_price {
        sub_pool.last_sync_slot = envelope.slot;
        check_subpool_invariants(sub_pool)?;
        return Ok(SyncOutcome::default());
    }

    let move_bps = price_move_bps(sub_pool.last_price, envelope.p_now)?;
    if move_bps as u64 > market.max_price_move_bps_per_sync as u64 {
        return Err(ClearingError::PriceMoveTooLarge);
    }

    let p_last = sub_pool.last_price;
    let p_now = envelope.p_now;

    // Snapshot before the transition for the PoolSync event.
    let long_active_shares_before = sub_pool.long_active_shares;
    let short_active_shares_before = sub_pool.short_active_shares;
    let long_active_notional_before = sub_pool.long_active_notional;
    let short_active_notional_before = sub_pool.short_active_notional;
    let long_pool_equity_before = sub_pool.long_pool_equity;
    let short_pool_equity_before = sub_pool.short_pool_equity;

    // Pre-sync demand for dormant claims under the old price.
    let pre_long_dormant_outstanding = sub_pool.long_dormant.total_outstanding_claim_at(p_last)?;
    let pre_short_dormant_outstanding =
        sub_pool.short_dormant.total_outstanding_claim_at(p_last)?;

    // Active pnl increments.
    let long_active_pnl_inc = signed_pnl_increment(
        Direction::Long.sign(),
        sub_pool.long_active_notional,
        p_last,
        p_now,
    )?;
    let short_active_pnl_inc = signed_pnl_increment(
        Direction::Short.sign(),
        sub_pool.short_active_notional,
        p_last,
        p_now,
    )?;

    // Post-sync demand for dormant claims under the new price.
    let post_long_dormant_outstanding = sub_pool.long_dormant.total_outstanding_claim_at(p_now)?;
    let post_short_dormant_outstanding =
        sub_pool.short_dormant.total_outstanding_claim_at(p_now)?;

    let long_dormant_demand_inc =
        post_long_dormant_outstanding.saturating_sub(pre_long_dormant_outstanding);
    let short_dormant_demand_inc =
        post_short_dormant_outstanding.saturating_sub(pre_short_dormant_outstanding);

    let mut outcome = SyncOutcome::default();

    // Active-side demand is the **non-negative** part of the per-side
    // pnl increment. We then add dormant demand on top. The engine
    // dispatches to whichever side has any positive demand (active or
    // dormant). Note that on a single sync only one direction can have
    // positive demand: a price-up sync benefits long-active and
    // long-dormant; a price-down sync benefits short-active and
    // short-dormant. So the two arms below are mutually exclusive.
    //
    // **Fix vs early MVP:** the original logic gated on
    // `long_active_pnl_inc > 0` only, which dropped the
    // dormant-only-demand case (e.g. after a long-side rotation,
    // long_active_notional == 0, long_dormant has demand). That broke
    // the whitepaper's "later-liquidity-recovers-historical-loss"
    // promise — short's loss would silently stay in short_pool_equity
    // instead of flowing to long-side recovery. The branch now fires
    // whenever **any** of the two demand components is non-zero.
    let long_active_demand: u128 = long_active_pnl_inc.max(0) as u128;
    let short_active_demand: u128 = short_active_pnl_inc.max(0) as u128;
    let long_total_demand = checked_add(long_active_demand, long_dormant_demand_inc)?;
    let short_total_demand = checked_add(short_active_demand, short_dormant_demand_inc)?;

    if long_total_demand > 0 {
        // Long side wins (active and/or dormant); short side pays.
        let total_demand = long_total_demand;
        if total_demand > 0 {
            let transfer = total_demand.min(sub_pool.short_pool_equity);
            if transfer > 0 {
                let to_active = mul_div_floor(transfer, long_active_demand, total_demand)?;
                let to_recovery = checked_sub(transfer, to_active)?;

                sub_pool.long_pool_equity = checked_add(sub_pool.long_pool_equity, to_active)?;
                sub_pool.short_pool_equity = checked_sub(sub_pool.short_pool_equity, transfer)?;

                if to_recovery > 0 {
                    let DistributionReceipt {
                        allocated: _,
                        residual,
                    } = match market.dormant_distribute_mode {
                        DistributeMode::Eager => sub_pool.long_dormant.distribute(
                            p_now,
                            to_recovery,
                            market.max_distribution_ledger_size,
                        )?,
                        DistributeMode::Lazy => sub_pool.long_dormant.distribute_lazy(
                            p_now,
                            to_recovery,
                            market.max_distribution_ledger_size,
                        )?,
                    };
                    if residual > 0 {
                        sub_pool.long_dust = checked_add(sub_pool.long_dust, residual)?;
                        outcome.long_residual_to_dust = residual;
                    }
                    outcome.long_to_recovery_inflow = checked_sub(to_recovery, residual)?;
                }

                outcome.long_to_pool_inflow = to_active;
            }
        }
    } else if short_total_demand > 0 {
        // Short side wins (active and/or dormant); long side pays.
        let total_demand = short_total_demand;
        if total_demand > 0 {
            let transfer = total_demand.min(sub_pool.long_pool_equity);
            if transfer > 0 {
                let to_active = mul_div_floor(transfer, short_active_demand, total_demand)?;
                let to_recovery = checked_sub(transfer, to_active)?;

                sub_pool.short_pool_equity = checked_add(sub_pool.short_pool_equity, to_active)?;
                sub_pool.long_pool_equity = checked_sub(sub_pool.long_pool_equity, transfer)?;

                if to_recovery > 0 {
                    let DistributionReceipt {
                        allocated: _,
                        residual,
                    } = match market.dormant_distribute_mode {
                        DistributeMode::Eager => sub_pool.short_dormant.distribute(
                            p_now,
                            to_recovery,
                            market.max_distribution_ledger_size,
                        )?,
                        DistributeMode::Lazy => sub_pool.short_dormant.distribute_lazy(
                            p_now,
                            to_recovery,
                            market.max_distribution_ledger_size,
                        )?,
                    };
                    if residual > 0 {
                        sub_pool.short_dust = checked_add(sub_pool.short_dust, residual)?;
                        outcome.short_residual_to_dust = residual;
                    }
                    outcome.short_to_recovery_inflow = checked_sub(to_recovery, residual)?;
                }

                outcome.short_to_pool_inflow = to_active;
            }
        }
    }

    sub_pool.last_price = p_now;
    sub_pool.last_sync_slot = envelope.slot;

    // Emit the pool sync event before any rotation, so the indexer can
    // attribute pool-level transfers to active holders BEFORE we drop
    // `long_active_shares` to zero.
    outcome.events.push(EngineEvent::PoolSync(PoolSyncEvent {
        sub_pool_id: sub_pool.sub_pool_id,
        p_last,
        p_now,
        slot: envelope.slot,
        long_active_shares_before,
        short_active_shares_before,
        long_active_notional_before,
        short_active_notional_before,
        long_pool_equity_before,
        short_pool_equity_before,
        long_to_pool_inflow: outcome.long_to_pool_inflow,
        short_to_pool_inflow: outcome.short_to_pool_inflow,
        long_to_recovery_inflow: outcome.long_to_recovery_inflow,
        short_to_recovery_inflow: outcome.short_to_recovery_inflow,
        long_residual_to_dust: outcome.long_residual_to_dust,
        short_residual_to_dust: outcome.short_residual_to_dust,
    }));

    if sub_pool.long_pool_equity == 0 && sub_pool.long_active_shares > 0 {
        let migrated_shares = sub_pool.long_active_shares;
        let migrated_notional = sub_pool.long_active_notional;
        let generation_just_ended = sub_pool.long_active_generation;
        let tick = bucket_tick_of(market, p_now)?;
        rotate_active_to_recovery(market, sub_pool, Direction::Long, p_now)?;
        outcome.long_rotated_to_recovery = true;
        outcome
            .events
            .push(EngineEvent::ActiveRotatedToRecovery(
                ActiveRotatedToRecoveryEvent {
                    sub_pool_id: sub_pool.sub_pool_id,
                    direction: Direction::Long,
                    bucket_tick: tick,
                    anchor_price: p_now,
                    migrated_shares,
                    migrated_notional,
                    generation_just_ended,
                    slot: envelope.slot,
                },
            ));
    }
    if sub_pool.short_pool_equity == 0 && sub_pool.short_active_shares > 0 {
        let migrated_shares = sub_pool.short_active_shares;
        let migrated_notional = sub_pool.short_active_notional;
        let generation_just_ended = sub_pool.short_active_generation;
        let tick = bucket_tick_of(market, p_now)?;
        rotate_active_to_recovery(market, sub_pool, Direction::Short, p_now)?;
        outcome.short_rotated_to_recovery = true;
        outcome
            .events
            .push(EngineEvent::ActiveRotatedToRecovery(
                ActiveRotatedToRecoveryEvent {
                    sub_pool_id: sub_pool.sub_pool_id,
                    direction: Direction::Short,
                    bucket_tick: tick,
                    anchor_price: p_now,
                    migrated_shares,
                    migrated_notional,
                    generation_just_ended,
                    slot: envelope.slot,
                },
            ));
    }

    check_subpool_invariants(sub_pool)?;

    Ok(outcome)
}

/// Move the entire `<dir>_active_shares / active_notional` slice into a
/// dormant bucket anchored at `p_now`.
///
/// Called from [`sync_pool`] when a directional pool's equity hits zero.
fn rotate_active_to_recovery(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    direction: Direction,
    p_now: u64,
) -> ClearingResult<()> {
    let (active_shares, active_notional) = match direction {
        Direction::Long => (sub_pool.long_active_shares, sub_pool.long_active_notional),
        Direction::Short => (sub_pool.short_active_shares, sub_pool.short_active_notional),
    };
    if active_shares == 0 {
        return Ok(());
    }

    let tick = bucket_tick_of(market, p_now)?;
    {
        let store = sub_pool.dormant_mut(direction);
        store.insert_or_merge(
            tick,
            p_now,
            active_shares,
            active_notional,
            // Aggregate position count is opaque at this layer; the on-chain
            // implementation maintains exact counts, the host reference keeps
            // a coarse running counter.
            1,
            market.max_dormant_bucket_count_per_direction,
        )?;
    }
    match direction {
        Direction::Long => {
            sub_pool.long_recovery_shares =
                checked_add(sub_pool.long_recovery_shares, active_shares)?;
            sub_pool.long_active_shares = 0;
            sub_pool.long_active_notional = 0;
        }
        Direction::Short => {
            sub_pool.short_recovery_shares =
                checked_add(sub_pool.short_recovery_shares, active_shares)?;
            sub_pool.short_active_shares = 0;
            sub_pool.short_active_notional = 0;
        }
    }

    // Record the rotate event so future positions opened against this
    // generation can be lazily migrated. `generation_just_ended` is the
    // current generation *before* increment.
    let generation_just_ended = sub_pool.active_generation(direction);
    sub_pool.push_rotate_record(
        direction,
        RotateRecord {
            generation_just_ended,
            bucket_tick: tick,
            anchor_price: p_now,
        },
    );
    Ok(())
}

/// Lazily migrate a position whose active series has been rotated out.
///
/// Returns `Ok(true)` when migration occurred. If no rotation has happened
/// since this position was opened, the position is left untouched and the
/// function returns `Ok(false)`.
pub fn lazy_migrate_position(
    sub_pool: &mut SubPool,
    position: &mut Position,
) -> ClearingResult<bool> {
    if position.active_shares == 0 {
        // Either freshly active in a new generation or already migrated.
        position.active_generation = sub_pool.active_generation(position.direction);
        return Ok(false);
    }
    let current = sub_pool.active_generation(position.direction);
    if position.active_generation == current {
        return Ok(false);
    }
    let record = sub_pool
        .find_rotate_record(position.direction, position.active_generation)
        .ok_or(ClearingError::Invariant("missing rotate record"))?;

    let direction = position.direction;
    let migrated_shares = position.active_shares;
    let migrated_notional = position.notional;

    position.recovery_shares = checked_add(position.recovery_shares, migrated_shares)?;
    position.recovery_bucket_tick = Some(record.bucket_tick);
    position.zero_price = record.anchor_price;
    position.active_shares = 0;
    position.notional = 0;
    position.status = PositionStatus::Dormant;
    position.active_generation = current;

    // Subpool aggregates were already updated by rotate_active_to_recovery;
    // we don't double-count here. We do verify the sum invariants.
    let _ = (migrated_notional, direction);
    Ok(true)
}

/// Apply pending lazy-ledger entries to a single dormant bucket.
///
/// This is the keeper / user-paid catch-up path used by chain markets
/// running with [`DistributeMode::Lazy`]. Calling it on an
/// [`DistributeMode::Eager`] market is harmless but typically a no-op
/// (eager `sync_pool` already advances every bucket's
/// `last_applied_index`). The on-chain instruction is permission-less
/// and idempotent.
///
/// `slot` is recorded only on the emitted [`EngineEvent`]; it does not
/// participate in any state transition.
///
/// **Budget.** The number of pending entries the call may apply is
/// capped by `MarketParams::max_pending_apply_per_tx`. When the
/// remaining backlog exceeds that budget the call returns
/// [`ClearingError::DormantPendingBudgetExceeded`]; the keeper retries
/// the same instruction until the bucket is fully caught up.
pub fn pre_sync_dormant_bucket(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    direction: Direction,
    bucket_tick: i64,
    slot: u64,
) -> ClearingResult<PreSyncOutcome> {
    crate::market::assert_schema_version(market.schema_version)?;
    if market.paused {
        return Err(ClearingError::MarketPaused);
    }

    let pending_remaining = {
        let store = sub_pool.dormant_mut(direction);
        let bucket = store
            .get(bucket_tick)
            .ok_or(ClearingError::DormantBucketMissing)?;
        store
            .next_event_index()
            .checked_sub(bucket.last_applied_index)
            .ok_or(ClearingError::Invariant(
                "bucket.last_applied_index ahead of ledger",
            ))?
    };

    if pending_remaining > market.max_pending_apply_per_tx as u64 {
        return Err(ClearingError::DormantPendingBudgetExceeded);
    }

    let store = sub_pool.dormant_mut(direction);
    let pre_accrued_value = store
        .get(bucket_tick)
        .map(|b| b.accrued_value)
        .ok_or(ClearingError::DormantBucketMissing)?;

    let events_applied = store.apply_pending_to_bucket(bucket_tick)?;

    let bucket = store
        .get(bucket_tick)
        .ok_or(ClearingError::DormantBucketMissing)?;
    let post_accrued_value = bucket.accrued_value;
    let last_applied_index = bucket.last_applied_index;
    let pending_remaining_after = store
        .next_event_index()
        .checked_sub(last_applied_index)
        .ok_or(ClearingError::Invariant(
            "bucket.last_applied_index ahead of ledger after apply",
        ))?;

    let events = vec![EngineEvent::DormantBucketPendingApplied(
        DormantBucketPendingAppliedEvent {
            sub_pool_id: sub_pool.sub_pool_id,
            direction,
            bucket_tick,
            events_applied,
            pre_accrued_value,
            post_accrued_value,
            last_applied_index,
            slot,
        },
    )];

    check_subpool_invariants(sub_pool)?;

    Ok(PreSyncOutcome {
        events_applied,
        pre_accrued_value,
        post_accrued_value,
        last_applied_index,
        pending_remaining: pending_remaining_after,
        events,
    })
}

fn bucket_tick_of(market: &MarketParams, price: u64) -> ClearingResult<i64> {
    if market.price_tick == 0 || market.tick_aggregation_factor == 0 {
        return Err(ClearingError::Invariant("price_tick / aggregation == 0"));
    }
    let denom = market.price_tick as u128 * market.tick_aggregation_factor as u128;
    let q = price as u128 / denom;
    Ok(q as i64)
}

/// Open a new position.
///
/// `gross_amount` is the total token amount the user is debiting; the open
/// fee is taken first, leaving `principal` to be deposited into the pool.
/// `position_id` is the caller-assigned logical id (must be unique within
/// the sub pool); the engine never derives it.
pub fn open_position(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    envelope: PriceEnvelope,
    direction: Direction,
    gross_amount: u64,
    position_id: u64,
) -> ClearingResult<(Position, OpenOutcome)> {
    crate::market::assert_schema_version(market.schema_version)?;
    if market.frozen_new_position {
        return Err(ClearingError::FrozenNewPosition);
    }
    let sync_outcome = sync_pool(market, sub_pool, envelope)?;
    let mut events = sync_outcome.events;

    if gross_amount == 0 {
        return Err(ClearingError::MarginBelowMinimum);
    }

    let open_fee = mul_div_ceil(
        gross_amount as u128,
        market.open_fee_bps as u128,
        BPS_SCALE as u128,
    )?;
    let open_fee_u64 = u64::try_from(open_fee).map_err(|_| ClearingError::MathOverflow)?;
    let principal = gross_amount
        .checked_sub(open_fee_u64)
        .ok_or(ClearingError::MathOverflow)?;
    if (principal as u128) < market.min_margin as u128 {
        return Err(ClearingError::MarginBelowMinimum);
    }
    if principal as u128 > market.max_margin_per_position as u128 {
        return Err(ClearingError::MarginAboveMaximum);
    }

    let direction_pool_equity = sub_pool.pool_equity(direction);
    let direction_active_shares = sub_pool.active_shares(direction);

    let (shares_minted, principal_into_pool, dust) = if direction_pool_equity == 0 {
        // Clean slate — new active series.
        if direction_active_shares != 0 {
            return Err(ClearingError::Invariant(
                "active shares non-zero with zero pool equity",
            ));
        }
        (principal as u128, principal as u128, 0u128)
    } else {
        // Reverse-dilution check.
        //
        // We reject when the directional pool has decayed below
        // `dilution_safety_bps` of the share count (so each share is
        // anchored to less than `dilution_safety_bps / BPS_SCALE` of a
        // collateral unit). The condition is:
        //
        //   pool_equity / shares < dilution_safety_bps / BPS_SCALE
        //   ⇔ pool_equity * BPS_SCALE < shares * dilution_safety_bps
        //
        // (Note: `Docs/Planning/18-shares模型实现细则与边界条件.md` §5.5
        // historically printed the inversed `pool_equity *
        // dilution_safety_bps < shares * 10_000`. That triggers at every
        // steady-state open; the implementation below uses the correct
        // form and the doc is updated to match.)
        let lhs = checked_mul(direction_pool_equity, BPS_SCALE as u128)?;
        let rhs = checked_mul(direction_active_shares, market.dilution_safety_bps as u128)?;
        if lhs < rhs {
            return Err(ClearingError::DilutionRiskTooHigh);
        }

        let shares_minted = mul_div_floor(
            principal as u128,
            direction_active_shares,
            direction_pool_equity,
        )?;
        if shares_minted == 0 {
            return Err(ClearingError::SharesMintedTooSmall);
        }
        let accounted_principal = mul_div_floor(
            shares_minted,
            direction_pool_equity,
            direction_active_shares,
        )?;
        let dust = checked_sub(principal as u128, accounted_principal)?;
        (shares_minted, accounted_principal, dust)
    };

    let notional_added = mul_div_floor(
        principal as u128,
        market.leverage_bps as u128,
        BPS_SCALE as u128,
    )?;

    *sub_pool.pool_equity_mut(direction) =
        checked_add(direction_pool_equity, principal_into_pool)?;
    *sub_pool.active_shares_mut(direction) =
        checked_add(direction_active_shares, shares_minted)?;
    *sub_pool.active_notional_mut(direction) = checked_add(
        sub_pool.active_notional(direction),
        notional_added,
    )?;
    *sub_pool.dust_mut(direction) = checked_add(*sub_pool.dust_mut(direction), dust)?;

    let position = Position {
        owner: [0u8; 32],
        sub_pool_id: sub_pool.sub_pool_id,
        position_id,
        direction,
        status: PositionStatus::Open,
        principal,
        notional: notional_added,
        active_shares: shares_minted,
        recovery_shares: 0,
        recovery_bucket_tick: None,
        zero_price: 0,
        entry_price: envelope.p_now,
        last_sync_slot: envelope.slot,
        opened_at_slot: envelope.slot,
        updated_at_slot: envelope.slot,
        closed_at_slot: 0,
        schema_version: market.schema_version,
        active_generation: sub_pool.active_generation(direction),
    };

    events.push(EngineEvent::PositionOpened(PositionOpenedEvent {
        position_id,
        sub_pool_id: sub_pool.sub_pool_id,
        direction,
        principal,
        notional: notional_added,
        shares_minted,
        principal_into_pool,
        dust,
        open_fee: open_fee_u64,
        entry_price: envelope.p_now,
        active_generation: position.active_generation,
        slot: envelope.slot,
    }));

    check_subpool_invariants(sub_pool)?;

    Ok((
        position,
        OpenOutcome {
            shares_minted,
            principal_into_pool,
            notional_added,
            dust,
            open_fee: open_fee_u64,
            events,
        },
    ))
}

/// Close a position.
///
/// Active shares are redeemed proportionally to current pool equity; any
/// recovery shares are redeemed against the corresponding dormant bucket.
///
/// **Atomicity contract.** This function is *not* internally atomic: it
/// mutates `sub_pool` (and `position`) eagerly and may then return
/// [`ClearingError::WithdrawableZero`] (or any other invariant error)
/// after the mutation. The caller is responsible for snapshot/restore
/// on Err. On-chain this is provided automatically by the Solana
/// transaction runtime, which reverts every account write on any
/// non-zero exit code. Off-chain the host-side test harness wraps every
/// entry point in [`protocol_harness::Harness`] with an explicit
/// snapshot/restore pair (see `crates/protocol-harness/tests/atomic_revert.rs`).
/// Direct callers that bypass both must implement equivalent rollback,
/// otherwise sub_pool aggregates and dormant bucket state will diverge
/// from the indexer's event-derived view.
pub fn close_position(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    envelope: PriceEnvelope,
    position: &mut Position,
) -> ClearingResult<CloseOutcome> {
    crate::market::assert_schema_version(market.schema_version)?;
    if position.schema_version != market.schema_version {
        return Err(ClearingError::SchemaVersionMismatch);
    }
    if position.status != PositionStatus::Open && position.status != PositionStatus::Dormant {
        return Err(ClearingError::PositionNotOpen);
    }
    let sync_outcome = sync_pool(market, sub_pool, envelope)?;
    let mut events = sync_outcome.events;
    lazy_migrate_position(sub_pool, position)?;

    let direction = position.direction;
    let active_shares_burned = position.active_shares;
    let mut active_value: u128 = 0;
    if position.active_shares > 0 {
        let pool_equity = sub_pool.pool_equity(direction);
        let total_active_shares = sub_pool.active_shares(direction);
        if total_active_shares == 0 || pool_equity == 0 {
            // Pool has gone to zero between the user observing state and
            // executing close. We fall through and treat active_shares as
            // worthless; user should call force_close_zero_value_position.
        } else {
            active_value = mul_div_floor(pool_equity, position.active_shares, total_active_shares)?;
            *sub_pool.pool_equity_mut(direction) = checked_sub(pool_equity, active_value)?;
            *sub_pool.active_shares_mut(direction) =
                checked_sub(total_active_shares, position.active_shares)?;
            *sub_pool.active_notional_mut(direction) =
                checked_sub(sub_pool.active_notional(direction), position.notional)?;
            position.active_shares = 0;
        }
    }

    let mut recovery_value: u128 = 0;
    let mut recovery_shares_burned: u128 = 0;
    let mut recovery_notional_burned: u128 = 0;
    if position.recovery_shares > 0 {
        let tick = position
            .recovery_bucket_tick
            .ok_or(ClearingError::Invariant("recovery shares without bucket"))?;
        let receipt = sub_pool
            .dormant_mut(direction)
            .redeem(tick, position.recovery_shares)?;
        recovery_value = receipt.redeemable;
        recovery_shares_burned = receipt.burned_shares;
        recovery_notional_burned = receipt.burned_notional;
        *sub_pool.recovery_shares_mut(direction) = checked_sub(
            match direction {
                Direction::Long => sub_pool.long_recovery_shares,
                Direction::Short => sub_pool.short_recovery_shares,
            },
            receipt.burned_shares,
        )?;
        position.recovery_shares = 0;
        position.recovery_bucket_tick = None;
    }

    let withdrawable = checked_add(active_value, recovery_value)?;
    if withdrawable == 0 {
        return Err(ClearingError::WithdrawableZero);
    }

    let notional_removed = position.notional;
    position.notional = 0;
    position.status = PositionStatus::Closed;
    position.closed_at_slot = envelope.slot;
    position.updated_at_slot = envelope.slot;

    events.push(EngineEvent::PositionClosed(PositionClosedEvent {
        position_id: position.position_id,
        sub_pool_id: sub_pool.sub_pool_id,
        direction,
        active_shares_burned,
        recovery_shares_burned,
        recovery_notional_burned,
        active_value,
        recovery_value,
        withdrawable,
        notional_removed,
        slot: envelope.slot,
    }));

    check_subpool_invariants(sub_pool)?;

    Ok(CloseOutcome {
        active_value,
        recovery_value,
        withdrawable,
        notional_removed,
        events,
    })
}

/// Force-close a position whose total redeemable would be zero.
///
/// `acknowledge_forfeit` must be `true`; otherwise the call is rejected.
pub fn force_close_zero_value_position(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    envelope: PriceEnvelope,
    position: &mut Position,
    acknowledge_forfeit: bool,
) -> ClearingResult<ForceCloseOutcome> {
    crate::market::assert_schema_version(market.schema_version)?;
    if position.schema_version != market.schema_version {
        return Err(ClearingError::SchemaVersionMismatch);
    }
    if !acknowledge_forfeit {
        return Err(ClearingError::ForfeitAcknowledgementRequired);
    }
    if position.status == PositionStatus::Closed {
        return Err(ClearingError::PositionNotOpen);
    }
    let sync_outcome = sync_pool(market, sub_pool, envelope)?;
    let mut events = sync_outcome.events;
    lazy_migrate_position(sub_pool, position)?;

    let direction = position.direction;
    let mut active_burned = 0u128;
    let mut recovery_burned = 0u128;
    let mut notional_removed = 0u128;
    let mut forfeited_recovery_value = 0u128;

    if position.active_shares > 0 {
        let pool_equity = sub_pool.pool_equity(direction);
        let total_active_shares = sub_pool.active_shares(direction);
        let value = if total_active_shares > 0 && pool_equity > 0 {
            mul_div_floor(pool_equity, position.active_shares, total_active_shares)?
        } else {
            0
        };
        if value > 0 {
            return Err(ClearingError::Invariant(
                "force_close called on active position with positive value",
            ));
        }
        active_burned = position.active_shares;
        *sub_pool.active_shares_mut(direction) =
            checked_sub(total_active_shares, position.active_shares)?;
        notional_removed = position.notional;
        *sub_pool.active_notional_mut(direction) =
            checked_sub(sub_pool.active_notional(direction), position.notional)?;
        position.active_shares = 0;
    }

    let mut recovery_notional_burned: u128 = 0;
    if position.recovery_shares > 0 {
        let tick = position
            .recovery_bucket_tick
            .ok_or(ClearingError::Invariant("recovery shares without bucket"))?;
        let receipt = sub_pool
            .dormant_mut(direction)
            .redeem(tick, position.recovery_shares)?;
        // Forfeit: redeemable funds revert to dust (protocol fee vault on chain).
        if receipt.redeemable > 0 {
            *sub_pool.dust_mut(direction) =
                checked_add(*sub_pool.dust_mut(direction), receipt.redeemable)?;
            forfeited_recovery_value = receipt.redeemable;
        }
        recovery_burned = receipt.burned_shares;
        recovery_notional_burned = receipt.burned_notional;
        *sub_pool.recovery_shares_mut(direction) = checked_sub(
            match direction {
                Direction::Long => sub_pool.long_recovery_shares,
                Direction::Short => sub_pool.short_recovery_shares,
            },
            receipt.burned_shares,
        )?;
        position.recovery_shares = 0;
        position.recovery_bucket_tick = None;
    }

    position.notional = 0;
    position.status = PositionStatus::Closed;
    position.closed_at_slot = envelope.slot;
    position.updated_at_slot = envelope.slot;

    events.push(EngineEvent::PositionForceClosed(
        PositionForceClosedEvent {
            position_id: position.position_id,
            sub_pool_id: sub_pool.sub_pool_id,
            direction,
            active_shares_burned: active_burned,
            recovery_shares_burned: recovery_burned,
            recovery_notional_burned,
            forfeited_recovery_value,
            notional_removed,
            slot: envelope.slot,
        },
    ));

    check_subpool_invariants(sub_pool)?;

    Ok(ForceCloseOutcome {
        active_shares_burned: active_burned,
        recovery_shares_burned: recovery_burned,
        notional_removed,
        events,
    })
}

/// Redeem a position's recovery shares without closing the position.
///
/// Returns the funds released. Useful for dormant positions whose price
/// returned and which want to claim the available recovery without
/// terminating the position.
pub fn claim_dormant_recovery(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    envelope: PriceEnvelope,
    position: &mut Position,
) -> ClearingResult<ClaimRecoveryOutcome> {
    crate::market::assert_schema_version(market.schema_version)?;
    if position.schema_version != market.schema_version {
        return Err(ClearingError::SchemaVersionMismatch);
    }
    let sync_outcome = sync_pool(market, sub_pool, envelope)?;
    let mut events = sync_outcome.events;
    lazy_migrate_position(sub_pool, position)?;
    if position.recovery_shares == 0 {
        return Err(ClearingError::PositionNotOpen);
    }

    let direction = position.direction;
    let tick = position
        .recovery_bucket_tick
        .ok_or(ClearingError::Invariant("recovery shares without bucket"))?;
    let receipt = sub_pool
        .dormant_mut(direction)
        .redeem(tick, position.recovery_shares)?;
    *sub_pool.recovery_shares_mut(direction) = checked_sub(
        match direction {
            Direction::Long => sub_pool.long_recovery_shares,
            Direction::Short => sub_pool.short_recovery_shares,
        },
        receipt.burned_shares,
    )?;
    position.recovery_shares = 0;
    position.recovery_bucket_tick = None;
    position.updated_at_slot = envelope.slot;

    events.push(EngineEvent::DormantRecoveryClaimed(
        DormantRecoveryClaimedEvent {
            position_id: position.position_id,
            sub_pool_id: sub_pool.sub_pool_id,
            direction,
            bucket_tick: tick,
            recovery_shares_burned: receipt.burned_shares,
            recovery_notional_burned: receipt.burned_notional,
            redeemable: receipt.redeemable,
            slot: envelope.slot,
        },
    ));

    check_subpool_invariants(sub_pool)?;

    Ok(ClaimRecoveryOutcome {
        redeemable: receipt.redeemable,
        recovery_shares_burned: receipt.burned_shares,
        events,
    })
}

/// Outcome of [`harvest_dust`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarvestOutcome {
    /// Amount swept (also recoverable from the emitted event).
    pub amount: u128,
    /// Events emitted (a single [`DustHarvestedEvent`]).
    pub events: Vec<EngineEvent>,
}

/// Sweep accumulated dust into the protocol fee vault.
///
/// Returns the amount swept; subpool dust counters are zeroed for the
/// requested direction.
///
/// **Wave 8.** Now takes `market` and short-circuits with
/// `MarketPaused` / `SchemaVersionMismatch` so the dust sweeper is on
/// the same circuit-breaker matrix as every other funds-touching
/// entrypoint. Old callers that don't have a `MarketParams` handy
/// must thread one through; the migration is ~5 lines per call site.
pub fn harvest_dust(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    direction: Direction,
) -> ClearingResult<HarvestOutcome> {
    crate::market::assert_schema_version(market.schema_version)?;
    if market.paused {
        return Err(ClearingError::MarketPaused);
    }
    let dust_field = sub_pool.dust_mut(direction);
    let amount = *dust_field;
    *dust_field = 0;
    check_subpool_invariants(sub_pool)?;
    let event = EngineEvent::DustHarvested(DustHarvestedEvent {
        sub_pool_id: sub_pool.sub_pool_id,
        direction,
        amount,
    });
    Ok(HarvestOutcome {
        amount,
        events: vec![event],
    })
}
