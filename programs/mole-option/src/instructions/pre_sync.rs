//! `pre_sync_dormant_bucket` instruction handler.
//!
//! In **lazy distribute mode** (`MarketParams.dormant_distribute_mode
//! == Lazy`), `sync_pool` only appends a `DistributionEntry` to the
//! ledger ring; it does NOT update `bucket.accrued_value`. A keeper
//! must subsequently call `pre_sync_dormant_bucket` to drain the
//! pending entries onto a specific bucket. This is the per-bucket
//! "catch-up" entrypoint that the engine surfaces to keepers.
//!
//! ### Why this isn't a single-side / single-bucket account contract
//!
//! The engine's `pre_sync_dormant_bucket` runs
//! [`clearing_core::invariants::check_subpool_invariants`] at the
//! end, which checks the **cross-direction** invariant
//! `Σ recovery_shares == sub_pool.<dir>_recovery_shares` for BOTH
//! sides. So even though the mutation only touches one bucket on one
//! side, the read scope is the full `SubPool.long_dormant` and
//! `short_dormant`. The bridge therefore unpacks both directions
//! (just like [`super::sync::sync_pool`]); only the *write-back* is
//! single-bucket.
//!
//! ### `remaining_accounts` contract
//!
//! ```text
//!   [0 .. L]      DormantBucket Long  (mut)  — every live long bucket
//!   [L .. L+S]    DormantBucket Short (mut)  — every live short bucket
//! ```
//!
//! The chosen `(direction, bucket_tick)` MUST appear in the
//! corresponding side's slice; otherwise unpack will see the bucket
//! as missing and return `DormantBucketMissing`.
//!
//! ### Differences vs `sync_pool`
//!
//! * No oracle validation (lazy keepers don't need a fresh Pyth
//!   price; they only drain the ledger).
//! * No `envelope` argument; the engine call uses the pool's last
//!   price implicitly via `apply_pending_to_bucket`, which only adds
//!   ledger-recorded value to the selected bucket.
//! * No price-move bound check.
//!
//! ### Failure modes
//!
//! * `DormantBucketMissing` — the requested bucket isn't loaded /
//!   doesn't exist.
//! * `DormantPendingBudgetExceeded` — too many pending entries for
//!   one tx (compute-budget protection); split across multiple calls.
//! * `Invariant` — invariant check failed; tx atomically reverts so
//!   the caller can re-attempt with a corrected account list.

use anchor_lang::prelude::*;

use clearing_core::Direction;

use crate::error::{map_err, ProgramError};
use crate::state::{DistributionLedger, Market, SubPool as SubPoolAccount};

use super::dormant_bridge::run_bridged;
use super::sync::market_params_from;

#[derive(Accounts)]
pub struct PreSyncDormantBucket<'info> {
    pub market: Account<'info, Market>,
    #[account(mut, has_one = market)]
    pub sub_pool: Account<'info, SubPoolAccount>,
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
    pub clock: Sysvar<'info, Clock>,
    /// Permissionless: any signer may pay rent for the catch-up tx.
    /// This is intentional — keepers shouldn't need a privileged role
    /// to drain ledger entries. The engine enforces correctness; the
    /// only thing a malicious keeper can achieve is wasting their own
    /// CU budget on a tx that lands a no-op.
    pub keeper: Signer<'info>,
}

pub fn pre_sync_dormant_bucket<'info>(
    ctx: Context<'_, '_, 'info, 'info, PreSyncDormantBucket<'info>>,
    direction_is_long: bool,
    bucket_tick: i64,
    long_bucket_count: u32,
    short_bucket_count: u32,
) -> Result<()> {
    let market_acc = &ctx.accounts.market;
    let market_params = market_params_from(market_acc);
    let direction = if direction_is_long {
        Direction::Long
    } else {
        Direction::Short
    };
    let slot = ctx.accounts.clock.slot;

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
            clearing_core::pre_sync_dormant_bucket(params, sp, direction, bucket_tick, slot)
                .map(|_| ())
                .map_err(map_err)
        },
    )
}
