// Wave 30 — browser wallet discovery.
//
// Phantom, Backpack, and Solflare inject their providers at DIFFERENT
// `window` keys (and historically all also aliased `window.solana`).
// A production app must enumerate the installed wallets so the user can
// pick one, rather than blindly grabbing `window.solana`. This module
// scans the known injection points and returns the installed wallets in
// a stable priority order.
//
// It is pure + window-injectable so it unit-tests without a browser:
// pass a fake `win` object describing which providers are present.

import type { WindowSolanaProvider } from "./windowWalletAdapter";

export type WalletName = "phantom" | "backpack" | "solflare";

export interface DiscoveredWallet {
  name: WalletName;
  provider: WindowSolanaProvider;
}

/** The shape of `window` we read provider injections from. */
export interface WalletWindow {
  phantom?: { solana?: WindowSolanaProvider };
  backpack?: WindowSolanaProvider;
  solflare?: WindowSolanaProvider;
  solana?: WindowSolanaProvider;
}

/** Stable priority — the order wallets appear in the picker / the one */
/** `pickPreferredWallet` returns when no explicit choice is made. */
const PRIORITY: readonly WalletName[] = ["phantom", "backpack", "solflare"];

function resolveWindow(win?: WalletWindow): WalletWindow | undefined {
  if (win) return win;
  if (typeof window === "undefined") return undefined;
  return window as unknown as WalletWindow;
}

/**
 * Locate the provider for a specific wallet, checking the wallet's own
 * injection key first, then the legacy `window.solana` alias guarded by
 * the matching `is<Wallet>` flag.
 */
function providerFor(
  win: WalletWindow,
  name: WalletName,
): WindowSolanaProvider | undefined {
  switch (name) {
    case "phantom": {
      const own = win.phantom?.solana;
      if (own?.isPhantom) return own;
      if (win.solana?.isPhantom) return win.solana;
      return undefined;
    }
    case "backpack": {
      if (win.backpack?.isBackpack) return win.backpack;
      if (win.solana?.isBackpack) return win.solana;
      return undefined;
    }
    case "solflare": {
      if (win.solflare?.isSolflare) return win.solflare;
      if (win.solana?.isSolflare) return win.solana;
      return undefined;
    }
  }
}

/**
 * Enumerate installed Solana wallets, in priority order. Each entry
 * carries the concrete provider so a `WindowWalletAdapter` can be
 * constructed against the user's chosen wallet.
 */
export function discoverWallets(win?: WalletWindow): DiscoveredWallet[] {
  const w = resolveWindow(win);
  if (!w) return [];
  const out: DiscoveredWallet[] = [];
  for (const name of PRIORITY) {
    const provider = providerFor(w, name);
    if (provider) out.push({ name, provider });
  }
  return out;
}

/**
 * Return the preferred wallet: the explicitly-requested one when it is
 * installed, otherwise the highest-priority installed wallet, otherwise
 * `null` (no wallet extension present).
 */
export function pickPreferredWallet(
  prefer?: WalletName | "auto",
  win?: WalletWindow,
): DiscoveredWallet | null {
  const wallets = discoverWallets(win);
  if (wallets.length === 0) return null;
  if (prefer && prefer !== "auto") {
    const match = wallets.find((w) => w.name === prefer);
    if (match) return match;
  }
  return wallets[0] ?? null;
}
