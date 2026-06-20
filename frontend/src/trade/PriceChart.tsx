import { useEffect, useRef } from "react";
import type { JSX } from "react";
import {
  createChart,
  ColorType,
  CrosshairMode,
  type CandlestickData,
  type HistogramData,
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
  volume: number;
}

const UP = "#36d399";
const DOWN = "#ff5d6d";
const UP_VOL = "rgba(54, 211, 153, 0.45)";
const DOWN_VOL = "rgba(255, 93, 109, 0.45)";

/**
 * Devnet exposes no OHLC history, so we synthesize a believable backfill
 * around the first observed price (a seeded random walk) and then fold
 * every live price tick into the trailing candle. Volume is synthesized
 * alongside each candle (proportional to the candle's range + noise) so
 * the chart carries a VOL histogram like a real venue.
 */
function seedHistory(price: number, intervalSec: number, count: number): Candle[] {
  const now = Math.floor(Date.now() / 1000);
  const startBucket = Math.floor(now / intervalSec) * intervalSec;
  const out: Candle[] = [];
  let p = price;
  // Larger timeframes accumulate more variance per candle (~sqrt of time),
  // so scale the synthetic volatility accordingly — a 1Y candle should
  // swing far more than a 1m candle.
  const vol = Math.min(9, Math.max(1, Math.sqrt(intervalSec / 60)));
  const volBase = 800 * Math.sqrt(intervalSec / 60); // arbitrary turnover units
  const prices: number[] = [p];
  for (let i = 0; i < count; i += 1) {
    const drift = (Math.sin(i / 6) + Math.cos(i / 11)) * 0.0011 * vol;
    const noise = (Math.random() - 0.5) * 0.004 * vol;
    p = p / (1 + drift + noise);
    prices.push(p);
  }
  prices.reverse();
  for (let i = 0; i < count; i += 1) {
    const open = prices[i]!;
    const close = prices[i + 1]!;
    const wick =
      Math.abs(open - close) + open * (Math.random() * 0.0025 + 0.0006) * vol;
    const swing = Math.abs(open - close) / Math.max(open, 1e-9);
    const volume = volBase * (0.4 + Math.random() * 0.9 + swing * 40);
    out.push({
      time: startBucket - (count - i) * intervalSec,
      open,
      high: Math.max(open, close) + wick * Math.random(),
      low: Math.min(open, close) - wick * Math.random(),
      close,
      volume,
    });
  }
  return out;
}

export function PriceChart({ priceUsd, intervalSec, symbol }: Props): JSX.Element {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const seriesRef = useRef<ISeriesApi<"Candlestick"> | null>(null);
  const volRef = useRef<ISeriesApi<"Histogram"> | null>(null);
  const candleRef = useRef<Candle | null>(null);
  const seededRef = useRef(false);

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
      rightPriceScale: { borderColor: "#1e2733", scaleMargins: { top: 0.08, bottom: 0.26 } },
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
    // Volume histogram pinned to the bottom ~20% via an overlay price scale.
    const volume = chart.addHistogramSeries({
      priceFormat: { type: "volume" },
      priceScaleId: "vol",
    });
    chart.priceScale("vol").applyOptions({
      scaleMargins: { top: 0.82, bottom: 0 },
    });
    chartRef.current = chart;
    seriesRef.current = series;
    volRef.current = volume;
    seededRef.current = false;
    candleRef.current = null;
    return () => {
      chart.remove();
      chartRef.current = null;
      seriesRef.current = null;
      volRef.current = null;
    };
  }, [intervalSec]);

  // Re-seed history when the underlying symbol changes so the chart
  // jumps to the newly selected market's price band immediately.
  useEffect(() => {
    seededRef.current = false;
    candleRef.current = null;
  }, [symbol]);

  // Fold live prices into candles + volume.
  useEffect(() => {
    const series = seriesRef.current;
    const volSeries = volRef.current;
    if (!series || !volSeries || priceUsd == null || !Number.isFinite(priceUsd)) return;

    if (!seededRef.current) {
      const history = seedHistory(priceUsd, intervalSec, 90);
      series.setData(
        history.map((c) => ({
          time: c.time as UTCTimestamp,
          open: c.open,
          high: c.high,
          low: c.low,
          close: c.close,
        })) as CandlestickData[],
      );
      volSeries.setData(
        history.map((c) => ({
          time: c.time as UTCTimestamp,
          value: c.volume,
          color: c.close >= c.open ? UP_VOL : DOWN_VOL,
        })) as HistogramData[],
      );
      const last = history[history.length - 1]!;
      candleRef.current = { ...last };
      seededRef.current = true;
      chartRef.current?.timeScale().fitContent();
      return;
    }

    const now = Math.floor(Date.now() / 1000);
    const bucket = Math.floor(now / intervalSec) * intervalSec;
    const cur = candleRef.current;
    const volBase = 800 * Math.sqrt(intervalSec / 60);
    if (!cur || bucket > cur.time) {
      const next: Candle = {
        time: bucket,
        open: cur ? cur.close : priceUsd,
        high: priceUsd,
        low: priceUsd,
        close: priceUsd,
        volume: volBase * (0.4 + Math.random() * 0.6),
      };
      candleRef.current = next;
      series.update({
        time: next.time as UTCTimestamp,
        open: next.open,
        high: next.high,
        low: next.low,
        close: next.close,
      });
      volSeries.update({
        time: next.time as UTCTimestamp,
        value: next.volume,
        color: next.close >= next.open ? UP_VOL : DOWN_VOL,
      });
    } else {
      cur.close = priceUsd;
      cur.high = Math.max(cur.high, priceUsd);
      cur.low = Math.min(cur.low, priceUsd);
      cur.volume += volBase * 0.05;
      series.update({
        time: cur.time as UTCTimestamp,
        open: cur.open,
        high: cur.high,
        low: cur.low,
        close: cur.close,
      });
      volSeries.update({
        time: cur.time as UTCTimestamp,
        value: cur.volume,
        color: cur.close >= cur.open ? UP_VOL : DOWN_VOL,
      });
    }
  }, [priceUsd, intervalSec]);

  return (
    <div className="tv-chart-wrap">
      <div className="tv-chart-watermark">{symbol}</div>
      <div ref={containerRef} className="tv-chart" />
    </div>
  );
}
