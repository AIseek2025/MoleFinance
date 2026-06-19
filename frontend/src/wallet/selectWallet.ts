// Wave 30 — wallet adapter selection.
//
// Decides which `WalletAdapter` the app runs with:
//   - `?wallet=mock`  → always the offline MockWalletAdapter (demos / CI)
//   - `?wallet=phantom|backpack|solflare` → force that wallet if installed
//   - `?wallet=auto` (default) → the highest-priority installed wallet,
//     falling back to the mock adapter when no extension is present so
//     `npm run dev` keeps working with zero browser wallet.
//
// Pure + injectable: pass `search` / `win` for unit tests.

import type { WalletAdapter } from "./adapter";
import { MockWalletAdapter } from "./mockWalletAdapter";
import { WindowWalletAdapter } from "./windowWalletAdapter";
import {
  pickPreferredWallet,
  type WalletName,
  type WalletWindow,
} from "./discoverWallets";

export type WalletPreference = WalletName | "auto" | "mock";

const VALID: readonly WalletPreference[] = [
  "auto",
  "mock",
  "phantom",
  "backpack",
  "solflare",
];

/** Parse the `?wallet=` URL param into a preference (default `auto`). */
export function walletPreferenceFromUrl(search?: string): WalletPreference {
  let raw: string | null = null;
  if (search !== undefined) {
    raw = new URLSearchParams(search).get("wallet");
  } else if (typeof window !== "undefined") {
    raw = new URLSearchParams(window.location.search).get("wallet");
  }
  const v = (raw ?? "auto").toLowerCase();
  return (VALID as readonly string[]).includes(v)
    ? (v as WalletPreference)
    : "auto";
}

export interface SelectWalletOptions {
  preference?: WalletPreference;
  win?: WalletWindow;
}

/**
 * Build the wallet adapter for this session. Returns a real
 * `WindowWalletAdapter` bound to the discovered provider when a wallet
 * is available (and not forced to mock), else the `MockWalletAdapter`.
 */
export function selectWalletAdapter(opts?: SelectWalletOptions): WalletAdapter {
  const preference = opts?.preference ?? walletPreferenceFromUrl();
  if (preference === "mock") return new MockWalletAdapter();
  const picked = pickPreferredWallet(preference, opts?.win);
  if (!picked) return new MockWalletAdapter();
  return new WindowWalletAdapter({ provider: picked.provider });
}
