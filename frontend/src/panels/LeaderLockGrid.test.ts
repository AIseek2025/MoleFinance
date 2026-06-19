/**
 * Wave 18 — LeaderLockGrid pure-helper tests.
 *
 * Verifies the row-derivation contract:
 *
 *   1. Empty / undefined view → 0 rows.
 *   2. Multi-market view → one row per entry, alphabetic order.
 *   3. Per-row state matches the wave-16 banner state machine
 *      (uninitialised / unowned / fresh / stale).
 *   4. Decode failures don't crash; row falls back to
 *      "uninitialised".
 *   5. `expected` map flags `expectedMismatch` only on `fresh` /
 *      `stale` rows where the holder hex differs.
 *
 * @vitest-environment node
 */
import { describe, expect, it } from "vitest";

import { computeLeaderLockGridRows } from "./LeaderLockGrid";
import type { MultiMarketView } from "../types";
import type { KeeperLeaderLockView } from "../tx/wasmBuilder";

function lockView(args: {
  hasLeader: boolean;
  leaderByte: number;
  lastHb: bigint;
  threshold: bigint;
}): KeeperLeaderLockView {
  const leader = new Uint8Array(32);
  leader[0] = args.leaderByte;
  // Wave-15 wasm-decoded view normally carries `free` +
  // `Symbol.dispose`. Tests don't exercise the wasm lifetime
  // surface, so we cast through `unknown` to satisfy TS's
  // structural-overlap check while keeping the test readable.
  return {
    hasLeader: args.hasLeader,
    currentLeader: leader,
    lastHeartbeatSlot: args.lastHb,
    takeoverThresholdSlots: args.threshold,
    schemaVersion: 1,
    bump: 0,
  } as unknown as KeeperLeaderLockView;
}

describe("computeLeaderLockGridRows", () => {
  it("returns empty array for undefined view", () => {
    const rows = computeLeaderLockGridRows(undefined, 100n, () => {
      throw new Error("decoder must not be called");
    });
    expect(rows).toEqual([]);
  });

  it("returns one row per market in alphabetic symbol order", () => {
    const view: MultiMarketView = {
      entries: new Map([
        ["SOL-USD", { symbol: "SOL-USD", marketPdaHex: "00", lockPdaHex: "01" }],
        ["BTC-USD", { symbol: "BTC-USD", marketPdaHex: "02", lockPdaHex: "03" }],
        ["ETH-USD", { symbol: "ETH-USD", marketPdaHex: "04", lockPdaHex: "05" }],
      ]),
    };
    const rows = computeLeaderLockGridRows(view, 100n, () => {
      throw new Error("uninitialised — no lockBytes");
    });
    expect(rows.length).toBe(3);
    expect(rows.map((r) => r.symbol)).toEqual(["BTC-USD", "ETH-USD", "SOL-USD"]);
    expect(rows.every((r) => r.state.kind === "uninitialised")).toBe(true);
  });

  it("reflects fresh / stale / unowned state per row", () => {
    const view: MultiMarketView = {
      entries: new Map([
        [
          "FRESH",
          {
            symbol: "FRESH",
            marketPdaHex: "00",
            lockPdaHex: "01",
            lockBytes: new Uint8Array([1]),
          },
        ],
        [
          "STALE",
          {
            symbol: "STALE",
            marketPdaHex: "00",
            lockPdaHex: "01",
            lockBytes: new Uint8Array([2]),
          },
        ],
        [
          "UNOWNED",
          {
            symbol: "UNOWNED",
            marketPdaHex: "00",
            lockPdaHex: "01",
            lockBytes: new Uint8Array([3]),
          },
        ],
      ]),
    };
    const decode = (bytes: Uint8Array): KeeperLeaderLockView => {
      switch (bytes[0]) {
        case 1:
          return lockView({
            hasLeader: true,
            leaderByte: 0xaa,
            lastHb: 95n,
            threshold: 50n,
          });
        case 2:
          return lockView({
            hasLeader: true,
            leaderByte: 0xbb,
            lastHb: 30n,
            threshold: 50n,
          });
        case 3:
          return lockView({
            hasLeader: false,
            leaderByte: 0,
            lastHb: 0n,
            threshold: 50n,
          });
        default:
          throw new Error("unexpected");
      }
    };
    const rows = computeLeaderLockGridRows(view, 100n, decode);
    const byName = Object.fromEntries(rows.map((r) => [r.symbol, r]));
    expect(byName["FRESH"]!.state.kind).toBe("fresh");
    expect(byName["STALE"]!.state.kind).toBe("stale");
    expect(byName["UNOWNED"]!.state.kind).toBe("unowned");
  });

  it("falls back to uninitialised on decode error without throwing", () => {
    const view: MultiMarketView = {
      entries: new Map([
        [
          "X",
          {
            symbol: "X",
            marketPdaHex: "00",
            lockPdaHex: "01",
            lockBytes: new Uint8Array([1]),
          },
        ],
      ]),
    };
    const rows = computeLeaderLockGridRows(view, 100n, () => {
      throw new Error("schema drift");
    });
    expect(rows[0]!.state.kind).toBe("uninitialised");
  });

  it("flags expected_leader mismatch only on fresh/stale", () => {
    const view: MultiMarketView = {
      entries: new Map([
        [
          "MISMATCH",
          {
            symbol: "MISMATCH",
            marketPdaHex: "00",
            lockPdaHex: "01",
            lockBytes: new Uint8Array([1]),
          },
        ],
        [
          "MATCH",
          {
            symbol: "MATCH",
            marketPdaHex: "00",
            lockPdaHex: "01",
            lockBytes: new Uint8Array([2]),
          },
        ],
        [
          "UNOWNED",
          {
            symbol: "UNOWNED",
            marketPdaHex: "00",
            lockPdaHex: "01",
            lockBytes: new Uint8Array([3]),
          },
        ],
      ]),
    };
    const decode = (bytes: Uint8Array): KeeperLeaderLockView => {
      switch (bytes[0]) {
        case 1:
          return lockView({
            hasLeader: true,
            leaderByte: 0xaa,
            lastHb: 95n,
            threshold: 50n,
          });
        case 2:
          return lockView({
            hasLeader: true,
            leaderByte: 0xbb,
            lastHb: 95n,
            threshold: 50n,
          });
        case 3:
          return lockView({
            hasLeader: false,
            leaderByte: 0,
            lastHb: 0n,
            threshold: 50n,
          });
        default:
          throw new Error("unexpected");
      }
    };
    const expected = new Map<string, string>([
      ["MISMATCH", "bb" + "00".repeat(31)], // expecting BB but holder is AA
      ["MATCH", "bb" + "00".repeat(31)], // expecting BB and holder is BB
      ["UNOWNED", "cc" + "00".repeat(31)], // unowned doesn't get flagged
    ]);
    const rows = computeLeaderLockGridRows(view, 100n, decode, expected);
    const byName = Object.fromEntries(rows.map((r) => [r.symbol, r]));
    expect(byName["MISMATCH"]!.expectedMismatch).toBe(true);
    expect(byName["MATCH"]!.expectedMismatch).toBe(false);
    // Unowned: state.kind doesn't carry holder, so we don't flag
    // (operator already gets the louder "unowned" signal).
    expect(byName["UNOWNED"]!.expectedMismatch).toBeUndefined();
  });
});
