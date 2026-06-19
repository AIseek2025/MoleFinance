//! Wave 10 — runnable keeper daemon library.
//!
//! Wires together the four wave-9/10 building blocks:
//!
//! 1. [`ChainSnapshot`] — per-tick chain view
//! 2. [`Scheduler`] — turns the view into prioritised actions
//! 3. [`RotateRiskPredictor`] — feeds the scheduler init hints
//! 4. [`RealizedVolatilityEstimator`] — auto-tunes the predictor
//!    from the same prices the snapshot reads
//! 5. [`RpcExecutor`] — submits actions over the configured `TxBuilder`
//!
//! The crate provides a [`KeeperBot`] struct exposing one `tick`
//! method; production deployments wrap that in their preferred
//! event-loop (tokio interval, supervisord, k8s liveness, …).
//!
//! ## Why this lives in a separate crate
//!
//! - Keeps the per-component crates pure (no event-loop churn in
//!   `keeper`, no Solana awareness in `clearing-core`).
//! - Makes the bot itself testable end-to-end with `chain-mirror`
//!   under default features, no Solana RPC required.
//! - Lets a hosted deployment run the bot from the CLI binary
//!   (`crates/keeper-bot/src/main.rs`), while integrators embed
//!   the same bot in custom infra by depending on this crate.

#![deny(missing_docs)]

pub mod leader;
pub mod metrics;
pub mod multi;
pub mod run;
pub mod serve;

pub use leader::{FixedLeaderPolicy, HostMirrorLeaderPolicy, LeaderPolicy};
pub use metrics::{KeeperMetrics, LeaderStatus};
pub use multi::{
    MarketRegistry, MarketSlot, MarketSlotState, MultiMarketLoopOutcome, MultiMarketRunConfig,
    PerMarketOutcome, run_loop_multi_market_leader_and_rpc_reconcile,
};
pub use run::{
    LeaderRpcReconcileConfig, LoopOutcome, LoopTerminationReason, RunLoopConfig, is_transient,
    run_loop, run_loop_with_factory, run_loop_with_leader,
    run_loop_with_leader_and_rpc_reconcile, try_graceful_release,
};
pub use serve::{
    render_response, spawn_metrics_server, spawn_metrics_server_with_multi,
    MultiMarketJsonProvider,
};

use keeper::{
    ActionDispatchResult, ActionExecutor, KeeperAction, PredictorConfig, PriceSample,
    RealizedVolatilityEstimator, RotateRiskPredictor, Scheduler, SubPoolHealth,
};
use keeper_rpc::{
    AccountFetcher, ChainSnapshot, MarketContext, RpcExecutor, SnapshotConfig, SnapshotError,
    TxBuilder,
};

/// Bot-level errors.
#[derive(Debug, thiserror::Error)]
pub enum BotError {
    /// RPC fetch / decode error.
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    /// Scheduler returned an invariant violation.
    #[error(transparent)]
    Scheduler(#[from] keeper::KeeperError),
}

/// Configuration for one [`KeeperBot::tick`] cycle.
#[derive(Debug, Clone)]
pub struct BotConfig {
    /// Snapshot tunables forwarded to [`ChainSnapshot::refresh`].
    pub snapshot: SnapshotConfig,
    /// Predictor tunables. The bot overrides
    /// `predictor.annual_vol` from
    /// [`RealizedVolatilityEstimator::current_estimate`] each tick.
    pub predictor: PredictorConfig,
    /// Whether to feed the [`RotateRiskPredictor`] this tick. Set
    /// to `false` to run the bot in pure pre-sync / close mode
    /// (e.g. while validating wave-9 governance changes in prod).
    pub run_predictor: bool,
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            snapshot: SnapshotConfig::default(),
            predictor: PredictorConfig::default(),
            run_predictor: true,
        }
    }
}

/// One cycle's execution report.
#[derive(Debug, Clone)]
pub struct TickReport {
    /// Total actions emitted by the scheduler.
    pub actions_planned: usize,
    /// Per-action dispatch result, in order.
    pub dispatched: Vec<(KeeperAction, ActionDispatchResult)>,
    /// Realised vol applied to the predictor this tick (if warm).
    pub applied_vol: Option<f64>,
    /// Number of init-hints surfaced by the predictor this tick.
    pub init_hints_added: usize,
}

/// A long-lived keeper daemon. Owns the scheduler / predictor /
/// vol-estimator state across ticks; the snapshot + chain view are
/// rebuilt every tick from a fresh `AccountFetcher` query.
pub struct KeeperBot {
    config: BotConfig,
    scheduler: Scheduler,
    predictor: RotateRiskPredictor,
    vol: RealizedVolatilityEstimator,
}

impl KeeperBot {
    /// Construct with the given configuration.
    pub fn new(config: BotConfig) -> Self {
        let predictor = RotateRiskPredictor::new(config.predictor);
        Self {
            config,
            scheduler: Scheduler::default(),
            predictor,
            vol: RealizedVolatilityEstimator::default(),
        }
    }

    /// Read-only borrow of the wrapped scheduler.
    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    /// Read-only borrow of the wrapped vol estimator. Lets the bot
    /// host expose `σ̂` to monitoring without surfacing the whole
    /// internal state.
    pub fn vol(&self) -> &RealizedVolatilityEstimator {
        &self.vol
    }

    /// Read-only borrow of the predictor.
    pub fn predictor(&self) -> &RotateRiskPredictor {
        &self.predictor
    }

    /// One scheduling tick.
    ///
    /// 1. `snapshot.refresh(fetcher, ctx, snapshot_cfg)`
    /// 2. For every sub-pool, record the `(price, slot)` pair into
    ///    the vol estimator.
    /// 3. Apply the estimator to the predictor's `annual_vol`.
    /// 4. (When `run_predictor`) for every sub-pool with a health
    ///    snapshot, run the predictor and forward init hints to the
    ///    scheduler.
    /// 5. `scheduler.plan(view)` → action queue.
    /// 6. Dispatch each action through the executor and aggregate
    ///    results into a [`TickReport`].
    ///
    /// Wave 16: this is a thin wrapper around
    /// [`KeeperBot::tick_with_snap`] that owns the snapshot refresh.
    /// Production callers that already hold a fresh snapshot (e.g.
    /// the leader-gated run-loop) should use `tick_with_snap`
    /// directly to avoid a double RPC round-trip.
    pub fn tick<F: AccountFetcher, B: TxBuilder>(
        &mut self,
        fetcher: &F,
        ctx: &MarketContext,
        builder: B,
        keeper_pubkey: keeper_rpc::Pubkey32,
        clock_sysvar: keeper_rpc::Pubkey32,
        system_program: keeper_rpc::Pubkey32,
    ) -> Result<(TickReport, B), BotError> {
        let mut snap = ChainSnapshot::new();
        snap.refresh(fetcher, ctx, self.config.snapshot)?;
        self.tick_with_snap::<B>(&snap, ctx, builder, keeper_pubkey, clock_sysvar, system_program)
    }

    /// Wave 16 — single-refresh tick. Caller is responsible for
    /// having already refreshed `snap` (which lets the wave-15
    /// `run_loop_with_leader` consult the leader gate using the
    /// same snapshot the executor will operate on, instead of
    /// burning a second RPC round-trip).
    ///
    /// The host-side semantics are byte-identical to [`tick`]:
    /// volatility estimation, predictor warm-up, scheduler plan,
    /// per-action dispatch, and `TickReport` aggregation are all
    /// driven from `snap`. The only difference is who owns the
    /// snapshot lifetime.
    pub fn tick_with_snap<B: TxBuilder>(
        &mut self,
        snap: &ChainSnapshot,
        ctx: &MarketContext,
        builder: B,
        keeper_pubkey: keeper_rpc::Pubkey32,
        clock_sysvar: keeper_rpc::Pubkey32,
        system_program: keeper_rpc::Pubkey32,
    ) -> Result<(TickReport, B), BotError> {
        // Record every active sub-pool's last price into the vol
        // estimator. Each (price, slot) pair is independent across
        // sub-pools, so we just feed them sequentially. The
        // estimator's out-of-order guard handles concurrent feeds.
        for entry in &ctx.sub_pools {
            if let Some(sp) = snap.sub_pools.get(&entry.sub_pool_id) {
                self.vol.record(PriceSample {
                    price: sp.last_price,
                    slot: sp.last_sync_slot,
                });
            }
        }

        // Apply realised vol to the predictor.
        let mut effective_predictor_cfg = self.config.predictor;
        let applied = if self.vol.apply_to_predictor_config(&mut effective_predictor_cfg) {
            Some(effective_predictor_cfg.annual_vol)
        } else {
            None
        };
        // Re-build the predictor with the effective config for this
        // tick. Cheap — the predictor is a small struct.
        self.predictor = RotateRiskPredictor::new(effective_predictor_cfg);

        // Run predictor → record init hints. `populate_scheduler`
        // takes the full chain view in one go and walks every
        // sub-pool, so we call it once with the snapshot rather
        // than once per sub-pool. Returns the predictions made
        // this tick (for monitoring); the scheduler-side state is
        // recorded internally via `record_init_hint`.
        let init_hints_added = if self.config.run_predictor {
            self.predictor
                .populate_scheduler(snap, &mut self.scheduler)
                .len()
        } else {
            0
        };

        let actions = self.scheduler.plan(snap)?;
        let actions_planned = actions.len();

        // Build executor + dispatch each action.
        let mut exec = RpcExecutor::new(
            ctx.program_id,
            ctx.market,
            keeper_pubkey,
            clock_sysvar,
            system_program,
            snap,
            builder,
        );
        let mut dispatched = Vec::with_capacity(actions_planned);
        for action in actions {
            let r = exec.execute(action);
            dispatched.push((action, r));
        }

        Ok((
            TickReport {
                actions_planned,
                dispatched,
                applied_vol: applied,
                init_hints_added,
            },
            exec.builder,
        ))
    }
}

/// Wave 15 — extract the bot's best-known cluster slot from a fresh
/// snapshot. We use `max(sub_pool.last_sync_slot)` over all observed
/// sub-pools as a conservative proxy for "current cluster slot". The
/// on-chain `keeper_leader_heartbeat` ix accepts any
/// `observed_slot ≥ recorded slot`; using the freshest per-tick
/// snapshot gives us a value that's never further behind than one
/// tick. Production deployments may instead query `getSlot` directly
/// for tighter freshness; the host-mirror policy is happy either way.
///
/// Returns `None` if the snapshot is empty (no sub-pools active yet
/// — first boot, before any pre-sync). Callers should skip the
/// leader-lock gate in that case.
pub fn snapshot_best_slot(snap: &keeper_rpc::ChainSnapshot) -> Option<u64> {
    snap.sub_pools.values().map(|sp| sp.last_sync_slot).max()
}

/// Helper that materialises a [`SubPoolHealth`] from raw on-chain
/// `SubPool` fields. Useful for tests + bot consumers that want to
/// peek the predictor without wiring a full snapshot. Direction-
/// specific anchor-price overrides default to `last_price` since
/// the on-chain account doesn't carry per-direction anchors directly.
#[allow(clippy::too_many_arguments)]
pub fn health_from_sub_pool(
    sub_pool_id: u32,
    last_price: u64,
    long_pool_equity: u128,
    short_pool_equity: u128,
    long_active_notional: u128,
    short_active_notional: u128,
    long_active_generation: u64,
    short_active_generation: u64,
) -> SubPoolHealth {
    SubPoolHealth {
        sub_pool_id,
        last_price,
        long_anchor_price: last_price,
        short_anchor_price: last_price,
        long_pool_equity,
        short_pool_equity,
        long_active_notional,
        short_active_notional,
        long_active_generation,
        short_active_generation,
    }
}

/// Re-export the engine-side `Direction` so embedders don't have to
/// add a separate `clearing-core` dependency just for the enum.
pub use clearing_core::Direction as EngineDirection;
/// Re-export `ActionDispatchResult` so consumers can pattern-match
/// without depending on `keeper` directly.
pub use keeper::ActionDispatchResult as DispatchResult;

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensure the `health_from_sub_pool` helper preserves the
    /// invariant the predictor relies on (`anchor == last_price` in
    /// pristine pre-rotate state). The wave-9 predictor has unit
    /// tests that prove this is the right default.
    #[test]
    fn health_helper_defaults_anchor_to_last_price() {
        let h = health_from_sub_pool(0, 100_000_000, 1_000, 1_000, 5_000, 5_000, 0, 0);
        assert_eq!(h.long_anchor_price, 100_000_000);
        assert_eq!(h.short_anchor_price, 100_000_000);
    }

    /// `BotConfig::default` runs the predictor (the common case);
    /// a bot booted with `run_predictor=false` must skip predictor
    /// work entirely. The actual integration-test of `tick` lives
    /// in `tests/keeper_bot_chain_mirror.rs`.
    #[test]
    fn default_config_runs_predictor() {
        let cfg = BotConfig::default();
        assert!(cfg.run_predictor);
    }

    /// Wave 16 — `tick_with_snap` is byte-equivalent to `tick`'s
    /// post-refresh pipeline (it just trusts the caller's already-
    /// refreshed snapshot). For an empty snapshot the scheduler
    /// surfaces no actions and the report is internally consistent.
    /// The full executor + `RpcExecutor` integration paths are
    /// covered by the wave-10 / wave-15 e2e tests in
    /// `tests/end_to_end.rs`.
    #[test]
    fn tick_with_snap_on_empty_snapshot_returns_zero_actions() {
        use keeper_rpc::{ChainSnapshot, MarketContext, MockTxBuilder};

        let mut bot = KeeperBot::new(BotConfig::default());
        let snap = ChainSnapshot::new();
        let ctx = MarketContext {
            program_id: [0u8; 32],
            market: [0u8; 32],
            market_symbol: [0u8; 16],
            sub_pools: vec![],
        };
        let (report, builder) = bot
            .tick_with_snap::<MockTxBuilder>(
                &snap,
                &ctx,
                MockTxBuilder::new(),
                [0u8; 32],
                [0u8; 32],
                [0u8; 32],
            )
            .expect("empty-snap tick succeeds");
        assert_eq!(report.actions_planned, 0);
        assert_eq!(report.dispatched.len(), 0);
        assert_eq!(report.init_hints_added, 0);
        assert!(report.applied_vol.is_none());
        assert_eq!(builder.submitted.len(), 0);
    }

    /// Wave 15 — `snapshot_best_slot` returns the maximum
    /// `last_sync_slot` across the snapshot's sub-pools, or `None`
    /// when the snapshot is empty.
    #[test]
    fn snapshot_best_slot_returns_max_or_none() {
        use keeper_rpc::ChainSnapshot;
        use keeper_rpc::accounts::OnchainSubPool;
        let mut snap = ChainSnapshot::new();
        assert_eq!(snapshot_best_slot(&snap), None);
        let make_sp = |id: u32, slot: u64| OnchainSubPool {
            market: [0u8; 32],
            sub_pool_id: id,
            long_pool_equity: 0,
            short_pool_equity: 0,
            long_active_shares: 0,
            short_active_shares: 0,
            long_recovery_shares: 0,
            short_recovery_shares: 0,
            long_active_notional: 0,
            short_active_notional: 0,
            long_active_generation: 0,
            short_active_generation: 0,
            last_price: 100_000_000,
            last_sync_slot: slot,
            long_dust: 0,
            short_dust: 0,
            long_dormant_bucket_count: 0,
            short_dormant_bucket_count: 0,
            bump: 0,
            _pad: [0u8; 7],
        };
        snap.sub_pools.insert(0, make_sp(0, 100));
        snap.sub_pools.insert(1, make_sp(1, 7_777));
        snap.sub_pools.insert(2, make_sp(2, 42));
        assert_eq!(snapshot_best_slot(&snap), Some(7_777));
    }
}
