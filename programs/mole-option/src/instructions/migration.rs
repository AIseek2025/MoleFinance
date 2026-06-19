//! Wave 9 — schema migration instructions.
//!
//! Wave 8 sealed the *protocol-side* reject matrix: any
//! [`clearing_core::MarketParams`] / [`clearing_core::Position`] whose
//! `schema_version` does not equal
//! [`clearing_core::SCHEMA_VERSION_CURRENT`] is refused entry to every
//! funds-touching engine entrypoint with `SchemaVersionMismatch`.
//! Wave 9 ships the *forward-side* of the same gate: instructions
//! that walk an old-schema account up to the current epoch one step
//! at a time.
//!
//! ## Why two separate instructions
//!
//! `Market` and `Position` have independent lifecycle owners:
//!
//! - `Market` is bumped exactly once per `bump_market_schema_version`
//!   call by the admin multisig. The migration is per-market and
//!   must happen *before* any user instruction touches it under the
//!   new epoch.
//! - `Position` is migrated by its owner (or any keeper) on the next
//!   user-driven entry the position takes part in. We don't force
//!   batch migration: the `SchemaVersionMismatch` reject acts as a
//!   permission-less gate that pushes the migration onto the user's
//!   own gas budget.
//!
//! ## Migration registry
//!
//! [`SchemaMigrationStep`] enumerates every supported (from, to)
//! pair. In Wave 9, the registry is empty (`SCHEMA_VERSION_CURRENT
//! == 1`, no 0 → 1 migration was ever needed because v1 is the
//! launch epoch). A future bump (1 → 2, say) will:
//!
//! 1. Add `SchemaMigrationStep::V1ToV2` to the enum.
//! 2. Implement the in-place data shape change inside
//!    `apply_to_position` / `apply_to_market`.
//! 3. Bump `SCHEMA_VERSION_CURRENT` in `clearing-core`.
//! 4. The admin multisig calls `bump_market_schema_version(2)` AFTER
//!    the migration code is deployed; from that point on, every old-
//!    schema position MUST migrate before any close/claim succeeds.
//!
//! ## Why we keep this as a separate module
//!
//! Bundling the migration logic inside `engine.rs` would make every
//! engine call carry version-dispatch overhead. Keeping it
//! out-of-band lets the engine assume a single shape (the current
//! one) and lets migrations be deployed independently of engine
//! upgrades.

use anchor_lang::prelude::*;

use clearing_core::SCHEMA_VERSION_CURRENT;

use crate::error::ProgramError;
use crate::state::{Market, Position};

// ====================================================================
// Migration registry
// ====================================================================

/// All registered migration steps. New variants are added in
/// strictly-increasing `(from, to)` order each time
/// `clearing_core::SCHEMA_VERSION_CURRENT` is bumped.
///
/// Wave 9: empty (current version is launch epoch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaMigrationStep {
    // No registered steps as of Wave 9. Future:
    //   V1ToV2,
    //   V2ToV3,
    //   ...
}

impl SchemaMigrationStep {
    /// Resolve the migration step that brings `from` to `from + 1`,
    /// or `None` if no such step is registered.
    pub const fn from_source(_from: u16) -> Option<Self> {
        // Empty registry — every call returns None until a v2 bump.
        None
    }

    /// Apply this step to a position — in-place data-shape upgrade.
    /// Wave 9 has no concrete steps so this matches the empty enum.
    pub fn apply_to_position(self, _position: &mut Position) {
        match self {} // unreachable on the empty enum
    }

    /// Apply this step to a market — in-place data-shape upgrade.
    pub fn apply_to_market(self, _market: &mut Market) {
        match self {} // unreachable on the empty enum
    }
}

// ====================================================================
// `migrate_position`
// ====================================================================

/// Account context for [`migrate_position`].
#[derive(Accounts)]
pub struct MigratePosition<'info> {
    #[account(mut)]
    pub position: Account<'info, Position>,
    /// The market the position was opened against. We do NOT take
    /// `&mut` since position migration must not depend on market
    /// state — it's a pure structural rewrite.
    pub market: Account<'info, Market>,
    /// Caller pays the (small) compute fee. Permissionless: anyone
    /// (the owner, a keeper, even a third-party indexer) can drive a
    /// position to the current epoch.
    pub payer: Signer<'info>,
}

/// Walk a position's `schema_version` up to
/// `SCHEMA_VERSION_CURRENT` by applying each registered migration
/// step in order.
///
/// Errors:
/// - [`ProgramError::SchemaMigrationNoop`] if `position.schema_version
///   == SCHEMA_VERSION_CURRENT` (caller should have skipped the
///   call).
/// - [`ProgramError::SchemaMigrationPathMissing`] if no step is
///   registered for some intermediate version. Cannot happen in Wave
///   9 because the only path through the loop is the noop guard.
pub fn migrate_position(ctx: Context<MigratePosition>) -> Result<()> {
    let position = &mut ctx.accounts.position;
    let target = SCHEMA_VERSION_CURRENT;
    if position.schema_version == target {
        return Err(ProgramError::SchemaMigrationNoop.into());
    }
    while position.schema_version < target {
        let step = SchemaMigrationStep::from_source(position.schema_version)
            .ok_or(ProgramError::SchemaMigrationPathMissing)?;
        step.apply_to_position(position);
        position.schema_version = position
            .schema_version
            .checked_add(1)
            .ok_or(ProgramError::MathOverflow)?;
    }
    Ok(())
}

// ====================================================================
// `migrate_market`
// ====================================================================

/// Account context for [`migrate_market`]. The market is the
/// migration target; the admin authority signs.
#[derive(Accounts)]
pub struct MigrateMarket<'info> {
    #[account(mut)]
    pub market: Account<'info, Market>,
    #[account(address = market.global_config)]
    pub global_config: Account<'info, crate::state::GlobalConfig>,
    /// Admin authority: same multisig that gates
    /// [`super::admin::bump_market_schema_version`]. We require it
    /// here so the migration tx is part of the same governance
    /// timelock as the version bump.
    #[account(address = global_config.admin_authority)]
    pub authority: Signer<'info>,
}

/// Walk a market's `schema_version` up to `SCHEMA_VERSION_CURRENT`.
/// Identical contract to [`migrate_position`] but for markets — see
/// that function's doc-comment for error semantics.
///
/// Sequencing rule (enforced off-chain by governance scripts, not on-
/// chain): `migrate_market` runs *before* the matching
/// `bump_market_schema_version` call. This way the engine never sees
/// a market whose data is at version N but whose `schema_version`
/// header is N+1 — that combination would simulataneously satisfy
/// `assert_schema_version` AND read mis-laid-out data.
pub fn migrate_market(ctx: Context<MigrateMarket>) -> Result<()> {
    let market = &mut ctx.accounts.market;
    let target = SCHEMA_VERSION_CURRENT;
    if market.schema_version == target {
        return Err(ProgramError::SchemaMigrationNoop.into());
    }
    while market.schema_version < target {
        let step = SchemaMigrationStep::from_source(market.schema_version)
            .ok_or(ProgramError::SchemaMigrationPathMissing)?;
        step.apply_to_market(market);
        market.schema_version = market
            .schema_version
            .checked_add(1)
            .ok_or(ProgramError::MathOverflow)?;
    }
    Ok(())
}
