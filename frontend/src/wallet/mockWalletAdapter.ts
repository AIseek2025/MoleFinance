// Wave 12 — Mock WalletAdapter.
//
// Always succeeds, always returns the same pubkey. Used for the
// offline demo so trader-panel buttons are clickable without
// installing a Solana wallet extension.

import type { Pubkey32 } from "../types";
import type { UnsignedTxDraft, WalletAdapter, WalletStatus } from "./adapter";

const MOCK_PUBKEY: Pubkey32 = {
  hex: "11ee22dd33cc44bb55aa66998877665544332211ffdd11aa22bb33cc44dd5500",
};

export class MockWalletAdapter implements WalletAdapter {
  readonly name = "mock" as const;
  private currentStatus: WalletStatus = "disconnected";
  private currentPubkey: Pubkey32 | null = null;
  private nonce = 0;

  status(): WalletStatus {
    return this.currentStatus;
  }

  pubkey(): Pubkey32 | null {
    return this.currentPubkey;
  }

  async connect(): Promise<Pubkey32> {
    this.currentStatus = "connecting";
    await tinySleep(40);
    this.currentPubkey = MOCK_PUBKEY;
    this.currentStatus = "connected";
    return MOCK_PUBKEY;
  }

  async disconnect(): Promise<void> {
    await tinySleep(10);
    this.currentPubkey = null;
    this.currentStatus = "disconnected";
  }

  async signAndSubmit(draft: UnsignedTxDraft): Promise<string> {
    if (this.currentStatus !== "connected") {
      throw new Error("MockWalletAdapter: not connected");
    }
    this.nonce += 1;
    await tinySleep(20);
    // Synthetic signature: deterministic for the same nonce.
    const tag = (draft.description.codePointAt(0) ?? 0).toString(16);
    return `mock-sig-${this.nonce.toString().padStart(6, "0")}-${tag}`;
  }
}

function tinySleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
