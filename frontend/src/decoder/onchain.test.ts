/**
 * Wave 14 — TypeScript Borsh decoder unit tests.
 *
 * These exercise the same surface `crates/keeper-decoder` covers
 * Rust-side, so a wave-14 schema bump that updates the Rust source
 * but forgets to update the TS layouts fails here on the next CI
 * run. The pinning point is `SCHEMA_DESCRIPTOR` (TS) vs
 * `schema_descriptor_json()` (Rust); both are committed and the
 * count test below mirrors the Rust-side
 * `schema_descriptor_contains_eighty_field_entries`.
 *
 * @vitest-environment node
 */
import { describe, expect, it } from "vitest";
import { Buffer } from "buffer";
import {
  ANCHOR_DISCRIMINATOR_LEN,
  AccountDecodeError,
  SCHEMA_DESCRIPTOR,
  decodeOnchainDormantBucket,
  decodeOnchainMarket,
  decodeOnchainMarketWithDiscriminator,
  decodeOnchainPosition,
  decodeOnchainPositionWithDiscriminator,
  decodeOnchainSubPool,
  decodeOnchainSubPoolWithDiscriminator,
  encodeOnchainDormantBucket,
  encodeOnchainMarket,
  encodeOnchainPosition,
  encodeOnchainSubPool,
} from "./onchain";
import type { OnchainPosition, Pubkey32 } from "./onchain";

function dummyPubkey(seed: number): Pubkey32 {
  const buf = Buffer.alloc(32);
  buf[0] = seed;
  return { hex: buf.toString("hex") };
}

function dummySubPool() {
  return {
    market: dummyPubkey(1),
    sub_pool_id: 7,
    long_pool_equity: 1234567890n,
    short_pool_equity: 234567890n,
    long_active_shares: 100000n,
    short_active_shares: 90000n,
    long_recovery_shares: 50n,
    short_recovery_shares: 40n,
    long_active_notional: 5000000n,
    short_active_notional: 4000000n,
    long_active_generation: 3n,
    short_active_generation: 2n,
    last_price: 100000000n,
    last_sync_slot: 12345n,
    long_dust: 7n,
    short_dust: 9n,
    long_dormant_bucket_count: 12,
    short_dormant_bucket_count: 8,
    bump: 254,
    _pad: Buffer.alloc(7),
  };
}

function dummyBucket() {
  return {
    sub_pool: dummyPubkey(2),
    direction_is_long: true,
    zero_price_tick: -1234n,
    anchor_price: 50000000n,
    total_recovery_shares: 7777n,
    total_recovery_notional: 8888n,
    accrued_value: 9999n,
    position_count: 4n,
    last_applied_index: 12n,
    bump: 253,
    _pad: Buffer.alloc(6),
  };
}

function dummyMarket() {
  const symbol = Buffer.alloc(16);
  Buffer.from("SOL-USD").copy(symbol, 0);
  return {
    global_config: dummyPubkey(4),
    symbol,
    collateral_mint: dummyPubkey(5),
    vault: dummyPubkey(6),
    fee_vault: dummyPubkey(7),
    oracle_price_feed: dummyPubkey(8),
    oracle_program_id: dummyPubkey(9),
    leverage_bps: 5000,
    min_margin: 1000000n,
    max_margin_per_position: 100000000000n,
    max_total_principal: 5000000000000n,
    max_total_notional: 50000000000000n,
    current_total_principal: 1234567890n,
    current_total_notional: 12345678900n,
    open_fee_bps: 5,
    max_oracle_age_seconds: 60n,
    max_oracle_age_slots: 64n,
    max_confidence_bps: 200,
    max_price_move_bps_per_sync: 1000,
    price_tick: 10000n,
    tick_aggregation_factor: 10,
    max_dormant_bucket_count_per_direction: 16,
    dilution_safety_bps: 100,
    max_idle_slots: 128n,
    paused: false,
    frozen_new_position: false,
    schema_version: 1,
    sub_pool_count: 4,
    dormant_distribute_mode: 1,
    max_pending_apply_per_tx: 8,
    max_distribution_ledger_size: 64,
    bump: 251,
    _pad: Buffer.alloc(2),
  };
}

describe("onchain Borsh decoder", () => {
  it("OnchainSubPool round-trips through encode → decode byte-for-byte", () => {
    const sp = dummySubPool();
    const disc = new Uint8Array([1, 1, 1, 1, 1, 1, 1, 1]);
    const raw = encodeOnchainSubPool(sp, disc);
    const sp2 = decodeOnchainSubPool(raw);
    expect(sp2).toEqual(sp);
  });

  it("OnchainDormantBucket round-trips through encode → decode byte-for-byte (negative i64)", () => {
    const b = dummyBucket();
    const disc = new Uint8Array([2, 2, 2, 2, 2, 2, 2, 2]);
    const raw = encodeOnchainDormantBucket(b, disc);
    const b2 = decodeOnchainDormantBucket(raw);
    expect(b2).toEqual(b);
  });

  it("OnchainMarket round-trips through encode → decode byte-for-byte", () => {
    const m = dummyMarket();
    const disc = new Uint8Array([3, 3, 3, 3, 3, 3, 3, 3]);
    const raw = encodeOnchainMarket(m, disc);
    const m2 = decodeOnchainMarket(raw);
    expect(m2).toEqual(m);
  });

  it("decodeOnchainSubPool throws TooShort on payload < 8 bytes", () => {
    try {
      decodeOnchainSubPool(new Uint8Array([0, 0, 0, 0]));
      throw new Error("should have thrown");
    } catch (e) {
      expect(e).toBeInstanceOf(AccountDecodeError);
      expect((e as AccountDecodeError).kind).toBe("TooShort");
    }
  });

  it("decodeOnchainSubPool throws TooShort on empty payload", () => {
    try {
      decodeOnchainSubPool(new Uint8Array(0));
      throw new Error("should have thrown");
    } catch (e) {
      expect(e).toBeInstanceOf(AccountDecodeError);
      expect((e as AccountDecodeError).kind).toBe("TooShort");
    }
  });

  it("decodeOnchainSubPoolWithDiscriminator rejects mismatched discriminator", () => {
    const sp = dummySubPool();
    const goodDisc = new Uint8Array([1, 1, 1, 1, 1, 1, 1, 1]);
    const badDisc = new Uint8Array([2, 2, 2, 2, 2, 2, 2, 2]);
    const raw = encodeOnchainSubPool(sp, goodDisc);
    try {
      decodeOnchainSubPoolWithDiscriminator(raw, badDisc);
      throw new Error("should have thrown");
    } catch (e) {
      expect(e).toBeInstanceOf(AccountDecodeError);
      expect((e as AccountDecodeError).kind).toBe("DiscriminatorMismatch");
    }
  });

  it("decodeOnchainSubPoolWithDiscriminator accepts matching discriminator", () => {
    const sp = dummySubPool();
    const disc = new Uint8Array([7, 7, 7, 7, 7, 7, 7, 7]);
    const raw = encodeOnchainSubPool(sp, disc);
    const sp2 = decodeOnchainSubPoolWithDiscriminator(raw, disc);
    expect(sp2).toEqual(sp);
  });

  it("decodeOnchainMarketWithDiscriminator round-trips with matching discriminator", () => {
    const m = dummyMarket();
    const disc = new Uint8Array([42, 42, 42, 42, 42, 42, 42, 42]);
    const raw = encodeOnchainMarket(m, disc);
    const m2 = decodeOnchainMarketWithDiscriminator(raw, disc);
    expect(m2).toEqual(m);
  });

  it("Pubkey32 layout decodes a 64-character hex string", () => {
    const sp = dummySubPool();
    const disc = new Uint8Array(ANCHOR_DISCRIMINATOR_LEN);
    const raw = encodeOnchainSubPool(sp, disc);
    const sp2 = decodeOnchainSubPool(raw);
    expect(sp2.market.hex).toBe(sp.market.hex);
    expect(sp2.market.hex).toMatch(/^[0-9a-f]{64}$/);
  });

  it("ANCHOR_DISCRIMINATOR_LEN is 8 — wire-format constant lock", () => {
    expect(ANCHOR_DISCRIMINATOR_LEN).toBe(8);
  });

  it("SCHEMA_DESCRIPTOR enumerates exactly 103 fields across 6 structs (wave-21 +OnchainPosition)", () => {
    let total = 0;
    for (const s of Object.values(SCHEMA_DESCRIPTOR)) {
      total += s.length;
    }
    expect(total).toBe(103);
    expect(Object.keys(SCHEMA_DESCRIPTOR).length).toBe(6);
  });

  it("SCHEMA_DESCRIPTOR field order matches keeper-decoder source", () => {
    expect(SCHEMA_DESCRIPTOR.OnchainSubPool[0]!.name).toBe("market");
    expect(SCHEMA_DESCRIPTOR.OnchainSubPool[0]!.type).toBe("Pubkey32");
    const subPoolLast =
      SCHEMA_DESCRIPTOR.OnchainSubPool[
        SCHEMA_DESCRIPTOR.OnchainSubPool.length - 1
      ]!;
    expect(subPoolLast.name).toBe("_pad");
    expect(subPoolLast.type).toBe("array<u8,7>");
    const marketLast =
      SCHEMA_DESCRIPTOR.OnchainMarket[
        SCHEMA_DESCRIPTOR.OnchainMarket.length - 1
      ]!;
    expect(marketLast.name).toBe("_pad");
    expect(marketLast.type).toBe("array<u8,2>");
  });

  // ----------------------------------------------------------------
  // Wave 21 — OnchainPosition decoder parity
  // ----------------------------------------------------------------

  function dummyPosition(): OnchainPosition {
    return {
      owner: dummyPubkey(11),
      market: dummyPubkey(12),
      sub_pool: dummyPubkey(13),
      position_id: 42n,
      direction_is_long: true,
      status: 0,
      principal: 1000000n,
      leverage_bps: 5000,
      notional: 5000000n,
      active_shares: 100n,
      recovery_shares: 0n,
      recovery_bucket_tick: 0n,
      has_recovery_bucket: false,
      zero_price: 0n,
      entry_price: 60123456000n,
      last_sync_slot: 217000000n,
      active_generation: 3n,
      opened_at: 1700000000n,
      updated_at: 1700000120n,
      closed_at: 0n,
      schema_version: 1,
      bump: 252,
      _pad: Buffer.alloc(5),
    };
  }

  it("OnchainPosition round-trips through encode → decode byte-for-byte", () => {
    const p = dummyPosition();
    const disc = new Uint8Array([7, 7, 7, 7, 7, 7, 7, 7]);
    const raw = encodeOnchainPosition(p, disc);
    const p2 = decodeOnchainPosition(raw);
    expect(p2).toEqual(p);
  });

  it("OnchainPosition body length is 239 bytes (Position::LEN - 8 disc)", () => {
    const p = dummyPosition();
    const disc = new Uint8Array(ANCHOR_DISCRIMINATOR_LEN);
    const raw = encodeOnchainPosition(p, disc);
    expect(raw.length).toBe(ANCHOR_DISCRIMINATOR_LEN + 239);
  });

  it("OnchainPosition.market round-trips intact (wave-22 marketPdaHex routing key)", () => {
    const p = dummyPosition();
    const targetMarket: Pubkey32 = { hex: "ab".repeat(32) };
    p.market = targetMarket;
    const raw = encodeOnchainPosition(p, new Uint8Array(8));
    const p2 = decodeOnchainPosition(raw);
    expect(p2.market.hex).toBe(targetMarket.hex);
  });

  it("decodeOnchainPosition rejects truncated payload with TooShort", () => {
    try {
      decodeOnchainPosition(new Uint8Array(4));
      throw new Error("should have thrown");
    } catch (e) {
      expect(e).toBeInstanceOf(AccountDecodeError);
      expect((e as AccountDecodeError).kind).toBe("TooShort");
    }
  });

  it("decodeOnchainPositionWithDiscriminator rejects mismatched discriminator", () => {
    const p = dummyPosition();
    const goodDisc = new Uint8Array([1, 1, 1, 1, 1, 1, 1, 1]);
    const badDisc = new Uint8Array([2, 2, 2, 2, 2, 2, 2, 2]);
    const raw = encodeOnchainPosition(p, goodDisc);
    try {
      decodeOnchainPositionWithDiscriminator(raw, badDisc);
      throw new Error("should have thrown");
    } catch (e) {
      expect(e).toBeInstanceOf(AccountDecodeError);
      expect((e as AccountDecodeError).kind).toBe("DiscriminatorMismatch");
    }
  });

  it("decodeOnchainPosition preserves direction_is_long=false / status=closed", () => {
    const p = dummyPosition();
    p.direction_is_long = false;
    p.status = 2;
    const raw = encodeOnchainPosition(p, new Uint8Array(8));
    const p2 = decodeOnchainPosition(raw);
    expect(p2.direction_is_long).toBe(false);
    expect(p2.status).toBe(2);
  });
});
