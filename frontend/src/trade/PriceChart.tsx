import { useEffect, useRef } from "react";
import type { JSX } from "react";
import {
  createChart,
  ColorType,
  CrosshairMode,
  type CandlestickData,
  type IChartApi,
  type ISeriesApi,
  type UTCTimestamp,
} from "lightweight-charts";

import { baseOf, findSymbol } from "../markets/catalog";

interface Props {
  /** Live mid price in USD. New values fold into the current candle. */
  priceUsd: number | null;
  /** Candle bucket size in seconds (timeframe selector). */
  intervalSec: number;
  symbol: string;
}

interface Candle {
  time: number; // unix seconds (bucket start)
  open: number;
  high: number;
  low: number;
  close: number;
}

const UP = "#36d399";
const DOWN = "#ff5d6d";
const STORAGE_PREFIX = "mole.realCandles.v1";
const MAX_CANDLES = 240;

function storageKey(symbol: string, intervalSec: number): string {
  return `${STORAGE_PREFIX}:${symbol}:${intervalSec}`;
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isPlausibleAbsolutePrice(price: number, expectedPriceUsd: number | null): boolean {
  if (!Number.isFinite(price) || price <= 0) return false;
  if (expectedPriceUsd == null || expectedPriceUsd <= 0) return true;
  return price >= expectedPriceUsd / 100 && price <= expectedPriceUsd * 100;
}

function sanitizeHistory(raw: unknown, expectedPriceUsd: number | null): Candle[] {
  if (!Array.isArray(raw)) return [];
  return raw
    .filter(
      (item): item is Candle =>
        !!item &&
        typeof item === "object" &&
        isFiniteNumber((item as Candle).time) &&
        isFiniteNumber((item as Candle).open) &&
        isFiniteNumber((item as Candle).high) &&
        isFiniteNumber((item as Candle).low) &&
        isFiniteNumber((item as Candle).close) &&
        isPlausibleAbsolutePrice((item as Candle).open, expectedPriceUsd) &&
        isPlausibleAbsolutePrice((item as Candle).high, expectedPriceUsd) &&
        isPlausibleAbsolutePrice((item as Candle).low, expectedPriceUsd) &&
        isPlausibleAbsolutePrice((item as Candle).close, expectedPriceUsd),
    )
    .sort((a, b) => a.time - b.time)
    .slice(-MAX_CANDLES);
}

function loadHistory(
  symbol: string,
  intervalSec: number,
  expectedPriceUsd: number | null,
): Candle[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = window.sessionStorage.getItem(storageKey(symbol, intervalSec));
    return raw ? sanitizeHistory(JSON.parse(raw), expectedPriceUsd) : [];
  } catch {
    return [];
  }
}

function saveHistory(symbol: string, intervalSec: number, history: Candle[]): void {
  if (typeof window === "undefined") return;
  try {
    window.sessionStorage.setItem(
      storageKey(symbol, intervalSec),
      JSON.stringify(history.slice(-MAX_CANDLES)),
    );
  } catch {
    // ignore storage write failures
  }
}

function toSeriesData(history: Candle[]): CandlestickData[] {
  return history.map((c) => ({
    time: c.time as UTCTimestamp,
    open: c.open,
    high: c.high,
    low: c.low,
    close: c.close,
  }));
}

export function PriceChart({ priceUsd, intervalSec, symbol }: Props): JSX.Element {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const seriesRef = useRef<ISeriesApi<"Candlestick"> | null>(null);
  const candleRef = useRef<Candle | null>(null);
  const historyRef = useRef<Candle[]>([]);
  const expectedPriceUsd = findSymbol(baseOf(symbol))?.basePriceUsd ?? null;

  // Build the chart once per timeframe.
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const chart = createChart(el, {
      layout: {
        background: { type: ColorType.Solid, color: "transparent" },
        textColor: "#8b97ab",
        fontFamily: "ui-monospace, SF Mono, Menlo, monospace",
      },
      grid: {
        vertLines: { color: "rgba(42, 51, 64, 0.4)" },
        horzLines: { color: "rgba(42, 51, 64, 0.4)" },
      },
      crosshair: { mode: CrosshairMode.Normal },
      rightPriceScale: { borderColor: "#1e2733", scaleMargins: { top: 0.08, bottom: 0.08 } },
      timeScale: {
        borderColor: "#1e2733",
        timeVisible: true,
        secondsVisible: intervalSec < 60,
      },
      autoSize: true,
    });
    const series = chart.addCandlestickSeries({
      upColor: UP,
      downColor: DOWN,
      borderUpColor: UP,
      borderDownColor: DOWN,
      wickUpColor: UP,
      wickDownColor: DOWN,
    });
    chartRef.current = chart;
    seriesRef.current = series;
    candleRef.current = null;
    historyRef.current = [];
    return () => {
      chart.remove();
      chartRef.current = null;
      seriesRef.current = null;
    };
  }, [intervalSec]);

  // Load previously observed real candles for this symbol/timeframe.
  useEffect(() => {
    const series = seriesRef.current;
    if (!series) return;
    const history = loadHistory(symbol, intervalSec, expectedPriceUsd);
    historyRef.current = history;
    candleRef.current = history.length > 0 ? { ...history[history.length - 1]! } : null;
    series.setData(toSeriesData(history));
    if (history.length > 0) {
      chartRef.current?.timeScale().fitContent();
    }
  }, [symbol, intervalSec, expectedPriceUsd]);

  // Build real candles from live oracle ticks only. No synthetic backfill.
  useEffect(() => {
    const series = seriesRef.current;
    if (!series || priceUsd == null || !Number.isFinite(priceUsd)) return;
    if (!isPlausibleAbsolutePrice(priceUsd, expectedPriceUsd)) return;

    const now = Math.floor(Date.now() / 1000);
    const bucket = Math.floor(now / intervalSec) * intervalSec;
    const history = historyRef.current;
    const cur = candleRef.current;

    if (!cur) {
      const first: Candle = {
        time: bucket,
        open: priceUsd,
        high: priceUsd,
        low: priceUsd,
        close: priceUsd,
      };
      historyRef.current = [first];
      candleRef.current = first;
      saveHistory(symbol, intervalSec, historyRef.current);
      series.setData(toSeriesData(historyRef.current));
      chartRef.current?.timeScale().fitContent();
      return;
    }

    if (bucket > cur.time) {
      const next: Candle = {
        time: bucket,
        open: cur.close,
        high: priceUsd,
        low: priceUsd,
        close: priceUsd,
      };
      const nextHistory = [...history, next].slice(-MAX_CANDLES);
      historyRef.current = nextHistory;
      candleRef.current = next;
      saveHistory(symbol, intervalSec, nextHistory);
      series.update({
        time: next.time as UTCTimestamp,
        open: next.open,
        high: next.high,
        low: next.low,
        close: next.close,
      });
    } else {
      cur.close = priceUsd;
      cur.high = Math.max(cur.high, priceUsd);
      cur.low = Math.min(cur.low, priceUsd);
      if (history.length > 0) {
        history[history.length - 1] = { ...cur };
        saveHistory(symbol, intervalSec, history);
      }
      series.update({
        time: cur.time as UTCTimestamp,
        open: cur.open,
        high: cur.high,
        low: cur.low,
        close: cur.close,
      });
    }
  }, [priceUsd, intervalSec, symbol, expectedPriceUsd]);

  return (
    <div className="tv-chart-wrap">
      <div className="tv-chart-watermark">{symbol}</div>
      <div ref={containerRef} className="tv-chart" />
    </div>
  );
}
