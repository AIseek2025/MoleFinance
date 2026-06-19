/**
 * Wave 19 — useActiveMarket pure resolver tests.
 *
 * Covers the resolution priority:
 *   URL (`?market=`) > localStorage > first symbol.
 *
 * @vitest-environment node
 */
import { describe, expect, it } from "vitest";
import { resolveActiveMarket } from "./useActiveMarket";

describe("resolveActiveMarket", () => {
  it("returns empty string when symbols list is empty", () => {
    expect(resolveActiveMarket([], "BTC-USD", null)).toBe("");
  });

  it("prefers URL value when it is a valid configured symbol", () => {
    expect(resolveActiveMarket(["SOL-USD", "BTC-USD"], "BTC-USD", null)).toBe(
      "BTC-USD",
    );
  });

  it("ignores URL value that is not a configured symbol", () => {
    expect(resolveActiveMarket(["SOL-USD"], "ETH-USD", null)).toBe("SOL-USD");
  });

  it("falls back to localStorage value when URL is missing", () => {
    expect(resolveActiveMarket(["SOL-USD", "BTC-USD"], null, "BTC-USD")).toBe(
      "BTC-USD",
    );
  });

  it("ignores stale localStorage value not in symbols list", () => {
    expect(resolveActiveMarket(["SOL-USD"], null, "STALE")).toBe("SOL-USD");
  });

  it("URL beats localStorage", () => {
    expect(
      resolveActiveMarket(["SOL-USD", "BTC-USD"], "BTC-USD", "SOL-USD"),
    ).toBe("BTC-USD");
  });

  it("falls back to first symbol when both URL and storage absent", () => {
    expect(resolveActiveMarket(["SOL-USD", "BTC-USD"], null, null)).toBe(
      "SOL-USD",
    );
  });
});
