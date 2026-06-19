// Wave 23 — open-interest aggregation unit tests.
//
// Mirrors the backend `ops_toolkit::position_interest` test matrix so
// the two aggregators stay byte-aligned on semantics (long/short
// split, collateral + qty sums, net skew sign).
//
// @vitest-environment node
import { describe, expect, it } from "vitest";

import {
  aggregateOpenInterest,
  emptyOpenInterest,
  netCollateralImbalance,
  openInterestByMarket,
  reconcileByMarket,
  reconcilePrincipal,
  reconcileProgramAggregate,
  totalCollateral,
  totalCount,
} from "./openInterest";
import type { Direction, PositionSummary } from "../types";

function pos(
  direction: Direction,
  collateral: bigint,
  qty: bigint,
  marketPdaHex?: string,
): PositionSummary {
  return {
    owner: { hex: "aa".repeat(32) },
    subPoolId: 0,
    direction,
    qty,
    collateral,
    openedAt: 1_700_000_000,
    ...(marketPdaHex !== undefined && { marketPdaHex }),
  };
}

describe("aggregateOpenInterest", () => {
  it("splits long and short exposure", () => {
    const stats = aggregateOpenInterest([
      pos("Long", 1_000n, 10n),
      pos("Long", 2_000n, 20n),
      pos("Short", 500n, 5n),
    ]);
    expect(stats.longCount).toBe(2);
    expect(stats.shortCount).toBe(1);
    expect(stats.longCollateral).toBe(3_000n);
    expect(stats.shortCollateral).toBe(500n);
    expect(stats.longQty).toBe(30n);
    expect(stats.shortQty).toBe(5n);
    expect(totalCount(stats)).toBe(3);
    expect(totalCollateral(stats)).toBe(3_500n);
    expect(netCollateralImbalance(stats)).toBe(2_500n);
  });

  it("returns the identity for an empty list", () => {
    const stats = aggregateOpenInterest([]);
    expect(stats).toEqual(emptyOpenInterest());
    expect(totalCount(stats)).toBe(0);
    expect(netCollateralImbalance(stats)).toBe(0n);
  });

  it("reports negative skew for a short-heavy book", () => {
    const stats = aggregateOpenInterest([
      pos("Long", 100n, 1n),
      pos("Short", 900n, 9n),
    ]);
    expect(netCollateralImbalance(stats)).toBe(-800n);
  });
});

describe("openInterestByMarket", () => {
  it("groups positions by marketPdaHex", () => {
    const mktA = "11".repeat(32);
    const mktB = "22".repeat(32);
    const byMarket = openInterestByMarket([
      pos("Long", 1_000n, 10n, mktA),
      pos("Short", 400n, 4n, mktA),
      pos("Long", 7_000n, 70n, mktB),
    ]);
    expect(byMarket.size).toBe(2);
    const a = byMarket.get(mktA)!;
    expect(a.longCount).toBe(1);
    expect(a.shortCount).toBe(1);
    expect(totalCollateral(a)).toBe(1_400n);
    const b = byMarket.get(mktB)!;
    expect(b.longCount).toBe(1);
    expect(b.longCollateral).toBe(7_000n);
  });

  it("buckets untagged positions under the empty-string key", () => {
    const byMarket = openInterestByMarket([
      pos("Long", 100n, 1n),
      pos("Short", 200n, 2n),
    ]);
    expect(byMarket.size).toBe(1);
    const legacy = byMarket.get("")!;
    expect(totalCount(legacy)).toBe(2);
    expect(legacy.longCollateral).toBe(100n);
    expect(legacy.shortCollateral).toBe(200n);
  });
});

describe("reconcilePrincipal", () => {
  it("is disabled when there is no on-chain collateral", () => {
    const r = reconcilePrincipal(0n, 5_000n);
    expect(r.status).toBe("disabled");
    expect(r.driftRatio).toBe(0);
  });

  it("reports ok when reconciled within 0.5%", () => {
    const r = reconcilePrincipal(100_000n, 100_000n);
    expect(r.status).toBe("ok");
    expect(r.driftRatio).toBe(0);
  });

  it("warns between 0.5% and 2% drift", () => {
    const r = reconcilePrincipal(101_000n, 100_000n);
    expect(r.status).toBe("warn");
    expect(r.driftRatio).toBeCloseTo(0.01, 6);
  });

  it("goes critical at or above 2% drift", () => {
    const r = reconcilePrincipal(95_000n, 100_000n);
    expect(r.status).toBe("critical");
    expect(r.driftRatio).toBeCloseTo(0.05, 6);
  });

  it("is symmetric in over- vs under-report", () => {
    const over = reconcilePrincipal(103_000n, 100_000n);
    const under = reconcilePrincipal(97_000n, 100_000n);
    expect(over.status).toBe(under.status);
    expect(over.driftRatio).toBeCloseTo(under.driftRatio, 6);
  });
});

describe("reconcileByMarket", () => {
  const mktA = "11".repeat(32);
  const mktB = "22".repeat(32);

  it("reconciles each market against its own reported collateral", () => {
    const positions = [
      pos("Long", 60_000n, 10n, mktA),
      pos("Short", 40_000n, 4n, mktA),
      pos("Long", 50_000n, 5n, mktB),
    ];
    const reported = new Map<string, bigint>([
      [mktA, 100_000n], // matches on-chain 100_000 → ok
      [mktB, 51_500n], // 3% above on-chain 50_000 → critical
    ]);
    const byMarket = reconcileByMarket(positions, reported);
    expect(byMarket.get(mktA)!.status).toBe("ok");
    expect(byMarket.get(mktA)!.onchainCollateral).toBe(100_000n);
    expect(byMarket.get(mktB)!.status).toBe("critical");
  });

  it("emits markets present on only one side", () => {
    const positions = [pos("Long", 10_000n, 1n, mktA)];
    // mktB reported by indexer but no on-chain positions.
    const reported = new Map<string, bigint>([[mktB, 5_000n]]);
    const byMarket = reconcileByMarket(positions, reported);
    expect(byMarket.size).toBe(2);
    // mktA: on-chain present, indexer reports 0 → drift (critical).
    expect(byMarket.get(mktA)!.onchainCollateral).toBe(10_000n);
    expect(byMarket.get(mktA)!.reportedCollateral).toBe(0n);
    // mktB: no on-chain collateral → disabled.
    expect(byMarket.get(mktB)!.status).toBe("disabled");
  });
});

describe("reconcileProgramAggregate", () => {
  it("reconciles position collateral against the market aggregate", () => {
    const positions = [pos("Long", 4_000n, 1n), pos("Short", 3_000n, 1n)];
    const r = reconcileProgramAggregate(positions, 7_000n);
    expect(r.onchainCollateral).toBe(7_000n);
    expect(r.reportedCollateral).toBe(7_000n);
    expect(r.status).toBe("ok");
    expect(r.driftRatio).toBeCloseTo(0);
  });

  it("flags a critical drift when the aggregate diverges", () => {
    const positions = [pos("Long", 7_000n, 1n)];
    const r = reconcileProgramAggregate(positions, 5_000n);
    expect(r.status).toBe("critical");
    expect(r.driftRatio).toBeCloseTo(0.4);
  });

  it("is disabled when the market aggregate is unavailable", () => {
    const positions = [pos("Long", 7_000n, 1n)];
    const r = reconcileProgramAggregate(positions, undefined);
    expect(r.status).toBe("disabled");
    expect(r.onchainCollateral).toBe(7_000n);
  });

  it("is disabled when there are no live positions", () => {
    const r = reconcileProgramAggregate([], 7_000n);
    expect(r.status).toBe("disabled");
  });
});
