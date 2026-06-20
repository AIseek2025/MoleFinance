// Dashboard — protocol-wide market overview (renamed from "Overview").
//
// A protocol-wide rollup of value locked, open interest, long/short
// skew, recovery outstanding, live oracle price, and per-market health.
// Data comes from `aggregateProtocolStats(feed)`, the frontend mirror of
// the backend `ops_toolkit::protocol_summary` rollup.

import type { JSX } from "react";
import { useTranslation } from "react-i18next";

import type { FeedSnapshot } from "../types";
import {
  aggregateProtocolStats,
  longShareBps,
  type MarketStat,
} from "../feed/protocolStats";
import type { ProberSnapshot } from "../feed/proberSnapshot";
import { formatUsdcMicro, formatPriceMicro } from "../format";

interface OverviewPanelProps {
  feed: FeedSnapshot;
  prober?: ProberSnapshot | null;
}

function ProtocolHealthBanner({
  prober,
}: {
  prober: ProberSnapshot | null | undefined;
}): JSX.Element | null {
  const { t } = useTranslation();
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
      <span className={`ov-health-badge ov-health-${cls}`}>{p.overallStatus}</span>
      <span className="ov-health-text">
        {t("dashboard.health")} ·{" "}
        {t("dashboard.healthy", { n: p.healthyMarkets, total: p.markets })}
        {p.warnMarkets > 0 ? ` · ${t("dashboard.warnSuffix", { n: p.warnMarkets })}` : ""}
        {p.criticalMarkets > 0
          ? ` · ${t("dashboard.criticalSuffix", { n: p.criticalMarkets })}`
          : ""}
        {p.firingChecks > 0
          ? ` · ${t("dashboard.checksFiring", { n: p.firingChecks })}`
          : ""}
      </span>
    </div>
  );
}

function MarketRow({ m }: { m: MarketStat }): JSX.Element {
  const { t } = useTranslation();
  return (
    <tr>
      <td>
        <span className="ov-sym">{m.symbol}</span>
      </td>
      <td>
        <span className={`ov-pill ov-pill-${m.paused ? "paused" : "active"}`}>
          {m.paused ? t("dashboard.paused") : t("dashboard.active")}
        </span>
      </td>
      <td className="ov-num">{formatUsdcMicro(m.collateralMicro)}</td>
      <td className="ov-num ov-long">{formatUsdcMicro(m.longCollateralMicro)}</td>
      <td className="ov-num ov-short">{formatUsdcMicro(m.shortCollateralMicro)}</td>
      <td className="ov-num">{m.openPositions}</td>
      <td className="ov-num">{formatUsdcMicro(m.recoveryOutstandingMicro)}</td>
    </tr>
  );
}

export function OverviewPanel({
  feed,
  prober,
}: OverviewPanelProps): JSX.Element {
  const { t } = useTranslation();
  const stats = aggregateProtocolStats(feed);
  const longBps = longShareBps(stats);
  const longPct = (longBps / 100).toFixed(1);
  const shortPct = (100 - longBps / 100).toFixed(1);
  const skewText =
    stats.netSkewMicro === 0n
      ? t("dashboard.skewBalanced")
      : stats.netSkewMicro > 0n
        ? t("dashboard.skewLong")
        : t("dashboard.skewShort");
  const skewCls =
    stats.netSkewMicro === 0n ? "neutral" : stats.netSkewMicro > 0n ? "long" : "short";

  const market = feed.indexer.market;
  const oracleLag = Math.max(0, market.currentSlot - market.lastOracleSlot);

  return (
    <section className="overview">
      <ProtocolHealthBanner prober={prober} />
      <div className="ov-kpis">
        <div className="ov-card">
          <span className="ov-card-label">{t("dashboard.tvl")}</span>
          <span className="ov-card-value">{formatUsdcMicro(stats.tvlMicro)}</span>
          <span className="ov-card-sub">{t("dashboard.tvlSub")}</span>
        </div>
        <div className="ov-card">
          <span className="ov-card-label">{t("dashboard.livePrice")}</span>
          <span className="ov-card-value">${formatPriceMicro(market.midPriceMicro)}</span>
          <span className="ov-card-sub">{t("dashboard.livePriceSub")}</span>
        </div>
        <div className="ov-card">
          <span className="ov-card-label">{t("dashboard.markets")}</span>
          <span className="ov-card-value">{stats.markets}</span>
          <span className="ov-card-sub">
            {t("dashboard.marketsSub", {
              active: stats.activeMarkets,
              paused: stats.pausedMarkets,
            })}
          </span>
        </div>
        <div className="ov-card">
          <span className="ov-card-label">{t("dashboard.openPositions")}</span>
          <span className="ov-card-value">{stats.openPositions}</span>
          <span className="ov-card-sub">{t("dashboard.openPositionsSub")}</span>
        </div>
        <div className="ov-card">
          <span className="ov-card-label">{t("dashboard.netSkew")}</span>
          <span className={`ov-card-value ov-skew-${skewCls}`}>{skewText}</span>
          <span className="ov-card-sub">
            {t("dashboard.imbalance", {
              amount: formatUsdcMicro(
                stats.netSkewMicro < 0n ? -stats.netSkewMicro : stats.netSkewMicro,
              ),
            })}
          </span>
        </div>
        <div className="ov-card">
          <span className="ov-card-label">{t("dashboard.oracleLag")}</span>
          <span className="ov-card-value">{oracleLag}</span>
          <span className="ov-card-sub">{t("dashboard.oracleLagSub")}</span>
        </div>
        <div className="ov-card">
          <span className="ov-card-label">{t("dashboard.recovery")}</span>
          <span className="ov-card-value">
            {formatUsdcMicro(stats.recoveryOutstandingMicro)}
          </span>
          <span className="ov-card-sub">{t("dashboard.recoverySub")}</span>
        </div>
      </div>

      <div
        className="ov-skew-bar"
        title={t("dashboard.longShort", { long: longPct, short: shortPct })}
      >
        <div className="ov-skew-long" style={{ width: `${longPct}%` }}>
          {longBps > 1500 ? `${t("dashboard.colLong")} ${longPct}%` : ""}
        </div>
        <div className="ov-skew-short" style={{ width: `${shortPct}%` }}>
          {longBps < 8500 ? `${t("dashboard.colShort")} ${shortPct}%` : ""}
        </div>
      </div>

      <table className="ov-table">
        <thead>
          <tr>
            <th>{t("dashboard.colMarket")}</th>
            <th>{t("dashboard.colState")}</th>
            <th className="ov-num">{t("dashboard.colCollateral")}</th>
            <th className="ov-num">{t("dashboard.colLong")}</th>
            <th className="ov-num">{t("dashboard.colShort")}</th>
            <th className="ov-num">{t("dashboard.colPositions")}</th>
            <th className="ov-num">{t("dashboard.colRecovery")}</th>
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
