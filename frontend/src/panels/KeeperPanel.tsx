import { useState, type JSX } from "react";
import type { FeedSnapshot } from "../types";
import type { WalletAdapter } from "../wallet";
import { formatPercent, formatSlot, formatVol } from "../format";
import {
  buildKeeperLeaderAcquireTx,
  buildKeeperLeaderHeartbeatTx,
  buildKeeperLeaderReleaseTx,
} from "../tx/wasmBuilder";

interface Props {
  feed: FeedSnapshot;
  /**
   * Wave 17 — wallet seam used by the leader-lock ops card. The
   * panel renders the card only when `wallet` is present; tests
   * that don't exercise the ops flow can omit it. Mirrors the
   * trader-panel wallet plumbing.
   */
  wallet?: WalletAdapter;
}

export function KeeperPanel({ feed, wallet }: Props): JSX.Element {
  const k = feed.keeper;
  const m = k.metrics;

  const failRate =
    m.cumulative.submitted + m.cumulative.failed === 0
      ? 0
      : m.cumulative.failed / (m.cumulative.submitted + m.cumulative.failed);

  const top = k.predictions.slice(0, 8);

  return (
    <div className="panel keeper-panel">
      <section className="stat-strip">
        <Stat label="Status" value={k.status} cls={`status-${k.status}`} />
        <Stat label="Tick slot" value={formatSlot(m.tickSlot)} />
        <Stat label="σ̂ (vol)" value={formatVol(m.appliedVol)} />
        <Stat
          label="Vol samples"
          value={`${m.volSamples}/32`}
          warn={m.appliedVol === null}
        />
        <Stat label="Wallet (SOL)" value={m.walletBalanceSol.toFixed(4)} warn={m.walletBalanceSol < 0.5} />
        <Stat
          label="Fail rate"
          value={formatPercent(failRate)}
          warn={failRate > 0.05}
        />
      </section>

      <section className="card">
        <h2>Cumulative tick metrics</h2>
        <div className="kv-grid">
          <KV label="Submitted (cum.)" value={m.cumulative.submitted.toString()} />
          <KV label="Failed (cum.)" value={m.cumulative.failed.toString()} />
          <KV label="Skipped (cum.)" value={m.cumulative.skipped.toString()} />
          <KV label="Recent submitted" value={m.recent.submitted.toString()} />
          <KV label="Recent failed" value={m.recent.failed.toString()} />
          <KV label="Recent skipped" value={m.recent.skipped.toString()} />
          <KV label="Tick duration (ms)" value={m.recent.durationMs.toString()} />
          <KV
            label="Vol estimator"
            value={m.appliedVol === null ? "warming up" : "applied"}
          />
        </div>
      </section>

      <section className="card">
        <h2>Top {top.length} rotation predictions</h2>
        <table className="data-table">
          <thead>
            <tr>
              <th>Sub-pool</th>
              <th>Direction</th>
              <th>Tick</th>
              <th>Score</th>
              <th>Bar</th>
              <th>Triggered?</th>
            </tr>
          </thead>
          <tbody>
            {top.map((p, i) => (
              <tr key={`${p.subPoolId}-${p.direction}-${p.tick}-${i}`}>
                <td>#{p.subPoolId}</td>
                <td className={`side-${p.direction.toLowerCase()}`}>{p.direction}</td>
                <td>{p.tick}</td>
                <td>{p.score.toFixed(3)}</td>
                <td>
                  <div className="score-bar">
                    <div
                      className={`score-fill ${p.triggered ? "score-fill-hot" : ""}`}
                      style={{ width: `${Math.min(100, p.score * 100)}%` }}
                    />
                  </div>
                </td>
                <td>
                  {p.triggered ? (
                    <span className="status status-running">YES</span>
                  ) : (
                    <span className="status status-paused">no</span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>

      {wallet && (
        <LeaderLockOpsCard
          wallet={wallet}
          currentSlot={feed.currentSlot ?? BigInt(feed.indexer.market.lastOracleSlot)}
        />
      )}

      <section className="card">
        <h2>Recent submitted signatures</h2>
        {k.recentSignatures.length === 0 ? (
          <p className="empty">No submissions in the most recent tick.</p>
        ) : (
          <ul className="sig-list">
            {k.recentSignatures.map((s, i) => (
              <li key={i} className="mono">
                {s}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

/**
 * Wave 17 — manual keeper-leader instruction ops card.
 *
 * Renders three buttons (acquire / heartbeat / release) that build
 * the wave-15 keeper-leader instruction bytes via wasm and route
 * them through the wallet's `signAndSubmit`. This is the browser
 * equivalent of the `ops-toolkit/ts/keeper-leader-{acquire,
 * heartbeat,release}.ts` CLI scripts — useful during incident
 * response when the operator only has Phantom + a console window.
 *
 * The card never *forces* a tx: every button is dry-run-aware via
 * the underlying wallet adapter, so the wave-15 demo path stays
 * synthetic-signature-only until a real wallet is connected.
 */
function LeaderLockOpsCard({
  wallet,
  currentSlot,
}: {
  wallet: WalletAdapter;
  currentSlot: bigint;
}): JSX.Element {
  const [busy, setBusy] = useState<"acquire" | "heartbeat" | "release" | null>(
    null,
  );
  const [feedback, setFeedback] = useState<string | null>(null);

  async function submit(
    kind: "acquire" | "heartbeat" | "release",
    bytes: Uint8Array,
    description: string,
  ): Promise<void> {
    if (wallet.status() !== "connected") {
      setFeedback(
        `[wallet] connect ${wallet.name} before submitting ${kind} (${bytes.length}B).`,
      );
      return;
    }
    setBusy(kind);
    setFeedback(null);
    try {
      const sig = await wallet.signAndSubmit({ description, borshBytes: bytes });
      setFeedback(
        `[${wallet.name}] ${kind} submitted (${bytes.length}B) sig=${sig}`,
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setFeedback(`[wallet error] ${msg}`);
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className="card">
      <h2>Keeper-leader-lock ops (wave 17)</h2>
      <p className="card-help">
        Manual instruction triggers for incident response. See{" "}
        <code>Docs/Planning/24-operator-runbook.md §6.5</code> (KL-02..05) for
        when to use each one. The on-chain handler enforces the same reject
        matrix the wave-16 program-test pinned, so wrong-state buttons fail
        with a clear error rather than a quiet success.
      </p>
      <div className="leader-ops-row">
        <button
          type="button"
          disabled={busy !== null}
          onClick={() =>
            submit(
              "acquire",
              buildKeeperLeaderAcquireTx({ observedSlot: currentSlot }),
              `keeper_leader_acquire observed_slot=${currentSlot}`,
            )
          }
        >
          {busy === "acquire" ? "Submitting…" : "Acquire (force-take stale)"}
        </button>
        <button
          type="button"
          disabled={busy !== null}
          onClick={() =>
            submit(
              "heartbeat",
              buildKeeperLeaderHeartbeatTx({ observedSlot: currentSlot }),
              `keeper_leader_heartbeat observed_slot=${currentSlot}`,
            )
          }
        >
          {busy === "heartbeat" ? "Submitting…" : "Heartbeat (refresh as holder)"}
        </button>
        <button
          type="button"
          disabled={busy !== null}
          onClick={() =>
            submit(
              "release",
              buildKeeperLeaderReleaseTx(),
              "keeper_leader_release",
            )
          }
        >
          {busy === "release" ? "Submitting…" : "Release (planned handoff)"}
        </button>
      </div>
      {feedback && <p className="leader-ops-feedback mono">{feedback}</p>}
    </section>
  );
}

function Stat({
  label,
  value,
  warn = false,
  cls = "",
}: {
  label: string;
  value: string;
  warn?: boolean;
  cls?: string;
}): JSX.Element {
  return (
    <div className={`stat ${warn ? "stat-warn" : ""} ${cls}`}>
      <span className="stat-label">{label}</span>
      <span className="stat-value">{value}</span>
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
