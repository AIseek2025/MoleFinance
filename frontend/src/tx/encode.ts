/**
 * Wave 15 — TypeScript instruction encoder for the mole-option program.
 *
 * Hand-rolled mirror of `crates/tx-codec/src/lib.rs` until wave 16
 * ships the wasm-pack artifact. The byte layout MUST match the Rust
 * encoder exactly; the test suite at `frontend/src/tx/encode.test.ts`
 * pins this against fixtures lifted from
 * `crates/tx-codec/src/lib.rs::tests::*`.
 *
 * Implementation:
 *   - Anchor discriminators are computed at module load via the same
 *     `@noble/hashes/sha256` library `decoder/discriminators.ts` uses,
 *     so a future on-chain rename surfaces in both decoders + encoders
 *     simultaneously.
 *   - Borsh args are encoded via `@coral-xyz/borsh` `struct(...)`
 *     layouts that mirror the Rust `BorshSerialize` derives byte-for-byte
 *     (4 × u64 for `PriceEnvelopeArgs`, etc).
 *   - Returns plain `Uint8Array` so callers can build a Solana
 *     `TransactionInstruction` without further conversion.
 */
import {
  bool,
  i64 as borshI64,
  struct,
  u32,
  u64 as borshU64,
} from "@coral-xyz/borsh";
import BN from "bn.js";
import { Buffer } from "buffer";
import { sha256 } from "@noble/hashes/sha256";

const ENCODER = new TextEncoder();

/** Anchor instruction-namespace prefix. */
export const ANCHOR_INSTRUCTION_NAMESPACE = "global:";

/**
 * Compute the 8-byte Anchor instruction discriminator for `ixName`.
 * Matches `sha256("global:<ix>")[..8]` byte-for-byte; pinned by
 * `tx-codec`'s `discriminator_constants_match_sha256_of_anchor_namespace`
 * Rust self-test.
 */
export function deriveAnchorInstructionDiscriminator(
  ixName: string,
): Uint8Array {
  const input = ENCODER.encode(`${ANCHOR_INSTRUCTION_NAMESPACE}${ixName}`);
  return sha256(input).slice(0, 8);
}

/** All 8 mole-option Anchor ix discriminators. */
export interface MoleInstructionDiscriminators {
  syncPool: Uint8Array;
  openPosition: Uint8Array;
  closePosition: Uint8Array;
  claimDormantRecovery: Uint8Array;
  harvestDust: Uint8Array;
  preSyncDormantBucket: Uint8Array;
  closeDormantBucket: Uint8Array;
  initializeDormantBucket: Uint8Array;
}

/**
 * Snapshot of the 8 canonical mole-option ix discriminators,
 * computed once at module load. Frozen so callers can't mutate them
 * (the on-chain bytes never change without a corresponding rename
 * in `programs/mole-option/src/lib.rs`).
 */
export const MOLE_IX_DISCRIMINATORS: Readonly<MoleInstructionDiscriminators> =
  Object.freeze({
    syncPool: deriveAnchorInstructionDiscriminator("sync_pool"),
    openPosition: deriveAnchorInstructionDiscriminator("open_position"),
    closePosition: deriveAnchorInstructionDiscriminator("close_position"),
    claimDormantRecovery: deriveAnchorInstructionDiscriminator(
      "claim_dormant_recovery",
    ),
    harvestDust: deriveAnchorInstructionDiscriminator("harvest_dust"),
    preSyncDormantBucket: deriveAnchorInstructionDiscriminator(
      "pre_sync_dormant_bucket",
    ),
    closeDormantBucket: deriveAnchorInstructionDiscriminator(
      "close_dormant_bucket",
    ),
    initializeDormantBucket: deriveAnchorInstructionDiscriminator(
      "initialize_dormant_bucket",
    ),
  });

// ---------------------------------------------------------------------
// Arg structs (mirror `tx-codec` PriceEnvelopeArgs / OpenParams).
// ---------------------------------------------------------------------

/**
 * Mirror of `tx_codec::PriceEnvelopeArgs`. All four fields encode as
 * little-endian u64.
 */
export interface PriceEnvelopeArgs {
  /** Current oracle price (PRICE_SCALE = 1e8). */
  pNow: bigint;
  /** Slot the price was fetched at. */
  slot: bigint;
  /** Lower bound of the assertion. */
  expectedMin: bigint;
  /** Upper bound of the assertion, inclusive. */
  expectedMax: bigint;
}

/**
 * Mirror of `tx_codec::OpenParams`. Borsh-encoded as
 * `envelope ++ direction_is_long ++ gross_amount ++ position_id`.
 */
export interface OpenParams {
  envelope: PriceEnvelopeArgs;
  /** Long-side iff true. */
  directionIsLong: boolean;
  /** Collateral amount in minor units. */
  grossAmount: bigint;
  /** Caller-allocated position id. */
  positionId: bigint;
}

interface RawOpenParams {
  envelope_p_now: BN;
  envelope_slot: BN;
  envelope_expected_min: BN;
  envelope_expected_max: BN;
  direction_is_long: boolean;
  gross_amount: BN;
  position_id: BN;
}

interface RawEnvelopePlusBuckets {
  envelope_p_now: BN;
  envelope_slot: BN;
  envelope_expected_min: BN;
  envelope_expected_max: BN;
  long_bucket_count: number;
  short_bucket_count: number;
}

interface RawPreSync {
  direction_is_long: boolean;
  tick: BN;
  long_bucket_count: number;
  short_bucket_count: number;
}

interface RawCloseOrInitBucket {
  direction_is_long: boolean;
  tick: BN;
}

interface RawHarvestDust {
  direction_is_long: boolean;
}

const OPEN_PARAMS_LAYOUT = struct<RawOpenParams>([
  borshU64("envelope_p_now"),
  borshU64("envelope_slot"),
  borshU64("envelope_expected_min"),
  borshU64("envelope_expected_max"),
  bool("direction_is_long"),
  borshU64("gross_amount"),
  borshU64("position_id"),
]);

const ENVELOPE_PLUS_BUCKETS_LAYOUT = struct<RawEnvelopePlusBuckets>([
  borshU64("envelope_p_now"),
  borshU64("envelope_slot"),
  borshU64("envelope_expected_min"),
  borshU64("envelope_expected_max"),
  u32("long_bucket_count"),
  u32("short_bucket_count"),
]);

const HARVEST_DUST_BODY_LAYOUT = struct<RawHarvestDust>([
  bool("direction_is_long"),
]);

const PRE_SYNC_BODY_LAYOUT = struct<RawPreSync>([
  bool("direction_is_long"),
  borshI64("tick"),
  u32("long_bucket_count"),
  u32("short_bucket_count"),
]);

const CLOSE_OR_INIT_BUCKET_BODY_LAYOUT = struct<RawCloseOrInitBucket>([
  bool("direction_is_long"),
  borshI64("tick"),
]);

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

function bigIntToBN(value: bigint): BN {
  // Borsh u64 is unsigned; reject negative values up-front.
  if (value < 0n) {
    throw new Error(`expected non-negative bigint, got ${value}`);
  }
  return new BN(value.toString(10), 10);
}

function bigIntToSignedBN(value: bigint): BN {
  // Borsh i64 wraps negative via two's complement; bn.js handles
  // this when given a string with leading '-'.
  return new BN(value.toString(10), 10);
}

interface FlattenedEnvelopeFields {
  envelope_p_now: BN;
  envelope_slot: BN;
  envelope_expected_min: BN;
  envelope_expected_max: BN;
}

function flattenEnvelope(envelope: PriceEnvelopeArgs): FlattenedEnvelopeFields {
  return {
    envelope_p_now: bigIntToBN(envelope.pNow),
    envelope_slot: bigIntToBN(envelope.slot),
    envelope_expected_min: bigIntToBN(envelope.expectedMin),
    envelope_expected_max: bigIntToBN(envelope.expectedMax),
  };
}

function concatDiscAndBody(disc: Uint8Array, body: Uint8Array): Uint8Array {
  const out = new Uint8Array(disc.length + body.length);
  out.set(disc, 0);
  out.set(body, disc.length);
  return out;
}

function encodeBody<T>(
  layout: ReturnType<typeof struct<T>>,
  value: T,
  expectedLen: number,
): Uint8Array {
  const buf = Buffer.alloc(expectedLen);
  const written = layout.encode(value, buf);
  if (written !== expectedLen) {
    throw new Error(
      `borsh layout wrote ${written} bytes, expected ${expectedLen}`,
    );
  }
  return new Uint8Array(buf);
}

// ---------------------------------------------------------------------
// Public encoders — `disc ++ borsh(args)`
// ---------------------------------------------------------------------

/**
 * Encode the `open_position` instruction's `data` blob (no account
 * metas; the caller assembles those when building the
 * `TransactionInstruction`).
 *
 * Wire layout: `disc[8] ++ envelope[32] ++ direction_is_long[1] ++ gross_amount[8] ++ position_id[8]`.
 * Total 57 bytes.
 */
export function encodeOpenPositionIx(params: OpenParams): Uint8Array {
  const raw: RawOpenParams = {
    ...flattenEnvelope(params.envelope),
    direction_is_long: params.directionIsLong,
    gross_amount: bigIntToBN(params.grossAmount),
    position_id: bigIntToBN(params.positionId),
  };
  const body = encodeBody(OPEN_PARAMS_LAYOUT, raw, 32 + 1 + 8 + 8);
  return concatDiscAndBody(MOLE_IX_DISCRIMINATORS.openPosition, body);
}

/**
 * Encode `close_position(envelope, long_bucket_count, short_bucket_count)`.
 *
 * Wire layout: `disc[8] ++ envelope[32] ++ long_bucket_count[4] ++ short_bucket_count[4]`.
 * Total 48 bytes.
 */
export function encodeClosePositionIx(
  envelope: PriceEnvelopeArgs,
  longBucketCount: number,
  shortBucketCount: number,
): Uint8Array {
  return encodeEnvelopePlusBucketCounts(
    MOLE_IX_DISCRIMINATORS.closePosition,
    envelope,
    longBucketCount,
    shortBucketCount,
  );
}

/**
 * Encode `sync_pool(envelope, long_bucket_count, short_bucket_count)`.
 * Same layout as close_position with a different discriminator.
 */
export function encodeSyncPoolIx(
  envelope: PriceEnvelopeArgs,
  longBucketCount: number,
  shortBucketCount: number,
): Uint8Array {
  return encodeEnvelopePlusBucketCounts(
    MOLE_IX_DISCRIMINATORS.syncPool,
    envelope,
    longBucketCount,
    shortBucketCount,
  );
}

/**
 * Encode `claim_dormant_recovery(envelope, long_bucket_count, short_bucket_count)`.
 */
export function encodeClaimDormantRecoveryIx(
  envelope: PriceEnvelopeArgs,
  longBucketCount: number,
  shortBucketCount: number,
): Uint8Array {
  return encodeEnvelopePlusBucketCounts(
    MOLE_IX_DISCRIMINATORS.claimDormantRecovery,
    envelope,
    longBucketCount,
    shortBucketCount,
  );
}

function encodeEnvelopePlusBucketCounts(
  disc: Uint8Array,
  envelope: PriceEnvelopeArgs,
  longBucketCount: number,
  shortBucketCount: number,
): Uint8Array {
  if (
    !Number.isInteger(longBucketCount) ||
    !Number.isInteger(shortBucketCount) ||
    longBucketCount < 0 ||
    shortBucketCount < 0 ||
    longBucketCount > 0xffff_ffff ||
    shortBucketCount > 0xffff_ffff
  ) {
    throw new Error(
      `bucket counts must be u32 integers in [0, 2^32-1]; got long=${longBucketCount} short=${shortBucketCount}`,
    );
  }
  const body = encodeBody(
    ENVELOPE_PLUS_BUCKETS_LAYOUT,
    {
      ...flattenEnvelope(envelope),
      long_bucket_count: longBucketCount,
      short_bucket_count: shortBucketCount,
    },
    32 + 4 + 4,
  );
  return concatDiscAndBody(disc, body);
}

/** Encode `harvest_dust(direction_is_long: bool)` — 9 bytes total. */
export function encodeHarvestDustIx(directionIsLong: boolean): Uint8Array {
  const body = encodeBody(
    HARVEST_DUST_BODY_LAYOUT,
    { direction_is_long: directionIsLong },
    1,
  );
  return concatDiscAndBody(MOLE_IX_DISCRIMINATORS.harvestDust, body);
}

/**
 * Encode `pre_sync_dormant_bucket(direction_is_long, tick, long_bucket_count, short_bucket_count)`.
 * Wire layout: `disc[8] ++ direction_is_long[1] ++ tick[8] ++ long_bucket_count[4] ++ short_bucket_count[4]`.
 */
export function encodePreSyncDormantBucketIx(
  directionIsLong: boolean,
  tick: bigint,
  longBucketCount: number,
  shortBucketCount: number,
): Uint8Array {
  if (
    !Number.isInteger(longBucketCount) ||
    !Number.isInteger(shortBucketCount) ||
    longBucketCount < 0 ||
    shortBucketCount < 0 ||
    longBucketCount > 0xffff_ffff ||
    shortBucketCount > 0xffff_ffff
  ) {
    throw new Error(
      `bucket counts must be u32 integers in [0, 2^32-1]; got long=${longBucketCount} short=${shortBucketCount}`,
    );
  }
  const body = encodeBody(
    PRE_SYNC_BODY_LAYOUT,
    {
      direction_is_long: directionIsLong,
      tick: bigIntToSignedBN(tick),
      long_bucket_count: longBucketCount,
      short_bucket_count: shortBucketCount,
    },
    1 + 8 + 4 + 4,
  );
  return concatDiscAndBody(MOLE_IX_DISCRIMINATORS.preSyncDormantBucket, body);
}

/** Encode `close_dormant_bucket(direction_is_long, tick)`. */
export function encodeCloseDormantBucketIx(
  directionIsLong: boolean,
  tick: bigint,
): Uint8Array {
  const body = encodeBody(
    CLOSE_OR_INIT_BUCKET_BODY_LAYOUT,
    { direction_is_long: directionIsLong, tick: bigIntToSignedBN(tick) },
    1 + 8,
  );
  return concatDiscAndBody(MOLE_IX_DISCRIMINATORS.closeDormantBucket, body);
}

/** Encode `initialize_dormant_bucket(direction_is_long, tick)`. */
export function encodeInitializeDormantBucketIx(
  directionIsLong: boolean,
  tick: bigint,
): Uint8Array {
  const body = encodeBody(
    CLOSE_OR_INIT_BUCKET_BODY_LAYOUT,
    { direction_is_long: directionIsLong, tick: bigIntToSignedBN(tick) },
    1 + 8,
  );
  return concatDiscAndBody(
    MOLE_IX_DISCRIMINATORS.initializeDormantBucket,
    body,
  );
}

/**
 * Schema descriptor mirroring `tx_codec::schema_descriptor_json()`.
 * Frontend tests pin this against the Rust output to catch silent
 * field-order drift.
 */
export const SCHEMA_DESCRIPTOR = Object.freeze({
  PriceEnvelopeArgs: [
    { name: "p_now", type: "u64" },
    { name: "slot", type: "u64" },
    { name: "expected_min", type: "u64" },
    { name: "expected_max", type: "u64" },
  ],
  OpenParams: [
    { name: "envelope", type: "PriceEnvelopeArgs" },
    { name: "direction_is_long", type: "bool" },
    { name: "gross_amount", type: "u64" },
    { name: "position_id", type: "u64" },
  ],
  Discriminators: [
    { name: "DISC_SYNC_POOL", type: "8B" },
    { name: "DISC_OPEN_POSITION", type: "8B" },
    { name: "DISC_CLOSE_POSITION", type: "8B" },
    { name: "DISC_CLAIM_DORMANT_RECOVERY", type: "8B" },
    { name: "DISC_HARVEST_DUST", type: "8B" },
    { name: "DISC_PRE_SYNC_DORMANT_BUCKET", type: "8B" },
    { name: "DISC_CLOSE_DORMANT_BUCKET", type: "8B" },
    { name: "DISC_INITIALIZE_DORMANT_BUCKET", type: "8B" },
  ],
});
