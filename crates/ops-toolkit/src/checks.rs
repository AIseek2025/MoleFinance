//! 18 health checks documented in
//! `Docs/Planning/24-operator-runbook.md` §2.
//!
//! Each check is a pure function `(ctx) -> CheckResult`. Severity
//! and threshold values match the runbook line-for-line; if you
//! tighten a threshold here, update the runbook in the same commit
//! so paging matches what's in the code.
//!
//! ## Check organisation
//!
//! Order in `CHECK_NAMES` mirrors runbook §2 numbering:
//!
//! ```text
//!  1..8  on-chain (market + sub-pool + recovery)
//!  9..13 keeper bot
//! 14..16 RPC fleet
//! 17..18 Pyth oracle
//! 19..21 keeper-leader-lock (wave 17)
//!    22  position principal/notional drift (wave 24)
//! ```

use crate::context::HealthContext;
use crate::report::{CheckResult, CheckStatus, HealthReport, Severity};

/// Names of every check, in the order [`run_all_checks`] runs them.
pub const CHECK_NAMES: &[&str] = &[
    "global_paused",
    "market_paused",
    "frozen_new_position",
    "schema_version_match",
    "trading_activity",
    "dormant_inventory",
    "recovery_outstanding",
    "pending_init_hints",
    "keeper_alive",
    "keeper_failed_actions_rate",
    "keeper_skipped_actions_rate",
    "keeper_vol_estimator",
    "keeper_wallet_balance",
    "rpc_primary_latency",
    "rpc_primary_backup_lag",
    "rpc_get_program_accounts_latency",
    "oracle_slot_age",
    "oracle_confidence",
    // Wave 17 — keeper-leader-lock health checks. Skipped (returned
    // as `Pass` with `leader_lock_enabled=0`) when `ctx.leader_lock`
    // is `None` so single-replica probers don't get false alarms.
    "keeper_leader_lock_initialized",
    "keeper_leader_lock_freshness",
    "keeper_leader_lock_holder_matches_expected",
    // Wave 24 — reconcile indexer-reported notional vs the on-chain
    // open-interest aggregate. Skipped (Pass) when the position scan
    // didn't run this cycle.
    "position_principal_drift",
];

/// Runbook §2.1: global pause is unset.
pub fn check_global_paused(ctx: &HealthContext) -> CheckResult {
    let status = if ctx.market.paused_globally {
        CheckStatus::Critical
    } else {
        CheckStatus::Pass
    };
    CheckResult {
        name: "global_paused",
        status,
        severity: Severity::P0,
        message: if status == CheckStatus::Critical {
            "GlobalConfig.paused_globally == true".to_string()
        } else {
            "global pause flag is clear".to_string()
        },
        measurements: vec![("paused_globally", ctx.market.paused_globally as u8 as f64)],
    }
}

/// Runbook §2.2: market pause is unset.
pub fn check_market_paused(ctx: &HealthContext) -> CheckResult {
    let status = if ctx.market.paused {
        CheckStatus::Critical
    } else {
        CheckStatus::Pass
    };
    CheckResult {
        name: "market_paused",
        status,
        severity: Severity::P0,
        message: if status == CheckStatus::Critical {
            "Market.paused == true".to_string()
        } else {
            "market pause flag is clear".to_string()
        },
        measurements: vec![("paused", ctx.market.paused as u8 as f64)],
    }
}

/// Runbook §2.3: market is not frozen for new positions.
pub fn check_frozen_new_position(ctx: &HealthContext) -> CheckResult {
    let status = if ctx.market.frozen_new_position {
        CheckStatus::Warn // Frozen is a deliberate operator action — Warn, not Critical.
    } else {
        CheckStatus::Pass
    };
    CheckResult {
        name: "frozen_new_position",
        status,
        severity: Severity::P2,
        message: if status == CheckStatus::Warn {
            "Market.frozen_new_position == true (planned deprecation?)".to_string()
        } else {
            "new positions accepted".to_string()
        },
        measurements: vec![(
            "frozen_new_position",
            ctx.market.frozen_new_position as u8 as f64,
        )],
    }
}

/// Runbook §2.4: on-chain schema version equals the compiled binary.
pub fn check_schema_version(ctx: &HealthContext) -> CheckResult {
    let onchain = ctx.market.schema_version_onchain;
    let compiled = ctx.market.schema_version_compiled;
    let status = if onchain == compiled {
        CheckStatus::Pass
    } else {
        CheckStatus::Critical
    };
    CheckResult {
        name: "schema_version_match",
        status,
        severity: Severity::P1,
        message: format!("on-chain={onchain} compiled={compiled}"),
        measurements: vec![
            ("schema_version_onchain", onchain as f64),
            ("schema_version_compiled", compiled as f64),
        ],
    }
}

/// Runbook §2.5: trading activity exists in the past 24h.
///
/// We synthesise a "trading is happening" signal from the sum of
/// open Long + Short qty across all sub-pools — if that sum is
/// non-zero, traders are present. The runbook talks about 24h
/// rolling activity; that's a derivative the prober has to
/// pre-compute, so this check just observes the instantaneous total.
pub fn check_trading_activity(ctx: &HealthContext) -> CheckResult {
    let total: u128 = ctx
        .sub_pools
        .iter()
        .map(|s| s.open_long_qty + s.open_short_qty)
        .sum();
    let status = if total == 0 {
        CheckStatus::Warn
    } else {
        CheckStatus::Pass
    };
    CheckResult {
        name: "trading_activity",
        status,
        severity: Severity::P3,
        message: format!("total open qty across sub-pools: {total}"),
        measurements: vec![("total_open_qty", total as f64)],
    }
}

/// Runbook §2.6: dormant ticks count < 5_000 (Critical).
pub fn check_dormant_inventory(ctx: &HealthContext) -> CheckResult {
    let max_ticks = ctx.sub_pools.iter().map(|s| s.dormant_ticks).max().unwrap_or(0);
    let (status, severity) = if max_ticks > 5_000 {
        (CheckStatus::Critical, Severity::P1)
    } else if max_ticks > 2_000 {
        (CheckStatus::Warn, Severity::P2)
    } else {
        (CheckStatus::Pass, Severity::P2)
    };
    CheckResult {
        name: "dormant_inventory",
        status,
        severity,
        message: format!("max dormant ticks across sub-pools: {max_ticks}"),
        measurements: vec![("max_dormant_ticks", max_ticks as f64)],
    }
}

/// Runbook §2.7: `projected_recovery_outstanding < 0.1 %` of
/// notional. Critical at > 1 %, Warn at > 0.1 %.
pub fn check_recovery_outstanding(ctx: &HealthContext) -> CheckResult {
    let total = ctx.pool.total_notional_micro_usdc;
    let outstanding = ctx.pool.recovery_outstanding_micro_usdc;
    let ratio = if total == 0 {
        0.0
    } else {
        (outstanding as f64) / (total as f64)
    };
    let (status, severity) = if ratio > 0.01 {
        (CheckStatus::Critical, Severity::P0)
    } else if ratio > 0.001 {
        (CheckStatus::Warn, Severity::P2)
    } else {
        (CheckStatus::Pass, Severity::P2)
    };
    CheckResult {
        name: "recovery_outstanding",
        status,
        severity,
        message: format!("recovery / notional = {:.6}", ratio),
        measurements: vec![
            ("recovery_outstanding", outstanding as f64),
            ("total_notional", total as f64),
            ("recovery_ratio", ratio),
        ],
    }
}

/// Runbook §2.8: pending_init_hints < 50 per ledger. Use the max
/// across all (sub_pool, direction) ledgers.
pub fn check_pending_init_hints(ctx: &HealthContext) -> CheckResult {
    let max_pending = ctx
        .sub_pools
        .iter()
        .map(|s| s.pending_init_hints)
        .max()
        .unwrap_or(0);
    let (status, severity) = if max_pending >= 50 {
        (CheckStatus::Warn, Severity::P2)
    } else {
        (CheckStatus::Pass, Severity::P2)
    };
    CheckResult {
        name: "pending_init_hints",
        status,
        severity,
        message: format!("max pending init_hints: {max_pending}"),
        measurements: vec![("max_pending_init_hints", max_pending as f64)],
    }
}

/// Runbook §2.9: keeper bot heartbeat seen in past 60s.
pub fn check_keeper_alive(ctx: &HealthContext) -> CheckResult {
    let alive = ctx.keeper.heartbeat_within_60s;
    let status = if alive { CheckStatus::Pass } else { CheckStatus::Critical };
    CheckResult {
        name: "keeper_alive",
        status,
        severity: Severity::P1,
        message: if alive {
            "keeper heartbeat fresh".to_string()
        } else {
            "no keeper heartbeat in past 60s".to_string()
        },
        measurements: vec![("heartbeat_fresh", alive as u8 as f64)],
    }
}

/// Runbook §2.10: failed_actions/h < 5 (Critical at >= 5).
pub fn check_keeper_failed_actions_rate(ctx: &HealthContext) -> CheckResult {
    let n = ctx.keeper.failed_actions_last_hour;
    let (status, severity) = if n >= 5 {
        (CheckStatus::Critical, Severity::P1)
    } else if n >= 2 {
        (CheckStatus::Warn, Severity::P2)
    } else {
        (CheckStatus::Pass, Severity::P1)
    };
    CheckResult {
        name: "keeper_failed_actions_rate",
        status,
        severity,
        message: format!("failed_actions in past 1h: {n}"),
        measurements: vec![("failed_actions_last_hour", n as f64)],
    }
}

/// Runbook §2.11: skipped_actions/h < 100 (Warn at >= 100).
pub fn check_keeper_skipped_actions_rate(ctx: &HealthContext) -> CheckResult {
    let n = ctx.keeper.skipped_actions_last_hour;
    let (status, severity) = if n >= 100 {
        (CheckStatus::Warn, Severity::P2)
    } else {
        (CheckStatus::Pass, Severity::P2)
    };
    CheckResult {
        name: "keeper_skipped_actions_rate",
        status,
        severity,
        message: format!("skipped_actions in past 1h: {n}"),
        measurements: vec![("skipped_actions_last_hour", n as f64)],
    }
}

/// Runbook §2.12: vol estimator is producing values, not stuck.
/// Triggers Warn at 3 consecutive None-vol ticks.
pub fn check_keeper_vol_estimator(ctx: &HealthContext) -> CheckResult {
    let warming = ctx.keeper.consecutive_warming_ticks;
    let last = ctx.keeper.last_applied_vol;
    let (status, severity) = if warming >= 3 && last.is_none() {
        (CheckStatus::Warn, Severity::P2)
    } else {
        (CheckStatus::Pass, Severity::P2)
    };
    CheckResult {
        name: "keeper_vol_estimator",
        status,
        severity,
        message: match last {
            Some(v) => format!("σ̂ = {v:.4} (warm)"),
            None => format!("vol estimator still warming up ({warming} consecutive ticks)"),
        },
        measurements: vec![
            ("consecutive_warming_ticks", warming as f64),
            ("applied_vol", last.unwrap_or(f64::NAN)),
        ],
    }
}

/// Runbook §2.13: keeper wallet balance ≥ 0.5 SOL (Warn), ≥ 0.2 SOL (Critical).
/// 1 SOL = 1_000_000_000 lamports.
pub fn check_keeper_wallet_balance(ctx: &HealthContext) -> CheckResult {
    let bal = ctx.keeper.wallet_balance_lamports;
    let sol = (bal as f64) / 1_000_000_000.0;
    let (status, severity) = if bal < 200_000_000 {
        (CheckStatus::Critical, Severity::P1)
    } else if bal < 500_000_000 {
        (CheckStatus::Warn, Severity::P2)
    } else {
        (CheckStatus::Pass, Severity::P1)
    };
    CheckResult {
        name: "keeper_wallet_balance",
        status,
        severity,
        message: format!("wallet balance = {sol:.4} SOL"),
        measurements: vec![("wallet_balance_lamports", bal as f64)],
    }
}

/// Runbook §2.14: primary RPC `getSlot` p95 latency < 200ms.
pub fn check_rpc_primary_latency(ctx: &HealthContext) -> CheckResult {
    let ms = ctx.rpc.primary_get_slot_p95_ms;
    let (status, severity) = if ms > 1_000 {
        (CheckStatus::Critical, Severity::P1)
    } else if ms > 200 {
        (CheckStatus::Warn, Severity::P2)
    } else {
        (CheckStatus::Pass, Severity::P1)
    };
    CheckResult {
        name: "rpc_primary_latency",
        status,
        severity,
        message: format!("primary getSlot p95: {ms} ms"),
        measurements: vec![("primary_get_slot_p95_ms", ms as f64)],
    }
}

/// Runbook §2.15: primary vs backup RPC slot diff < 5 (Pass), < 32 (Warn).
pub fn check_rpc_primary_backup_lag(ctx: &HealthContext) -> CheckResult {
    let d = ctx.rpc.primary_backup_slot_diff;
    let (status, severity) = if d > 32 {
        (CheckStatus::Critical, Severity::P1)
    } else if d > 5 {
        (CheckStatus::Warn, Severity::P2)
    } else {
        (CheckStatus::Pass, Severity::P1)
    };
    CheckResult {
        name: "rpc_primary_backup_lag",
        status,
        severity,
        message: format!("primary-backup slot diff: {d}"),
        measurements: vec![("primary_backup_slot_diff", d as f64)],
    }
}

/// Runbook §2.16: `getProgramAccounts` < 5s (Pass), < 30s (Warn).
pub fn check_rpc_gpa_latency(ctx: &HealthContext) -> CheckResult {
    let ms = ctx.rpc.get_program_accounts_ms;
    let (status, severity) = if ms > 30_000 {
        (CheckStatus::Critical, Severity::P2)
    } else if ms > 5_000 {
        (CheckStatus::Warn, Severity::P2)
    } else {
        (CheckStatus::Pass, Severity::P2)
    };
    CheckResult {
        name: "rpc_get_program_accounts_latency",
        status,
        severity,
        message: format!("getProgramAccounts: {ms} ms"),
        measurements: vec![("get_program_accounts_ms", ms as f64)],
    }
}

/// Runbook §2.17: oracle slot age < 30 (Pass), < 64 (Warn).
pub fn check_oracle_slot_age(ctx: &HealthContext) -> CheckResult {
    let age = ctx.oracle.slot_age;
    let (status, severity) = if age > 64 {
        (CheckStatus::Critical, Severity::P0)
    } else if age > 30 {
        (CheckStatus::Warn, Severity::P1)
    } else {
        (CheckStatus::Pass, Severity::P0)
    };
    CheckResult {
        name: "oracle_slot_age",
        status,
        severity,
        message: format!("oracle slot_age: {age}"),
        measurements: vec![("slot_age", age as f64)],
    }
}

/// Runbook §2.18: oracle confidence_ratio < 0.5 % (Pass), < 2 % (Warn).
pub fn check_oracle_confidence(ctx: &HealthContext) -> CheckResult {
    let ratio = ctx.oracle.confidence_ratio;
    let (status, severity) = if ratio > 0.02 {
        (CheckStatus::Critical, Severity::P1)
    } else if ratio > 0.005 {
        (CheckStatus::Warn, Severity::P2)
    } else {
        (CheckStatus::Pass, Severity::P1)
    };
    CheckResult {
        name: "oracle_confidence",
        status,
        severity,
        message: format!("oracle confidence ratio: {:.4}", ratio),
        measurements: vec![("confidence_ratio", ratio)],
    }
}

// ---------------------------------------------------------------------
// Wave 17 — keeper-leader-lock checks (runbook §6.5)
// ---------------------------------------------------------------------

/// Wave 17 — flat "no leader-lock facts gathered" result that all
/// three leader-lock checks share when the prober didn't populate
/// `ctx.leader_lock`. Returned as `Pass` so multi-replica leader-
/// lock checks are opt-in rather than alarming on single-replica
/// deployments by default.
fn leader_lock_skipped(name: &'static str, severity: Severity) -> CheckResult {
    CheckResult {
        name,
        status: CheckStatus::Pass,
        severity,
        message: "leader-lock probe disabled (no LeaderLockFacts in HealthContext)".to_string(),
        measurements: vec![("leader_lock_enabled", 0.0)],
    }
}

/// Runbook §6.5 — KL-A: the on-chain `KeeperLeaderLock` PDA exists.
/// Critical when missing — multi-replica deployments are not
/// race-protected without the lock account.
pub fn check_keeper_leader_lock_initialized(ctx: &HealthContext) -> CheckResult {
    let Some(facts) = ctx.leader_lock.as_ref() else {
        return leader_lock_skipped("keeper_leader_lock_initialized", Severity::P1);
    };
    let status = if facts.initialized {
        CheckStatus::Pass
    } else {
        CheckStatus::Critical
    };
    let message = if facts.initialized {
        "KeeperLeaderLock PDA exists on chain".to_string()
    } else {
        "KeeperLeaderLock PDA not initialised — operator must run `initialize_keeper_leader_lock`"
            .to_string()
    };
    CheckResult {
        name: "keeper_leader_lock_initialized",
        status,
        severity: Severity::P1,
        message,
        measurements: vec![
            ("leader_lock_enabled", 1.0),
            ("initialized", facts.initialized as u8 as f64),
        ],
    }
}

/// Runbook §6.5 — KL-B: the active leader is fresh; the elapsed
/// slot count since the most recent heartbeat is well within the
/// configured `takeover_threshold_slots`.
///
/// Severity model:
/// - Pass: elapsed < 60% of threshold (well within the safe band)
/// - Warn: 60..=90% of threshold (early warning — leader bot may
///   be lagging; standby will take over soon if this trends up)
/// - Critical: > 90% of threshold OR `has_leader = false` (no
///   leader at all means dispatch is paused right now)
///
/// Slot regression (e.g., new RPC reporting a lower slot than the
/// lock recorded — clock skew) is treated as "fresh" with a 0
/// elapsed reading because the on-chain ix would have rejected
/// such a heartbeat anyway. The mismatch is logged as a measurement
/// but doesn't alarm — that's a different bug class.
pub fn check_keeper_leader_lock_freshness(ctx: &HealthContext) -> CheckResult {
    let Some(facts) = ctx.leader_lock.as_ref() else {
        return leader_lock_skipped("keeper_leader_lock_freshness", Severity::P1);
    };
    if !facts.initialized {
        return CheckResult {
            name: "keeper_leader_lock_freshness",
            status: CheckStatus::Critical,
            severity: Severity::P1,
            message: "PDA not initialised — freshness undefined".to_string(),
            measurements: vec![
                ("leader_lock_enabled", 1.0),
                ("initialized", 0.0),
            ],
        };
    }
    if !facts.has_leader {
        return CheckResult {
            name: "keeper_leader_lock_freshness",
            status: CheckStatus::Critical,
            severity: Severity::P1,
            message: "no leader currently holds the lock — dispatch is paused".to_string(),
            measurements: vec![
                ("leader_lock_enabled", 1.0),
                ("has_leader", 0.0),
            ],
        };
    }
    let elapsed = facts
        .current_slot
        .saturating_sub(facts.last_heartbeat_slot);
    let threshold = facts.takeover_threshold_slots.max(1);
    let pct = (elapsed as f64) / (threshold as f64);
    let (status, message) = if pct >= 0.90 {
        (
            CheckStatus::Critical,
            format!(
                "leader heartbeat is stale ({} / {} slots, {:.0}% of threshold)",
                elapsed,
                threshold,
                pct * 100.0
            ),
        )
    } else if pct >= 0.60 {
        (
            CheckStatus::Warn,
            format!(
                "leader heartbeat trending stale ({} / {} slots, {:.0}% of threshold)",
                elapsed,
                threshold,
                pct * 100.0
            ),
        )
    } else {
        (
            CheckStatus::Pass,
            format!(
                "leader heartbeat fresh ({} / {} slots, {:.0}% of threshold)",
                elapsed,
                threshold,
                pct * 100.0
            ),
        )
    };
    CheckResult {
        name: "keeper_leader_lock_freshness",
        status,
        severity: Severity::P1,
        message,
        measurements: vec![
            ("leader_lock_enabled", 1.0),
            ("elapsed_slots", elapsed as f64),
            ("takeover_threshold_slots", threshold as f64),
            ("freshness_pct", pct),
        ],
    }
}

/// Runbook §6.5 — KL-C: the on-chain holder matches the operator's
/// declared `expected_leader` pubkey.
///
/// Use cases:
/// - **Planned handoff in progress**: ops set `expected_leader = B`
///   (the standby) and triggered a release+acquire flow. While the
///   chain still shows `A`, this check goes critical — that's the
///   intended page; ops sees the failed handoff immediately.
/// - **Unexpected takeover**: another keeper acquired the lock
///   without ops knowledge — same critical, prompts triage of
///   which replica is rogue.
/// - **No expected holder configured**: degrades to warn-level
///   advisory ("set `expected_leader` in the prober config to
///   enable holder-match alerting").
pub fn check_keeper_leader_lock_holder_matches_expected(ctx: &HealthContext) -> CheckResult {
    let Some(facts) = ctx.leader_lock.as_ref() else {
        return leader_lock_skipped(
            "keeper_leader_lock_holder_matches_expected",
            Severity::P2,
        );
    };
    let Some(expected) = facts.expected_leader else {
        return CheckResult {
            name: "keeper_leader_lock_holder_matches_expected",
            status: CheckStatus::Warn,
            severity: Severity::P2,
            message: "no expected_leader configured in prober — set the active replica's wallet \
                      pubkey to enable holder-match alerting"
                .to_string(),
            measurements: vec![
                ("leader_lock_enabled", 1.0),
                ("expected_leader_set", 0.0),
            ],
        };
    };
    if !facts.initialized || !facts.has_leader {
        return CheckResult {
            name: "keeper_leader_lock_holder_matches_expected",
            status: CheckStatus::Critical,
            severity: Severity::P2,
            message: "lock has no active holder — cannot match expected".to_string(),
            measurements: vec![
                ("leader_lock_enabled", 1.0),
                ("initialized", facts.initialized as u8 as f64),
                ("has_leader", facts.has_leader as u8 as f64),
                ("expected_leader_set", 1.0),
            ],
        };
    }
    let matches = facts.current_leader == expected;
    let status = if matches {
        CheckStatus::Pass
    } else {
        CheckStatus::Critical
    };
    let message = if matches {
        "on-chain holder matches expected_leader".to_string()
    } else {
        format!(
            "on-chain holder ({}) does NOT match expected_leader ({}) — \
             investigate via `solana account <lock_pda>` and the keeper-bot logs",
            short_hex(&facts.current_leader),
            short_hex(&expected),
        )
    };
    CheckResult {
        name: "keeper_leader_lock_holder_matches_expected",
        status,
        severity: Severity::P2,
        message,
        measurements: vec![
            ("leader_lock_enabled", 1.0),
            ("expected_leader_set", 1.0),
            ("holder_matches", matches as u8 as f64),
        ],
    }
}

/// Runbook §2.22 (wave 24): reconcile the indexer-reported pool
/// notional against the independent on-chain aggregate computed by
/// `fetch_open_interest` (sum of decoded `Position.notional`). A
/// non-trivial gap means the indexer and the chain disagree about
/// live exposure — a money-grade integrity signal.
///
/// Drift ratio = `|onchain − reported| / max(reported, 1)`.
/// Thresholds: Pass < 0.5 %, Warn (P2) < 2 %, Critical (P1) ≥ 2 %.
///
/// When `onchain_position_notional_micro_usdc == 0` the probe was not
/// run this cycle (single-source prober / open-interest scan disabled)
/// — the check returns `Pass` with `drift_enabled = 0`, exactly like
/// the wave-17 leader-lock checks skip when `ctx.leader_lock` is
/// `None`. This keeps deployments that don't run the position scan
/// from false-alarming.
pub fn check_position_principal_drift(ctx: &HealthContext) -> CheckResult {
    let reported = ctx.pool.total_notional_micro_usdc;
    let onchain = ctx.pool.onchain_position_notional_micro_usdc;

    if onchain == 0 {
        return CheckResult {
            name: "position_principal_drift",
            status: CheckStatus::Pass,
            severity: Severity::P1,
            message: "open-interest probe not run this cycle — drift check skipped".to_string(),
            measurements: vec![
                ("drift_enabled", 0.0),
                ("reported_notional", reported as f64),
                ("onchain_notional", 0.0),
            ],
        };
    }

    let diff = onchain.abs_diff(reported);
    let denom = reported.max(1);
    let ratio = (diff as f64) / (denom as f64);

    let (status, severity) = if ratio >= 0.02 {
        (CheckStatus::Critical, Severity::P1)
    } else if ratio >= 0.005 {
        (CheckStatus::Warn, Severity::P2)
    } else {
        (CheckStatus::Pass, Severity::P1)
    };

    CheckResult {
        name: "position_principal_drift",
        status,
        severity,
        message: format!(
            "on-chain notional {} vs reported {} (drift {:.4})",
            onchain, reported, ratio
        ),
        measurements: vec![
            ("drift_enabled", 1.0),
            ("reported_notional", reported as f64),
            ("onchain_notional", onchain as f64),
            ("drift_ratio", ratio),
        ],
    }
}

fn short_hex(pk: &[u8; 32]) -> String {
    // Match the wave-16 frontend banner shortener so ops paging
    // shows the same bytes as the UI: first 4 + last 4 hex chars.
    let mut hex = String::with_capacity(20);
    for b in &pk[..4] {
        hex.push_str(&format!("{:02x}", b));
    }
    hex.push('…');
    for b in &pk[28..] {
        hex.push_str(&format!("{:02x}", b));
    }
    hex
}

pub fn run_all_checks(ctx: &HealthContext) -> HealthReport {
    let checks = vec![
        check_global_paused(ctx),
        check_market_paused(ctx),
        check_frozen_new_position(ctx),
        check_schema_version(ctx),
        check_trading_activity(ctx),
        check_dormant_inventory(ctx),
        check_recovery_outstanding(ctx),
        check_pending_init_hints(ctx),
        check_keeper_alive(ctx),
        check_keeper_failed_actions_rate(ctx),
        check_keeper_skipped_actions_rate(ctx),
        check_keeper_vol_estimator(ctx),
        check_keeper_wallet_balance(ctx),
        check_rpc_primary_latency(ctx),
        check_rpc_primary_backup_lag(ctx),
        check_rpc_gpa_latency(ctx),
        check_oracle_slot_age(ctx),
        check_oracle_confidence(ctx),
        check_keeper_leader_lock_initialized(ctx),
        check_keeper_leader_lock_freshness(ctx),
        check_keeper_leader_lock_holder_matches_expected(ctx),
        check_position_principal_drift(ctx),
    ];
    HealthReport {
        timestamp_unix: ctx.now_unix_secs,
        checks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::*;

    fn healthy_ctx() -> HealthContext {
        HealthContext {
            now_unix_secs: 1_700_000_000,
            market: MarketFacts {
                paused_globally: false,
                paused: false,
                frozen_new_position: false,
                schema_version_onchain: 1,
                schema_version_compiled: 1,
            },
            sub_pools: vec![SubPoolFacts {
                id: 0,
                dormant_ticks: 100,
                pending_init_hints: 5,
                open_long_qty: 1_000,
                open_short_qty: 1_000,
            }],
            keeper: KeeperFacts {
                heartbeat_within_60s: true,
                failed_actions_last_hour: 0,
                skipped_actions_last_hour: 0,
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
                // Wave 24 — on-chain aggregate matches reported, so the
                // `position_principal_drift` check passes (drift = 0).
                onchain_position_notional_micro_usdc: 100_000_000_000,
            },
            // Wave 17 — leader-lock probe disabled by default in
            // tests so the wave-12 happy path stays "all 18+3
            // checks pass" without needing fake on-chain facts.
            leader_lock: None,
        }
    }

    fn fresh_leader_lock_facts() -> LeaderLockFacts {
        let me = [0xab; 32];
        LeaderLockFacts {
            initialized: true,
            has_leader: true,
            current_leader: me,
            last_heartbeat_slot: 100,
            takeover_threshold_slots: 75,
            current_slot: 110, // 10 / 75 = 13% of threshold → fresh
            expected_leader: Some(me),
        }
    }

    #[test]
    fn happy_path_all_22_checks_pass() {
        let r = run_all_checks(&healthy_ctx());
        // 18 wave-12 + 3 wave-17 leader-lock + 1 wave-24 drift = 22
        assert_eq!(r.checks.len(), 22);
        for c in &r.checks {
            assert_eq!(c.status, CheckStatus::Pass, "{} should pass: {}", c.name, c.message);
        }
        assert_eq!(r.overall_status(), CheckStatus::Pass);
    }

    #[test]
    fn leader_lock_initialized_critical_when_pda_missing() {
        let mut ctx = healthy_ctx();
        ctx.leader_lock = Some(LeaderLockFacts {
            initialized: false,
            ..fresh_leader_lock_facts()
        });
        let r = check_keeper_leader_lock_initialized(&ctx);
        assert_eq!(r.status, CheckStatus::Critical);
        assert_eq!(r.severity, Severity::P1);
        assert!(r.message.contains("initialize_keeper_leader_lock"));
    }

    #[test]
    fn leader_lock_freshness_warns_at_60_pct_threshold() {
        let mut ctx = healthy_ctx();
        ctx.leader_lock = Some(LeaderLockFacts {
            current_slot: 100 + 60, // 60 / 75 = 80% — well into Warn band
            ..fresh_leader_lock_facts()
        });
        let r = check_keeper_leader_lock_freshness(&ctx);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains("80%"));
    }

    #[test]
    fn leader_lock_freshness_critical_at_90_pct_threshold() {
        let mut ctx = healthy_ctx();
        ctx.leader_lock = Some(LeaderLockFacts {
            current_slot: 100 + 70, // 70 / 75 = 93% — Critical band
            ..fresh_leader_lock_facts()
        });
        let r = check_keeper_leader_lock_freshness(&ctx);
        assert_eq!(r.status, CheckStatus::Critical);
        assert!(r.message.contains("93%"));
    }

    #[test]
    fn leader_lock_freshness_critical_when_no_leader() {
        let mut ctx = healthy_ctx();
        ctx.leader_lock = Some(LeaderLockFacts {
            has_leader: false,
            ..fresh_leader_lock_facts()
        });
        let r = check_keeper_leader_lock_freshness(&ctx);
        assert_eq!(r.status, CheckStatus::Critical);
        assert!(r.message.contains("dispatch is paused"));
    }

    #[test]
    fn leader_lock_holder_matches_expected_critical_on_mismatch() {
        let me = [0xab; 32];
        let other = [0xcd; 32];
        let mut ctx = healthy_ctx();
        ctx.leader_lock = Some(LeaderLockFacts {
            current_leader: other,
            expected_leader: Some(me),
            ..fresh_leader_lock_facts()
        });
        let r = check_keeper_leader_lock_holder_matches_expected(&ctx);
        assert_eq!(r.status, CheckStatus::Critical);
        assert!(r.message.contains("does NOT match"));
    }

    #[test]
    fn leader_lock_holder_matches_expected_warns_when_unset() {
        let mut ctx = healthy_ctx();
        ctx.leader_lock = Some(LeaderLockFacts {
            expected_leader: None,
            ..fresh_leader_lock_facts()
        });
        let r = check_keeper_leader_lock_holder_matches_expected(&ctx);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains("expected_leader"));
    }

    #[test]
    fn leader_lock_checks_skipped_when_facts_missing() {
        // No `leader_lock` set; all three checks return Pass with
        // `leader_lock_enabled = 0` so single-replica probers don't
        // false-alarm.
        let ctx = healthy_ctx();
        for name in [
            "keeper_leader_lock_initialized",
            "keeper_leader_lock_freshness",
            "keeper_leader_lock_holder_matches_expected",
        ] {
            let r = run_all_checks(&ctx);
            let c = r.checks.iter().find(|c| c.name == name).unwrap();
            assert_eq!(c.status, CheckStatus::Pass, "{}", name);
            assert!(
                c.measurements
                    .iter()
                    .any(|(k, v)| *k == "leader_lock_enabled" && *v == 0.0),
                "{} should advertise leader_lock_enabled=0",
                name
            );
        }
    }

    #[test]
    fn check_names_match_run_all_order() {
        let r = run_all_checks(&healthy_ctx());
        for (i, c) in r.checks.iter().enumerate() {
            assert_eq!(c.name, CHECK_NAMES[i]);
        }
        assert_eq!(r.checks.len(), CHECK_NAMES.len());
    }

    #[test]
    fn global_paused_critical_when_set() {
        let mut ctx = healthy_ctx();
        ctx.market.paused_globally = true;
        let r = check_global_paused(&ctx);
        assert_eq!(r.status, CheckStatus::Critical);
        assert_eq!(r.severity, Severity::P0);
    }

    #[test]
    fn schema_mismatch_is_p1_critical() {
        let mut ctx = healthy_ctx();
        ctx.market.schema_version_onchain = 2;
        ctx.market.schema_version_compiled = 1;
        let r = check_schema_version(&ctx);
        assert_eq!(r.status, CheckStatus::Critical);
        assert_eq!(r.severity, Severity::P1);
        assert!(r.message.contains("on-chain=2"));
        assert!(r.message.contains("compiled=1"));
    }

    #[test]
    fn dormant_inventory_warn_at_2k_critical_at_5k() {
        let mut ctx = healthy_ctx();
        ctx.sub_pools[0].dormant_ticks = 2_500;
        let r = check_dormant_inventory(&ctx);
        assert_eq!(r.status, CheckStatus::Warn);

        ctx.sub_pools[0].dormant_ticks = 5_001;
        let r = check_dormant_inventory(&ctx);
        assert_eq!(r.status, CheckStatus::Critical);
    }

    #[test]
    fn recovery_outstanding_warn_warn_critical_thresholds() {
        let mut ctx = healthy_ctx();
        // 0.5 % = warn
        ctx.pool.recovery_outstanding_micro_usdc = 500_000_000;
        let r = check_recovery_outstanding(&ctx);
        assert_eq!(r.status, CheckStatus::Warn);

        // 1.5 % = critical P0
        ctx.pool.recovery_outstanding_micro_usdc = 1_500_000_000;
        let r = check_recovery_outstanding(&ctx);
        assert_eq!(r.status, CheckStatus::Critical);
        assert_eq!(r.severity, Severity::P0);
    }

    #[test]
    fn keeper_alive_critical_when_no_heartbeat() {
        let mut ctx = healthy_ctx();
        ctx.keeper.heartbeat_within_60s = false;
        let r = check_keeper_alive(&ctx);
        assert_eq!(r.status, CheckStatus::Critical);
        assert_eq!(r.severity, Severity::P1);
    }

    #[test]
    fn keeper_failed_actions_rate_promotes_to_critical_at_5() {
        let mut ctx = healthy_ctx();
        ctx.keeper.failed_actions_last_hour = 4;
        assert_eq!(check_keeper_failed_actions_rate(&ctx).status, CheckStatus::Warn);
        ctx.keeper.failed_actions_last_hour = 5;
        assert_eq!(check_keeper_failed_actions_rate(&ctx).status, CheckStatus::Critical);
    }

    #[test]
    fn vol_estimator_warn_after_3_consecutive_warming_ticks() {
        let mut ctx = healthy_ctx();
        ctx.keeper.last_applied_vol = None;
        ctx.keeper.consecutive_warming_ticks = 2;
        assert_eq!(check_keeper_vol_estimator(&ctx).status, CheckStatus::Pass);
        ctx.keeper.consecutive_warming_ticks = 3;
        assert_eq!(check_keeper_vol_estimator(&ctx).status, CheckStatus::Warn);
    }

    #[test]
    fn wallet_balance_critical_below_0_2_sol() {
        let mut ctx = healthy_ctx();
        ctx.keeper.wallet_balance_lamports = 199_999_999;
        let r = check_keeper_wallet_balance(&ctx);
        assert_eq!(r.status, CheckStatus::Critical);
        assert_eq!(r.severity, Severity::P1);
    }

    #[test]
    fn oracle_slot_age_p0_critical_above_64() {
        let mut ctx = healthy_ctx();
        ctx.oracle.slot_age = 65;
        let r = check_oracle_slot_age(&ctx);
        assert_eq!(r.status, CheckStatus::Critical);
        assert_eq!(r.severity, Severity::P0);
    }

    #[test]
    fn pending_init_hints_warn_at_50() {
        let mut ctx = healthy_ctx();
        ctx.sub_pools[0].pending_init_hints = 49;
        assert_eq!(check_pending_init_hints(&ctx).status, CheckStatus::Pass);
        ctx.sub_pools[0].pending_init_hints = 50;
        assert_eq!(check_pending_init_hints(&ctx).status, CheckStatus::Warn);
    }

    #[test]
    fn rpc_latency_promotes_warn_at_200ms() {
        let mut ctx = healthy_ctx();
        ctx.rpc.primary_get_slot_p95_ms = 199;
        assert_eq!(check_rpc_primary_latency(&ctx).status, CheckStatus::Pass);
        ctx.rpc.primary_get_slot_p95_ms = 250;
        assert_eq!(check_rpc_primary_latency(&ctx).status, CheckStatus::Warn);
        ctx.rpc.primary_get_slot_p95_ms = 1_500;
        assert_eq!(check_rpc_primary_latency(&ctx).status, CheckStatus::Critical);
    }

    #[test]
    fn empty_subpool_array_does_not_panic() {
        let mut ctx = healthy_ctx();
        ctx.sub_pools.clear();
        let r = run_all_checks(&ctx);
        // Wave 24 — 18 wave-12 + 3 wave-17 leader-lock + 1 wave-24 drift.
        assert_eq!(r.checks.len(), 22);
        // trading_activity warns when total qty is 0
        let trading = r
            .checks
            .iter()
            .find(|c| c.name == "trading_activity")
            .unwrap();
        assert_eq!(trading.status, CheckStatus::Warn);
    }

    /// Exit-code derivation: a single P0 critical drives exit 4 even
    /// if 17 other checks pass.
    #[test]
    fn run_all_with_p0_critical_drives_exit_4() {
        let mut ctx = healthy_ctx();
        ctx.market.paused_globally = true; // P0 Critical
        let report = run_all_checks(&ctx);
        assert_eq!(crate::report::exit_code_for_status(&report), 4);
    }

    // Wave 24 — position principal/notional drift reconciliation.

    #[test]
    fn drift_check_skips_when_probe_not_run() {
        let mut ctx = healthy_ctx();
        ctx.pool.onchain_position_notional_micro_usdc = 0; // probe off
        let r = check_position_principal_drift(&ctx);
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.message.contains("skipped"));
        let enabled = r
            .measurements
            .iter()
            .find(|(k, _)| *k == "drift_enabled")
            .unwrap()
            .1;
        assert_eq!(enabled, 0.0);
    }

    #[test]
    fn drift_check_passes_when_reconciled() {
        let ctx = healthy_ctx(); // onchain == reported == 100_000_000_000
        let r = check_position_principal_drift(&ctx);
        assert_eq!(r.status, CheckStatus::Pass);
    }

    #[test]
    fn drift_check_warns_at_one_percent() {
        let mut ctx = healthy_ctx();
        // reported 100_000_000_000; on-chain 1% higher → Warn band.
        ctx.pool.onchain_position_notional_micro_usdc = 101_000_000_000;
        let r = check_position_principal_drift(&ctx);
        assert_eq!(r.status, CheckStatus::Warn);
        assert_eq!(r.severity, Severity::P2);
    }

    #[test]
    fn drift_check_critical_at_five_percent() {
        let mut ctx = healthy_ctx();
        // on-chain 5% below reported → Critical (P1).
        ctx.pool.onchain_position_notional_micro_usdc = 95_000_000_000;
        let r = check_position_principal_drift(&ctx);
        assert_eq!(r.status, CheckStatus::Critical);
        assert_eq!(r.severity, Severity::P1);
    }

    #[test]
    fn drift_check_is_symmetric_in_direction() {
        // Over-report and under-report of the same magnitude land in
        // the same band (abs_diff).
        let mut over = healthy_ctx();
        over.pool.onchain_position_notional_micro_usdc = 103_000_000_000;
        let mut under = healthy_ctx();
        under.pool.onchain_position_notional_micro_usdc = 97_000_000_000;
        assert_eq!(
            check_position_principal_drift(&over).status,
            check_position_principal_drift(&under).status
        );
    }
}
