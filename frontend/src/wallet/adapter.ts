// Wave 12 — WalletAdapter abstraction.
//
// The trader panel needs three operations from the user's wallet:
//   1. connect()                    — surface the public key
//   2. signAndSubmit(unsignedTx)    — sign + submit, return signature
//   3. disconnect()                 — drop session
//
// Wave 12 ships:
//   - the interface (this file)
//   - a `MockWalletAdapter` for offline UI development
//   - a `WindowWalletAdapter` skeleton that detects window.solana
//
// Wave 13 wires the real Phantom / Backpack / Solflare adapter via
// `@solana/wallet-adapter-base` once the live feed is up; until
// then `signAndSubmit` returns a synthetic signature so the demo
// renders end-to-end.

import type { Pubkey32 } from "../types";

export type WalletStatus =
  | "disconnected"
  | "connecting"
  | "connected"
  | "error";

export interface UnsignedTxDraft {
  /** Free-form action description for the demo confirmation dialog. */
  description: string;
  /** Borsh-encoded transaction bytes. Wave 12 placeholder is empty. */
  borshBytes?: Uint8Array;
}

export interface WalletAdapter {
  /** Adapter name surfaced in the topbar. */
  readonly name: "mock" | "phantom" | "backpack" | "solflare";

  /** Current connection status. */
  status(): WalletStatus;

  /** Connected pubkey (null when disconnected). */
  pubkey(): Pubkey32 | null;

  /** Establish a session. Resolves to the pubkey on success. */
  connect(): Promise<Pubkey32>;

  /** Drop the session. Idempotent. */
  disconnect(): Promise<void>;

  /**
   * Sign + submit. Returns a transaction signature (base58 string).
   * The wave-12 placeholder returns a deterministic synthetic
   * signature so demos work without real wallet plumbing.
   */
  signAndSubmit(draft: UnsignedTxDraft): Promise<string>;
}
