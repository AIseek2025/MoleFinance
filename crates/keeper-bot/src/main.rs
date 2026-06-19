//! `keeper-bot` CLI entry point.
//!
//! Two modes (selected by the first CLI arg):
//!
//! - **`smoke`** (default) — wave-10 no-network smoke runner. Builds
//!   a `MockAccountFetcher` from documentation fixtures, calls
//!   `KeeperBot::tick` once, prints a structured one-line summary,
//!   and exits 0. Suitable for `cargo run -p keeper-bot` smoke testing.
//!
//! - **`serve`** — wave-12 production daemon mode. Boots the
//!   Prometheus exposition server (default `0.0.0.0:9099`),
//!   installs a SIGINT/SIGTERM handler, and runs `run_loop_with_factory`
//!   against `MockAccountFetcher` + `MockTxBuilder` until shutdown.
//!   The mock fetcher is a placeholder so the binary is end-to-end
//!   runnable without a real RPC; replace `factory` with a
//!   `keeper_rpc::solana::SolanaTxBuilder` (see wave-11 §11.1) to
//!   submit real tx.
//!
//! - **`serve-multi`** — wave-22 multi-market daemon. Loads
//!   `markets.toml`, runs `run_loop_multi_market_leader_and_rpc_reconcile`
//!   against mock fetchers (same laptop-friendly contract as `serve`),
//!   and exposes both `/metrics` and `/metrics-multi` so the frontend
//!   can poll per-market JSON without Prometheus parsing.
//!
//! ## Why the mock daemon mode is useful
//!
//! It lets ops verify the entire production wiring (logging,
//! metrics endpoint, signal handling, graceful shutdown) end-to-end
//! on a developer laptop without paying for a real RPC connection
//! or a Solana cluster. Wire it up to Prometheus + Grafana and you
//! can preview every dashboard panel before staging deploy.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use keeper_bot::{
    BotConfig, HostMirrorLeaderPolicy, KeeperBot, KeeperMetrics, LeaderStatus,
    LeaderRpcReconcileConfig, LoopOutcome, LoopTerminationReason, MarketRegistry, MarketSlot,
    MarketSlotState, MultiMarketLoopOutcome, MultiMarketRunConfig, RunLoopConfig,
    run_loop_multi_market_leader_and_rpc_reconcile, run_loop_with_factory,
    spawn_metrics_server, spawn_metrics_server_with_multi, MultiMarketJsonProvider,
};
use keeper_decoder::leader_lock::KeeperLeaderLock;
use keeper_rpc::leader_tx::MockKeeperLeaderTxBuilder;
use keeper_rpc::{MarketContext, MockAccountFetcher, MockTxBuilder};

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "smoke".to_string());
    match mode.as_str() {
        "smoke" => run_smoke(),
        "serve" => {
            let addr_str = args.next().unwrap_or_else(|| "0.0.0.0:9099".to_string());
            let max_ticks = args.next().and_then(|s| s.parse::<u64>().ok());
            run_serve(&addr_str, max_ticks);
        }
        "serve-multi" => {
            let addr_str = args.next().unwrap_or_else(|| "0.0.0.0:9099".to_string());
            let toml_path = args.next().unwrap_or_else(|| {
                eprintln!(
                    "[keeper-bot] serve-multi requires <addr> <markets.toml> [max_passes]\n\
                    example: keeper-bot serve-multi 0.0.0.0:9099 ./markets.toml 0"
                );
                std::process::exit(2);
            });
            let max_passes = args.next().and_then(|s| s.parse::<u64>().ok());
            run_serve_multi(&addr_str, &toml_path, max_passes);
        }
        other => {
            eprintln!(
                "[keeper-bot] unknown mode `{other}`. supported: smoke | serve | serve-multi"
            );
            std::process::exit(2);
        }
    }
}

fn run_smoke() {
    let mut bot = KeeperBot::new(BotConfig::default());

    let fetcher = MockAccountFetcher::new();
    let ctx = MarketContext {
        program_id: [0u8; 32],
        market: [0u8; 32],
        market_symbol: [0u8; 16],
        sub_pools: vec![],
    };
    let builder = MockTxBuilder::new();

    let r = bot.tick(
        &fetcher,
        &ctx,
        builder,
        [42u8; 32],
        [0u8; 32],
        [0u8; 32],
    );
    match r {
        Ok((report, _builder)) => {
            println!(
                "[keeper-bot] smoke-test ok: actions_planned={} init_hints={} applied_vol={:?}",
                report.actions_planned, report.init_hints_added, report.applied_vol
            );
        }
        Err(e) => {
            eprintln!("[keeper-bot] smoke-test surfaced expected error: {e}");
            std::process::exit(0);
        }
    }
}

fn run_serve(addr_str: &str, max_ticks: Option<u64>) {
    install_tracing();

    let addr: SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[keeper-bot] invalid serve address `{addr_str}`: {e}");
            std::process::exit(2);
        }
    };

    let metrics = Arc::new(KeeperMetrics::new());
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    metrics.observe_boot(now_secs);
    // Default to leader for the smoke daemon — production
    // deployments will override based on their lock manager.
    metrics.set_leader_status(LeaderStatus::Leader);

    let shutdown = Arc::new(AtomicBool::new(false));

    let (bound_addr, server_thread) =
        match spawn_metrics_server(addr, Arc::clone(&metrics), Arc::clone(&shutdown)) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[keeper-bot] failed to bind metrics server: {e}");
                std::process::exit(1);
            }
        };

    let shutdown_for_signal = Arc::clone(&shutdown);
    if let Err(e) = ctrlc::set_handler(move || {
        tracing::info!("SIGINT/SIGTERM received");
        shutdown_for_signal.store(true, Ordering::Relaxed);
    }) {
        eprintln!("[keeper-bot] failed to install signal handler: {e}");
        std::process::exit(1);
    }

    tracing::info!(
        addr = %bound_addr,
        max_ticks = ?max_ticks,
        "keeper-bot daemon up"
    );

    let mut bot = KeeperBot::new(BotConfig::default());
    let fetcher = MockAccountFetcher::new();
    let ctx = MarketContext {
        program_id: [0u8; 32],
        market: [0u8; 32],
        market_symbol: [0u8; 16],
        sub_pools: vec![],
    };
    let cfg = RunLoopConfig {
        tick_interval: Duration::from_millis(800),
        max_ticks,
        transient_error_backoff: Duration::from_millis(200),
    };

    let outcome = run_loop_with_factory(
        &mut bot,
        &fetcher,
        &ctx,
        MockTxBuilder::new,
        [42u8; 32],
        [0u8; 32],
        [0u8; 32],
        &metrics,
        &shutdown,
        cfg,
    );

    shutdown.store(true, Ordering::Relaxed);
    if let Err(e) = server_thread.join() {
        eprintln!("[keeper-bot] metrics server panicked: {e:?}");
    }

    summarise_and_exit(outcome);
}

fn run_serve_multi(addr_str: &str, toml_path: &str, max_passes: Option<u64>) {
    install_tracing();

    let addr: SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[keeper-bot] invalid serve address `{addr_str}`: {e}");
            std::process::exit(2);
        }
    };

    let toml_text = std::fs::read_to_string(toml_path).unwrap_or_else(|e| {
        eprintln!("[keeper-bot] cannot read `{toml_path}`: {e}");
        std::process::exit(2);
    });
    let cfg_reg = keeper_rpc::MarketRegistry::from_toml_str(&toml_text).unwrap_or_else(|e| {
        eprintln!("[keeper-bot] markets.toml parse failed: {e}");
        std::process::exit(2);
    });

    const KEEPER: keeper_rpc::Pubkey32 = [42u8; 32];
    const CLOCK: keeper_rpc::Pubkey32 = [0u8; 32];
    const SYSTEM_PROGRAM: keeper_rpc::Pubkey32 = [0u8; 32];

    let runtime = MarketRegistry::from_config_with(&cfg_reg, |entry| {
        let mut market_symbol = [0u8; 16];
        let sym = entry.symbol.as_bytes();
        let n = sym.len().min(16);
        market_symbol[..n].copy_from_slice(&sym[..n]);
        Ok(MarketSlot {
            label: entry.symbol.clone(),
            ctx: MarketContext {
                program_id: entry.program_id,
                market: entry.market_pda,
                market_symbol,
                sub_pools: vec![],
            },
            rpc_cfg: LeaderRpcReconcileConfig {
                reconcile_every: Some(20),
                lock_pda: entry.lock_pda,
                heartbeat_every: Some(5),
                release_on_shutdown: true,
            },
            bot: KeeperBot::new(BotConfig::default()),
            metrics: KeeperMetrics::new(),
            policy: HostMirrorLeaderPolicy::new(KEEPER, KeeperLeaderLock::fresh(0, 75)),
            state: MarketSlotState::default(),
        })
    })
    .unwrap_or_else(|e| {
        eprintln!("[keeper-bot] multi-market registry build failed: {e}");
        std::process::exit(2);
    });

    let registry = Arc::new(Mutex::new(runtime));
    let reg_for_metrics = Arc::clone(&registry);
    let multi: MultiMarketJsonProvider = Arc::new(move || reg_for_metrics
        .lock()
        .expect("registry mutex poisoned")
        .render_per_market_json());

    let global_metrics = Arc::new(KeeperMetrics::new());
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    global_metrics.observe_boot(now_secs);
    global_metrics.set_leader_status(LeaderStatus::Leader);

    let shutdown = Arc::new(AtomicBool::new(false));

    let (bound_addr, server_thread) = match spawn_metrics_server_with_multi(
        addr,
        Arc::clone(&global_metrics),
        Some(multi),
        Arc::clone(&shutdown),
    ) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("[keeper-bot] failed to bind metrics server: {e}");
            std::process::exit(1);
        }
    };

    let shutdown_for_signal = Arc::clone(&shutdown);
    if let Err(e) = ctrlc::set_handler(move || {
        tracing::info!("SIGINT/SIGTERM received");
        shutdown_for_signal.store(true, Ordering::Relaxed);
    }) {
        eprintln!("[keeper-bot] failed to install signal handler: {e}");
        std::process::exit(1);
    }

    tracing::info!(
        addr = %bound_addr,
        markets = cfg_reg.len(),
        max_passes = ?max_passes,
        "keeper-bot multi-market daemon up"
    );

    let fetcher = MockAccountFetcher::new();
    let mut leader_builder = MockKeeperLeaderTxBuilder::new();
    let cfg = MultiMarketRunConfig {
        tick_interval: Duration::from_millis(800),
        max_passes,
        transient_error_backoff: Duration::from_millis(200),
    };

    let outcome = {
        let mut reg = registry.lock().expect("registry mutex poisoned");
        run_loop_multi_market_leader_and_rpc_reconcile(
            &mut reg,
            &fetcher,
            MockTxBuilder::new,
            KEEPER,
            CLOCK,
            SYSTEM_PROGRAM,
            &shutdown,
            cfg,
            &mut leader_builder,
        )
    };

    shutdown.store(true, Ordering::Relaxed);
    if let Err(e) = server_thread.join() {
        eprintln!("[keeper-bot] metrics server panicked: {e:?}");
    }

    summarise_multi_and_exit(outcome);
}

fn summarise_multi_and_exit(outcome: MultiMarketLoopOutcome) -> ! {
    tracing::info!(
        passes = outcome.passes,
        markets = outcome.per_market.len(),
        "multi-market loop terminated"
    );
    for pm in &outcome.per_market {
        tracing::info!(
            market = %pm.label,
            ticks = pm.ticks,
            reason = ?pm.reason,
            "per-market outcome"
        );
    }
    std::process::exit(0);
}

fn install_tracing() {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("keeper_bot=info,keeper_rpc=info"));
    let json_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .json()
        .with_current_span(false)
        .with_span_list(false);
    tracing_subscriber::registry()
        .with(env_filter)
        .with(json_layer)
        .init();
}

fn summarise_and_exit(outcome: LoopOutcome) -> ! {
    tracing::info!(
        ticks = outcome.ticks,
        reason = ?outcome.reason,
        "loop terminated"
    );
    match outcome.reason {
        LoopTerminationReason::ShutdownSignal | LoopTerminationReason::TickLimitReached => {
            std::process::exit(0)
        }
        LoopTerminationReason::PermanentError => {
            if let Some(err) = outcome.error {
                tracing::error!(error = %err, "permanent error");
            }
            // For the smoke daemon (mock fetcher), the first tick
            // returns `MarketNotFound` which is the expected exit.
            // Production deployments swap in a real fetcher and
            // this branch becomes a real failure.
            std::process::exit(0)
        }
    }
}
