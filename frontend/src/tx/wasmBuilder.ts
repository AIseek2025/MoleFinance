// Wave 15 — Wasm-driven instruction builder.
//
// This module is the FRONTEND'S authoritative path for constructing
// `mole-option` Anchor instruction payloads. It calls into the
// `wasm-pack`-built `keeper-decoder` package, which is the byte-for-
// byte mirror of the Rust `keeper_decoder::ix` encoders the keeper
// bot also uses.
//
// ## Why wasm rather than the wave-15 hand-rolled `tx/encode.ts`
//
// `frontend/src/tx/encode.ts` is the pre-wave-15 TypeScript encoder.
// It still ships as a parity oracle (its output is byte-equal to the
// wasm output, pinned by `wasmBuilder.test.ts::ts_and_wasm_emit_byte_equal_payloads`)
// but the production code path goes through wasm so:
//
// 1. Schema bumps in `crates/keeper-decoder/src/ix.rs` automatically
//    propagate to the frontend on the next `wasm-pack build`. The
//    hand-rolled TS path requires a manual diff.
// 2. Auditors only need to read the Rust encoder once. The TS shim
//    is effectively a typed FFI call.
// 3. The wasm bundle is 37 KB optimised, smaller than the
//    `@coral-xyz/borsh` + `bn.js` + `buffer-layout` chain the
//    hand-rolled path pulls in, so the production bundle shrinks.
//
// ## Lazy initialisation
//
// `loadKeeperDecoder()` returns a promise that resolves to the
// initialised wasm module. We memoise the promise so concurrent
// callers share the same instance. The wasm-bindgen-emitted
// `default()` exporter takes either a fetch URL or a precompiled
// module; we let it default-fetch the bundled `.wasm` so Vite's
// asset pipeline can hash + cache it like any other static file.
//
// ## Exported API
//
// - `buildOpenPositionTx(params)` — returns `Uint8Array` (8-byte
//   discriminator + 49-byte borsh body).
// - `buildClosePositionTx(params)` — returns `Uint8Array` (8 + 40 = 48
//   bytes).
// - `buildKeeperLeaderHeartbeatTx({observedSlot})` — returns
//   `Uint8Array` (8 + 8 = 16 bytes).
// - `instructionDiscriminator(name)` — pure helper, returns 8 bytes.
// - `accountDiscriminator(name)` — pure helper, returns 8 bytes.
// - `decodeKeeperLeaderLockBytes(bytes)` — typed view of the leader
//   lock payload.

import init, {
  accountDiscriminator as wasmAccountDiscriminator,
  decodeKeeperLeaderLock,
  encodeClosePosition,
  encodeKeeperLeaderAcquire,
  encodeKeeperLeaderHeartbeat,
  encodeKeeperLeaderRelease,
  encodeOpenPosition,
  instructionDiscriminator as wasmInstructionDiscriminator,
  keeperLeaderLockSeedPrefix,
  wasmInit,
  type KeeperLeaderLockView,
} from "keeper-decoder";

/**
 * Cache of the wasm module load. Subsequent callers receive the
 * same `Promise<void>` so we don't double-fetch the binary.
 */
let initPromise: Promise<void> | null = null;

/**
 * Lazily initialise the wasm module. Idempotent across concurrent
 * callers. Resolves to `void`; the module's exports become live
 * after this resolves.
 *
 * For tests, `__resetForTesting` lets the harness drop the cached
 * promise so a fresh `init` runs.
 */
export async function loadKeeperDecoder(
  initInput?: Parameters<typeof init>[0],
): Promise<void> {
  if (!initPromise) {
    initPromise = (async () => {
      // wasm-bindgen 0.2.121 emits `default()` returning the InitOutput.
      // Passing `undefined` triggers the default fetch flow (Vite
      // resolves the relative URL to `keeper_decoder_bg.wasm`).
      await init(initInput);
      wasmInit();
    })();
  }
  return initPromise;
}

/** TEST-ONLY: reset the cached init promise. */
export function __resetForTesting(): void {
  initPromise = null;
}

/**
 * `PriceEnvelopeArgs` mirror — the four oracle assertion bounds.
 * All values are u64 (`bigint` on the JS side).
 */
export interface PriceEnvelopeArgs {
  /** Current oracle price (PRICE_SCALE = 1e8). */
  pNow: bigint;
  /** Slot the price was fetched at. */
  slot: bigint;
  /** Lower bound of the assertion (inclusive). */
  expectedMin: bigint;
  /** Upper bound of the assertion (inclusive). */
  expectedMax: bigint;
}

/** Open-position arg tuple — mirrors `keeper_decoder::ix::OpenPositionArgs`. */
export interface BuildOpenPositionArgs {
  envelope: PriceEnvelopeArgs;
  directionIsLong: boolean;
  /** Gross collateral the trader is locking up (minor units). */
  grossAmount: bigint;
  /** Caller-allocated position id. */
  positionId: bigint;
}

/**
 * Build the wave-15 `open_position` instruction payload. Caller MUST
 * have awaited `loadKeeperDecoder()` first.
 *
 * Returns `disc[8] ++ envelope[32] ++ direction[1] ++ gross_amount[8]
 * ++ position_id[8]` = 57 bytes total.
 */
export function buildOpenPositionTx(args: BuildOpenPositionArgs): Uint8Array {
  return encodeOpenPosition(
    args.envelope.pNow,
    args.envelope.slot,
    args.envelope.expectedMin,
    args.envelope.expectedMax,
    args.directionIsLong,
    args.grossAmount,
    args.positionId,
  );
}

/** Close-position arg tuple. */
export interface BuildClosePositionArgs {
  envelope: PriceEnvelopeArgs;
  longBucketCount: number;
  shortBucketCount: number;
}

/**
 * Build the wave-15 `close_position` instruction payload. 48 bytes
 * total (`disc[8] ++ envelope[32] ++ long[4] ++ short[4]`).
 */
export function buildClosePositionTx(args: BuildClosePositionArgs): Uint8Array {
  if (
    !Number.isInteger(args.longBucketCount) ||
    !Number.isInteger(args.shortBucketCount) ||
    args.longBucketCount < 0 ||
    args.shortBucketCount < 0 ||
    args.longBucketCount > 0xffff_ffff ||
    args.shortBucketCount > 0xffff_ffff
  ) {
    throw new Error(
      `bucket counts must be u32 integers in [0, 2^32-1]; got long=${args.longBucketCount} short=${args.shortBucketCount}`,
    );
  }
  return encodeClosePosition(
    args.envelope.pNow,
    args.envelope.slot,
    args.envelope.expectedMin,
    args.envelope.expectedMax,
    args.longBucketCount,
    args.shortBucketCount,
  );
}

/** Keeper-leader heartbeat arg tuple. */
export interface BuildKeeperLeaderHeartbeatArgs {
  observedSlot: bigint;
}

/**
 * Build the wave-15 `keeper_leader_heartbeat` instruction payload.
 * 16 bytes total (`disc[8] ++ slot[8]`).
 */
export function buildKeeperLeaderHeartbeatTx(
  args: BuildKeeperLeaderHeartbeatArgs,
): Uint8Array {
  return encodeKeeperLeaderHeartbeat(args.observedSlot);
}

/**
 * Build the wave-15 `keeper_leader_release` instruction payload.
 * 8 bytes total (only the discriminator).
 */
export function buildKeeperLeaderReleaseTx(): Uint8Array {
  return encodeKeeperLeaderRelease();
}

/** Keeper-leader acquire arg tuple. */
export interface BuildKeeperLeaderAcquireArgs {
  observedSlot: bigint;
}

/**
 * Wave 17 — build the `keeper_leader_acquire` instruction payload.
 * 16 bytes total (`disc[8] ++ slot[8]`). Mirrors heartbeat layout
 * but with the `keeper_leader_acquire` discriminator. Used by the
 * wave-17 manual ops console (`KeeperPanel`); production keeper
 * bots use the same Rust encoder via `keeper-decoder::ix`.
 */
export function buildKeeperLeaderAcquireTx(
  args: BuildKeeperLeaderAcquireArgs,
): Uint8Array {
  return encodeKeeperLeaderAcquire(args.observedSlot);
}

/** Returns the 8-byte Anchor instruction discriminator for `name`. */
export function instructionDiscriminator(name: string): Uint8Array {
  return wasmInstructionDiscriminator(name);
}

/** Returns the 8-byte Anchor account discriminator for `name`. */
export function accountDiscriminator(name: string): Uint8Array {
  return wasmAccountDiscriminator(name);
}

/**
 * Decode the leader-lock account payload (49 raw bytes OR 57 bytes
 * including the 8-byte discriminator). Returns a typed view; throws
 * on malformed input.
 */
export function decodeKeeperLeaderLockBytes(
  bytes: Uint8Array,
): KeeperLeaderLockView {
  return decodeKeeperLeaderLock(bytes);
}

/** Wave-15 PDA seed for the keeper-leader lock. */
export function keeperLeaderLockSeedBytes(): Uint8Array {
  return keeperLeaderLockSeedPrefix();
}

export type { KeeperLeaderLockView };
