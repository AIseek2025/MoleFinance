//! Wave 12 — Prometheus metrics for the keeper bot.
//!
//! ## Why text format, not protobuf
//!
//! Prometheus's text exposition format is the lingua franca of
//! everything in the modern observability stack: Prometheus itself,
//! Grafana Cloud, Datadog's OpenMetrics scrape, VictoriaMetrics, etc.
//! all consume the same string. The protobuf format is a marginal
//! latency win at scrape time but requires `prost` + a generated
//! schema; we deliberately keep this module dependency-free so the
//! keeper bot can be rebuilt and redeployed in seconds during an
//! incident.
//!
//! ## Naming convention
//!
//! Metric names follow Prometheus best practices:
//!
//! - `<subsystem>_<unit>` — e.g. `keeper_tick_duration_milliseconds`
//! - `<subsystem>_<thing>_total` for counters
//! - `<subsystem>_<thing>` for gauges
//! - All metrics carry the `MetricKind` line so downstream consumers
//!   handle counter rollovers correctly.
//!
//! ## Atomicity guarantees
//!
//! All metric mutators take `&self` and use `AtomicU64::fetch_add` /
//! `AtomicI64::store` with `Ordering::Relaxed`. The bot's tick loop
//! calls `observe_tick` on a single thread, so we don't actually need
//! `SeqCst`; relaxed loads are still consistent within the single
//! HTTP scrape thread because the scrape happens between ticks 99%
//! of the time and the few-microsecond mid-tick scrape just sees a
//! mid-update value (acceptable trade-off for free correctness).

use std::sync::atomic::{AtomicI64, AtomicU64, AtomicU8, Ordering};

use crate::TickReport;
use keeper::ActionDispatchResult;

/// Sentinel value used by `last_applied_vol_milli` to mean "not yet
/// warmed up". Chosen to be unmistakably out of band — real σ̂ is
/// always in [50, 5000] (clamped by `RealizedVolatilityEstimator`).
pub const APPLIED_VOL_NOT_WARM: i64 = -1;

/// Leadership state. Today the bot's leader-election seam is
/// purely informational (see `wave 12.4` in
/// `Docs/Planning/20-…md`); production deployments override this
/// gauge based on their external lock manager.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaderStatus {
    /// Initial state before any leader-election event has been observed.
    Unknown = 0,
    /// This replica is the active leader and is allowed to submit tx.
    Leader = 1,
    /// This replica is a hot-standby (dry-run mode).
    Standby = 2,
}

impl LeaderStatus {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Leader,
            2 => Self::Standby,
            _ => Self::Unknown,
        }
    }
}

/// Process-wide metric register.
///
/// Construct once per `keeper-bot` process and share via `Arc` to
/// the tick loop and the HTTP scrape thread.
#[derive(Debug, Default)]
pub struct KeeperMetrics {
    // ---------- counters ----------
    /// Total ticks completed (not counting failures that returned
    /// before `observe_tick`).
    pub ticks_total: AtomicU64,
    /// Cumulative `Submitted` actions across all ticks.
    pub actions_submitted_total: AtomicU64,
    /// Cumulative `Failed` actions across all ticks. The runbook
    /// alert `KEEPER_FAIL_RATE_HIGH` triggers off `rate(this[1h])`.
    pub actions_failed_total: AtomicU64,
    /// Cumulative `Skipped` actions across all ticks.
    pub actions_skipped_total: AtomicU64,
    /// Cumulative init hints recorded in the scheduler. Includes
    /// duplicate hints that the scheduler dedupes — high level
    /// monitoring should compare against `actions_submitted_total`
    /// for the InitDormantBucket subset.
    pub init_hints_recorded_total: AtomicU64,
    /// Cumulative snapshot fetch errors (transient + permanent).
    /// `permanent` errors (paused, schema mismatch) cause the loop
    /// to bail; transient errors keep the loop running.
    pub snapshot_errors_total: AtomicU64,
    // ---------- gauges ----------
    /// Wall-clock duration of the most recent tick (ms).
    pub last_tick_duration_ms: AtomicU64,
    /// Realised vol applied to the predictor on the last tick.
    /// Stored as σ̂ × 1000 because Prometheus gauges are integers
    /// here; convert to float at scrape time. `-1` means
    /// "not warmed up" (vol estimator is below `min_samples`).
    pub last_applied_vol_milli: AtomicI64,
    /// Number of price samples currently in the vol estimator window.
    pub vol_samples: AtomicU64,
    /// Init hints surfaced by the predictor on the last tick.
    pub last_init_hints: AtomicU64,
    /// Total actions planned by the scheduler on the last tick.
    pub last_actions_planned: AtomicU64,
    /// Unix timestamp (seconds) when the bot booted. `0` until the
    /// first time `observe_boot` is called.
    pub up_since_unix_secs: AtomicU64,
    /// Encoded `LeaderStatus`. See `set_leader_status`.
    pub leader_status: AtomicU8,
    /// Wallet balance (lamports). Production deployments populate
    /// this via a side-channel (the bot itself doesn't sign at
    /// the same time as it tracks balance; that's a leader concern).
    pub wallet_balance_lamports: AtomicU64,
}

impl KeeperMetrics {
    /// Construct an empty register. Equivalent to `default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the bot as up at the given unix time. Called once on
    /// boot.
    pub fn observe_boot(&self, unix_secs: u64) {
        self.up_since_unix_secs.store(unix_secs, Ordering::Relaxed);
    }

    /// Set the leader-status gauge.
    pub fn set_leader_status(&self, s: LeaderStatus) {
        self.leader_status.store(s as u8, Ordering::Relaxed);
    }

    /// Return the current leader status (decoded).
    pub fn leader_status(&self) -> LeaderStatus {
        LeaderStatus::from_u8(self.leader_status.load(Ordering::Relaxed))
    }

    /// Set the wallet-balance gauge.
    pub fn set_wallet_balance_lamports(&self, lamports: u64) {
        self.wallet_balance_lamports.store(lamports, Ordering::Relaxed);
    }

    /// Increment snapshot-error counter.
    pub fn observe_snapshot_error(&self) {
        self.snapshot_errors_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Roll up one tick's `TickReport` into the counters + gauges.
    pub fn observe_tick(&self, report: &TickReport, duration_ms: u64) {
        self.ticks_total.fetch_add(1, Ordering::Relaxed);
        self.last_tick_duration_ms.store(duration_ms, Ordering::Relaxed);
        self.last_init_hints
            .store(report.init_hints_added as u64, Ordering::Relaxed);
        self.last_actions_planned
            .store(report.actions_planned as u64, Ordering::Relaxed);
        self.init_hints_recorded_total
            .fetch_add(report.init_hints_added as u64, Ordering::Relaxed);

        let mut submitted = 0u64;
        let mut failed = 0u64;
        let mut skipped = 0u64;
        for (_action, r) in &report.dispatched {
            match r {
                ActionDispatchResult::Submitted { .. } => submitted += 1,
                ActionDispatchResult::Failed { .. } => failed += 1,
                ActionDispatchResult::Skipped { .. } => skipped += 1,
            }
        }
        self.actions_submitted_total
            .fetch_add(submitted, Ordering::Relaxed);
        self.actions_failed_total
            .fetch_add(failed, Ordering::Relaxed);
        self.actions_skipped_total
            .fetch_add(skipped, Ordering::Relaxed);

        match report.applied_vol {
            Some(v) => {
                let milli = (v * 1000.0).round() as i64;
                self.last_applied_vol_milli.store(milli, Ordering::Relaxed);
            }
            None => self
                .last_applied_vol_milli
                .store(APPLIED_VOL_NOT_WARM, Ordering::Relaxed),
        }
    }

    /// Update the vol-sample-count gauge. Called separately from
    /// `observe_tick` because the estimator state lives in the
    /// `KeeperBot` struct and the metrics register doesn't own it.
    pub fn set_vol_samples(&self, n: u64) {
        self.vol_samples.store(n, Ordering::Relaxed);
    }

    /// Render the full register as Prometheus text exposition.
    /// One allocation; safe to call from any thread including the
    /// tick loop (read-only over atomics).
    pub fn render_prometheus(&self) -> String {
        self.render_prometheus_with_labels(&[])
    }

    /// Wave 18 — labeled variant. Emits every metric line with the
    /// supplied `(key, value)` label pairs spliced into the metric
    /// expression so a multi-market bot can publish the same
    /// metric family multiple times under different `market` labels
    /// (e.g. `keeper_leader_status{market="SOL-USD"} 1`).
    ///
    /// Preserves the wave-12 single-`# HELP` / `# TYPE` per metric
    /// invariant — the labeled body is the only thing that changes.
    /// Pass an empty slice for the wave-12 unlabeled shape.
    ///
    /// The caller must guarantee:
    /// - Label keys are non-empty, valid Prometheus identifiers
    ///   (`[a-zA-Z_][a-zA-Z0-9_]*`).
    /// - Label values do NOT contain unescaped `"` / `\n` / `\\`.
    ///   We escape `\` and `"` defensively below; raw newlines are
    ///   stripped as a hard guarantee against malformed scrapes.
    pub fn render_prometheus_with_labels(&self, labels: &[(&str, &str)]) -> String {
        let mut out = String::with_capacity(2048 + labels.len() * 32);

        let label_block = format_label_block(labels);

        // Helper to emit one metric block (HELP + TYPE + value).
        fn write_counter(
            out: &mut String,
            name: &str,
            help: &str,
            labels: &str,
            value: u64,
        ) {
            out.push_str("# HELP ");
            out.push_str(name);
            out.push(' ');
            out.push_str(help);
            out.push('\n');
            out.push_str("# TYPE ");
            out.push_str(name);
            out.push_str(" counter\n");
            out.push_str(name);
            out.push_str(labels);
            out.push(' ');
            out.push_str(&value.to_string());
            out.push('\n');
        }
        fn write_gauge_u64(
            out: &mut String,
            name: &str,
            help: &str,
            labels: &str,
            value: u64,
        ) {
            out.push_str("# HELP ");
            out.push_str(name);
            out.push(' ');
            out.push_str(help);
            out.push('\n');
            out.push_str("# TYPE ");
            out.push_str(name);
            out.push_str(" gauge\n");
            out.push_str(name);
            out.push_str(labels);
            out.push(' ');
            out.push_str(&value.to_string());
            out.push('\n');
        }
        fn write_gauge_f64(
            out: &mut String,
            name: &str,
            help: &str,
            labels: &str,
            value: f64,
        ) {
            out.push_str("# HELP ");
            out.push_str(name);
            out.push(' ');
            out.push_str(help);
            out.push('\n');
            out.push_str("# TYPE ");
            out.push_str(name);
            out.push_str(" gauge\n");
            out.push_str(name);
            out.push_str(labels);
            out.push(' ');
            // Prometheus accepts standard floating-point formats.
            // Use a plain format so very small / very large values
            // serialise correctly.
            out.push_str(&format!("{value}"));
            out.push('\n');
        }

        write_counter(
            &mut out,
            "keeper_ticks_total",
            "Total ticks the bot has completed.",
            &label_block,
            self.ticks_total.load(Ordering::Relaxed),
        );
        write_counter(
            &mut out,
            "keeper_actions_submitted_total",
            "Cumulative actions whose tx submission was acknowledged.",
            &label_block,
            self.actions_submitted_total.load(Ordering::Relaxed),
        );
        write_counter(
            &mut out,
            "keeper_actions_failed_total",
            "Cumulative actions whose tx submission failed.",
            &label_block,
            self.actions_failed_total.load(Ordering::Relaxed),
        );
        write_counter(
            &mut out,
            "keeper_actions_skipped_total",
            "Cumulative actions skipped by the executor (e.g. unknown PDAs).",
            &label_block,
            self.actions_skipped_total.load(Ordering::Relaxed),
        );
        write_counter(
            &mut out,
            "keeper_init_hints_recorded_total",
            "Cumulative init hints surfaced by the predictor.",
            &label_block,
            self.init_hints_recorded_total.load(Ordering::Relaxed),
        );
        write_counter(
            &mut out,
            "keeper_snapshot_errors_total",
            "Cumulative snapshot fetch errors (any kind).",
            &label_block,
            self.snapshot_errors_total.load(Ordering::Relaxed),
        );

        write_gauge_u64(
            &mut out,
            "keeper_last_tick_duration_milliseconds",
            "Wall-clock duration of the most recent tick, in milliseconds.",
            &label_block,
            self.last_tick_duration_ms.load(Ordering::Relaxed),
        );
        let milli = self.last_applied_vol_milli.load(Ordering::Relaxed);
        if milli == APPLIED_VOL_NOT_WARM {
            write_gauge_f64(
                &mut out,
                "keeper_last_applied_vol",
                "σ̂ applied to the predictor on the last tick. NaN means warming up.",
                &label_block,
                f64::NAN,
            );
        } else {
            write_gauge_f64(
                &mut out,
                "keeper_last_applied_vol",
                "σ̂ applied to the predictor on the last tick. NaN means warming up.",
                &label_block,
                (milli as f64) / 1000.0,
            );
        }
        write_gauge_u64(
            &mut out,
            "keeper_vol_samples",
            "Number of price samples currently in the vol estimator window.",
            &label_block,
            self.vol_samples.load(Ordering::Relaxed),
        );
        write_gauge_u64(
            &mut out,
            "keeper_last_init_hints",
            "Init hints surfaced by the predictor on the last tick.",
            &label_block,
            self.last_init_hints.load(Ordering::Relaxed),
        );
        write_gauge_u64(
            &mut out,
            "keeper_last_actions_planned",
            "Actions emitted by the scheduler on the last tick.",
            &label_block,
            self.last_actions_planned.load(Ordering::Relaxed),
        );
        write_gauge_u64(
            &mut out,
            "keeper_up_since_unix_seconds",
            "Unix timestamp at which the bot last booted.",
            &label_block,
            self.up_since_unix_secs.load(Ordering::Relaxed),
        );
        write_gauge_u64(
            &mut out,
            "keeper_leader_status",
            "0=unknown, 1=leader, 2=standby.",
            &label_block,
            self.leader_status.load(Ordering::Relaxed) as u64,
        );
        write_gauge_u64(
            &mut out,
            "keeper_wallet_balance_lamports",
            "Most recently observed payer wallet balance, in lamports.",
            &label_block,
            self.wallet_balance_lamports.load(Ordering::Relaxed),
        );

        out
    }

    /// Wave 21 — render the metric register as a *stable* compact
    /// JSON object. Used by the new `/metrics-multi` route in
    /// `serve.rs` and by the `MarketRegistry::render_per_market_json`
    /// aggregator, which feeds the wave-20 frontend
    /// `MarketViewEntry.keeperState` directly (no Prometheus
    /// scrape-and-parse round-trip needed for multi-market UIs).
    ///
    /// Field name conventions match the wave-20
    /// `KeeperState` / `KeeperLoopMetrics` TypeScript surface
    /// (camelCase) so the frontend can `JSON.parse` straight into
    /// the existing types. Numbers are emitted as JSON numbers
    /// (no string-quoting) to keep the wire format simple.
    ///
    /// Stability guarantee: the field set is append-only. Future
    /// waves may add keys; never remove or rename.
    pub fn render_json_snapshot(&self) -> String {
        let mut out = String::with_capacity(384);
        out.push('{');
        let mut first = true;
        let push_u64 = |out: &mut String, first: &mut bool, key: &str, val: u64| {
            if !*first {
                out.push(',');
            }
            *first = false;
            out.push('"');
            out.push_str(key);
            out.push_str("\":");
            out.push_str(&val.to_string());
        };
        push_u64(
            &mut out,
            &mut first,
            "ticksTotal",
            self.ticks_total.load(Ordering::Relaxed),
        );
        push_u64(
            &mut out,
            &mut first,
            "actionsSubmittedTotal",
            self.actions_submitted_total.load(Ordering::Relaxed),
        );
        push_u64(
            &mut out,
            &mut first,
            "actionsFailedTotal",
            self.actions_failed_total.load(Ordering::Relaxed),
        );
        push_u64(
            &mut out,
            &mut first,
            "actionsSkippedTotal",
            self.actions_skipped_total.load(Ordering::Relaxed),
        );
        push_u64(
            &mut out,
            &mut first,
            "initHintsRecordedTotal",
            self.init_hints_recorded_total.load(Ordering::Relaxed),
        );
        push_u64(
            &mut out,
            &mut first,
            "snapshotErrorsTotal",
            self.snapshot_errors_total.load(Ordering::Relaxed),
        );
        push_u64(
            &mut out,
            &mut first,
            "lastTickDurationMs",
            self.last_tick_duration_ms.load(Ordering::Relaxed),
        );
        push_u64(
            &mut out,
            &mut first,
            "volSamples",
            self.vol_samples.load(Ordering::Relaxed),
        );
        push_u64(
            &mut out,
            &mut first,
            "lastInitHints",
            self.last_init_hints.load(Ordering::Relaxed),
        );
        push_u64(
            &mut out,
            &mut first,
            "lastActionsPlanned",
            self.last_actions_planned.load(Ordering::Relaxed),
        );
        push_u64(
            &mut out,
            &mut first,
            "upSinceUnixSecs",
            self.up_since_unix_secs.load(Ordering::Relaxed),
        );
        push_u64(
            &mut out,
            &mut first,
            "walletBalanceLamports",
            self.wallet_balance_lamports.load(Ordering::Relaxed),
        );

        // appliedVolMilli — `null` when not warmed up so the
        // frontend can branch on `=== null` instead of magic
        // sentinels.
        out.push_str(",\"appliedVolMilli\":");
        let v = self.last_applied_vol_milli.load(Ordering::Relaxed);
        if v == APPLIED_VOL_NOT_WARM {
            out.push_str("null");
        } else {
            out.push_str(&v.to_string());
        }

        // leaderStatus — emit as the same string the wave-12
        // Prometheus gauge would map to, so dashboards and the
        // JSON path agree.
        out.push_str(",\"leaderStatus\":\"");
        out.push_str(match self.leader_status() {
            LeaderStatus::Leader => "leader",
            LeaderStatus::Standby => "standby",
            LeaderStatus::Unknown => "unknown",
        });
        out.push('"');

        out.push('}');
        out
    }
}

/// Wave 18 — render `{k1="v1",k2="v2"}` for the supplied label
/// pairs, escaping `\` and `"` and stripping literal newlines.
/// Empty slice returns an empty string so the wave-12 unlabeled
/// shape is byte-identical to before.
fn format_label_block(labels: &[(&str, &str)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let mut s = String::with_capacity(labels.len() * 24);
    s.push('{');
    for (i, (k, v)) in labels.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(k);
        s.push_str("=\"");
        for c in v.chars() {
            match c {
                '\\' => s.push_str("\\\\"),
                '"' => s.push_str("\\\""),
                '\n' => {} // strip raw newlines defensively
                _ => s.push(c),
            }
        }
        s.push('"');
    }
    s.push('}');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use clearing_core::Direction;
    use keeper::KeeperAction;

    fn fake_action(sub_pool_id: u32) -> KeeperAction {
        KeeperAction::CloseDormantBucket {
            sub_pool_id,
            direction: Direction::Long,
            tick: 0,
        }
    }

    fn report_with(submitted: usize, failed: usize, skipped: usize) -> TickReport {
        let mut dispatched = Vec::new();
        for i in 0..submitted {
            dispatched.push((
                fake_action(i as u32),
                ActionDispatchResult::Submitted { signature: None },
            ));
        }
        for i in 0..failed {
            dispatched.push((
                fake_action((submitted + i) as u32),
                ActionDispatchResult::Failed {
                    reason: "test".into(),
                },
            ));
        }
        for i in 0..skipped {
            dispatched.push((
                fake_action((submitted + failed + i) as u32),
                ActionDispatchResult::Skipped { reason: "test" },
            ));
        }
        TickReport {
            actions_planned: submitted + failed + skipped,
            dispatched,
            applied_vol: Some(1.234),
            init_hints_added: 2,
        }
    }

    #[test]
    fn observe_tick_increments_counters_and_gauges() {
        let m = KeeperMetrics::new();
        let r = report_with(3, 1, 2);
        m.observe_tick(&r, 95);
        m.observe_tick(&r, 105);

        assert_eq!(m.ticks_total.load(Ordering::Relaxed), 2);
        assert_eq!(m.actions_submitted_total.load(Ordering::Relaxed), 6);
        assert_eq!(m.actions_failed_total.load(Ordering::Relaxed), 2);
        assert_eq!(m.actions_skipped_total.load(Ordering::Relaxed), 4);
        assert_eq!(m.init_hints_recorded_total.load(Ordering::Relaxed), 4);
        assert_eq!(m.last_tick_duration_ms.load(Ordering::Relaxed), 105);
        assert_eq!(m.last_actions_planned.load(Ordering::Relaxed), 6);
        // 1.234 → 1234 milli
        assert_eq!(m.last_applied_vol_milli.load(Ordering::Relaxed), 1234);
    }

    #[test]
    fn observe_tick_records_warming_up_vol() {
        let m = KeeperMetrics::new();
        let mut r = report_with(0, 0, 0);
        r.applied_vol = None;
        m.observe_tick(&r, 50);
        assert_eq!(
            m.last_applied_vol_milli.load(Ordering::Relaxed),
            APPLIED_VOL_NOT_WARM
        );
    }

    #[test]
    fn render_prometheus_emits_help_and_type_per_metric() {
        let m = KeeperMetrics::new();
        m.observe_tick(&report_with(2, 1, 0), 80);
        m.set_leader_status(LeaderStatus::Leader);
        m.observe_boot(1_700_000_000);
        m.set_vol_samples(17);
        let text = m.render_prometheus();

        // Spot-check the produced format.
        assert!(text.contains("# HELP keeper_ticks_total"));
        assert!(text.contains("# TYPE keeper_ticks_total counter"));
        assert!(text.contains("\nkeeper_ticks_total 1\n"));
        assert!(text.contains("# TYPE keeper_actions_submitted_total counter"));
        assert!(text.contains("\nkeeper_actions_submitted_total 2\n"));
        assert!(text.contains("# TYPE keeper_leader_status gauge"));
        assert!(text.contains("\nkeeper_leader_status 1\n"));
        assert!(text.contains("\nkeeper_up_since_unix_seconds 1700000000\n"));
        assert!(text.contains("\nkeeper_vol_samples 17\n"));

        // Every emitted line either is empty, starts with "#", or
        // is `<name> <value>` — Prometheus parsers reject anything
        // else.
        for (i, line) in text.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.splitn(2, ' ').collect();
            assert_eq!(
                parts.len(),
                2,
                "line {i} `{line}` is not `<name> <value>`",
            );
        }
    }

    #[test]
    fn warming_up_vol_renders_as_nan() {
        let m = KeeperMetrics::new();
        let mut r = report_with(0, 0, 0);
        r.applied_vol = None;
        m.observe_tick(&r, 10);
        let text = m.render_prometheus();
        assert!(text.contains("keeper_last_applied_vol NaN\n"));
    }

    #[test]
    fn leader_status_round_trips() {
        let m = KeeperMetrics::new();
        assert_eq!(m.leader_status(), LeaderStatus::Unknown);
        m.set_leader_status(LeaderStatus::Leader);
        assert_eq!(m.leader_status(), LeaderStatus::Leader);
        m.set_leader_status(LeaderStatus::Standby);
        assert_eq!(m.leader_status(), LeaderStatus::Standby);
    }

    #[test]
    fn snapshot_errors_counter_increments() {
        let m = KeeperMetrics::new();
        m.observe_snapshot_error();
        m.observe_snapshot_error();
        assert_eq!(m.snapshot_errors_total.load(Ordering::Relaxed), 2);
    }

    /// Wallet balance is set externally (the leader writes it on
    /// each tick). The metric must reflect the latest write
    /// regardless of how many writes preceded it.
    #[test]
    fn wallet_balance_reflects_latest_write() {
        let m = KeeperMetrics::new();
        m.set_wallet_balance_lamports(1_000_000_000);
        m.set_wallet_balance_lamports(2_500_000_000);
        let text = m.render_prometheus();
        assert!(text.contains("\nkeeper_wallet_balance_lamports 2500000000\n"));
    }

    /// Wave 18 — labeled rendering splices `{market="…"}` into
    /// every metric expression. Validates that the wave-12
    /// unlabeled shape AND the wave-18 labeled shape both
    /// satisfy the `# HELP` / `# TYPE` once-per-metric invariant.
    #[test]
    fn render_prometheus_with_labels_splices_market_dimension() {
        let m = KeeperMetrics::new();
        m.observe_tick(&report_with(2, 0, 0), 50);
        m.set_leader_status(LeaderStatus::Leader);
        m.set_wallet_balance_lamports(7_000_000);

        let text = m.render_prometheus_with_labels(&[("market", "SOL-USD")]);
        assert!(text.contains("# HELP keeper_ticks_total"));
        assert!(text.contains("# TYPE keeper_ticks_total counter"));
        assert!(text.contains("\nkeeper_ticks_total{market=\"SOL-USD\"} 1\n"));
        assert!(
            text.contains("\nkeeper_actions_submitted_total{market=\"SOL-USD\"} 2\n")
        );
        assert!(text.contains("\nkeeper_leader_status{market=\"SOL-USD\"} 1\n"));
        assert!(
            text.contains("\nkeeper_wallet_balance_lamports{market=\"SOL-USD\"} 7000000\n")
        );

        // Empty-label call must produce the wave-12 shape verbatim.
        let plain = m.render_prometheus_with_labels(&[]);
        assert_eq!(plain, m.render_prometheus());
    }

    /// Wave 18 — multiple label pairs are emitted in insertion
    /// order separated by commas. Backslash and quote in label
    /// values are escaped; raw newlines are stripped.
    #[test]
    fn render_prometheus_with_multi_labels_orders_and_escapes() {
        let m = KeeperMetrics::new();
        m.observe_tick(&report_with(1, 0, 0), 10);

        let text = m.render_prometheus_with_labels(&[
            ("market", "SOL-USD"),
            ("region", "ap-east-1"),
        ]);
        assert!(text.contains(
            "\nkeeper_ticks_total{market=\"SOL-USD\",region=\"ap-east-1\"} 1\n"
        ));

        // Escaping check.
        let escaped = m.render_prometheus_with_labels(&[("market", r#"a"b\c"#)]);
        assert!(escaped.contains(
            r#"keeper_ticks_total{market="a\"b\\c"} 1"#
        ));

        // Newline stripping check.
        let stripped = m.render_prometheus_with_labels(&[("market", "x\ny")]);
        assert!(stripped.contains("keeper_ticks_total{market=\"xy\"} 1"));
    }
}
