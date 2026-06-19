// Wave 28 — protocol-wide aggregation for the Overview landing page.
//
// Every other panel zooms into ONE market (Trader / Indexer / Keeper).
// The launch landing page needs the opposite: a single protocol-level
// rollup answering "how big is the protocol and is it healthy right
// now?" — total value locked, open interest, long/short skew, recovery
// outstanding, and per-market health at a glance.
//
// This is the frontend mirror of the backend's wave-28
// `ops_toolkit::protocol_summary`: both fold N markets into one
// protocol verdict so the ops status line and this page agree.

import type {
  FeedSnapshot,
  MarketViewEntry,
  PositionSummary,
  SubPoolSummary,
} from "../types";

/** One market's slice of the protocol rollup. */
export interface MarketStat {
  symbol: string;
  marketPdaHex: string;
  paused: boolean;
  /** Σ (long + short) sub-pool collateral for this market, microUSDC. */
  collateralMicro: bigint;
  longCollateralMicro: bigint;
  shortCollateralMicro: bigint;
  /** Live open positions whose `marketPdaHex` matches this market. */
  openPositions: number;
  /** Projected recovery outstanding for this market, microUSDC. */
  recoveryOutstandingMicro: bigint;
}

/** Protocol-wide headline numbers for the Overview page. */
export interface ProtocolStats {
  /** Markets observed (multi-market mode) or 1 (single-market mode). */
  markets: number;
  /** Markets whose `paused || pausedGlobally` flag is set. */
  pausedMarkets: number;
  /** Markets actively trading (`markets - pausedMarkets`). */
  activeMarkets: number;
  /** Total live open positions across every market. */
  openPositions: number;
  /** Total value locked = Σ all sub-pool collateral, microUSDC. */
  tvlMicro: bigint;
  longCollateralMicro: bigint;
  shortCollateralMicro: bigint;
  /** Long − short collateral, microUSDC (signed). */
  netSkewMicro: bigint;
  /** Σ projected recovery outstanding across markets, microUSDC. */
  recoveryOutstandingMicro: bigint;
  /** Per-market breakdown, sorted by collateral descending. */
  perMarket: MarketStat[];
}

function subPoolCollateral(
  subPools: readonly SubPoolSummary[] | undefined,
): { long: bigint; short: bigint } {
  let long = 0n;
  let short = 0n;
  for (const sp of subPools ?? []) {
    long += sp.longCollateral;
    short += sp.shortCollateral;
  }
  return { long, short };
}

function countPositionsForMarket(
  positions: readonly PositionSummary[],
  marketPdaHex: string,
  singleMarket: boolean,
): number {
  // Single-market snapshots leave `marketPdaHex` undefined on every
  // position — they all belong to the one market, so count them all.
  if (singleMarket) return positions.length;
  return positions.filter((p) => p.marketPdaHex === marketPdaHex).length;
}

function statFromEntry(
  entry: MarketViewEntry,
  positions: readonly PositionSummary[],
): MarketStat {
  const { long, short } = subPoolCollateral(entry.subPools);
  const summary = entry.marketSummary;
  const paused = Boolean(summary?.paused || summary?.pausedGlobally);
  return {
    symbol: entry.symbol,
    marketPdaHex: entry.marketPdaHex,
    paused,
    collateralMicro: long + short,
    longCollateralMicro: long,
    shortCollateralMicro: short,
    openPositions: countPositionsForMarket(positions, entry.marketPdaHex, false),
    recoveryOutstandingMicro: entry.projectedRecoveryOutstandingMicroUsdc ?? 0n,
  };
}

/**
 * Aggregate a [`FeedSnapshot`] into protocol-wide headline stats.
 *
 * Multi-market mode (adapter populated `feed.marketsView`): folds every
 * entry. Single-market mode: folds the lone `feed.indexer` snapshot so
 * the Overview page still renders meaningfully with one market.
 */
export function aggregateProtocolStats(feed: FeedSnapshot): ProtocolStats {
  const entries = feed.marketsView
    ? Array.from(feed.marketsView.entries.values())
    : [];

  const perMarket: MarketStat[] =
    entries.length > 0
      ? entries.map((e) => statFromEntry(e, feed.positions))
      : [singleMarketStat(feed)];

  perMarket.sort((a, b) =>
    a.collateralMicro === b.collateralMicro
      ? 0
      : a.collateralMicro > b.collateralMicro
        ? -1
        : 1,
  );

  let tvl = 0n;
  let long = 0n;
  let short = 0n;
  let recovery = 0n;
  let paused = 0;
  let positions = 0;
  for (const m of perMarket) {
    tvl += m.collateralMicro;
    long += m.longCollateralMicro;
    short += m.shortCollateralMicro;
    recovery += m.recoveryOutstandingMicro;
    positions += m.openPositions;
    if (m.paused) paused += 1;
  }

  return {
    markets: perMarket.length,
    pausedMarkets: paused,
    activeMarkets: perMarket.length - paused,
    openPositions: positions,
    tvlMicro: tvl,
    longCollateralMicro: long,
    shortCollateralMicro: short,
    netSkewMicro: long - short,
    recoveryOutstandingMicro: recovery,
    perMarket,
  };
}

function singleMarketStat(feed: FeedSnapshot): MarketStat {
  const { long, short } = subPoolCollateral(feed.indexer.subPools);
  const market = feed.indexer.market;
  const paused = Boolean(market.paused || market.pausedGlobally);
  return {
    symbol: market.symbol,
    marketPdaHex: market.pubkey.hex,
    paused,
    collateralMicro: long + short,
    longCollateralMicro: long,
    shortCollateralMicro: short,
    openPositions: countPositionsForMarket(
      feed.positions,
      market.pubkey.hex,
      true,
    ),
    recoveryOutstandingMicro:
      feed.indexer.projectedRecoveryOutstandingMicroUsdc,
  };
}

/**
 * Long share of collateral in basis points (0..10000). Returns 0 when
 * there is no collateral (avoids a divide-by-zero in the gauge).
 */
export function longShareBps(stats: ProtocolStats): number {
  const total = stats.longCollateralMicro + stats.shortCollateralMicro;
  if (total === 0n) return 0;
  return Number((stats.longCollateralMicro * 10_000n) / total);
}
