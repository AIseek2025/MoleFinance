/**
 * Wave 19 — selectActiveMarketSnapshot tests.
 *
 * Verifies the snapshot rewriter:
 *   1. Returns input untouched when marketsView is absent.
 *   2. Returns input untouched when symbol isn't in entries.
 *   3. Returns input untouched when entry has no marketSummary yet.
 *   4. Swaps indexer/keeper to the selected market's data when ready.
 *   5. Flips keeper.status to 'paused' when the selected market is paused.
 *
 * @vitest-environment node
 */
import { describe, expect, it } from "vitest";
import {
  filterPositionsByMarket,
  selectActiveMarketSnapshot,
} from "./selectMarket";
import type {
  DormantBucketSummary,
  FeedSnapshot,
  KeeperState,
  MarketSummary,
  MarketViewEntry,
  MultiMarketView,
  PositionSummary,
  SubPoolSummary,
} from "../types";

function baseFeed(view?: MultiMarketView): FeedSnapshot {
  return {
    indexer: {
      slot: 100,
      market: {
        pubkey: { hex: "0".repeat(64) },
        symbol: "PRIMARY",
        schemaVersion: 1,
        paused: false,
        pausedGlobally: false,
        frozenNewPosition: false,
        midPriceMicro: 1n,
        lastOracleSlot: 0,
        currentSlot: 100,
      },
      subPools: [],
      dormantBuckets: [],
      pendingInitHints: [
        { subPoolId: 1, direction: "Long", tick: 7, hintSlot: 100 },
      ],
      projectedRecoveryOutstandingMicroUsdc: 0n,
    },
    keeper: {
      status: "running",
      metrics: {
        tickSlot: 0,
        appliedVol: null,
        volSamples: 0,
        cumulative: { submitted: 0, failed: 0, skipped: 0 },
        recent: { submitted: 0, failed: 0, skipped: 0, durationMs: 0 },
        walletBalanceSol: 0,
      },
      predictions: [],
      recentSignatures: [],
    },
    positions: [],
    timestampMs: 0,
    ...(view !== undefined && { marketsView: view }),
  };
}

function entry(opts: {
  symbol: string;
  decoded?: boolean;
  paused?: boolean;
  subPools?: SubPoolSummary[];
  buckets?: DormantBucketSummary[];
  slot?: number;
  outstanding?: bigint;
}): MarketViewEntry {
  const market: MarketSummary | undefined = opts.decoded
    ? {
        pubkey: { hex: "1".repeat(64) },
        symbol: opts.symbol,
        schemaVersion: 2,
        paused: opts.paused ?? false,
        pausedGlobally: opts.paused ?? false,
        frozenNewPosition: false,
        midPriceMicro: 99n,
        lastOracleSlot: 0,
        currentSlot: 0,
      }
    : undefined;
  const e: MarketViewEntry = {
    symbol: opts.symbol,
    marketPdaHex: "1".repeat(64),
    lockPdaHex: "2".repeat(64),
  };
  if (market) e.marketSummary = market;
  if (opts.subPools) e.subPools = opts.subPools;
  if (opts.buckets) e.dormantBuckets = opts.buckets;
  if (opts.slot !== undefined) e.indexerSlot = opts.slot;
  if (opts.outstanding !== undefined) {
    e.projectedRecoveryOutstandingMicroUsdc = opts.outstanding;
  }
  return e;
}

describe("selectActiveMarketSnapshot", () => {
  it("returns input unchanged when marketsView is absent", () => {
    const f = baseFeed();
    expect(selectActiveMarketSnapshot(f, "BTC-USD")).toBe(f);
  });

  it("returns input unchanged when symbol is empty", () => {
    const view: MultiMarketView = {
      entries: new Map([["SOL-USD", entry({ symbol: "SOL-USD" })]]),
    };
    const f = baseFeed(view);
    expect(selectActiveMarketSnapshot(f, "")).toBe(f);
  });

  it("returns input unchanged when symbol not in entries", () => {
    const view: MultiMarketView = {
      entries: new Map([["SOL-USD", entry({ symbol: "SOL-USD" })]]),
    };
    const f = baseFeed(view);
    expect(selectActiveMarketSnapshot(f, "MISSING")).toBe(f);
  });

  it("returns input unchanged when entry has no decoded summary yet", () => {
    const view: MultiMarketView = {
      entries: new Map([
        ["SOL-USD", entry({ symbol: "SOL-USD", decoded: false })],
      ]),
    };
    const f = baseFeed(view);
    expect(selectActiveMarketSnapshot(f, "SOL-USD")).toBe(f);
  });

  it("swaps indexer/keeper to the selected market's decoded data", () => {
    const sp: SubPoolSummary = {
      id: 7,
      pubkey: { hex: "3".repeat(64) },
      totalOpenLongQty: 100n,
      totalOpenShortQty: 50n,
      longCollateral: 1_000n,
      shortCollateral: 500n,
      dormantInventory: { Long: 1, Short: 0 },
    };
    const view: MultiMarketView = {
      entries: new Map([
        [
          "BTC-USD",
          entry({
            symbol: "BTC-USD",
            decoded: true,
            subPools: [sp],
            slot: 555,
            outstanding: 1_234n,
          }),
        ],
      ]),
    };
    const f = baseFeed(view);
    const out = selectActiveMarketSnapshot(f, "BTC-USD");
    expect(out).not.toBe(f);
    expect(out.indexer.market.symbol).toBe("BTC-USD");
    expect(out.indexer.subPools).toEqual([sp]);
    expect(out.indexer.slot).toBe(555);
    expect(out.indexer.projectedRecoveryOutstandingMicroUsdc).toBe(1_234n);
    // Pending init hints carry across (they're not per-market yet).
    expect(out.indexer.pendingInitHints).toEqual([
      { subPoolId: 1, direction: "Long", tick: 7, hintSlot: 100 },
    ]);
  });

  it("flips keeper.status to paused when selected market is paused", () => {
    const view: MultiMarketView = {
      entries: new Map([
        [
          "BTC-USD",
          entry({ symbol: "BTC-USD", decoded: true, paused: true }),
        ],
      ]),
    };
    const f = baseFeed(view);
    const out = selectActiveMarketSnapshot(f, "BTC-USD");
    expect(out.keeper.status).toBe("paused");
  });

  // ----------------------------------------------------------------
  // Wave 20 — multi-market position filter + per-market keeperState
  // ----------------------------------------------------------------

  it("filters feed.positions to only those tagged with active market", () => {
    const btcMarketHex = "1".repeat(64);
    const solMarketHex = "2".repeat(64);
    const positions: PositionSummary[] = [
      {
        owner: { hex: "a".repeat(64) },
        subPoolId: 0,
        direction: "Long",
        qty: 1n,
        collateral: 100n,
        openedAt: 1,
        marketPdaHex: btcMarketHex,
      },
      {
        owner: { hex: "b".repeat(64) },
        subPoolId: 1,
        direction: "Short",
        qty: 2n,
        collateral: 200n,
        openedAt: 2,
        marketPdaHex: solMarketHex,
      },
      {
        // Untagged position — wave-9..18 mocks; should be kept.
        owner: { hex: "c".repeat(64) },
        subPoolId: 2,
        direction: "Long",
        qty: 3n,
        collateral: 300n,
        openedAt: 3,
      },
    ];
    const view: MultiMarketView = {
      entries: new Map([
        [
          "BTC-USD",
          {
            ...entry({ symbol: "BTC-USD", decoded: true }),
            marketPdaHex: btcMarketHex,
          },
        ],
      ]),
    };
    const f: FeedSnapshot = {
      ...baseFeed(view),
      positions,
    };
    const out = selectActiveMarketSnapshot(f, "BTC-USD");
    expect(out.positions.map((p) => p.subPoolId)).toEqual([0, 2]);
  });

  it("uses per-market keeperState when MarketViewEntry exposes one", () => {
    const perMarketKeeper: KeeperState = {
      status: "running",
      metrics: {
        tickSlot: 9999,
        appliedVol: 1.23,
        volSamples: 45,
        cumulative: { submitted: 7, failed: 1, skipped: 2 },
        recent: { submitted: 1, failed: 0, skipped: 0, durationMs: 50 },
        walletBalanceSol: 4.2,
      },
      predictions: [],
      recentSignatures: [],
    };
    const e = entry({ symbol: "BTC-USD", decoded: true });
    e.keeperState = perMarketKeeper;
    const view: MultiMarketView = {
      entries: new Map([["BTC-USD", e]]),
    };
    const f = baseFeed(view);
    const out = selectActiveMarketSnapshot(f, "BTC-USD");
    expect(out.keeper.metrics.tickSlot).toBe(9999);
    expect(out.keeper.metrics.appliedVol).toBe(1.23);
    expect(out.keeper.metrics.cumulative.submitted).toBe(7);
  });

  it("keeperState fallback: uses global feed.keeper when entry lacks one", () => {
    const view: MultiMarketView = {
      entries: new Map([
        ["BTC-USD", entry({ symbol: "BTC-USD", decoded: true })],
      ]),
    };
    const f = baseFeed(view);
    f.keeper.metrics.tickSlot = 12345;
    const out = selectActiveMarketSnapshot(f, "BTC-USD");
    expect(out.keeper.metrics.tickSlot).toBe(12345);
  });
});

describe("filterPositionsByMarket (wave 20 helper)", () => {
  const matching: PositionSummary = {
    owner: { hex: "a".repeat(64) },
    subPoolId: 0,
    direction: "Long",
    qty: 1n,
    collateral: 1n,
    openedAt: 0,
    marketPdaHex: "1".repeat(64),
  };
  const otherMarket: PositionSummary = {
    ...matching,
    subPoolId: 1,
    marketPdaHex: "2".repeat(64),
  };
  const untagged: PositionSummary = {
    ...matching,
    subPoolId: 2,
  };
  // remove `marketPdaHex` for the untagged fixture
  delete (untagged as { marketPdaHex?: string }).marketPdaHex;

  it("keeps matching-market positions", () => {
    expect(filterPositionsByMarket([matching], "1".repeat(64))).toEqual([
      matching,
    ]);
  });
  it("drops other-market positions", () => {
    expect(filterPositionsByMarket([otherMarket], "1".repeat(64))).toEqual([]);
  });
  it("keeps untagged positions for back-compat", () => {
    expect(filterPositionsByMarket([untagged], "1".repeat(64))).toEqual([
      untagged,
    ]);
  });
  it("mixed list — matching + untagged kept, mismatched dropped", () => {
    const out = filterPositionsByMarket(
      [matching, otherMarket, untagged],
      "1".repeat(64),
    );
    expect(out.map((p) => p.subPoolId).sort()).toEqual([0, 2]);
  });
});
