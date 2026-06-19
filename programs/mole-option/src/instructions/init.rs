//! Account initialization handlers.
//!
//! Skeleton only — production deployment requires a Squads multi-sig
//! deployer per `Docs/Planning/16-合约升级与治理紧急响应.md`.

use anchor_lang::prelude::*;

use crate::state::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct InitGlobalConfigParams {
    pub admin_authority: Pubkey,
    pub emergency_authority: Pubkey,
    pub protocol_treasury: Pubkey,
    pub upgrade_authority: Pubkey,
}

#[derive(Accounts)]
pub struct InitializeGlobalConfig<'info> {
    #[account(
        init,
        payer = payer,
        space = GlobalConfig::LEN,
        seeds = [b"global_config"],
        bump,
    )]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

pub fn initialize_global_config(
    ctx: Context<InitializeGlobalConfig>,
    params: InitGlobalConfigParams,
) -> Result<()> {
    let cfg = &mut ctx.accounts.global_config;
    cfg.admin_authority = params.admin_authority;
    cfg.emergency_authority = params.emergency_authority;
    cfg.protocol_treasury = params.protocol_treasury;
    cfg.upgrade_authority = params.upgrade_authority;
    cfg.paused_globally = false;
    cfg.schema_version = 1;
    cfg.bump = ctx.bumps.global_config;
    Ok(())
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct InitMarketParams {
    pub symbol: [u8; 16],
    pub leverage_bps: u32,
    pub min_margin: u64,
    pub max_margin_per_position: u64,
    pub max_total_principal: u128,
    pub max_total_notional: u128,
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
    pub sub_pool_count: u32,
    /// 0 = Eager, 1 = Lazy. See `Market::dormant_distribute_mode`.
    pub dormant_distribute_mode: u8,
    pub max_pending_apply_per_tx: u32,
    /// Hard cap on the per-direction distribution ledger ring buffer
    /// size. See `clearing_core::MarketParams::max_distribution_ledger_size`.
    pub max_distribution_ledger_size: u32,
}

#[derive(Accounts)]
#[instruction(params: InitMarketParams)]
pub struct InitializeMarket<'info> {
    #[account(seeds = [b"global_config"], bump = global_config.bump)]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(
        init,
        payer = payer,
        space = Market::LEN,
        seeds = [b"market".as_ref(), params.symbol.as_ref()],
        bump,
    )]
    pub market: Account<'info, Market>,
    /// CHECK: collateral mint, validated off-band.
    pub collateral_mint: AccountInfo<'info>,
    /// CHECK: market vault PDA, created off this instruction by anchor-spl
    /// in a follow-up tx.
    pub vault: AccountInfo<'info>,
    /// CHECK: fee vault PDA.
    pub fee_vault: AccountInfo<'info>,
    /// CHECK: oracle feed account.
    pub oracle_price_feed: AccountInfo<'info>,
    /// CHECK: oracle program id.
    pub oracle_program: AccountInfo<'info>,
    #[account(mut, address = global_config.admin_authority)]
    pub admin: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

pub fn initialize_market(
    ctx: Context<InitializeMarket>,
    params: InitMarketParams,
) -> Result<()> {
    let m = &mut ctx.accounts.market;
    m.global_config = ctx.accounts.global_config.key();
    m.symbol = params.symbol;
    m.collateral_mint = ctx.accounts.collateral_mint.key();
    m.vault = ctx.accounts.vault.key();
    m.fee_vault = ctx.accounts.fee_vault.key();
    m.oracle_price_feed = ctx.accounts.oracle_price_feed.key();
    m.oracle_program_id = ctx.accounts.oracle_program.key();
    m.leverage_bps = params.leverage_bps;
    m.min_margin = params.min_margin;
    m.max_margin_per_position = params.max_margin_per_position;
    m.max_total_principal = params.max_total_principal;
    m.max_total_notional = params.max_total_notional;
    m.current_total_principal = 0;
    m.current_total_notional = 0;
    m.open_fee_bps = params.open_fee_bps;
    m.max_oracle_age_seconds = params.max_oracle_age_seconds;
    m.max_oracle_age_slots = params.max_oracle_age_slots;
    m.max_confidence_bps = params.max_confidence_bps;
    m.max_price_move_bps_per_sync = params.max_price_move_bps_per_sync;
    m.price_tick = params.price_tick;
    m.tick_aggregation_factor = params.tick_aggregation_factor;
    m.max_dormant_bucket_count_per_direction = params.max_dormant_bucket_count_per_direction;
    m.dilution_safety_bps = params.dilution_safety_bps;
    m.max_idle_slots = params.max_idle_slots;
    m.paused = false;
    m.frozen_new_position = false;
    m.schema_version = ctx.accounts.global_config.schema_version;
    m.sub_pool_count = params.sub_pool_count;
    require!(
        params.dormant_distribute_mode <= 1,
        crate::error::ProgramError::InvalidParameter
    );
    m.dormant_distribute_mode = params.dormant_distribute_mode;
    m.max_pending_apply_per_tx = params.max_pending_apply_per_tx;
    require!(
        params.max_distribution_ledger_size > 0,
        crate::error::ProgramError::InvalidParameter
    );
    m.max_distribution_ledger_size = params.max_distribution_ledger_size;
    m.bump = ctx.bumps.market;
    m._pad = [0u8; 2];
    Ok(())
}

#[derive(Accounts)]
#[instruction(sub_pool_id: u32)]
pub struct InitializeSubPool<'info> {
    pub market: Account<'info, Market>,
    #[account(
        init,
        payer = payer,
        space = SubPool::LEN,
        seeds = [b"sub_pool", market.key().as_ref(), &sub_pool_id.to_le_bytes()],
        bump,
    )]
    pub sub_pool: Account<'info, SubPool>,
    #[account(mut, address = market.global_config)]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

pub fn initialize_sub_pool(ctx: Context<InitializeSubPool>, sub_pool_id: u32) -> Result<()> {
    require!(sub_pool_id < ctx.accounts.market.sub_pool_count, crate::error::ProgramError::InvalidSubPool);
    let sp = &mut ctx.accounts.sub_pool;
    sp.market = ctx.accounts.market.key();
    sp.sub_pool_id = sub_pool_id;
    sp.long_pool_equity = 0;
    sp.short_pool_equity = 0;
    sp.long_active_shares = 0;
    sp.short_active_shares = 0;
    sp.long_recovery_shares = 0;
    sp.short_recovery_shares = 0;
    sp.long_active_notional = 0;
    sp.short_active_notional = 0;
    sp.long_active_generation = 0;
    sp.short_active_generation = 0;
    sp.last_price = 0;
    sp.last_sync_slot = 0;
    sp.long_dust = 0;
    sp.short_dust = 0;
    sp.long_dormant_bucket_count = 0;
    sp.short_dormant_bucket_count = 0;
    sp.bump = ctx.bumps.sub_pool;
    sp._pad = [0u8; 7];
    Ok(())
}

// ────────────────────────────────────────────────────────────────────
// Wave 6: dormant PDA materialisation
// ────────────────────────────────────────────────────────────────────

#[derive(Accounts)]
#[instruction(direction_is_long: bool)]
pub struct InitializeDistributionLedger<'info> {
    pub market: Account<'info, Market>,
    #[account(has_one = market)]
    pub sub_pool: Account<'info, SubPool>,
    /// Per-`(sub_pool, direction)` ledger PDA. Sized at the maximum
    /// the parent market admits via `max_distribution_ledger_size`.
    #[account(
        init,
        payer = payer,
        space = DistributionLedger::account_size(market.max_distribution_ledger_size),
        seeds = [
            b"dist_ledger",
            sub_pool.key().as_ref(),
            &[direction_is_long as u8],
        ],
        bump,
    )]
    pub ledger: Account<'info, DistributionLedger>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

pub fn initialize_distribution_ledger(
    ctx: Context<InitializeDistributionLedger>,
    direction_is_long: bool,
) -> Result<()> {
    let l = &mut ctx.accounts.ledger;
    l.sub_pool = ctx.accounts.sub_pool.key();
    l.direction_is_long = direction_is_long;
    l.max_entries = ctx.accounts.market.max_distribution_ledger_size;
    l.gc_offset = 0;
    l.next_event_index = 0;
    l.accrued_value_total = 0;
    l.pending_distribution_total = 0;
    l.entry_count = 0;
    l.entries = Vec::new();
    l.bump = ctx.bumps.ledger;
    l._pad = [0u8; 7];
    Ok(())
}

#[derive(Accounts)]
#[instruction(direction_is_long: bool, zero_price_tick: i64)]
pub struct InitializeDormantBucket<'info> {
    pub market: Account<'info, Market>,
    #[account(has_one = market)]
    pub sub_pool: Account<'info, SubPool>,
    /// Per-`(sub_pool, direction, zero_price_tick)` bucket PDA. We
    /// materialise it empty (zero shares, zero accrued); the engine
    /// then fills it via `insert_or_merge` on the next
    /// `close_position` / `force_close` that lands at this tick.
    #[account(
        init,
        payer = payer,
        space = DormantBucket::LEN,
        seeds = [
            b"dormant_bucket",
            sub_pool.key().as_ref(),
            &[direction_is_long as u8],
            &zero_price_tick.to_le_bytes(),
        ],
        bump,
    )]
    pub bucket: Account<'info, DormantBucket>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

pub fn initialize_dormant_bucket(
    ctx: Context<InitializeDormantBucket>,
    direction_is_long: bool,
    zero_price_tick: i64,
) -> Result<()> {
    let b = &mut ctx.accounts.bucket;
    b.sub_pool = ctx.accounts.sub_pool.key();
    b.direction_is_long = direction_is_long;
    b.zero_price_tick = zero_price_tick;
    b.anchor_price = 0;
    b.total_recovery_shares = 0;
    b.total_recovery_notional = 0;
    b.accrued_value = 0;
    b.position_count = 0;
    // The first time the engine touches this bucket, it will set
    // `last_applied_index` to the ledger's current `next_event_index`
    // (matching `DormantStore::insert_or_merge`'s semantics for newly-
    // created buckets — they "skip past" any pre-existing events
    // because they didn't exist at those events). The fully-zero
    // record makes the bridge layer's `record_is_dead` predicate
    // hold, so `unpack_direction` will skip this PDA until the engine
    // populates it.
    b.last_applied_index = 0;
    b.bump = ctx.bumps.bucket;
    b._pad = [0u8; 6];
    Ok(())
}

// ────────────────────────────────────────────────────────────────────
// Wave 7: dormant PDA reclamation
// ────────────────────────────────────────────────────────────────────

/// Close a dormant bucket PDA whose engine-side state has been zeroed
/// out (every recovery share burned, no pending claim left). Refunds
/// the rent lamports to `receiver`.
///
/// ### When the keeper calls this
///
/// `pack_direction` zeroes a bucket account whenever the engine
/// removes the matching `(sub_pool, direction, tick)` from its
/// `DormantStore` — typically because the last position holding that
/// bucket's recovery shares redeemed via `claim_dormant_recovery`. The
/// PDA stays around with all observables = 0 (matching
/// [`crate::instructions::dormant_bridge::record_is_dead`]). Without
/// this instruction, those PDAs accumulate indefinitely and bloat the
/// per-sub-pool PDA pool.
///
/// ### Safety
///
/// The Anchor `close = receiver` constraint atomically transfers the
/// account's lamports and marks the underlying buffer as closed. Two
/// engine-level guards re-check that the close is safe even if a
/// keeper raced with a concurrent `sync_pool` that re-allocated this
/// tick:
///
///   1. `record_is_dead(bucket)` must hold (no live shares / accrued /
///      positions). If the engine just wrote into this bucket, the
///      check fails and the tx reverts.
///   2. `bucket.last_applied_index >= ledger.next_event_index`
///      ensures no pending lazy-mode entries are still allocated to
///      this bucket. (Pack Pass 3 sets `last_applied_index =
///      ledger.next_event_index` whenever it zeroes a bucket, so this
///      naturally holds for engine-deleted buckets. The check
///      protects against a keeper closing a freshly-init'd PDA whose
///      `last_applied_index = 0` while the ledger has accumulated
///      entries in the meantime — without this check, a future
///      re-init at the same tick would re-create the bucket with
///      stale knowledge.)
///
/// Permissionless: any signer may call. Worst case for an adversarial
/// keeper is wasting their own CU budget on a tx that lands a no-op
/// or reverts.
#[derive(Accounts)]
pub struct CloseDormantBucket<'info> {
    pub market: Account<'info, Market>,
    #[account(has_one = market)]
    pub sub_pool: Account<'info, SubPool>,
    /// Per-`(sub_pool, direction)` distribution ledger. Read-only; we
    /// only check `next_event_index` for the second close-safety
    /// invariant.
    #[account(
        has_one = sub_pool,
        constraint = ledger.direction_is_long == bucket.direction_is_long
            @ crate::error::ProgramError::DormantBridgeAccountMismatch,
    )]
    pub ledger: Account<'info, DistributionLedger>,
    #[account(
        mut,
        has_one = sub_pool,
        constraint = bucket.total_recovery_shares == 0
            && bucket.total_recovery_notional == 0
            && bucket.accrued_value == 0
            && bucket.position_count == 0
            @ crate::error::ProgramError::DormantBucketStillLive,
        constraint = bucket.last_applied_index >= ledger.next_event_index
            @ crate::error::ProgramError::DormantBucketHasPendingApply,
        close = receiver,
    )]
    pub bucket: Account<'info, DormantBucket>,
    /// CHECK: lamport receiver. Permissionless on purpose — the
    /// keeper picks where to send the rent (typically their own
    /// account) since they pay the close-tx fee.
    #[account(mut)]
    pub receiver: AccountInfo<'info>,
    pub keeper: Signer<'info>,
}

pub fn close_dormant_bucket(_ctx: Context<CloseDormantBucket>) -> Result<()> {
    // All preconditions enforced by `#[account(constraint = ...)]`.
    // The `close = receiver` directive performs the actual transfer +
    // account close atomically.
    Ok(())
}
