//! Wave 18 — multi-market ops-toolkit scanner.
//!
//! The wave-12..17 prober runs `run_all_checks(&HealthContext)`
//! against ONE market. Multi-market deployments need the same
//! probe to cover N markets and aggregate the results so a single
//! exit code drives AlertManager.
//!
//! Strategy:
//!
//! - **Config-driven** — load a [`keeper_rpc::MarketRegistry`]
//!   from TOML (wave-18 schema). The registry already carries
//!   `expected_leader` per market; the scanner pipes it straight
//!   into [`LeaderLockFacts::expected_leader`].
//! - **Caller supplies the per-market `HealthContext`** — same
//!   shape the prober uses today. A closure decides how to fetch
//!   the per-market facts (cluster getMultipleAccounts, internal
//!   SDK call, fixture). The scanner fans out, runs the standard
//!   21-check battery, and aggregates.
//! - **Output** — a [`MultiMarketHealthReport`] with one
//!   `HealthReport` per market plus the worst overall exit code.
//!   `render_json_multi` produces a stable wire format the wave-12
//!   AlertManager pipeline can consume.

use crate::context::{HealthContext, LeaderLockFacts};
use crate::report::{exit_code_for_status, render_json, HealthReport};
use crate::run_all_checks;

pub use keeper_rpc::{MarketRegistry, MarketEntry, RegistryError};

/// Wave 18 — aggregated multi-market scan output.
#[derive(Debug, Clone)]
pub struct MultiMarketHealthReport {
    /// One entry per scanned market, in registry insertion order.
    pub per_market: Vec<MarketScanResult>,
    /// Worst exit code across all per-market reports. AlertManager
    /// uses this to drive paging tier (matching the wave-12
    /// single-market `exit_code_for_status` semantics).
    pub worst_exit_code: i32,
}

/// One market's scan result (`(symbol, report)`).
#[derive(Debug, Clone)]
pub struct MarketScanResult {
    /// Market symbol (matches the registry entry).
    pub symbol: String,
    /// 21-check report (wave-17 semantics).
    pub report: HealthReport,
    /// `exit_code_for_status(&report)` — surfaced so callers don't
    /// recompute it.
    pub exit_code: i32,
}

/// Wave 18 — scanner errors. Wraps registry errors plus a
/// per-market builder error so the orchestrator can decide whether
/// the failure is "this one market is unreachable" (skip) vs "the
/// whole config is unparseable" (abort).
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// Failed to parse the registry TOML.
    #[error("registry parse error: {0}")]
    Registry(#[from] RegistryError),
    /// At least one market's `HealthContext` builder returned `Err`.
    #[error("market `{symbol}` context builder failed: {detail}")]
    Builder {
        /// Market symbol that failed.
        symbol: String,
        /// Underlying reason.
        detail: String,
    },
}

/// Wave 18 — scan every registered market.
///
/// `make_ctx` is invoked once per `MarketEntry` to produce the
/// per-market `HealthContext`. The scanner injects the entry's
/// `expected_leader` into the returned context's
/// `leader_lock.expected_leader` BEFORE running checks, so callers
/// don't have to thread that wiring themselves — they just need to
/// build the live `LeaderLockFacts` with `initialized` /
/// `has_leader` / `current_leader` / `last_heartbeat_slot` /
/// `current_slot` and leave `expected_leader: None`.
///
/// On `Err` from the closure the entire scan aborts with
/// [`ScanError::Builder`]. Callers that want partial-success
/// (skip-this-market-and-continue) wrap the error themselves and
/// return `Ok(ctx_with_marker_facts)`.
pub fn scan_all_markets<F>(
    registry: &MarketRegistry,
    mut make_ctx: F,
) -> Result<MultiMarketHealthReport, ScanError>
where
    F: FnMut(&MarketEntry) -> Result<HealthContext, String>,
{
    let mut per_market = Vec::with_capacity(registry.len());
    let mut worst_exit_code = 0;
    for entry in registry.iter() {
        let mut ctx = make_ctx(entry).map_err(|detail| ScanError::Builder {
            symbol: entry.symbol.clone(),
            detail,
        })?;
        // Wave 18 — inject expected_leader from the registry. We
        // overwrite *only* if the closure left it empty so test
        // fixtures with explicit values still behave deterministically.
        if let Some(expected) = entry.expected_leader {
            if let Some(facts) = ctx.leader_lock.as_mut() {
                if facts.expected_leader.is_none() {
                    facts.expected_leader = Some(expected);
                }
            }
        }
        let report = run_all_checks(&ctx);
        let code = exit_code_for_status(&report);
        if code > worst_exit_code {
            worst_exit_code = code;
        }
        per_market.push(MarketScanResult {
            symbol: entry.symbol.clone(),
            report,
            exit_code: code,
        });
    }
    Ok(MultiMarketHealthReport {
        per_market,
        worst_exit_code,
    })
}

/// Wave 18 — render a multi-market report as a JSON object keyed
/// by symbol with a top-level `worst_exit_code` field.
///
/// Stable wire format:
///
/// ```json
/// {
///   "worst_exit_code": 4,
///   "markets": {
///     "SOL-USD": { ... HealthReport ... },
///     "BTC-USD": { ... HealthReport ... }
///   }
/// }
/// ```
pub fn render_json_multi(report: &MultiMarketHealthReport) -> String {
    let mut out = String::with_capacity(8 * 1024);
    out.push_str("{\"worst_exit_code\":");
    out.push_str(&report.worst_exit_code.to_string());
    out.push_str(",\"markets\":{");
    for (i, m) in report.per_market.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        push_json_str(&mut out, &m.symbol);
        out.push_str("\":");
        out.push_str(&render_json(&m.report));
    }
    out.push('}');
    // Wave 29 — embed the protocol-wide rollup so a single read of the
    // daemon's snapshot answers "is the whole protocol healthy?" without
    // the consumer re-folding every per-market report. Mirrors the
    // frontend Overview page's protocol pulse.
    out.push_str(",\"protocol\":");
    out.push_str(&crate::protocol_summary::render_protocol_summary_json(
        &crate::protocol_summary::summarize_protocol(report),
    ));
    out.push('}');
    out
}

fn push_json_str(buf: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => buf.push_str("\\\""),
            '\\' => buf.push_str("\\\\"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\t' => buf.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                buf.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => buf.push(c),
        }
    }
}

/// Wave 18 — short-form helper: build a `HealthContext` ctx from a
/// caller-supplied "live facts" closure that ONLY produces the
/// `LeaderLockFacts` plus borrowed defaults for the rest of the
/// HealthContext fields (market / sub_pools / keeper / rpc /
/// oracle / pool).
///
/// Used by the wave-18 demo + integration tests where the rest of
/// the context is identical across markets.
pub fn ctx_with_leader_lock(
    base: &HealthContext,
    facts: LeaderLockFacts,
) -> HealthContext {
    HealthContext {
        leader_lock: Some(facts),
        ..base.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{
        HealthContext, KeeperFacts, MarketFacts, OracleFacts, PoolFacts, RpcFacts, SubPoolFacts,
    };

    fn happy_base_ctx() -> HealthContext {
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

    fn registry_two_markets(with_expected: bool) -> MarketRegistry {
        const PROGRAM: &str = "11111111111111111111111111111112";
        const MARKET_A: &str = "Sysvar1nstructions1111111111111111111111111";
        const MARKET_B: &str = "SysvarC1ock11111111111111111111111111111111";
        let expected_line = if with_expected {
            format!("expected_leader = \"{MARKET_A}\"\n")
        } else {
            String::new()
        };
        let toml = format!(
            "[[markets]]\nsymbol = \"SOL-USD\"\nprogram_id = \"{PROGRAM}\"\nmarket_pda = \"{MARKET_A}\"\nlock_pda = \"{MARKET_B}\"\n{expected_line}\n[[markets]]\nsymbol = \"BTC-USD\"\nprogram_id = \"{PROGRAM}\"\nmarket_pda = \"{MARKET_B}\"\nlock_pda = \"{MARKET_A}\"\n"
        );
        MarketRegistry::from_toml_str(&toml).expect("parse")
    }

    #[test]
    fn scan_two_markets_happy_path() {
        let registry = registry_two_markets(false);
        let report = scan_all_markets(&registry, |_| Ok(happy_base_ctx()))
            .expect("scan ok");
        assert_eq!(report.per_market.len(), 2);
        assert_eq!(report.per_market[0].symbol, "SOL-USD");
        assert_eq!(report.per_market[1].symbol, "BTC-USD");
        assert_eq!(report.worst_exit_code, 0);
    }

    #[test]
    fn scan_injects_expected_leader_from_registry() {
        let registry = registry_two_markets(true);
        let report = scan_all_markets(&registry, |entry| {
            let mut ctx = happy_base_ctx();
            // Only SOL-USD has live leader-lock facts. BTC-USD
            // intentionally leaves `leader_lock = None` so we can
            // verify the wave-17 "skipped → Pass" semantics still
            // hold for markets whose lock isn't probed.
            if entry.symbol == "SOL-USD" {
                ctx.leader_lock = Some(LeaderLockFacts {
                    initialized: true,
                    has_leader: true,
                    current_leader: [0xaa; 32],
                    last_heartbeat_slot: 100,
                    takeover_threshold_slots: 75,
                    current_slot: 150,
                    expected_leader: None,
                });
            }
            Ok(ctx)
        })
        .expect("scan ok");
        // SOL-USD: holder 0xaa.. but expected_leader (registry value)
        // is the program pubkey → mismatch check flips to non-pass.
        let sol = &report.per_market[0];
        let mismatch_check = sol
            .report
            .checks
            .iter()
            .find(|c| c.name == "keeper_leader_lock_holder_matches_expected")
            .expect("mismatch check present");
        assert!(
            !matches!(mismatch_check.status, crate::report::CheckStatus::Pass),
            "expected non-pass status when holder differs from expected, got {:?}",
            mismatch_check.status,
        );
        // BTC-USD: leader_lock left None → wave-17 skipped semantics
        // → Pass.
        let btc = &report.per_market[1];
        let btc_mismatch = btc
            .report
            .checks
            .iter()
            .find(|c| c.name == "keeper_leader_lock_holder_matches_expected")
            .expect("present");
        assert!(matches!(btc_mismatch.status, crate::report::CheckStatus::Pass));
    }

    #[test]
    fn scan_propagates_builder_errors() {
        let registry = registry_two_markets(false);
        let err = scan_all_markets(&registry, |entry| {
            if entry.symbol == "BTC-USD" {
                Err("simulated rpc unreachable".to_string())
            } else {
                Ok(happy_base_ctx())
            }
        })
        .expect_err("must abort on builder error");
        match err {
            ScanError::Builder { symbol, detail } => {
                assert_eq!(symbol, "BTC-USD");
                assert!(detail.contains("simulated rpc unreachable"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn render_json_multi_emits_keyed_object() {
        let registry = registry_two_markets(false);
        let report = scan_all_markets(&registry, |_| Ok(happy_base_ctx())).unwrap();
        let json = render_json_multi(&report);
        assert!(json.starts_with("{\"worst_exit_code\":"));
        assert!(json.contains("\"SOL-USD\""));
        assert!(json.contains("\"BTC-USD\""));
        // Symbol with embedded quote characters wouldn't naively
        // appear in our registry (validate_symbol caps to ASCII),
        // but we still escape defensively. Spot-check the key
        // ordering matches insertion order.
        let sol_idx = json.find("\"SOL-USD\"").unwrap();
        let btc_idx = json.find("\"BTC-USD\"").unwrap();
        assert!(sol_idx < btc_idx, "SOL-USD must precede BTC-USD");
        // Wave 29 — protocol rollup block is embedded after markets.
        assert!(json.contains("\"protocol\":{\"markets\":2"));
        assert!(json.contains("\"overall_status\":\"PASS\""));
        assert!(json.ends_with("}"));
    }

    #[test]
    fn render_json_multi_protocol_block_reflects_critical() {
        let registry = registry_two_markets(false);
        let report = scan_all_markets(&registry, |entry| {
            let mut ctx = happy_base_ctx();
            if entry.symbol == "BTC-USD" {
                ctx.market.paused_globally = true;
            }
            Ok(ctx)
        })
        .unwrap();
        let json = render_json_multi(&report);
        assert!(json.contains("\"protocol\":{\"markets\":2"));
        assert!(json.contains("\"critical_markets\":1"));
        assert!(json.contains("\"overall_status\":\"CRITICAL\""));
    }
}
