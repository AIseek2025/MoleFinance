/**
 * Wave 18 — frontend market-registry parser tests.
 *
 * @vitest-environment node
 */
import { describe, expect, it } from "vitest";

import { parseMarketsConfig } from "./marketRegistry";

const PROGRAM = "11111111111111111111111111111112";
const MARKET_A = "Sysvar1nstructions1111111111111111111111111";
const MARKET_B = "SysvarC1ock11111111111111111111111111111111";

describe("parseMarketsConfig", () => {
  it("returns null on undefined / null / empty", () => {
    expect(parseMarketsConfig(undefined)).toBeNull();
    expect(parseMarketsConfig(null)).toBeNull();
    expect(parseMarketsConfig("")).toBeNull();
    expect(parseMarketsConfig("   ")).toBeNull();
  });

  it("parses a valid two-market JSON array", () => {
    const json = JSON.stringify([
      { symbol: "SOL-USD", programId: PROGRAM, marketPda: MARKET_A },
      {
        symbol: "BTC-USD",
        programId: PROGRAM,
        marketPda: MARKET_B,
        expectedLeader: "ab".repeat(32),
      },
    ]);
    const out = parseMarketsConfig(json);
    expect(out).not.toBeNull();
    expect(out!.adapter.length).toBe(2);
    expect(out!.adapter[0]!.symbol).toBe("SOL-USD");
    expect(out!.expectedLeaders.size).toBe(1);
    expect(out!.expectedLeaders.get("BTC-USD")).toBe("ab".repeat(32));
  });

  it("rejects malformed JSON", () => {
    expect(() => parseMarketsConfig("{not json")).toThrow(/invalid JSON/);
  });

  it("rejects non-array roots", () => {
    expect(() => parseMarketsConfig('{"foo":1}')).toThrow(/expected a JSON array/);
  });

  it("rejects entries missing required fields", () => {
    const json = JSON.stringify([{ symbol: "X" }]);
    expect(() => parseMarketsConfig(json)).toThrow(/missing one of/);
  });

  it("rejects duplicate symbols", () => {
    const json = JSON.stringify([
      { symbol: "X", programId: PROGRAM, marketPda: MARKET_A },
      { symbol: "X", programId: PROGRAM, marketPda: MARKET_B },
    ]);
    expect(() => parseMarketsConfig(json)).toThrow(/duplicate symbol/);
  });

  it("rejects oversized symbol (> 16 bytes)", () => {
    const json = JSON.stringify([
      {
        symbol: "THIS_IS_TOO_LONG_FOR_THE_CAP",
        programId: PROGRAM,
        marketPda: MARKET_A,
      },
    ]);
    expect(() => parseMarketsConfig(json)).toThrow(/1\.\.16 ASCII bytes/);
  });

  it("rejects malformed expectedLeader hex", () => {
    const json = JSON.stringify([
      {
        symbol: "X",
        programId: PROGRAM,
        marketPda: MARKET_A,
        expectedLeader: "not-hex",
      },
    ]);
    expect(() => parseMarketsConfig(json)).toThrow(/64 hex chars/);
  });

  it("derives lockPda when not pinned", () => {
    const json = JSON.stringify([
      { symbol: "X", programId: PROGRAM, marketPda: MARKET_A },
    ]);
    const out = parseMarketsConfig(json);
    // Derivation must produce a lockPda distinct from the marketPda.
    expect(out!.adapter[0]!.lockPda.toBase58()).not.toBe(MARKET_A);
  });

  it("uses pinned lockPda when supplied", () => {
    const json = JSON.stringify([
      {
        symbol: "X",
        programId: PROGRAM,
        marketPda: MARKET_A,
        lockPda: MARKET_B,
      },
    ]);
    const out = parseMarketsConfig(json);
    expect(out!.adapter[0]!.lockPda.toBase58()).toBe(MARKET_B);
  });
});
