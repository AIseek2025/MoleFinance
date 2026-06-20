//! `open_position` instruction handler.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};

use clearing_core::Direction;

use crate::error::map_err;
use crate::state::{Market, Position, SubPool as SubPoolAccount};

use super::sync::{apply_clearing_view, clearing_view, market_params_from};
use super::PriceEnvelopeArgs;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct OpenParams {
    pub envelope: PriceEnvelopeArgs,
    pub direction_is_long: bool,
    pub gross_amount: u64,
    pub position_id: u64,
}

#[derive(Accounts)]
#[instruction(params: OpenParams)]
pub struct OpenPosition<'info> {
    // Heavy account-data structs are `Box`ed so Anchor's generated
    // `try_accounts` deserialises them onto the heap instead of the
    // 4 KB BPF stack frame (otherwise this instruction overflows the
    // stack — undefined behaviour on chain).
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut, has_one = market)]
    pub sub_pool: Box<Account<'info, SubPoolAccount>>,
    #[account(
        init,
        payer = owner,
        space = Position::LEN,
        seeds = [
            b"position",
            market.key().as_ref(),
            owner.key().as_ref(),
            &params.position_id.to_le_bytes(),
        ],
        bump,
    )]
    pub position: Box<Account<'info, Position>>,

    /// CHECK: PDA validated by has_one against `market`.
    #[account(mut, address = market.vault)]
    pub vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: PDA validated against `market.fee_vault`.
    #[account(mut, address = market.fee_vault)]
    pub fee_vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = market.collateral_mint, token::authority = owner)]
    pub user_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub owner: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn open_position(ctx: Context<OpenPosition>, params: OpenParams) -> Result<()> {
    let market = &ctx.accounts.market;
    let market_params = market_params_from(market);
    let mut sp_view = clearing_view(&ctx.accounts.sub_pool);

    let direction = if params.direction_is_long {
        Direction::Long
    } else {
        Direction::Short
    };

    let (pos, outcome) = clearing_core::open_position(
        &market_params,
        &mut sp_view,
        params.envelope.into(),
        direction,
        params.gross_amount,
        params.position_id,
    )
    .map_err(map_err)?;

    // SPL transfer of principal into vault, fee into fee_vault.
    if outcome.principal_into_pool > 0 {
        let cpi = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.user_token_account.to_account_info(),
                to: ctx.accounts.vault.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        );
        token::transfer(cpi, pos.principal)?;
    }
    if outcome.open_fee > 0 {
        let cpi = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.user_token_account.to_account_info(),
                to: ctx.accounts.fee_vault.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        );
        token::transfer(cpi, outcome.open_fee)?;
    }

    apply_clearing_view(&mut ctx.accounts.sub_pool, &sp_view);

    let p_acc = &mut ctx.accounts.position;
    p_acc.owner = ctx.accounts.owner.key();
    p_acc.market = market.key();
    p_acc.sub_pool = ctx.accounts.sub_pool.key();
    p_acc.position_id = params.position_id;
    p_acc.direction_is_long = params.direction_is_long;
    p_acc.status = 0;
    p_acc.principal = pos.principal;
    p_acc.leverage_bps = market.leverage_bps;
    p_acc.notional = pos.notional;
    p_acc.active_shares = pos.active_shares;
    p_acc.recovery_shares = 0;
    p_acc.recovery_bucket_tick = 0;
    p_acc.has_recovery_bucket = false;
    p_acc.zero_price = 0;
    p_acc.entry_price = pos.entry_price;
    p_acc.last_sync_slot = pos.last_sync_slot;
    p_acc.active_generation = pos.active_generation;
    let now = Clock::get()?.unix_timestamp;
    p_acc.opened_at = now;
    p_acc.updated_at = now;
    p_acc.closed_at = 0;
    p_acc.schema_version = market.schema_version;
    p_acc.bump = ctx.bumps.position;
    p_acc._pad = [0u8; 5];
    Ok(())
}
