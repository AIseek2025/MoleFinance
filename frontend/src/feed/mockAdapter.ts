// Wave 12 — Mock FeedAdapter that wraps the existing deterministic
// snapshot generator.
//
// This is the single source of mock data; previously the logic
// lived in `mocks/feed.ts` as a hook. Wave 12 splits the data
// generation (here) from the React adapter (`useFeed`) so the same
// generator can be unit-tested without a DOM.

import type { FeedSnapshot } from "../types";
import type { FeedAdapter, FeedStatus } from "./adapter";
import { buildSnapshot, FEED_TICK_INTERVAL_MS } from "./mockGenerator";

/**
 * Deterministic mock adapter — emits a fresh `FeedSnapshot` every
 * `FEED_TICK_INTERVAL_MS`, advancing an internal tick counter.
 */
export class MockFeedAdapter implements FeedAdapter {
  readonly kind = "mock" as const;
  private current: FeedStatus = "idle";

  status(): FeedStatus {
    return this.current;
  }

  start(onSnapshot: (snapshot: FeedSnapshot) => void): () => void {
    this.current = "connecting";
    let tickIndex = 0;
    let prevCumulative = { submitted: 0, failed: 0, skipped: 0 };
    // Emit one synchronous initial snapshot so the UI doesn't
    // render an empty state.
    const first = buildSnapshot(0, prevCumulative);
    prevCumulative = first.keeper.metrics.cumulative;
    onSnapshot(first);
    this.current = "connected";

    const id = setInterval(() => {
      tickIndex += 1;
      const next = buildSnapshot(tickIndex, prevCumulative);
      prevCumulative = next.keeper.metrics.cumulative;
      onSnapshot(next);
    }, FEED_TICK_INTERVAL_MS);

    return () => {
      clearInterval(id);
      this.current = "idle";
    };
  }
}
