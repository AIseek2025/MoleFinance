//! `harvest_dust` handler.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};

use clearing_core::Direction;

use crate::error::map_err;
use crate::state::{GlobalConfig, Market, SubPool as SubPoolAccount};

use super::sync::{apply_clearing_view, clearing_view, market_params_from};

#[derive(Accounts)]
pub struct HarvestDust<'info> {
    #[account(mut)]
    pub market: Account<'info, Market>,
    #[account(mut, has_one = market)]
    pub sub_pool: Account<'info, SubPoolAccount>,
    #[account(mut, address = market.vault)]
    pub vault: Account<'info, TokenAccount>,
    #[account(mut, address = market.fee_vault)]
    pub fee_vault: Account<'info, TokenAccount>,
    /// CHECK: signer for vault PDA.
    #[account(seeds = [b"market_vault_authority", market.key().as_ref()], bump)]
    pub vault_authority: AccountInfo<'info>,
    #[account(address = market.global_config)]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(address = global_config.protocol_treasury)]
    pub protocol_treasury_signer: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

pub fn harvest_dust(ctx: Context<HarvestDust>, direction_is_long: bool) -> Result<()> {
    let market = &ctx.accounts.market;
    let market_params = market_params_from(market);
    let mut sp_view = clearing_view(&ctx.accounts.sub_pool);

    let outcome = clearing_core::harvest_dust(
        &market_params,
        &mut sp_view,
        if direction_is_long {
            Direction::Long
        } else {
            Direction::Short
        },
    )
    .map_err(map_err)?;

    apply_clearing_view(&mut ctx.accounts.sub_pool, &sp_view);

    let amount_u64 =
        u64::try_from(outcome.amount).map_err(|_| crate::error::ProgramError::MathOverflow)?;
    if amount_u64 > 0 {
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
                to: ctx.accounts.fee_vault.to_account_info(),
                authority: ctx.accounts.vault_authority.to_account_info(),
            },
            signer_seeds,
        );
        token::transfer(cpi, amount_u64)?;
    }
    Ok(())
}
