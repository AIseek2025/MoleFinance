import { useEffect, useRef } from "react";
import type { JSX } from "react";

interface Props {
  /** TradingView ticker, e.g. "BINANCE:BTCUSDT". */
  tvSymbol: string;
  /** Candle bucket size in seconds (timeframe selector). */
  intervalSec: number;
  /** UI language code from i18next. */
  lang?: string;
  /** Watermark / display label. */
  label?: string;
}

// catalog base -> TradingView ticker. Crypto routes to Binance spot; tradfi /
// commodities / fx route to the most liquid public TradingView feed. Bases not
// present here have no real public market data and fall back to the on-chain
// PriceChart in TradeView.
const TV_SYMBOL: Record<string, string> = {
  // crypto (Binance USDT pairs)
  BTC: "BINANCE:BTCUSDT",
  ETH: "BINANCE:ETHUSDT",
  SOL: "BINANCE:SOLUSDT",
  HYPE: "MEXC:HYPEUSDT",
  ZEC: "BINANCE:ZECUSDT",
  BNB: "BINANCE:BNBUSDT",
  XRP: "BINANCE:XRPUSDT",
  DOGE: "BINANCE:DOGEUSDT",
  AVAX: "BINANCE:AVAXUSDT",
  LINK: "BINANCE:LINKUSDT",
  SUI: "BINANCE:SUIUSDT",
  APT: "BINANCE:APTUSDT",
  LTC: "BINANCE:LTCUSDT",
  ARB: "BINANCE:ARBUSDT",
  OP: "BINANCE:OPUSDT",
  // indices
  SP500: "SP:SPX",
  NAS100: "NASDAQ:NDX",
  // US equities
  NVDA: "NASDAQ:NVDA",
  TSLA: "NASDAQ:TSLA",
  AAPL: "NASDAQ:AAPL",
  SKHYNIX: "KRX:000660",
  // commodities
  WTI: "TVC:USOIL",
  GOLD: "OANDA:XAUUSD",
  SILVER: "OANDA:XAGUSD",
  // fx
  EURUSD: "FX:EURUSD",
  GBPUSD: "FX:GBPUSD",
  USDJPY: "FX:USDJPY",
  AUDUSD: "FX:AUDUSD",
  USDCAD: "FX:USDCAD",
};

/** Resolve a catalog base to a TradingView ticker, or null if unavailable. */
export function tradingViewSymbol(base: string): string | null {
  return TV_SYMBOL[base] ?? null;
}

function tvInterval(intervalSec: number): string {
  // TradingView interval codes: minutes as numbers, then D / W / M.
  switch (intervalSec) {
    case 60:
      return "1";
    case 300:
      return "5";
    case 900:
      return "15";
    case 1800:
      return "30";
    case 3600:
      return "60";
    case 14400:
      return "240";
    case 86400:
      return "D";
    case 604800:
      return "W";
    case 2592000:
      return "M";
    case 31536000:
      return "M"; // 1Y view uses monthly candles
    default:
      return "60";
  }
}

function tvLocale(lang: string | undefined): string {
  switch (lang) {
    case "zh-Hans":
      return "zh_CN";
    case "zh-Hant":
      return "zh_TW";
    case "ja":
      return "ja";
    case "ko":
      return "kr";
    case "vi":
      return "vi_VN";
    default:
      return "en";
  }
}

export function TradingViewChart({ tvSymbol, intervalSec, lang, label }: Props): JSX.Element {
  const containerRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const host = containerRef.current;
    if (!host) return;
    host.innerHTML = "";

    const widget = document.createElement("div");
    widget.className = "tradingview-widget-container__widget";
    widget.style.height = "100%";
    widget.style.width = "100%";
    host.appendChild(widget);

    const script = document.createElement("script");
    script.type = "text/javascript";
    script.async = true;
    script.src =
      "https://s3.tradingview.com/external-embedding/embed-widget-advanced-chart.js";
    script.innerHTML = JSON.stringify({
      autosize: true,
      symbol: tvSymbol,
      interval: tvInterval(intervalSec),
      timezone: "Etc/UTC",
      theme: "dark",
      style: "1", // candlesticks
      locale: tvLocale(lang),
      backgroundColor: "rgba(11, 15, 20, 1)",
      gridColor: "rgba(42, 51, 64, 0.4)",
      hide_side_toolbar: false,
      allow_symbol_change: false,
      save_image: false,
      withdateranges: true,
      details: false,
      hide_volume: false,
      studies: ["STD;Volume"],
      support_host: "https://www.tradingview.com",
    });
    host.appendChild(script);

    return () => {
      host.innerHTML = "";
    };
  }, [tvSymbol, intervalSec, lang]);

  return (
    <div className="tv-chart-wrap">
      {label ? <div className="tv-chart-watermark">{label}</div> : null}
      <div
        ref={containerRef}
        className="tradingview-widget-container tv-chart"
        style={{ height: "100%", width: "100%" }}
      />
    </div>
  );
}
