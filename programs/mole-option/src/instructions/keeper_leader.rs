//! Wave 15 — Keeper-leader-lock instruction handlers.
//!
//! Three instructions live here, all permissionless:
//!
//! - [`keeper_leader_acquire`] — strict claim-stale path. Rejects if
//!   the lock is currently fresh (`takeover_threshold_slots` not yet
//!   elapsed). Used by an HA keeper replica that wants to take over
//!   when the active leader has gone silent.
//! - [`keeper_leader_heartbeat`] — all-paths heartbeat. Acquires fresh,
//!   acquires from stale, refreshes self, OR rejects with the right
//!   reason. The keeper bot's normal tick path drives this.
//! - [`keeper_leader_release`] — graceful shutdown path. Holder calls
//!   this so a standby replica doesn't have to wait
//!   `takeover_threshold_slots` slots to claim leadership.
//!
//! ## Invariants enforced on chain
//!
//! 1. **Strict slot monotonicity.** `args.observed_slot` MUST be
//!    `>= lock.last_heartbeat_slot`; otherwise the handler rejects
//!    with `KeeperLeaderClockSkew`. The host-side state machine in
//!    `keeper_decoder::leader_lock::KeeperLeaderLock::try_heartbeat`
//!    enforces the same predicate; the Anchor handler is just
//!    another physical realisation.
//! 2. **Fresh-lock owner exclusivity.** A non-leader cannot acquire
//!    while the lock is fresh. Surfaced as `KeeperLeaderHeldByOther`.
//! 3. **Caller-passed slot is bounded by `Clock`.** We additionally
//!    require `args.observed_slot <= Clock::get()?.slot`; a keeper
//!    cannot stamp a future slot to extend its leadership ahead of
//!    real time. This complements the host-side check (which
//!    operates with whatever `current_slot` the bot fed in).
//! 4. **Single-writer determinism.** `Clock::slot` is the on-chain
//!    "current slot"; the lock then uses `args.observed_slot` for
//!    its in-account stamp (so the keeper can deterministically
//!    pin its own clock without racing the validator). The wave-16
//!    `solana-program-test` matrix exercises both paths.
//!
//! ## PDA
//!
//! `seeds = [b"keeper_leader_lock", market.key().as_ref()]`. Each
//! market gets its own lock — keeper failures in one market don't
//! starve another. The `market` account is `mut = false`; we read
//! it only to scope the lock.

use anchor_lang::prelude::*;

use crate::error::ProgramError;
use crate::state::{KeeperLeaderLock, Market};

/// Caller-passed args for `keeper_leader_acquire` /
/// `keeper_leader_heartbeat`. Must mirror
/// `keeper_decoder::ix::KeeperLeaderHeartbeatArgs`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct KeeperLeaderHeartbeatArgs {
    /// Slot the keeper observed off-chain when it constructed the
    /// transaction. The handler asserts this is `>=
    /// lock.last_heartbeat_slot` AND `<= Clock::slot`.
    pub observed_slot: u64,
}

#[derive(Accounts)]
pub struct InitializeKeeperLeaderLock<'info> {
    /// The market this lock belongs to. Read-only — we just hash it
    /// into the PDA seed.
    pub market: Account<'info, Market>,
    #[account(
        init,
        payer = payer,
        space = KeeperLeaderLock::LEN,
        seeds = [b"keeper_leader_lock", market.key().as_ref()],
        bump,
    )]
    pub keeper_leader_lock: Account<'info, KeeperLeaderLock>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct KeeperLeaderHeartbeat<'info> {
    pub market: Account<'info, Market>,
    #[account(
        mut,
        seeds = [b"keeper_leader_lock", market.key().as_ref()],
        bump,
    )]
    pub keeper_leader_lock: Account<'info, KeeperLeaderLock>,
    /// The keeper signing this heartbeat. The handler stamps
    /// `keeper.key()` into the lock on success.
    pub keeper: Signer<'info>,
}

#[derive(Accounts)]
pub struct KeeperLeaderRelease<'info> {
    pub market: Account<'info, Market>,
    #[account(
        mut,
        seeds = [b"keeper_leader_lock", market.key().as_ref()],
        bump,
    )]
    pub keeper_leader_lock: Account<'info, KeeperLeaderLock>,
    pub keeper: Signer<'info>,
}

/// Initialise the per-market keeper-leader lock PDA. Permissionless;
/// any wallet can pay rent to create the account. The lock starts
/// in the unowned state (`has_leader = false`) and the configured
/// takeover threshold is the wave-15 default
/// (`KeeperLeaderLock::DEFAULT_TAKEOVER_THRESHOLD_SLOTS`). Future
/// governance changes can re-init via a separate ix.
pub fn initialize_keeper_leader_lock(
    ctx: Context<InitializeKeeperLeaderLock>,
) -> Result<()> {
    let lock = &mut ctx.accounts.keeper_leader_lock;
    lock.has_leader = false;
    lock.current_leader = [0u8; 32];
    let clock = Clock::get()?;
    lock.last_heartbeat_slot = clock.slot;
    lock.takeover_threshold_slots = KeeperLeaderLock::DEFAULT_TAKEOVER_THRESHOLD_SLOTS;
    Ok(())
}

/// Strict claim-stale path. Rejects when the lock is currently fresh
/// (with `KeeperLeaderAcquireWhileFresh`) — that's `keeper_leader_heartbeat`'s
/// job. Use this from an HA keeper replica that explicitly wants to
/// only take over when the active leader has gone silent.
pub fn keeper_leader_acquire(
    ctx: Context<KeeperLeaderHeartbeat>,
    args: KeeperLeaderHeartbeatArgs,
) -> Result<()> {
    let lock = &mut ctx.accounts.keeper_leader_lock;
    let signer = ctx.accounts.keeper.key().to_bytes();
    let chain_slot = Clock::get()?.slot;
    validate_observed_slot(args.observed_slot, lock.last_heartbeat_slot, chain_slot)?;

    let stale = is_lock_stale(lock, args.observed_slot);
    if !stale {
        return Err(error!(ProgramError::KeeperLeaderAcquireWhileFresh));
    }
    lock.has_leader = true;
    lock.current_leader = signer;
    lock.last_heartbeat_slot = args.observed_slot;
    Ok(())
}

/// All-paths heartbeat. Mirrors
/// `keeper_decoder::leader_lock::KeeperLeaderLock::try_heartbeat`
/// behaviour matrix exactly. The keeper bot's normal tick drives
/// this.
pub fn keeper_leader_heartbeat(
    ctx: Context<KeeperLeaderHeartbeat>,
    args: KeeperLeaderHeartbeatArgs,
) -> Result<()> {
    let lock = &mut ctx.accounts.keeper_leader_lock;
    let signer = ctx.accounts.keeper.key().to_bytes();
    let chain_slot = Clock::get()?.slot;
    validate_observed_slot(args.observed_slot, lock.last_heartbeat_slot, chain_slot)?;

    if !lock.has_leader {
        lock.has_leader = true;
        lock.current_leader = signer;
        lock.last_heartbeat_slot = args.observed_slot;
        return Ok(());
    }

    let stale = is_lock_stale(lock, args.observed_slot);
    let same_signer = lock.current_leader == signer;

    if same_signer {
        // Refresh self (or self-recover from stall — no rejection).
        lock.last_heartbeat_slot = args.observed_slot;
        return Ok(());
    }
    if stale {
        // Takeover.
        lock.current_leader = signer;
        lock.last_heartbeat_slot = args.observed_slot;
        return Ok(());
    }
    Err(error!(ProgramError::KeeperLeaderHeldByOther))
}

/// Graceful release. Only the current holder may release. Returns the
/// lock to the unowned state so a standby keeper can immediately
/// claim leadership without waiting for the takeover threshold.
pub fn keeper_leader_release(ctx: Context<KeeperLeaderRelease>) -> Result<()> {
    let lock = &mut ctx.accounts.keeper_leader_lock;
    let signer = ctx.accounts.keeper.key().to_bytes();
    if !lock.has_leader {
        return Err(error!(ProgramError::KeeperLeaderNotHeld));
    }
    if lock.current_leader != signer {
        return Err(error!(ProgramError::KeeperLeaderNotHolder));
    }
    lock.has_leader = false;
    lock.current_leader = [0u8; 32];
    let chain_slot = Clock::get()?.slot;
    lock.last_heartbeat_slot = chain_slot;
    Ok(())
}

/// Internal helper — caller's `observed_slot` must be `>=
/// recorded_slot` AND `<= chain_slot`. The first half is the host-
/// side `try_heartbeat`'s clock-skew guard; the second half is the
/// on-chain extra guard against a keeper trying to stamp a future
/// slot to extend its leadership.
fn validate_observed_slot(
    observed_slot: u64,
    recorded_slot: u64,
    chain_slot: u64,
) -> Result<()> {
    if observed_slot < recorded_slot {
        return Err(error!(ProgramError::KeeperLeaderClockSkew));
    }
    if observed_slot > chain_slot {
        return Err(error!(ProgramError::KeeperLeaderClockSkew));
    }
    Ok(())
}

/// Internal helper — same predicate as
/// `keeper_decoder::leader_lock::KeeperLeaderLock::is_stale`.
fn is_lock_stale(lock: &KeeperLeaderLock, current_slot: u64) -> bool {
    if !lock.has_leader {
        return true;
    }
    let elapsed = current_slot.saturating_sub(lock.last_heartbeat_slot);
    elapsed > lock.takeover_threshold_slots
}
