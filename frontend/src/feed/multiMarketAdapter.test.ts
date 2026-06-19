/**
 * Wave 18 — MultiMarketFeedAdapter unit tests.
 *
 * Verifies:
 *   1. Constructor rejects empty / duplicate-symbol market lists.
 *   2. `start()` subscribes to BOTH the market PDA and the lock
 *      PDA for every configured market (so 3 markets → 6 subs).
 *   3. The aggregator holds back snapshots until the FIRST lock-PDA
 *      sample arrives, then emits one row per market.
 *   4. The legacy wave-17 single-market `keeperLeaderLockBytes`
 *      mirrors the FIRST configured market's lock bytes (backward
 *      compat for `LeaderLockBanner`).
 *   5. `stop()` cleans up every subscription and the slot poll.
 *
 * @vitest-environment node
 */
import { describe, expect, it, vi } from "vitest";
import { Buffer } from "buffer";
import {
  PublicKey,
  type AccountInfo,
  type KeyedAccountInfo,
} from "@solana/web3.js";

import {
  MultiMarketFeedAdapter,
  type MultiMarketEntry,
} from "./multiMarketAdapter";
import type {
  FeedConnection,
  FeedConnectionFactory,
  AccountChangeCallback,
  ProgramAccountChangeCallback,
} from "./websocketAdapter";
import type { FeedSnapshot } from "../types";
import {
  encodeOnchainDormantBucket,
  encodeOnchainMarket,
  encodeOnchainPosition,
  encodeOnchainSubPool,
  type OnchainDormantBucket,
  type OnchainMarket,
  type OnchainPosition,
  type OnchainSubPool,
  type Pubkey32,
} from "../decoder/onchain";

function pk(seed: number): PublicKey {
  const buf = Buffer.alloc(32);
  buf[0] = seed;
  return new PublicKey(buf);
}

function pk32FromPubkey(p: PublicKey): Pubkey32 {
  return { hex: Buffer.from(p.toBytes()).toString("hex") };
}

function pk32(seed: number): Pubkey32 {
  const buf = Buffer.alloc(32);
  buf[0] = seed;
  return { hex: buf.toString("hex") };
}

const LOCK_DISC = new Uint8Array([9, 8, 7, 6, 5, 4, 3, 2]);

function lockBytes(seed: number): AccountInfo<Buffer> {
  // 8-byte disc + 49-byte body = 57. We don't need the body to
  // decode for the adapter test; we only verify byte-shape passthrough.
  const data = Buffer.alloc(57);
  data.set(LOCK_DISC, 0);
  data[8] = seed;
  return {
    executable: false,
    lamports: 0,
    owner: pk(0),
    rentEpoch: 0,
    data,
  };
}

function marketBytes(seed: number): AccountInfo<Buffer> {
  const data = Buffer.alloc(120);
  data[0] = seed;
  return {
    executable: false,
    lamports: 0,
    owner: pk(0),
    rentEpoch: 0,
    data,
  };
}

// Wave 19 — dummy builders matching `websocketAdapter.test.ts`.
const MARKET_DISC = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]);
const SUB_POOL_DISC = new Uint8Array([10, 11, 12, 13, 14, 15, 16, 17]);
const BUCKET_DISC = new Uint8Array([20, 21, 22, 23, 24, 25, 26, 27]);
const LEDGER_DISC = new Uint8Array([30, 31, 32, 33, 34, 35, 36, 37]);
const POSITION_DISC = new Uint8Array([40, 41, 42, 43, 44, 45, 46, 47]);

function dummyMarket(symbol: string): OnchainMarket {
  const sym = Buffer.alloc(16);
  Buffer.from(symbol).copy(sym, 0);
  return {
    global_config: pk32(2),
    symbol: sym,
    collateral_mint: pk32(3),
    vault: pk32(4),
    fee_vault: pk32(5),
    oracle_price_feed: pk32(6),
    oracle_program_id: pk32(7),
    leverage_bps: 5000,
    min_margin: 1_000_000n,
    max_margin_per_position: 10_000_000_000n,
    max_total_principal: 5_000_000_000_000n,
    max_total_notional: 50_000_000_000_000n,
    current_total_principal: 0n,
    current_total_notional: 0n,
    open_fee_bps: 5,
    max_oracle_age_seconds: 60n,
    max_oracle_age_slots: 64n,
    max_confidence_bps: 200,
    max_price_move_bps_per_sync: 1000,
    price_tick: 12345n,
    tick_aggregation_factor: 10,
    max_dormant_bucket_count_per_direction: 16,
    dilution_safety_bps: 100,
    max_idle_slots: 128n,
    paused: false,
    frozen_new_position: false,
    schema_version: 2,
    sub_pool_count: 1,
    dormant_distribute_mode: 1,
    max_pending_apply_per_tx: 8,
    max_distribution_ledger_size: 64,
    bump: 250,
    _pad: Buffer.alloc(2),
  };
}

function dummySubPool(market: PublicKey, id: number, slot: bigint): OnchainSubPool {
  return {
    market: pk32FromPubkey(market),
    sub_pool_id: id,
    long_pool_equity: 10_000n,
    short_pool_equity: 9_000n,
    long_active_shares: 500n,
    short_active_shares: 450n,
    long_recovery_shares: 0n,
    short_recovery_shares: 0n,
    long_active_notional: 1_000n,
    short_active_notional: 900n,
    long_active_generation: 1n,
    short_active_generation: 1n,
    last_price: 12345n,
    last_sync_slot: slot,
    long_dust: 0n,
    short_dust: 0n,
    long_dormant_bucket_count: 2,
    short_dormant_bucket_count: 1,
    bump: 254,
    _pad: Buffer.alloc(7),
  };
}

function dummyBucket(subPoolPubkey: PublicKey, notional: bigint): OnchainDormantBucket {
  return {
    sub_pool: pk32FromPubkey(subPoolPubkey),
    direction_is_long: true,
    zero_price_tick: 7n,
    anchor_price: 12345n,
    total_recovery_shares: 100n,
    total_recovery_notional: notional,
    accrued_value: 50n,
    position_count: 2n,
    last_applied_index: 1n,
    bump: 253,
    _pad: Buffer.alloc(6),
  };
}

function makeKeyedAccountInfo(
  accountId: PublicKey,
  data: Uint8Array,
): KeyedAccountInfo {
  return {
    accountId,
    accountInfo: {
      data: Buffer.from(data),
      executable: false,
      lamports: 0,
      owner: pk(0),
      rentEpoch: 0,
    },
  };
}

interface Stub extends FeedConnection {
  account: Map<string, AccountChangeCallback>;
  programCallbacks: ProgramAccountChangeCallback[];
  programRemoved: number[];
  removed: number[];
  slotCalls: number;
  setSlot(n: number): void;
}

function buildStub(): Stub {
  let nextId = 1;
  let slot = 0;
  const account = new Map<string, AccountChangeCallback>();
  const programCallbacks: ProgramAccountChangeCallback[] = [];
  const removed: number[] = [];
  const programRemoved: number[] = [];
  const idPubkey = new Map<number, string>();
  return {
    account,
    programCallbacks,
    programRemoved,
    removed,
    slotCalls: 0,
    setSlot(n: number) {
      slot = n;
    },
    onAccountChange(pubkey, cb) {
      const id = nextId++;
      const key = pubkey.toBase58();
      account.set(key, cb);
      idPubkey.set(id, key);
      return id;
    },
    onProgramAccountChange(_programId, cb) {
      programCallbacks.push(cb);
      return nextId++;
    },
    async removeAccountChangeListener(id) {
      removed.push(id);
      const key = idPubkey.get(id);
      if (key) account.delete(key);
    },
    async removeProgramAccountChangeListener(id) {
      programRemoved.push(id);
    },
    async getSlot() {
      this.slotCalls += 1;
      return slot;
    },
  } as Stub;
}

const MARKETS: MultiMarketEntry[] = [
  { symbol: "SOL-USD", marketPda: pk(10), lockPda: pk(11) },
  { symbol: "BTC-USD", marketPda: pk(20), lockPda: pk(21) },
  { symbol: "ETH-USD", marketPda: pk(30), lockPda: pk(31) },
];

describe("MultiMarketFeedAdapter", () => {
  it("rejects empty market list", () => {
    expect(
      () =>
        new MultiMarketFeedAdapter({
          url: "ws://x",
          programId: pk(1),
          markets: [],
        }),
    ).toThrow(/non-empty markets/);
  });

  it("rejects duplicate symbols", () => {
    expect(
      () =>
        new MultiMarketFeedAdapter({
          url: "ws://x",
          programId: pk(1),
          markets: [
            { symbol: "X", marketPda: pk(1), lockPda: pk(2) },
            { symbol: "X", marketPda: pk(3), lockPda: pk(4) },
          ],
        }),
    ).toThrow(/duplicate market symbol/);
  });

  it("subscribes to every market PDA + lock PDA on start", () => {
    const stub = buildStub();
    const factory: FeedConnectionFactory = () => stub;
    const adapter = new MultiMarketFeedAdapter({
      url: "ws://x",
      programId: pk(1),
      markets: MARKETS,
      connectionFactory: factory,
    });
    const stop = adapter.start(() => {});
    // 3 markets × 2 subs = 6 distinct base58 keys.
    expect(stub.account.size).toBe(6);
    for (const m of MARKETS) {
      expect(stub.account.has(m.marketPda.toBase58())).toBe(true);
      expect(stub.account.has(m.lockPda.toBase58())).toBe(true);
    }
    stop();
    expect(stub.account.size).toBe(0);
  });

  it("emits one row per market after first lock update", () => {
    const stub = buildStub();
    const adapter = new MultiMarketFeedAdapter({
      url: "ws://x",
      programId: pk(1),
      markets: MARKETS,
      connectionFactory: () => stub,
    });
    const snaps: FeedSnapshot[] = [];
    adapter.start((s) => snaps.push(s));
    // No emission before the first lock sample.
    expect(snaps.length).toBe(0);
    // Push a lock sample for SOL-USD.
    const solLockCb = stub.account.get(MARKETS[0]!.lockPda.toBase58())!;
    solLockCb(lockBytes(7));
    expect(snaps.length).toBe(1);
    const view = snaps[0]!.marketsView!;
    expect(view.entries.size).toBe(3);
    const sol = view.entries.get("SOL-USD")!;
    expect(sol.lockBytes).toBeDefined();
    expect(sol.lockBytes![8]).toBe(7);
    // Other markets present but lockBytes still undefined.
    expect(view.entries.get("BTC-USD")!.lockBytes).toBeUndefined();
    // Wave-17 backward compat: legacy field mirrors the FIRST market.
    expect(snaps[0]!.keeperLeaderLockBytes).toBeDefined();
    expect(snaps[0]!.keeperLeaderLockBytes![8]).toBe(7);
  });

  it("market PDA updates also trigger emission once primed", () => {
    const stub = buildStub();
    const adapter = new MultiMarketFeedAdapter({
      url: "ws://x",
      programId: pk(1),
      markets: MARKETS.slice(0, 2),
      connectionFactory: () => stub,
    });
    const snaps: FeedSnapshot[] = [];
    adapter.start((s) => snaps.push(s));
    // Prime with a lock sample first.
    stub.account.get(MARKETS[0]!.lockPda.toBase58())!(lockBytes(1));
    const before = snaps.length;
    // Now a market PDA change should re-emit.
    stub.account.get(MARKETS[1]!.marketPda.toBase58())!(marketBytes(99));
    expect(snaps.length).toBe(before + 1);
    const view = snaps[snaps.length - 1]!.marketsView!;
    expect(view.entries.get("BTC-USD")!.marketBytes).toBeDefined();
  });

  it("polls cluster slot when trackClusterSlot=true", async () => {
    vi.useFakeTimers();
    try {
      const stub = buildStub();
      stub.setSlot(12345);
      const adapter = new MultiMarketFeedAdapter({
        url: "ws://x",
        programId: pk(1),
        markets: [MARKETS[0]!],
        connectionFactory: () => stub,
        trackClusterSlot: true,
        slotPollIntervalMs: 1000,
      });
      const snaps: FeedSnapshot[] = [];
      adapter.start((s) => snaps.push(s));
      // Synchronous getSlot fired once at start; flush microtasks.
      await vi.runOnlyPendingTimersAsync();
      // Prime so the slot-driven emit can land.
      stub.account.get(MARKETS[0]!.lockPda.toBase58())!(lockBytes(2));
      expect(snaps.length).toBeGreaterThanOrEqual(1);
      expect(snaps[snaps.length - 1]!.currentSlot).toBe(12345n);
    } finally {
      vi.useRealTimers();
    }
  });

  it("rejects truncated lock payloads (<8 bytes) without crashing", () => {
    const stub = buildStub();
    const adapter = new MultiMarketFeedAdapter({
      url: "ws://x",
      programId: pk(1),
      markets: [MARKETS[0]!],
      connectionFactory: () => stub,
    });
    const snaps: FeedSnapshot[] = [];
    adapter.start((s) => snaps.push(s));
    const data = Buffer.alloc(4);
    stub.account.get(MARKETS[0]!.lockPda.toBase58())!({
      executable: false,
      lamports: 0,
      owner: pk(0),
      rentEpoch: 0,
      data,
    });
    expect(snaps.length).toBe(0);
    expect(adapter.decodeFailureCount()).toBe(1);
  });

  // ----------------------------------------------------------------
  // Wave 19 — per-market decode + sub-pool / bucket routing
  // ----------------------------------------------------------------

  it("decodes per-market OnchainMarket bytes into marketSummary", () => {
    const stub = buildStub();
    const adapter = new MultiMarketFeedAdapter({
      url: "ws://x",
      programId: pk(1),
      markets: MARKETS.slice(0, 2),
      connectionFactory: () => stub,
    });
    const snaps: FeedSnapshot[] = [];
    adapter.start((s) => snaps.push(s));
    // Prime so emissions are allowed.
    stub.account.get(MARKETS[0]!.lockPda.toBase58())!(lockBytes(1));
    // Push real `OnchainMarket` bytes for SOL-USD.
    const market = dummyMarket("SOL-USD");
    market.paused = true;
    market.price_tick = 99_999n;
    const buf = Buffer.from(encodeOnchainMarket(market, MARKET_DISC));
    stub.account.get(MARKETS[0]!.marketPda.toBase58())!({
      executable: false,
      lamports: 0,
      owner: pk(0),
      rentEpoch: 0,
      data: buf,
    });
    const last = snaps[snaps.length - 1]!;
    const sol = last.marketsView!.entries.get("SOL-USD")!;
    expect(sol.marketSummary).toBeDefined();
    expect(sol.marketSummary!.paused).toBe(true);
    expect(sol.marketSummary!.midPriceMicro).toBe(99_999n);
    // BTC-USD has no market bytes yet — summary stays undefined.
    const btc = last.marketsView!.entries.get("BTC-USD")!;
    expect(btc.marketSummary).toBeUndefined();
    // Wave-17 legacy `indexer.market` mirrors primary's summary.
    expect(last.indexer.market.paused).toBe(true);
    expect(last.indexer.market.midPriceMicro).toBe(99_999n);
  });

  it("subscribes to onProgramAccountChange when discriminators provided", () => {
    const stub = buildStub();
    const adapter = new MultiMarketFeedAdapter({
      url: "ws://x",
      programId: pk(1),
      markets: MARKETS,
      connectionFactory: () => stub,
      discriminators: {
        market: MARKET_DISC,
        subPool: SUB_POOL_DISC,
        dormantBucket: BUCKET_DISC,
        distributionLedger: LEDGER_DISC,
        position: POSITION_DISC,
      },
    });
    expect(stub.programCallbacks.length).toBe(0);
    const stop = adapter.start(() => {});
    expect(stub.programCallbacks.length).toBe(1);
    stop();
    expect(stub.programRemoved.length).toBe(1);
  });

  it("routes sub-pool updates to the owning market by market PDA", () => {
    const stub = buildStub();
    const adapter = new MultiMarketFeedAdapter({
      url: "ws://x",
      programId: pk(1),
      markets: MARKETS,
      connectionFactory: () => stub,
      discriminators: {
        market: MARKET_DISC,
        subPool: SUB_POOL_DISC,
        dormantBucket: BUCKET_DISC,
        distributionLedger: LEDGER_DISC,
        position: POSITION_DISC,
      },
    });
    const snaps: FeedSnapshot[] = [];
    adapter.start((s) => snaps.push(s));
    // Prime so emissions land.
    stub.account.get(MARKETS[0]!.lockPda.toBase58())!(lockBytes(1));
    // Build a sub-pool that points at SOL-USD's market PDA.
    const sp = dummySubPool(MARKETS[0]!.marketPda, 7, 1234n);
    const spBytes = encodeOnchainSubPool(sp, SUB_POOL_DISC);
    const subPoolPubkey = pk(50);
    stub.programCallbacks[0]!(
      makeKeyedAccountInfo(subPoolPubkey, spBytes),
    );
    const last = snaps[snaps.length - 1]!;
    const sol = last.marketsView!.entries.get("SOL-USD")!;
    expect(sol.subPools).toBeDefined();
    expect(sol.subPools!.length).toBe(1);
    expect(sol.subPools![0]!.id).toBe(7);
    expect(sol.indexerSlot).toBe(1234);
    // BTC-USD should have NO sub-pools — routing dropped non-matching market.
    const btc = last.marketsView!.entries.get("BTC-USD")!;
    expect(btc.subPools).toBeUndefined();
    // Wave-17 legacy indexer mirrors primary's sub-pools.
    expect(last.indexer.subPools.length).toBe(1);
    expect(last.indexer.slot).toBe(1234);
  });

  it("routes dormant buckets via parent sub-pool's owning market", () => {
    const stub = buildStub();
    const adapter = new MultiMarketFeedAdapter({
      url: "ws://x",
      programId: pk(1),
      markets: MARKETS,
      connectionFactory: () => stub,
      discriminators: {
        market: MARKET_DISC,
        subPool: SUB_POOL_DISC,
        dormantBucket: BUCKET_DISC,
        distributionLedger: LEDGER_DISC,
        position: POSITION_DISC,
      },
    });
    const snaps: FeedSnapshot[] = [];
    adapter.start((s) => snaps.push(s));
    stub.account.get(MARKETS[0]!.lockPda.toBase58())!(lockBytes(1));
    // Push BTC-USD sub-pool first so the bucket can route.
    const sp = dummySubPool(MARKETS[1]!.marketPda, 5, 500n);
    const spBytes = encodeOnchainSubPool(sp, SUB_POOL_DISC);
    const subPoolPubkey = pk(60);
    stub.programCallbacks[0]!(
      makeKeyedAccountInfo(subPoolPubkey, spBytes),
    );
    // Now push a bucket that references that sub-pool.
    const bucket = dummyBucket(subPoolPubkey, 4_321n);
    const bBytes = encodeOnchainDormantBucket(bucket, BUCKET_DISC);
    stub.programCallbacks[0]!(
      makeKeyedAccountInfo(pk(61), bBytes),
    );
    const last = snaps[snaps.length - 1]!;
    const btc = last.marketsView!.entries.get("BTC-USD")!;
    expect(btc.dormantBuckets).toBeDefined();
    expect(btc.dormantBuckets!.length).toBe(1);
    expect(btc.dormantBuckets![0]!.subPoolId).toBe(5);
    expect(btc.projectedRecoveryOutstandingMicroUsdc).toBe(4_321n);
    // SOL-USD is unaffected.
    const sol = last.marketsView!.entries.get("SOL-USD")!;
    expect(sol.dormantBuckets).toBeUndefined();
  });

  it("drops sub-pool updates whose market is not in our watch list", () => {
    const stub = buildStub();
    const adapter = new MultiMarketFeedAdapter({
      url: "ws://x",
      programId: pk(1),
      markets: MARKETS.slice(0, 1),
      connectionFactory: () => stub,
      discriminators: {
        market: MARKET_DISC,
        subPool: SUB_POOL_DISC,
        dormantBucket: BUCKET_DISC,
        distributionLedger: LEDGER_DISC,
        position: POSITION_DISC,
      },
    });
    const snaps: FeedSnapshot[] = [];
    adapter.start((s) => snaps.push(s));
    stub.account.get(MARKETS[0]!.lockPda.toBase58())!(lockBytes(1));
    const before = snaps.length;
    // Sub-pool that points at an UNwatched market PDA.
    const unwatched = pk(99);
    const sp = dummySubPool(unwatched, 1, 1n);
    const spBytes = encodeOnchainSubPool(sp, SUB_POOL_DISC);
    stub.programCallbacks[0]!(
      makeKeyedAccountInfo(pk(70), spBytes),
    );
    // Unwatched market — adapter should silently drop, no extra emit.
    expect(snaps.length).toBe(before);
  });

  it("routes Position updates to the owning market feed.positions with marketPdaHex", () => {
    const stub = buildStub();
    const solMarket = MARKETS[0]!.marketPda;
    const solMarketHex = pk32FromPubkey(solMarket).hex;
    const subPoolPk = pk(80);
    const subPoolHex = pk32FromPubkey(subPoolPk).hex;
    const adapter = new MultiMarketFeedAdapter({
      url: "ws://x",
      programId: pk(1),
      markets: MARKETS,
      connectionFactory: () => stub,
      discriminators: {
        market: MARKET_DISC,
        subPool: SUB_POOL_DISC,
        dormantBucket: BUCKET_DISC,
        distributionLedger: LEDGER_DISC,
        position: POSITION_DISC,
      },
    });
    const snaps: FeedSnapshot[] = [];
    adapter.start((s) => snaps.push(s));
    stub.account.get(MARKETS[0]!.lockPda.toBase58())!(lockBytes(1));
    const sp = dummySubPool(solMarket, 1, 1n);
    stub.programCallbacks[0]!(
      makeKeyedAccountInfo(subPoolPk, encodeOnchainSubPool(sp, SUB_POOL_DISC)),
    );
    const pos: OnchainPosition = {
      owner: pk32(90),
      market: { hex: solMarketHex },
      sub_pool: { hex: subPoolHex },
      position_id: 1n,
      direction_is_long: true,
      status: 0,
      principal: 2_000_000n,
      leverage_bps: 5000,
      notional: 10_000_000n,
      active_shares: 50n,
      recovery_shares: 0n,
      recovery_bucket_tick: 0n,
      has_recovery_bucket: false,
      zero_price: 0n,
      entry_price: 60_000_000_000n,
      last_sync_slot: 100n,
      active_generation: 1n,
      opened_at: 1_700_000_000n,
      updated_at: 1_700_000_100n,
      closed_at: 0n,
      schema_version: 1,
      bump: 250,
      _pad: Buffer.alloc(5),
    };
    stub.programCallbacks[0]!(
      makeKeyedAccountInfo(pk(81), encodeOnchainPosition(pos, POSITION_DISC)),
    );
    const last = snaps[snaps.length - 1]!;
    expect(last.positions).toHaveLength(1);
    expect(last.positions[0]!.marketPdaHex).toBe(solMarketHex);
  });
});
