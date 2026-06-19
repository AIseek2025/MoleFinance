// Wave 28 — protocol-wide aggregation unit tests.
//
// Mirrors the backend `ops_toolkit::protocol_summary` rollup intent:
// fold N markets into one protocol verdict (TVL, OI, skew, paused
// counts) and keep single-market mode meaningful.
//
// @vitest-environment node
import { describe, expect, it } from "vitest";

import { aggregateProtocolStats, longShareBps } from "./protocolStats";
import { buildSnapshot } from "./mockGenerator";
import type {
  Direction,
  FeedSnapshot,
  MarketSummary,
  MarketViewEntry,
  PositionSummary,
  SubPoolSummary,
} from "../types";

function subPool(
  id: number,
  longCollateral: bigint,
  shortCollateral: bigint,
): SubPoolSummary {
  return {
    id,
    pubkey: { hex: `${id}`.padStart(2, "0").repeat(32).slice(0, 64) },
    totalOpenLongQty: longCollateral / 10n,
    totalOpenShortQty: shortCollateral / 10n,
    longCollateral,
    shortCollateral,
    dormantInventory: { Long: 0, Short: 0 } as Record<Direction, number>,
  };
}

function marketSummary(
  symbol: string,
  hex: string,
  paused: boolean,
): MarketSummary {
  return {
    pubkey: { hex },
    symbol,
    schemaVersion: 1,
    paused,
    pausedGlobally: false,
    frozenNewPosition: false,
    midPriceMicro: 1_000_000n,
    lastOracleSlot: 10,
    currentSlot: 12,
  };
}

function entry(
  symbol: string,
  hex: string,
  subPools: SubPoolSummary[],
  paused = false,
  recovery = 0n,
): MarketViewEntry {
  return {
    symbol,
    marketPdaHex: hex,
    lockPdaHex: "ff".repeat(32),
    marketSummary: marketSummary(symbol, hex, paused),
    subPools,
    projectedRecoveryOutstandingMicroUsdc: recovery,
  };
}

function pos(hex: string, direction: Direction): PositionSummary {
  return {
    owner: { hex: "aa".repeat(32) },
    subPoolId: 0,
    direction,
    qty: 100n,
    collateral: 1_000n,
    openedAt: 1_700_000_000,
    marketPdaHex: hex,
  };
}

const A = "11".repeat(32);
const B = "22".repeat(32);

function multiMarketFeed(): FeedSnapshot {
  const base = buildSnapshot(0, { submitted: 0, failed: 0, skipped: 0 });
  const entries = new Map<string, MarketViewEntry>();
  // Market A: 3_000 long + 1_000 short = 4_000 collateral, active.
  entries.set(
    "SOL-USD",
    entry("SOL-USD", A, [subPool(0, 3_000n, 1_000n)], false, 500n),
  );
  // Market B: 1_000 long + 1_000 short = 2_000 collateral, paused.
  entries.set(
    "BTC-USD",
    entry("BTC-USD", B, [subPool(0, 1_000n, 1_000n)], true, 250n),
  );
  return {
    ...base,
    marketsView: { entries },
    positions: [pos(A, "Long"), pos(A, "Short"), pos(B, "Long")],
  };
}

describe("aggregateProtocolStats — multi-market", () => {
  it("rolls up TVL, skew, and recovery across markets", () => {
    const s = aggregateProtocolStats(multiMarketFeed());
    expect(s.markets).toBe(2);
    expect(s.tvlMicro).toBe(6_000n);
    expect(s.longCollateralMicro).toBe(4_000n);
    expect(s.shortCollateralMicro).toBe(2_000n);
    expect(s.netSkewMicro).toBe(2_000n);
    expect(s.recoveryOutstandingMicro).toBe(750n);
  });

  it("counts paused vs active markets", () => {
    const s = aggregateProtocolStats(multiMarketFeed());
    expect(s.pausedMarkets).toBe(1);
    expect(s.activeMarkets).toBe(1);
  });

  it("groups open positions by owning market", () => {
    const s = aggregateProtocolStats(multiMarketFeed());
    expect(s.openPositions).toBe(3);
    const a = s.perMarket.find((m) => m.marketPdaHex === A);
    const b = s.perMarket.find((m) => m.marketPdaHex === B);
    expect(a?.openPositions).toBe(2);
    expect(b?.openPositions).toBe(1);
  });

  it("sorts perMarket by collateral descending", () => {
    const s = aggregateProtocolStats(multiMarketFeed());
    expect(s.perMarket[0]!.symbol).toBe("SOL-USD");
    expect(s.perMarket[1]!.symbol).toBe("BTC-USD");
  });

  it("computes long share in basis points", () => {
    // 4000 / 6000 = 6666.67bps → floored to 6666.
    expect(longShareBps(aggregateProtocolStats(multiMarketFeed()))).toBe(6666);
  });
});

describe("aggregateProtocolStats — single-market fallback", () => {
  it("folds the lone indexer snapshot when marketsView is absent", () => {
    const feed = buildSnapshot(0, { submitted: 0, failed: 0, skipped: 0 });
    const s = aggregateProtocolStats(feed);
    expect(s.markets).toBe(1);
    // Single-market: every position belongs to the one market.
    expect(s.openPositions).toBe(feed.positions.length);
    const expectedTvl = feed.indexer.subPools.reduce(
      (acc, sp) => acc + sp.longCollateral + sp.shortCollateral,
      0n,
    );
    expect(s.tvlMicro).toBe(expectedTvl);
  });

  it("treats an empty marketsView as single-market fallback", () => {
    const feed = buildSnapshot(0, { submitted: 0, failed: 0, skipped: 0 });
    const withEmpty: FeedSnapshot = {
      ...feed,
      marketsView: { entries: new Map() },
    };
    expect(aggregateProtocolStats(withEmpty).markets).toBe(1);
  });

  it("reports zero long share when there is no collateral", () => {
    const feed = buildSnapshot(0, { submitted: 0, failed: 0, skipped: 0 });
    const entries = new Map<string, MarketViewEntry>();
    entries.set("EMPTY", entry("EMPTY", A, [subPool(0, 0n, 0n)]));
    const s = aggregateProtocolStats({ ...feed, marketsView: { entries } });
    expect(longShareBps(s)).toBe(0);
  });
});
