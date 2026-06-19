// Wave 23 — open-interest aggregation from the live position feed.
//
// Wave 22 made `feed.positions` an end-to-end live stream: the
// websocket / multi-market adapters now decode on-chain `Position`
// PDAs and tag each summary with `marketPdaHex`. Wave 23 folds that
// stream into a per-market long/short exposure KPI — the frontend
// mirror of the backend `ops_toolkit::position_interest::OpenInterestFacts`.
//
// The two aggregators intentionally compute the *same* shape from the
// *same* on-chain fields:
//   - count            ← number of live positions
//   - collateral       ← Position.principal      (PositionSummary.collateral)
//   - qty              ← Position.active_shares   (PositionSummary.qty)
//
// `feed.positions` is already filtered to displayable positions
// (closed status removed) by the adapter layer, so this helper does
// not re-filter — it sums whatever it is handed.

import type { PositionSummary } from "../types";

/** Per-market (or global) open-interest aggregate. */
export interface OpenInterestStats {
  /** Number of live Long positions. */
  longCount: number;
  /** Number of live Short positions. */
  shortCount: number;
  /** Sum of collateral (principal, microUSDC) across live Longs. */
  longCollateral: bigint;
  /** Sum of collateral (principal, microUSDC) across live Shorts. */
  shortCollateral: bigint;
  /** Sum of qty (active_shares) across live Longs. */
  longQty: bigint;
  /** Sum of qty (active_shares) across live Shorts. */
  shortQty: bigint;
}

/** Zeroed stats — the identity element for the aggregator. */
export function emptyOpenInterest(): OpenInterestStats {
  return {
    longCount: 0,
    shortCount: 0,
    longCollateral: 0n,
    shortCollateral: 0n,
    longQty: 0n,
    shortQty: 0n,
  };
}

/** Fold a list of positions into a single [`OpenInterestStats`]. */
export function aggregateOpenInterest(
  positions: readonly PositionSummary[],
): OpenInterestStats {
  const stats = emptyOpenInterest();
  for (const pos of positions) {
    if (pos.direction === "Long") {
      stats.longCount += 1;
      stats.longCollateral += pos.collateral;
      stats.longQty += pos.qty;
    } else {
      stats.shortCount += 1;
      stats.shortCollateral += pos.collateral;
      stats.shortQty += pos.qty;
    }
  }
  return stats;
}

/**
 * Group positions by `marketPdaHex` and aggregate each group.
 * Positions without a `marketPdaHex` tag are bucketed under the
 * empty-string key (legacy single-market snapshots).
 */
export function openInterestByMarket(
  positions: readonly PositionSummary[],
): Map<string, OpenInterestStats> {
  const byMarket = new Map<string, OpenInterestStats>();
  for (const pos of positions) {
    const key = pos.marketPdaHex ?? "";
    let stats = byMarket.get(key);
    if (!stats) {
      stats = emptyOpenInterest();
      byMarket.set(key, stats);
    }
    if (pos.direction === "Long") {
      stats.longCount += 1;
      stats.longCollateral += pos.collateral;
      stats.longQty += pos.qty;
    } else {
      stats.shortCount += 1;
      stats.shortCollateral += pos.collateral;
      stats.shortQty += pos.qty;
    }
  }
  return byMarket;
}

/** Total live position count (Long + Short). */
export function totalCount(stats: OpenInterestStats): number {
  return stats.longCount + stats.shortCount;
}

/** Total collateral committed across all live positions (microUSDC). */
export function totalCollateral(stats: OpenInterestStats): bigint {
  return stats.longCollateral + stats.shortCollateral;
}

/**
 * Signed collateral imbalance (`long − short`, microUSDC). Positive
 * means the book is net-long. Drives the trader panel's directional-
 * skew indicator.
 */
export function netCollateralImbalance(stats: OpenInterestStats): bigint {
  return stats.longCollateral - stats.shortCollateral;
}

// ---------------------------------------------------------------------
// Wave 24 — principal reconciliation (frontend mirror of the backend
// `position_principal_drift` health check).
//
// The backend reconciles on-chain `Position.notional` against the
// indexer-reported pool notional. The frontend has collateral
// (`Position.principal`) on both sides — live positions and sub-pool
// summaries — so it reconciles principal-to-principal. Same intent:
// detect when the indexer and the on-chain position set disagree.
// ---------------------------------------------------------------------

/** Reconciliation verdict. `disabled` == no live positions to compare. */
export type ReconcileStatus = "ok" | "warn" | "critical" | "disabled";

/** Result of a principal reconciliation. */
export interface PrincipalReconciliation {
  /** Sum of collateral across live on-chain positions (microUSDC). */
  onchainCollateral: bigint;
  /** Sum of collateral the indexer reports across sub-pools (microUSDC). */
  reportedCollateral: bigint;
  /** `|onchain − reported| / max(reported, 1)`. */
  driftRatio: number;
  /** Verdict band. */
  status: ReconcileStatus;
}

/**
 * Reconcile on-chain aggregated collateral against the indexer-
 * reported collateral. Thresholds mirror the backend check:
 * `ok` < 0.5 %, `warn` < 2 %, `critical` ≥ 2 %. Returns `disabled`
 * when there is no on-chain collateral to compare (no decoded live
 * positions this snapshot).
 */
export function reconcilePrincipal(
  onchainCollateral: bigint,
  reportedCollateral: bigint,
): PrincipalReconciliation {
  if (onchainCollateral === 0n) {
    return {
      onchainCollateral,
      reportedCollateral,
      driftRatio: 0,
      status: "disabled",
    };
  }
  const diff =
    onchainCollateral > reportedCollateral
      ? onchainCollateral - reportedCollateral
      : reportedCollateral - onchainCollateral;
  const denom = reportedCollateral > 0n ? reportedCollateral : 1n;
  const driftRatio = Number(diff) / Number(denom);
  const status: ReconcileStatus =
    driftRatio >= 0.02 ? "critical" : driftRatio >= 0.005 ? "warn" : "ok";
  return { onchainCollateral, reportedCollateral, driftRatio, status };
}

/**
 * Wave 27 — reconcile the program's own aggregate principal
 * (`Market.current_total_principal`) against the sum of collateral
 * across live on-chain positions. The frontend mirror of the backend's
 * wave-27 `RpcMarketFetcher` lift, where the reported notional comes
 * from the `Market` counter rather than a fixture — so the drift check
 * compares two INDEPENDENT on-chain truths (the program aggregate vs
 * the position set). A non-trivial gap means the program's running
 * counter and the actual open positions disagree.
 *
 * Returns `disabled` when there are no live positions (nothing to
 * reconcile this snapshot) or the market aggregate is unavailable.
 */
export function reconcileProgramAggregate(
  positions: readonly PositionSummary[],
  marketAggregatePrincipal: bigint | undefined,
): PrincipalReconciliation {
  const stats = aggregateOpenInterest(positions);
  const positionCollateral = totalCollateral(stats);
  if (marketAggregatePrincipal === undefined) {
    return {
      onchainCollateral: positionCollateral,
      reportedCollateral: 0n,
      driftRatio: 0,
      status: "disabled",
    };
  }
  return reconcilePrincipal(positionCollateral, marketAggregatePrincipal);
}

/**
 * Wave 25 — per-market principal reconciliation. The frontend mirror
 * of the backend's wave-25 prober wiring, where each market runs its
 * OWN `position_principal_drift` check against a per-market on-chain
 * open-interest slice (`fetch_open_interest_for_market`).
 *
 * Groups the live positions by `marketPdaHex`, then reconciles each
 * market's on-chain collateral against `reportedByMarket` (the
 * indexer-reported collateral keyed by the same hex). Markets present
 * on only one side are still emitted (the missing side counts as 0),
 * so a market the indexer reports but the chain has no live positions
 * for surfaces as `disabled`, and vice-versa surfaces as drift.
 */
export function reconcileByMarket(
  positions: readonly PositionSummary[],
  reportedByMarket: ReadonlyMap<string, bigint>,
): Map<string, PrincipalReconciliation> {
  const onchain = openInterestByMarket(positions);
  const keys = new Set<string>([
    ...onchain.keys(),
    ...reportedByMarket.keys(),
  ]);
  const out = new Map<string, PrincipalReconciliation>();
  for (const key of keys) {
    const stats = onchain.get(key);
    const onchainCollateral = stats ? totalCollateral(stats) : 0n;
    const reportedCollateral = reportedByMarket.get(key) ?? 0n;
    out.set(key, reconcilePrincipal(onchainCollateral, reportedCollateral));
  }
  return out;
}
