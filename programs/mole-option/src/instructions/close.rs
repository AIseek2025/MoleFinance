//! `close_position` and `force_close_zero_value_position` handlers.
//!
//! Both internally call `clearing_core::sync_pool`, which can rotate /
//! redeem dormant buckets and append distribution-ledger entries.
//! That means both handlers MUST take the full Wave 6 dormant-bridge
//! account contract (long ledger + short ledger + every live bucket
//! PDA), exactly like [`super::sync::sync_pool`].
//!
//! ### `remaining_accounts` contract
//!
//! Identical to `sync_pool`:
//! ```text
//!   [0 .. L]      DormantBucket Long  (mut)
//!   [L .. L+S]    DormantBucket Short (mut)
//! ```
//!
//! `L = long_bucket_count`, `S = short_bucket_count` are passed as
//! instruction arguments. Missing a single live bucket = silent
//! distribute skip + tripped invariant on the next unpack.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};

use clearing_core::{Direction, Position as CorePosition, PositionStatus};

use crate::error::{map_err, ProgramError};
use crate::state::{DistributionLedger, Market, Position, SubPool as SubPoolAccount};

use super::dormant_bridge::run_bridged;
use super::sync::market_params_from;
use super::{validate_oracle_envelope, PriceEnvelopeArgs};

#[derive(Accounts)]
pub struct ClosePosition<'info> {
    #[account(mut)]
    pub market: Account<'info, Market>,
    #[account(mut, has_one = market)]
    pub sub_pool: Account<'info, SubPoolAccount>,
    #[account(
        mut,
        has_one = owner,
        has_one = market,
        constraint = position.sub_pool == sub_pool.key(),
    )]
    pub position: Account<'info, Position>,
    #[account(
        mut,
        has_one = sub_pool,
        constraint = long_ledger.direction_is_long
            @ ProgramError::DormantBridgeAccountMismatch,
    )]
    pub long_ledger: Account<'info, DistributionLedger>,
    #[account(
        mut,
        has_one = sub_pool,
        constraint = !short_ledger.direction_is_long
            @ ProgramError::DormantBridgeAccountMismatch,
    )]
    pub short_ledger: Account<'info, DistributionLedger>,
    /// CHECK: oracle feed; validated against `market.oracle_price_feed`
    /// and `market.oracle_program_id` inside the handler.
    #[account(address = market.oracle_price_feed)]
    pub oracle_price_feed: AccountInfo<'info>,
    pub clock: Sysvar<'info, Clock>,
    #[account(mut, address = market.vault)]
    pub vault: Account<'info, TokenAccount>,
    #[account(mut, token::mint = market.collateral_mint, token::authority = owner)]
    pub user_token_account: Account<'info, TokenAccount>,
    /// CHECK: market vault PDA signer.
    #[account(seeds = [b"market_vault_authority", market.key().as_ref()], bump)]
    pub vault_authority: AccountInfo<'info>,
    pub owner: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

pub fn close_position<'info>(
    ctx: Context<'_, '_, 'info, 'info, ClosePosition<'info>>,
    mut envelope: PriceEnvelopeArgs,
    long_bucket_count: u32,
    short_bucket_count: u32,
) -> Result<()> {
    let market = &ctx.accounts.market;
    let market_params = market_params_from(market);

    validate_oracle_envelope(
        market,
        &ctx.accounts.oracle_price_feed,
        ctx.accounts.clock.slot,
        &mut envelope,
    )?;

    let mut core_pos = position_to_core(&ctx.accounts.position);
    let outcome = run_bridged(
        &mut ctx.accounts.sub_pool,
        &mut ctx.accounts.long_ledger,
        &mut ctx.accounts.short_ledger,
        &market_params,
        ctx.program_id,
        ctx.remaining_accounts,
        long_bucket_count,
        short_bucket_count,
        |params, sp| {
            clearing_core::close_position(params, sp, envelope.into(), &mut core_pos)
                .map_err(map_err)
        },
    )?;
    write_back_position(&mut ctx.accounts.position, &core_pos);

    let amount = u64::try_from(outcome.withdrawable).map_err(|_| ProgramError::MathOverflow)?;
    if amount > 0 {
        let market_key = market.key();
        let bump = ctx.bumps.vault_authority;
        let signer_seeds: &[&[&[u8]]] = &[&[
            b"market_vault_authority",
            market_key.as_ref(),
            &[bump],
        ]];
        let cpi = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.vault.to_account_info(),
                to: ctx.accounts.user_token_account.to_account_info(),
                authority: ctx.accounts.vault_authority.to_account_info(),
            },
            signer_seeds,
        );
        token::transfer(cpi, amount)?;
    }
    Ok(())
}

#[derive(Accounts)]
pub struct ForceClosePosition<'info> {
    #[account(mut)]
    pub market: Account<'info, Market>,
    #[account(mut, has_one = market)]
    pub sub_pool: Account<'info, SubPoolAccount>,
    #[account(
        mut,
        has_one = owner,
        has_one = market,
        constraint = position.sub_pool == sub_pool.key(),
    )]
    pub position: Account<'info, Position>,
    #[account(
        mut,
        has_one = sub_pool,
        constraint = long_ledger.direction_is_long
            @ ProgramError::DormantBridgeAccountMismatch,
    )]
    pub long_ledger: Account<'info, DistributionLedger>,
    #[account(
        mut,
        has_one = sub_pool,
        constraint = !short_ledger.direction_is_long
            @ ProgramError::DormantBridgeAccountMismatch,
    )]
    pub short_ledger: Account<'info, DistributionLedger>,
    /// CHECK: oracle feed; validated against `market.oracle_price_feed`
    /// and `market.oracle_program_id` inside the handler.
    #[account(address = market.oracle_price_feed)]
    pub oracle_price_feed: AccountInfo<'info>,
    pub clock: Sysvar<'info, Clock>,
    pub owner: Signer<'info>,
}

pub fn force_close_zero_value_position<'info>(
    ctx: Context<'_, '_, 'info, 'info, ForceClosePosition<'info>>,
    mut envelope: PriceEnvelopeArgs,
    acknowledge_forfeit: bool,
    long_bucket_count: u32,
    short_bucket_count: u32,
) -> Result<()> {
    let market = &ctx.accounts.market;
    let market_params = market_params_from(market);

    validate_oracle_envelope(
        market,
        &ctx.accounts.oracle_price_feed,
        ctx.accounts.clock.slot,
        &mut envelope,
    )?;

    let mut core_pos = position_to_core(&ctx.accounts.position);
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
            clearing_core::force_close_zero_value_position(
                params,
                sp,
                envelope.into(),
                &mut core_pos,
                acknowledge_forfeit,
            )
            .map(|_| ())
            .map_err(map_err)
        },
    )?;
    write_back_position(&mut ctx.accounts.position, &core_pos);
    Ok(())
}

pub(crate) fn position_to_core(p: &Position) -> CorePosition {
    let mut owner = [0u8; 32];
    owner.copy_from_slice(p.owner.as_ref());
    CorePosition {
        owner,
        sub_pool_id: 0, // sub_pool is identified by Pubkey on chain; the
        // engine doesn't use sub_pool_id here.
        position_id: p.position_id,
        direction: if p.direction_is_long {
            Direction::Long
        } else {
            Direction::Short
        },
        status: match p.status {
            0 => PositionStatus::Open,
            1 => PositionStatus::Dormant,
            _ => PositionStatus::Closed,
        },
        principal: p.principal,
        notional: p.notional,
        active_shares: p.active_shares,
        recovery_shares: p.recovery_shares,
        recovery_bucket_tick: if p.has_recovery_bucket {
            Some(p.recovery_bucket_tick)
        } else {
            None
        },
        zero_price: p.zero_price,
        entry_price: p.entry_price,
        last_sync_slot: p.last_sync_slot,
        opened_at_slot: 0,
        updated_at_slot: 0,
        closed_at_slot: 0,
        schema_version: p.schema_version,
        active_generation: p.active_generation,
    }
}

pub(crate) fn write_back_position(p: &mut Position, core: &CorePosition) {
    p.principal = core.principal;
    p.notional = core.notional;
    p.active_shares = core.active_shares;
    p.recovery_shares = core.recovery_shares;
    if let Some(tick) = core.recovery_bucket_tick {
        p.recovery_bucket_tick = tick;
        p.has_recovery_bucket = true;
    } else {
        p.recovery_bucket_tick = 0;
        p.has_recovery_bucket = false;
    }
    p.zero_price = core.zero_price;
    p.last_sync_slot = core.last_sync_slot;
    p.active_generation = core.active_generation;
    p.status = match core.status {
        PositionStatus::Open => 0,
        PositionStatus::Dormant => 1,
        PositionStatus::Closed => 2,
    };
    let now = Clock::get().map(|c| c.unix_timestamp).unwrap_or(0);
    p.updated_at = now;
    if core.status == PositionStatus::Closed {
        p.closed_at = now;
    }
}
