// Wave 12 — FeedAdapter abstraction.
//
// The feed is a stream of `FeedSnapshot` values, refreshed at some
// cadence. Today we have one implementation (a deterministic mock);
// wave 13 adds a websocket-backed live implementation. The panels
// don't care which is wired — they consume the same shape.
//
// ## Why an interface, not a hook
//
// `useMockFeed` was originally a hook. Wrapping it in an interface
// gives us:
//   - separation of concerns (the network protocol vs. the React
//     state machine)
//   - testability without a DOM (FeedAdapter is plain TS, no React)
//   - swappability via URL param (`?feed=live`)
//
// ## Lifecycle contract
//
//   const stop = adapter.start(snap => setSnapshot(snap));
//   // ... eventually
//   stop();
//
// `start` MUST return a synchronous `stop` function — React effects
// expect the cleanup to execute synchronously when the component
// unmounts. Adapters with async teardown (e.g. closing a websocket)
// fire and forget the close, then resolve the synchronous portion
// immediately.

import type { FeedSnapshot } from "../types";

export type FeedStatus =
  | "idle"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "error";

export interface FeedAdapter {
  /** Stable identifier for the adapter, surfaced in the topbar. */
  readonly kind: "mock" | "websocket";

  /** Latest observed status (read-only — adapters update it internally). */
  status(): FeedStatus;

  /** Start emitting snapshots to `onSnapshot`. Returns a stop fn. */
  start(onSnapshot: (snapshot: FeedSnapshot) => void): () => void;
}

/** Convenience: detect requested adapter from the URL `?feed=` query. */
export function adapterKindFromUrl(): "mock" | "websocket" {
  if (typeof window === "undefined") return "mock";
  const params = new URLSearchParams(window.location.search);
  const requested = params.get("feed");
  if (requested === "live" || requested === "ws" || requested === "websocket") {
    return "websocket";
  }
  if (requested === "mock") {
    return "mock";
  }
  const env = (import.meta as unknown as { env?: Record<string, string | undefined> })
    .env;
  const defaultFeed = env?.VITE_DEFAULT_FEED?.trim().toLowerCase();
  if (
    defaultFeed === "live" ||
    defaultFeed === "ws" ||
    defaultFeed === "websocket"
  ) {
    return "websocket";
  }
  return "mock";
}
