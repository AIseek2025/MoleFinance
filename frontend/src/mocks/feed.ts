// Wave 12 — re-export shim. The mock feed logic moved to
// `src/feed/mockGenerator.ts`; this file remains only so existing
// imports keep compiling. Wave 13 will delete this file.

import { useMemo } from "react";

import type { FeedSnapshot } from "../types";
import {
  FEED_FAKE_MARKET_PDA,
  FEED_FAKE_PROGRAM_ID,
  FEED_TICK_INTERVAL_MS,
  MockFeedAdapter,
  useFeed,
} from "../feed";

const VOL_WARM_UP_TICKS = 32;

/**
 * Drop-in replacement for the wave-11 hook. Always returns a non-
 * null `FeedSnapshot` because the `MockFeedAdapter` emits an initial
 * synchronous snapshot.
 */
export function useMockFeed(): FeedSnapshot {
  const adapter = useMemo(() => new MockFeedAdapter(), []);
  const { snapshot } = useFeed(adapter);
  if (!snapshot) {
    throw new Error("useMockFeed: adapter produced no initial snapshot (logic error)");
  }
  return snapshot;
}

export const FEED_CONSTANTS = {
  PROGRAM_ID: FEED_FAKE_PROGRAM_ID,
  MARKET_PDA: FEED_FAKE_MARKET_PDA,
  TICK_INTERVAL_MS: FEED_TICK_INTERVAL_MS,
  VOL_WARM_UP_TICKS,
};
