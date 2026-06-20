import { useEffect, useMemo, useState } from "react";
import type { JSX } from "react";
import { useTranslation } from "react-i18next";
import type { Direction, FeedSnapshot } from "../types";
import type { WalletAdapter, WalletStatus } from "../wallet";
import {
  formatPubkey,
  formatUsdcMicro,
  formatBigQty,
} from "../format";
import {
  buildClosePositionTx,
  buildOpenPositionTx,
  loadKeeperDecoder,
} from "../tx/wasmBuilder";
import { readLiveConfig, buildOpenTransaction } from "../tx/buildOrderTransaction";
import {
  aggregateOpenInterest,
  netCollateralImbalance,
  totalCollateral,
} from "../feed/openInterest";
import { LanguageSwitcher } from "../i18n/LanguageSwitcher";
import {
  CATALOG,
  baseOf,
  findSymbol,
  formatQuote,
  marketSymbol,
  tiersFor,
  type CatalogSymbol,
} from "../markets/catalog";
import { useTickers } from "../markets/syntheticTicker";
import { useRealQuotes } from "../markets/realQuotes";
import { PriceChart } from "./PriceChart";
import { TradingViewChart, tradingViewSymbol } from "./TradingViewChart";
import { MarketBrowser } from "./MarketBrowser";
import "./trade.css";

interface Props {
  feed: FeedSnapshot;
  wallet: WalletAdapter;
  walletStatus: WalletStatus;
  walletPubkeyHex?: string;
  onConnect: () => void;
  onDisconnect: () => void;
  symbols: string[];
  activeSymbol: string | null;
  onSymbolChange: (s: string) => void;
  onHome: () => void;
  onConsole: () => void;
}

const ENVELOPE_HALF_WIDTH_BPS = 50n;
const BPS_DENOMINATOR = 10_000n;

// Comprehensive timeframe set: minute / hour / day / week / month / year.
const TIMEFRAMES: { label: string; sec: number }[] = [
  { label: "1m", sec: 60 },
  { label: "5m", sec: 300 },
  { label: "15m", sec: 900 },
  { label: "30m", sec: 1800 },
  { label: "1h", sec: 3600 },
  { label: "4h", sec: 14400 },
  { label: "1D", sec: 86400 },
  { label: "1W", sec: 604800 },
  { label: "1M", sec: 2592000 },
  { label: "1Y", sec: 31536000 },
];

const DEFAULT_BASE = "BTC";
const PREFERRED_DEFAULT_LEVERAGE = 20;

function buildEnvelopeFromUsd(priceUsd: number, slotNum: number) {
  const pNow = BigInt(Math.max(0, Math.round(priceUsd * 1_000_000)));
  const slot = BigInt(slotNum);
  const half = (pNow * ENVELOPE_HALF_WIDTH_BPS) / BPS_DENOMINATOR;
  return {
    pNow,
    slot,
    expectedMin: pNow > half ? pNow - half : 0n,
    expectedMax: pNow + half,
  };
}

function nextPositionId(walletHex: string | undefined): bigint {
  const ts = BigInt(Math.floor(Date.now() / 1000));
  if (!walletHex) return ts;
  let folded = 0n;
  for (let i = 0; i < 8 && i < walletHex.length / 2; i += 1) {
    const byte = BigInt(parseInt(walletHex.slice(i * 2, i * 2 + 2), 16));
    folded = folded ^ (byte << BigInt(i * 8));
  }
  return (ts << 32n) | (folded & 0xffff_ffffn);
}

function compactUsd(v: number): string {
  if (v >= 1_000_000_000) return `$${(v / 1_000_000_000).toFixed(2)}B`;
  if (v >= 1_000_000) return `$${(v / 1_000_000).toFixed(1)}M`;
  if (v >= 1_000) return `$${(v / 1_000).toFixed(0)}K`;
  return `$${v.toFixed(0)}`;
}

interface HistoryRow {
  ts: number;
  kind: "open" | "close" | "error";
  text: string;
}

type BottomTab = "positions" | "history";

export function TradeView({
  feed,
  wallet,
  walletStatus,
  walletPubkeyHex,
  onConnect,
  onDisconnect,
  symbols,
  activeSymbol,
  onSymbolChange,
  onHome,
  onConsole,
}: Props): JSX.Element {
  const { t, i18n } = useTranslation();
  const market = feed.indexer.market;

  // ── catalog-driven market + leverage selection ────────────────────────
  const liveBase = baseOf(activeSymbol ?? market.symbol);
  const [selectedBase, setSelectedBase] = useState<string>(() =>
    findSymbol(liveBase) ? liveBase : DEFAULT_BASE,
  );
  const sym: CatalogSymbol = useMemo(
    () => findSymbol(selectedBase) ?? CATALOG[0]!,
    [selectedBase],
  );
  const tiers = useMemo(() => tiersFor(sym), [sym]);
  // Real TradingView K-line for any base with a public market feed; bases
  // without one fall back to the on-chain oracle-tick chart.
  const tvSym = tradingViewSymbol(selectedBase);
  const [leverage, setLeverage] = useState<number>(() =>
    tiers.includes(PREFERRED_DEFAULT_LEVERAGE)
      ? PREFERRED_DEFAULT_LEVERAGE
      : tiers[tiers.length - 1]!,
  );
  const [browserOpen, setBrowserOpen] = useState(false);

  // Clamp leverage into the selected underlying's tier ladder.
  useEffect(() => {
    if (!tiers.includes(leverage)) {
      setLeverage(
        tiers.includes(PREFERRED_DEFAULT_LEVERAGE)
          ? PREFERRED_DEFAULT_LEVERAGE
          : tiers[tiers.length - 1]!,
      );
    }
  }, [tiers, leverage]);

  // The on-chain market for the current (base, leverage) selection.
  const onchainSymbol = marketSymbol(selectedBase, leverage);
  const isLiveMarket = symbols.includes(onchainSymbol);

  // When the selected market exists on-chain, point the live feed at it so
  // positions / sub-pools / status follow the selection.
  useEffect(() => {
    if (isLiveMarket && activeSymbol !== onchainSymbol) {
      onSymbolChange(onchainSymbol);
    }
  }, [isLiveMarket, onchainSymbol, activeSymbol, onSymbolChange]);

  // Live price override: only when the live feed market shares our base.
  const livePriceUsd =
    baseOf(market.symbol) === selectedBase && market.midPriceMicro != null
      ? Number(market.midPriceMicro) / 1_000_000
      : null;
  const liveOverrides = useMemo(() => {
    const m = new Map<string, number>();
    if (livePriceUsd != null && livePriceUsd > 0) m.set(selectedBase, livePriceUsd);
    return m;
  }, [livePriceUsd, selectedBase]);
  const realQuotes = useRealQuotes();
  const tickers = useTickers(liveOverrides, realQuotes);
  const activeTicker = tickers.get(selectedBase);
  const priceUsd = activeTicker?.price ?? sym.basePriceUsd;
  const change24h = activeTicker?.change24hPct ?? 0;
  const volume24h = activeTicker?.volume24hUsd ?? 0;
  const priceIsLive = activeTicker?.live ?? false;
  const up = change24h >= 0;

  // The marquee / chart show the real underlying market price, but the
  // on-chain protocol settles against the oracle (SubPool.last_price) pushed
  // by the keeper. Order envelopes MUST be built around that on-chain price or
  // open/close will fail the ±0.5% envelope validation. Fall back to the
  // displayed price for demo (mock-wallet) markets that have no live feed.
  const settlementPriceUsd = livePriceUsd ?? priceUsd;

  // ── order form state ───────────────────────────────────────────────────
  const [side, setSide] = useState<Direction>("Long");
  const [collateral, setCollateral] = useState<number>(1000);
  const [subPoolId, setSubPoolId] = useState<number>(0);
  const [intervalSec, setIntervalSec] = useState<number>(60);
  const [bottomTab, setBottomTab] = useState<BottomTab>("positions");
  const [history, setHistory] = useState<HistoryRow[]>([]);
  const [wasmReady, setWasmReady] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    loadKeeperDecoder()
      .then(() => !cancelled && setWasmReady(true))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  const openInterest = useMemo(
    () => aggregateOpenInterest(feed.positions),
    [feed.positions],
  );
  const longUsd = Number(openInterest.longCollateral) / 1e6;
  const shortUsd = Number(openInterest.shortCollateral) / 1e6;
  const oiTotal = longUsd + shortUsd;
  const longPct = oiTotal > 0 ? (longUsd / oiTotal) * 100 : 50;
  const shortPct = 100 - longPct;
  const skew = netCollateralImbalance(openInterest);

  const marketDisabled =
    isLiveMarket &&
    (market.paused || market.pausedGlobally || market.frozenNewPosition);
  const connected = walletStatus === "connected";
  const notional = collateral * leverage;

  function pushHistory(row: HistoryRow) {
    setHistory((h) => [row, ...h].slice(0, 60));
  }

  async function submitOpen() {
    if (!connected) {
      onConnect();
      return;
    }
    if (!wasmReady || marketDisabled || busy) return;
    setBusy(true);
    try {
      const envelope = buildEnvelopeFromUsd(settlementPriceUsd, market.lastOracleSlot);
      const ownerHex = wallet.pubkey()?.hex;
      const positionId = nextPositionId(ownerHex);
      const grossAmount = BigInt(Math.max(0, Math.floor(collateral))) * 1_000_000n;
      const instructionData = buildOpenPositionTx({
        envelope,
        directionIsLong: side === "Long",
        grossAmount,
        positionId,
      });

      // Real wallets get a fully-assembled, submittable transaction
      // (9 account metas + ATA + PDAs + blockhash). The offline mock
      // path keeps using raw instruction bytes for the demo signature.
      const liveCfg = readLiveConfig();
      let payloadBytes: Uint8Array = instructionData;
      if (liveCfg && ownerHex && wallet.name !== "mock") {
        payloadBytes = await buildOpenTransaction({
          cfg: liveCfg,
          ownerHex,
          subPoolId,
          positionId,
          instructionData,
        });
      }

      const sig = await wallet.signAndSubmit({
        description: `open ${side} ${onchainSymbol} sub_pool=${subPoolId} collateral=${collateral} USDC lev=${leverage}x`,
        borshBytes: payloadBytes,
      });
      pushHistory({
        ts: Date.now(),
        kind: "open",
        text: t("trade.openLog", {
          side: side === "Long" ? t("trade.sideLong") : t("trade.sideShort"),
          amount: collateral,
          sp: subPoolId,
          sig: formatPubkey(sig),
        }) + ` · ${onchainSymbol}`,
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      pushHistory({ ts: Date.now(), kind: "error", text: t("trade.openFail", { msg }) });
    } finally {
      setBusy(false);
    }
  }

  async function submitClose(label: string, sp: number) {
    if (!connected || !wasmReady || busy) return;
    setBusy(true);
    try {
      const envelope = buildEnvelopeFromUsd(settlementPriceUsd, market.lastOracleSlot);
      const borshBytes = buildClosePositionTx({
        envelope,
        longBucketCount: 0,
        shortBucketCount: 0,
      });
      const sig = await wallet.signAndSubmit({
        description: `close ${label} sub_pool=${sp}`,
        borshBytes,
      });
      pushHistory({
        ts: Date.now(),
        kind: "close",
        text: t("trade.closeLog", { label, sp, sig: formatPubkey(sig) }),
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      pushHistory({ ts: Date.now(), kind: "error", text: t("trade.closeFail", { msg }) });
    } finally {
      setBusy(false);
    }
  }

  const statusLabel = !isLiveMarket
    ? t("trade.statusNormal")
    : market.pausedGlobally
      ? t("trade.statusGlobalPaused")
      : market.paused
        ? t("trade.statusPaused")
        : market.frozenNewPosition
          ? t("trade.statusFrozen")
          : t("trade.statusNormal");

  return (
    <div className="tv">
      <header className="tv-top">
        <button type="button" className="tv-brand" onClick={onHome}>
          <span className="tv-brand-mark" /> {t("common.appName")}
        </button>

        <div className="tv-market-pick">
          <button
            type="button"
            className="tv-market-btn"
            onClick={() => setBrowserOpen(true)}
          >
            <span className="tv-market-sym">{selectedBase}-USDC</span>
            <span className="tv-market-cap">{sym.maxLeverage}x</span>
            <span className="tv-market-caret">▾</span>
          </button>
          <span className={`tv-status-chip ${marketDisabled ? "off" : "on"}`}>
            {statusLabel}
          </span>
          {!isLiveMarket ? (
            <span className="tv-demo-chip" title={t("trade.demoMarketHint")}>
              {t("trade.demoMarket")}
            </span>
          ) : null}
        </div>

        <div className="tv-stats">
          <div className="tv-stat">
            <span className="tv-stat-label">{t("trade.price")}</span>
            <span className={`tv-stat-value tv-price ${up ? "pos" : "neg"}`}>
              ${formatQuote(priceUsd, sym)}
              {priceIsLive ? <em className="tv-live-dot" title="live oracle">●</em> : null}
            </span>
          </div>
          <div className="tv-stat">
            <span className="tv-stat-label">{t("trade.change")}</span>
            <span className={`tv-stat-value ${up ? "pos" : "neg"}`}>
              {up ? "+" : ""}
              {change24h.toFixed(2)}%
            </span>
          </div>
          <div className="tv-stat">
            <span className="tv-stat-label">{t("trade.volume24h")}</span>
            <span className="tv-stat-value">{compactUsd(volume24h)}</span>
          </div>
          <div className="tv-stat">
            <span className="tv-stat-label">{t("trade.maxLev")}</span>
            <span className="tv-stat-value">{sym.maxLeverage}x</span>
          </div>
          <div className="tv-stat">
            <span className="tv-stat-label">{t("trade.openInterest")}</span>
            <span className="tv-stat-value">${formatUsdcMicro(totalCollateral(openInterest))}</span>
          </div>
        </div>

        <div className="tv-top-actions">
          <LanguageSwitcher variant="compact" />
          <button type="button" className="tv-link" onClick={onConsole}>
            {t("common.dashboard")}
          </button>
          {connected ? (
            <button type="button" className="tv-wallet connected" onClick={onDisconnect}>
              {walletPubkeyHex ? formatPubkey(walletPubkeyHex) : t("common.connected")} ·{" "}
              {t("common.disconnect")}
            </button>
          ) : (
            <button
              type="button"
              className="tv-wallet"
              onClick={onConnect}
              disabled={walletStatus === "connecting"}
            >
              {walletStatus === "connecting" ? t("common.connecting") : t("common.connectWallet")}
            </button>
          )}
        </div>
      </header>

      <div className="tv-main">
        <div className="tv-left">
          <div className="tv-chart-card">
            <div className="tv-chart-head">
              <div className="tv-tf">
                {TIMEFRAMES.map((tf) => (
                  <button
                    key={tf.sec}
                    type="button"
                    className={intervalSec === tf.sec ? "active" : ""}
                    onClick={() => setIntervalSec(tf.sec)}
                  >
                    {tf.label}
                  </button>
                ))}
              </div>
              <span className="tv-chart-note">{t("trade.chartNote")}</span>
            </div>
            {tvSym ? (
              <TradingViewChart
                tvSymbol={tvSym}
                intervalSec={intervalSec}
                lang={i18n.language}
                label={`${selectedBase}-USDC`}
              />
            ) : (
              <PriceChart
                priceUsd={priceIsLive && Number.isFinite(priceUsd) ? priceUsd : null}
                intervalSec={intervalSec}
                symbol={onchainSymbol}
              />
            )}
          </div>

          <div className="tv-bottom">
            <div className="tv-tabs">
              <button
                type="button"
                className={bottomTab === "positions" ? "active" : ""}
                onClick={() => setBottomTab("positions")}
              >
                {t("trade.positions")} ({feed.positions.length})
              </button>
              <button
                type="button"
                className={bottomTab === "history" ? "active" : ""}
                onClick={() => setBottomTab("history")}
              >
                {t("trade.history")} ({history.length})
              </button>
            </div>

            {bottomTab === "positions" ? (
              <div className="tv-table-scroll">
                <table className="tv-table">
                  <thead>
                    <tr>
                      <th>{t("trade.colSide")}</th>
                      <th>{t("trade.colAccount")}</th>
                      <th>{t("trade.colSubPool")}</th>
                      <th>{t("trade.colQty")}</th>
                      <th>{t("trade.colMargin")}</th>
                      <th>{t("trade.colMaxLoss")}</th>
                      <th>{t("trade.colOpened")}</th>
                      <th />
                    </tr>
                  </thead>
                  <tbody>
                    {feed.positions.length === 0 ? (
                      <tr className="tv-empty-row">
                        <td colSpan={8}>{t("trade.noPositions")}</td>
                      </tr>
                    ) : (
                      feed.positions.map((p, i) => (
                        <tr key={`${p.owner.hex}-${i}`}>
                          <td className={p.direction === "Long" ? "pos" : "neg"}>
                            {p.direction === "Long" ? t("trade.sideLong") : t("trade.sideShort")}
                          </td>
                          <td className="tv-mono">{formatPubkey(p.owner.hex)}</td>
                          <td>#{p.subPoolId}</td>
                          <td className="tv-mono">{formatBigQty(p.qty)}</td>
                          <td className="tv-mono">${formatUsdcMicro(p.collateral)}</td>
                          <td className="tv-mono tv-dim">${formatUsdcMicro(p.collateral)}</td>
                          <td className="tv-dim">
                            {new Date(p.openedAt * 1000).toLocaleTimeString()}
                          </td>
                          <td>
                            <button
                              type="button"
                              className="tv-close-btn"
                              disabled={!wasmReady || !connected || busy}
                              onClick={() =>
                                void submitClose(
                                  `${p.direction} ${p.owner.hex.slice(0, 6)}`,
                                  p.subPoolId,
                                )
                              }
                            >
                              {t("trade.close")}
                            </button>
                          </td>
                        </tr>
                      ))
                    )}
                  </tbody>
                </table>
              </div>
            ) : (
              <div className="tv-table-scroll">
                {history.length === 0 ? (
                  <div className="tv-history-empty">{t("trade.noHistory")}</div>
                ) : (
                  <ul className="tv-history">
                    {history.map((h, i) => (
                      <li key={`${h.ts}-${i}`} className={`tv-h-${h.kind}`}>
                        <span className="tv-h-time">
                          {new Date(h.ts).toLocaleTimeString()}
                        </span>
                        <span className="tv-h-text">{h.text}</span>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            )}
          </div>
        </div>

        <div className="tv-depth">
          <div className="tv-depth-head">{t("trade.depthTitle")}</div>
          <div className="tv-depth-bars">
            <div className="tv-depth-row">
              <span className="tv-depth-side pos">{t("trade.depthLong")}</span>
              <div className="tv-depth-track">
                <div className="tv-depth-fill pos" style={{ width: `${longPct}%` }} />
              </div>
              <span className="tv-depth-val">{longPct.toFixed(1)}%</span>
            </div>
            <div className="tv-depth-row">
              <span className="tv-depth-side neg">{t("trade.depthShort")}</span>
              <div className="tv-depth-track">
                <div className="tv-depth-fill neg" style={{ width: `${shortPct}%` }} />
              </div>
              <span className="tv-depth-val">{shortPct.toFixed(1)}%</span>
            </div>
          </div>

          <div className="tv-depth-kv">
            <Row label={t("trade.longPrincipal")} value={`$${longUsd.toLocaleString("en-US", { maximumFractionDigits: 0 })}`} cls="pos" />
            <Row label={t("trade.shortPrincipal")} value={`$${shortUsd.toLocaleString("en-US", { maximumFractionDigits: 0 })}`} cls="neg" />
            <Row
              label={t("trade.netSkew")}
              value={`${skew >= 0n ? "+" : "−"}$${formatUsdcMicro(skew >= 0n ? skew : -skew)}`}
            />
            <Row label={t("trade.longPos")} value={`${openInterest.longCount}`} />
            <Row label={t("trade.shortPos")} value={`${openInterest.shortCount}`} />
            <Row
              label={t("trade.pendingRecovery")}
              value={`$${formatUsdcMicro(feed.indexer.projectedRecoveryOutstandingMicroUsdc)}`}
            />
          </div>

          <div className="tv-depth-note">{t("trade.depthNote")}</div>
        </div>

        <aside className="tv-order">
          <div className="tv-side-toggle">
            <button
              type="button"
              className={`buy ${side === "Long" ? "active" : ""}`}
              onClick={() => setSide("Long")}
            >
              {t("trade.longBtn")} / Long
            </button>
            <button
              type="button"
              className={`sell ${side === "Short" ? "active" : ""}`}
              onClick={() => setSide("Short")}
            >
              {t("trade.shortBtn")} / Short
            </button>
          </div>

          <div className="tv-lev">
            <div className="tv-lev-head">
              <span>{t("trade.leverage")}</span>
              <span className="tv-lev-current">{leverage}x</span>
            </div>
            <div className="tv-lev-tiers">
              {tiers.map((lv) => (
                <button
                  key={lv}
                  type="button"
                  className={leverage === lv ? "active" : ""}
                  onClick={() => setLeverage(lv)}
                >
                  {lv}x
                </button>
              ))}
            </div>
            <span className="tv-lev-cap">
              {t("trade.maxLevHint", { cls: t(`market.class.${sym.assetClass}`), max: sym.maxLeverage })}
            </span>
          </div>

          <div className="tv-order-type">
            <span className="active">{t("trade.marketOrder")}</span>
            <span className="tv-order-type-note">{t("trade.marketOrderNote")}</span>
          </div>

          <label className="tv-field">
            <span>{t("trade.margin")}</span>
            <div className="tv-input-wrap">
              <input
                type="number"
                min={0}
                value={collateral}
                onChange={(e) => setCollateral(parseFloat(e.target.value) || 0)}
              />
              <span className="tv-input-suffix">USDC</span>
            </div>
          </label>

          <div className="tv-quick">
            {[100, 500, 1000, 5000].map((v) => (
              <button key={v} type="button" onClick={() => setCollateral(v)}>
                {v >= 1000 ? `${v / 1000}k` : v}
              </button>
            ))}
          </div>

          <label className="tv-field">
            <span>{t("trade.subPool")}</span>
            <select
              value={subPoolId}
              onChange={(e) => setSubPoolId(parseInt(e.target.value, 10))}
            >
              {feed.indexer.subPools.length === 0 ? (
                <option value={0}>#0</option>
              ) : (
                feed.indexer.subPools.map((s) => (
                  <option key={s.id} value={s.id}>
                    #{s.id} ({formatPubkey(s.pubkey.hex)})
                  </option>
                ))
              )}
            </select>
          </label>

          <div className="tv-order-summary">
            <Row label={t("trade.estEntry")} value={`$${formatQuote(priceUsd, sym)}`} />
            <Row label={t("trade.leverage")} value={`${leverage}x`} />
            <Row
              label={t("trade.notional")}
              value={`$${notional.toLocaleString("en-US", { maximumFractionDigits: 0 })}`}
            />
            <Row
              label={t("trade.priceEnvelope")}
              value={`±${(Number(ENVELOPE_HALF_WIDTH_BPS) / 100).toFixed(2)}%`}
            />
            <Row label={t("trade.openFee")} value="0.05%" />
            <Row
              label={t("trade.maxLoss")}
              value={t("trade.maxLossVal", { amount: collateral.toLocaleString() })}
              cls="warn"
            />
            <Row label={t("trade.liq")} value={t("common.none")} cls="pos" />
          </div>

          <button
            type="button"
            className={`tv-submit ${side === "Long" ? "buy" : "sell"}`}
            disabled={marketDisabled || (!wasmReady && connected) || busy}
            onClick={() => void submitOpen()}
          >
            {marketDisabled
              ? t("trade.submitPaused")
              : !connected
                ? t("trade.submitConnect")
                : !wasmReady
                  ? t("trade.submitLoading")
                  : busy
                    ? t("trade.submitBusy")
                    : side === "Long"
                      ? t("trade.goLong", { symbol: `${selectedBase} ${leverage}x` })
                      : t("trade.goShort", { symbol: `${selectedBase} ${leverage}x` })}
          </button>

          <p className="tv-order-disclaimer">{t("trade.orderDisclaimer")}</p>
        </aside>
      </div>

      {browserOpen ? (
        <MarketBrowser
          activeBase={selectedBase}
          tickers={tickers}
          onSelect={setSelectedBase}
          onClose={() => setBrowserOpen(false)}
        />
      ) : null}
    </div>
  );
}

function Row({
  label,
  value,
  cls,
}: {
  label: string;
  value: string;
  cls?: string;
}): JSX.Element {
  return (
    <div className="tv-row">
      <span className="tv-row-label">{label}</span>
      <span className={`tv-row-value ${cls ?? ""}`}>{value}</span>
    </div>
  );
}
