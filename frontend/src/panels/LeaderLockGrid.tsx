// Wave 18 — multi-market keeper-leader-lock status grid.
//
// The wave-16 `LeaderLockBanner` displays ONE row for ONE market.
// Wave-18 deployments supervise N markets from one console; the
// operator wants to see all leaders at a glance, plus catch the
// case where one market's heartbeat is stalling while the others
// are healthy.
//
// `LeaderLockGrid` renders a compact table:
//
//   | Market    | Status  | Holder      | Slots         |
//   |-----------|---------|-------------|---------------|
//   | SOL-USD   | fresh   | abcd…1234   | 38 left       |
//   | BTC-USD   | stale   | dead…beef   | 12 overdue    |
//   | ETH-USD   | unowned | —           | —             |
//
// Status semantics match the wave-16 banner exactly (uninitialised
// / unowned / fresh / stale) so existing operator runbooks (KL-01..
// KL-08 in `Docs/Planning/24-operator-runbook.md`) carry over.
//
// The grid renders synchronously from a `MultiMarketView`; the
// caller is responsible for decoding `lockBytes` via wasm and
// supplying the cluster slot. We keep that wiring in `App.tsx`
// (one place that already knows about wasm) so this component
// stays a pure presentation layer.

import type { JSX } from "react";

import type { KeeperLeaderLockView } from "../tx/wasmBuilder";
import type { MultiMarketView } from "../types";
import {
  deriveLeaderLockState,
  shortenHex,
  type LeaderLockState,
} from "./LeaderLockBanner";

/** Wave 18 — one row in the grid (decoded view + state). */
export interface LeaderLockGridRow {
  symbol: string;
  state: LeaderLockState;
  /** Set when the operator-supplied `expectedLeader` mismatches. */
  expectedMismatch?: boolean;
  /** Operator's expected leader (32-byte hex, no `0x`). */
  expectedHex?: string;
}

/**
 * Wave 18 — pure helper: compute one row per market entry.
 *
 * `decode` accepts the optional wasm decoder so non-test callers
 * can pass `decodeKeeperLeaderLockBytes` directly. Tests can
 * inject a deterministic decoder to avoid pulling wasm into the
 * unit-test environment.
 *
 * @param view  `FeedSnapshot.marketsView`. `undefined` produces an
 *              empty array.
 * @param currentSlot  Cluster slot used by `deriveLeaderLockState`.
 * @param decode  Decoder for raw lock bytes → typed view.
 * @param expected  Optional map `symbol → expected_leader hex` so
 *              the grid can flag holder mismatches per market.
 */
export function computeLeaderLockGridRows(
  view: MultiMarketView | undefined,
  currentSlot: bigint,
  decode: (bytes: Uint8Array) => KeeperLeaderLockView,
  expected?: Map<string, string>,
): LeaderLockGridRow[] {
  if (!view) return [];
  const rows: LeaderLockGridRow[] = [];
  // Stable, alphabetic order so two ticks render in the same DOM
  // order even if the underlying Map mutates internally.
  const symbols = Array.from(view.entries.keys()).sort();
  for (const symbol of symbols) {
    const entry = view.entries.get(symbol);
    if (!entry) continue;
    let lockView: KeeperLeaderLockView | null = null;
    if (entry.lockBytes !== undefined) {
      try {
        lockView = decode(entry.lockBytes);
      } catch (e) {
        // Decode failure: render as uninitialised so the grid
        // doesn't crash. Surface the failure via console.error so
        // operators see the schema-drift signal in devtools.
        console.error(
          `[mole/frontend] LeaderLockGrid: decode failed for ${symbol}:`,
          e,
        );
      }
    }
    const state = deriveLeaderLockState(lockView, currentSlot);
    const row: LeaderLockGridRow = { symbol, state };
    const expectedHex = expected?.get(symbol);
    if (expectedHex !== undefined && expectedHex.length > 0) {
      row.expectedHex = expectedHex;
      if (state.kind === "fresh" || state.kind === "stale") {
        row.expectedMismatch =
          state.holderHex.toLowerCase() !== expectedHex.toLowerCase();
      }
    }
    rows.push(row);
  }
  return rows;
}

interface Props {
  /** Multi-market view from `FeedSnapshot.marketsView`. */
  view: MultiMarketView | undefined;
  /** Current cluster slot (same value the banner uses). */
  currentSlot: bigint;
  /** wasm decoder for the 57-byte `KeeperLeaderLock` payload. */
  decode: (bytes: Uint8Array) => KeeperLeaderLockView;
  /** Optional `symbol → expected_leader hex` map. */
  expected?: Map<string, string>;
}

/**
 * Wave 18 — multi-market leader-lock status grid. Replaces the
 * wave-16 `LeaderLockBanner` for multi-market consoles; the banner
 * remains the default for single-market embeds.
 */
export function LeaderLockGrid({
  view,
  currentSlot,
  decode,
  expected,
}: Props): JSX.Element {
  const rows = computeLeaderLockGridRows(view, currentSlot, decode, expected);
  if (rows.length === 0) {
    return (
      <div
        className="leader-lock-grid leader-lock-grid-empty"
        role="status"
        aria-live="polite"
      >
        <span className="leader-lock-label">Keeper leaders</span>
        <span className="leader-lock-body">no markets configured</span>
      </div>
    );
  }
  return (
    <div className="leader-lock-grid" role="status" aria-live="polite">
      <div className="leader-lock-grid-header">
        <span className="leader-lock-label">Keeper leaders</span>
        <span className="leader-lock-grid-count">{rows.length} markets</span>
      </div>
      <table className="leader-lock-grid-table">
        <thead>
          <tr>
            <th scope="col">Market</th>
            <th scope="col">Status</th>
            <th scope="col">Holder</th>
            <th scope="col">Slots</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <RowView key={row.symbol} row={row} />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function RowView({ row }: { row: LeaderLockGridRow }): JSX.Element {
  const cls = `leader-lock-row leader-lock-row-${row.state.kind}${
    row.expectedMismatch ? " leader-lock-row-mismatch" : ""
  }`;
  return (
    <tr className={cls}>
      <td className="leader-lock-symbol">{row.symbol}</td>
      <td className="leader-lock-status">
        {row.state.kind}
        {row.expectedMismatch && (
          <span
            className="leader-lock-mismatch-badge"
            title={`expected holder ${shortenHex(row.expectedHex ?? "")}`}
          >
            {" "}
            ⚠ mismatch
          </span>
        )}
      </td>
      <td className="leader-lock-holder">{renderHolder(row.state)}</td>
      <td className="leader-lock-slots">{renderSlots(row.state)}</td>
    </tr>
  );
}

function renderHolder(state: LeaderLockState): JSX.Element {
  switch (state.kind) {
    case "fresh":
    case "stale":
      return <code>{shortenHex(state.holderHex)}</code>;
    case "uninitialised":
    case "unowned":
      return <span className="leader-lock-empty">—</span>;
  }
}

function renderSlots(state: LeaderLockState): JSX.Element {
  switch (state.kind) {
    case "fresh":
      return (
        <span className="leader-lock-countdown">
          {state.slotsUntilStale.toString()} left
        </span>
      );
    case "stale":
      return (
        <span className="leader-lock-countdown leader-lock-countdown-overdue">
          {state.slotsOverdue.toString()} overdue
        </span>
      );
    case "uninitialised":
    case "unowned":
      return <span className="leader-lock-empty">—</span>;
  }
}
