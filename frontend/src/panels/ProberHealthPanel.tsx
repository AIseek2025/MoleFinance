// Wave 26 — operator-facing view of the ops-toolkit prober's published
// health snapshot. The `ops-toolkit prober` daemon writes a
// multi-market JSON report every cycle (now folding live open-interest
// into the wave-24 `position_principal_drift` check); this panel polls
// that snapshot (via `useProberSnapshot`) and renders the per-market
// verdict, with the principal/notional drift check surfaced first.
//
// Renders nothing when no snapshot is configured (`VITE_PROBER_SNAPSHOT_URL`
// unset) so mock / offline dev is unaffected.

import type { JSX } from "react";

import {
  driftCheckFor,
  firingChecks,
  type ProberCheck,
  type ProberCheckStatus,
  type ProberSnapshot,
} from "../feed/proberSnapshot";

export interface ProberHealthPanelProps {
  snapshot: ProberSnapshot | null;
}

function statusClass(s: ProberCheckStatus): string {
  return s === "CRITICAL" ? "crit" : s === "WARN" ? "warn" : "ok";
}

function driftPct(check: ProberCheck): string | null {
  const ratio = check.measurements.drift_ratio;
  if (typeof ratio !== "number") return null;
  return `${(ratio * 100).toFixed(2)}%`;
}

export function ProberHealthPanel(props: ProberHealthPanelProps): JSX.Element | null {
  const snapshot = props.snapshot;
  if (!snapshot || snapshot.markets.size === 0) return null;

  const markets = [...snapshot.markets.values()].sort((a, b) =>
    a.symbol.localeCompare(b.symbol),
  );
  const worst = snapshot.worstExitCode;

  return (
    <section className="prober-health card" aria-label="Prober health snapshot">
      <header className="prober-health-head">
        <h2>Prober health</h2>
        <span className={`prober-overall ${worst === 0 ? "ok" : "crit"}`}>
          {worst === 0 ? "all markets healthy" : `worst exit ${worst}`}
        </span>
      </header>
      <div className="prober-grid">
        {markets.map((m) => {
          const drift = driftCheckFor(m);
          const firing = firingChecks(m);
          return (
            <article key={m.symbol} className="prober-market">
              <div className="prober-market-head">
                <span className="prober-symbol">{m.symbol}</span>
                <span className={`prober-badge ${statusClass(m.overallStatus)}`}>
                  {m.overallStatus}
                </span>
              </div>
              <div className="prober-counts">
                {m.counts.pass} pass · {m.counts.warn} warn ·{" "}
                {m.counts.critical} crit
              </div>
              {(() => {
                const enabled = drift?.measurements.drift_enabled === 1;
                const cls = drift && enabled ? statusClass(drift.status) : "disabled";
                const val =
                  drift && enabled
                    ? `${drift.status}${driftPct(drift) ? ` · ${driftPct(drift)}` : ""}`
                    : "skipped (no probe)";
                return (
                  <div className={`prober-drift ${cls}`} title={drift?.message}>
                    <span className="prober-drift-label">principal drift</span>
                    <span className="prober-drift-val">{val}</span>
                  </div>
                );
              })()}
              {firing.length > 0 ? (
                <ul className="prober-firing">
                  {firing.slice(0, 4).map((c) => (
                    <li key={c.name} className={statusClass(c.status)}>
                      <code>{c.name}</code> ({c.severity}) — {c.message}
                    </li>
                  ))}
                </ul>
              ) : null}
            </article>
          );
        })}
      </div>
    </section>
  );
}
