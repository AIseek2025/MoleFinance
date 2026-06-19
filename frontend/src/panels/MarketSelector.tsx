// Wave 19 — top-bar market selector for multi-market deployments.
//
// Wave 18 introduced `LeaderLockGrid` so operators could SEE every
// market at once. Wave 19 finishes the user-facing loop: the
// trader / indexer / keeper panels now render data for ONE market
// at a time, and `MarketSelector` is the control that picks which
// one. Selection is persisted across reloads (localStorage +
// `?market=` URL param) so deep links work.

import type { JSX } from "react";

import type { MultiMarketView, PositionSummary } from "../types";
import { decodeKeeperLeaderLockBytes } from "../tx/wasmBuilder";
import {
  openInterestByMarket,
  reconcileByMarket,
  totalCount,
  type OpenInterestStats,
  type PrincipalReconciliation,
} from "../feed/openInterest";

export interface MarketSelectorProps {
  /** All symbols available to the operator (sorted alphabetically). */
  symbols: string[];
  /** Currently active symbol — must be a member of `symbols`. */
  active: string;
  /** Click handler when the operator picks a different symbol. */
  onChange: (symbol: string) => void;
  /** Live multi-market view used to flag stale / leaderless markets. */
  view?: MultiMarketView;
  /** Cluster slot — used to compute per-market freshness. */
  currentSlot?: bigint;
  /**
   * Wave 25 — the global live position feed. Used to surface each
   * market's on-chain open-interest count + indexer reconciliation in
   * the pill tooltip, mirroring the backend's per-market
   * `position_principal_drift` check wired into the prober loop.
   */
  positions?: readonly PositionSummary[];
}

export function MarketSelector(props: MarketSelectorProps): JSX.Element {
  if (props.symbols.length === 0) {
    return <div className="market-selector market-selector-empty" />;
  }
  const positions = props.positions ?? [];
  const oiByMarket = openInterestByMarket(positions);
  const reconByMarket = reconcileByMarket(
    positions,
    reportedCollateralByMarket(props.view),
  );
  return (
    <div className="market-selector" role="tablist" aria-label="Select market">
      {props.symbols.map((s) => {
        const indicator = computeIndicator(s, props.view, props.currentSlot);
        const hex = props.view?.entries.get(s)?.marketPdaHex;
        const oi = hex ? oiByMarket.get(hex) : undefined;
        const recon = hex ? reconByMarket.get(hex) : undefined;
        return (
          <button
            key={s}
            type="button"
            role="tab"
            aria-selected={s === props.active}
            className={`market-pill ${s === props.active ? "active" : ""} indicator-${indicator}`}
            onClick={() => props.onChange(s)}
            title={pillTitle(indicator, oi, recon)}
          >
            <span className="market-pill-symbol">{s}</span>
            {oi && totalCount(oi) > 0 ? (
              <span className="market-pill-oi">{totalCount(oi)}</span>
            ) : null}
            <span
              className={`market-pill-dot dot-${indicator}`}
              title={indicatorTitle(indicator)}
            />
          </button>
        );
      })}
    </div>
  );
}

/**
 * Build per-market indexer-reported collateral (microUSDC) keyed by
 * `marketPdaHex`, summed across each entry's sub-pools. Feeds the
 * wave-25 per-market reconciliation tooltip.
 */
export function reportedCollateralByMarket(
  view: MultiMarketView | undefined,
): Map<string, bigint> {
  const out = new Map<string, bigint>();
  if (!view) return out;
  for (const entry of view.entries.values()) {
    let sum = 0n;
    for (const sp of entry.subPools ?? []) {
      sum += sp.longCollateral + sp.shortCollateral;
    }
    out.set(entry.marketPdaHex, sum);
  }
  return out;
}

/** Compose the pill hover tooltip from freshness + open-interest. */
export function pillTitle(
  indicator: Indicator,
  oi: OpenInterestStats | undefined,
  recon: PrincipalReconciliation | undefined,
): string {
  const parts = [indicatorTitle(indicator)];
  if (oi && totalCount(oi) > 0) {
    parts.push(`${totalCount(oi)} live positions (${oi.longCount}L/${oi.shortCount}S)`);
  }
  if (recon && recon.status !== "disabled") {
    const pct = (recon.driftRatio * 100).toFixed(2);
    parts.push(`indexer reconciliation: ${recon.status} (${pct}% drift)`);
  }
  return parts.join(" · ");
}

type Indicator = "fresh" | "stale" | "uninitialised" | "unowned" | "unknown";

function computeIndicator(
  symbol: string,
  view: MultiMarketView | undefined,
  currentSlot: bigint | undefined,
): Indicator {
  if (!view) return "unknown";
  const entry = view.entries.get(symbol);
  if (!entry) return "unknown";
  if (!entry.lockBytes || entry.lockBytes.length === 0) return "uninitialised";
  let decoded;
  try {
    decoded = decodeKeeperLeaderLockBytes(entry.lockBytes);
  } catch {
    return "unknown";
  }
  if (!decoded.hasLeader) return "unowned";
  if (currentSlot === undefined) return "fresh";
  const elapsed = currentSlot - decoded.lastHeartbeatSlot;
  return elapsed > decoded.takeoverThresholdSlots ? "stale" : "fresh";
}

function indicatorTitle(i: Indicator): string {
  switch (i) {
    case "fresh":
      return "Leader heartbeat fresh";
    case "stale":
      return "Leader heartbeat stale — takeover window open";
    case "unowned":
      return "Leader slot empty";
    case "uninitialised":
      return "Lock PDA not initialised yet";
    case "unknown":
      return "Status unknown";
  }
}
