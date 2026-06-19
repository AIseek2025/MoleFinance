// Wave 15 — wasm tx-builder unit tests.
//
// The tests in this file exercise the wasm-pack-built `keeper-decoder`
// package via the `frontend/src/tx/wasmBuilder.ts` adapter. They:
//
// 1. confirm round-trip correctness of every wave-15 encoder
//    (open_position, close_position, keeper_leader_heartbeat,
//    keeper_leader_release) against canonical fixtures;
// 2. assert byte-equivalence between the wasm encoder output and
//    the wave-15 hand-rolled `frontend/src/tx/encode.ts` (TS-only
//    parity oracle), which is itself byte-equal to the Rust
//    `keeper_decoder::ix` output;
// 3. exercise the leader-lock decoder for both the 49-byte body and
//    the 57-byte full Anchor payload paths.
//
// `loadKeeperDecoder()` is called at the top of every test; the
// memoised promise keeps subsequent invocations free.

import { describe, expect, it, beforeAll } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { Buffer } from "buffer";

import {
  __resetForTesting,
  accountDiscriminator,
  buildClosePositionTx,
  buildKeeperLeaderAcquireTx,
  buildKeeperLeaderHeartbeatTx,
  buildKeeperLeaderReleaseTx,
  buildOpenPositionTx,
  decodeKeeperLeaderLockBytes,
  instructionDiscriminator,
  keeperLeaderLockSeedBytes,
  loadKeeperDecoder,
} from "./wasmBuilder";
import {
  encodeClosePositionIx,
  encodeOpenPositionIx,
  type OpenParams,
  type PriceEnvelopeArgs as TsEnvelope,
} from "./encode";

beforeAll(async () => {
  // Load the wasm artifact once for the whole test file. We feed
  // the raw bytes directly so vitest doesn't have to resolve the
  // bundler's relative URL flow.
  __resetForTesting();
  const wasmPath = resolve(
    __dirname,
    "../../node_modules/keeper-decoder/keeper_decoder_bg.wasm",
  );
  const wasmBytes = readFileSync(wasmPath);
  await loadKeeperDecoder({ module_or_path: wasmBytes });
});

describe("wasmBuilder — discriminators", () => {
  it("instructionDiscriminator returns 8 bytes and is stable", () => {
    const a = instructionDiscriminator("open_position");
    const b = instructionDiscriminator("open_position");
    expect(a).toBeInstanceOf(Uint8Array);
    expect(a.length).toBe(8);
    expect(Buffer.from(a)).toEqual(Buffer.from(b));
  });

  it("instructionDiscriminator matches Rust golden vector", () => {
    // Computed via `printf 'global:open_position' | shasum -a 256
    // | head -c 16` → 87802f4d0f98f031.
    expect(Array.from(instructionDiscriminator("open_position"))).toEqual([
      0x87, 0x80, 0x2f, 0x4d, 0x0f, 0x98, 0xf0, 0x31,
    ]);
    expect(Array.from(instructionDiscriminator("close_position"))).toEqual([
      0x7b, 0x86, 0x51, 0x00, 0x31, 0x44, 0x62, 0x62,
    ]);
    expect(
      Array.from(instructionDiscriminator("keeper_leader_heartbeat")),
    ).toEqual([0x2f, 0x0b, 0x5a, 0x8b, 0xb7, 0xa4, 0x08, 0x1c]);
  });

  it("accountDiscriminator returns 8 bytes for a known type", () => {
    const disc = accountDiscriminator("KeeperLeaderLock");
    expect(disc).toBeInstanceOf(Uint8Array);
    expect(disc.length).toBe(8);
  });
});

describe("wasmBuilder — open_position", () => {
  const envelope: TsEnvelope = {
    pNow: 100_000_000n,
    slot: 12_345n,
    expectedMin: 99_500_000n,
    expectedMax: 100_500_000n,
  };

  it("emits 57 bytes (8 disc + 49 body)", () => {
    const raw = buildOpenPositionTx({
      envelope: {
        pNow: envelope.pNow,
        slot: envelope.slot,
        expectedMin: envelope.expectedMin,
        expectedMax: envelope.expectedMax,
      },
      directionIsLong: true,
      grossAmount: 1_000_000n,
      positionId: 0xdead_beefn,
    });
    expect(raw.length).toBe(57);
    expect(Array.from(raw.slice(0, 8))).toEqual([
      0x87, 0x80, 0x2f, 0x4d, 0x0f, 0x98, 0xf0, 0x31,
    ]);
    // Direction byte at offset 40.
    expect(raw[40]).toBe(0x01);
  });

  it("byte-matches the wave-15 TS encoder (parity oracle)", () => {
    const params: OpenParams = {
      envelope,
      directionIsLong: true,
      grossAmount: 1_000_000n,
      positionId: 0xdead_beefn,
    };
    const tsBytes = encodeOpenPositionIx(params);
    const wasmBytes = buildOpenPositionTx({
      envelope: { ...envelope },
      directionIsLong: true,
      grossAmount: 1_000_000n,
      positionId: 0xdead_beefn,
    });
    expect(Buffer.from(wasmBytes)).toEqual(Buffer.from(tsBytes));
  });

  it("short side flips the direction byte", () => {
    const raw = buildOpenPositionTx({
      envelope: { ...envelope },
      directionIsLong: false,
      grossAmount: 500n,
      positionId: 1n,
    });
    expect(raw[40]).toBe(0x00);
  });
});

describe("wasmBuilder — close_position", () => {
  const envelope: TsEnvelope = {
    pNow: 100_000_000n,
    slot: 12_345n,
    expectedMin: 99_500_000n,
    expectedMax: 100_500_000n,
  };

  it("emits 48 bytes (8 disc + 40 body)", () => {
    const raw = buildClosePositionTx({
      envelope: { ...envelope },
      longBucketCount: 3,
      shortBucketCount: 0,
    });
    expect(raw.length).toBe(48);
    expect(Array.from(raw.slice(0, 8))).toEqual([
      0x7b, 0x86, 0x51, 0x00, 0x31, 0x44, 0x62, 0x62,
    ]);
  });

  it("rejects negative or non-integer bucket counts", () => {
    expect(() =>
      buildClosePositionTx({
        envelope: { ...envelope },
        longBucketCount: -1,
        shortBucketCount: 0,
      }),
    ).toThrow(/bucket counts must be u32/);
    expect(() =>
      buildClosePositionTx({
        envelope: { ...envelope },
        longBucketCount: 1.5,
        shortBucketCount: 0,
      }),
    ).toThrow(/bucket counts must be u32/);
  });

  it("byte-matches the wave-15 TS encoder", () => {
    const tsBytes = encodeClosePositionIx(envelope, 3, 7);
    const wasmBytes = buildClosePositionTx({
      envelope: { ...envelope },
      longBucketCount: 3,
      shortBucketCount: 7,
    });
    expect(Buffer.from(wasmBytes)).toEqual(Buffer.from(tsBytes));
  });
});

describe("wasmBuilder — keeper-leader instructions", () => {
  it("heartbeat emits 16 bytes (8 disc + u64 slot)", () => {
    const raw = buildKeeperLeaderHeartbeatTx({ observedSlot: 1_000n });
    expect(raw.length).toBe(16);
    expect(Array.from(raw.slice(0, 8))).toEqual([
      0x2f, 0x0b, 0x5a, 0x8b, 0xb7, 0xa4, 0x08, 0x1c,
    ]);
    // slot bytes are little-endian.
    const view = new DataView(raw.buffer, raw.byteOffset + 8, 8);
    expect(view.getBigUint64(0, true)).toBe(1_000n);
  });

  it("release emits exactly 8 bytes (discriminator only)", () => {
    const raw = buildKeeperLeaderReleaseTx();
    expect(raw.length).toBe(8);
    expect(Buffer.from(raw)).toEqual(
      Buffer.from(instructionDiscriminator("keeper_leader_release")),
    );
  });

  // Wave 17 — manual ops console encoder for `keeper_leader_acquire`.
  it("acquire emits 16 bytes (8 disc + u64 slot) with the right discriminator", () => {
    const raw = buildKeeperLeaderAcquireTx({ observedSlot: 42_000n });
    expect(raw.length).toBe(16);
    // Discriminator must equal sha256("global:keeper_leader_acquire")[..8] —
    // the wasm bundle ships the same helper.
    expect(Buffer.from(raw.slice(0, 8))).toEqual(
      Buffer.from(instructionDiscriminator("keeper_leader_acquire")),
    );
    const view = new DataView(raw.buffer, raw.byteOffset + 8, 8);
    expect(view.getBigUint64(0, true)).toBe(42_000n);
  });

  it("acquire and heartbeat have DIFFERENT discriminators (regression guard)", () => {
    // If a refactor accidentally aliased one to the other (e.g.
    // copy-paste from heartbeat to acquire), this test catches it
    // before a frontend tx silently lands on the wrong on-chain
    // handler.
    const acquireDisc = buildKeeperLeaderAcquireTx({ observedSlot: 1n }).slice(0, 8);
    const heartbeatDisc = buildKeeperLeaderHeartbeatTx({ observedSlot: 1n }).slice(0, 8);
    expect(Buffer.from(acquireDisc).equals(Buffer.from(heartbeatDisc))).toBe(false);
  });
});

describe("wasmBuilder — KeeperLeaderLock decoder", () => {
  it("decodes a 49-byte body without discriminator", () => {
    // Hand-craft a payload: leader = ALICE (0xa1×32), slot = 7,
    // takeover = 75.
    const body = new Uint8Array(49);
    body[0] = 1; // option marker
    body.fill(0xa1, 1, 33);
    new DataView(body.buffer).setBigUint64(33, 7n, true);
    new DataView(body.buffer).setBigUint64(41, 75n, true);

    const view = decodeKeeperLeaderLockBytes(body);
    expect(view.hasLeader).toBe(true);
    expect(Array.from(view.currentLeader)).toEqual(Array(32).fill(0xa1));
    expect(view.lastHeartbeatSlot).toBe(7n);
    expect(view.takeoverThresholdSlots).toBe(75n);
  });

  it("decodes the 57-byte form with leading 8-byte discriminator", () => {
    // Anchor account discriminator can be anything for decode; the
    // wasm helper only strips the prefix.
    const full = new Uint8Array(57);
    full.fill(0xab, 0, 8); // arbitrary disc
    full[8] = 1; // option marker
    full.fill(0xb0, 9, 41); // BOB
    new DataView(full.buffer).setBigUint64(41, 999n, true);
    new DataView(full.buffer).setBigUint64(49, 30n, true);

    const view = decodeKeeperLeaderLockBytes(full);
    expect(view.hasLeader).toBe(true);
    expect(view.lastHeartbeatSlot).toBe(999n);
    expect(view.takeoverThresholdSlots).toBe(30n);
  });

  it("decodes an unowned lock (option marker = 0) with all-zero leader bytes", () => {
    const body = new Uint8Array(49);
    body[0] = 0; // unowned
    new DataView(body.buffer).setBigUint64(33, 0n, true);
    new DataView(body.buffer).setBigUint64(41, 75n, true);
    const view = decodeKeeperLeaderLockBytes(body);
    expect(view.hasLeader).toBe(false);
    expect(Array.from(view.currentLeader)).toEqual(Array(32).fill(0));
  });

  it("throws on truncated payload", () => {
    expect(() => decodeKeeperLeaderLockBytes(new Uint8Array(4))).toThrow();
  });
});

describe("wasmBuilder — PDA seeds", () => {
  it("returns the canonical keeper_leader_lock seed bytes", () => {
    const seed = keeperLeaderLockSeedBytes();
    expect(Buffer.from(seed).toString("utf-8")).toBe("keeper_leader_lock");
  });
});
