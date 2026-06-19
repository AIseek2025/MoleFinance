import { describe, expect, it } from "vitest";

import {
  DRIFT_CHECK_NAME,
  driftCheckFor,
  firingChecks,
  parseProberSnapshot,
  snapshotHasFiring,
} from "./proberSnapshot";

// A trimmed-but-faithful sample of the `render_json_multi` wire format
// the `ops-toolkit prober` daemon writes each cycle.
function sample(): string {
  return JSON.stringify({
    worst_exit_code: 4,
    markets: {
      "SOL-USD": {
        timestamp_unix: 1781845649,
        overall_status: "CRITICAL",
        highest_firing_severity: "P1",
        counts: { pass: 20, warn: 1, critical: 1 },
        checks: [
          {
            name: "global_paused",
            status: "PASS",
            severity: "P0",
            message: "ok",
            measurements: { paused_globally: 0 },
          },
          {
            name: "frozen_new_position",
            status: "WARN",
            severity: "P2",
            message: "frozen",
            measurements: { frozen_new_position: 1 },
          },
          {
            name: DRIFT_CHECK_NAME,
            status: "CRITICAL",
            severity: "P1",
            message: "on-chain notional 110 vs reported 100 (drift 0.1000)",
            measurements: {
              drift_enabled: 1,
              reported_notional: 100,
              onchain_notional: 110,
              drift_ratio: 0.1,
            },
          },
        ],
      },
      "BTC-USD": {
        timestamp_unix: 1781845649,
        overall_status: "PASS",
        highest_firing_severity: "NONE",
        counts: { pass: 22, warn: 0, critical: 0 },
        checks: [
          {
            name: DRIFT_CHECK_NAME,
            status: "PASS",
            severity: "P1",
            message: "open-interest probe not run this cycle — drift check skipped",
            measurements: {
              drift_enabled: 0,
              reported_notional: 100,
              onchain_notional: 0,
            },
          },
        ],
      },
    },
  });
}

describe("parseProberSnapshot", () => {
  it("decodes worst exit code + per-market reports", () => {
    const snap = parseProberSnapshot(sample());
    expect(snap.worstExitCode).toBe(4);
    expect(snap.markets.size).toBe(2);
    const sol = snap.markets.get("SOL-USD");
    expect(sol?.overallStatus).toBe("CRITICAL");
    expect(sol?.counts).toEqual({ pass: 20, warn: 1, critical: 1 });
    expect(sol?.checks).toHaveLength(3);
  });

  it("extracts the drift check + drift ratio measurement", () => {
    const snap = parseProberSnapshot(sample());
    const sol = snap.markets.get("SOL-USD")!;
    const drift = driftCheckFor(sol);
    expect(drift?.status).toBe("CRITICAL");
    expect(drift?.measurements.drift_enabled).toBe(1);
    expect(drift?.measurements.drift_ratio).toBeCloseTo(0.1);
  });

  it("treats a skipped drift probe as drift_enabled 0 with no ratio", () => {
    const snap = parseProberSnapshot(sample());
    const btc = snap.markets.get("BTC-USD")!;
    const drift = driftCheckFor(btc);
    expect(drift?.measurements.drift_enabled).toBe(0);
    expect(drift?.measurements.drift_ratio).toBeUndefined();
  });

  it("orders firing checks worst-first and ignores PASS", () => {
    const snap = parseProberSnapshot(sample());
    const sol = snap.markets.get("SOL-USD")!;
    const firing = firingChecks(sol);
    expect(firing.map((c) => c.status)).toEqual(["CRITICAL", "WARN"]);
    const btc = snap.markets.get("BTC-USD")!;
    expect(firingChecks(btc)).toHaveLength(0);
  });

  it("flags a snapshot with any non-PASS market", () => {
    const snap = parseProberSnapshot(sample());
    expect(snapshotHasFiring(snap)).toBe(true);
  });

  it("throws on a malformed status", () => {
    const bad = JSON.stringify({
      worst_exit_code: 0,
      markets: {
        "X-USD": {
          overall_status: "BOGUS",
          counts: { pass: 1, warn: 0, critical: 0 },
          checks: [],
        },
      },
    });
    expect(() => parseProberSnapshot(bad)).toThrow(/invalid status/);
  });

  it("throws when markets is missing", () => {
    expect(() => parseProberSnapshot('{"worst_exit_code":0}')).toThrow(
      /markets expected object/,
    );
  });
});
