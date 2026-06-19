// Wave 22 — poll keeper-bot `/metrics-multi` and expose per-market
// `KeeperState` for App.tsx to merge into the live feed.

import { useEffect, useState } from "react";

import type { KeeperState } from "./types";
import {
  keeperStateMapFromMetricsMulti,
  parseMetricsMultiJson,
} from "./feed/keeperMetricsMulti";

const DEFAULT_POLL_MS = 4_000;

function readMetricsBaseUrl(): string | null {
  const env = (import.meta as unknown as { env?: Record<string, string | undefined> })
    .env;
  const raw = env?.VITE_KEEPER_METRICS_URL?.trim();
  if (!raw) return null;
  return raw.replace(/\/+$/, "");
}

/**
 * Poll `GET {VITE_KEEPER_METRICS_URL}/metrics-multi` on an interval.
 * Returns `null` when the env var is unset (mock / offline dev).
 */
export function useKeeperMetricsMulti(
  pollMs = DEFAULT_POLL_MS,
): Map<string, KeeperState> | null {
  const baseUrl = readMetricsBaseUrl();
  const [byMarket, setByMarket] = useState<Map<string, KeeperState> | null>(
    null,
  );

  useEffect(() => {
    if (!baseUrl) {
      setByMarket(null);
      return;
    }
    let cancelled = false;

    const tick = async () => {
      try {
        const resp = await fetch(`${baseUrl}/metrics-multi`, {
          method: "GET",
          headers: { Accept: "application/json" },
        });
        if (!resp.ok) return;
        const text = await resp.text();
        const entries = parseMetricsMultiJson(text);
        if (!cancelled) {
          setByMarket(keeperStateMapFromMetricsMulti(entries));
        }
      } catch (e) {
        console.warn("[mole/frontend] metrics-multi poll failed —", e);
      }
    };

    void tick();
    const id = setInterval(() => {
      void tick();
    }, pollMs);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [baseUrl, pollMs]);

  return baseUrl ? byMarket : null;
}
