/**
 * Wave 14 — WindowWalletAdapter unit tests.
 *
 * Tests use the constructor's `provider` injection rather than
 * stubbing `window.solana` directly, so they run cleanly in both
 * `jsdom` and `node` environments. Coverage:
 *   1. connect() pulls the public key, surfaces hex form
 *   2. signAndSubmit() invokes signAndSendTransaction with the
 *      draft's `borshBytes` and returns the resulting signature
 *   3. user-rejection (code 4001) maps to `WalletSignError.kind === "UserRejected"`
 *   4. missing tx bytes maps to `NoTxBytes`
 *   5. wallet not connected maps to `WalletNotConnected`
 *   6. missing provider maps to `ProviderMissing`
 *   7. provider lacks signAndSendTransaction → `ProviderUnsupported`
 *   8. provider returns malformed result → `ProviderError`
 *   9. detectName picks Backpack > Solflare > Phantom in priority
 *
 * @vitest-environment node
 */
import { describe, expect, it, vi } from "vitest";
import { Buffer } from "buffer";
import {
  WalletSignError,
  WindowWalletAdapter,
  type WindowSolanaProvider,
} from "./windowWalletAdapter";

function makeProvider(
  overrides: Partial<WindowSolanaProvider>,
): WindowSolanaProvider {
  // exactOptionalPropertyTypes — only spread keys actually defined.
  const base: WindowSolanaProvider = { isPhantom: true };
  return { ...base, ...overrides };
}

function pkBytes(seed: number): Uint8Array {
  const buf = Buffer.alloc(32);
  buf[0] = seed;
  return buf;
}

describe("WindowWalletAdapter", () => {
  it("connect() pulls a 32-byte pubkey and surfaces hex form", async () => {
    const connect = vi.fn(async () => ({
      publicKey: { toBytes: () => pkBytes(7) },
    }));
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({ connect }),
    });
    const pk = await adapter.connect();
    expect(connect).toHaveBeenCalledOnce();
    expect(pk.hex).toMatch(/^07[0-9a-f]{62}$/);
    expect(adapter.status()).toBe("connected");
    expect(adapter.pubkey()?.hex).toBe(pk.hex);
  });

  it("signAndSubmit invokes signAndSendTransaction with the draft bytes", async () => {
    const signAndSendTransaction = vi.fn(
      async (_bytes: Uint8Array) => ({ signature: "5sig...stub" }),
    );
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({
        connect: async () => ({ publicKey: { toBytes: () => pkBytes(1) } }),
        signAndSendTransaction,
      }),
    });
    await adapter.connect();
    const sig = await adapter.signAndSubmit({
      description: "open long",
      borshBytes: new Uint8Array([1, 2, 3, 4]),
    });
    expect(sig).toBe("5sig...stub");
    expect(signAndSendTransaction).toHaveBeenCalledOnce();
    const firstCall = signAndSendTransaction.mock.calls[0];
    expect(firstCall).toBeDefined();
    const arg = firstCall![0];
    expect(Array.from(arg)).toEqual([1, 2, 3, 4]);
  });

  it("user rejection (code 4001) maps to UserRejected", async () => {
    const signAndSendTransaction = vi.fn(async () => {
      const e = Object.assign(new Error("User rejected the request"), {
        code: 4001,
      });
      throw e;
    });
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({
        connect: async () => ({ publicKey: { toBytes: () => pkBytes(1) } }),
        signAndSendTransaction,
      }),
    });
    await adapter.connect();
    try {
      await adapter.signAndSubmit({
        description: "x",
        borshBytes: new Uint8Array([0]),
      });
      throw new Error("should have thrown");
    } catch (e) {
      expect(e).toBeInstanceOf(WalletSignError);
      expect((e as WalletSignError).kind).toBe("UserRejected");
    }
  });

  it("rejection inferred from message text also maps to UserRejected", async () => {
    const signAndSendTransaction = vi.fn(async () => {
      throw new Error("Approval was denied by the user");
    });
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({
        connect: async () => ({ publicKey: { toBytes: () => pkBytes(1) } }),
        signAndSendTransaction,
      }),
    });
    await adapter.connect();
    try {
      await adapter.signAndSubmit({
        description: "x",
        borshBytes: new Uint8Array([0]),
      });
      throw new Error("should have thrown");
    } catch (e) {
      expect((e as WalletSignError).kind).toBe("UserRejected");
    }
  });

  it("missing tx bytes maps to NoTxBytes", async () => {
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({
        connect: async () => ({ publicKey: { toBytes: () => pkBytes(1) } }),
        signAndSendTransaction: async () => ({ signature: "x" }),
      }),
    });
    await adapter.connect();
    try {
      await adapter.signAndSubmit({ description: "x" });
      throw new Error("should have thrown");
    } catch (e) {
      expect((e as WalletSignError).kind).toBe("NoTxBytes");
    }
  });

  it("not connected → WalletNotConnected", async () => {
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({
        signAndSendTransaction: async () => ({ signature: "x" }),
      }),
    });
    try {
      await adapter.signAndSubmit({
        description: "x",
        borshBytes: new Uint8Array([1]),
      });
      throw new Error("should have thrown");
    } catch (e) {
      expect((e as WalletSignError).kind).toBe("WalletNotConnected");
    }
  });

  it("provider missing → ProviderMissing", async () => {
    const adapter = new WindowWalletAdapter({});
    try {
      await adapter.connect();
      throw new Error("should have thrown");
    } catch (e) {
      expect((e as WalletSignError).kind).toBe("ProviderMissing");
    }
  });

  it("provider lacks signAndSendTransaction → ProviderUnsupported", async () => {
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({
        connect: async () => ({ publicKey: { toBytes: () => pkBytes(1) } }),
      }),
    });
    await adapter.connect();
    try {
      await adapter.signAndSubmit({
        description: "x",
        borshBytes: new Uint8Array([1]),
      });
      throw new Error("should have thrown");
    } catch (e) {
      expect((e as WalletSignError).kind).toBe("ProviderUnsupported");
    }
  });

  it("provider returns malformed result → ProviderError", async () => {
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({
        connect: async () => ({ publicKey: { toBytes: () => pkBytes(1) } }),
        signAndSendTransaction: async () =>
          ({ signature: 42 } as unknown as { signature: string }),
      }),
    });
    await adapter.connect();
    try {
      await adapter.signAndSubmit({
        description: "x",
        borshBytes: new Uint8Array([1]),
      });
      throw new Error("should have thrown");
    } catch (e) {
      expect((e as WalletSignError).kind).toBe("ProviderError");
    }
  });

  it("detects Backpack ahead of Phantom when both flags present", () => {
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({ isPhantom: true, isBackpack: true }),
    });
    expect(adapter.name).toBe("backpack");
  });

  it("detects Solflare", () => {
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({ isPhantom: false, isSolflare: true }),
    });
    expect(adapter.name).toBe("solflare");
  });

  it("eagerConnect() silently restores a trusted session", async () => {
    const connect = vi.fn(async (opts?: { onlyIfTrusted?: boolean }) => {
      expect(opts?.onlyIfTrusted).toBe(true);
      return { publicKey: { toBytes: () => pkBytes(9) } };
    });
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({ connect }),
    });
    const pk = await adapter.eagerConnect();
    expect(pk?.hex).toMatch(/^09/);
    expect(adapter.status()).toBe("connected");
  });

  it("eagerConnect() returns null (no throw) when not trusted", async () => {
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({
        connect: async () => {
          throw new Error("not trusted");
        },
      }),
    });
    expect(await adapter.eagerConnect()).toBeNull();
    expect(adapter.status()).toBe("disconnected");
  });

  it("onChange fires on connect / disconnect", async () => {
    const events: Array<[string, string | null]> = [];
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({
        connect: async () => ({ publicKey: { toBytes: () => pkBytes(3) } }),
        disconnect: async () => {},
      }),
    });
    adapter.onChange((s, pk) => events.push([s, pk?.hex ?? null]));
    await adapter.connect();
    await adapter.disconnect();
    expect(events.map((e) => e[0])).toEqual(["connected", "disconnected"]);
  });

  it("reacts to a provider accountChanged event", async () => {
    let accountChanged: ((arg: unknown) => void) | undefined;
    const on = vi.fn((event: string, handler: (arg: unknown) => void) => {
      if (event === "accountChanged") accountChanged = handler;
    });
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({
        connect: async () => ({ publicKey: { toBytes: () => pkBytes(1) } }),
        on,
      }),
    });
    await adapter.connect();
    // User switches accounts in the extension.
    accountChanged?.({ toBytes: () => pkBytes(42) });
    expect(adapter.pubkey()?.hex).toMatch(/^2a/);
    expect(adapter.status()).toBe("connected");
    // Extension locks / disconnects the account (null payload).
    accountChanged?.(null);
    expect(adapter.pubkey()).toBeNull();
    expect(adapter.status()).toBe("disconnected");
  });

  it("dispose() detaches provider listeners", async () => {
    const off = vi.fn();
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({
        on: () => {},
        off,
        connect: async () => ({ publicKey: { toBytes: () => pkBytes(1) } }),
      }),
    });
    adapter.dispose();
    expect(off).toHaveBeenCalledWith("accountChanged", expect.any(Function));
    expect(off).toHaveBeenCalledWith("disconnect", expect.any(Function));
  });

  it("disconnect() resets status + clears pubkey", async () => {
    const disconnect = vi.fn(async () => {});
    const adapter = new WindowWalletAdapter({
      provider: makeProvider({
        connect: async () => ({ publicKey: { toBytes: () => pkBytes(1) } }),
        disconnect,
      }),
    });
    await adapter.connect();
    expect(adapter.status()).toBe("connected");
    await adapter.disconnect();
    expect(disconnect).toHaveBeenCalledOnce();
    expect(adapter.status()).toBe("disconnected");
    expect(adapter.pubkey()).toBeNull();
  });
});
