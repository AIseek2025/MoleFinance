// Wave 16 — `deriveLeaderLockState` matrix.
//
// We test the pure derivation function rather than the React tree
// directly. The visual `LeaderLockBanner` is a thin presentation
// layer over the derivation result; correctness lives in the matrix
// of `(view, currentSlot) → state` and is fully covered here.
//
// The wave-15 wasm-bound `KeeperLeaderLockView` exposes JS-friendly
// getters; rather than instantiating the wasm class in unit tests
// (which would force a `wasm-pack`-built artifact at test-collect
// time), we synthesise structurally-compatible objects that satisfy
// the surface the banner reads. Any drift between the synthesised
// shape and the wasm getters surfaces as a TS compile error.

import { describe, it, expect } from "vitest";

import {
  deriveLeaderLockState,
  shortenHex,
} from "./LeaderLockBanner";
import type { KeeperLeaderLockView } from "../tx/wasmBuilder";

function fakeView(opts: {
  hasLeader: boolean;
  currentLeader?: Uint8Array;
  lastHeartbeatSlot: bigint;
  takeoverThresholdSlots: bigint;
}): KeeperLeaderLockView {
  const view = {
    hasLeader: opts.hasLeader,
    currentLeader: opts.currentLeader ?? new Uint8Array(32),
    lastHeartbeatSlot: opts.lastHeartbeatSlot,
    takeoverThresholdSlots: opts.takeoverThresholdSlots,
  } satisfies Pick<
    KeeperLeaderLockView,
    "hasLeader" | "currentLeader" | "lastHeartbeatSlot" | "takeoverThresholdSlots"
  >;
  return view as unknown as KeeperLeaderLockView;
}

describe("deriveLeaderLockState — matrix", () => {
  it("returns `uninitialised` when the PDA hasn't been fetched", () => {
    expect(deriveLeaderLockState(null, 100n)).toEqual({ kind: "uninitialised" });
  });

  it("returns `unowned` when the lock exists but has no holder", () => {
    const view = fakeView({
      hasLeader: false,
      lastHeartbeatSlot: 0n,
      takeoverThresholdSlots: 75n,
    });
    expect(deriveLeaderLockState(view, 100n)).toEqual({ kind: "unowned" });
  });

  it("returns `fresh` while the elapsed slot count is within threshold", () => {
    const holder = new Uint8Array(32);
    holder[0] = 0xa1;
    holder[31] = 0xee;
    const view = fakeView({
      hasLeader: true,
      currentLeader: holder,
      lastHeartbeatSlot: 100n,
      takeoverThresholdSlots: 75n,
    });
    const state = deriveLeaderLockState(view, 130n);
    expect(state.kind).toBe("fresh");
    if (state.kind === "fresh") {
      expect(state.slotsUntilStale).toBe(45n);
      expect(state.holderHex.startsWith("a1")).toBe(true);
      expect(state.holderHex.endsWith("ee")).toBe(true);
      expect(state.holderHex).toHaveLength(64);
    }
  });

  it("treats the boundary slot (elapsed === threshold) as fresh", () => {
    const view = fakeView({
      hasLeader: true,
      lastHeartbeatSlot: 100n,
      takeoverThresholdSlots: 75n,
    });
    const state = deriveLeaderLockState(view, 175n);
    expect(state.kind).toBe("fresh");
    if (state.kind === "fresh") {
      expect(state.slotsUntilStale).toBe(0n);
    }
  });

  it("returns `stale` when elapsed exceeds the threshold by 1+ slots", () => {
    const holder = new Uint8Array(32);
    holder[0] = 0xb0;
    const view = fakeView({
      hasLeader: true,
      currentLeader: holder,
      lastHeartbeatSlot: 100n,
      takeoverThresholdSlots: 75n,
    });
    const state = deriveLeaderLockState(view, 200n);
    expect(state.kind).toBe("stale");
    if (state.kind === "stale") {
      expect(state.slotsOverdue).toBe(25n);
    }
  });

  it("clamps elapsed to zero when currentSlot < lastHeartbeatSlot (clock skew)", () => {
    const view = fakeView({
      hasLeader: true,
      lastHeartbeatSlot: 1_000n,
      takeoverThresholdSlots: 75n,
    });
    const state = deriveLeaderLockState(view, 500n);
    // Negative-elapsed must be reported as fresh with full
    // takeover_threshold_slots remaining (we never tell ops the
    // lock is "stale by negative slots" — that's nonsense).
    expect(state.kind).toBe("fresh");
    if (state.kind === "fresh") {
      expect(state.slotsUntilStale).toBe(75n);
    }
  });
});

describe("shortenHex", () => {
  it("returns input untouched when ≤ 12 chars", () => {
    expect(shortenHex("abc")).toBe("abc");
    expect(shortenHex("abcdef012345")).toBe("abcdef012345");
  });

  it("truncates to head…tail for full 64-char hex pubkey", () => {
    const hex = "a".repeat(32) + "b".repeat(32);
    const short = shortenHex(hex);
    expect(short).toBe("aaaaaa…bbbbbb");
  });
});
