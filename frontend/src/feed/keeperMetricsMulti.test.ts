import { describe, expect, it } from "vitest";

import type { FeedSnapshot, KeeperState } from "../types";
import {
  keeperStateMapFromMetricsMulti,
  mergeKeeperMetricsIntoFeed,
  metricsJsonToKeeperState,
  parseMetricsMultiJson,
} from "./keeperMetricsMulti";

const SAMPLE_JSON = `[{"market":"SOL-USD","metrics":{"ticksTotal":42,"actionsSubmittedTotal":7,"actionsFailedTotal":1,"actionsSkippedTotal":2,"lastTickDurationMs":50,"volSamples":10,"walletBalanceLamports":1500000000,"appliedVolMilli":850,"leaderStatus":"leader"}}]`;

describe("keeperMetricsMulti (wave 22)", () => {
  it("parseMetricsMultiJson parses wave-21 array shape", () => {
    const entries = parseMetricsMultiJson(SAMPLE_JSON);
    expect(entries).toHaveLength(1);
    expect(entries[0]!.market).toBe("SOL-USD");
    expect(entries[0]!.metrics.ticksTotal).toBe(42);
  });

  it("metricsJsonToKeeperState maps appliedVolMilli to appliedVol", () => {
    const ks = metricsJsonToKeeperState({
      ticksTotal: 10,
      actionsSubmittedTotal: 3,
      actionsFailedTotal: 0,
      actionsSkippedTotal: 1,
      lastTickDurationMs: 25,
      volSamples: 5,
      walletBalanceLamports: 2_000_000_000,
      appliedVolMilli: 1234,
      leaderStatus: "standby",
    });
    expect(ks.metrics.appliedVol).toBe(1.234);
    expect(ks.metrics.walletBalanceSol).toBe(2);
    expect(ks.status).toBe("running");
  });

  it("metricsJsonToKeeperState uses warming_up when vol is null and samples low", () => {
    const ks = metricsJsonToKeeperState({
      ticksTotal: 0,
      actionsSubmittedTotal: 0,
      actionsFailedTotal: 0,
      actionsSkippedTotal: 0,
      lastTickDurationMs: 0,
      volSamples: 0,
      walletBalanceLamports: 0,
      appliedVolMilli: null,
      leaderStatus: "unknown",
    });
    expect(ks.status).toBe("warming_up");
  });

  it("mergeKeeperMetricsIntoFeed fills marketsView keeperState", () => {
    const byMarket = keeperStateMapFromMetricsMulti(parseMetricsMultiJson(SAMPLE_JSON));
    const base: FeedSnapshot = {
      indexer: {
        slot: 0,
        market: {
          pubkey: { hex: "a".repeat(64) },
          symbol: "SOL-USD",
          schemaVersion: 1,
          paused: false,
          pausedGlobally: false,
          frozenNewPosition: false,
          midPriceMicro: 1n,
          lastOracleSlot: 0,
          currentSlot: 0,
        },
        subPools: [],
        dormantBuckets: [],
        pendingInitHints: [],
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
      timestampMs: 1,
      marketsView: {
        entries: new Map([
          [
            "SOL-USD",
            {
              symbol: "SOL-USD",
              marketPdaHex: "a".repeat(64),
              lockPdaHex: "b".repeat(64),
            },
          ],
        ]),
      },
    };
    const merged = mergeKeeperMetricsIntoFeed(base, byMarket);
    const entry = merged.marketsView!.entries.get("SOL-USD")!;
    expect(entry.keeperState?.metrics.cumulative.submitted).toBe(7);
    expect(merged.keeper.metrics.cumulative.submitted).toBe(7);
  });

  it("mergeKeeperMetricsIntoFeed overwrites single-market feed.keeper", () => {
    const ks: KeeperState = metricsJsonToKeeperState({
      ticksTotal: 99,
      actionsSubmittedTotal: 5,
      actionsFailedTotal: 0,
      actionsSkippedTotal: 0,
      lastTickDurationMs: 10,
      volSamples: 3,
      walletBalanceLamports: 0,
      appliedVolMilli: 500,
      leaderStatus: "leader",
    });
    const base: FeedSnapshot = {
      indexer: {
        slot: 0,
        market: {
          pubkey: { hex: "a".repeat(64) },
          symbol: "BTC-USD",
          schemaVersion: 1,
          paused: false,
          pausedGlobally: false,
          frozenNewPosition: false,
          midPriceMicro: 1n,
          lastOracleSlot: 0,
          currentSlot: 0,
        },
        subPools: [],
        dormantBuckets: [],
        pendingInitHints: [],
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
      timestampMs: 1,
    };
    const merged = mergeKeeperMetricsIntoFeed(
      base,
      new Map([["BTC-USD", ks]]),
    );
    expect(merged.keeper.metrics.tickSlot).toBe(99);
  });
});
