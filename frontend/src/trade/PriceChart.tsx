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

/**
 * Devnet exposes no OHLC history, so we synthesize a believable backfill
 * around the first observed price (a seeded random walk) and then fold
 * every live price tick into the trailing candle. This keeps the chart
 * visually alive while the keeper pushes one fresh price every ~6s.
 */
function seedHistory(price: number, intervalSec: number, count: number): Candle[] {
  const now = Math.floor(Date.now() / 1000);
  const startBucket = Math.floor(now / intervalSec) * intervalSec;
  const out: Candle[] = [];
  let p = price;
  // Walk backwards from `price` so the last synthesized close lands near it.
  const prices: number[] = [p];
  for (let i = 0; i < count; i += 1) {
    const drift = (Math.sin(i / 6) + Math.cos(i / 11)) * 0.0011;
    const noise = (Math.random() - 0.5) * 0.004;
    p = p / (1 + drift + noise);
    prices.push(p);
  }
  prices.reverse();
  for (let i = 0; i < count; i += 1) {
    const open = prices[i]!;
    const close = prices[i + 1]!;
    const wick = Math.abs(open - close) + open * (Math.random() * 0.0025 + 0.0006);
    out.push({
      time: startBucket - (count - i) * intervalSec,
      open,
      high: Math.max(open, close) + wick * Math.random(),
      low: Math.min(open, close) - wick * Math.random(),
      close,
    });
  }
  return out;
}

export function PriceChart({ priceUsd, intervalSec, symbol }: Props): JSX.Element {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const seriesRef = useRef<ISeriesApi<"Candlestick"> | null>(null);
  const candleRef = useRef<Candle | null>(null);
  const seededRef = useRef(false);

  // Build the chart once.
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
      rightPriceScale: { borderColor: "#1e2733" },
      timeScale: {
        borderColor: "#1e2733",
        timeVisible: true,
        secondsVisible: intervalSec < 60,
      },
      autoSize: true,
    });
    const series = chart.addCandlestickSeries({
      upColor: "#36d399",
      downColor: "#ff5d6d",
      borderUpColor: "#36d399",
      borderDownColor: "#ff5d6d",
      wickUpColor: "#36d399",
      wickDownColor: "#ff5d6d",
    });
    chartRef.current = chart;
    seriesRef.current = series;
    // Reset seed/candle state so the new interval rebuilds cleanly.
    seededRef.current = false;
    candleRef.current = null;
    return () => {
      chart.remove();
      chartRef.current = null;
      seriesRef.current = null;
    };
  }, [intervalSec]);

  // Fold live prices into candles.
  useEffect(() => {
    const series = seriesRef.current;
    if (!series || priceUsd == null || !Number.isFinite(priceUsd)) return;

    if (!seededRef.current) {
      const history = seedHistory(priceUsd, intervalSec, 80);
      series.setData(history as CandlestickData[]);
      const last = history[history.length - 1]!;
      candleRef.current = { ...last };
      seededRef.current = true;
      chartRef.current?.timeScale().fitContent();
      return;
    }

    const now = Math.floor(Date.now() / 1000);
    const bucket = Math.floor(now / intervalSec) * intervalSec;
    const cur = candleRef.current;
    if (!cur || bucket > cur.time) {
      const next: Candle = {
        time: bucket,
        open: cur ? cur.close : priceUsd,
        high: priceUsd,
        low: priceUsd,
        close: priceUsd,
      };
      candleRef.current = next;
      series.update({ ...next, time: next.time as UTCTimestamp });
    } else {
      cur.close = priceUsd;
      cur.high = Math.max(cur.high, priceUsd);
      cur.low = Math.min(cur.low, priceUsd);
      series.update({ ...cur, time: cur.time as UTCTimestamp });
    }
  }, [priceUsd, intervalSec]);

  return (
    <div className="tv-chart-wrap">
      <div className="tv-chart-watermark">{symbol}</div>
      <div ref={containerRef} className="tv-chart" />
    </div>
  );
}
