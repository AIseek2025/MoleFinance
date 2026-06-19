//! Wave 19 — periodic multi-market prober daemon.
//!
//! Wave 18 turned `scan_all_markets` into a one-shot health-check
//! that takes a `MarketRegistry` + a per-market `HealthContext`
//! builder and returns an aggregated `MultiMarketHealthReport`. The
//! one-shot binary path is fine for local SOPs (KL-09, KL-10) and
//! for AlertManager hooks that exec it on a Prometheus probe — but
//! a long-running prober that:
//!
//!   * polls the cluster every N seconds,
//!   * publishes Prometheus textfiles on every cycle (so an
//!     ops-stack node-exporter can scrape them),
//!   * publishes a stable JSON snapshot file (for kuberentes
//!     `liveness` style probes that tail the file),
//!   * gracefully handles partial-failure (one bad market doesn't
//!     blow up the whole loop)
//!
//! is what an actual production deployment needs. This module is
//! the daemon scaffolding for that prober.
//!
//! The module is deliberately host-driver-agnostic: it doesn't
//! talk to Solana itself. Instead, callers supply:
//!   1. A `MarketRegistry` (loaded once at boot, reload on
//!      SIGHUP — the current binary keeps the process simple by
//!      only loading once).
//!   2. A `MarketFetcher` impl that maps `MarketEntry → Result<
//!      HealthContext, String>`. Production code wires this to
//!      `solana-client`; tests wire it to a fixture.
//!   3. A clock impl (`std::time::Instant` in production, a fake
//!      clock in tests).
//!   4. A sink impl that accepts `(prom_textfile, json)` strings —
//!      production writes to disk, tests collect into vectors.
//!
//! All inputs are traits (no `Box<dyn Trait>`), so the test path
//! is fully synchronous and free of `tokio` / `solana-client` /
//! file-system dependencies — the wave-18 hand-rolled-toml /
//! no-serde governance carries forward.

use std::time::Duration;

use keeper_rpc::AccountFetcher;

use crate::context::HealthContext;
use crate::multi::{
    render_json_multi, scan_all_markets, MarketEntry, MarketRegistry, MultiMarketHealthReport,
    ScanError,
};
use crate::position_interest::{apply_open_interest_to_pool, fetch_open_interest_for_market};
use crate::report::render_prometheus_textfile;

/// Wave 19 — abstract per-market fetcher used by the prober loop.
///
/// **Fetcher contract**: implementations MUST return a populated
/// `HealthContext` on every call. Transient RPC errors should be
/// degraded *inside* the fetcher (e.g. inflate `rpc_p95_ms`, mark
/// `getProgramAccounts` slow) so the standard 21-check battery
/// fires the right alert. Returning `Err` is reserved for
/// catastrophic conditions ("RPC pool empty") and aborts the
/// entire cycle's publication — AlertManager sees a textfile gap
/// that drives its `for: 30s` rule.
pub trait MarketFetcher {
    /// Build a `HealthContext` for one market.
    fn fetch(&mut self, entry: &MarketEntry) -> Result<HealthContext, String>;
}

/// Wave 19 — abstract clock so we can drive the loop deterministically
/// in tests. The default impl uses `std::time::Instant`.
pub trait ProberClock {
    /// Returns the current monotonic instant as nanoseconds since
    /// boot. The exact zero is unspecified; `tick_every` only cares
    /// about deltas.
    fn now_nanos(&self) -> u128;
    /// Sleep until the next tick. The default impl uses
    /// `std::thread::sleep`; tests substitute a no-op + counter.
    fn sleep(&mut self, dur: Duration);
}

/// Wave 19 — sink that receives every cycle's prometheus + json
/// payloads. Production writes to `prom_path` + `json_path`; tests
/// push into vectors.
pub trait ProberSink {
    /// Called once per cycle with the rendered Prometheus textfile.
    fn publish_prometheus(&mut self, body: &str) -> Result<(), String>;
    /// Called once per cycle with the rendered JSON snapshot.
    fn publish_json(&mut self, body: &str) -> Result<(), String>;
}

/// Wave 19 — runtime config for `ProberLoop`.
#[derive(Debug, Clone)]
pub struct ProberConfig {
    /// Tick cadence. Production default is 10s; AlertManager
    /// `for: 30s` rules expect at most a 30s missed-cycle window.
    pub tick_interval: Duration,
    /// Maximum number of cycles to run before the loop returns.
    /// Production binaries set this to `usize::MAX` (run forever);
    /// tests set it to a small finite number.
    pub max_cycles: usize,
}

impl Default for ProberConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(10),
            max_cycles: usize::MAX,
        }
    }
}

/// Wave 19 — outcome of a single prober cycle.
#[derive(Debug, Clone)]
pub struct CycleOutcome {
    /// Cycle index (0-based).
    pub cycle: usize,
    /// Aggregated multi-market report. `None` when a hard scan
    /// error aborted publication for this cycle.
    pub report: Option<MultiMarketHealthReport>,
    /// Reserved for future "tolerate one bad market" mode. Empty
    /// in the wave-19 strict-fail policy.
    pub partial_failures: Vec<String>,
    /// Hard scan error that aborted the cycle. When `Some`, no
    /// report and no publication occurred.
    pub scan_error: Option<String>,
    /// Whether `sink.publish_prometheus` succeeded. Always `false`
    /// when `scan_error.is_some()`.
    pub prom_published: bool,
    /// Whether `sink.publish_json` succeeded. Always `false` when
    /// `scan_error.is_some()`.
    pub json_published: bool,
}

impl CycleOutcome {
    /// Worst exit code observed in this cycle. Returns `0` when no
    /// report was produced.
    pub fn worst_exit_code(&self) -> i32 {
        self.report.as_ref().map_or(0, |r| r.worst_exit_code)
    }
}

/// Wave 19 — the daemon loop itself. Owns no I/O; pluggable via
/// generics so the test path stays sync + zero-dep.
pub struct ProberLoop<F, C, S>
where
    F: MarketFetcher,
    C: ProberClock,
    S: ProberSink,
{
    registry: MarketRegistry,
    fetcher: F,
    clock: C,
    sink: S,
    config: ProberConfig,
}

impl<F, C, S> ProberLoop<F, C, S>
where
    F: MarketFetcher,
    C: ProberClock,
    S: ProberSink,
{
    /// Build a new prober loop. The registry is consumed because the
    /// loop pins it for the lifetime of the daemon.
    pub fn new(
        registry: MarketRegistry,
        fetcher: F,
        clock: C,
        sink: S,
        config: ProberConfig,
    ) -> Self {
        Self {
            registry,
            fetcher,
            clock,
            sink,
            config,
        }
    }

    /// Run the loop. Returns the per-cycle outcomes in order. The
    /// loop sleeps `config.tick_interval` BETWEEN cycles, not before
    /// the first one — so a `max_cycles=1` run is effectively a
    /// one-shot scan with publication side effects.
    pub fn run(&mut self) -> Vec<CycleOutcome> {
        let mut out = Vec::with_capacity(self.config.max_cycles.min(64));
        for cycle in 0..self.config.max_cycles {
            let outcome = self.run_one_cycle(cycle);
            out.push(outcome);
            if cycle + 1 < self.config.max_cycles {
                self.clock.sleep(self.config.tick_interval);
            }
        }
        out
    }

    fn run_one_cycle(&mut self, cycle: usize) -> CycleOutcome {
        let partial_failures: Vec<String> = Vec::new();
        // Wave 19 — fetcher contract: must NEVER bubble a transient
        // RPC error up. Production fetchers degrade gracefully (cache
        // last-good ctx, set elevated `rpc_p95_ms`, etc.). The prober
        // treats every fetcher Err as a HARD scan failure that
        // aborts publication for this cycle so AlertManager sees a
        // gap rather than stale all-Pass data. Idle retry happens on
        // the next tick.
        let scan_result =
            scan_all_markets(&self.registry, |entry| self.fetcher.fetch(entry));
        let (report, scan_error) = match scan_result {
            Ok(r) => (Some(r), None),
            Err(e) => (None, Some(scan_error_to_string(&e))),
        };
        let mut prom_published = false;
        let mut json_published = false;
        if let Some(ref r) = report {
            // Prometheus textfile: union per-market prom blocks
            // separated by `# market=<symbol>` comment markers so
            // node_exporter's textfile collector keeps timeseries
            // distinct (each per-market block has the same metric
            // names, but the markdown # comment doesn't break it).
            let mut prom = String::with_capacity(8 * 1024);
            prom.push_str("# HELP mole_prober_cycle Cycle counter.\n");
            prom.push_str("# TYPE mole_prober_cycle counter\n");
            prom.push_str("mole_prober_cycle ");
            prom.push_str(&cycle.to_string());
            prom.push('\n');
            prom.push_str("# HELP mole_prober_worst_exit_code Worst exit code across markets.\n");
            prom.push_str("# TYPE mole_prober_worst_exit_code gauge\n");
            prom.push_str("mole_prober_worst_exit_code ");
            prom.push_str(&r.worst_exit_code.to_string());
            prom.push('\n');
            // Wave 29 — protocol-wide rollup gauges so a dashboard can
            // alert on "N markets critical" without scraping every
            // per-market block. Mirrors the JSON `protocol` block and
            // the frontend Overview page.
            let protocol = crate::protocol_summary::summarize_protocol(r);
            prom.push_str("# HELP mole_prober_markets Markets scanned by overall status.\n");
            prom.push_str("# TYPE mole_prober_markets gauge\n");
            prom.push_str("mole_prober_markets{status=\"total\"} ");
            prom.push_str(&protocol.markets.to_string());
            prom.push('\n');
            prom.push_str("mole_prober_markets{status=\"healthy\"} ");
            prom.push_str(&protocol.healthy_markets.to_string());
            prom.push('\n');
            prom.push_str("mole_prober_markets{status=\"warn\"} ");
            prom.push_str(&protocol.warn_markets.to_string());
            prom.push('\n');
            prom.push_str("mole_prober_markets{status=\"critical\"} ");
            prom.push_str(&protocol.critical_markets.to_string());
            prom.push('\n');
            for m in &r.per_market {
                prom.push_str("# market=");
                prom.push_str(&m.symbol);
                prom.push('\n');
                let body = render_prometheus_textfile(&m.report);
                // Re-prefix every metric line with a `market=` label
                // so the union file has unique label sets.
                prom.push_str(&relabel_with_market(&body, &m.symbol));
            }
            prom_published = self.sink.publish_prometheus(&prom).is_ok();
            // JSON snapshot: stable wave-18 multi-market shape.
            let json = render_json_multi(r);
            json_published = self.sink.publish_json(&json).is_ok();
        }
        let _now_ns = self.clock.now_nanos();
        CycleOutcome {
            cycle,
            report,
            partial_failures,
            scan_error,
            prom_published,
            json_published,
        }
    }
}

/// Wave 25 — a `MarketFetcher` decorator that augments each per-market
/// `HealthContext` with **on-chain open-interest** before the standard
/// check battery runs.
///
/// Wave 23 added `fetch_open_interest` (program-wide position scan) and
/// wave 24 added both `apply_open_interest_to_pool` and the
/// `position_principal_drift` health check — but nothing yet fed real
/// cluster data into the check, so it always skipped (on-chain notional
/// stuck at 0). This decorator closes that loop:
///
///   1. delegate to a `base` `MarketFetcher` that builds the ctx with
///      the indexer-reported pool facts (notional from the indexer),
///   2. run a per-market open-interest scan against an `AccountFetcher`
///      `source`, and
///   3. fold the on-chain aggregate into `ctx.pool` via
///      `apply_open_interest_to_pool`, so the drift check reconciles
///      real positions vs the indexer figure.
///
/// The `source` is generic so the test path uses `MockAccountFetcher`
/// (zero-dep, sync) while production wires `SolanaRpcAccountFetcher`
/// (behind the `solana-rpc` feature).
///
/// **Failure policy**: an open-interest scan error is NON-fatal — the
/// base ctx flows through unchanged (on-chain notional stays 0, so the
/// drift check *skips* rather than false-alarms). Persistent RPC
/// trouble already surfaces through the dedicated RPC checks, so we
/// don't want a transient `getProgramAccounts` hiccup to abort the
/// whole cycle's publication.
pub struct OpenInterestAugmentingFetcher<F, S>
where
    F: MarketFetcher,
    S: AccountFetcher,
{
    base: F,
    source: S,
}

impl<F, S> OpenInterestAugmentingFetcher<F, S>
where
    F: MarketFetcher,
    S: AccountFetcher,
{
    /// Wrap `base` so every fetched context is augmented with the
    /// on-chain open-interest scanned from `source`.
    pub fn new(base: F, source: S) -> Self {
        Self { base, source }
    }

    /// Unwrap back into the `(base, source)` pair (used by callers that
    /// need to reclaim the source for shutdown / metrics).
    pub fn into_inner(self) -> (F, S) {
        (self.base, self.source)
    }
}

#[cfg(feature = "solana-rpc")]
impl<F> OpenInterestAugmentingFetcher<F, keeper_rpc::solana::SolanaRpcAccountFetcher>
where
    F: MarketFetcher,
{
    /// Wave 25 — production constructor. Wraps `base` with a live
    /// `SolanaRpcAccountFetcher` open-interest source built from an RPC
    /// URL + commitment. This is the single-call path a production
    /// prober binary uses to fold real per-market open-interest into
    /// the wave-24 `position_principal_drift` check — no need for the
    /// caller to assemble the `getProgramAccounts` plumbing by hand.
    ///
    /// Compile-checked under `--features solana-rpc`; runtime requires a
    /// reachable cluster, so it is exercised on devnet rather than in
    /// the host-only test suite.
    pub fn with_solana_rpc(
        base: F,
        rpc_url: String,
        commitment: solana_commitment_config::CommitmentConfig,
    ) -> Self {
        Self::new(
            base,
            keeper_rpc::solana::SolanaRpcAccountFetcher::new(rpc_url, commitment),
        )
    }
}

impl<F, S> MarketFetcher for OpenInterestAugmentingFetcher<F, S>
where
    F: MarketFetcher,
    S: AccountFetcher,
{
    fn fetch(&mut self, entry: &MarketEntry) -> Result<HealthContext, String> {
        let mut ctx = self.base.fetch(entry)?;
        if let Ok(oi) =
            fetch_open_interest_for_market(&self.source, &entry.program_id, &entry.market_pda)
        {
            apply_open_interest_to_pool(&mut ctx.pool, &oi);
        }
        // On scan error we deliberately leave `onchain_position_notional`
        // at whatever the base produced (0 for the live path), which
        // makes `position_principal_drift` skip instead of false-alarm.
        Ok(ctx)
    }
}

fn scan_error_to_string(e: &ScanError) -> String {
    match e {
        ScanError::Registry(_) => format!("registry: {e}"),
        ScanError::Builder { symbol, detail } => format!("builder {symbol}: {detail}"),
    }
}

/// Re-prefix every prom metric line in `body` with a `market="<m>"`
/// label so the union file has unique label sets per market.
///
/// Best-effort textual rewrite — every line that starts with
/// `mole_health_check_status{name=` or `mole_health_overall ` gets
/// a `,market="<m>"` inserted before the closing brace (or after
/// the metric name if it's bare-gauge). Comment lines (`#`) and
/// other text pass through.
fn relabel_with_market(body: &str, symbol: &str) -> String {
    let mut out = String::with_capacity(body.len() + 32);
    for line in body.lines() {
        if line.starts_with('#') || line.is_empty() {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if line.contains('{') {
            // Insert `,market="<symbol>"` before the closing `}`.
            if let Some(close) = line.find('}') {
                let (head, tail) = line.split_at(close);
                out.push_str(head);
                out.push_str(",market=\"");
                push_json_str_minimal(&mut out, symbol);
                out.push('"');
                out.push_str(tail);
                out.push('\n');
                continue;
            }
            // Misformatted, fall through.
            out.push_str(line);
            out.push('\n');
            continue;
        }
        // Bare-gauge style: `metric_name <value>` — convert to
        // `metric_name{market="..."} <value>`.
        if let Some(sp) = line.find(' ') {
            let (name, value) = line.split_at(sp);
            out.push_str(name);
            out.push_str("{market=\"");
            push_json_str_minimal(&mut out, symbol);
            out.push_str("\"}");
            out.push_str(value);
            out.push('\n');
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn push_json_str_minimal(buf: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => buf.push_str("\\\""),
            '\\' => buf.push_str("\\\\"),
            c => buf.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{
        KeeperFacts, LeaderLockFacts, MarketFacts, OracleFacts, PoolFacts, RpcFacts, SubPoolFacts,
    };
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    fn registry_two() -> MarketRegistry {
        let toml = r#"
[[markets]]
symbol = "SOL-USD"
program_id = "11111111111111111111111111111111"
market_pda = "11111111111111111111111111111112"
lock_pda = "11111111111111111111111111111113"

[[markets]]
symbol = "BTC-USD"
program_id = "11111111111111111111111111111111"
market_pda = "11111111111111111111111111111114"
lock_pda = "11111111111111111111111111111115"
"#;
        MarketRegistry::from_toml_str(toml).expect("registry")
    }

    /// Healthy `HealthContext` — every check passes.
    fn happy_ctx() -> HealthContext {
        HealthContext {
            now_unix_secs: 1_700_000_000,
            market: MarketFacts {
                paused_globally: false,
                paused: false,
                frozen_new_position: false,
                schema_version_onchain: clearing_core::SCHEMA_VERSION_CURRENT,
                schema_version_compiled: clearing_core::SCHEMA_VERSION_CURRENT,
            },
            sub_pools: vec![SubPoolFacts {
                id: 0,
                dormant_ticks: 100,
                pending_init_hints: 5,
                open_long_qty: 5_000,
                open_short_qty: 4_800,
            }],
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
                onchain_position_notional_micro_usdc: 100_000_000_000,
            },
            leader_lock: None,
        }
    }

    /// Failing fetcher — every market returns Err.
    struct FailFetcher;
    impl MarketFetcher for FailFetcher {
        fn fetch(&mut self, entry: &MarketEntry) -> Result<HealthContext, String> {
            Err(format!("rpc unreachable for {}", entry.symbol))
        }
    }

    /// Healthy fetcher — every market returns a happy ctx.
    struct OkFetcher;
    impl MarketFetcher for OkFetcher {
        fn fetch(&mut self, _entry: &MarketEntry) -> Result<HealthContext, String> {
            Ok(happy_ctx())
        }
    }

    /// Degraded-RPC fetcher — every market returns a ctx with
    /// elevated RPC P95 to trigger the rpc_p95 check (P2 warn).
    struct DegradedFetcher;
    impl MarketFetcher for DegradedFetcher {
        fn fetch(&mut self, _entry: &MarketEntry) -> Result<HealthContext, String> {
            let mut ctx = happy_ctx();
            ctx.rpc.primary_get_slot_p95_ms = 5_000;
            Ok(ctx)
        }
    }

    /// Test clock — counts sleeps, reports synthetic time.
    struct FakeClock {
        nanos: u128,
        sleeps: Vec<Duration>,
    }
    impl ProberClock for FakeClock {
        fn now_nanos(&self) -> u128 {
            self.nanos
        }
        fn sleep(&mut self, dur: Duration) {
            self.nanos += dur.as_nanos();
            self.sleeps.push(dur);
        }
    }

    /// Test sink — collects published payloads.
    #[derive(Default)]
    struct VecSink {
        prom: Rc<RefCell<Vec<String>>>,
        json: Rc<RefCell<Vec<String>>>,
        fail_after: Option<usize>,
        published: usize,
    }
    impl ProberSink for VecSink {
        fn publish_prometheus(&mut self, body: &str) -> Result<(), String> {
            if matches!(self.fail_after, Some(n) if self.published >= n) {
                return Err("disk full".into());
            }
            self.prom.borrow_mut().push(body.to_string());
            self.published += 1;
            Ok(())
        }
        fn publish_json(&mut self, body: &str) -> Result<(), String> {
            self.json.borrow_mut().push(body.to_string());
            Ok(())
        }
    }

    #[test]
    fn run_single_cycle_with_ok_fetcher_publishes_both_files() {
        let sink = VecSink::default();
        let prom = sink.prom.clone();
        let json = sink.json.clone();
        let mut prober = ProberLoop::new(
            registry_two(),
            OkFetcher,
            FakeClock { nanos: 0, sleeps: vec![] },
            sink,
            ProberConfig {
                tick_interval: Duration::from_secs(1),
                max_cycles: 1,
            },
        );
        let outcomes = prober.run();
        assert_eq!(outcomes.len(), 1);
        let o = &outcomes[0];
        assert!(o.scan_error.is_none(), "no scan error");
        assert!(o.partial_failures.is_empty(), "no partial failures");
        assert!(o.prom_published, "prom published");
        assert!(o.json_published, "json published");
        assert_eq!(o.worst_exit_code(), 0);
        let prom = prom.borrow();
        assert_eq!(prom.len(), 1);
        assert!(prom[0].contains("mole_prober_cycle 0"));
        assert!(prom[0].contains("mole_prober_worst_exit_code 0"));
        assert!(prom[0].contains("market=SOL-USD"));
        assert!(prom[0].contains("market=BTC-USD"));
        let json = json.borrow();
        assert_eq!(json.len(), 1);
        assert!(json[0].contains("\"SOL-USD\""));
        assert!(json[0].contains("\"BTC-USD\""));
        assert!(json[0].contains("\"worst_exit_code\":0"));
    }

    #[test]
    fn run_three_cycles_sleeps_between() {
        let sleeps = Rc::new(RefCell::new(vec![]));
        struct ProxyClock(Rc<RefCell<Vec<Duration>>>);
        impl ProberClock for ProxyClock {
            fn now_nanos(&self) -> u128 {
                0
            }
            fn sleep(&mut self, dur: Duration) {
                self.0.borrow_mut().push(dur);
            }
        }
        let mut prober = ProberLoop::new(
            registry_two(),
            OkFetcher,
            ProxyClock(sleeps.clone()),
            VecSink::default(),
            ProberConfig {
                tick_interval: Duration::from_millis(10),
                max_cycles: 3,
            },
        );
        let outcomes = prober.run();
        assert_eq!(outcomes.len(), 3);
        // Sleeps happen between cycles → 2 sleeps for 3 cycles.
        let s = sleeps.borrow();
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|d| *d == Duration::from_millis(10)));
    }

    #[test]
    fn fail_fetcher_aborts_cycle_publication() {
        let sink = VecSink::default();
        let prom_arc = sink.prom.clone();
        let json_arc = sink.json.clone();
        let mut prober = ProberLoop::new(
            registry_two(),
            FailFetcher,
            FakeClock { nanos: 0, sleeps: vec![] },
            sink,
            ProberConfig {
                tick_interval: Duration::from_secs(1),
                max_cycles: 1,
            },
        );
        let outcomes = prober.run();
        // Hard-fail policy: scan_error set, no publication.
        assert!(outcomes[0].scan_error.is_some());
        assert!(outcomes[0].report.is_none());
        assert!(!outcomes[0].prom_published);
        assert!(!outcomes[0].json_published);
        assert!(prom_arc.borrow().is_empty());
        assert!(json_arc.borrow().is_empty());
    }

    #[test]
    fn degraded_rpc_fetcher_surfaces_in_report() {
        let mut prober = ProberLoop::new(
            registry_two(),
            DegradedFetcher,
            FakeClock { nanos: 0, sleeps: vec![] },
            VecSink::default(),
            ProberConfig {
                tick_interval: Duration::from_secs(1),
                max_cycles: 1,
            },
        );
        let outcomes = prober.run();
        let r = outcomes[0].report.as_ref().expect("report");
        // Every market sees the elevated RPC P95 → at least one
        // check at warn or worse.
        let any_non_pass = r.per_market.iter().any(|m| m.exit_code > 0);
        assert!(any_non_pass, "expected at least one non-pass exit code");
    }

    #[test]
    fn sink_failure_marks_outcome_but_does_not_panic() {
        let sink = VecSink {
            prom: Rc::new(RefCell::new(vec![])),
            json: Rc::new(RefCell::new(vec![])),
            fail_after: Some(0),
            published: 0,
        };
        let mut prober = ProberLoop::new(
            registry_two(),
            OkFetcher,
            FakeClock { nanos: 0, sleeps: vec![] },
            sink,
            ProberConfig {
                tick_interval: Duration::from_secs(1),
                max_cycles: 1,
            },
        );
        let outcomes = prober.run();
        assert!(!outcomes[0].prom_published, "prom failed");
        assert!(outcomes[0].json_published, "json still publishes");
    }

    #[test]
    fn relabel_with_market_inserts_label_into_existing_braces() {
        let body = "mole_health_check_status{name=\"foo\",severity=\"P1\"} 0\n";
        let out = relabel_with_market(body, "SOL-USD");
        assert!(out.contains("market=\"SOL-USD\""));
        assert!(out.contains("name=\"foo\""));
    }

    #[test]
    fn relabel_with_market_handles_bare_gauge() {
        let body = "mole_health_overall 0\n";
        let out = relabel_with_market(body, "SOL-USD");
        assert!(out.contains("mole_health_overall{market=\"SOL-USD\"} 0"));
    }

    #[test]
    fn relabel_with_market_passes_comments_unchanged() {
        let body = "# HELP mole_health_check_status …\n# TYPE mole_health_check_status gauge\n";
        let out = relabel_with_market(body, "SOL-USD");
        assert!(out.contains("# HELP mole_health_check_status"));
        assert!(out.contains("# TYPE mole_health_check_status gauge"));
        // No data lines so no `market=` label inserted.
        assert!(!out.contains("market=\"SOL-USD\""));
    }

    /// Wave 25 — build a borsh-encoded `Position` account body for the
    /// MockAccountFetcher (discriminator + body), tagged to `market`.
    fn encoded_position(market: [u8; 32], long: bool, principal: u64, notional: u128) -> Vec<u8> {
        use borsh::BorshSerialize;
        use crate::position_interest::position_account_discriminator;
        let pos = keeper_decoder::OnchainPosition {
            owner: [1u8; 32],
            market,
            sub_pool: [3u8; 32],
            position_id: 7,
            direction_is_long: long,
            status: crate::position_interest::POSITION_STATUS_OPEN,
            principal,
            leverage_bps: 20_000,
            notional,
            active_shares: 1,
            recovery_shares: 0,
            recovery_bucket_tick: 0,
            has_recovery_bucket: false,
            zero_price: 0,
            entry_price: 100,
            last_sync_slot: 0,
            active_generation: 0,
            opened_at: 1_700_000_000,
            updated_at: 1_700_000_000,
            closed_at: 0,
            schema_version: 1,
            bump: 254,
            _pad: [0u8; 5],
        };
        let mut out = position_account_discriminator().to_vec();
        pos.serialize(&mut out).expect("borsh serialize");
        out
    }

    fn entry(symbol: &str, program_id: [u8; 32], market_pda: [u8; 32]) -> MarketEntry {
        MarketEntry {
            symbol: symbol.to_string(),
            program_id,
            market_pda,
            lock_pda: [0u8; 32],
            expected_leader: None,
        }
    }

    /// Base fetcher that returns a happy ctx with on-chain notional
    /// pre-zeroed (the live-path default before augmentation).
    struct ZeroOnchainBase;
    impl MarketFetcher for ZeroOnchainBase {
        fn fetch(&mut self, _entry: &MarketEntry) -> Result<HealthContext, String> {
            let mut ctx = happy_ctx();
            ctx.pool.onchain_position_notional_micro_usdc = 0;
            Ok(ctx)
        }
    }

    #[test]
    fn augmenting_fetcher_folds_per_market_open_interest_into_pool() {
        use keeper_rpc::MockAccountFetcher;
        let program_id = [7u8; 32];
        let mkt_a = [0xaa; 32];
        let mkt_b = [0xbb; 32];
        let mut source = MockAccountFetcher::new();
        // Two positions in market A (notional 60_000) and one in B.
        source.insert([1u8; 32], program_id, encoded_position(mkt_a, true, 1_000, 40_000));
        source.insert([2u8; 32], program_id, encoded_position(mkt_a, false, 500, 20_000));
        source.insert([3u8; 32], program_id, encoded_position(mkt_b, true, 9_000, 99_000));

        let mut f = OpenInterestAugmentingFetcher::new(ZeroOnchainBase, source);
        let ctx = f.fetch(&entry("A", program_id, mkt_a)).expect("fetch ok");
        // Only market A's positions counted → 40_000 + 20_000.
        assert_eq!(ctx.pool.onchain_position_notional_micro_usdc, 60_000);
    }

    #[test]
    fn augmenting_fetcher_scan_miss_leaves_onchain_zero() {
        use keeper_rpc::MockAccountFetcher;
        let program_id = [7u8; 32];
        let mkt = [0xcc; 32];
        // Empty source → no positions for this market.
        let source = MockAccountFetcher::new();
        let mut f = OpenInterestAugmentingFetcher::new(ZeroOnchainBase, source);
        let ctx = f.fetch(&entry("C", program_id, mkt)).expect("fetch ok");
        // No positions → on-chain notional stays 0 → drift check skips.
        assert_eq!(ctx.pool.onchain_position_notional_micro_usdc, 0);
    }

    #[test]
    fn augmenting_fetcher_drives_drift_check_through_prober_loop() {
        use keeper_rpc::MockAccountFetcher;
        // happy_ctx reports total_notional 100_000_000_000; supply an
        // on-chain aggregate that MATCHES so the drift check passes
        // (rather than skips), proving the value reaches the battery.
        let program_id = [7u8; 32];
        let mkt = [0xaa; 32];
        let mut source = MockAccountFetcher::new();
        source.insert(
            [1u8; 32],
            program_id,
            encoded_position(mkt, true, 1_000, 100_000_000_000),
        );
        let augmenting = OpenInterestAugmentingFetcher::new(ZeroOnchainBase, source);
        // Registry with one market; overwrite the decoded pubkeys so
        // they match our fixtures (program_id + market_pda).
        let mut reg = MarketRegistry::from_toml_str(
            r#"
[[markets]]
symbol = "A"
program_id = "11111111111111111111111111111111"
market_pda = "11111111111111111111111111111112"
lock_pda = "11111111111111111111111111111113"
"#,
        )
        .expect("registry");
        reg.markets[0].program_id = program_id;
        reg.markets[0].market_pda = mkt;
        let mut prober = ProberLoop::new(
            reg,
            augmenting,
            FakeClock { nanos: 0, sleeps: vec![] },
            VecSink::default(),
            ProberConfig {
                tick_interval: Duration::from_secs(1),
                max_cycles: 1,
            },
        );
        let outcomes = prober.run();
        let r = outcomes[0].report.as_ref().expect("report");
        let m = &r.per_market[0];
        let drift = m
            .report
            .checks
            .iter()
            .find(|c| c.name == "position_principal_drift")
            .expect("drift check present");
        // On-chain matches reported → Pass, and NOT the skip message.
        assert!(
            !drift.message.to_lowercase().contains("skip"),
            "drift check should run, not skip: {}",
            drift.message
        );
    }

    /// Sanity: a fetcher with explicit `LeaderLockFacts` routes
    /// the values through the report path correctly.
    #[test]
    fn fetcher_with_leader_lock_facts_routes_into_report() {
        struct OkWithFacts;
        impl MarketFetcher for OkWithFacts {
            fn fetch(&mut self, _entry: &MarketEntry) -> Result<HealthContext, String> {
                let mut ctx = happy_ctx();
                ctx.leader_lock = Some(LeaderLockFacts {
                    initialized: true,
                    has_leader: true,
                    current_leader: [0u8; 32],
                    last_heartbeat_slot: 100,
                    current_slot: 105,
                    takeover_threshold_slots: 50,
                    expected_leader: None,
                });
                Ok(ctx)
            }
        }
        let mut prober = ProberLoop::new(
            registry_two(),
            OkWithFacts,
            FakeClock { nanos: 0, sleeps: vec![] },
            VecSink::default(),
            ProberConfig {
                tick_interval: Duration::from_secs(1),
                max_cycles: 1,
            },
        );
        let outcomes = prober.run();
        let r = outcomes[0].report.as_ref().expect("report");
        // Two markets in the registry → two per-market reports.
        assert_eq!(r.per_market.len(), 2);
        // Both reports include the leader-lock check (wave-17
        // `holder_matches_expected` fires Warn when expected is
        // unset; we don't assert exit code here — the routing
        // is what we're verifying).
        for m in &r.per_market {
            let has_holder_check = m
                .report
                .checks
                .iter()
                .any(|c| c.name == "keeper_leader_lock_holder_matches_expected");
            assert!(has_holder_check, "expected holder check in `{}`", m.symbol);
        }
    }
}
