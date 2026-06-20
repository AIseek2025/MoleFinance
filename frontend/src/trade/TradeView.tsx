import { useEffect, useMemo, useRef, useState } from "react";
import type { JSX } from "react";
import type { Direction, FeedSnapshot } from "../types";
import type { WalletAdapter, WalletStatus } from "../wallet";
import {
  formatPriceMicro,
  formatPubkey,
  formatUsdcMicro,
  formatBigQty,
} from "../format";
import {
  buildClosePositionTx,
  buildOpenPositionTx,
  loadKeeperDecoder,
} from "../tx/wasmBuilder";
import {
  aggregateOpenInterest,
  netCollateralImbalance,
  totalCollateral,
} from "../feed/openInterest";
import { PriceChart } from "./PriceChart";
import "./trade.css";

interface Props {
  feed: FeedSnapshot;
  wallet: WalletAdapter;
  walletStatus: WalletStatus;
  walletPubkeyHex?: string;
  onConnect: () => void;
  onDisconnect: () => void;
  /** Configured market symbols (multi-market). Empty in single-market mode. */
  symbols: string[];
  activeSymbol: string | null;
  onSymbolChange: (s: string) => void;
  onHome: () => void;
  onConsole: () => void;
}

const ENVELOPE_HALF_WIDTH_BPS = 50n;
const BPS_DENOMINATOR = 10_000n;

const TIMEFRAMES: { label: string; sec: number }[] = [
  { label: "5s", sec: 5 },
  { label: "15s", sec: 15 },
  { label: "1m", sec: 60 },
  { label: "5m", sec: 300 },
];

function buildEnvelope(feed: FeedSnapshot) {
  const pNow = BigInt(feed.indexer.market.midPriceMicro);
  const slot = BigInt(feed.indexer.market.lastOracleSlot);
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
  const market = feed.indexer.market;
  const [side, setSide] = useState<Direction>("Long");
  const [collateral, setCollateral] = useState<number>(1000);
  const [subPoolId, setSubPoolId] = useState<number>(0);
  const [intervalSec, setIntervalSec] = useState<number>(15);
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

  const priceUsd = useMemo(
    () => Number(market.midPriceMicro) / 1_000_000,
    [market.midPriceMicro],
  );

  // Session anchor: first price observed this mount → drives % change.
  const sessionOpenRef = useRef<number | null>(null);
  if (sessionOpenRef.current == null && Number.isFinite(priceUsd) && priceUsd > 0) {
    sessionOpenRef.current = priceUsd;
  }
  const sessionOpen = sessionOpenRef.current ?? priceUsd;
  const changePct = sessionOpen > 0 ? ((priceUsd - sessionOpen) / sessionOpen) * 100 : 0;
  const up = changePct >= 0;

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
    market.paused || market.pausedGlobally || market.frozenNewPosition;
  const connected = walletStatus === "connected";

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
      const envelope = buildEnvelope(feed);
      const positionId = nextPositionId(wallet.pubkey()?.hex);
      const grossAmount = BigInt(Math.max(0, Math.floor(collateral))) * 1_000_000n;
      const borshBytes = buildOpenPositionTx({
        envelope,
        directionIsLong: side === "Long",
        grossAmount,
        positionId,
      });
      const sig = await wallet.signAndSubmit({
        description: `open ${side} sub_pool=${subPoolId} collateral=${collateral} USDC`,
        borshBytes,
      });
      pushHistory({
        ts: Date.now(),
        kind: "open",
        text: `开仓 ${side} · ${collateral} USDC · 子池#${subPoolId} · sig ${formatPubkey(sig)}`,
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      pushHistory({ ts: Date.now(), kind: "error", text: `开仓失败: ${msg}` });
    } finally {
      setBusy(false);
    }
  }

  async function submitClose(label: string, sp: number) {
    if (!connected || !wasmReady || busy) return;
    setBusy(true);
    try {
      const envelope = buildEnvelope(feed);
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
        text: `平仓 ${label} · 子池#${sp} · sig ${formatPubkey(sig)}`,
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      pushHistory({ ts: Date.now(), kind: "error", text: `平仓失败: ${msg}` });
    } finally {
      setBusy(false);
    }
  }

  const statusLabel = market.pausedGlobally
    ? "全局暂停"
    : market.paused
      ? "市场暂停"
      : market.frozenNewPosition
        ? "禁止开仓"
        : "正常交易";

  return (
    <div className="tv">
      {/* Top bar */}
      <header className="tv-top">
        <button type="button" className="tv-brand" onClick={onHome}>
          <span className="tv-brand-mark" /> MoleOption
        </button>

        <div className="tv-market-pick">
          {symbols.length > 0 ? (
            <select
              value={activeSymbol ?? ""}
              onChange={(e) => onSymbolChange(e.target.value)}
              className="tv-symbol-select"
            >
              {symbols.map((s) => (
                <option key={s} value={s}>
                  {s}
                </option>
              ))}
            </select>
          ) : (
            <span className="tv-symbol-static">{market.symbol}</span>
          )}
          <span className={`tv-status-chip ${marketDisabled ? "off" : "on"}`}>
            {statusLabel}
          </span>
        </div>

        <div className="tv-stats">
          <div className="tv-stat">
            <span className="tv-stat-label">现价</span>
            <span className={`tv-stat-value tv-price ${up ? "pos" : "neg"}`}>
              ${formatPriceMicro(market.midPriceMicro)}
            </span>
          </div>
          <div className="tv-stat">
            <span className="tv-stat-label">本场涨跌</span>
            <span className={`tv-stat-value ${up ? "pos" : "neg"}`}>
              {up ? "+" : ""}
              {changePct.toFixed(2)}%
            </span>
          </div>
          <div className="tv-stat">
            <span className="tv-stat-label">强平价</span>
            <span className="tv-stat-value tv-none">无</span>
          </div>
          <div className="tv-stat">
            <span className="tv-stat-label">预言机 slot</span>
            <span className="tv-stat-value">
              {market.lastOracleSlot}
              <em className="tv-lag"> · lag {market.currentSlot - market.lastOracleSlot}</em>
            </span>
          </div>
          <div className="tv-stat">
            <span className="tv-stat-label">未平仓本金</span>
            <span className="tv-stat-value">${formatUsdcMicro(totalCollateral(openInterest))}</span>
          </div>
        </div>

        <div className="tv-top-actions">
          <button type="button" className="tv-link" onClick={onConsole}>
            控制台
          </button>
          {connected ? (
            <button type="button" className="tv-wallet connected" onClick={onDisconnect}>
              {walletPubkeyHex ? formatPubkey(walletPubkeyHex) : "已连接"} · 断开
            </button>
          ) : (
            <button
              type="button"
              className="tv-wallet"
              onClick={onConnect}
              disabled={walletStatus === "connecting"}
            >
              {walletStatus === "connecting" ? "连接中…" : "连接钱包"}
            </button>
          )}
        </div>
      </header>

      {/* Main grid */}
      <div className="tv-main">
        {/* Left: chart + bottom tabs */}
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
              <span className="tv-chart-note">价格源 · 预言机逐块喂价(devnet)</span>
            </div>
            <PriceChart
              priceUsd={Number.isFinite(priceUsd) ? priceUsd : null}
              intervalSec={intervalSec}
              symbol={market.symbol}
            />
          </div>

          <div className="tv-bottom">
            <div className="tv-tabs">
              <button
                type="button"
                className={bottomTab === "positions" ? "active" : ""}
                onClick={() => setBottomTab("positions")}
              >
                持仓 ({feed.positions.length})
              </button>
              <button
                type="button"
                className={bottomTab === "history" ? "active" : ""}
                onClick={() => setBottomTab("history")}
              >
                交易历史 ({history.length})
              </button>
            </div>

            {bottomTab === "positions" ? (
              <div className="tv-table-scroll">
                <table className="tv-table">
                  <thead>
                    <tr>
                      <th>方向</th>
                      <th>账户</th>
                      <th>子池</th>
                      <th>数量</th>
                      <th>保证金</th>
                      <th>最大亏损</th>
                      <th>开仓时间</th>
                      <th />
                    </tr>
                  </thead>
                  <tbody>
                    {feed.positions.length === 0 ? (
                      <tr className="tv-empty-row">
                        <td colSpan={8}>暂无持仓 — 在右侧开出你的第一笔仓位</td>
                      </tr>
                    ) : (
                      feed.positions.map((p, i) => (
                        <tr key={`${p.owner.hex}-${i}`}>
                          <td className={p.direction === "Long" ? "pos" : "neg"}>
                            {p.direction === "Long" ? "做多" : "做空"}
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
                              平仓
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
                  <div className="tv-history-empty">本会话尚无交易记录。</div>
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

        {/* Middle: pool depth / long-short skew */}
        <div className="tv-depth">
          <div className="tv-depth-head">多空力量 · 子池盘口</div>
          <div className="tv-depth-bars">
            <div className="tv-depth-row">
              <span className="tv-depth-side pos">做多</span>
              <div className="tv-depth-track">
                <div className="tv-depth-fill pos" style={{ width: `${longPct}%` }} />
              </div>
              <span className="tv-depth-val">{longPct.toFixed(1)}%</span>
            </div>
            <div className="tv-depth-row">
              <span className="tv-depth-side neg">做空</span>
              <div className="tv-depth-track">
                <div className="tv-depth-fill neg" style={{ width: `${shortPct}%` }} />
              </div>
              <span className="tv-depth-val">{shortPct.toFixed(1)}%</span>
            </div>
          </div>

          <div className="tv-depth-kv">
            <Row label="多头本金" value={`$${longUsd.toLocaleString("en-US", { maximumFractionDigits: 0 })}`} cls="pos" />
            <Row label="空头本金" value={`$${shortUsd.toLocaleString("en-US", { maximumFractionDigits: 0 })}`} cls="neg" />
            <Row
              label="净偏斜"
              value={`${skew >= 0n ? "+" : "−"}$${formatUsdcMicro(skew >= 0n ? skew : -skew)}`}
            />
            <Row label="多头仓位" value={`${openInterest.longCount}`} />
            <Row label="空头仓位" value={`${openInterest.shortCount}`} />
            <Row
              label="待兑付 recovery"
              value={`$${formatUsdcMicro(feed.indexer.projectedRecoveryOutstandingMicroUsdc)}`}
            />
          </div>

          <div className="tv-depth-note">
            盈利来自对手盘亏损——此处展示当前多空本金分布与净偏斜，替代传统订单簿。
          </div>
        </div>

        {/* Right: order form */}
        <aside className="tv-order">
          <div className="tv-side-toggle">
            <button
              type="button"
              className={`buy ${side === "Long" ? "active" : ""}`}
              onClick={() => setSide("Long")}
            >
              做多 / Long
            </button>
            <button
              type="button"
              className={`sell ${side === "Short" ? "active" : ""}`}
              onClick={() => setSide("Short")}
            >
              做空 / Short
            </button>
          </div>

          <div className="tv-order-type">
            <span className="active">市价单</span>
            <span className="tv-order-type-note">按预言机价格逐块结算</span>
          </div>

          <label className="tv-field">
            <span>保证金 (USDC)</span>
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
            <span>子池</span>
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
            <Row label="预计开仓价" value={`$${formatPriceMicro(market.midPriceMicro)}`} />
            <Row
              label="价格封套"
              value={`±${(Number(ENVELOPE_HALF_WIDTH_BPS) / 100).toFixed(2)}%`}
            />
            <Row label="开仓费" value="0.05%" />
            <Row label="最大亏损" value={`${collateral.toLocaleString()} USDC + 费`} cls="warn" />
            <Row label="强平价格" value="无 · None" cls="pos" />
          </div>

          <button
            type="button"
            className={`tv-submit ${side === "Long" ? "buy" : "sell"}`}
            disabled={marketDisabled || (!wasmReady && connected) || busy}
            onClick={() => void submitOpen()}
          >
            {marketDisabled
              ? "市场暂停"
              : !connected
                ? "连接钱包"
                : !wasmReady
                  ? "编码器加载中…"
                  : busy
                    ? "提交中…"
                    : side === "Long"
                      ? `做多 ${market.symbol}`
                      : `做空 ${market.symbol}`}
          </button>

          <p className="tv-order-disclaimer">
            "永不爆仓"指仓位不会被强平，最大亏损为本金加费用。盈利兑现取决于对手盘亏损。
          </p>
        </aside>
      </div>
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
