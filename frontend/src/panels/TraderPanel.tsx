import { useEffect, useMemo, useState } from "react";
import type { JSX } from "react";
import type { FeedSnapshot, Direction } from "../types";
import type { WalletAdapter } from "../wallet";
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
  reconcilePrincipal,
  reconcileProgramAggregate,
  totalCollateral,
  totalCount,
} from "../feed/openInterest";

interface Props {
  feed: FeedSnapshot;
  /**
   * Wallet seam. Wave 14 wired the real `WindowWalletAdapter` and
   * `MockWalletAdapter`; wave 15 forwards real Borsh-encoded transaction
   * bytes through `signAndSubmit({ borshBytes })`.
   */
  wallet: WalletAdapter;
}

interface DraftPosition {
  subPoolId: number;
  direction: Direction;
  qty: number;
  collateral: number;
}

const DEFAULT_DRAFT: DraftPosition = {
  subPoolId: 0,
  direction: "Long",
  qty: 100,
  collateral: 1_000,
};

/**
 * Wave 15 — envelope width tolerated by the on-chain handler when the
 * frontend doesn't have a fresh oracle reading. ±50 bps mirrors the
 * default `max_price_move_bps_per_sync` setting and gives the on-chain
 * `PriceEnvelope::assert_in` check enough slack for a 0.5 % oracle
 * jitter between the time the user clicks and the time the tx lands.
 */
const DEFAULT_ENVELOPE_HALF_WIDTH_BPS = 50n;
const BPS_DENOMINATOR = 10_000n;

/**
 * Compute the four-tuple `(p_now, slot, expected_min, expected_max)`
 * required by the on-chain `open_position` / `close_position` ix.
 *
 * Wave 15 uses the latest indexer-side mid price + slot directly. When
 * the frontend wires a richer oracle source (wave 16+) this helper
 * gets a tighter envelope; for now it gives the user a deterministic
 * ±50 bps band that matches the audit-readiness wave-13 contract.
 */
function buildEnvelope(feed: FeedSnapshot): {
  pNow: bigint;
  slot: bigint;
  expectedMin: bigint;
  expectedMax: bigint;
} {
  const pNow = BigInt(feed.indexer.market.midPriceMicro);
  const slot = BigInt(feed.indexer.market.lastOracleSlot);
  const half = (pNow * DEFAULT_ENVELOPE_HALF_WIDTH_BPS) / BPS_DENOMINATOR;
  return {
    pNow,
    slot,
    expectedMin: pNow > half ? pNow - half : 0n,
    expectedMax: pNow + half,
  };
}

/**
 * Synthesise a 64-bit position id from the wallet pubkey + a fresh
 * timestamp. Wave 15: deterministic enough that retries within the
 * same second collide on purpose (so a user double-clicking doesn't
 * spawn two open positions); production wave 16 will read the
 * keeper-bot's `nonce` from chain.
 */
function nextPositionId(walletHex: string | undefined): bigint {
  const ts = BigInt(Math.floor(Date.now() / 1000));
  if (!walletHex) {
    return ts;
  }
  // Hash-fold the first 8 bytes of the wallet hex for entropy.
  let folded = 0n;
  for (let i = 0; i < 8 && i < walletHex.length / 2; i += 1) {
    const byte = BigInt(parseInt(walletHex.slice(i * 2, i * 2 + 2), 16));
    folded = folded ^ (byte << BigInt(i * 8));
  }
  // Pack: high 32 bits = ts, low 32 bits = folded entropy.
  return (ts << 32n) | (folded & 0xffff_ffffn);
}

export function TraderPanel({ feed, wallet }: Props): JSX.Element {
  const [draft, setDraft] = useState<DraftPosition>(DEFAULT_DRAFT);
  const [confirmation, setConfirmation] = useState<string | null>(null);
  const [wasmReady, setWasmReady] = useState(false);
  const [wasmError, setWasmError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    loadKeeperDecoder()
      .then(() => {
        if (!cancelled) {
          setWasmReady(true);
        }
      })
      .catch((e: unknown) => {
        if (!cancelled) {
          const msg = e instanceof Error ? e.message : String(e);
          setWasmError(msg);
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const market = feed.indexer.market;
  const subPool = useMemo(
    () => feed.indexer.subPools.find((s) => s.id === draft.subPoolId),
    [feed.indexer.subPools, draft.subPoolId],
  );
  const openInterest = useMemo(
    () => aggregateOpenInterest(feed.positions),
    [feed.positions],
  );
  const reconciliation = useMemo(() => {
    const reported = feed.indexer.subPools.reduce(
      (acc, sp) => acc + sp.longCollateral + sp.shortCollateral,
      0n,
    );
    return reconcilePrincipal(totalCollateral(openInterest), reported);
  }, [feed.indexer.subPools, openInterest]);
  // Wave 27 — program-aggregate reconciliation: the Market's own
  // running `current_total_principal` counter against the live
  // position-collateral sum (two independent on-chain truths).
  const programReconciliation = useMemo(
    () =>
      reconcileProgramAggregate(
        feed.positions,
        feed.indexer.market.currentTotalPrincipal,
      ),
    [feed.positions, feed.indexer.market.currentTotalPrincipal],
  );

  function update<K extends keyof DraftPosition>(key: K, val: DraftPosition[K]) {
    setDraft((d) => ({ ...d, [key]: val }));
    setConfirmation(null);
  }

  async function submitOpen() {
    if (wallet.status() !== "connected") {
      setConfirmation(
        `[wallet] connect a wallet (${wallet.name}) before submitting open_position.`,
      );
      return;
    }
    if (!wasmReady) {
      setConfirmation(
        `[tx-builder] wasm encoder still loading${wasmError ? ` (error: ${wasmError})` : ""}.`,
      );
      return;
    }
    try {
      const envelope = buildEnvelope(feed);
      const positionId = nextPositionId(wallet.pubkey()?.hex);
      // 1 USDC = 1e6 minor units. Borsh-encoded as little-endian u64.
      const grossAmount = BigInt(draft.collateral) * 1_000_000n;
      const borshBytes = buildOpenPositionTx({
        envelope,
        directionIsLong: draft.direction === "Long",
        grossAmount,
        positionId,
      });
      const sig = await wallet.signAndSubmit({
        description: `open_position sub_pool_id=${draft.subPoolId} dir=${draft.direction} qty=${draft.qty} collateral=${draft.collateral}`,
        borshBytes,
      });
      setConfirmation(
        `[${wallet.name}] open_position submitted (sub_pool=${draft.subPoolId}, ${draft.direction}, gross=${grossAmount}, position_id=${positionId}, env=[${envelope.expectedMin},${envelope.expectedMax}]) sig=${sig} (${borshBytes.length}B)`,
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setConfirmation(`[wallet error] ${msg}`);
    }
  }

  async function submitClose(positionLabel: string, subPoolId: number) {
    if (wallet.status() !== "connected") {
      setConfirmation(
        `[wallet] connect ${wallet.name} before closing position ${positionLabel}.`,
      );
      return;
    }
    if (!wasmReady) {
      setConfirmation(
        `[tx-builder] wasm encoder still loading${wasmError ? ` (error: ${wasmError})` : ""}.`,
      );
      return;
    }
    try {
      const envelope = buildEnvelope(feed);
      // wave-15 frontend doesn't yet route per-position dormant buckets
      // — closing always asks the on-chain handler for zero buckets,
      // and the keeper bot's pre_sync passes is responsible for
      // surfacing them. The wave-16 dormant routing layer will fill
      // these in. Audit gate: this MUST stay a fixed 0,0 until then.
      const borshBytes = buildClosePositionTx({
        envelope,
        longBucketCount: 0,
        shortBucketCount: 0,
      });
      const sig = await wallet.signAndSubmit({
        description: `close_position sub_pool=${subPoolId} ${positionLabel}`,
        borshBytes,
      });
      setConfirmation(
        `[${wallet.name}] close_position submitted (sub_pool=${subPoolId}, env=[${envelope.expectedMin},${envelope.expectedMax}]) sig=${sig} (${borshBytes.length}B)`,
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setConfirmation(`[wallet error] ${msg}`);
    }
  }

  const marketDisabled =
    market.paused || market.pausedGlobally || market.frozenNewPosition;
  const walletConnected = wallet.status() === "connected";
  const disabled = marketDisabled;

  return (
    <div className="panel trader-panel">
      <section className="hero">
        <div className="hero-stack">
          <span className="hero-label">Mid price</span>
          <span className="hero-value">${formatPriceMicro(market.midPriceMicro)}</span>
          <span className="hero-sub">
            oracle slot {market.lastOracleSlot} (lag{" "}
            {market.currentSlot - market.lastOracleSlot})
          </span>
        </div>
        <div className="hero-stack">
          <span className="hero-label">Symbol</span>
          <span className="hero-value">{market.symbol}</span>
          <span className="hero-sub">market {formatPubkey(market.pubkey.hex)}</span>
        </div>
        <div className="hero-stack">
          <span className="hero-label">Status</span>
          <span className={`hero-value status-${disabled ? "paused" : "running"}`}>
            {market.pausedGlobally
              ? "GLOBAL PAUSED"
              : market.paused
                ? "MARKET PAUSED"
                : market.frozenNewPosition
                  ? "OPENS FROZEN"
                  : "OPEN"}
          </span>
          <span className="hero-sub">schema v{market.schemaVersion}</span>
        </div>
        <div className="hero-stack">
          <span className="hero-label">Tx encoder</span>
          <span
            className={`hero-value status-${wasmError ? "paused" : wasmReady ? "running" : "warming"}`}
          >
            {wasmError ? "ERR" : wasmReady ? "wasm READY" : "loading…"}
          </span>
          <span className="hero-sub">
            keeper-decoder.wasm (wave 15)
          </span>
        </div>
      </section>

      <section className="card">
        <h2>Open new position</h2>
        <div className="form-grid">
          <label>
            <span>Sub-pool</span>
            <select
              value={draft.subPoolId}
              onChange={(e) => update("subPoolId", parseInt(e.target.value, 10))}
            >
              {feed.indexer.subPools.map((s) => (
                <option key={s.id} value={s.id}>
                  #{s.id} ({formatPubkey(s.pubkey.hex)})
                </option>
              ))}
            </select>
          </label>
          <label>
            <span>Direction</span>
            <div className="dir-toggle">
              <button
                type="button"
                className={draft.direction === "Long" ? "active" : ""}
                onClick={() => update("direction", "Long")}
              >
                Long
              </button>
              <button
                type="button"
                className={draft.direction === "Short" ? "active" : ""}
                onClick={() => update("direction", "Short")}
              >
                Short
              </button>
            </div>
          </label>
          <label>
            <span>Quantity (units)</span>
            <input
              type="number"
              min={1}
              value={draft.qty}
              onChange={(e) => update("qty", parseInt(e.target.value, 10) || 0)}
            />
          </label>
          <label>
            <span>Collateral (USDC)</span>
            <input
              type="number"
              min={0}
              value={draft.collateral}
              onChange={(e) => update("collateral", parseInt(e.target.value, 10) || 0)}
            />
          </label>
        </div>
        <button
          type="button"
          className="primary-btn"
          onClick={() => {
            void submitOpen();
          }}
          disabled={disabled || !wasmReady}
        >
          {disabled
            ? "Disabled — market not accepting opens"
            : !wasmReady
              ? "Loading tx encoder…"
              : walletConnected
                ? `Submit via ${wallet.name}`
                : "Connect wallet to submit"}
        </button>
        {confirmation && <pre className="confirmation">{confirmation}</pre>}
      </section>

      {subPool && (
        <section className="card">
          <h2>Selected sub-pool snapshot</h2>
          <div className="kv-grid">
            <KV label="Total open Long" value={formatBigQty(subPool.totalOpenLongQty)} />
            <KV label="Total open Short" value={formatBigQty(subPool.totalOpenShortQty)} />
            <KV
              label="Long collateral (USDC)"
              value={formatUsdcMicro(subPool.longCollateral)}
            />
            <KV
              label="Short collateral (USDC)"
              value={formatUsdcMicro(subPool.shortCollateral)}
            />
            <KV
              label="Dormant Long ticks"
              value={String(subPool.dormantInventory.Long)}
            />
            <KV
              label="Dormant Short ticks"
              value={String(subPool.dormantInventory.Short)}
            />
          </div>
        </section>
      )}

      <section className="card">
        <h2>Market open interest</h2>
        <div className="kv-grid">
          <KV
            label="Live positions"
            value={`${totalCount(openInterest)} (${openInterest.longCount}L / ${openInterest.shortCount}S)`}
          />
          <KV
            label="Total collateral (USDC)"
            value={formatUsdcMicro(totalCollateral(openInterest))}
          />
          <KV
            label="Long collateral (USDC)"
            value={formatUsdcMicro(openInterest.longCollateral)}
          />
          <KV
            label="Short collateral (USDC)"
            value={formatUsdcMicro(openInterest.shortCollateral)}
          />
          <KV
            label="Long qty"
            value={formatBigQty(openInterest.longQty)}
          />
          <KV
            label="Short qty"
            value={formatBigQty(openInterest.shortQty)}
          />
          <KV
            label="Net skew (USDC)"
            value={`${netCollateralImbalance(openInterest) >= 0n ? "+" : "−"}${formatUsdcMicro(
              netCollateralImbalance(openInterest) >= 0n
                ? netCollateralImbalance(openInterest)
                : -netCollateralImbalance(openInterest),
            )}`}
          />
        </div>
        <div className="reconcile-row">
          <span className="kv-label">Indexer reconciliation</span>
          <span className={`recon-badge recon-${reconciliation.status}`}>
            {reconciliation.status === "disabled"
              ? "no live positions"
              : reconciliation.status === "ok"
                ? `reconciled (drift ${(reconciliation.driftRatio * 100).toFixed(2)}%)`
                : `${reconciliation.status.toUpperCase()} — drift ${(reconciliation.driftRatio * 100).toFixed(2)}% (on-chain ${formatUsdcMicro(reconciliation.onchainCollateral)} vs reported ${formatUsdcMicro(reconciliation.reportedCollateral)})`}
          </span>
        </div>
        <div className="reconcile-row">
          <span className="kv-label">Program aggregate</span>
          <span className={`recon-badge recon-${programReconciliation.status}`}>
            {programReconciliation.status === "disabled"
              ? "no aggregate / positions"
              : programReconciliation.status === "ok"
                ? `reconciled (drift ${(programReconciliation.driftRatio * 100).toFixed(2)}%)`
                : `${programReconciliation.status.toUpperCase()} — drift ${(programReconciliation.driftRatio * 100).toFixed(2)}% (positions ${formatUsdcMicro(programReconciliation.onchainCollateral)} vs market ${formatUsdcMicro(programReconciliation.reportedCollateral)})`}
          </span>
        </div>
      </section>

      <section className="card">
        <h2>Your positions ({feed.positions.length})</h2>
        <table className="data-table">
          <thead>
            <tr>
              <th>Owner</th>
              <th>Sub-pool</th>
              <th>Side</th>
              <th>Qty</th>
              <th>Collateral</th>
              <th>Opened</th>
              <th>Action</th>
            </tr>
          </thead>
          <tbody>
            {feed.positions.map((p, idx) => (
              <tr key={`${p.owner.hex}-${idx}`}>
                <td className="mono">{formatPubkey(p.owner.hex)}</td>
                <td>#{p.subPoolId}</td>
                <td className={`side-${p.direction.toLowerCase()}`}>{p.direction}</td>
                <td>{formatBigQty(p.qty)}</td>
                <td>${formatUsdcMicro(p.collateral)}</td>
                <td>{new Date(p.openedAt * 1000).toLocaleTimeString()}</td>
                <td>
                  <button
                    type="button"
                    className="secondary-btn"
                    onClick={() => {
                      void submitClose(
                        `owner=${p.owner.hex.slice(0, 8)}… ${p.direction}`,
                        p.subPoolId,
                      );
                    }}
                    disabled={!wasmReady}
                  >
                    Close
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </div>
  );
}

function KV({ label, value }: { label: string; value: string }): JSX.Element {
  return (
    <div className="kv">
      <span className="kv-label">{label}</span>
      <span className="kv-value">{value}</span>
    </div>
  );
}
