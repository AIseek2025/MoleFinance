//! Wave 28 — protocol-wide headline rollup.
//!
//! `MultiMarketHealthReport` (wave 18) carries one `HealthReport` per
//! market plus a `worst_exit_code`. Operators staring at a status page
//! or an AlertManager annotation want a single LINE that answers "is the
//! protocol healthy right now?" — how many markets, how many are firing,
//! and at what severity. This module folds the per-market report into
//! that one-line [`ProtocolSummary`].
//!
//! It is the backend mirror of the frontend's wave-28
//! `feed/protocolStats.ts` aggregation: both roll N markets up into a
//! single protocol-level verdict, so the ops status line and the web
//! Overview landing page tell the same story.

use crate::multi::MultiMarketHealthReport;
use crate::report::{CheckStatus, Severity};

/// Wave 28 — one-line protocol health rollup across every market.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProtocolSummary {
    /// Number of markets scanned.
    pub markets: u32,
    /// Markets whose `overall_status` is `Pass`.
    pub healthy_markets: u32,
    /// Markets whose worst check is `Warn`.
    pub warn_markets: u32,
    /// Markets whose worst check is `Critical`.
    pub critical_markets: u32,
    /// Total firing (Warn + Critical) checks summed across all markets.
    pub firing_checks: u32,
    /// Worst exit code across markets (mirrors the report field).
    pub worst_exit_code: i32,
}

impl ProtocolSummary {
    /// True when every market's overall status is `Pass`.
    pub fn all_healthy(&self) -> bool {
        self.markets > 0 && self.healthy_markets == self.markets
    }

    /// The protocol-level worst status string (`PASS` / `WARN` /
    /// `CRITICAL`). Empty protocol (no markets) reports `PASS`.
    pub fn overall_status(&self) -> &'static str {
        if self.critical_markets > 0 {
            "CRITICAL"
        } else if self.warn_markets > 0 {
            "WARN"
        } else {
            "PASS"
        }
    }

    /// Highest firing severity across the protocol, or `NONE` when all
    /// markets are clean.
    pub fn highest_firing_severity(&self) -> &'static str {
        if self.critical_markets > 0 {
            Severity::P1.as_str()
        } else if self.warn_markets > 0 {
            Severity::P2.as_str()
        } else {
            "NONE"
        }
    }
}

/// Fold a [`MultiMarketHealthReport`] into a [`ProtocolSummary`].
pub fn summarize_protocol(report: &MultiMarketHealthReport) -> ProtocolSummary {
    let mut summary = ProtocolSummary {
        worst_exit_code: report.worst_exit_code,
        ..ProtocolSummary::default()
    };
    for market in &report.per_market {
        summary.markets += 1;
        let (_pass, warn, critical) = market.report.count_by_status();
        summary.firing_checks += (warn + critical) as u32;
        match market.report.overall_status() {
            CheckStatus::Pass => summary.healthy_markets += 1,
            CheckStatus::Warn => summary.warn_markets += 1,
            CheckStatus::Critical => summary.critical_markets += 1,
        }
    }
    summary
}

/// Render a [`ProtocolSummary`] as a stable, flat JSON object — the
/// single-line protocol status page / annotation payload.
pub fn render_protocol_summary_json(summary: &ProtocolSummary) -> String {
    format!(
        "{{\"markets\":{},\"healthy_markets\":{},\"warn_markets\":{},\
        \"critical_markets\":{},\"firing_checks\":{},\"worst_exit_code\":{},\
        \"overall_status\":\"{}\",\"highest_firing_severity\":\"{}\"}}",
        summary.markets,
        summary.healthy_markets,
        summary.warn_markets,
        summary.critical_markets,
        summary.firing_checks,
        summary.worst_exit_code,
        summary.overall_status(),
        summary.highest_firing_severity(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{
        HealthContext, KeeperFacts, MarketFacts, OracleFacts, PoolFacts, RpcFacts, SubPoolFacts,
    };
    use crate::multi::{scan_all_markets, MarketRegistry, MultiMarketHealthReport};

    fn healthy_ctx() -> HealthContext {
        HealthContext {
            now_unix_secs: 1_000_000,
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

    /// Build a registry of `n` markets (n ≤ 4) from TOML, reusing
    /// well-known valid base58 pubkeys for the PDA fields.
    fn registry(symbols: &[&str]) -> MarketRegistry {
        const PROGRAM: &str = "11111111111111111111111111111112";
        // Four distinct, valid 32-byte base58 sysvar addresses.
        const PDAS: [&str; 4] = [
            "Sysvar1nstructions1111111111111111111111111",
            "SysvarC1ock11111111111111111111111111111111",
            "SysvarRent111111111111111111111111111111111",
            "SysvarS1otHashes111111111111111111111111111",
        ];
        let mut toml = String::new();
        for (i, s) in symbols.iter().enumerate() {
            let market = PDAS[i % PDAS.len()];
            let lock = PDAS[(i + 1) % PDAS.len()];
            toml.push_str(&format!(
                "[[markets]]\nsymbol = \"{s}\"\nprogram_id = \"{PROGRAM}\"\nmarket_pda = \"{market}\"\nlock_pda = \"{lock}\"\n\n"
            ));
        }
        MarketRegistry::from_toml_str(&toml).expect("parse")
    }

    #[test]
    fn summarizes_all_healthy_protocol() {
        let reg = registry(&["SOL-USD", "BTC-USD", "ETH-USD"]);
        let report = scan_all_markets(&reg, |_| Ok(healthy_ctx())).unwrap();
        let s = summarize_protocol(&report);
        assert_eq!(s.markets, 3);
        assert_eq!(s.healthy_markets, 3);
        assert_eq!(s.warn_markets, 0);
        assert_eq!(s.critical_markets, 0);
        assert_eq!(s.firing_checks, 0);
        assert_eq!(s.worst_exit_code, 0);
        assert!(s.all_healthy());
        assert_eq!(s.overall_status(), "PASS");
        assert_eq!(s.highest_firing_severity(), "NONE");
    }

    #[test]
    fn counts_critical_market_and_worst_exit() {
        let reg = registry(&["SOL-USD", "BTC-USD"]);
        // BTC market globally paused → P0 critical.
        let report = scan_all_markets(&reg, |entry| {
            let mut ctx = healthy_ctx();
            if entry.symbol == "BTC-USD" {
                ctx.market.paused_globally = true;
            }
            Ok(ctx)
        })
        .unwrap();
        let s = summarize_protocol(&report);
        assert_eq!(s.markets, 2);
        assert_eq!(s.healthy_markets, 1);
        assert_eq!(s.critical_markets, 1);
        assert!(!s.all_healthy());
        assert_eq!(s.overall_status(), "CRITICAL");
        assert!(s.firing_checks >= 1);
        assert_ne!(s.worst_exit_code, 0);
    }

    #[test]
    fn empty_protocol_is_pass_but_not_all_healthy() {
        // `from_toml_str` rejects an empty registry, so construct the
        // zero-market report directly.
        let report = MultiMarketHealthReport {
            per_market: Vec::new(),
            worst_exit_code: 0,
        };
        let s = summarize_protocol(&report);
        assert_eq!(s.markets, 0);
        assert!(!s.all_healthy(), "no markets ⇒ not 'all healthy'");
        assert_eq!(s.overall_status(), "PASS");
    }

    #[test]
    fn json_render_is_stable_and_flat() {
        let reg = registry(&["SOL-USD"]);
        let report = scan_all_markets(&reg, |_| Ok(healthy_ctx())).unwrap();
        let json = render_protocol_summary_json(&summarize_protocol(&report));
        assert!(json.starts_with("{\"markets\":1,"));
        assert!(json.contains("\"overall_status\":\"PASS\""));
        assert!(json.contains("\"highest_firing_severity\":\"NONE\""));
        assert!(json.ends_with("}"));
    }
}
