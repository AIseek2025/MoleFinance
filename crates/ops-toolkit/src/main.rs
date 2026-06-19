//! `ops-toolkit` CLI entry point.
//!
//! Three modes:
//!
//! - **`demo`** — run all 18 checks against a hard-coded healthy
//!   `HealthContext`. Useful for testing the JSON / Prometheus
//!   wire format without a real prober. Exits 0.
//!
//! - **`demo-broken`** — same as `demo` but flips two checks into
//!   Critical so ops can verify their pager pipeline end-to-end.
//!   Exits 4 (P0 critical).
//!
//! - **`check-stdin`** — read a `HealthContext`-shaped JSON from
//!   stdin (one object on a single line, top-level fields snake_case
//!   matching `context.rs`), run checks, emit JSON to stdout, exit
//!   with the appropriate severity-tier code.
//!
//! For wave 12 we ship `demo` + `demo-broken` end-to-end. The
//! `check-stdin` mode is stubbed: it emits a friendly error pointing
//! to the JSON schema documentation and exits 2. Wave 13 will land
//! a serde-based input parser.

use ops_toolkit::cli_loader::{
    extract_sources, load_registry, read_process_stdin, MarketsSource,
};
use ops_toolkit::{
    HealthContext, KeeperFacts, MarketFacts, OracleFacts, PoolFacts, RpcFacts, SubPoolFacts,
    exit_code_for_status, render_json, render_json_multi, render_prometheus_textfile,
    run_all_checks, scan_all_markets, MarketEntry, MarketFetcher,
    ProberClock, ProberConfig, ProberLoop, ProberSink,
};
use std::time::{Duration, Instant};

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "demo".to_string());
    match mode.as_str() {
        "demo" => run_demo(false),
        "demo-broken" => run_demo(true),
        "scan" => run_scan(),
        "prober" => run_prober(),
        "check-stdin" => {
            eprintln!(
                "[ops-toolkit] check-stdin not yet wired (wave 13). \
                For now use the library API directly: \
                ops_toolkit::run_all_checks(ctx) -> HealthReport."
            );
            std::process::exit(2);
        }
        other => {
            eprintln!(
                "[ops-toolkit] unknown mode `{other}`. \
                supported: demo | demo-broken | scan | prober | check-stdin"
            );
            std::process::exit(2);
        }
    }
}

/// Wave 19 — `prober` mode. Long-running daemon that periodically
/// scans every market in `markets.toml`, writes a Prometheus
/// textfile and a JSON snapshot, and loops.
///
/// Usage:
///
///   ops-toolkit prober <markets.toml> <prom-path> <json-path> \
///                       [interval_secs] [max_cycles]
///
/// `interval_secs` defaults to 10. `max_cycles` defaults to a single
/// cycle (the long-running scenario is satisfied by an external
/// supervisor like `systemd` re-spawning ops-toolkit; that keeps
/// the binary simple and crash-safe).
///
/// Wave 26 — when built `--features solana-rpc` AND `MOLE_OI_RPC_URL`
/// is set, the fixture base fetcher is wrapped in
/// `OpenInterestAugmentingFetcher::with_solana_rpc`, so every cycle
/// scans live `Position` PDAs and folds the per-market notional into
/// the `position_principal_drift` check — the check finally runs on
/// real cluster data. Without the feature/env, the base fetcher
/// produces the `healthy_demo_context()` fixture and the drift check
/// skips (on-chain notional 0), so the metrics pipeline can still be
/// stood up before live data is wired.
fn run_prober() {
    let raw_args: Vec<String> = std::env::args().skip(2).collect();
    let (markets_src, env_src, positional, errs) = extract_sources(raw_args);
    if !errs.is_empty() {
        for e in &errs {
            eprintln!("[ops-toolkit] prober: {e}");
        }
        std::process::exit(2);
    }
    if matches!(&markets_src, MarketsSource::File(p) if p.is_empty()) {
        eprintln!(
            "[ops-toolkit] prober requires <markets.toml> <prom-path> <json-path>\n\
            (or: --markets-stdin <prom-path> <json-path>)\n\
            example: ops-toolkit prober ./markets.toml /var/lib/node-exporter/mole.prom \
            /var/lib/mole/prober.json 10 0\n\
            example: sops -d markets.enc.toml | ops-toolkit prober \\\n\
                       --markets-stdin --env-from-file=/run/secrets/prober.env \\\n\
                       /var/lib/.../mole.prom /var/lib/.../prober.json 10 0"
        );
        std::process::exit(2);
    }
    let mut pos = positional.into_iter();
    let prom_path = pos.next().unwrap_or_else(|| {
        eprintln!("[ops-toolkit] prober: missing <prom-path>");
        std::process::exit(2);
    });
    let json_path = pos.next().unwrap_or_else(|| {
        eprintln!("[ops-toolkit] prober: missing <json-path>");
        std::process::exit(2);
    });
    let interval_secs: u64 = pos.next().map(|s| s.parse().unwrap_or(10)).unwrap_or(10);
    let max_cycles: usize = pos.next().map(|s| s.parse().unwrap_or(1)).unwrap_or(1);

    let registry = load_registry(
        &markets_src,
        &env_src,
        |path| std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}")),
        read_process_stdin,
        |k| std::env::var(k).ok(),
    )
    .unwrap_or_else(|e| {
        eprintln!("[ops-toolkit] {e}");
        std::process::exit(2);
    });

    let cfg = ProberConfig {
        tick_interval: Duration::from_secs(interval_secs.max(1)),
        max_cycles: if max_cycles == 0 {
            usize::MAX
        } else {
            max_cycles
        },
    };

    // Wave 26 — fold live per-market open-interest into the drift
    // check when a cluster RPC is configured. `MOLE_OI_RPC_URL` selects
    // the augmented path (requires the `solana-rpc` build feature); the
    // default build / unset env keeps the fixture-only base fetcher, so
    // `position_principal_drift` skips instead of false-alarming.
    #[cfg(feature = "solana-rpc")]
    {
        if let Ok(url) = std::env::var("MOLE_OI_RPC_URL") {
            let url = url.trim().to_string();
            if !url.is_empty() {
                use ops_toolkit::OpenInterestAugmentingFetcher;
                use solana_commitment_config::CommitmentConfig;
                eprintln!(
                    "[ops-toolkit] prober: folding live open-interest from {url} \
                    (commitment=confirmed)"
                );
                let fetcher = OpenInterestAugmentingFetcher::with_solana_rpc(
                    DemoFetcher,
                    url,
                    CommitmentConfig::confirmed(),
                );
                drive_prober(registry, fetcher, prom_path, json_path, cfg);
                return;
            }
        }
        eprintln!(
            "[ops-toolkit] prober: MOLE_OI_RPC_URL unset — open-interest \
            drift check will skip (fixture-only context)."
        );
    }

    drive_prober(registry, DemoFetcher, prom_path, json_path, cfg);
}

/// Wave 19 — fixture base `MarketFetcher`. Returns the hand-coded
/// healthy demo context for every market. Wave 26 wraps this in
/// `OpenInterestAugmentingFetcher` when a cluster RPC is configured so
/// the drift check reconciles real positions.
struct DemoFetcher;
impl MarketFetcher for DemoFetcher {
    fn fetch(&mut self, _entry: &MarketEntry) -> Result<HealthContext, String> {
        Ok(healthy_demo_context())
    }
}

/// Wave 19 — production monotonic clock for the prober loop.
struct StdClock;
impl ProberClock for StdClock {
    fn now_nanos(&self) -> u128 {
        Instant::now().elapsed().as_nanos()
    }
    fn sleep(&mut self, dur: Duration) {
        std::thread::sleep(dur);
    }
}

/// Wave 19 — file-backed prober sink. Writes the Prometheus textfile
/// and JSON snapshot to their configured paths each cycle.
struct FileSink {
    prom_path: String,
    json_path: String,
}
impl ProberSink for FileSink {
    fn publish_prometheus(&mut self, body: &str) -> Result<(), String> {
        std::fs::write(&self.prom_path, body).map_err(|e| e.to_string())
    }
    fn publish_json(&mut self, body: &str) -> Result<(), String> {
        std::fs::write(&self.json_path, body).map_err(|e| e.to_string())
    }
}

/// Wave 26 — generic prober driver. Hoisted out of `run_prober` so the
/// fixture-only base path and the `solana-rpc` open-interest-augmented
/// path share one loop body. Builds the `StdClock` + `FileSink`, runs
/// the loop, and propagates the worst exit code (or a scan error) from
/// the final cycle.
fn drive_prober<F: MarketFetcher>(
    registry: ops_toolkit::MarketRegistry,
    fetcher: F,
    prom_path: String,
    json_path: String,
    cfg: ProberConfig,
) {
    let mut prober = ProberLoop::new(
        registry,
        fetcher,
        StdClock,
        FileSink {
            prom_path,
            json_path,
        },
        cfg,
    );
    let outcomes = prober.run();
    if let Some(last) = outcomes.last() {
        if let Some(err) = &last.scan_error {
            eprintln!("[ops-toolkit] prober: scan error on last cycle: {err}");
            std::process::exit(2);
        }
        let code = last.worst_exit_code();
        if code != 0 {
            std::process::exit(code);
        }
    }
}

/// Wave 18 — `scan` mode. Loads a multi-market TOML registry and
/// runs the 21-check battery against the demo `HealthContext` for
/// every entry, then aggregates.
///
/// Usage:
///
///   ops-toolkit scan <markets.toml>
///
/// The `HealthContext` is the same hand-coded healthy demo fixture
/// — multi-market integration with REAL cluster data lands when
/// the dedicated prober binary spawns this lib API directly. The
/// scan mode demonstrates the wire format + exit-code aggregation
/// today so AlertManager pipelines can be set up before live data
/// is wired.
fn run_scan() {
    let raw_args: Vec<String> = std::env::args().skip(2).collect();
    let (markets_src, env_src, _positional, errs) = extract_sources(raw_args);
    if !errs.is_empty() {
        for e in &errs {
            eprintln!("[ops-toolkit] scan: {e}");
        }
        std::process::exit(2);
    }
    if matches!(&markets_src, MarketsSource::File(p) if p.is_empty()) {
        eprintln!(
            "[ops-toolkit] scan requires a path to markets.toml as the second arg.\n\
            example: ops-toolkit scan ./markets.toml\n\
            example: sops -d markets.enc.toml | ops-toolkit scan --markets-stdin \\\n\
                       --env-from-file=/run/secrets/prober.env"
        );
        std::process::exit(2);
    }
    let registry = load_registry(
        &markets_src,
        &env_src,
        |path| std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}")),
        read_process_stdin,
        |k| std::env::var(k).ok(),
    )
    .unwrap_or_else(|e| {
        eprintln!("[ops-toolkit] {e}");
        std::process::exit(2);
    });
    let report = scan_all_markets(&registry, |_| Ok(healthy_demo_context())).unwrap_or_else(
        |e| {
            eprintln!("[ops-toolkit] scan failed: {e}");
            std::process::exit(2);
        },
    );
    println!("{}", render_json_multi(&report));
    if report.worst_exit_code != 0 {
        std::process::exit(report.worst_exit_code);
    }
}

fn run_demo(broken: bool) {
    let format = std::env::args().nth(2).unwrap_or_else(|| "json".to_string());

    let mut ctx = healthy_demo_context();
    if broken {
        // Two simulated incidents: market accidentally globally
        // paused (P0) and oracle stale (P0). Worst case: ops gets
        // paged immediately.
        ctx.market.paused_globally = true;
        ctx.oracle.slot_age = 200;
    }
    let report = run_all_checks(&ctx);

    match format.as_str() {
        "json" => println!("{}", render_json(&report)),
        "prom" | "prometheus" => println!("{}", render_prometheus_textfile(&report)),
        "human" => print_human(&report),
        other => {
            eprintln!("[ops-toolkit] unknown format `{other}`. supported: json | prom | human");
            std::process::exit(2);
        }
    }

    let code = exit_code_for_status(&report);
    if code != 0 {
        std::process::exit(code);
    }
}

fn print_human(report: &ops_toolkit::HealthReport) {
    use ops_toolkit::CheckStatus;
    println!(
        "ops-toolkit health report @ unix={}\nOverall: {}\n",
        report.timestamp_unix,
        report.overall_status().as_str()
    );
    for c in &report.checks {
        let pip = match c.status {
            CheckStatus::Pass => "[ OK ]",
            CheckStatus::Warn => "[WARN]",
            CheckStatus::Critical => "[CRIT]",
        };
        println!("{pip} {} ({}) — {}", c.name, c.severity.as_str(), c.message);
    }
    let (p, w, ccount) = report.count_by_status();
    println!("\nPass: {p} · Warn: {w} · Critical: {ccount}");
}

/// Hand-coded fixture matching the wave 12 happy path. Mirrors
/// `crates/ops-toolkit/src/checks.rs::tests::healthy_ctx`.
fn healthy_demo_context() -> HealthContext {
    HealthContext {
        now_unix_secs: now_unix(),
        market: MarketFacts {
            paused_globally: false,
            paused: false,
            frozen_new_position: false,
            schema_version_onchain: clearing_core::SCHEMA_VERSION_CURRENT,
            schema_version_compiled: clearing_core::SCHEMA_VERSION_CURRENT,
        },
        sub_pools: vec![
            SubPoolFacts {
                id: 0,
                dormant_ticks: 100,
                pending_init_hints: 5,
                open_long_qty: 5_000,
                open_short_qty: 4_800,
            },
            SubPoolFacts {
                id: 1,
                dormant_ticks: 80,
                pending_init_hints: 3,
                open_long_qty: 3_200,
                open_short_qty: 3_500,
            },
        ],
        keeper: KeeperFacts {
            heartbeat_within_60s: true,
            failed_actions_last_hour: 0,
            skipped_actions_last_hour: 4,
            last_applied_vol: Some(0.85),
            consecutive_warming_ticks: 0,
            wallet_balance_lamports: 1_500_000_000,
        },
        rpc: RpcFacts {
            primary_get_slot_p95_ms: 50,
            primary_backup_slot_diff: 1,
            get_program_accounts_ms: 800,
        },
        oracle: OracleFacts {
            slot_age: 4,
            confidence_ratio: 0.001,
        },
        pool: PoolFacts {
            total_notional_micro_usdc: 100_000_000_000,
            recovery_outstanding_micro_usdc: 1_000,
            // Demo reconciles cleanly: on-chain aggregate == reported.
            onchain_position_notional_micro_usdc: 100_000_000_000,
        },
        // Wave 17 — leader-lock probe disabled in the demo
        // fixture; ops bins inject real LeaderLockFacts via a
        // separate cluster-fetch path.
        leader_lock: None,
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
