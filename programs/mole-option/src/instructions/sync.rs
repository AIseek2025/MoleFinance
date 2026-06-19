//! `sync_pool` instruction handler.
//!
//! This handler is the **first** on-chain integration of the
//! `clearing_core` ⇄ Anchor account dormant bridge. The full call
//! lifecycle is:
//!
//! 1. Validate the Pyth price account via `pyth_adapter`.
//! 2. Cross-check the validated price against the caller's
//!    [`PriceEnvelopeArgs`] envelope.
//! 3. Snapshot the on-chain `SubPool` into a `clearing_core::SubPool`
//!    with EMPTY dormant stores.
//! 4. **Bridge in**: replace the empty stores with the result of
//!    [`dormant_bridge::unpack_direction`] for both directions (the
//!    long-side `DistributionLedger` PDA + every live long bucket
//!    PDA; same for short).
//! 5. Run `clearing_core::sync_pool`. The engine touches dormant
//!    buckets via `distribute` / `distribute_lazy` / etc.
//! 6. **Bridge out**: pack each mutated `DormantStore` back into the
//!    same accounts via `dormant_bridge::pack_direction`.
//! 7. Write the mutated `SubPool` view back.
//!
//! ### `remaining_accounts` contract
//!
//! Callers must pass the following accounts in `remaining_accounts`,
//! exactly in this order:
//!
//! | index             | account kind          | mutability |
//! |-------------------|-----------------------|------------|
//! | 0                 | DistributionLedger L  | mut        |
//! | 1                 | DistributionLedger S  | mut        |
//! | 2 .. 2+L          | DormantBucket Long    | mut        |
//! | 2+L .. 2+L+S      | DormantBucket Short   | mut        |
//!
//! `L = long_bucket_count`, `S = short_bucket_count` — both passed
//! as instruction arguments. The bucket slice MUST cover **every
//! currently-live bucket** for that direction; if a single live
//! bucket is missing the engine will silently skip its allocation
//! and the pending-distribution invariant will trip on the next
//! `unpack` (failing the tx with `Invariant`).
//!
//! Live-bucket discovery is the responsibility of the keeper / front-
//! end indexer (see `crates/indexer/src/lib.rs` for the canonical
//! enumeration). The contract is documented in
//! `Docs/Planning/23-on-chain-dormant-bridge.md`.

use anchor_lang::prelude::*;

use crate::error::{map_err, ProgramError};
use crate::state::{DistributionLedger, Market, SubPool as SubPoolAccount};

use super::dormant_bridge::run_bridged;
use super::{validate_oracle_envelope, PriceEnvelopeArgs};

#[derive(Accounts)]
pub struct SyncPool<'info> {
    #[account(mut, has_one = market)]
    pub sub_pool: Account<'info, SubPoolAccount>,
    pub market: Account<'info, Market>,
    /// Long-side distribution ledger PDA. Verified against
    /// `seeds = [b"dist_ledger", sub_pool, &[1]]`.
    #[account(
        mut,
        has_one = sub_pool,
        constraint = long_ledger.direction_is_long
            @ ProgramError::DormantBridgeAccountMismatch,
    )]
    pub long_ledger: Account<'info, DistributionLedger>,
    /// Short-side distribution ledger PDA. Verified against
    /// `seeds = [b"dist_ledger", sub_pool, &[0]]`.
    #[account(
        mut,
        has_one = sub_pool,
        constraint = !short_ledger.direction_is_long
            @ ProgramError::DormantBridgeAccountMismatch,
    )]
    pub short_ledger: Account<'info, DistributionLedger>,
    /// CHECK: oracle feed account; validated against `market.oracle_price_feed` and
    /// `market.oracle_program_id` inside the handler.
    #[account(address = market.oracle_price_feed)]
    pub oracle_price_feed: AccountInfo<'info>,
    pub clock: Sysvar<'info, Clock>,
}

pub fn sync_pool<'info>(
    ctx: Context<'_, '_, 'info, 'info, SyncPool<'info>>,
    mut envelope: PriceEnvelopeArgs,
    long_bucket_count: u32,
    short_bucket_count: u32,
) -> Result<()> {
    let market_acc = &ctx.accounts.market;
    let market_params = market_params_from(market_acc);

    validate_oracle_envelope(
        market_acc,
        &ctx.accounts.oracle_price_feed,
        ctx.accounts.clock.slot,
        &mut envelope,
    )?;

    run_bridged(
        &mut ctx.accounts.sub_pool,
        &mut ctx.accounts.long_ledger,
        &mut ctx.accounts.short_ledger,
        &market_params,
        ctx.program_id,
        ctx.remaining_accounts,
        long_bucket_count,
        short_bucket_count,
        |params, sp| {
            clearing_core::sync_pool(params, sp, envelope.into())
                .map(|_| ())
                .map_err(map_err)
        },
    )
}

pub(crate) fn market_params_from(m: &Market) -> clearing_core::MarketParams {
    let mode = match m.dormant_distribute_mode {
        0 => clearing_core::DistributeMode::Eager,
        1 => clearing_core::DistributeMode::Lazy,
        // Defensive: any unrecognised value falls back to Eager so the
        // engine never silently switches modes. The init handler enforces
        // the value is in {0, 1} via `InvalidParameter`.
        _ => clearing_core::DistributeMode::Eager,
    };
    clearing_core::MarketParams {
        leverage_bps: m.leverage_bps,
        min_margin: m.min_margin,
        max_margin_per_position: m.max_margin_per_position,
        max_total_principal: m.max_total_principal,
        max_total_notional: m.max_total_notional,
        open_fee_bps: m.open_fee_bps,
        max_oracle_age_seconds: m.max_oracle_age_seconds,
        max_confidence_bps: m.max_confidence_bps,
        max_price_move_bps_per_sync: m.max_price_move_bps_per_sync,
        price_tick: m.price_tick,
        tick_aggregation_factor: m.tick_aggregation_factor,
        max_dormant_bucket_count_per_direction: m.max_dormant_bucket_count_per_direction,
        dormant_distribute_mode: mode,
        max_pending_apply_per_tx: m.max_pending_apply_per_tx,
        max_distribution_ledger_size: m.max_distribution_ledger_size,
        dilution_safety_bps: m.dilution_safety_bps,
        max_idle_slots: m.max_idle_slots,
        schema_version: m.schema_version,
        paused: m.paused,
        frozen_new_position: m.frozen_new_position,
    }
}

/// Snapshot the Anchor `SubPool` into a `clearing_core::SubPool`
/// view. Dormant stores are intentionally LEFT EMPTY here — the
/// caller is expected to overwrite them via
/// [`unpack_direction`] before invoking the engine. Handlers that
/// don't need dormant bridging (e.g. `harvest_dust`,
/// `open_position`) may call the engine on the empty-stores view
/// because those engine entry points never read from / write to
/// dormant state.
pub(crate) fn clearing_view(sp: &SubPoolAccount) -> clearing_core::SubPool {
    let mut view = clearing_core::SubPool::new(sp.sub_pool_id, sp.last_price, sp.last_sync_slot);
    view.long_pool_equity = sp.long_pool_equity;
    view.short_pool_equity = sp.short_pool_equity;
    view.long_active_shares = sp.long_active_shares;
    view.short_active_shares = sp.short_active_shares;
    view.long_recovery_shares = sp.long_recovery_shares;
    view.short_recovery_shares = sp.short_recovery_shares;
    view.long_active_notional = sp.long_active_notional;
    view.short_active_notional = sp.short_active_notional;
    view.long_active_generation = sp.long_active_generation;
    view.short_active_generation = sp.short_active_generation;
    view.long_dust = sp.long_dust;
    view.short_dust = sp.short_dust;
    view.long_dormant_bucket_count = sp.long_dormant_bucket_count;
    view.short_dormant_bucket_count = sp.short_dormant_bucket_count;
    view
}

/// Reverse of [`clearing_view`]: writes the engine's mutated state
/// back into the Anchor `SubPool` account. Dormant aggregates
/// (`long_dormant_bucket_count`, `short_dormant_bucket_count`) are
/// persisted; the underlying `DormantStore` itself was already
/// flushed by [`pack_direction`] before this call.
pub(crate) fn apply_clearing_view(sp: &mut SubPoolAccount, view: &clearing_core::SubPool) {
    sp.long_pool_equity = view.long_pool_equity;
    sp.short_pool_equity = view.short_pool_equity;
    sp.long_active_shares = view.long_active_shares;
    sp.short_active_shares = view.short_active_shares;
    sp.long_recovery_shares = view.long_recovery_shares;
    sp.short_recovery_shares = view.short_recovery_shares;
    sp.long_active_notional = view.long_active_notional;
    sp.short_active_notional = view.short_active_notional;
    sp.long_active_generation = view.long_active_generation;
    sp.short_active_generation = view.short_active_generation;
    sp.last_price = view.last_price;
    sp.last_sync_slot = view.last_sync_slot;
    sp.long_dust = view.long_dust;
    sp.short_dust = view.short_dust;
    // Dormant bucket aggregates: the bridge keeps these in sync with
    // the underlying `DormantStore`, so the count we read off the
    // engine view (which `clearing_core::sync_pool` updates on every
    // rotate / redeem) is authoritative.
    sp.long_dormant_bucket_count = view.long_dormant_bucket_count;
    sp.short_dormant_bucket_count = view.short_dormant_bucket_count;
}
