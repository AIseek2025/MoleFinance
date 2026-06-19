//! Output types: `CheckResult`, `HealthReport`, JSON / Prometheus
//! textfile rendering, exit-code derivation.
//!
//! ## Severity ordering
//!
//! `Severity` orders P0 (highest) > P1 > P2 > P3 (lowest). When
//! aggregating multiple checks, the report's overall severity is
//! the *worst* check's severity that fired Critical or Warn.

/// Pager severity tier. Lower numerically means more urgent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Page on-call immediately, walk the cat.
    P0 = 0,
    /// Page on-call within the SLA.
    P1 = 1,
    /// File ticket, investigate within SLA.
    P2 = 2,
    /// Log only.
    P3 = 3,
}

impl Severity {
    /// `"P0"` / `"P1"` / `"P2"` / `"P3"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::P0 => "P0",
            Severity::P1 => "P1",
            Severity::P2 => "P2",
            Severity::P3 => "P3",
        }
    }
}

/// Outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// Threshold(s) all clear.
    Pass,
    /// Threshold(s) close to firing — operator should investigate
    /// soon but no immediate action required.
    Warn,
    /// Threshold(s) breached — immediate action required.
    Critical,
}

impl CheckStatus {
    /// `"PASS"` / `"WARN"` / `"CRITICAL"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Critical => "CRITICAL",
        }
    }
    /// Internal ranking for "max status" reductions.
    fn rank(&self) -> u8 {
        match self {
            CheckStatus::Pass => 0,
            CheckStatus::Warn => 1,
            CheckStatus::Critical => 2,
        }
    }
    /// Worst of two statuses.
    pub fn worst(a: Self, b: Self) -> Self {
        if a.rank() >= b.rank() { a } else { b }
    }
}

/// Per-check result.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// Check identifier (matches `CHECK_NAMES`).
    pub name: &'static str,
    /// Status determined by the check.
    pub status: CheckStatus,
    /// Severity tier the check would trigger at if it went
    /// Critical. P-tier doesn't change between Pass / Warn /
    /// Critical — what changes is whether it actually fires.
    pub severity: Severity,
    /// Human-readable rationale shown in alert payloads.
    pub message: String,
    /// Numeric measurements the check observed (e.g. the actual
    /// value vs. the threshold). Surfaced verbatim in the JSON
    /// output for debugging.
    pub measurements: Vec<(&'static str, f64)>,
}

/// Aggregate of every check run in one probe cycle.
#[derive(Debug, Clone)]
pub struct HealthReport {
    /// Unix-seconds the report was produced.
    pub timestamp_unix: u64,
    /// Per-check results, in `CHECK_NAMES` order.
    pub checks: Vec<CheckResult>,
}

impl HealthReport {
    /// Worst status across all checks.
    pub fn overall_status(&self) -> CheckStatus {
        self.checks.iter().fold(CheckStatus::Pass, |acc, r| {
            CheckStatus::worst(acc, r.status)
        })
    }

    /// Highest severity that's actually firing (status >= Warn).
    /// Returns `None` if every check is Pass.
    pub fn highest_firing_severity(&self) -> Option<Severity> {
        self.checks
            .iter()
            .filter(|r| r.status != CheckStatus::Pass)
            .map(|r| r.severity)
            .min() // Severity has Ord with P0 < P1 < … so min == most urgent
    }

    /// Number of checks at each status. Returned as
    /// `(pass, warn, critical)`.
    pub fn count_by_status(&self) -> (usize, usize, usize) {
        let mut p = 0;
        let mut w = 0;
        let mut c = 0;
        for r in &self.checks {
            match r.status {
                CheckStatus::Pass => p += 1,
                CheckStatus::Warn => w += 1,
                CheckStatus::Critical => c += 1,
            }
        }
        (p, w, c)
    }
}

/// Map a [`HealthReport`] into a process exit code.
///
/// - `0` — every check passed
/// - `1` — at least one Warn at any severity (includes P3 critical)
/// - `2` — at least one P2 Critical
/// - `3` — at least one P1 Critical
/// - `4` — at least one P0 Critical
pub fn exit_code_for_status(report: &HealthReport) -> i32 {
    let mut highest: i32 = 0;
    for r in &report.checks {
        match (r.status, r.severity) {
            (CheckStatus::Pass, _) => {}
            (CheckStatus::Critical, Severity::P0) => highest = highest.max(4),
            (CheckStatus::Critical, Severity::P1) => highest = highest.max(3),
            (CheckStatus::Critical, Severity::P2) => highest = highest.max(2),
            (CheckStatus::Critical, Severity::P3) => highest = highest.max(1),
            (CheckStatus::Warn, _) => highest = highest.max(1),
        }
    }
    highest
}

/// JSON-encode the report (no external dependencies).
///
/// Output shape:
///
/// ```json
/// {
///   "timestamp_unix": 1700000000,
///   "overall_status": "WARN",
///   "highest_firing_severity": "P1",
///   "counts": { "pass": 12, "warn": 4, "critical": 2 },
///   "checks": [
///     {
///       "name": "market_not_paused",
///       "status": "PASS",
///       "severity": "P0",
///       "message": "...",
///       "measurements": { "paused": 0 }
///     }
///   ]
/// }
/// ```
pub fn render_json(report: &HealthReport) -> String {
    let mut out = String::with_capacity(2048);
    out.push('{');
    write_kv_u64(&mut out, "timestamp_unix", report.timestamp_unix, true);
    write_kv_str(&mut out, "overall_status", report.overall_status().as_str(), false);
    let firing = match report.highest_firing_severity() {
        Some(s) => s.as_str(),
        None => "NONE",
    };
    write_kv_str(&mut out, "highest_firing_severity", firing, false);
    let (p, w, c) = report.count_by_status();
    out.push_str(",\"counts\":{");
    write_kv_u64(&mut out, "pass", p as u64, true);
    write_kv_u64(&mut out, "warn", w as u64, false);
    write_kv_u64(&mut out, "critical", c as u64, false);
    out.push('}');
    out.push_str(",\"checks\":[");
    for (i, r) in report.checks.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('{');
        write_kv_str(&mut out, "name", r.name, true);
        write_kv_str(&mut out, "status", r.status.as_str(), false);
        write_kv_str(&mut out, "severity", r.severity.as_str(), false);
        write_kv_str(&mut out, "message", &escape_json_string(&r.message), false);
        out.push_str(",\"measurements\":{");
        for (j, (k, v)) in r.measurements.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            write_kv_f64(&mut out, k, *v, true);
        }
        out.push('}');
        out.push('}');
    }
    out.push(']');
    out.push('}');
    out
}

/// Prometheus textfile-collector format (for the node_exporter
/// textfile collector or VictoriaMetrics' equivalent). One gauge
/// per check: `mole_health_check_status{name="…",severity="P1"} 0|1|2`
/// (0 = Pass, 1 = Warn, 2 = Critical).
pub fn render_prometheus_textfile(report: &HealthReport) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("# HELP mole_health_check_status 0=PASS, 1=WARN, 2=CRITICAL.\n");
    out.push_str("# TYPE mole_health_check_status gauge\n");
    for r in &report.checks {
        let v = r.status.rank();
        // Prometheus labels: name="…",severity="P1"
        // No special chars in our `name`s so escaping is unneeded.
        out.push_str("mole_health_check_status{name=\"");
        out.push_str(r.name);
        out.push_str("\",severity=\"");
        out.push_str(r.severity.as_str());
        out.push_str("\"} ");
        out.push_str(&v.to_string());
        out.push('\n');
    }
    out.push_str("# HELP mole_health_overall 0=PASS, 1=WARN, 2=CRITICAL.\n");
    out.push_str("# TYPE mole_health_overall gauge\n");
    out.push_str("mole_health_overall ");
    out.push_str(&report.overall_status().rank().to_string());
    out.push('\n');
    out
}

/* ------------------------- private helpers ------------------------- */

fn write_kv_u64(out: &mut String, k: &str, v: u64, first: bool) {
    if !first {
        out.push(',');
    }
    out.push('"');
    out.push_str(k);
    out.push_str("\":");
    out.push_str(&v.to_string());
}

fn write_kv_f64(out: &mut String, k: &str, v: f64, first: bool) {
    if !first {
        out.push(',');
    }
    out.push('"');
    out.push_str(k);
    out.push_str("\":");
    if v.is_finite() {
        out.push_str(&format!("{v}"));
    } else if v.is_nan() {
        out.push_str("\"NaN\"");
    } else if v.is_sign_positive() {
        out.push_str("\"+Infinity\"");
    } else {
        out.push_str("\"-Infinity\"");
    }
}

fn write_kv_str(out: &mut String, k: &str, v: &str, first: bool) {
    if !first {
        out.push(',');
    }
    out.push('"');
    out.push_str(k);
    out.push_str("\":\"");
    out.push_str(v);
    out.push('"');
}

fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(name: &'static str, sev: Severity) -> CheckResult {
        CheckResult {
            name,
            status: CheckStatus::Pass,
            severity: sev,
            message: "ok".to_string(),
            measurements: vec![],
        }
    }
    fn warn(name: &'static str, sev: Severity) -> CheckResult {
        CheckResult {
            name,
            status: CheckStatus::Warn,
            severity: sev,
            message: "near limit".to_string(),
            measurements: vec![("value", 1.5)],
        }
    }
    fn critical(name: &'static str, sev: Severity) -> CheckResult {
        CheckResult {
            name,
            status: CheckStatus::Critical,
            severity: sev,
            message: "fired".to_string(),
            measurements: vec![("value", 9.9)],
        }
    }

    #[test]
    fn worst_status_promotes_critical_above_warn() {
        let report = HealthReport {
            timestamp_unix: 0,
            checks: vec![
                pass("a", Severity::P0),
                warn("b", Severity::P1),
                critical("c", Severity::P2),
                pass("d", Severity::P3),
            ],
        };
        assert_eq!(report.overall_status(), CheckStatus::Critical);
    }

    #[test]
    fn highest_firing_severity_returns_p0_when_present() {
        let report = HealthReport {
            timestamp_unix: 0,
            checks: vec![
                warn("a", Severity::P3),
                critical("b", Severity::P0),
                warn("c", Severity::P1),
            ],
        };
        assert_eq!(report.highest_firing_severity(), Some(Severity::P0));
    }

    #[test]
    fn highest_firing_severity_is_none_when_all_pass() {
        let report = HealthReport {
            timestamp_unix: 0,
            checks: vec![pass("a", Severity::P0), pass("b", Severity::P1)],
        };
        assert_eq!(report.highest_firing_severity(), None);
    }

    #[test]
    fn exit_code_p0_critical_is_4() {
        let report = HealthReport {
            timestamp_unix: 0,
            checks: vec![critical("a", Severity::P0)],
        };
        assert_eq!(exit_code_for_status(&report), 4);
    }

    #[test]
    fn exit_code_p1_critical_is_3() {
        let report = HealthReport {
            timestamp_unix: 0,
            checks: vec![critical("a", Severity::P1)],
        };
        assert_eq!(exit_code_for_status(&report), 3);
    }

    #[test]
    fn exit_code_warn_only_is_1() {
        let report = HealthReport {
            timestamp_unix: 0,
            checks: vec![pass("a", Severity::P0), warn("b", Severity::P1)],
        };
        assert_eq!(exit_code_for_status(&report), 1);
    }

    #[test]
    fn exit_code_picks_max_across_checks() {
        let report = HealthReport {
            timestamp_unix: 0,
            checks: vec![
                critical("a", Severity::P2),
                critical("b", Severity::P0),
                warn("c", Severity::P3),
            ],
        };
        // P0 critical wins: exit 4
        assert_eq!(exit_code_for_status(&report), 4);
    }

    #[test]
    fn json_render_includes_all_fields() {
        let report = HealthReport {
            timestamp_unix: 1_700_000_000,
            checks: vec![
                pass("p", Severity::P0),
                critical("c", Severity::P1),
            ],
        };
        let out = render_json(&report);
        assert!(out.contains("\"timestamp_unix\":1700000000"));
        assert!(out.contains("\"overall_status\":\"CRITICAL\""));
        assert!(out.contains("\"highest_firing_severity\":\"P1\""));
        assert!(out.contains("\"counts\":{\"pass\":1,\"warn\":0,\"critical\":1}"));
        assert!(out.contains("\"name\":\"p\""));
        assert!(out.contains("\"name\":\"c\""));
        assert!(out.contains("\"status\":\"CRITICAL\""));
        // Output starts with `{` and ends with `}` (basic JSON shape).
        assert!(out.starts_with('{'));
        assert!(out.ends_with('}'));
    }

    #[test]
    fn json_escapes_special_chars_in_message() {
        let cr = CheckResult {
            name: "x",
            status: CheckStatus::Warn,
            severity: Severity::P1,
            message: r#"line1
"quoted"	tab"#
                .to_string(),
            measurements: vec![],
        };
        let report = HealthReport {
            timestamp_unix: 0,
            checks: vec![cr],
        };
        let out = render_json(&report);
        // The newline is escaped to \n (not embedded literally).
        assert!(out.contains("\\n"));
        // The double-quote is escaped to \".
        assert!(out.contains("\\\"quoted\\\""));
        // The tab is escaped.
        assert!(out.contains("\\t"));
        // Verify we can find the original substring before the
        // newline.
        assert!(out.contains("line1"));
    }

    #[test]
    fn prometheus_textfile_emits_help_type_and_per_check_lines() {
        let report = HealthReport {
            timestamp_unix: 0,
            checks: vec![
                pass("a", Severity::P0),
                warn("b", Severity::P1),
                critical("c", Severity::P2),
            ],
        };
        let out = render_prometheus_textfile(&report);
        assert!(out.contains("# HELP mole_health_check_status"));
        assert!(out.contains("# TYPE mole_health_check_status gauge"));
        assert!(out.contains(
            "mole_health_check_status{name=\"a\",severity=\"P0\"} 0\n"
        ));
        assert!(out.contains(
            "mole_health_check_status{name=\"b\",severity=\"P1\"} 1\n"
        ));
        assert!(out.contains(
            "mole_health_check_status{name=\"c\",severity=\"P2\"} 2\n"
        ));
        assert!(out.contains("mole_health_overall 2\n"));
    }

    /// Basic structural sanity — the JSON output must be parseable
    /// at least at the bracket level. We don't pull serde_json in
    /// as a dep, so we walk the string and assert balanced
    /// brackets / quotes.
    #[test]
    fn json_brackets_balance() {
        let report = HealthReport {
            timestamp_unix: 1,
            checks: vec![
                pass("a", Severity::P0),
                warn("b", Severity::P1),
                critical("c", Severity::P2),
            ],
        };
        let out = render_json(&report);
        let mut depth = 0i32;
        let mut bracket_depth = 0i32;
        let mut in_string = false;
        let mut prev_escape = false;
        for c in out.chars() {
            if in_string {
                if c == '"' && !prev_escape {
                    in_string = false;
                }
                prev_escape = c == '\\' && !prev_escape;
                continue;
            }
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                '[' => bracket_depth += 1,
                ']' => bracket_depth -= 1,
                '"' => in_string = true,
                _ => {}
            }
            prev_escape = false;
        }
        assert_eq!(depth, 0, "unbalanced braces in {out}");
        assert_eq!(bracket_depth, 0, "unbalanced square-brackets in {out}");
    }
}
