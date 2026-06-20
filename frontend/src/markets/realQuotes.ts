// Real spot quotes for crypto bases (Binance public 24h ticker).
//
// The TradingView chart shows the real underlying market, so the marquee
// price / 24h change / 24h volume must agree with it instead of the
// synthetic walk. Binance's public REST endpoint is CORS-enabled and needs
// no key. Non-crypto assets (indices / commodities / fx / equities) have no
// equally-easy keyless feed, so they keep the synthetic ticker — their chart
// is still real via TradingView.

import { useEffect, useRef, useState } from "react";

export interface RealQuote {
  price: number;
  change24hPct: number;
  volume24hUsd: number;
}

// catalog base -> Binance spot symbol.
const BINANCE_SYMBOL: Record<string, string> = {
  BTC: "BTCUSDT",
  ETH: "ETHUSDT",
  SOL: "SOLUSDT",
  ZEC: "ZECUSDT",
  BNB: "BNBUSDT",
  XRP: "XRPUSDT",
  DOGE: "DOGEUSDT",
  AVAX: "AVAXUSDT",
  LINK: "LINKUSDT",
  SUI: "SUIUSDT",
  APT: "APTUSDT",
  LTC: "LTCUSDT",
  ARB: "ARBUSDT",
  OP: "OPUSDT",
};

const SYMBOL_TO_BASE: Record<string, string> = Object.fromEntries(
  Object.entries(BINANCE_SYMBOL).map(([base, sym]) => [sym, base]),
);

interface Binance24hr {
  symbol: string;
  lastPrice: string;
  priceChangePercent: string;
  quoteVolume: string; // 24h volume denominated in quote asset (USDT ≈ USD)
}

async function fetchBinanceQuotes(signal: AbortSignal): Promise<Map<string, RealQuote>> {
  const symbols = Object.values(BINANCE_SYMBOL);
  const url = `https://api.binance.com/api/v3/ticker/24hr?symbols=${encodeURIComponent(
    JSON.stringify(symbols),
  )}`;
  const res = await fetch(url, { signal });
  if (!res.ok) throw new Error(`binance ${res.status}`);
  const body = (await res.json()) as unknown;
  if (!Array.isArray(body)) throw new Error("binance non-array response");
  const rows = body as Binance24hr[];
  const out = new Map<string, RealQuote>();
  for (const r of rows) {
    const base = SYMBOL_TO_BASE[r.symbol];
    if (!base) continue;
    const price = Number(r.lastPrice);
    const change24hPct = Number(r.priceChangePercent);
    const volume24hUsd = Number(r.quoteVolume);
    if (Number.isFinite(price) && price > 0) {
      out.set(base, { price, change24hPct, volume24hUsd });
    }
  }
  return out;
}

/**
 * Poll real crypto quotes from Binance. Returns an empty map until the first
 * successful fetch; on any error it keeps the last good snapshot so transient
 * network blips don't blank the UI. Failures are swallowed (synthetic
 * fallback remains in place).
 */
export function useRealQuotes(intervalMs = 15000): Map<string, RealQuote> {
  const [quotes, setQuotes] = useState<Map<string, RealQuote>>(() => new Map());
  const lastGood = useRef<Map<string, RealQuote>>(new Map());

  useEffect(() => {
    let cancelled = false;
    const controller = new AbortController();

    async function tick() {
      try {
        const next = await fetchBinanceQuotes(controller.signal);
        if (cancelled || next.size === 0) return;
        lastGood.current = next;
        setQuotes(next);
      } catch {
        // keep last good snapshot
      }
    }

    void tick();
    const id = setInterval(() => void tick(), intervalMs);
    return () => {
      cancelled = true;
      controller.abort();
      clearInterval(id);
    };
  }, [intervalMs]);

  return quotes;
}
