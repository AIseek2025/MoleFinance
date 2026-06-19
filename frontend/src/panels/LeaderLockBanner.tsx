// Wave 16 — KeeperLeaderLock status banner.
//
// Renders the wave-15 on-chain `KeeperLeaderLock` PDA decoded by
// `wasmBuilder.decodeKeeperLeaderLockBytes`. The banner answers
// three operator-facing questions in one glance:
//
//   1. Is the lock initialised? (i.e. has ops sent
//      `initialize_keeper_leader_lock` on this market yet?)
//   2. Who currently holds it? (Pubkey, truncated for readability.)
//   3. How fresh is the heartbeat? (slots since last heartbeat vs
//      `takeoverThresholdSlots` — when the elapsed slot count
//      exceeds the threshold, any keeper can take over.)
//
// Why this exists in the frontend:
//
// - Production deployments run multiple keeper replicas (wave 15+).
//   Operators need a single place to confirm "the bot I expect to
//   be leader is actually leader, and its heartbeat hasn't stalled".
// - Read-only endpoint — the banner subscribes to the on-chain PDA
//   via the same wave-14 `WebSocketFeedAdapter` that drives the
//   trader / keeper panels. No additional RPC.
// - The 49-byte body decode lives in Rust (wave-15 wasm) so the TS
//   surface is just a presentation layer — schema drift surfaces
//   as a wasm decode error, not a silent UI miscount.

import type { JSX } from "react";

import type { KeeperLeaderLockView } from "../tx/wasmBuilder";

/// Visual state computed from the decoded `KeeperLeaderLock`.
export type LeaderLockState =
  | { kind: "uninitialised" }
  | { kind: "fresh"; holderHex: string; slotsUntilStale: bigint }
  | { kind: "stale"; holderHex: string; slotsOverdue: bigint }
  | { kind: "unowned" };

/**
 * Wave 16 — derive the visual state from the wave-15 decoded view +
 * the current slot. Pure function so vitest can pin the matrix.
 *
 * Predicates:
 *
 * - `unowned` — `hasLeader === false`. The lock was initialised but
 *   the previous holder released gracefully (`keeper_leader_release`)
 *   or hasn't acquired yet.
 * - `fresh` — `hasLeader === true` AND
 *   `currentSlot - lastHeartbeatSlot <= takeoverThresholdSlots`.
 *   Holder is healthy.
 * - `stale` — `hasLeader === true` AND
 *   `currentSlot - lastHeartbeatSlot > takeoverThresholdSlots`.
 *   Anyone may take over via `keeper_leader_acquire`.
 *
 * @param view  Decoded `KeeperLeaderLock`. `null` means the PDA
 *              hasn't been initialised on this market yet.
 * @param currentSlot  Cluster slot. Caller can pass either the bot's
 *              `getSlot()` reading or `max(sub_pool.last_sync_slot)`
 *              from the same `WebSocketFeedAdapter` snapshot.
 */
export function deriveLeaderLockState(
  view: KeeperLeaderLockView | null,
  currentSlot: bigint,
): LeaderLockState {
  if (view === null) return { kind: "uninitialised" };
  if (!view.hasLeader) return { kind: "unowned" };
  const holder = view.currentLeader;
  const holderHex = bytesToHex(holder);
  const elapsed = currentSlot >= view.lastHeartbeatSlot
    ? currentSlot - view.lastHeartbeatSlot
    : 0n;
  if (elapsed > view.takeoverThresholdSlots) {
    return {
      kind: "stale",
      holderHex,
      slotsOverdue: elapsed - view.takeoverThresholdSlots,
    };
  }
  return {
    kind: "fresh",
    holderHex,
    slotsUntilStale: view.takeoverThresholdSlots - elapsed,
  };
}

/// Truncate a 32-byte hex string to `XXXX…YYYY` for display.
export function shortenHex(hex: string): string {
  if (hex.length <= 12) return hex;
  return `${hex.slice(0, 6)}…${hex.slice(-6)}`;
}

function bytesToHex(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 1) {
    const b = bytes[i] ?? 0;
    s += b.toString(16).padStart(2, "0");
  }
  return s;
}

interface Props {
  /** Decoded `KeeperLeaderLock`. `null` means the PDA isn't on chain yet. */
  view: KeeperLeaderLockView | null;
  /** Current cluster slot. */
  currentSlot: bigint;
}

/**
 * Wave 16 — operator-facing leader-lock status badge. Sits above
 * the `KeeperPanel` / `TraderPanel` so multi-replica deployments
 * can confirm leadership at a glance.
 */
export function LeaderLockBanner({ view, currentSlot }: Props): JSX.Element {
  const state = deriveLeaderLockState(view, currentSlot);
  const cls = `leader-lock leader-lock-${state.kind}`;
  return (
    <div className={cls} role="status" aria-live="polite">
      <span className="leader-lock-label">Keeper leader</span>
      <span className="leader-lock-body">{render(state)}</span>
    </div>
  );
}

function render(state: LeaderLockState): JSX.Element {
  switch (state.kind) {
    case "uninitialised":
      return (
        <>
          <span className="leader-lock-status">uninitialised</span>
          <span className="leader-lock-detail">
            ops must send <code>initialize_keeper_leader_lock</code>
          </span>
        </>
      );
    case "unowned":
      return (
        <>
          <span className="leader-lock-status">unowned</span>
          <span className="leader-lock-detail">no keeper currently holds this lock</span>
        </>
      );
    case "fresh":
      return (
        <>
          <span className="leader-lock-status">fresh</span>
          <span className="leader-lock-detail">
            held by <code>{shortenHex(state.holderHex)}</code>
            {" · "}
            <span className="leader-lock-countdown">
              {state.slotsUntilStale.toString()} slots until stale
            </span>
          </span>
        </>
      );
    case "stale":
      return (
        <>
          <span className="leader-lock-status">stale</span>
          <span className="leader-lock-detail">
            holder <code>{shortenHex(state.holderHex)}</code> overdue by{" "}
            <span className="leader-lock-countdown">
              {state.slotsOverdue.toString()} slots
            </span>
            {" · standby keeper may acquire"}
          </span>
        </>
      );
  }
}
