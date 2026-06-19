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
  /**
   * Wave 30 — EventEmitter surface exposed by Phantom / Backpack /
   * Solflare. The adapter subscribes to `accountChanged` (user
   * switched accounts in the extension → arg is the new PublicKey or
   * `null`) and `disconnect` (extension dropped the session) so the
   * React UI reflects out-of-band wallet state changes.
   */
  on?: (event: string, handler: (arg: unknown) => void) => void;
  off?: (event: string, handler: (arg: unknown) => void) => void;
  removeListener?: (event: string, handler: (arg: unknown) => void) => void;
}

/** Wave 30 — listener notified when the wallet state changes (connect, */
/** disconnect, or an out-of-band account switch in the extension). */
export type WalletChangeListener = (
  status: WalletStatus,
  pubkey: Pubkey32 | null,
) => void;

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

/** Parse a wallet-provided public-key-ish value into a `Pubkey32`, or */
/** `null` when the value is missing / malformed. */
function pubkeyFromMaybe(value: unknown): Pubkey32 | null {
  if (typeof value !== "object" || value === null) return null;
  const toBytes = (value as { toBytes?: () => Uint8Array }).toBytes;
  if (typeof toBytes !== "function") return null;
  const bytes = toBytes.call(value);
  if (!bytes || bytes.length !== 32) return null;
  return { hex: bytesToHex(bytes) };
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
  private readonly listeners = new Set<WalletChangeListener>();
  private readonly accountChangedHandler = (arg: unknown): void => {
    const pk = pubkeyFromMaybe(arg);
    if (pk) {
      // User switched to a different account in the extension.
      this.currentPubkey = pk;
      this.currentStatus = "connected";
    } else {
      // Phantom passes `null` when the active account is locked /
      // disconnected — treat as a session drop.
      this.currentPubkey = null;
      this.currentStatus = "disconnected";
    }
    this.emit();
  };
  private readonly disconnectHandler = (): void => {
    this.currentPubkey = null;
    this.currentStatus = "disconnected";
    this.emit();
  };

  constructor(opts?: { provider?: WindowSolanaProvider }) {
    this.providerOverride = opts?.provider;
    const s = this.resolveProvider();
    this.name = s ? detectName(s) : "phantom";
    this.subscribeProviderEvents(s);
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

  /**
   * Wave 30 — subscribe to wallet state changes. The returned function
   * unsubscribes. Lets React reflect out-of-band changes (account
   * switch / extension disconnect) without polling.
   */
  onChange(listener: WalletChangeListener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  private emit(): void {
    for (const l of this.listeners) {
      try {
        l(this.currentStatus, this.currentPubkey);
      } catch (e) {
        console.warn("[mole/frontend] wallet onChange listener threw:", e);
      }
    }
  }

  private subscribeProviderEvents(s: WindowSolanaProvider | undefined): void {
    if (!s || typeof s.on !== "function") return;
    s.on("accountChanged", this.accountChangedHandler);
    s.on("disconnect", this.disconnectHandler);
  }

  /** Wave 30 — detach provider event listeners. Idempotent. */
  dispose(): void {
    const s = this.resolveProvider();
    const off = s?.off ?? s?.removeListener;
    if (s && typeof off === "function") {
      off.call(s, "accountChanged", this.accountChangedHandler);
      off.call(s, "disconnect", this.disconnectHandler);
    }
    this.listeners.clear();
  }

  /**
   * Wave 30 — best-effort silent reconnect on page load. Uses the
   * wallet's `onlyIfTrusted` connect path so a previously-approved
   * session is restored WITHOUT a popup. Never throws: an untrusted
   * / missing / locked wallet simply resolves to `null` and the user
   * connects explicitly via the button.
   */
  async eagerConnect(): Promise<Pubkey32 | null> {
    const s = this.resolveProvider();
    if (!s || !s.connect) return null;
    try {
      const r = await s.connect({ onlyIfTrusted: true });
      const pk = pubkeyFromMaybe(r.publicKey);
      if (!pk) return null;
      this.currentPubkey = pk;
      this.currentStatus = "connected";
      this.emit();
      return pk;
    } catch {
      return null;
    }
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
    const pk = pubkeyFromMaybe(r.publicKey);
    if (!pk) {
      this.currentStatus = "error";
      throw new WalletSignError(
        "ProviderError",
        "WindowWalletAdapter.connect returned no 32-byte public key",
      );
    }
    this.currentPubkey = pk;
    this.currentStatus = "connected";
    this.emit();
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
    this.emit();
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
