// Wave 14 — real Window-based WalletAdapter.
//
// Wave 12 introduced this skeleton; wave 14 wires `signAndSubmit` to
// the actual `window.solana.signAndSendTransaction` API, with full
// error mapping (user-rejected, no-Tx-bytes, wallet-disconnected,
// generic provider error) so the trader panel can surface
// actionable messages.
//
// The wallet provider surface here matches Phantom / Backpack /
// Solflare's de-facto API:
//   - `connect()` returns `{ publicKey: { toBytes(), toString() } }`
//   - `disconnect()` returns `void`
//   - `signAndSendTransaction(serializedTx?: Uint8Array)` returns
//     `{ signature: string }`. Some wallets accept a `VersionedTransaction`
//     instance; we keep the surface byte-buffer-driven so it works
//     with the Borsh-encoded payloads the wave-15 wasm tx-builder
//     produces. When the wallet doesn't expose a serialised-bytes
//     overload we fall back to passing the bytes directly.

import type { Pubkey32 } from "../types";
import type { UnsignedTxDraft, WalletAdapter, WalletStatus } from "./adapter";

export interface WindowSolanaSignResult {
  /** Base58 transaction signature returned by the wallet. */
  signature: string;
  /** Some wallets echo the public key — ignored. */
  publicKey?: unknown;
}

export interface WindowSolanaProvider {
  isPhantom?: boolean;
  isBackpack?: boolean;
  isSolflare?: boolean;
  publicKey?: { toBytes?: () => Uint8Array; toString?: () => string };
  connect?: (opts?: {
    onlyIfTrusted?: boolean;
  }) => Promise<{ publicKey?: { toBytes?: () => Uint8Array } }>;
  disconnect?: () => Promise<void>;
  /**
   * Phantom / Backpack / Solflare entry-point. Receives a
   * Uint8Array of serialized transaction bytes and returns a
   * signature on success, throws on user rejection or RPC error.
   */
  signAndSendTransaction?: (
    serializedTx: Uint8Array,
  ) => Promise<WindowSolanaSignResult>;
}

interface MaybeWindow {
  solana?: WindowSolanaProvider;
}

function getMaybeSolana(): WindowSolanaProvider | undefined {
  if (typeof window === "undefined") return undefined;
  return (window as unknown as MaybeWindow).solana;
}

function detectName(s: WindowSolanaProvider): "phantom" | "backpack" | "solflare" {
  if (s.isBackpack) return "backpack";
  if (s.isSolflare) return "solflare";
  return "phantom";
}

function bytesToHex(bytes: Uint8Array): string {
  let hex = "";
  for (const b of bytes) {
    hex += b.toString(16).padStart(2, "0");
  }
  return hex;
}

/**
 * Errors surfaced by `WindowWalletAdapter.signAndSubmit`. Trader
 * panels pattern-match on `kind` to render the right banner without
 * leaking provider-specific message text.
 */
export class WalletSignError extends Error {
  readonly kind:
    | "WalletNotConnected"
    | "NoTxBytes"
    | "ProviderMissing"
    | "ProviderUnsupported"
    | "UserRejected"
    | "ProviderError";
  readonly cause?: unknown;
  constructor(kind: WalletSignError["kind"], message: string, cause?: unknown) {
    super(message);
    this.kind = kind;
    this.cause = cause;
  }
}

export class WindowWalletAdapter implements WalletAdapter {
  readonly name: "phantom" | "backpack" | "solflare";
  private currentStatus: WalletStatus = "disconnected";
  private currentPubkey: Pubkey32 | null = null;
  private readonly providerOverride: WindowSolanaProvider | undefined;

  constructor(opts?: { provider?: WindowSolanaProvider }) {
    this.providerOverride = opts?.provider;
    const s = this.resolveProvider();
    this.name = s ? detectName(s) : "phantom";
  }

  /** Test-only — exposes the resolved provider for debug assertions. */
  protected resolveProvider(): WindowSolanaProvider | undefined {
    return this.providerOverride ?? getMaybeSolana();
  }

  status(): WalletStatus {
    return this.currentStatus;
  }

  pubkey(): Pubkey32 | null {
    return this.currentPubkey;
  }

  async connect(): Promise<Pubkey32> {
    const s = this.resolveProvider();
    if (!s || !s.connect) {
      this.currentStatus = "error";
      throw new WalletSignError(
        "ProviderMissing",
        "WindowWalletAdapter: no window.solana detected. Install Phantom / Backpack / Solflare.",
      );
    }
    this.currentStatus = "connecting";
    let r;
    try {
      r = await s.connect();
    } catch (e) {
      this.currentStatus = "error";
      throw new WalletSignError(
        "ProviderError",
        "WindowWalletAdapter.connect threw",
        e,
      );
    }
    const bytes = r.publicKey?.toBytes?.();
    if (!bytes || bytes.length !== 32) {
      this.currentStatus = "error";
      throw new WalletSignError(
        "ProviderError",
        "WindowWalletAdapter.connect returned no 32-byte public key",
      );
    }
    const pk: Pubkey32 = { hex: bytesToHex(bytes) };
    this.currentPubkey = pk;
    this.currentStatus = "connected";
    return pk;
  }

  async disconnect(): Promise<void> {
    const s = this.resolveProvider();
    if (s?.disconnect) {
      try {
        await s.disconnect();
      } catch (e) {
        console.warn("[mole/frontend] window wallet disconnect failed:", e);
      }
    }
    this.currentStatus = "disconnected";
    this.currentPubkey = null;
  }

  async signAndSubmit(draft: UnsignedTxDraft): Promise<string> {
    if (this.currentStatus !== "connected") {
      throw new WalletSignError(
        "WalletNotConnected",
        "WindowWalletAdapter: not connected — call connect() first",
      );
    }
    if (!draft.borshBytes || draft.borshBytes.length === 0) {
      throw new WalletSignError(
        "NoTxBytes",
        "WindowWalletAdapter: no transaction bytes — wave-15 wasm tx-builder still TODO",
      );
    }
    const s = this.resolveProvider();
    if (!s) {
      throw new WalletSignError(
        "ProviderMissing",
        "WindowWalletAdapter: window.solana disappeared mid-session",
      );
    }
    if (!s.signAndSendTransaction) {
      throw new WalletSignError(
        "ProviderUnsupported",
        "WindowWalletAdapter: provider lacks signAndSendTransaction",
      );
    }
    let result: WindowSolanaSignResult;
    try {
      result = await s.signAndSendTransaction(draft.borshBytes);
    } catch (e) {
      // Phantom / Backpack throw `WalletSignTransactionError`-shaped
      // objects whose .code === 4001 means "user rejected request".
      const rejected =
        typeof e === "object" &&
        e !== null &&
        ((e as { code?: number }).code === 4001 ||
          /reject|denied|cancel/i.test(
            (e as { message?: string }).message ?? "",
          ));
      throw new WalletSignError(
        rejected ? "UserRejected" : "ProviderError",
        rejected
          ? "WindowWalletAdapter: user rejected the transaction"
          : "WindowWalletAdapter: signAndSendTransaction threw",
        e,
      );
    }
    if (!result || typeof result.signature !== "string") {
      throw new WalletSignError(
        "ProviderError",
        "WindowWalletAdapter: provider returned no signature",
      );
    }
    return result.signature;
  }
}
