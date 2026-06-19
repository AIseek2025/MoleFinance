// Wave 12 — pure data generator extracted from wave-11
// `mocks/feed.ts`. Has zero React or browser dependencies.
//
// The generator is fully deterministic — `buildSnapshot(N,
// cumulative)` always returns the same `FeedSnapshot` for the same
// `N` regardless of when called. Cumulative counts are passed in
// because they're history-dependent.

import type {
  Direction,
  DormantBucketSummary,
  FeedSnapshot,
  IndexerSnapshot,
  KeeperLoopMetrics,
  KeeperState,
  MarketSummary,
  PendingInitHint,
  PositionSummary,
  RotatePrediction,
  SubPoolSummary,
} from "../types";

export const FEED_TICK_INTERVAL_MS = 800;
const STARTING_SLOT = 217_000_000;
const VOL_WARM_UP_TICKS = 32;

export const FEED_FAKE_PROGRAM_ID = {
  hex: "504a7e0102d9c0a5e4b8136d5a0b3f1a8a87d8b5b6a3c5b9d6e7f0a1b2c3d4e5",
};
export const FEED_FAKE_MARKET_PDA = {
  hex: "1f4d33b7e8a9c0d5e6f78091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5",
};
export const FEED_FAKE_SYMBOL = "BTC-PERP";

function pkFromSeed(seed: number): { hex: string } {
  const buf = new Uint8Array(32);
  let x = seed >>> 0;
  for (let i = 0; i < 32; i += 1) {
    x = (x * 1_103_515_245 + 12_345) >>> 0;
    buf[i] = x & 0xff;
  }
  let hex = "";
  for (const b of buf) {
    hex += b.toString(16).padStart(2, "0");
  }
  return { hex };
}

function dirOfId(id: number): Direction {
  return id % 2 === 0 ? "Long" : "Short";
}

function priceAt(tickIndex: number): bigint {
  const base = 60_000.0;
  const drift = Math.sin(tickIndex / 17) * 250;
  const wobble = Math.sin(tickIndex / 3.7) * 35;
  const px = base + drift + wobble;
  return BigInt(Math.round(px * 1_000_000));
}

function buildMarket(tickIndex: number, slot: number): MarketSummary {
  return {
    pubkey: FEED_FAKE_MARKET_PDA,
    symbol: FEED_FAKE_SYMBOL,
    schemaVersion: 1,
    paused: false,
    pausedGlobally: false,
    frozenNewPosition: false,
    midPriceMicro: priceAt(tickIndex),
    lastOracleSlot: slot - 2,
    currentSlot: slot,
    // Wave 27 — program aggregate; matches the sum of `buildPositions`
    // collateral (5 × ~1.4e9 ≈ 7e9) so the demo reconciliation reads
    // healthy. Notional is a representative ~5× leverage figure.
    currentTotalPrincipal: 7_000_000_000n,
    currentTotalNotional: 35_000_000_000n,
  };
}

function buildSubPools(tickIndex: number): SubPoolSummary[] {
  const pools: SubPoolSummary[] = [];
  for (let id = 0; id < 4; id += 1) {
    const longSeed = 100 + id * 23 + Math.floor(tickIndex / 5);
    const shortSeed = 200 + id * 17 + Math.floor(tickIndex / 7);
    pools.push({
      id,
      pubkey: pkFromSeed(1000 + id),
      totalOpenLongQty: BigInt(longSeed * 1000),
      totalOpenShortQty: BigInt(shortSeed * 1000),
      longCollateral: BigInt(longSeed * 1_000_000_000),
      shortCollateral: BigInt(shortSeed * 1_000_000_000),
      dormantInventory: {
        Long: 4 + ((tickIndex + id) % 5),
        Short: 3 + ((tickIndex + id * 3) % 7),
      },
    });
  }
  return pools;
}

function buildDormantBuckets(tickIndex: number): DormantBucketSummary[] {
  const out: DormantBucketSummary[] = [];
  for (let id = 0; id < 4; id += 1) {
    const baseTick = 60_000 + id * 50;
    for (let k = 0; k < 4; k += 1) {
      const tick = baseTick + k * 25 + (tickIndex % 5);
      out.push({
        subPoolId: id,
        direction: k % 2 === 0 ? "Long" : "Short",
        tick,
        totalShares: BigInt((id + 1) * (k + 1) * 100_000),
        pendingRecoveryMicroUsdc: BigInt((id + 1) * (k + 1) * 12_345),
        readyToClose: (tickIndex + k) % 11 === 0,
      });
    }
  }
  return out;
}

function buildPendingInitHints(tickIndex: number): PendingInitHint[] {
  const hints: PendingInitHint[] = [];
  const wave = (tickIndex >> 2) & 3;
  for (let i = 0; i < wave; i += 1) {
    const subPoolId = (tickIndex + i) % 4;
    hints.push({
      subPoolId,
      direction: dirOfId(subPoolId + i),
      tick: 60_500 + ((tickIndex + i * 5) % 200),
      hintSlot: STARTING_SLOT + tickIndex - 5 + i,
    });
  }
  return hints;
}

function buildPredictions(tickIndex: number): RotatePrediction[] {
  const preds: RotatePrediction[] = [];
  for (let id = 0; id < 4; id += 1) {
    for (const direction of ["Long", "Short"] as const) {
      const baseScore = 0.2 + (((tickIndex + id) % 10) / 20);
      const score = Math.min(0.99, baseScore + (direction === "Long" ? 0.05 : 0));
      preds.push({
        subPoolId: id,
        direction,
        tick: 60_500 + ((tickIndex + id * 3) % 250),
        score,
        triggered: score > 0.45,
      });
    }
  }
  return preds.sort((a, b) => b.score - a.score);
}

function buildKeeperMetrics(
  tickIndex: number,
  slot: number,
  cumulative: { submitted: number; failed: number; skipped: number },
): KeeperLoopMetrics {
  const isWarming = tickIndex < VOL_WARM_UP_TICKS;
  const appliedVol = isWarming
    ? null
    : 0.85 + Math.sin(tickIndex / 13) * 0.15 + (Math.cos(tickIndex / 23) * 0.05);
  return {
    tickSlot: slot,
    appliedVol,
    volSamples: Math.min(VOL_WARM_UP_TICKS, tickIndex),
    cumulative,
    recent: {
      submitted: tickIndex % 4 === 0 ? 2 : 1,
      failed: tickIndex % 23 === 0 ? 1 : 0,
      skipped: tickIndex % 9 === 0 ? 1 : 0,
      durationMs: 95 + (tickIndex % 5) * 7,
    },
    walletBalanceSol: 1.92 - tickIndex * 0.0001,
  };
}

function buildPositions(tickIndex: number): PositionSummary[] {
  const out: PositionSummary[] = [];
  for (let i = 0; i < 5; i += 1) {
    out.push({
      owner: pkFromSeed(7000 + i),
      subPoolId: i % 4,
      direction: i % 2 === 0 ? "Long" : "Short",
      qty: BigInt(50 + i * 15 + (tickIndex % 7)),
      collateral: BigInt(1_000_000_000 + i * 200_000_000),
      openedAt: 1_700_000_000 - i * 600,
      // Wave 20 — tag with the mock market PDA so
      // `selectActiveMarketSnapshot`'s wave-20 filter behaves
      // identically in mock and live paths.
      marketPdaHex: FEED_FAKE_MARKET_PDA.hex,
    });
  }
  return out;
}

export function buildSnapshot(
  tickIndex: number,
  prevCumulative: { submitted: number; failed: number; skipped: number },
): FeedSnapshot {
  const slot = STARTING_SLOT + tickIndex * 2;
  const market = buildMarket(tickIndex, slot);
  const subPools = buildSubPools(tickIndex);
  const dormantBuckets = buildDormantBuckets(tickIndex);
  const pendingInitHints = buildPendingInitHints(tickIndex);
  const predictions = buildPredictions(tickIndex);

  const recentSubmitted = tickIndex % 4 === 0 ? 2 : 1;
  const recentFailed = tickIndex % 23 === 0 ? 1 : 0;
  const recentSkipped = tickIndex % 9 === 0 ? 1 : 0;
  const cumulative = {
    submitted: prevCumulative.submitted + recentSubmitted,
    failed: prevCumulative.failed + recentFailed,
    skipped: prevCumulative.skipped + recentSkipped,
  };

  const metrics = buildKeeperMetrics(tickIndex, slot, cumulative);

  const recentSignatures: string[] = [];
  for (let i = 0; i < recentSubmitted; i += 1) {
    recentSignatures.push(
      `${pkFromSeed(slot * 10 + i).hex.slice(0, 16)}…${pkFromSeed(slot * 10 + i).hex.slice(-12)}`,
    );
  }

  const keeper: KeeperState = {
    status: tickIndex < VOL_WARM_UP_TICKS ? "warming_up" : "running",
    metrics,
    predictions,
    recentSignatures,
  };

  const indexer: IndexerSnapshot = {
    slot,
    market,
    subPools,
    dormantBuckets,
    pendingInitHints,
    projectedRecoveryOutstandingMicroUsdc: dormantBuckets.reduce(
      (acc, b) => acc + b.pendingRecoveryMicroUsdc,
      0n,
    ),
  };

  return {
    indexer,
    keeper,
    positions: buildPositions(tickIndex),
    timestampMs: 1_700_000_000_000 + tickIndex * FEED_TICK_INTERVAL_MS,
  };
}
