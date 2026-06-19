// Wave 19 — `useActiveMarket(symbols)` hook.
//
// Resolves the operator's currently selected market from (in
// priority order):
//   1. `?market=<symbol>` URL query string.
//   2. `localStorage["mole.activeMarket"]`.
//   3. The first symbol in `symbols`.
//
// On change, both the URL and localStorage are updated so deep
// links share state and reloads remember the selection.
//
// The hook returns `[active, setActive]` where `active` is always
// a member of `symbols` (the hook guards against stale persisted
// values that no longer correspond to a configured market).

import { useCallback, useEffect, useState } from "react";

const STORAGE_KEY = "mole.activeMarket";
const URL_PARAM = "market";

/**
 * Pure resolver — given the configured symbols, the URL query
 * string, and the persisted value, return the symbol the panels
 * should render. Exported for unit tests.
 */
export function resolveActiveMarket(
  symbols: string[],
  url: string | null,
  stored: string | null,
): string {
  if (symbols.length === 0) return "";
  if (url && symbols.includes(url)) return url;
  if (stored && symbols.includes(stored)) return stored;
  return symbols[0]!;
}

function readUrlMarket(): string | null {
  if (typeof window === "undefined") return null;
  try {
    const params = new URLSearchParams(window.location.search);
    return params.get(URL_PARAM);
  } catch {
    return null;
  }
}

function readStoredMarket(): string | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage.getItem(STORAGE_KEY);
  } catch {
    return null;
  }
}

function writePersisted(symbol: string): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, symbol);
  } catch {
    // Ignore quota / private-mode errors — URL param still works.
  }
  try {
    const params = new URLSearchParams(window.location.search);
    params.set(URL_PARAM, symbol);
    const url = `${window.location.pathname}?${params.toString()}${window.location.hash}`;
    window.history.replaceState(window.history.state, "", url);
  } catch {
    // Ignore — non-browser env.
  }
}

export function useActiveMarket(
  symbols: string[],
): [string, (symbol: string) => void] {
  const [active, setActive] = useState<string>(() =>
    resolveActiveMarket(symbols, readUrlMarket(), readStoredMarket()),
  );
  // If the configured symbol list changes (rare — only on
  // reload), re-resolve so we don't stay on a stale symbol.
  useEffect(() => {
    setActive((prev) => {
      const next = resolveActiveMarket(
        symbols,
        readUrlMarket(),
        readStoredMarket(),
      );
      return symbols.includes(prev) ? prev : next;
    });
  }, [symbols]);
  const update = useCallback(
    (symbol: string) => {
      if (!symbols.includes(symbol)) return;
      setActive(symbol);
      writePersisted(symbol);
    },
    [symbols],
  );
  return [active, update];
}
