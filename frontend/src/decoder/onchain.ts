/**
 * Wave 14 — TypeScript Borsh decoder for the on-chain Anchor accounts.
 *
 * Hand-rolled mirror of `crates/keeper-decoder/src/lib.rs` until
 * wave 15 ships the wasm-pack artifact. Field order MUST match the
 * Rust source exactly; the test suite at
 * `frontend/src/decoder/onchain.test.ts` enforces this against a
 * snapshot derived from `keeper_decoder::schema_descriptor_json()`.
 *
 * Implementation notes:
 *   - `@coral-xyz/borsh` v0.30 returns `BN` for u64/u128/i64; we
 *     expose `bigint` to consumers via the `decodeOnchain*` wrappers.
 *   - Fixed-size byte arrays (`Pubkey32`, `_pad`, `symbol`) use
 *     `buffer-layout`'s `blob(N, name)` primitive which decodes to
 *     `Buffer` — exactly what Borsh's `[u8; N]` produces.
 *   - The raw layouts (`*_RAW_LAYOUT`) decode the on-wire payload as-is
 *     (BN + Buffer); the public `OnchainSubPool`-shape returns
 *     bigints + hex-encoded pubkeys so `frontend/src/types.ts`
 *     consumers don't need bn.js.
 */
import {
  bool,
  i64 as borshI64,
  struct,
  u8,
  u16,
  u32,
  u64 as borshU64,
  u128 as borshU128,
} from "@coral-xyz/borsh";
import bufferLayout from "buffer-layout";
import BN from "bn.js";
import { Buffer } from "buffer";

const blob = (
  bufferLayout as unknown as {
    blob: (length: number, property: string) => unknown;
  }
).blob;

/** Anchor account-discriminator length (bytes). */
export const ANCHOR_DISCRIMINATOR_LEN = 8;

/** 32-byte pubkey, hex-lowercased to mirror `frontend/src/types.ts::Pubkey32`. */
export interface Pubkey32 {
  hex: string;
}

// ---------------------------------------------------------------------
// Raw on-wire shapes (exactly what `buffer-layout` returns)
// ---------------------------------------------------------------------

interface RawOnchainSubPool {
  market: Buffer;
  sub_pool_id: number;
  long_pool_equity: BN;
  short_pool_equity: BN;
  long_active_shares: BN;
  short_active_shares: BN;
  long_recovery_shares: BN;
  short_recovery_shares: BN;
  long_active_notional: BN;
  short_active_notional: BN;
  long_active_generation: BN;
  short_active_generation: BN;
  last_price: BN;
  last_sync_slot: BN;
  long_dust: BN;
  short_dust: BN;
  long_dormant_bucket_count: number;
  short_dormant_bucket_count: number;
  bump: number;
  _pad: Buffer;
}

interface RawOnchainDormantBucket {
  sub_pool: Buffer;
  direction_is_long: boolean;
  zero_price_tick: BN;
  anchor_price: BN;
  total_recovery_shares: BN;
  total_recovery_notional: BN;
  accrued_value: BN;
  position_count: BN;
  last_applied_index: BN;
  bump: number;
  _pad: Buffer;
}

interface RawOnchainDistEntry {
  event_index: BN;
  p_at_event: BN;
  total_outstanding_at_event: BN;
  total_alloc_input: BN;
  allocated_sum_observed: BN;
}

interface RawOnchainPosition {
  owner: Buffer;
  market: Buffer;
  sub_pool: Buffer;
  position_id: BN;
  direction_is_long: boolean;
  status: number;
  principal: BN;
  leverage_bps: number;
  notional: BN;
  active_shares: BN;
  recovery_shares: BN;
  recovery_bucket_tick: BN;
  has_recovery_bucket: boolean;
  zero_price: BN;
  entry_price: BN;
  last_sync_slot: BN;
  active_generation: BN;
  opened_at: BN;
  updated_at: BN;
  closed_at: BN;
  schema_version: number;
  bump: number;
  _pad: Buffer;
}

interface RawOnchainMarket {
  global_config: Buffer;
  symbol: Buffer;
  collateral_mint: Buffer;
  vault: Buffer;
  fee_vault: Buffer;
  oracle_price_feed: Buffer;
  oracle_program_id: Buffer;
  leverage_bps: number;
  min_margin: BN;
  max_margin_per_position: BN;
  max_total_principal: BN;
  max_total_notional: BN;
  current_total_principal: BN;
  current_total_notional: BN;
  open_fee_bps: number;
  max_oracle_age_seconds: BN;
  max_oracle_age_slots: BN;
  max_confidence_bps: number;
  max_price_move_bps_per_sync: number;
  price_tick: BN;
  tick_aggregation_factor: number;
  max_dormant_bucket_count_per_direction: number;
  dilution_safety_bps: number;
  max_idle_slots: BN;
  paused: boolean;
  frozen_new_position: boolean;
  schema_version: number;
  sub_pool_count: number;
  dormant_distribute_mode: number;
  max_pending_apply_per_tx: number;
  max_distribution_ledger_size: number;
  bump: number;
  _pad: Buffer;
}

// ---------------------------------------------------------------------
// Raw layouts (field order MUST match crates/keeper-decoder/src/lib.rs)
// ---------------------------------------------------------------------

export const SUB_POOL_RAW_LAYOUT = struct<RawOnchainSubPool>([
  blob(32, "market") as never,
  u32("sub_pool_id"),
  borshU128("long_pool_equity"),
  borshU128("short_pool_equity"),
  borshU128("long_active_shares"),
  borshU128("short_active_shares"),
  borshU128("long_recovery_shares"),
  borshU128("short_recovery_shares"),
  borshU128("long_active_notional"),
  borshU128("short_active_notional"),
  borshU64("long_active_generation"),
  borshU64("short_active_generation"),
  borshU64("last_price"),
  borshU64("last_sync_slot"),
  borshU128("long_dust"),
  borshU128("short_dust"),
  u32("long_dormant_bucket_count"),
  u32("short_dormant_bucket_count"),
  u8("bump"),
  blob(7, "_pad") as never,
]);

export const DORMANT_BUCKET_RAW_LAYOUT = struct<RawOnchainDormantBucket>([
  blob(32, "sub_pool") as never,
  bool("direction_is_long"),
  borshI64("zero_price_tick"),
  borshU64("anchor_price"),
  borshU128("total_recovery_shares"),
  borshU128("total_recovery_notional"),
  borshU128("accrued_value"),
  borshU64("position_count"),
  borshU64("last_applied_index"),
  u8("bump"),
  blob(6, "_pad") as never,
]);

export const DIST_ENTRY_RAW_LAYOUT = struct<RawOnchainDistEntry>([
  borshU64("event_index"),
  borshU64("p_at_event"),
  borshU128("total_outstanding_at_event"),
  borshU128("total_alloc_input"),
  borshU128("allocated_sum_observed"),
]);

export const POSITION_RAW_LAYOUT = struct<RawOnchainPosition>([
  blob(32, "owner") as never,
  blob(32, "market") as never,
  blob(32, "sub_pool") as never,
  borshU64("position_id"),
  bool("direction_is_long"),
  u8("status"),
  borshU64("principal"),
  u32("leverage_bps"),
  borshU128("notional"),
  borshU128("active_shares"),
  borshU128("recovery_shares"),
  borshI64("recovery_bucket_tick"),
  bool("has_recovery_bucket"),
  borshU64("zero_price"),
  borshU64("entry_price"),
  borshU64("last_sync_slot"),
  borshU64("active_generation"),
  borshI64("opened_at"),
  borshI64("updated_at"),
  borshI64("closed_at"),
  u16("schema_version"),
  u8("bump"),
  blob(5, "_pad") as never,
]);

export const MARKET_RAW_LAYOUT = struct<RawOnchainMarket>([
  blob(32, "global_config") as never,
  blob(16, "symbol") as never,
  blob(32, "collateral_mint") as never,
  blob(32, "vault") as never,
  blob(32, "fee_vault") as never,
  blob(32, "oracle_price_feed") as never,
  blob(32, "oracle_program_id") as never,
  u32("leverage_bps"),
  borshU64("min_margin"),
  borshU64("max_margin_per_position"),
  borshU128("max_total_principal"),
  borshU128("max_total_notional"),
  borshU128("current_total_principal"),
  borshU128("current_total_notional"),
  u16("open_fee_bps"),
  borshI64("max_oracle_age_seconds"),
  borshU64("max_oracle_age_slots"),
  u16("max_confidence_bps"),
  u32("max_price_move_bps_per_sync"),
  borshU64("price_tick"),
  u32("tick_aggregation_factor"),
  u32("max_dormant_bucket_count_per_direction"),
  u32("dilution_safety_bps"),
  borshU64("max_idle_slots"),
  bool("paused"),
  bool("frozen_new_position"),
  u16("schema_version"),
  u32("sub_pool_count"),
  u8("dormant_distribute_mode"),
  u32("max_pending_apply_per_tx"),
  u32("max_distribution_ledger_size"),
  u8("bump"),
  blob(2, "_pad") as never,
]);

// ---------------------------------------------------------------------
// Public shapes — bigint + Pubkey32 hex string
// ---------------------------------------------------------------------

export interface OnchainSubPool {
  market: Pubkey32;
  sub_pool_id: number;
  long_pool_equity: bigint;
  short_pool_equity: bigint;
  long_active_shares: bigint;
  short_active_shares: bigint;
  long_recovery_shares: bigint;
  short_recovery_shares: bigint;
  long_active_notional: bigint;
  short_active_notional: bigint;
  long_active_generation: bigint;
  short_active_generation: bigint;
  last_price: bigint;
  last_sync_slot: bigint;
  long_dust: bigint;
  short_dust: bigint;
  long_dormant_bucket_count: number;
  short_dormant_bucket_count: number;
  bump: number;
  _pad: Buffer;
}

export interface OnchainDormantBucket {
  sub_pool: Pubkey32;
  direction_is_long: boolean;
  zero_price_tick: bigint;
  anchor_price: bigint;
  total_recovery_shares: bigint;
  total_recovery_notional: bigint;
  accrued_value: bigint;
  position_count: bigint;
  last_applied_index: bigint;
  bump: number;
  _pad: Buffer;
}

export interface OnchainDistEntry {
  event_index: bigint;
  p_at_event: bigint;
  total_outstanding_at_event: bigint;
  total_alloc_input: bigint;
  allocated_sum_observed: bigint;
}

/**
 * Wave 21 — public shape of `OnchainPosition`. Mirrors
 * `crates/keeper-decoder/src/lib.rs::OnchainPosition` byte-for-
 * byte. The wave-22 `websocketAdapter` calls
 * [`decodeOnchainPosition`] and lifts `position.market.hex` into
 * `PositionSummary.marketPdaHex`, which the wave-20
 * `selectActiveMarketSnapshot` filter then keys off.
 */
export interface OnchainPosition {
  owner: Pubkey32;
  market: Pubkey32;
  sub_pool: Pubkey32;
  position_id: bigint;
  direction_is_long: boolean;
  status: number;
  principal: bigint;
  leverage_bps: number;
  notional: bigint;
  active_shares: bigint;
  recovery_shares: bigint;
  recovery_bucket_tick: bigint;
  has_recovery_bucket: boolean;
  zero_price: bigint;
  entry_price: bigint;
  last_sync_slot: bigint;
  active_generation: bigint;
  opened_at: bigint;
  updated_at: bigint;
  closed_at: bigint;
  schema_version: number;
  bump: number;
  _pad: Buffer;
}

export interface OnchainMarket {
  global_config: Pubkey32;
  symbol: Buffer;
  collateral_mint: Pubkey32;
  vault: Pubkey32;
  fee_vault: Pubkey32;
  oracle_price_feed: Pubkey32;
  oracle_program_id: Pubkey32;
  leverage_bps: number;
  min_margin: bigint;
  max_margin_per_position: bigint;
  max_total_principal: bigint;
  max_total_notional: bigint;
  current_total_principal: bigint;
  current_total_notional: bigint;
  open_fee_bps: number;
  max_oracle_age_seconds: bigint;
  max_oracle_age_slots: bigint;
  max_confidence_bps: number;
  max_price_move_bps_per_sync: number;
  price_tick: bigint;
  tick_aggregation_factor: number;
  max_dormant_bucket_count_per_direction: number;
  dilution_safety_bps: number;
  max_idle_slots: bigint;
  paused: boolean;
  frozen_new_position: boolean;
  schema_version: number;
  sub_pool_count: number;
  dormant_distribute_mode: number;
  max_pending_apply_per_tx: number;
  max_distribution_ledger_size: number;
  bump: number;
  _pad: Buffer;
}

// ---------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------

function bnToBigInt(bn: BN): bigint {
  // `BN.toString()` emits decimal that BigInt understands. For BN
  // values that came from a signed i64 layout, `@coral-xyz/borsh` has
  // already applied the sign extension so the BN is negative when
  // appropriate.
  return BigInt(bn.toString());
}

function pubkey32FromBytes(buf: Buffer): Pubkey32 {
  if (buf.length !== 32) {
    throw new Error(`pubkey32: expected 32 bytes, got ${buf.length}`);
  }
  return { hex: buf.toString("hex") };
}

function pubkey32ToBytes(pk: Pubkey32): Buffer {
  const buf = Buffer.from(pk.hex, "hex");
  if (buf.length !== 32) {
    throw new Error(
      `pubkey32: expected 32-byte hex, got ${buf.length} bytes`,
    );
  }
  return buf;
}

// ---------------------------------------------------------------------
// Decode error class
// ---------------------------------------------------------------------

export class AccountDecodeError extends Error {
  readonly kind: "TooShort" | "DiscriminatorMismatch" | "Borsh";
  readonly detail: string;

  constructor(kind: AccountDecodeError["kind"], detail: string) {
    super(`AccountDecodeError(${kind}): ${detail}`);
    this.kind = kind;
    this.detail = detail;
  }
}

// ---------------------------------------------------------------------
// Top-level decode helpers
// ---------------------------------------------------------------------

export interface RawLayout<T> {
  decode(buffer: Buffer, offset?: number): T;
  encode(value: T, buffer: Buffer, offset?: number): number;
  span?: number;
}

function decodeAnchorRaw<T>(layout: RawLayout<T>, data: Uint8Array): T {
  if (data.length < ANCHOR_DISCRIMINATOR_LEN) {
    throw new AccountDecodeError(
      "TooShort",
      `got ${data.length} bytes, need at least ${ANCHOR_DISCRIMINATOR_LEN}`,
    );
  }
  const body = Buffer.from(
    data.buffer,
    data.byteOffset + ANCHOR_DISCRIMINATOR_LEN,
    data.length - ANCHOR_DISCRIMINATOR_LEN,
  );
  try {
    return layout.decode(body);
  } catch (e) {
    throw new AccountDecodeError(
      "Borsh",
      e instanceof Error ? e.message : String(e),
    );
  }
}

function checkDiscriminator(data: Uint8Array, expected: Uint8Array): void {
  if (data.length < ANCHOR_DISCRIMINATOR_LEN) {
    throw new AccountDecodeError(
      "TooShort",
      `got ${data.length} bytes, need at least ${ANCHOR_DISCRIMINATOR_LEN}`,
    );
  }
  if (expected.length !== ANCHOR_DISCRIMINATOR_LEN) {
    throw new AccountDecodeError(
      "DiscriminatorMismatch",
      `expected discriminator must be 8 bytes, got ${expected.length}`,
    );
  }
  for (let i = 0; i < ANCHOR_DISCRIMINATOR_LEN; i++) {
    const got = data[i]!;
    const want = expected[i]!;
    if (got !== want) {
      throw new AccountDecodeError(
        "DiscriminatorMismatch",
        `byte ${i}: got 0x${got.toString(16)}, expected 0x${want.toString(16)}`,
      );
    }
  }
}

// ---------------------------------------------------------------------
// Public decoders — bigint + Pubkey32 hex
// ---------------------------------------------------------------------

export function decodeOnchainSubPool(data: Uint8Array): OnchainSubPool {
  const r = decodeAnchorRaw(SUB_POOL_RAW_LAYOUT as unknown as RawLayout<RawOnchainSubPool>, data);
  return {
    market: pubkey32FromBytes(r.market),
    sub_pool_id: r.sub_pool_id,
    long_pool_equity: bnToBigInt(r.long_pool_equity),
    short_pool_equity: bnToBigInt(r.short_pool_equity),
    long_active_shares: bnToBigInt(r.long_active_shares),
    short_active_shares: bnToBigInt(r.short_active_shares),
    long_recovery_shares: bnToBigInt(r.long_recovery_shares),
    short_recovery_shares: bnToBigInt(r.short_recovery_shares),
    long_active_notional: bnToBigInt(r.long_active_notional),
    short_active_notional: bnToBigInt(r.short_active_notional),
    long_active_generation: bnToBigInt(r.long_active_generation),
    short_active_generation: bnToBigInt(r.short_active_generation),
    last_price: bnToBigInt(r.last_price),
    last_sync_slot: bnToBigInt(r.last_sync_slot),
    long_dust: bnToBigInt(r.long_dust),
    short_dust: bnToBigInt(r.short_dust),
    long_dormant_bucket_count: r.long_dormant_bucket_count,
    short_dormant_bucket_count: r.short_dormant_bucket_count,
    bump: r.bump,
    _pad: Buffer.from(r._pad),
  };
}

export function decodeOnchainDormantBucket(
  data: Uint8Array,
): OnchainDormantBucket {
  const r = decodeAnchorRaw(
    DORMANT_BUCKET_RAW_LAYOUT as unknown as RawLayout<RawOnchainDormantBucket>,
    data,
  );
  return {
    sub_pool: pubkey32FromBytes(r.sub_pool),
    direction_is_long: r.direction_is_long,
    zero_price_tick: bnToBigInt(r.zero_price_tick),
    anchor_price: bnToBigInt(r.anchor_price),
    total_recovery_shares: bnToBigInt(r.total_recovery_shares),
    total_recovery_notional: bnToBigInt(r.total_recovery_notional),
    accrued_value: bnToBigInt(r.accrued_value),
    position_count: bnToBigInt(r.position_count),
    last_applied_index: bnToBigInt(r.last_applied_index),
    bump: r.bump,
    _pad: Buffer.from(r._pad),
  };
}

export function decodeOnchainDistEntry(buffer: Buffer): OnchainDistEntry {
  const r = (DIST_ENTRY_RAW_LAYOUT as unknown as RawLayout<RawOnchainDistEntry>).decode(buffer);
  return {
    event_index: bnToBigInt(r.event_index),
    p_at_event: bnToBigInt(r.p_at_event),
    total_outstanding_at_event: bnToBigInt(r.total_outstanding_at_event),
    total_alloc_input: bnToBigInt(r.total_alloc_input),
    allocated_sum_observed: bnToBigInt(r.allocated_sum_observed),
  };
}

export function decodeOnchainMarket(data: Uint8Array): OnchainMarket {
  const r = decodeAnchorRaw(
    MARKET_RAW_LAYOUT as unknown as RawLayout<RawOnchainMarket>,
    data,
  );
  return {
    global_config: pubkey32FromBytes(r.global_config),
    symbol: Buffer.from(r.symbol),
    collateral_mint: pubkey32FromBytes(r.collateral_mint),
    vault: pubkey32FromBytes(r.vault),
    fee_vault: pubkey32FromBytes(r.fee_vault),
    oracle_price_feed: pubkey32FromBytes(r.oracle_price_feed),
    oracle_program_id: pubkey32FromBytes(r.oracle_program_id),
    leverage_bps: r.leverage_bps,
    min_margin: bnToBigInt(r.min_margin),
    max_margin_per_position: bnToBigInt(r.max_margin_per_position),
    max_total_principal: bnToBigInt(r.max_total_principal),
    max_total_notional: bnToBigInt(r.max_total_notional),
    current_total_principal: bnToBigInt(r.current_total_principal),
    current_total_notional: bnToBigInt(r.current_total_notional),
    open_fee_bps: r.open_fee_bps,
    max_oracle_age_seconds: bnToBigInt(r.max_oracle_age_seconds),
    max_oracle_age_slots: bnToBigInt(r.max_oracle_age_slots),
    max_confidence_bps: r.max_confidence_bps,
    max_price_move_bps_per_sync: r.max_price_move_bps_per_sync,
    price_tick: bnToBigInt(r.price_tick),
    tick_aggregation_factor: r.tick_aggregation_factor,
    max_dormant_bucket_count_per_direction:
      r.max_dormant_bucket_count_per_direction,
    dilution_safety_bps: r.dilution_safety_bps,
    max_idle_slots: bnToBigInt(r.max_idle_slots),
    paused: r.paused,
    frozen_new_position: r.frozen_new_position,
    schema_version: r.schema_version,
    sub_pool_count: r.sub_pool_count,
    dormant_distribute_mode: r.dormant_distribute_mode,
    max_pending_apply_per_tx: r.max_pending_apply_per_tx,
    max_distribution_ledger_size: r.max_distribution_ledger_size,
    bump: r.bump,
    _pad: Buffer.from(r._pad),
  };
}

/**
 * Wave 21 — decode an Anchor-prefixed `Position` PDA payload
 * (8-byte discriminator + 239-byte body) into the public
 * [`OnchainPosition`] shape with bigints + hex pubkeys.
 *
 * The wave-22 `websocketAdapter` will call this on every
 * `accountSubscribe` notification for `Position` PDAs and
 * splice the result into `feed.positions` (with
 * `marketPdaHex = position.market.hex`).
 */
export function decodeOnchainPosition(data: Uint8Array): OnchainPosition {
  const r = decodeAnchorRaw(
    POSITION_RAW_LAYOUT as unknown as RawLayout<RawOnchainPosition>,
    data,
  );
  return {
    owner: pubkey32FromBytes(r.owner),
    market: pubkey32FromBytes(r.market),
    sub_pool: pubkey32FromBytes(r.sub_pool),
    position_id: bnToBigInt(r.position_id),
    direction_is_long: r.direction_is_long,
    status: r.status,
    principal: bnToBigInt(r.principal),
    leverage_bps: r.leverage_bps,
    notional: bnToBigInt(r.notional),
    active_shares: bnToBigInt(r.active_shares),
    recovery_shares: bnToBigInt(r.recovery_shares),
    recovery_bucket_tick: bnToBigInt(r.recovery_bucket_tick),
    has_recovery_bucket: r.has_recovery_bucket,
    zero_price: bnToBigInt(r.zero_price),
    entry_price: bnToBigInt(r.entry_price),
    last_sync_slot: bnToBigInt(r.last_sync_slot),
    active_generation: bnToBigInt(r.active_generation),
    opened_at: bnToBigInt(r.opened_at),
    updated_at: bnToBigInt(r.updated_at),
    closed_at: bnToBigInt(r.closed_at),
    schema_version: r.schema_version,
    bump: r.bump,
    _pad: Buffer.from(r._pad),
  };
}

/**
 * Wave 21 — strict variant of [`decodeOnchainPosition`] that
 * rejects payloads whose discriminator doesn't match. Production
 * subscribers MUST use this path so a misrouted account (e.g. a
 * `Market` PDA accidentally fed into a position stream) fails
 * loudly instead of decoding into garbage data.
 */
export function decodeOnchainPositionWithDiscriminator(
  data: Uint8Array,
  expected: Uint8Array,
): OnchainPosition {
  checkDiscriminator(data, expected);
  return decodeOnchainPosition(data);
}

export function decodeOnchainSubPoolWithDiscriminator(
  data: Uint8Array,
  expected: Uint8Array,
): OnchainSubPool {
  checkDiscriminator(data, expected);
  return decodeOnchainSubPool(data);
}

export function decodeOnchainMarketWithDiscriminator(
  data: Uint8Array,
  expected: Uint8Array,
): OnchainMarket {
  checkDiscriminator(data, expected);
  return decodeOnchainMarket(data);
}

// ---------------------------------------------------------------------
// Encoders (test-only — used to build round-trip fixtures)
// ---------------------------------------------------------------------

function bigintToBN(value: bigint): BN {
  return new BN(value.toString());
}

export function encodeOnchainSubPool(
  value: OnchainSubPool,
  discriminator: Uint8Array,
): Uint8Array {
  if (discriminator.length !== ANCHOR_DISCRIMINATOR_LEN) {
    throw new AccountDecodeError(
      "DiscriminatorMismatch",
      `discriminator must be 8 bytes, got ${discriminator.length}`,
    );
  }
  const raw: RawOnchainSubPool = {
    market: pubkey32ToBytes(value.market),
    sub_pool_id: value.sub_pool_id,
    long_pool_equity: bigintToBN(value.long_pool_equity),
    short_pool_equity: bigintToBN(value.short_pool_equity),
    long_active_shares: bigintToBN(value.long_active_shares),
    short_active_shares: bigintToBN(value.short_active_shares),
    long_recovery_shares: bigintToBN(value.long_recovery_shares),
    short_recovery_shares: bigintToBN(value.short_recovery_shares),
    long_active_notional: bigintToBN(value.long_active_notional),
    short_active_notional: bigintToBN(value.short_active_notional),
    long_active_generation: bigintToBN(value.long_active_generation),
    short_active_generation: bigintToBN(value.short_active_generation),
    last_price: bigintToBN(value.last_price),
    last_sync_slot: bigintToBN(value.last_sync_slot),
    long_dust: bigintToBN(value.long_dust),
    short_dust: bigintToBN(value.short_dust),
    long_dormant_bucket_count: value.long_dormant_bucket_count,
    short_dormant_bucket_count: value.short_dormant_bucket_count,
    bump: value.bump,
    _pad: Buffer.from(value._pad),
  };
  const tmp = Buffer.alloc(2048);
  const written = (
    SUB_POOL_RAW_LAYOUT as unknown as RawLayout<RawOnchainSubPool>
  ).encode(raw, tmp, 0);
  const out = new Uint8Array(ANCHOR_DISCRIMINATOR_LEN + written);
  out.set(discriminator, 0);
  out.set(tmp.subarray(0, written), ANCHOR_DISCRIMINATOR_LEN);
  return out;
}

export function encodeOnchainDormantBucket(
  value: OnchainDormantBucket,
  discriminator: Uint8Array,
): Uint8Array {
  if (discriminator.length !== ANCHOR_DISCRIMINATOR_LEN) {
    throw new AccountDecodeError(
      "DiscriminatorMismatch",
      `discriminator must be 8 bytes, got ${discriminator.length}`,
    );
  }
  const raw: RawOnchainDormantBucket = {
    sub_pool: pubkey32ToBytes(value.sub_pool),
    direction_is_long: value.direction_is_long,
    zero_price_tick: bigintToBN(value.zero_price_tick),
    anchor_price: bigintToBN(value.anchor_price),
    total_recovery_shares: bigintToBN(value.total_recovery_shares),
    total_recovery_notional: bigintToBN(value.total_recovery_notional),
    accrued_value: bigintToBN(value.accrued_value),
    position_count: bigintToBN(value.position_count),
    last_applied_index: bigintToBN(value.last_applied_index),
    bump: value.bump,
    _pad: Buffer.from(value._pad),
  };
  const tmp = Buffer.alloc(2048);
  const written = (
    DORMANT_BUCKET_RAW_LAYOUT as unknown as RawLayout<RawOnchainDormantBucket>
  ).encode(raw, tmp, 0);
  const out = new Uint8Array(ANCHOR_DISCRIMINATOR_LEN + written);
  out.set(discriminator, 0);
  out.set(tmp.subarray(0, written), ANCHOR_DISCRIMINATOR_LEN);
  return out;
}

/**
 * Wave 21 — encode an [`OnchainPosition`] into a
 * discriminator-prefixed Anchor account payload. Used by the
 * test fixtures (round-trip parity); production code never needs
 * to encode a position (the on-chain program does that).
 */
export function encodeOnchainPosition(
  value: OnchainPosition,
  discriminator: Uint8Array,
): Uint8Array {
  if (discriminator.length !== ANCHOR_DISCRIMINATOR_LEN) {
    throw new AccountDecodeError(
      "DiscriminatorMismatch",
      `discriminator must be 8 bytes, got ${discriminator.length}`,
    );
  }
  const raw: RawOnchainPosition = {
    owner: pubkey32ToBytes(value.owner),
    market: pubkey32ToBytes(value.market),
    sub_pool: pubkey32ToBytes(value.sub_pool),
    position_id: bigintToBN(value.position_id),
    direction_is_long: value.direction_is_long,
    status: value.status,
    principal: bigintToBN(value.principal),
    leverage_bps: value.leverage_bps,
    notional: bigintToBN(value.notional),
    active_shares: bigintToBN(value.active_shares),
    recovery_shares: bigintToBN(value.recovery_shares),
    recovery_bucket_tick: bigintToBN(value.recovery_bucket_tick),
    has_recovery_bucket: value.has_recovery_bucket,
    zero_price: bigintToBN(value.zero_price),
    entry_price: bigintToBN(value.entry_price),
    last_sync_slot: bigintToBN(value.last_sync_slot),
    active_generation: bigintToBN(value.active_generation),
    opened_at: bigintToBN(value.opened_at),
    updated_at: bigintToBN(value.updated_at),
    closed_at: bigintToBN(value.closed_at),
    schema_version: value.schema_version,
    bump: value.bump,
    _pad: Buffer.from(value._pad),
  };
  const tmp = Buffer.alloc(2048);
  const written = (
    POSITION_RAW_LAYOUT as unknown as RawLayout<RawOnchainPosition>
  ).encode(raw, tmp, 0);
  const out = new Uint8Array(ANCHOR_DISCRIMINATOR_LEN + written);
  out.set(discriminator, 0);
  out.set(tmp.subarray(0, written), ANCHOR_DISCRIMINATOR_LEN);
  return out;
}

export function encodeOnchainMarket(
  value: OnchainMarket,
  discriminator: Uint8Array,
): Uint8Array {
  if (discriminator.length !== ANCHOR_DISCRIMINATOR_LEN) {
    throw new AccountDecodeError(
      "DiscriminatorMismatch",
      `discriminator must be 8 bytes, got ${discriminator.length}`,
    );
  }
  const raw: RawOnchainMarket = {
    global_config: pubkey32ToBytes(value.global_config),
    symbol: Buffer.from(value.symbol),
    collateral_mint: pubkey32ToBytes(value.collateral_mint),
    vault: pubkey32ToBytes(value.vault),
    fee_vault: pubkey32ToBytes(value.fee_vault),
    oracle_price_feed: pubkey32ToBytes(value.oracle_price_feed),
    oracle_program_id: pubkey32ToBytes(value.oracle_program_id),
    leverage_bps: value.leverage_bps,
    min_margin: bigintToBN(value.min_margin),
    max_margin_per_position: bigintToBN(value.max_margin_per_position),
    max_total_principal: bigintToBN(value.max_total_principal),
    max_total_notional: bigintToBN(value.max_total_notional),
    current_total_principal: bigintToBN(value.current_total_principal),
    current_total_notional: bigintToBN(value.current_total_notional),
    open_fee_bps: value.open_fee_bps,
    max_oracle_age_seconds: bigintToBN(value.max_oracle_age_seconds),
    max_oracle_age_slots: bigintToBN(value.max_oracle_age_slots),
    max_confidence_bps: value.max_confidence_bps,
    max_price_move_bps_per_sync: value.max_price_move_bps_per_sync,
    price_tick: bigintToBN(value.price_tick),
    tick_aggregation_factor: value.tick_aggregation_factor,
    max_dormant_bucket_count_per_direction:
      value.max_dormant_bucket_count_per_direction,
    dilution_safety_bps: value.dilution_safety_bps,
    max_idle_slots: bigintToBN(value.max_idle_slots),
    paused: value.paused,
    frozen_new_position: value.frozen_new_position,
    schema_version: value.schema_version,
    sub_pool_count: value.sub_pool_count,
    dormant_distribute_mode: value.dormant_distribute_mode,
    max_pending_apply_per_tx: value.max_pending_apply_per_tx,
    max_distribution_ledger_size: value.max_distribution_ledger_size,
    bump: value.bump,
    _pad: Buffer.from(value._pad),
  };
  const tmp = Buffer.alloc(2048);
  const written = (
    MARKET_RAW_LAYOUT as unknown as RawLayout<RawOnchainMarket>
  ).encode(raw, tmp, 0);
  const out = new Uint8Array(ANCHOR_DISCRIMINATOR_LEN + written);
  out.set(discriminator, 0);
  out.set(tmp.subarray(0, written), ANCHOR_DISCRIMINATOR_LEN);
  return out;
}

// ---------------------------------------------------------------------
// Schema descriptor — matches `keeper_decoder::schema_descriptor_json()`
// ---------------------------------------------------------------------

/**
 * Compact schema descriptor used by `decoder.test.ts` to assert that
 * the field listing here matches the Rust source 1:1. If a wave-14+
 * schema bump renames, adds, or removes a field, this descriptor
 * must be updated alongside `keeper-decoder/src/lib.rs` — the test
 * fails on first divergence.
 */
export const SCHEMA_DESCRIPTOR = {
  OnchainSubPool: [
    { name: "market", type: "Pubkey32" },
    { name: "sub_pool_id", type: "u32" },
    { name: "long_pool_equity", type: "u128" },
    { name: "short_pool_equity", type: "u128" },
    { name: "long_active_shares", type: "u128" },
    { name: "short_active_shares", type: "u128" },
    { name: "long_recovery_shares", type: "u128" },
    { name: "short_recovery_shares", type: "u128" },
    { name: "long_active_notional", type: "u128" },
    { name: "short_active_notional", type: "u128" },
    { name: "long_active_generation", type: "u64" },
    { name: "short_active_generation", type: "u64" },
    { name: "last_price", type: "u64" },
    { name: "last_sync_slot", type: "u64" },
    { name: "long_dust", type: "u128" },
    { name: "short_dust", type: "u128" },
    { name: "long_dormant_bucket_count", type: "u32" },
    { name: "short_dormant_bucket_count", type: "u32" },
    { name: "bump", type: "u8" },
    { name: "_pad", type: "array<u8,7>" },
  ],
  OnchainDormantBucket: [
    { name: "sub_pool", type: "Pubkey32" },
    { name: "direction_is_long", type: "bool" },
    { name: "zero_price_tick", type: "i64" },
    { name: "anchor_price", type: "u64" },
    { name: "total_recovery_shares", type: "u128" },
    { name: "total_recovery_notional", type: "u128" },
    { name: "accrued_value", type: "u128" },
    { name: "position_count", type: "u64" },
    { name: "last_applied_index", type: "u64" },
    { name: "bump", type: "u8" },
    { name: "_pad", type: "array<u8,6>" },
  ],
  OnchainDistEntry: [
    { name: "event_index", type: "u64" },
    { name: "p_at_event", type: "u64" },
    { name: "total_outstanding_at_event", type: "u128" },
    { name: "total_alloc_input", type: "u128" },
    { name: "allocated_sum_observed", type: "u128" },
  ],
  OnchainDistributionLedger: [
    { name: "sub_pool", type: "Pubkey32" },
    { name: "direction_is_long", type: "bool" },
    { name: "max_entries", type: "u32" },
    { name: "gc_offset", type: "u64" },
    { name: "next_event_index", type: "u64" },
    { name: "accrued_value_total", type: "u128" },
    { name: "pending_distribution_total", type: "u128" },
    { name: "entry_count", type: "u32" },
    { name: "entries", type: "vec<OnchainDistEntry>" },
    { name: "bump", type: "u8" },
    { name: "_pad", type: "array<u8,7>" },
  ],
  OnchainMarket: [
    { name: "global_config", type: "Pubkey32" },
    { name: "symbol", type: "array<u8,16>" },
    { name: "collateral_mint", type: "Pubkey32" },
    { name: "vault", type: "Pubkey32" },
    { name: "fee_vault", type: "Pubkey32" },
    { name: "oracle_price_feed", type: "Pubkey32" },
    { name: "oracle_program_id", type: "Pubkey32" },
    { name: "leverage_bps", type: "u32" },
    { name: "min_margin", type: "u64" },
    { name: "max_margin_per_position", type: "u64" },
    { name: "max_total_principal", type: "u128" },
    { name: "max_total_notional", type: "u128" },
    { name: "current_total_principal", type: "u128" },
    { name: "current_total_notional", type: "u128" },
    { name: "open_fee_bps", type: "u16" },
    { name: "max_oracle_age_seconds", type: "i64" },
    { name: "max_oracle_age_slots", type: "u64" },
    { name: "max_confidence_bps", type: "u16" },
    { name: "max_price_move_bps_per_sync", type: "u32" },
    { name: "price_tick", type: "u64" },
    { name: "tick_aggregation_factor", type: "u32" },
    { name: "max_dormant_bucket_count_per_direction", type: "u32" },
    { name: "dilution_safety_bps", type: "u32" },
    { name: "max_idle_slots", type: "u64" },
    { name: "paused", type: "bool" },
    { name: "frozen_new_position", type: "bool" },
    { name: "schema_version", type: "u16" },
    { name: "sub_pool_count", type: "u32" },
    { name: "dormant_distribute_mode", type: "u8" },
    { name: "max_pending_apply_per_tx", type: "u32" },
    { name: "max_distribution_ledger_size", type: "u32" },
    { name: "bump", type: "u8" },
    { name: "_pad", type: "array<u8,2>" },
  ],
  OnchainPosition: [
    { name: "owner", type: "Pubkey32" },
    { name: "market", type: "Pubkey32" },
    { name: "sub_pool", type: "Pubkey32" },
    { name: "position_id", type: "u64" },
    { name: "direction_is_long", type: "bool" },
    { name: "status", type: "u8" },
    { name: "principal", type: "u64" },
    { name: "leverage_bps", type: "u32" },
    { name: "notional", type: "u128" },
    { name: "active_shares", type: "u128" },
    { name: "recovery_shares", type: "u128" },
    { name: "recovery_bucket_tick", type: "i64" },
    { name: "has_recovery_bucket", type: "bool" },
    { name: "zero_price", type: "u64" },
    { name: "entry_price", type: "u64" },
    { name: "last_sync_slot", type: "u64" },
    { name: "active_generation", type: "u64" },
    { name: "opened_at", type: "i64" },
    { name: "updated_at", type: "i64" },
    { name: "closed_at", type: "i64" },
    { name: "schema_version", type: "u16" },
    { name: "bump", type: "u8" },
    { name: "_pad", type: "array<u8,5>" },
  ],
} as const;
