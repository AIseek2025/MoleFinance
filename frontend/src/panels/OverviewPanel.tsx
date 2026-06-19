// Wave 28 — protocol Overview landing page.
//
// The launch landing surface: a protocol-wide rollup of value locked,
// open interest, long/short skew, recovery outstanding, and per-market
// health — so a visitor (or operator) sees the whole protocol's pulse
// before drilling into a single market via the other tabs.
//
// Data comes from `aggregateProtocolStats(feed)`, the frontend mirror
// of the backend `ops_toolkit::protocol_summary` rollup.

import type { JSX } from "react";

import type { FeedSnapshot } from "../types";
import {
  aggregateProtocolStats,
  longShareBps,
  type MarketStat,
} from "../feed/protocolStats";
import type { ProberSnapshot } from "../feed/proberSnapshot";
import { formatUsdcMicro } from "../format";

interface OverviewPanelProps {
  feed: FeedSnapshot;
  /** Wave 29 — protocol health from the prober daemon (optional). */
  prober?: ProberSnapshot | null;
}

function ProtocolHealthBanner({
  prober,
}: {
  prober: ProberSnapshot | null | undefined;
}): JSX.Element | null {
  const p = prober?.protocol;
  if (!p) return null;
  const cls =
    p.overallStatus === "CRITICAL"
      ? "critical"
      : p.overallStatus === "WARN"
        ? "warn"
        : "ok";
  return (
    <div className={`ov-health ov-health-${cls}`}>
      <span className={`ov-health-badge ov-health-${cls}`}>
        {p.overallStatus}
      </span>
      <span className="ov-health-text">
        protocol health · {p.healthyMarkets}/{p.markets} healthy
        {p.warnMarkets > 0 ? ` · ${p.warnMarkets} warn` : ""}
        {p.criticalMarkets > 0 ? ` · ${p.criticalMarkets} critical` : ""}
        {p.firingChecks > 0 ? ` · ${p.firingChecks} checks firing` : ""}
      </span>
    </div>
  );
}

function skewLabel(netSkewMicro: bigint): { text: string; cls: string } {
  if (netSkewMicro === 0n) return { text: "balanced", cls: "neutral" };
  return netSkewMicro > 0n
    ? { text: "long-heavy", cls: "long" }
    : { text: "short-heavy", cls: "short" };
}

function MarketRow({ m }: { m: MarketStat }): JSX.Element {
  return (
    <tr>
      <td>
        <span className="ov-sym">{m.symbol}</span>
      </td>
      <td>
        <span className={`ov-pill ov-pill-${m.paused ? "paused" : "active"}`}>
          {m.paused ? "paused" : "active"}
        </span>
      </td>
      <td className="ov-num">{formatUsdcMicro(m.collateralMicro)}</td>
      <td className="ov-num ov-long">{formatUsdcMicro(m.longCollateralMicro)}</td>
      <td className="ov-num ov-short">
        {formatUsdcMicro(m.shortCollateralMicro)}
      </td>
      <td className="ov-num">{m.openPositions}</td>
      <td className="ov-num">{formatUsdcMicro(m.recoveryOutstandingMicro)}</td>
    </tr>
  );
}

export function OverviewPanel({
  feed,
  prober,
}: OverviewPanelProps): JSX.Element {
  const stats = aggregateProtocolStats(feed);
  const skew = skewLabel(stats.netSkewMicro);
  const longBps = longShareBps(stats);
  const longPct = (longBps / 100).toFixed(1);
  const shortPct = (100 - longBps / 100).toFixed(1);

  return (
    <section className="overview">
      <ProtocolHealthBanner prober={prober} />
      <div className="ov-kpis">
        <div className="ov-card">
          <span className="ov-card-label">Total value locked</span>
          <span className="ov-card-value">{formatUsdcMicro(stats.tvlMicro)}</span>
          <span className="ov-card-sub">USDC across all sub-pools</span>
        </div>
        <div className="ov-card">
          <span className="ov-card-label">Markets</span>
          <span className="ov-card-value">{stats.markets}</span>
          <span className="ov-card-sub">
            {stats.activeMarkets} active · {stats.pausedMarkets} paused
          </span>
        </div>
        <div className="ov-card">
          <span className="ov-card-label">Open positions</span>
          <span className="ov-card-value">{stats.openPositions}</span>
          <span className="ov-card-sub">live across protocol</span>
        </div>
        <div className="ov-card">
          <span className="ov-card-label">Net skew</span>
          <span className={`ov-card-value ov-skew-${skew.cls}`}>
            {skew.text}
          </span>
          <span className="ov-card-sub">
            {formatUsdcMicro(
              stats.netSkewMicro < 0n
                ? -stats.netSkewMicro
                : stats.netSkewMicro,
            )}{" "}
            imbalance
          </span>
        </div>
        <div className="ov-card">
          <span className="ov-card-label">Recovery outstanding</span>
          <span className="ov-card-value">
            {formatUsdcMicro(stats.recoveryOutstandingMicro)}
          </span>
          <span className="ov-card-sub">dormant recovery owed</span>
        </div>
      </div>

      <div className="ov-skew-bar" title={`long ${longPct}% / short ${shortPct}%`}>
        <div className="ov-skew-long" style={{ width: `${longPct}%` }}>
          {longBps > 1500 ? `long ${longPct}%` : ""}
        </div>
        <div className="ov-skew-short" style={{ width: `${shortPct}%` }}>
          {longBps < 8500 ? `short ${shortPct}%` : ""}
        </div>
      </div>

      <table className="ov-table">
        <thead>
          <tr>
            <th>Market</th>
            <th>State</th>
            <th className="ov-num">Collateral</th>
            <th className="ov-num">Long</th>
            <th className="ov-num">Short</th>
            <th className="ov-num">Positions</th>
            <th className="ov-num">Recovery</th>
          </tr>
        </thead>
        <tbody>
          {stats.perMarket.map((m) => (
            <MarketRow key={m.marketPdaHex} m={m} />
          ))}
        </tbody>
      </table>
    </section>
  );
}
