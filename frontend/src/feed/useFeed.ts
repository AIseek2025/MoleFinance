// Wave 12 — React hook that drives any FeedAdapter.

import { useEffect, useState } from "react";

import type { FeedSnapshot } from "../types";
import type { FeedAdapter, FeedStatus } from "./adapter";

export interface UseFeedResult {
  snapshot: FeedSnapshot | null;
  status: FeedStatus;
  adapterKind: FeedAdapter["kind"];
}

/**
 * Drive the supplied adapter. The hook re-binds whenever the
 * adapter reference changes — keep the adapter in a `useMemo` so
 * you don't accidentally restart on every render.
 */
export function useFeed(adapter: FeedAdapter): UseFeedResult {
  const [snapshot, setSnapshot] = useState<FeedSnapshot | null>(null);
  const [status, setStatus] = useState<FeedStatus>(adapter.status());

  useEffect(() => {
    const stop = adapter.start((snap) => {
      setSnapshot(snap);
      setStatus(adapter.status());
    });
    setStatus(adapter.status());
    const id = setInterval(() => setStatus(adapter.status()), 500);
    return () => {
      stop();
      clearInterval(id);
    };
  }, [adapter]);

  return { snapshot, status, adapterKind: adapter.kind };
}
