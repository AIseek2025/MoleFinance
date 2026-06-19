// Wave 26 — poll the ops-toolkit prober's JSON snapshot and expose a
// typed `ProberSnapshot` for App.tsx to render in the health panel.
// Returns `null` when `VITE_PROBER_SNAPSHOT_URL` is unset (mock /
// offline dev) so the panel hides itself instead of erroring.

import { useEffect, useState } from "react";

import { parseProberSnapshot, type ProberSnapshot } from "./feed/proberSnapshot";

const DEFAULT_POLL_MS = 5_000;

function readSnapshotUrl(): string | null {
  const env = (import.meta as unknown as { env?: Record<string, string | undefined> })
    .env;
  const raw = env?.VITE_PROBER_SNAPSHOT_URL?.trim();
  if (!raw) return null;
  return raw;
}

/**
 * Poll `GET {VITE_PROBER_SNAPSHOT_URL}` on an interval. The URL points
 * at the JSON file the `ops-toolkit prober` daemon writes each cycle
 * (typically served by the ops node-exporter sidecar). The last-good
 * snapshot is retained across transient fetch / parse failures.
 */
export function useProberSnapshot(
  pollMs = DEFAULT_POLL_MS,
): ProberSnapshot | null {
  const url = readSnapshotUrl();
  const [snapshot, setSnapshot] = useState<ProberSnapshot | null>(null);

  useEffect(() => {
    if (!url) {
      setSnapshot(null);
      return;
    }
    let cancelled = false;

    const tick = async () => {
      try {
        const resp = await fetch(url, {
          method: "GET",
          headers: { Accept: "application/json" },
          cache: "no-store",
        });
        if (!resp.ok) return;
        const text = await resp.text();
        const parsed = parseProberSnapshot(text);
        if (!cancelled) setSnapshot(parsed);
      } catch (e) {
        console.warn("[mole/frontend] prober snapshot poll failed —", e);
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
  }, [url, pollMs]);

  return url ? snapshot : null;
}
