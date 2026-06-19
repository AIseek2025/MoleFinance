/**
 * Wave 30 — wallet discovery + selection unit tests.
 *
 * Pure window-injection: we pass fake `WalletWindow` objects describing
 * which providers are present at which keys, so the multi-wallet
 * detection logic runs without a browser.
 *
 * @vitest-environment node
 */
import { describe, expect, it } from "vitest";

import {
  discoverWallets,
  pickPreferredWallet,
  type WalletWindow,
} from "./discoverWallets";
import {
  selectWalletAdapter,
  walletPreferenceFromUrl,
} from "./selectWallet";
import { WindowWalletAdapter } from "./windowWalletAdapter";
import { MockWalletAdapter } from "./mockWalletAdapter";
import type { WindowSolanaProvider } from "./windowWalletAdapter";

const phantom: WindowSolanaProvider = { isPhantom: true };
const backpack: WindowSolanaProvider = { isBackpack: true };
const solflare: WindowSolanaProvider = { isSolflare: true };

describe("discoverWallets", () => {
  it("finds wallets at their own injection keys", () => {
    const win: WalletWindow = {
      phantom: { solana: phantom },
      backpack,
      solflare,
    };
    expect(discoverWallets(win).map((w) => w.name)).toEqual([
      "phantom",
      "backpack",
      "solflare",
    ]);
  });

  it("falls back to the legacy window.solana alias with the matching flag", () => {
    const win: WalletWindow = { solana: backpack };
    const found = discoverWallets(win);
    expect(found).toHaveLength(1);
    expect(found[0]!.name).toBe("backpack");
    expect(found[0]!.provider).toBe(backpack);
  });

  it("ignores window.solana without a recognised is<Wallet> flag", () => {
    const win: WalletWindow = { solana: {} };
    expect(discoverWallets(win)).toEqual([]);
  });

  it("returns [] when no wallet is installed", () => {
    expect(discoverWallets({})).toEqual([]);
  });

  it("does not double-count a single provider aliased to window.solana", () => {
    // Phantom historically set BOTH window.phantom.solana and
    // window.solana to the same object.
    const win: WalletWindow = { phantom: { solana: phantom }, solana: phantom };
    expect(discoverWallets(win).map((w) => w.name)).toEqual(["phantom"]);
  });
});

describe("pickPreferredWallet", () => {
  const win: WalletWindow = {
    phantom: { solana: phantom },
    backpack,
  };

  it("honours an explicit preference when installed", () => {
    expect(pickPreferredWallet("backpack", win)?.name).toBe("backpack");
  });

  it("falls back to highest priority when preference absent", () => {
    expect(pickPreferredWallet("solflare", win)?.name).toBe("phantom");
  });

  it("auto picks the highest-priority installed wallet", () => {
    expect(pickPreferredWallet("auto", win)?.name).toBe("phantom");
  });

  it("returns null when nothing is installed", () => {
    expect(pickPreferredWallet("auto", {})).toBeNull();
  });
});

describe("walletPreferenceFromUrl", () => {
  it("defaults to auto", () => {
    expect(walletPreferenceFromUrl("")).toBe("auto");
    expect(walletPreferenceFromUrl("?foo=bar")).toBe("auto");
  });

  it("parses a valid wallet name", () => {
    expect(walletPreferenceFromUrl("?wallet=backpack")).toBe("backpack");
    expect(walletPreferenceFromUrl("?wallet=mock")).toBe("mock");
  });

  it("rejects an unknown value back to auto", () => {
    expect(walletPreferenceFromUrl("?wallet=ledgerXYZ")).toBe("auto");
  });
});

describe("selectWalletAdapter", () => {
  const win: WalletWindow = { phantom: { solana: phantom }, backpack };

  it("returns a real WindowWalletAdapter for an installed wallet", () => {
    const a = selectWalletAdapter({ preference: "auto", win });
    expect(a).toBeInstanceOf(WindowWalletAdapter);
    expect(a.name).toBe("phantom");
  });

  it("honours a specific wallet preference", () => {
    const a = selectWalletAdapter({ preference: "backpack", win });
    expect(a).toBeInstanceOf(WindowWalletAdapter);
    expect(a.name).toBe("backpack");
  });

  it("forces the mock adapter with preference=mock", () => {
    const a = selectWalletAdapter({ preference: "mock", win });
    expect(a).toBeInstanceOf(MockWalletAdapter);
  });

  it("falls back to mock when no wallet is installed", () => {
    const a = selectWalletAdapter({ preference: "auto", win: {} });
    expect(a).toBeInstanceOf(MockWalletAdapter);
  });
});
