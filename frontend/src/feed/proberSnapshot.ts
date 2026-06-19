// Wave 26 — parse the ops-toolkit prober's multi-market JSON snapshot
// (`render_json_multi` wire format written to disk / served over HTTP
// by the `ops-toolkit prober` daemon). The backend daemon now folds
// live per-market open-interest into the wave-24 `position_principal_drift`
// check; this module is the frontend mirror so the dashboard can surface
// the daemon's published health verdict — overall status per market plus
// the firing checks (drift highlighted) — without scraping Prometheus.

/** Health check status as emitted by `report.rs::CheckStatus::as_str`. */
export type ProberCheckStatus = "PASS" | "WARN" | "CRITICAL";

/** A single decoded health check from the prober snapshot. */
export interface ProberCheck {
  name: string;
  status: ProberCheckStatus;
  severity: string;
  message: string;
  measurements: Record<string, number>;
}

/** One market's health report inside the snapshot. */
export interface ProberMarketHealth {
  symbol: string;
  overallStatus: ProberCheckStatus;
  highestFiringSeverity: string;
  counts: { pass: number; warn: number; critical: number };
  checks: ProberCheck[];
}

/** The full multi-market prober snapshot. */
export interface ProberSnapshot {
  worstExitCode: number;
  markets: Map<string, ProberMarketHealth>;
}

/** The wave-24 reconciliation check name — surfaced prominently. */
export const DRIFT_CHECK_NAME = "position_principal_drift";

function asCheckStatus(v: unknown, where: string): ProberCheckStatus {
  if (v === "PASS" || v === "WARN" || v === "CRITICAL") return v;
  throw new Error(`prober snapshot: ${where} has invalid status ${String(v)}`);
}

function asRecord(v: unknown, where: string): Record<string, unknown> {
  if (typeof v !== "object" || v === null || Array.isArray(v)) {
    throw new Error(`prober snapshot: ${where} expected object`);
  }
  return v as Record<string, unknown>;
}

function asNumber(v: unknown, where: string): number {
  if (typeof v !== "number" || !Number.isFinite(v)) {
    throw new Error(`prober snapshot: ${where} expected finite number`);
  }
  return v;
}

function parseMeasurements(v: unknown): Record<string, number> {
  const out: Record<string, number> = {};
  if (v === undefined) return out;
  const obj = asRecord(v, "measurements");
  for (const [k, raw] of Object.entries(obj)) {
    if (typeof raw === "number" && Number.isFinite(raw)) out[k] = raw;
  }
  return out;
}

function parseCheck(v: unknown, where: string): ProberCheck {
  const obj = asRecord(v, where);
  const name = obj.name;
  if (typeof name !== "string" || name === "") {
    throw new Error(`prober snapshot: ${where} missing name`);
  }
  return {
    name,
    status: asCheckStatus(obj.status, `${where}.status`),
    severity: typeof obj.severity === "string" ? obj.severity : "?",
    message: typeof obj.message === "string" ? obj.message : "",
    measurements: parseMeasurements(obj.measurements),
  };
}

function parseMarket(symbol: string, v: unknown): ProberMarketHealth {
  const obj = asRecord(v, `markets.${symbol}`);
  const countsObj = asRecord(obj.counts, `markets.${symbol}.counts`);
  const checksRaw = obj.checks;
  if (!Array.isArray(checksRaw)) {
    throw new Error(`prober snapshot: markets.${symbol}.checks expected array`);
  }
  return {
    symbol,
    overallStatus: asCheckStatus(
      obj.overall_status,
      `markets.${symbol}.overall_status`,
    ),
    highestFiringSeverity:
      typeof obj.highest_firing_severity === "string"
        ? obj.highest_firing_severity
        : "NONE",
    counts: {
      pass: asNumber(countsObj.pass, `markets.${symbol}.counts.pass`),
      warn: asNumber(countsObj.warn, `markets.${symbol}.counts.warn`),
      critical: asNumber(
        countsObj.critical,
        `markets.${symbol}.counts.critical`,
      ),
    },
    checks: checksRaw.map((c, i) =>
      parseCheck(c, `markets.${symbol}.checks[${i}]`),
    ),
  };
}

/**
 * Parse the prober snapshot response body. Throws on malformed JSON or
 * a shape that doesn't match the `render_json_multi` contract, so the
 * caller can keep the last-good snapshot instead of rendering garbage.
 */
export function parseProberSnapshot(text: string): ProberSnapshot {
  const root = asRecord(JSON.parse(text), "root");
  const markets = asRecord(root.markets, "markets");
  const out = new Map<string, ProberMarketHealth>();
  for (const [symbol, raw] of Object.entries(markets)) {
    out.set(symbol, parseMarket(symbol, raw));
  }
  return {
    worstExitCode: asNumber(root.worst_exit_code, "worst_exit_code"),
    markets: out,
  };
}

/** Return the wave-24 drift check for a market, or `undefined`. */
export function driftCheckFor(
  market: ProberMarketHealth,
): ProberCheck | undefined {
  return market.checks.find((c) => c.name === DRIFT_CHECK_NAME);
}

/** Checks that are not passing (Warn or Critical), worst first. */
export function firingChecks(market: ProberMarketHealth): ProberCheck[] {
  const rank = (s: ProberCheckStatus): number =>
    s === "CRITICAL" ? 2 : s === "WARN" ? 1 : 0;
  return market.checks
    .filter((c) => c.status !== "PASS")
    .sort((a, b) => rank(b.status) - rank(a.status));
}

/** True when any market in the snapshot has a firing (non-PASS) status. */
export function snapshotHasFiring(snapshot: ProberSnapshot): boolean {
  for (const m of snapshot.markets.values()) {
    if (m.overallStatus !== "PASS") return true;
  }
  return false;
}
