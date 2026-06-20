// Synthetic ticker engine.
//
// Devnet only provisions a handful of real on-chain markets, but the
// product needs to *browse* and chart the full catalog. For any symbol
// without a live on-chain price we synthesize a believable ticker — a
// smooth, deterministic-per-symbol walk around its reference price plus
// a plausible 24h change and 24h volume. When a live on-chain price IS
// available for an underlying, callers override the synthetic price with
// the real one (see TradeView).

import { useEffect, useState } from "react";

import { CATALOG, type CatalogSymbol } from "./catalog";
import type { RealQuote } from "./realQuotes";

export interface Ticker {
  base: string;
  /** Current USD price. */
  price: number;
  /** 24h change as a percentage (e.g. +1.77). */
  change24hPct: number;
  /** Notional 24h volume in USD. */
  volume24hUsd: number;
  /** True when the price came from a live on-chain feed (not synthesized). */
  live: boolean;
}

/** Stable per-symbol seed in [0, 1) derived from the base ticker. */
function seedOf(base: string): number {
  let h = 2166136261;
  for (let i = 0; i < base.length; i += 1) {
    h ^= base.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return ((h >>> 0) % 100000) / 100000;
}

/** Per-class baseline 24h notional volume (USD), scaled by a per-symbol seed. */
function baseVolume(sym: CatalogSymbol, seed: number): number {
  const baseline =
    sym.assetClass === "crypto"
      ? 1_200_000_000
      : sym.assetClass === "equity"
        ? 250_000_000
        : 600_000_000; // fx
  return baseline * (0.25 + seed * 1.75);
}

/**
 * Compute a synthetic ticker for `sym` at time `nowMs`. Pure function so
 * it can drive both the browser table and the chart deterministically.
 */
export function syntheticTicker(sym: CatalogSymbol, nowMs: number): Ticker {
  const seed = seedOf(sym.base);
  const phase = seed * Math.PI * 2;
  const t = nowMs / 1000;
  // Slow daily swing + medium ripple + fast jitter — amplitudes are a
  // fraction of price so the walk stays near the reference.
  const slow = Math.sin(t / 3600 + phase) * 0.02;
  const mid = Math.sin(t / 240 + phase * 1.7) * 0.006;
  const fast = Math.sin(t / 11 + phase * 3.1) * 0.0015;
  const price = sym.basePriceUsd * (1 + slow + mid + fast);
  // 24h change anchored to the slow component so it reads consistently.
  const change24hPct = (slow + mid) * 100 * 1.5;
  const volume24hUsd = baseVolume(sym, seed) * (1 + Math.sin(t / 600 + phase) * 0.18);
  return { base: sym.base, price, change24hPct, volume24hUsd, live: false };
}

/**
 * Live map of synthetic tickers for the whole catalog, refreshed on a
 * timer so the browser table animates. `liveOverrides` maps an
 * underlying base → real on-chain price (USD); when present we swap the
 * synthetic price in and recompute the change against the reference.
 */
export function useTickers(
  liveOverrides?: Map<string, number>,
  realQuotes?: Map<string, RealQuote>,
  intervalMs = 2000,
): Map<string, Ticker> {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);

  const out = new Map<string, Ticker>();
  for (const sym of CATALOG) {
    const tk = syntheticTicker(sym, now);
    const real = realQuotes?.get(sym.base);
    const live = liveOverrides?.get(sym.base);
    if (real) {
      // Real market quote (matches the TradingView chart) wins.
      out.set(sym.base, {
        ...tk,
        price: real.price,
        change24hPct: real.change24hPct,
        volume24hUsd: real.volume24hUsd,
        live: true,
      });
    } else if (live != null && Number.isFinite(live) && live > 0) {
      const change = ((live - sym.basePriceUsd) / sym.basePriceUsd) * 100;
      out.set(sym.base, { ...tk, price: live, change24hPct: change, live: true });
    } else {
      out.set(sym.base, tk);
    }
  }
  return out;
}
