//! Wave 18 — multi-market run loop.
//!
//! The wave-12 [`run_loop`] / wave-15 [`run_loop_with_leader`] / wave-16
//! [`run_loop_with_leader_and_rpc_reconcile`] / wave-17 graceful release
//! family all assume a single [`MarketContext`] + a single
//! [`KeeperBot`] + a single [`KeeperMetrics`] register. Multi-market
//! deployments outgrow that contract:
//!
//! - Each market has its own `KeeperLeaderLock` PDA, so reconcile +
//!   heartbeat cadences are per-market.
//! - Per-market metrics labels (`market="SOL-USD"`) let dashboards
//!   show each market's leader status, applied vol, fail rate
//!   independently.
//! - One bot wallet should be able to supervise N markets without
//!   spawning N daemons (saves N-1 wallets, N-1 RPC connections,
//!   N-1 sets of restart logic).
//!
//! ## Strategy
//!
//! [`MarketRegistry`] owns one [`MarketSlot`] per supervised market;
//! every slot bundles the market's [`KeeperBot`], [`KeeperMetrics`],
//! [`MarketContext`], [`HostMirrorLeaderPolicy`],
//! [`LeaderRpcReconcileConfig`], and book-keeping counters. The
//! [`run_loop_multi_market_leader_and_rpc_reconcile`] outer loop
//! iterates the registry every wall-clock tick and, for each slot,
//! does what the single-market wave-16 loop did inside its inner
//! body: refresh snapshot, reconcile, leader gate, heartbeat
//! cadence, and tick. Shutdown protocol mirrors wave-17: every
//! leader market gets a graceful `keeper_leader_release` ix on the
//! way out.
//!
//! ## Why one outer loop, not N threads?
//!
//! Wave-12..17 was deliberately single-threaded to keep the
//! tick-loop reproducible (no MPSC, no tokio, no thread-pool). The
//! same property is what makes multi-market here trivial: per-tick
//! the outer loop walks the registry sequentially. If a future
//! deployment needs per-market parallelism the registry is a
//! `Vec<MarketSlot>` so a thread-pool can fan out — but until a
//! real production trace shows tick-budget pressure we don't ship
//! the complexity.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use keeper_decoder::ix::KeeperLeaderHeartbeatArgs;
use keeper_rpc::leader_tx::{
    build_keeper_leader_heartbeat, fetch_keeper_leader_lock, KeeperLeaderTxBuilder,
    LeaderReconcileError,
};
use keeper_rpc::{
    AccountFetcher, ChainSnapshot, MarketContext, Pubkey32, SnapshotConfig, TxBuilder,
};

use crate::leader::{HostMirrorLeaderPolicy, LeaderPolicy};
use crate::metrics::{KeeperMetrics, LeaderStatus};
use crate::run::{
    clamp_ms_pub, is_transient, try_graceful_release, LeaderRpcReconcileConfig, LoopOutcome,
    LoopTerminationReason, RunLoopConfig,
};
use crate::{snapshot_best_slot, BotError, KeeperBot};

/// Wave 18 — per-market state slot.
///
/// Owned by the registry; the registry hands out `&mut MarketSlot`
/// to the run loop per outer iteration. The slot is _self_-
/// contained: the wave-17 graceful-release path on shutdown
/// reads `was_leader_last_tick` from this struct, so a single bot
/// crash won't mistakenly release the wrong market's lock.
pub struct MarketSlot {
    /// Stable label used as the `market="…"` Prometheus dimension.
    /// Conventionally the on-chain `Market.symbol` ASCII
    /// (e.g. "SOL-USD"); pubkey hex works as a fallback.
    pub label: String,
    /// Frozen-at-boot per-market chain context (`MarketContext` from
    /// wave 10).
    pub ctx: MarketContext,
    /// Per-market leader-lock reconcile / heartbeat cadence config
    /// (wave 16/17).
    pub rpc_cfg: LeaderRpcReconcileConfig,
    /// Bot state machine for this market.
    pub bot: KeeperBot,
    /// Per-market metrics register (wave-12 register; wave-18
    /// labeled rendering via `KeeperMetrics::render_prometheus_with_labels`).
    pub metrics: KeeperMetrics,
    /// Per-market host-mirror leader policy.
    pub policy: HostMirrorLeaderPolicy,
    /// Internal book-keeping (mutated by the run loop).
    pub state: MarketSlotState,
}

/// Wave 18 — per-market book-keeping the run loop uses to track
/// reconcile cadence, heartbeat cadence, and the
/// `was_leader_last_tick` signal that drives wave-17 graceful
/// release. Public so tests can pre-seed it; production callers
/// always start from `Default`.
///
/// Not `Clone` — `finished: Option<LoopOutcome>` carries a
/// `BotError` which doesn't implement `Clone` because `thiserror`
/// transparent variants make that error-prone. The run loop
/// `take()`s the outcome at termination time, so cloning isn't
/// needed.
#[derive(Debug, Default)]
pub struct MarketSlotState {
    /// Tick count this slot has completed.
    pub ticks: u64,
    /// Tick index at which the slot last reconciled the on-chain
    /// `KeeperLeaderLock`. `None` means "haven't reconciled yet,
    /// next tick will" so the first reconcile fires immediately
    /// after boot.
    pub last_reconcile_tick: Option<u64>,
    /// Tick index at which the slot last published a
    /// `keeper_leader_heartbeat`. Same semantics as above.
    pub last_heartbeat_tick: Option<u64>,
    /// `true` iff the slot held leadership at the end of the most
    /// recent leader-gate evaluation. Drives wave-17 graceful-
    /// release on shutdown.
    pub was_leader_last_tick: bool,
    /// Latched outcome — once set, the slot is "retired" and the
    /// outer loop skips it on subsequent ticks. `None` means
    /// "still ticking".
    pub finished: Option<LoopOutcome>,
}

/// Wave 18 — registry of supervised markets.
#[derive(Default)]
pub struct MarketRegistry {
    slots: Vec<MarketSlot>,
}

impl MarketRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a market slot to the registry. Returns `Err(label)` if
    /// the label is already in use — labels MUST be unique because
    /// they're the metric-label dimension; collision would fold
    /// two markets' metrics together.
    pub fn add(&mut self, slot: MarketSlot) -> Result<(), String> {
        if self.slots.iter().any(|s| s.label == slot.label) {
            return Err(format!("duplicate market label '{}'", slot.label));
        }
        self.slots.push(slot);
        Ok(())
    }

    /// Number of registered markets.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// `true` iff no markets are registered.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Iterate slots (read-only). Test helpers + ops introspection
    /// use this; the run loop uses `slots_mut`.
    pub fn slots(&self) -> &[MarketSlot] {
        &self.slots
    }

    /// Mutable slot access — only the run loop should call this.
    pub fn slots_mut(&mut self) -> &mut [MarketSlot] {
        &mut self.slots
    }

    /// Render every slot's metrics with `market="<label>"` spliced
    /// into every line. Wave-12 `serve` route `/metrics` calls this
    /// in multi-market mode.
    pub fn render_prometheus_all(&self) -> String {
        let mut out = String::with_capacity(self.slots.len() * 2048);
        for slot in &self.slots {
            out.push_str(
                &slot
                    .metrics
                    .render_prometheus_with_labels(&[("market", slot.label.as_str())]),
            );
        }
        out
    }

    /// Wave 21 — render the per-market metric snapshot as a JSON
    /// array. Each element is `{ "market": "<symbol>", "metrics":
    /// { ... wave-21 KeeperMetrics::render_json_snapshot ... } }`.
    /// Used by the wave-21 `/metrics-multi` route in
    /// `crate::serve` so the frontend can fill
    /// `MarketViewEntry.keeperState` per market without
    /// Prometheus-grammar parsing.
    ///
    /// JSON-escaping for the symbol: we strip `"` and `\` and
    /// any control char (the wave-18 registry already validates
    /// symbols on load via `MarketEntry::symbol_bytes`, so this
    /// is a defence-in-depth pass — no real symbol will ever
    /// trigger it).
    pub fn render_per_market_json(&self) -> String {
        let mut out = String::with_capacity(self.slots.len() * 512);
        out.push('[');
        for (idx, slot) in self.slots.iter().enumerate() {
            if idx > 0 {
                out.push(',');
            }
            out.push_str("{\"market\":\"");
            for ch in slot.label.chars() {
                if ch == '"' || ch == '\\' || (ch as u32) < 0x20 {
                    continue;
                }
                out.push(ch);
            }
            out.push_str("\",\"metrics\":");
            out.push_str(&slot.metrics.render_json_snapshot());
            out.push('}');
        }
        out.push(']');
        out
    }

    /// Wave 18 — build a runtime registry from a config-side
    /// [`keeper_rpc::MarketRegistry`] (TOML-loaded).
    ///
    /// The config registry holds *static* per-market data (PDAs +
    /// `expected_leader`); this builder fans it out into
    /// [`MarketSlot`]s by calling the user-provided `make_slot`
    /// closure once per entry. The closure decides how to assemble
    /// the live state (`KeeperBot` config, `KeeperMetrics`,
    /// `LeaderRpcReconcileConfig` cadence, `HostMirrorLeaderPolicy`
    /// initial state, the per-market `MarketContext::sub_pools` set
    /// — which we deliberately don't try to derive automatically
    /// because it depends on the deployment's expected sub-pool
    /// fan-out).
    ///
    /// On any duplicate label or per-entry build error the function
    /// returns the failure as `String` so callers can wrap into
    /// their own error type.
    pub fn from_config_with<F>(
        cfg: &keeper_rpc::MarketRegistry,
        mut make_slot: F,
    ) -> Result<Self, String>
    where
        F: FnMut(&keeper_rpc::MarketEntry) -> Result<MarketSlot, String>,
    {
        let mut out = Self::new();
        for entry in cfg.iter() {
            let slot = make_slot(entry).map_err(|e| {
                format!(
                    "build slot for market `{}` failed: {}",
                    entry.symbol, e
                )
            })?;
            out.add(slot)
                .map_err(|e| format!("registry add failed for `{}`: {}", entry.symbol, e))?;
        }
        Ok(out)
    }
}

/// Wave 18 — multi-market run-loop config.
///
/// `tick_interval` is the wall-clock cadence applied AFTER one
/// pass through the entire registry, so a 4-market deployment
/// at `tick_interval = 800ms` ticks each market every 800ms (not
/// every 200ms). For tighter per-market cadence, lower
/// `tick_interval` linearly with the market count.
#[derive(Debug, Clone, Copy)]
pub struct MultiMarketRunConfig {
    /// Wall-clock interval between full registry passes.
    pub tick_interval: Duration,
    /// Optional cap on the number of completed registry passes.
    /// `None` runs until shutdown.
    pub max_passes: Option<u64>,
    /// Backoff applied when a slot's snapshot refresh hits a
    /// transient error. Mirrors the single-market default 200ms.
    pub transient_error_backoff: Duration,
}

impl Default for MultiMarketRunConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_millis(800),
            max_passes: None,
            transient_error_backoff: Duration::from_millis(200),
        }
    }
}

impl From<RunLoopConfig> for MultiMarketRunConfig {
    fn from(c: RunLoopConfig) -> Self {
        Self {
            tick_interval: c.tick_interval,
            max_passes: c.max_ticks,
            transient_error_backoff: c.transient_error_backoff,
        }
    }
}

/// Per-market terminal outcome. The multi-market loop returns one
/// of these per slot in the [`MultiMarketLoopOutcome::per_market`]
/// vector.
#[derive(Debug)]
pub struct PerMarketOutcome {
    /// Stable label (the same one in `MarketSlot::label`).
    pub label: String,
    /// Tick count this slot completed before terminating.
    pub ticks: u64,
    /// Why the slot terminated.
    pub reason: LoopTerminationReason,
    /// Permanent error (only `Some` when reason == `PermanentError`).
    pub error: Option<BotError>,
}

/// Aggregated terminal outcome from the multi-market loop. The
/// `reason` is the worst (most-severe) reason across the registry,
/// so a 4-market run where one slot panics on a schema mismatch
/// still surfaces `PermanentError` to the caller (which can then
/// inspect `per_market` for which one).
#[derive(Debug)]
pub struct MultiMarketLoopOutcome {
    /// Total registry passes the outer loop completed.
    pub passes: u64,
    /// Worst outcome across slots; matches the wave-12 single-
    /// market `LoopOutcome::reason` semantics.
    pub reason: LoopTerminationReason,
    /// Per-slot breakdown.
    pub per_market: Vec<PerMarketOutcome>,
}

/// Wave 18 — multi-market run loop.
///
/// Drives every registered market through the wave-16 single-
/// snapshot tick + reconcile + heartbeat + tick pipeline once per
/// outer pass. Shutdown triggers wave-17 graceful release on every
/// slot still holding leadership.
///
/// `max_passes` caps the OUTER loop count, not the per-slot tick
/// count; integration tests use this for bounded runtime.
#[allow(clippy::too_many_arguments)]
pub fn run_loop_multi_market_leader_and_rpc_reconcile<F, B, MakeB, L>(
    registry: &mut MarketRegistry,
    fetcher: &F,
    mut factory: MakeB,
    keeper_pk: Pubkey32,
    clock_sysvar: Pubkey32,
    system_program: Pubkey32,
    shutdown: &AtomicBool,
    cfg: MultiMarketRunConfig,
    leader_builder: &mut L,
) -> MultiMarketLoopOutcome
where
    F: AccountFetcher,
    B: TxBuilder,
    MakeB: FnMut() -> B,
    L: KeeperLeaderTxBuilder + ?Sized,
{
    let mut passes: u64 = 0;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            // Wave 17/18 — graceful release per still-leading slot.
            // Ordering matches the registry insertion order so logs
            // are deterministic.
            for slot in registry.slots_mut().iter_mut() {
                if slot.state.finished.is_some() {
                    continue;
                }
                match try_graceful_release(
                    &slot.rpc_cfg,
                    leader_builder,
                    slot.ctx.program_id,
                    slot.ctx.market,
                    keeper_pk,
                    slot.state.was_leader_last_tick,
                ) {
                    Ok(Some(sig)) => tracing::info!(
                        market = %slot.label,
                        ticks = slot.state.ticks,
                        sig = %sig,
                        "graceful keeper_leader_release submitted on shutdown (multi-market)",
                    ),
                    Ok(None) => tracing::info!(
                        market = %slot.label,
                        ticks = slot.state.ticks,
                        "shutdown — graceful release skipped (multi-market)",
                    ),
                    Err(e) => tracing::warn!(
                        market = %slot.label,
                        ticks = slot.state.ticks,
                        error = %e,
                        "graceful keeper_leader_release submit failed on shutdown (multi-market)",
                    ),
                }
                slot.state.finished = Some(LoopOutcome {
                    reason: LoopTerminationReason::ShutdownSignal,
                    ticks: slot.state.ticks,
                    error: None,
                });
            }
            break;
        }
        if let Some(max) = cfg.max_passes {
            if passes >= max {
                // Mark every still-running slot as TickLimitReached.
                for slot in registry.slots_mut().iter_mut() {
                    if slot.state.finished.is_none() {
                        slot.state.finished = Some(LoopOutcome {
                            reason: LoopTerminationReason::TickLimitReached,
                            ticks: slot.state.ticks,
                            error: None,
                        });
                    }
                }
                break;
            }
        }

        // Walk every market once.
        let mut all_finished = true;
        for slot in registry.slots_mut().iter_mut() {
            if slot.state.finished.is_some() {
                continue;
            }
            all_finished = false;
            if let Some(outcome) = tick_one_slot::<F, B, MakeB, L>(
                slot,
                fetcher,
                &mut factory,
                keeper_pk,
                clock_sysvar,
                system_program,
                cfg.transient_error_backoff,
                leader_builder,
            ) {
                slot.state.finished = Some(outcome);
            }
        }
        passes += 1;
        if all_finished {
            // Every slot is done; nothing left to do.
            break;
        }
        std::thread::sleep(cfg.tick_interval);
    }

    // Build the aggregated outcome. We move `finished` out of every
    // slot so the registry can be reused after the loop returns
    // (e.g. for one more `render_prometheus_all` snapshot).
    let mut per_market = Vec::with_capacity(registry.slots.len());
    let mut worst_reason = LoopTerminationReason::ShutdownSignal;
    for slot in registry.slots_mut().iter_mut() {
        let outcome = slot.state.finished.take().unwrap_or(LoopOutcome {
            reason: LoopTerminationReason::ShutdownSignal,
            ticks: slot.state.ticks,
            error: None,
        });
        worst_reason = promote_worst(worst_reason, outcome.reason);
        per_market.push(PerMarketOutcome {
            label: slot.label.clone(),
            ticks: outcome.ticks,
            reason: outcome.reason,
            error: outcome.error,
        });
    }
    MultiMarketLoopOutcome {
        passes,
        reason: worst_reason,
        per_market,
    }
}

/// Run ONE tick step against a single slot. Returns `Some(LoopOutcome)`
/// when the slot terminates; `None` when the slot is still healthy and
/// will tick again on the next registry pass.
#[allow(clippy::too_many_arguments)]
fn tick_one_slot<F, B, MakeB, L>(
    slot: &mut MarketSlot,
    fetcher: &F,
    factory: &mut MakeB,
    keeper_pk: Pubkey32,
    clock_sysvar: Pubkey32,
    system_program: Pubkey32,
    transient_error_backoff: Duration,
    leader_builder: &mut L,
) -> Option<LoopOutcome>
where
    F: AccountFetcher,
    B: TxBuilder,
    MakeB: FnMut() -> B,
    L: KeeperLeaderTxBuilder + ?Sized,
{
    let label = slot.label.clone();
    let ticks = slot.state.ticks;

    // 1. Snapshot.
    let mut snap = ChainSnapshot::new();
    if let Err(e) = snap.refresh(fetcher, &slot.ctx, SnapshotConfig::default()) {
        let bot_err = BotError::Snapshot(e);
        slot.metrics.observe_snapshot_error();
        if is_transient(&bot_err) {
            tracing::warn!(
                market = %label,
                error = %bot_err,
                "transient snapshot error during multi-market tick",
            );
            std::thread::sleep(transient_error_backoff);
            return None;
        }
        tracing::error!(
            market = %label,
            error = %bot_err,
            "permanent snapshot error during multi-market tick",
        );
        return Some(LoopOutcome {
            reason: LoopTerminationReason::PermanentError,
            ticks,
            error: Some(bot_err),
        });
    }

    // 2. Reconcile cadence.
    if let Some(every) = slot.rpc_cfg.reconcile_every {
        let due = match slot.state.last_reconcile_tick {
            None => true,
            Some(prev) => ticks.saturating_sub(prev) >= every,
        };
        if due {
            match fetch_keeper_leader_lock(fetcher, &slot.rpc_cfg.lock_pda) {
                Ok(chain_lock) => {
                    slot.policy.reconcile(chain_lock);
                    slot.state.last_reconcile_tick = Some(ticks);
                    tracing::debug!(
                        market = %label,
                        tick = ticks,
                        "leader-lock reconcile ok (multi-market)",
                    );
                }
                Err(LeaderReconcileError::NotFound(_)) => {
                    tracing::error!(
                        market = %label,
                        tick = ticks,
                        "keeper-leader-lock PDA not initialised on chain — \
                         ops must send `initialize_keeper_leader_lock` (see runbook KL-01)",
                    );
                    slot.state.last_reconcile_tick = Some(ticks);
                }
                Err(e) => {
                    tracing::warn!(
                        market = %label,
                        tick = ticks,
                        error = %e,
                        "leader-lock reconcile failed; falling back to cached mirror",
                    );
                }
            }
        }
    }

    // 3. Leader gate.
    let current_slot = snapshot_best_slot(&snap).unwrap_or(0);
    let should_submit = slot.policy.should_submit(current_slot);
    if !should_submit {
        slot.metrics.set_leader_status(LeaderStatus::Standby);
        tracing::info!(
            market = %label,
            tick = ticks,
            current_slot,
            "leader-lock gate denied — skipping dispatch this tick (multi-market)",
        );
        slot.state.was_leader_last_tick = false;
        slot.state.ticks += 1;
        return None;
    }
    slot.metrics.set_leader_status(LeaderStatus::Leader);

    // 4. Heartbeat cadence.
    if let Some(every) = slot.rpc_cfg.heartbeat_every {
        let just_became_leader = !slot.state.was_leader_last_tick;
        let due = match slot.state.last_heartbeat_tick {
            None => true,
            Some(prev) => ticks.saturating_sub(prev) >= every,
        };
        if just_became_leader || due {
            let ix = build_keeper_leader_heartbeat(
                slot.ctx.program_id,
                slot.ctx.market,
                slot.rpc_cfg.lock_pda,
                keeper_pk,
                KeeperLeaderHeartbeatArgs {
                    observed_slot: current_slot,
                },
            );
            match leader_builder.submit_leader_ix(ix) {
                Ok(sig) => {
                    slot.state.last_heartbeat_tick = Some(ticks);
                    tracing::info!(
                        market = %label,
                        tick = ticks,
                        current_slot,
                        ?sig,
                        "keeper_leader_heartbeat submitted (multi-market)",
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        market = %label,
                        tick = ticks,
                        error = %e,
                        "keeper_leader_heartbeat submit failed (multi-market; will retry next cadence)",
                    );
                }
            }
        }
    }
    slot.state.was_leader_last_tick = true;

    // 5. Tick.
    let builder = factory();
    let start = Instant::now();
    let result = slot.bot.tick_with_snap::<B>(
        &snap,
        &slot.ctx,
        builder,
        keeper_pk,
        clock_sysvar,
        system_program,
    );
    match result {
        Ok((report, _builder)) => {
            let duration_ms = clamp_ms_pub(start.elapsed());
            slot.metrics.observe_tick(&report, duration_ms);
            slot.metrics
                .set_vol_samples(slot.bot.vol().sample_count() as u64);
            tracing::info!(
                market = %label,
                tick = ticks,
                actions_planned = report.actions_planned,
                init_hints_added = report.init_hints_added,
                applied_vol = ?report.applied_vol,
                duration_ms,
                leader_slot = current_slot,
                "multi-market tick complete (leader, rpc-reconciled)",
            );
            slot.state.ticks += 1;
            None
        }
        Err(e) => {
            slot.metrics.observe_snapshot_error();
            tracing::error!(
                market = %label,
                error = %e,
                "permanent error from tick_with_snap (multi-market)",
            );
            Some(LoopOutcome {
                reason: LoopTerminationReason::PermanentError,
                ticks,
                error: Some(e),
            })
        }
    }
}

/// Severity ordering: PermanentError > ShutdownSignal > TickLimitReached.
/// (Permanent errors are the loudest signal; shutdown is operator-
/// triggered; tick limit is normal end-of-test.)
fn promote_worst(
    current: LoopTerminationReason,
    candidate: LoopTerminationReason,
) -> LoopTerminationReason {
    fn rank(r: LoopTerminationReason) -> u8 {
        match r {
            LoopTerminationReason::PermanentError => 3,
            LoopTerminationReason::ShutdownSignal => 2,
            LoopTerminationReason::TickLimitReached => 1,
        }
    }
    if rank(candidate) > rank(current) {
        candidate
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BotConfig;

    fn dummy_ctx(market: Pubkey32) -> MarketContext {
        MarketContext {
            program_id: [0xa1; 32],
            market,
            market_symbol: [0u8; 16],
            sub_pools: vec![],
        }
    }

    /// Wave 18 — `MarketRegistry::from_config_with` fans out a
    /// TOML-loaded `keeper_rpc::MarketRegistry` into a runtime
    /// `keeper_bot::MarketRegistry` of `MarketSlot`s. Verifies:
    /// - one slot per config entry,
    /// - per-slot `MarketContext` carries the right market PDA,
    /// - per-slot `LeaderRpcReconcileConfig.lock_pda` matches the
    ///   config's pre-derived `lock_pda`,
    /// - duplicate-label rejection still fires after the bridge
    ///   (the inner closure happens to return the same label twice).
    #[test]
    fn from_config_with_fans_out_slots() {
        const PUBKEY_A: &str = "11111111111111111111111111111112";
        const PUBKEY_B: &str = "Sysvar1nstructions1111111111111111111111111";
        const PUBKEY_C: &str = "SysvarC1ock11111111111111111111111111111111";
        let toml = format!(
            "[[markets]]\nsymbol = \"SOL-USD\"\nprogram_id = \"{a}\"\nmarket_pda = \"{b}\"\nlock_pda = \"{c}\"\n[[markets]]\nsymbol = \"BTC-USD\"\nprogram_id = \"{a}\"\nmarket_pda = \"{c}\"\nlock_pda = \"{b}\"\n",
            a = PUBKEY_A,
            b = PUBKEY_B,
            c = PUBKEY_C,
        );
        let cfg = keeper_rpc::MarketRegistry::from_toml_str(&toml).expect("parse");
        let runtime = MarketRegistry::from_config_with(&cfg, |entry| {
            Ok(MarketSlot {
                label: entry.symbol.clone(),
                ctx: dummy_ctx(entry.market_pda),
                rpc_cfg: LeaderRpcReconcileConfig {
                    reconcile_every: Some(1),
                    lock_pda: entry.lock_pda,
                    heartbeat_every: Some(5),
                    release_on_shutdown: true,
                },
                bot: KeeperBot::new(BotConfig::default()),
                metrics: KeeperMetrics::new(),
                policy: HostMirrorLeaderPolicy::new(
                    [0u8; 32],
                    keeper_decoder::leader_lock::KeeperLeaderLock::fresh(0, 75),
                ),
                state: MarketSlotState::default(),
            })
        })
        .expect("bridge ok");
        assert_eq!(runtime.len(), 2);
        assert_eq!(runtime.slots()[0].label, "SOL-USD");
        assert_eq!(runtime.slots()[1].label, "BTC-USD");
        assert_eq!(runtime.slots()[0].ctx.market, runtime.slots()[1].rpc_cfg.lock_pda);
        // Per-slot lock_pda comes from the config, not the snapshot.
        assert_eq!(
            runtime.slots()[0].rpc_cfg.lock_pda,
            cfg.markets[0].lock_pda
        );
    }

    #[test]
    fn from_config_with_propagates_closure_errors() {
        const PUBKEY_A: &str = "11111111111111111111111111111112";
        const PUBKEY_B: &str = "Sysvar1nstructions1111111111111111111111111";
        const PUBKEY_C: &str = "SysvarC1ock11111111111111111111111111111111";
        let toml = format!(
            "[[markets]]\nsymbol = \"SOL-USD\"\nprogram_id = \"{a}\"\nmarket_pda = \"{b}\"\nlock_pda = \"{c}\"\n",
            a = PUBKEY_A,
            b = PUBKEY_B,
            c = PUBKEY_C,
        );
        let cfg = keeper_rpc::MarketRegistry::from_toml_str(&toml).expect("parse");
        let result = MarketRegistry::from_config_with(&cfg, |_| {
            Err::<MarketSlot, _>("simulated rpc unreachable".to_string())
        });
        // Result has no Debug for the Ok variant (MarketRegistry no
        // Debug); pattern-match instead of `expect_err`.
        match result {
            Err(err) => {
                assert!(err.contains("SOL-USD"), "{err}");
                assert!(err.contains("simulated rpc unreachable"), "{err}");
            }
            Ok(_) => panic!("expected Err but got Ok"),
        }
    }

    // ----------------------------------------------------------------
    // Wave 21 — per-market JSON metrics renderer
    // ----------------------------------------------------------------

    fn build_two_slot_registry() -> MarketRegistry {
        const PUBKEY_A: &str = "11111111111111111111111111111112";
        const PUBKEY_B: &str = "Sysvar1nstructions1111111111111111111111111";
        const PUBKEY_C: &str = "SysvarC1ock11111111111111111111111111111111";
        let toml = format!(
            "[[markets]]\nsymbol = \"SOL-USD\"\nprogram_id = \"{a}\"\nmarket_pda = \"{b}\"\nlock_pda = \"{c}\"\n[[markets]]\nsymbol = \"BTC-USD\"\nprogram_id = \"{a}\"\nmarket_pda = \"{c}\"\nlock_pda = \"{b}\"\n",
            a = PUBKEY_A,
            b = PUBKEY_B,
            c = PUBKEY_C,
        );
        let cfg = keeper_rpc::MarketRegistry::from_toml_str(&toml).expect("parse");
        MarketRegistry::from_config_with(&cfg, |entry| {
            Ok(MarketSlot {
                label: entry.symbol.clone(),
                ctx: dummy_ctx(entry.market_pda),
                rpc_cfg: LeaderRpcReconcileConfig {
                    reconcile_every: Some(1),
                    lock_pda: entry.lock_pda,
                    heartbeat_every: Some(5),
                    release_on_shutdown: true,
                },
                bot: KeeperBot::new(BotConfig::default()),
                metrics: KeeperMetrics::new(),
                policy: HostMirrorLeaderPolicy::new(
                    [0u8; 32],
                    keeper_decoder::leader_lock::KeeperLeaderLock::fresh(0, 75),
                ),
                state: MarketSlotState::default(),
            })
        })
        .expect("bridge")
    }

    #[test]
    fn render_per_market_json_emits_array_with_one_element_per_slot() {
        let reg = build_two_slot_registry();
        let json = reg.render_per_market_json();
        assert!(json.starts_with('[') && json.ends_with(']'));
        assert!(json.contains("\"market\":\"SOL-USD\""));
        assert!(json.contains("\"market\":\"BTC-USD\""));
        // Two `metrics` objects (one per market).
        assert_eq!(
            json.matches("\"metrics\":").count(),
            2,
            "expected one metrics object per market"
        );
    }

    #[test]
    fn render_per_market_json_records_per_slot_state_independently() {
        let mut reg = build_two_slot_registry();
        reg.slots_mut()[0].metrics.observe_boot(1_700_000_001);
        reg.slots_mut()[0]
            .metrics
            .set_leader_status(LeaderStatus::Leader);
        reg.slots_mut()[1].metrics.observe_boot(1_700_000_002);
        reg.slots_mut()[1]
            .metrics
            .set_leader_status(LeaderStatus::Standby);
        let json = reg.render_per_market_json();
        // Slot 0 is leader at boot=…001
        let sol_idx = json.find("SOL-USD").unwrap();
        let btc_idx = json.find("BTC-USD").unwrap();
        let sol = &json[sol_idx..btc_idx];
        let btc = &json[btc_idx..];
        assert!(
            sol.contains("\"leaderStatus\":\"leader\""),
            "SOL-USD should be leader: {sol}"
        );
        assert!(
            btc.contains("\"leaderStatus\":\"standby\""),
            "BTC-USD should be standby: {btc}"
        );
        assert!(sol.contains("\"upSinceUnixSecs\":1700000001"));
        assert!(btc.contains("\"upSinceUnixSecs\":1700000002"));
    }

    #[test]
    fn render_per_market_json_handles_empty_registry() {
        let reg = MarketRegistry::new();
        assert_eq!(reg.render_per_market_json(), "[]");
    }
}
