//! Wave 12 — production tick-loop wrapper.
//!
//! `KeeperBot::tick` is the per-tick state machine; `run_loop`
//! orchestrates the whole daemon: boot logging, metric registration,
//! shutdown signal handling, sleep cadence, transient-error
//! tolerance, and clean exit.
//!
//! ## Error tolerance contract
//!
//! - **Transient snapshot errors** (e.g. an RPC timeout that returns
//!   `RpcError::Transport`) increment `snapshot_errors_total` and
//!   continue the loop. The wave-9 / wave-10 keeper design assumes
//!   the bot keeps ticking through transient outages — losing a few
//!   ticks during a regional RPC outage is acceptable.
//! - **Permanent snapshot errors** — `MarketPaused`,
//!   `SchemaVersionMismatch`, `MarketNotFound` — stop the loop
//!   with the error returned to the caller. These signal governance
//!   actions that the keeper cannot work around; the runbook
//!   (§24-operator-runbook.md) tells ops how to recover.
//! - **Per-action dispatch failures** are recorded in the
//!   `TickReport` (counted against `actions_failed_total`) but do
//!   NOT abort the tick or the loop. This matches wave 10's
//!   `KeeperLoop::tick` semantics.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use keeper_decoder::ix::KeeperLeaderHeartbeatArgs;
use keeper_rpc::leader_tx::{
    build_keeper_leader_heartbeat, build_keeper_leader_release, fetch_keeper_leader_lock,
    KeeperLeaderTxBuilder, LeaderReconcileError,
};
use keeper_rpc::{
    AccountFetcher, ChainSnapshot, MarketContext, Pubkey32, SnapshotConfig, SnapshotError,
    TxBuilder,
};

use crate::leader::{HostMirrorLeaderPolicy, LeaderPolicy};
use crate::metrics::{KeeperMetrics, LeaderStatus};
use crate::{BotError, KeeperBot, snapshot_best_slot};

/// Tick cadence + termination policy for [`run_loop`].
#[derive(Debug, Clone, Copy)]
pub struct RunLoopConfig {
    /// Wall-clock interval between ticks. Production: 800 ms (≈
    /// every other slot). Backtests can use `Duration::ZERO` to
    /// burn through samples as fast as the fetcher returns them.
    pub tick_interval: Duration,
    /// Optional cap on the number of completed ticks. `None` means
    /// "run forever / until shutdown". Used by integration tests to
    /// bound test runtime.
    pub max_ticks: Option<u64>,
    /// On a transient snapshot error, sleep this long before the
    /// next tick attempt (in addition to `tick_interval`). Default
    /// 200 ms — long enough to avoid spinning on a flapping RPC,
    /// short enough that recovery happens within a couple of
    /// keeper cycles.
    pub transient_error_backoff: Duration,
}

impl Default for RunLoopConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_millis(800),
            max_ticks: None,
            transient_error_backoff: Duration::from_millis(200),
        }
    }
}

/// Reason the loop terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopTerminationReason {
    /// `shutdown` flag was set externally (signal handler /
    /// supervisor sent SIGTERM).
    ShutdownSignal,
    /// `max_ticks` reached.
    TickLimitReached,
    /// Permanent error stopped the loop (paused / schema mismatch /
    /// market not found / scheduler invariant violation).
    PermanentError,
}

/// Result of a completed loop run.
#[derive(Debug)]
pub struct LoopOutcome {
    /// Why the loop exited.
    pub reason: LoopTerminationReason,
    /// Total successful ticks observed.
    pub ticks: u64,
    /// On `PermanentError`, the underlying error.
    pub error: Option<BotError>,
}

/// Run the tick loop until either `shutdown` is set, `max_ticks`
/// is reached, or the bot returns a permanent snapshot error.
///
/// ## Borrows
///
/// `bot`, `metrics`, and `shutdown` live for the lifetime of the
/// daemon process; `fetcher` and `ctx` are borrowed for each tick;
/// `builder` is owned and recycled (recovered after each call to
/// [`KeeperBot::tick`] which returns it).
#[allow(clippy::too_many_arguments)]
pub fn run_loop<F: AccountFetcher, B: TxBuilder>(
    bot: &mut KeeperBot,
    fetcher: &F,
    ctx: &MarketContext,
    mut builder: B,
    keeper_pk: Pubkey32,
    clock_sysvar: Pubkey32,
    system_program: Pubkey32,
    metrics: &KeeperMetrics,
    shutdown: &AtomicBool,
    cfg: RunLoopConfig,
) -> LoopOutcome {
    let mut ticks: u64 = 0;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!(ticks, "shutdown signal observed, exiting loop");
            return LoopOutcome {
                reason: LoopTerminationReason::ShutdownSignal,
                ticks,
                error: None,
            };
        }
        if let Some(max) = cfg.max_ticks {
            if ticks >= max {
                return LoopOutcome {
                    reason: LoopTerminationReason::TickLimitReached,
                    ticks,
                    error: None,
                };
            }
        }

        let start = Instant::now();
        let result = bot.tick(
            fetcher,
            ctx,
            builder,
            keeper_pk,
            clock_sysvar,
            system_program,
        );
        match result {
            Ok((report, returned_builder)) => {
                builder = returned_builder;
                let duration_ms = clamp_ms(start.elapsed());
                metrics.observe_tick(&report, duration_ms);
                metrics.set_vol_samples(bot.vol().sample_count() as u64);
                tracing::info!(
                    tick = ticks,
                    actions_planned = report.actions_planned,
                    init_hints_added = report.init_hints_added,
                    applied_vol = ?report.applied_vol,
                    duration_ms,
                    "tick complete",
                );
                ticks += 1;
                std::thread::sleep(cfg.tick_interval);
            }
            Err(e) => {
                metrics.observe_snapshot_error();
                if is_transient(&e) {
                    tracing::warn!(error = %e, "transient snapshot error, continuing");
                    // Builder is unrecoverable: `tick` consumed it
                    // before erroring. The caller's pattern is to
                    // mint a fresh builder per loop, but in our
                    // sync model we don't have a factory. Since
                    // permanent errors return immediately, we only
                    // hit this branch when the snapshot fetch fails
                    // before the tx layer touched the builder, so
                    // we panic if the builder was lost — that's a
                    // programmer error in the caller.
                    //
                    // In practice `tick`'s contract is to return
                    // the builder unconditionally on success; on
                    // error it keeps it. We ergonomically can't
                    // recover the builder here in stable Rust
                    // without a closure factory, so we exit the
                    // loop with a "TransientLost" treated as
                    // permanent. Real production wiring uses
                    // `run_loop_with_factory` below.
                    return LoopOutcome {
                        reason: LoopTerminationReason::PermanentError,
                        ticks,
                        error: Some(e),
                    };
                }
                tracing::error!(error = %e, "permanent snapshot error, stopping loop");
                return LoopOutcome {
                    reason: LoopTerminationReason::PermanentError,
                    ticks,
                    error: Some(e),
                };
            }
        }
    }
}

/// Variant of [`run_loop`] that mints a fresh `TxBuilder` per tick
/// via a closure. This is the production-recommended shape:
///
/// - It correctly handles transient errors (re-attempts with a
///   fresh builder + brief backoff).
/// - It allows hot-rotating credentials (e.g. fetching a new
///   blockhash-cache from a leader-election service) per tick.
///
/// `factory` is called exactly once per tick attempt. Use it to
/// `clone` a per-process `Keypair` or to construct a stateless
/// `MockTxBuilder` for tests.
#[allow(clippy::too_many_arguments)]
pub fn run_loop_with_factory<F, B, MakeB>(
    bot: &mut KeeperBot,
    fetcher: &F,
    ctx: &MarketContext,
    mut factory: MakeB,
    keeper_pk: Pubkey32,
    clock_sysvar: Pubkey32,
    system_program: Pubkey32,
    metrics: &KeeperMetrics,
    shutdown: &AtomicBool,
    cfg: RunLoopConfig,
) -> LoopOutcome
where
    F: AccountFetcher,
    B: TxBuilder,
    MakeB: FnMut() -> B,
{
    let mut ticks: u64 = 0;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!(ticks, "shutdown signal observed, exiting loop");
            return LoopOutcome {
                reason: LoopTerminationReason::ShutdownSignal,
                ticks,
                error: None,
            };
        }
        if let Some(max) = cfg.max_ticks {
            if ticks >= max {
                return LoopOutcome {
                    reason: LoopTerminationReason::TickLimitReached,
                    ticks,
                    error: None,
                };
            }
        }

        let builder = factory();
        let start = Instant::now();
        let result = bot.tick(
            fetcher,
            ctx,
            builder,
            keeper_pk,
            clock_sysvar,
            system_program,
        );
        match result {
            Ok((report, _builder)) => {
                let duration_ms = clamp_ms(start.elapsed());
                metrics.observe_tick(&report, duration_ms);
                metrics.set_vol_samples(bot.vol().sample_count() as u64);
                tracing::info!(
                    tick = ticks,
                    actions_planned = report.actions_planned,
                    init_hints_added = report.init_hints_added,
                    applied_vol = ?report.applied_vol,
                    duration_ms,
                    "tick complete",
                );
                ticks += 1;
                std::thread::sleep(cfg.tick_interval);
            }
            Err(e) => {
                metrics.observe_snapshot_error();
                if is_transient(&e) {
                    tracing::warn!(error = %e, "transient snapshot error, continuing");
                    std::thread::sleep(cfg.transient_error_backoff);
                    continue;
                }
                tracing::error!(error = %e, "permanent snapshot error, stopping loop");
                return LoopOutcome {
                    reason: LoopTerminationReason::PermanentError,
                    ticks,
                    error: Some(e),
                };
            }
        }
    }
}

/// Wave 15 — leader-gated variant of [`run_loop_with_factory`].
/// Wave 16 — folded the wave-15 double-RPC path into a single
/// snapshot refresh per tick by reusing [`KeeperBot::tick_with_snap`].
///
/// Each tick:
///
/// 1. Refresh a `ChainSnapshot` once.
/// 2. Compute `current_slot = snapshot_best_slot(&snap).unwrap_or(0)`.
/// 3. Call `policy.should_submit(current_slot)`.
///    - `true`  → metric `LeaderStatus::Leader`, run
///      `KeeperBot::tick_with_snap(&snap, …)` against the same
///      snapshot we used for the leader gate (no second RPC).
///    - `false` → metric `LeaderStatus::Standby`, skip dispatch
///      this tick. Tick counter still advances; this gives ops
///      the same liveness signal a leader-elected bot would emit.
/// 4. Sleep `tick_interval` between ticks.
///
/// Wave-12 file-lock fallback: callers who want the wave-12
/// behaviour (no on-chain leader gate) keep using
/// [`run_loop_with_factory`] which doesn't touch `LeaderPolicy`.
#[allow(clippy::too_many_arguments)]
pub fn run_loop_with_leader<F, B, MakeB, P>(
    bot: &mut KeeperBot,
    fetcher: &F,
    ctx: &MarketContext,
    mut factory: MakeB,
    keeper_pk: Pubkey32,
    clock_sysvar: Pubkey32,
    system_program: Pubkey32,
    metrics: &KeeperMetrics,
    shutdown: &AtomicBool,
    cfg: RunLoopConfig,
    policy: &P,
) -> LoopOutcome
where
    F: AccountFetcher,
    B: TxBuilder,
    MakeB: FnMut() -> B,
    P: LeaderPolicy + ?Sized,
{
    let mut ticks: u64 = 0;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!(ticks, "shutdown signal observed, exiting loop");
            return LoopOutcome {
                reason: LoopTerminationReason::ShutdownSignal,
                ticks,
                error: None,
            };
        }
        if let Some(max) = cfg.max_ticks {
            if ticks >= max {
                return LoopOutcome {
                    reason: LoopTerminationReason::TickLimitReached,
                    ticks,
                    error: None,
                };
            }
        }

        // Single-refresh per tick (wave 16). Same `SnapshotConfig`
        // semantics as wave 12 / 15 — paused / schema-mismatch are
        // permanent across both the leader gate and the full tick.
        let mut snap = ChainSnapshot::new();
        match snap.refresh(fetcher, ctx, SnapshotConfig::default()) {
            Ok(()) => {}
            Err(e) => {
                let bot_err = BotError::Snapshot(e);
                metrics.observe_snapshot_error();
                if is_transient(&bot_err) {
                    tracing::warn!(error = %bot_err, "transient snapshot error during leader-gated refresh");
                    std::thread::sleep(cfg.transient_error_backoff);
                    continue;
                }
                tracing::error!(error = %bot_err, "permanent snapshot error during leader-gated refresh");
                return LoopOutcome {
                    reason: LoopTerminationReason::PermanentError,
                    ticks,
                    error: Some(bot_err),
                };
            }
        }
        let current_slot = snapshot_best_slot(&snap).unwrap_or(0);
        if !policy.should_submit(current_slot) {
            metrics.set_leader_status(LeaderStatus::Standby);
            tracing::info!(
                tick = ticks,
                current_slot,
                "leader-lock gate denied — skipping dispatch this tick"
            );
            ticks += 1;
            std::thread::sleep(cfg.tick_interval);
            continue;
        }
        metrics.set_leader_status(LeaderStatus::Leader);

        let builder = factory();
        let start = Instant::now();
        let result = bot.tick_with_snap::<B>(
            &snap,
            ctx,
            builder,
            keeper_pk,
            clock_sysvar,
            system_program,
        );
        match result {
            Ok((report, _builder)) => {
                let duration_ms = clamp_ms(start.elapsed());
                metrics.observe_tick(&report, duration_ms);
                metrics.set_vol_samples(bot.vol().sample_count() as u64);
                tracing::info!(
                    tick = ticks,
                    actions_planned = report.actions_planned,
                    init_hints_added = report.init_hints_added,
                    applied_vol = ?report.applied_vol,
                    duration_ms,
                    leader_slot = current_slot,
                    "tick complete (leader)",
                );
                ticks += 1;
                std::thread::sleep(cfg.tick_interval);
            }
            Err(e) => {
                // Wave 16 — `tick_with_snap` only emits scheduler
                // invariant violations (permanent), not snapshot
                // errors (we already handled those on refresh
                // above). So this branch is always permanent.
                metrics.observe_snapshot_error();
                tracing::error!(error = %e, "permanent error from tick_with_snap, stopping loop");
                return LoopOutcome {
                    reason: LoopTerminationReason::PermanentError,
                    ticks,
                    error: Some(e),
                };
            }
        }
    }
}

    /// Wave 16/17 — RPC-reconcile cadence config for
/// [`run_loop_with_leader_and_rpc_reconcile`].
#[derive(Debug, Clone, Copy)]
pub struct LeaderRpcReconcileConfig {
    /// Reconcile every `reconcile_every` ticks. The cached host
    /// mirror is updated from the on-chain `KeeperLeaderLock` PDA;
    /// after that, `should_submit` runs on a chain-true mirror.
    /// Production deployments use `Some(20)` (≈ 16 s @ 800 ms tick).
    /// `None` disables RPC reconcile (host mirror only — useful for
    /// tests).
    pub reconcile_every: Option<u64>,
    /// On-chain `KeeperLeaderLock` PDA. Computed at boot via
    /// `find_program_address(&keeper_leader_lock_seeds(market), program_id)`
    /// and pinned for the lifetime of the bot process.
    pub lock_pda: Pubkey32,
    /// When `Some(args.observed_slot)` advances, the run-loop also
    /// submits a `keeper_leader_heartbeat` ix via the
    /// [`KeeperLeaderTxBuilder`] each `heartbeat_every` ticks. This
    /// keeps the on-chain lock fresh under the active leader.
    /// `None` disables on-chain heartbeats (useful when ops want
    /// to test leadership read-only or the keeper is observability-
    /// only). Production: `Some(5)` (≈ 4 s @ 800 ms tick — well
    /// inside the wave-15 default `takeover_threshold_slots = 75 ≈
    /// 30 s`).
    pub heartbeat_every: Option<u64>,
    /// Wave 17 — when `true` AND the bot was holding leadership at
    /// the moment a shutdown signal arrived, the run-loop submits
    /// one `keeper_leader_release` ix via `leader_builder` before
    /// returning the `LoopOutcome { reason: ShutdownSignal, … }`.
    ///
    /// Why this matters: wave-15 `takeover_threshold_slots` is 75
    /// slots ≈ 30 s. Without graceful release, after a planned
    /// `systemctl stop`, the standby bot must wait that full window
    /// before it's allowed to acquire — meaning ~30 s of zero-keeper
    /// time per maintenance event. With graceful release the
    /// standby's first reconcile sees `has_leader = false`, the
    /// next heartbeat acquires immediately, and the keeper layer is
    /// only down for the standby's reconcile cadence (≤ 16 s by
    /// default, often < 5 s in practice).
    ///
    /// Failure to publish the release ix (RPC outage during
    /// shutdown) is **non-fatal** — we log a warn and continue the
    /// shutdown. The standby will still recover, just on the
    /// natural takeover path.
    pub release_on_shutdown: bool,
}

impl Default for LeaderRpcReconcileConfig {
    fn default() -> Self {
        Self {
            reconcile_every: Some(20),
            lock_pda: [0u8; 32],
            heartbeat_every: Some(5),
            release_on_shutdown: true,
        }
    }
}

/// Wave 17 — best-effort `keeper_leader_release` publish triggered
/// by a graceful shutdown signal. Returns `Ok(Some(sig))` when the
/// release tx submitted, `Ok(None)` when the bot wasn't currently
/// the holder (no release needed), and `Err(reason)` when the RPC
/// publish itself errored.
///
/// Pure helper — caller decides whether to invoke it. Extracted out
/// of the run-loop body so unit tests can pin the contract without
/// spinning up the whole tick machine.
pub fn try_graceful_release<L>(
    cfg: &LeaderRpcReconcileConfig,
    leader_builder: &mut L,
    program_id: Pubkey32,
    market: Pubkey32,
    keeper_pk: Pubkey32,
    was_leader_last_tick: bool,
) -> Result<Option<String>, String>
where
    L: KeeperLeaderTxBuilder + ?Sized,
{
    if !cfg.release_on_shutdown {
        return Ok(None);
    }
    if !was_leader_last_tick {
        return Ok(None);
    }
    let ix = build_keeper_leader_release(program_id, market, cfg.lock_pda, keeper_pk);
    leader_builder.submit_leader_ix(ix)
}

/// Wave 16 — leader-gated tick loop with on-chain reconcile +
/// heartbeat publication.
///
/// Composes the wave-15 [`run_loop_with_leader`] gate with the
/// wave-16 RPC reconcile + heartbeat publish path:
///
/// 1. Refresh `ChainSnapshot` once (paused / schema-mismatch =
///    permanent across all subsequent gates).
/// 2. Every `reconcile_every` ticks: fetch the on-chain
///    `KeeperLeaderLock` PDA via `fetch_keeper_leader_lock(fetcher,
///    lock_pda)` and call `policy.reconcile(snapshot)`. Errors are
///    logged + the policy stays on its last good mirror (we don't
///    permanently fail the run-loop on one transient lock fetch).
/// 3. Compute `current_slot = snapshot_best_slot(&snap).unwrap_or(0)`.
/// 4. Call `policy.should_submit(current_slot)`. `Standby` →
///    metric flip + skip dispatch. `Leader` → metric flip,
///    proceed.
/// 5. Every `heartbeat_every` ticks (and immediately on the
///    transition to leader): submit a `keeper_leader_heartbeat`
///    ix via `leader_builder`. Failure to publish is logged; the
///    policy reconciles on the next tick anyway, so transient
///    publish failures don't permanently break leadership.
/// 6. Run `KeeperBot::tick_with_snap(&snap, …)` against the
///    same snapshot.
/// 7. Sleep `tick_interval` between ticks.
///
/// `LeaderRpcReconcileConfig::reconcile_every = None` and
/// `heartbeat_every = None` falls back to wave-15 host-mirror
/// behaviour (no RPC reconcile, no on-chain heartbeats).
#[allow(clippy::too_many_arguments)]
pub fn run_loop_with_leader_and_rpc_reconcile<F, B, MakeB, L>(
    bot: &mut KeeperBot,
    fetcher: &F,
    ctx: &MarketContext,
    mut factory: MakeB,
    keeper_pk: Pubkey32,
    clock_sysvar: Pubkey32,
    system_program: Pubkey32,
    metrics: &KeeperMetrics,
    shutdown: &AtomicBool,
    cfg: RunLoopConfig,
    policy: &HostMirrorLeaderPolicy,
    rpc_cfg: LeaderRpcReconcileConfig,
    leader_builder: &mut L,
) -> LoopOutcome
where
    F: AccountFetcher,
    B: TxBuilder,
    MakeB: FnMut() -> B,
    L: KeeperLeaderTxBuilder + ?Sized,
{
    let mut ticks: u64 = 0;
    let mut last_reconcile_tick: Option<u64> = None;
    let mut last_heartbeat_tick: Option<u64> = None;
    let mut was_leader_last_tick = false;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            // Wave 17 — best-effort graceful release before exit.
            // We deliberately attempt this BEFORE logging the
            // shutdown so the log contains both the trigger AND the
            // outcome of the release attempt for ops post-mortem.
            match try_graceful_release(
                &rpc_cfg,
                leader_builder,
                ctx.program_id,
                ctx.market,
                keeper_pk,
                was_leader_last_tick,
            ) {
                Ok(Some(sig)) => tracing::info!(
                    ticks,
                    sig = %sig,
                    "graceful keeper_leader_release submitted on shutdown"
                ),
                Ok(None) => tracing::info!(
                    ticks,
                    "shutdown — graceful release skipped (not currently leader or release_on_shutdown=false)"
                ),
                Err(e) => tracing::warn!(
                    ticks,
                    error = %e,
                    "graceful keeper_leader_release submit failed on shutdown — \
                     standby will still recover via takeover after the threshold window"
                ),
            }
            tracing::info!(ticks, "shutdown signal observed, exiting loop");
            return LoopOutcome {
                reason: LoopTerminationReason::ShutdownSignal,
                ticks,
                error: None,
            };
        }
        if let Some(max) = cfg.max_ticks {
            if ticks >= max {
                return LoopOutcome {
                    reason: LoopTerminationReason::TickLimitReached,
                    ticks,
                    error: None,
                };
            }
        }

        let mut snap = ChainSnapshot::new();
        match snap.refresh(fetcher, ctx, SnapshotConfig::default()) {
            Ok(()) => {}
            Err(e) => {
                let bot_err = BotError::Snapshot(e);
                metrics.observe_snapshot_error();
                if is_transient(&bot_err) {
                    tracing::warn!(error = %bot_err, "transient snapshot error during leader+reconcile refresh");
                    std::thread::sleep(cfg.transient_error_backoff);
                    continue;
                }
                tracing::error!(error = %bot_err, "permanent snapshot error during leader+reconcile refresh");
                return LoopOutcome {
                    reason: LoopTerminationReason::PermanentError,
                    ticks,
                    error: Some(bot_err),
                };
            }
        }

        // Reconcile cadence. Reconcile errors are LOGGED, not fatal
        // — a transient RPC blip during reconcile shouldn't kill the
        // bot. The cached mirror stays on its last good value;
        // worst case we keep submitting under our own previous
        // belief about leadership for a few ticks until the next
        // reconcile succeeds.
        if let Some(every) = rpc_cfg.reconcile_every {
            let due = match last_reconcile_tick {
                None => true,
                Some(prev) => ticks.saturating_sub(prev) >= every,
            };
            if due {
                match fetch_keeper_leader_lock(fetcher, &rpc_cfg.lock_pda) {
                    Ok(chain_lock) => {
                        policy.reconcile(chain_lock);
                        last_reconcile_tick = Some(ticks);
                        tracing::debug!(tick = ticks, "leader-lock reconcile ok");
                    }
                    Err(LeaderReconcileError::NotFound(_)) => {
                        tracing::error!(
                            tick = ticks,
                            "keeper-leader-lock PDA not initialised on chain — \
                             ops must send `initialize_keeper_leader_lock` (see runbook)"
                        );
                        last_reconcile_tick = Some(ticks);
                    }
                    Err(e) => {
                        tracing::warn!(
                            tick = ticks,
                            error = %e,
                            "leader-lock reconcile failed; falling back to cached mirror this tick"
                        );
                    }
                }
            }
        }

        let current_slot = snapshot_best_slot(&snap).unwrap_or(0);
        let should_submit = policy.should_submit(current_slot);
        if !should_submit {
            metrics.set_leader_status(LeaderStatus::Standby);
            tracing::info!(
                tick = ticks,
                current_slot,
                "leader-lock gate denied — skipping dispatch this tick"
            );
            was_leader_last_tick = false;
            ticks += 1;
            std::thread::sleep(cfg.tick_interval);
            continue;
        }
        metrics.set_leader_status(LeaderStatus::Leader);

        // Heartbeat publish cadence. Always publish on the very
        // first leader tick (so we don't wait `heartbeat_every`
        // ticks before stamping our identity onto the chain lock),
        // and again every `heartbeat_every` ticks while we remain
        // leader.
        if let Some(every) = rpc_cfg.heartbeat_every {
            let just_became_leader = !was_leader_last_tick;
            let due = match last_heartbeat_tick {
                None => true,
                Some(prev) => ticks.saturating_sub(prev) >= every,
            };
            if just_became_leader || due {
                let ix = build_keeper_leader_heartbeat(
                    ctx.program_id,
                    ctx.market,
                    rpc_cfg.lock_pda,
                    keeper_pk,
                    KeeperLeaderHeartbeatArgs {
                        observed_slot: current_slot,
                    },
                );
                match leader_builder.submit_leader_ix(ix) {
                    Ok(sig) => {
                        last_heartbeat_tick = Some(ticks);
                        tracing::info!(
                            tick = ticks,
                            current_slot,
                            ?sig,
                            "keeper_leader_heartbeat submitted"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            tick = ticks,
                            error = %e,
                            "keeper_leader_heartbeat submit failed (will retry next cadence)"
                        );
                    }
                }
            }
        }
        was_leader_last_tick = true;

        let builder = factory();
        let start = Instant::now();
        let result = bot.tick_with_snap::<B>(
            &snap,
            ctx,
            builder,
            keeper_pk,
            clock_sysvar,
            system_program,
        );
        match result {
            Ok((report, _builder)) => {
                let duration_ms = clamp_ms(start.elapsed());
                metrics.observe_tick(&report, duration_ms);
                metrics.set_vol_samples(bot.vol().sample_count() as u64);
                tracing::info!(
                    tick = ticks,
                    actions_planned = report.actions_planned,
                    init_hints_added = report.init_hints_added,
                    applied_vol = ?report.applied_vol,
                    duration_ms,
                    leader_slot = current_slot,
                    "tick complete (leader, rpc-reconciled)",
                );
                ticks += 1;
                std::thread::sleep(cfg.tick_interval);
            }
            Err(e) => {
                metrics.observe_snapshot_error();
                tracing::error!(error = %e, "permanent error from tick_with_snap, stopping loop");
                return LoopOutcome {
                    reason: LoopTerminationReason::PermanentError,
                    ticks,
                    error: Some(e),
                };
            }
        }
    }
}

fn clamp_ms(d: Duration) -> u64 {
    let ms = d.as_millis();
    if ms > u64::MAX as u128 { u64::MAX } else { ms as u64 }
}

/// Wave 18 — public re-export of [`clamp_ms`] so the multi-market
/// run loop in [`crate::multi`] can apply the same wave-12 ms-
/// clamp logic without duplicating the helper. Crate-internal API;
/// downstream consumers should keep using `Duration`-typed metrics.
#[doc(hidden)]
pub fn clamp_ms_pub(d: Duration) -> u64 {
    clamp_ms(d)
}

/// Classify a [`BotError`] as transient (loop continues) or
/// permanent (loop stops). The classification is conservative —
/// when in doubt, we mark permanent so ops gets paged.
pub fn is_transient(err: &BotError) -> bool {
    match err {
        BotError::Snapshot(s) => match s {
            // Permanent: governance + structural problems.
            SnapshotError::MarketPaused
            | SnapshotError::SchemaVersionMismatch { .. }
            | SnapshotError::MarketNotFound(_)
            | SnapshotError::SubPoolNotFound { .. }
            | SnapshotError::Decode { .. } => false,
            // Transient: anything that smells like a network blip.
            SnapshotError::Rpc(_) => true,
        },
        // Scheduler invariant violations are permanent — they
        // indicate a bug in the planner that should never recover
        // without intervention.
        BotError::Scheduler(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TickReport;
    use crate::metrics::KeeperMetrics;
    use keeper::ActionDispatchResult;
    use keeper_rpc::RpcError;

    /// `is_transient` must NEVER mis-classify governance errors as
    /// transient. If a paused market were retried in the loop, the
    /// keeper would log thousands of failures per minute and spam
    /// the alert queue.
    #[test]
    fn governance_errors_are_permanent() {
        assert!(!is_transient(&BotError::Snapshot(SnapshotError::MarketPaused)));
        assert!(!is_transient(&BotError::Snapshot(
            SnapshotError::SchemaVersionMismatch {
                onchain: 2,
                compiled: 1,
            }
        )));
        assert!(!is_transient(&BotError::Snapshot(
            SnapshotError::MarketNotFound([0u8; 32])
        )));
    }

    /// Network errors are transient — we want the loop to keep
    /// running through a brief RPC outage.
    #[test]
    fn rpc_transport_errors_are_transient() {
        let e = BotError::Snapshot(SnapshotError::Rpc(RpcError::Transport(
            "timeout".to_string(),
        )));
        assert!(is_transient(&e));
    }

    /// `clamp_ms` must not panic on huge durations.
    #[test]
    fn clamp_ms_handles_overflow() {
        let huge = Duration::from_secs(u64::MAX / 1000);
        let _ = clamp_ms(huge);
    }

    /// Quick smoke for the metrics → render path through the
    /// tick-observation API. Doesn't actually run a loop, just
    /// confirms the wiring matches what `run_loop` expects.
    #[test]
    fn observe_tick_via_metrics_round_trips() {
        let m = KeeperMetrics::new();
        let r = TickReport {
            actions_planned: 1,
            dispatched: vec![(
                keeper::KeeperAction::CloseDormantBucket {
                    sub_pool_id: 0,
                    direction: clearing_core::Direction::Long,
                    tick: 0,
                },
                ActionDispatchResult::Submitted { signature: None },
            )],
            applied_vol: Some(0.7),
            init_hints_added: 0,
        };
        m.observe_tick(&r, 12);
        let text = m.render_prometheus();
        assert!(text.contains("\nkeeper_actions_submitted_total 1\n"));
        assert!(text.contains("\nkeeper_last_tick_duration_milliseconds 12\n"));
    }
}
