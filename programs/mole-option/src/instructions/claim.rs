//! `claim_dormant_recovery` handler.
//!
//! Internally calls `clearing_core::sync_pool` first, so the full
//! Wave 6 dormant-bridge contract applies (long_ledger plus
//! short_ledger plus every live bucket of both sides). After
//! redemption the position transfers `outcome.redeemable` from the
//! protocol vault to the caller's token account.
//!
//! ### `remaining_accounts` contract
//!
//! Identical to [`super::sync::sync_pool`] /
//! [`super::close::close_position`]:
//! ```text
//!   [0 .. L]      DormantBucket Long  (mut)
//!   [L .. L+S]    DormantBucket Short (mut)
//! ```

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};

use crate::error::{map_err, ProgramError};
use crate::state::{DistributionLedger, Market, Position, SubPool as SubPoolAccount};

use super::close::{position_to_core, write_back_position};
use super::dormant_bridge::run_bridged;
use super::sync::market_params_from;
use super::{validate_oracle_envelope, PriceEnvelopeArgs};

#[derive(Accounts)]
pub struct ClaimDormantRecovery<'info> {
    // Heavy account-data structs are `Box`ed so Anchor's generated
    // `try_accounts` deserialises them onto the heap instead of the
    // 4 KB BPF stack frame (otherwise this instruction overflows the
    // stack — undefined behaviour on chain).
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut, has_one = market)]
    pub sub_pool: Box<Account<'info, SubPoolAccount>>,
    #[account(
        mut,
        has_one = owner,
        has_one = market,
        constraint = position.sub_pool == sub_pool.key(),
    )]
    pub position: Box<Account<'info, Position>>,
    #[account(
        mut,
        has_one = sub_pool,
        constraint = long_ledger.direction_is_long
            @ ProgramError::DormantBridgeAccountMismatch,
    )]
    pub long_ledger: Box<Account<'info, DistributionLedger>>,
    #[account(
        mut,
        has_one = sub_pool,
        constraint = !short_ledger.direction_is_long
            @ ProgramError::DormantBridgeAccountMismatch,
    )]
    pub short_ledger: Box<Account<'info, DistributionLedger>>,
    /// CHECK: oracle feed; validated against `market.oracle_price_feed`
    /// and `market.oracle_program_id` inside the handler.
    #[account(address = market.oracle_price_feed)]
    pub oracle_price_feed: AccountInfo<'info>,
    pub clock: Sysvar<'info, Clock>,
    #[account(mut, address = market.vault)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = market.collateral_mint, token::authority = owner)]
    pub user_token_account: Box<Account<'info, TokenAccount>>,
    /// CHECK: market vault PDA signer.
    #[account(seeds = [b"market_vault_authority", market.key().as_ref()], bump)]
    pub vault_authority: AccountInfo<'info>,
    pub owner: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

pub fn claim_dormant_recovery<'info>(
    ctx: Context<'_, '_, 'info, 'info, ClaimDormantRecovery<'info>>,
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
            clearing_core::claim_dormant_recovery(params, sp, envelope.into(), &mut core_pos)
                .map_err(map_err)
        },
    )?;
    write_back_position(&mut ctx.accounts.position, &core_pos);

    let amount = u64::try_from(outcome.redeemable).map_err(|_| ProgramError::MathOverflow)?;
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
