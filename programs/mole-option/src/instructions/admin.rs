//! Wave 1+9 — protocol governance handlers.
//!
//! All instructions here mutate **policy fields** of `Market` /
//! `GlobalConfig` only — never user funds. They split into three
//! authority classes pinned by [`GlobalConfig`]:
//!
//! - `emergency_authority`: latency-critical halts. May toggle
//!   `paused` and `frozen_new_position` on a single `Market` without
//!   any extra confirmation. Designed for an OpsEC Squads multisig
//!   with low-N signing thresholds (e.g. 2-of-5) so on-call engineers
//!   can move within a block.
//! - `admin_authority`: routine governance. May bump
//!   `Market::schema_version` (always tied to a deployed migration
//!   path) and toggle `GlobalConfig::paused_globally` (a kill switch
//!   that ANDs with every per-market `paused`). Designed for the
//!   primary protocol Squads multisig with high-N thresholds (e.g.
//!   5-of-9) and a timelock on the multisig itself — this layer
//!   trusts the multisig to enforce the timelock.
//! - `upgrade_authority`: BPF upgrade signer. Not used by any
//!   instruction here; surfaced so off-chain tooling can validate the
//!   account list at deploy time.
//!
//! Every handler is intentionally a one-liner of policy mutation: the
//! work and risk live entirely in the `#[derive(Accounts)]` block,
//! which uses Anchor's `address = ...` pin to require the matching
//! authority pubkey to sign. Adding a new governance op is a single
//! [`AdminAccounts`] / [`EmergencyAccounts`] reuse plus a one-line
//! handler; never a new authority class.
//!
//! See `Docs/Planning/16-治理与升级.md` for the full multisig rollout
//! plan.

use anchor_lang::prelude::*;

use crate::error::ProgramError;
use crate::state::{GlobalConfig, Market};

// ====================================================================
// Account contexts
// ====================================================================

/// Account context for any operation gated by the **emergency**
/// authority. Used by `pause_market`, `resume_market`,
/// `freeze_new_position`, `unfreeze_new_position`.
///
/// Wave 1 named this struct `PauseMarket`; we keep that exact name
/// because Anchor's `#[program]` macro derives a sibling
/// `__client_accounts_pause_market` module from it, and renaming
/// would silently break already-deployed clients. New emergency-
/// gated handlers should reuse this same struct rather than mint a
/// parallel one — the macro-generated client code is the
/// authoritative interface boundary.
#[derive(Accounts)]
pub struct PauseMarket<'info> {
    #[account(mut)]
    pub market: Account<'info, Market>,
    #[account(address = market.global_config)]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(address = global_config.emergency_authority)]
    pub authority: Signer<'info>,
}

/// Account context for any operation gated by the **admin** (routine-
/// governance, multi-sig + timelock) authority on a single market.
#[derive(Accounts)]
pub struct AdminMarketAccounts<'info> {
    #[account(mut)]
    pub market: Account<'info, Market>,
    #[account(address = market.global_config)]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(address = global_config.admin_authority)]
    pub authority: Signer<'info>,
}

/// Account context for any operation gated by the **admin** authority
/// on the global config (cross-market kill switch, schema epoch
/// bumps that span every market under this `GlobalConfig`).
#[derive(Accounts)]
pub struct AdminGlobalAccounts<'info> {
    #[account(mut)]
    pub global_config: Account<'info, GlobalConfig>,
    #[account(address = global_config.admin_authority)]
    pub authority: Signer<'info>,
}

// ====================================================================
// Emergency handlers
// ====================================================================

/// Halt all funds-touching instructions on this market. Mirrors
/// `clearing_core::MarketParams::paused = true`.
pub fn pause_market(ctx: Context<PauseMarket>) -> Result<()> {
    ctx.accounts.market.paused = true;
    Ok(())
}

/// Resume all funds-touching instructions on this market.
pub fn resume_market(ctx: Context<PauseMarket>) -> Result<()> {
    ctx.accounts.market.paused = false;
    Ok(())
}

/// Block new opens but allow existing positions to close. Used for
/// graceful market deprecation.
pub fn freeze_new_position(ctx: Context<PauseMarket>) -> Result<()> {
    ctx.accounts.market.frozen_new_position = true;
    Ok(())
}

/// Wave 9 — counterpart to `freeze_new_position`. Re-enable opens
/// after a paused window has cleared. Without this, a frozen market
/// would be a one-way trip; emergency authority should always be able
/// to walk back its own decision.
pub fn unfreeze_new_position(ctx: Context<PauseMarket>) -> Result<()> {
    ctx.accounts.market.frozen_new_position = false;
    Ok(())
}

// ====================================================================
// Admin handlers (Wave 9)
// ====================================================================

/// Toggle the global kill switch. While
/// `GlobalConfig::paused_globally == true`, every market's
/// `Market::paused` field is treated as `true` even if the per-market
/// flag is unset; this lets the admin authority halt the entire
/// protocol with one transaction during a multi-market exploit.
///
/// Wave 9 contract: the field is set on `GlobalConfig`; per-market
/// `Market::paused` is left untouched. Engine handlers (Wave 8 audit)
/// already short-circuit on `market.paused`, so the actual gating
/// happens in a follow-up patch that ORs the two flags inside
/// `clearing_view`.
pub fn set_globally_paused(ctx: Context<AdminGlobalAccounts>, paused: bool) -> Result<()> {
    ctx.accounts.global_config.paused_globally = paused;
    Ok(())
}

/// Bump `Market::schema_version` to a strictly greater value. Routes
/// through the admin authority because every bump implies a deployed
/// migration path (`migrate_position` / `migrate_market`) has shipped
/// and the multisig's timelock has elapsed.
///
/// Errors:
/// - [`ProgramError::SchemaBumpMustIncrease`] if `new_version <=
///   market.schema_version`.
///
/// Wave 9 ships this with `SCHEMA_VERSION_CURRENT == 1`, so any bump
/// will fail until the migration crate registers a `1 → 2` path —
/// matching the sequencing requirement in §16. The instruction is
/// nonetheless deployed today so the multisig has the call-site
/// available without a program upgrade.
pub fn bump_market_schema_version(
    ctx: Context<AdminMarketAccounts>,
    new_version: u16,
) -> Result<()> {
    let market = &mut ctx.accounts.market;
    if new_version <= market.schema_version {
        return Err(ProgramError::SchemaBumpMustIncrease.into());
    }
    market.schema_version = new_version;
    Ok(())
}
