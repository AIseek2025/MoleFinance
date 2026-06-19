/**
 * Wave 14 — WebSocketFeedAdapter unit tests.
 *
 * The real `Connection` from `@solana/web3.js` opens a websocket on
 * construction; we inject a stub via `connectionFactory` to keep the
 * tests offline. The fixture verifies:
 *   1. `start()` actually subscribes (account + program account)
 *   2. Market account changes route through `decodeOnchainMarket`
 *   3. Sub-pool & dormant-bucket changes route through their decoders
 *   4. The aggregator holds back snapshots until the market arrives
 *   5. Decode failures don't throw — they bump the failure counter
 *   6. The returned `stop` function cleans up subscriptions
 *
 * @vitest-environment node
 */
import { describe, expect, it, vi } from "vitest";
import { Buffer } from "buffer";
import { PublicKey, type AccountInfo, type KeyedAccountInfo } from "@solana/web3.js";

import {
  WebSocketFeedAdapter,
  type AccountChangeCallback,
  type FeedConnection,
  type FeedConnectionFactory,
  type ProgramAccountChangeCallback,
} from "./websocketAdapter";
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
import type { FeedSnapshot } from "../types";

function pk(seed: number): PublicKey {
  const buf = Buffer.alloc(32);
  buf[0] = seed;
  return new PublicKey(buf);
}

function pk32(seed: number): Pubkey32 {
  const buf = Buffer.alloc(32);
  buf[0] = seed;
  return { hex: buf.toString("hex") };
}

const MARKET_DISC = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]);
const SUB_POOL_DISC = new Uint8Array([10, 11, 12, 13, 14, 15, 16, 17]);
const BUCKET_DISC = new Uint8Array([20, 21, 22, 23, 24, 25, 26, 27]);
const LEDGER_DISC = new Uint8Array([30, 31, 32, 33, 34, 35, 36, 37]);
const POSITION_DISC = new Uint8Array([40, 41, 42, 43, 44, 45, 46, 47]);

interface StubConnection extends FeedConnection {
  accountListeners: Array<{ pubkey: PublicKey; cb: AccountChangeCallback }>;
  programListeners: Array<{
    programId: PublicKey;
    cb: ProgramAccountChangeCallback;
  }>;
  removeAccountChangeListenerCalls: number[];
  removeProgramAccountChangeListenerCalls: number[];
}

function makeStubConnection(): StubConnection {
  const accountListeners: StubConnection["accountListeners"] = [];
  const programListeners: StubConnection["programListeners"] = [];
  const removeAccountChangeListenerCalls: number[] = [];
  const removeProgramAccountChangeListenerCalls: number[] = [];
  return {
    accountListeners,
    programListeners,
    removeAccountChangeListenerCalls,
    removeProgramAccountChangeListenerCalls,
    onAccountChange(pubkey, cb) {
      accountListeners.push({ pubkey, cb });
      return accountListeners.length;
    },
    onProgramAccountChange(programId, cb) {
      programListeners.push({ programId, cb });
      return programListeners.length;
    },
    async removeAccountChangeListener(id) {
      removeAccountChangeListenerCalls.push(id);
    },
    async removeProgramAccountChangeListener(id) {
      removeProgramAccountChangeListenerCalls.push(id);
    },
  };
}

function dummyMarket(): OnchainMarket {
  const symbol = Buffer.alloc(16);
  Buffer.from("SOL-USD").copy(symbol, 0);
  return {
    global_config: pk32(2),
    symbol,
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

function dummySubPool(): OnchainSubPool {
  return {
    market: pk32(8),
    sub_pool_id: 3,
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
    last_sync_slot: 100n,
    long_dust: 0n,
    short_dust: 0n,
    long_dormant_bucket_count: 2,
    short_dormant_bucket_count: 1,
    bump: 254,
    _pad: Buffer.alloc(7),
  };
}

function dummyBucket(): OnchainDormantBucket {
  return {
    sub_pool: pk32(9),
    direction_is_long: true,
    zero_price_tick: 7n,
    anchor_price: 12345n,
    total_recovery_shares: 100n,
    total_recovery_notional: 1_000n,
    accrued_value: 50n,
    position_count: 2n,
    last_applied_index: 1n,
    bump: 253,
    _pad: Buffer.alloc(6),
  };
}

function makeAccountInfo(data: Uint8Array): AccountInfo<Buffer> {
  return {
    data: Buffer.from(data),
    executable: false,
    lamports: 0,
    owner: pk(0),
    rentEpoch: 0,
  };
}

function makeKeyedAccountInfo(
  accountId: PublicKey,
  data: Uint8Array,
): KeyedAccountInfo {
  return {
    accountId,
    accountInfo: makeAccountInfo(data),
  };
}

function buildAdapter(opts?: {
  factory?: FeedConnectionFactory;
}): {
  adapter: WebSocketFeedAdapter;
  factory: ReturnType<typeof vi.fn>;
  conn: StubConnection;
} {
  const conn = makeStubConnection();
  const factory = vi.fn(() => conn);
  const adapter = new WebSocketFeedAdapter({
    url: "wss://test/feed",
    programId: pk(99),
    marketPda: pk(100),
    discriminators: {
      market: MARKET_DISC,
      subPool: SUB_POOL_DISC,
      dormantBucket: BUCKET_DISC,
      distributionLedger: LEDGER_DISC,
      position: POSITION_DISC,
    },
    connectionFactory: opts?.factory ?? (factory as unknown as FeedConnectionFactory),
    now: () => 1_700_000_000_000,
  });
  return { adapter, factory, conn };
}

describe("WebSocketFeedAdapter", () => {
  it("subscribes to account + program account changes on start()", () => {
    const { adapter, factory, conn } = buildAdapter();
    expect(adapter.status()).toBe("idle");
    const stop = adapter.start(() => {});
    expect(factory).toHaveBeenCalledOnce();
    expect(factory).toHaveBeenCalledWith("wss://test/feed", "confirmed");
    expect(conn.accountListeners).toHaveLength(1);
    expect(conn.accountListeners[0]!.pubkey.equals(pk(100))).toBe(true);
    expect(conn.programListeners).toHaveLength(1);
    expect(conn.programListeners[0]!.programId.equals(pk(99))).toBe(true);
    expect(adapter.status()).toBe("connected");
    stop();
  });

  it("holds snapshots until the market PDA decodes", () => {
    const { adapter, conn } = buildAdapter();
    const snapshots: FeedSnapshot[] = [];
    adapter.start((s) => snapshots.push(s));
    // Push a sub-pool first; should NOT emit since market is unknown.
    const spBytes = encodeOnchainSubPool(dummySubPool(), SUB_POOL_DISC);
    conn.programListeners[0]!.cb(makeKeyedAccountInfo(pk(50), spBytes));
    expect(snapshots).toHaveLength(0);
    // Now push the market.
    const marketBytes = encodeOnchainMarket(dummyMarket(), MARKET_DISC);
    conn.accountListeners[0]!.cb(makeAccountInfo(marketBytes));
    expect(snapshots).toHaveLength(1);
    expect(snapshots[0]!.indexer.market.symbol).toBe("SOL-USD");
    expect(snapshots[0]!.indexer.subPools).toHaveLength(1);
    expect(snapshots[0]!.indexer.subPools[0]!.id).toBe(3);
  });

  it("aggregates dormant buckets after market arrives", () => {
    const { adapter, conn } = buildAdapter();
    const snapshots: FeedSnapshot[] = [];
    adapter.start((s) => snapshots.push(s));
    const marketBytes = encodeOnchainMarket(dummyMarket(), MARKET_DISC);
    conn.accountListeners[0]!.cb(makeAccountInfo(marketBytes));
    expect(snapshots).toHaveLength(1);
    const bucketBytes = encodeOnchainDormantBucket(dummyBucket(), BUCKET_DISC);
    conn.programListeners[0]!.cb(makeKeyedAccountInfo(pk(60), bucketBytes));
    expect(snapshots).toHaveLength(2);
    const last = snapshots[snapshots.length - 1]!;
    expect(last.indexer.dormantBuckets).toHaveLength(1);
    expect(last.indexer.dormantBuckets[0]!.direction).toBe("Long");
    expect(last.indexer.dormantBuckets[0]!.tick).toBe(7);
    expect(last.indexer.dormantBuckets[0]!.pendingRecoveryMicroUsdc).toBe(50n);
    expect(last.indexer.projectedRecoveryOutstandingMicroUsdc).toBe(1000n);
  });

  it("ignores unknown discriminators silently (no throw, no emit)", () => {
    const { adapter, conn } = buildAdapter();
    const snapshots: FeedSnapshot[] = [];
    adapter.start((s) => snapshots.push(s));
    const marketBytes = encodeOnchainMarket(dummyMarket(), MARKET_DISC);
    conn.accountListeners[0]!.cb(makeAccountInfo(marketBytes));
    const before = snapshots.length;
    // Distribution-ledger discriminator — adapter doesn't decode it
    // in wave 14, so no aggregation update + no extra emit.
    const ledger = new Uint8Array(64);
    ledger.set(LEDGER_DISC, 0);
    conn.programListeners[0]!.cb(makeKeyedAccountInfo(pk(70), ledger));
    expect(snapshots.length).toBe(before);
    expect(adapter.decodeFailureCount()).toBe(0);
  });

  it("counts decode failures without throwing", () => {
    const { adapter, conn } = buildAdapter();
    const snapshots: FeedSnapshot[] = [];
    adapter.start((s) => snapshots.push(s));
    // Truncated market payload: 8 bytes (just the discriminator).
    const truncated = new Uint8Array(8);
    truncated.set(MARKET_DISC, 0);
    conn.accountListeners[0]!.cb(makeAccountInfo(truncated));
    expect(adapter.decodeFailureCount()).toBe(1);
    expect(snapshots).toHaveLength(0);
  });

  it("stop() removes both subscriptions", () => {
    const { adapter, conn } = buildAdapter();
    const stop = adapter.start(() => {});
    expect(conn.removeAccountChangeListenerCalls).toHaveLength(0);
    expect(conn.removeProgramAccountChangeListenerCalls).toHaveLength(0);
    stop();
    expect(conn.removeAccountChangeListenerCalls).toHaveLength(1);
    expect(conn.removeProgramAccountChangeListenerCalls).toHaveLength(1);
    expect(adapter.status()).toBe("idle");
  });

  it("transitions to error if the connection factory throws", () => {
    const factory = vi.fn(() => {
      throw new Error("network down");
    }) as unknown as FeedConnectionFactory;
    const adapter = new WebSocketFeedAdapter({
      url: "wss://test/feed",
      programId: pk(99),
      marketPda: pk(100),
      discriminators: {
        market: MARKET_DISC,
        subPool: SUB_POOL_DISC,
        dormantBucket: BUCKET_DISC,
        distributionLedger: LEDGER_DISC,
        position: POSITION_DISC,
      },
      connectionFactory: factory,
    });
    const stop = adapter.start(() => {});
    expect(adapter.status()).toBe("error");
    stop();
  });

  // Wave 17 — leader-lock + cluster-slot path.

  it("subscribes to keeperLeaderLockPda when supplied (wave 17)", () => {
    const conn = makeStubConnection();
    const factory = vi.fn(() => conn) as unknown as FeedConnectionFactory;
    const adapter = new WebSocketFeedAdapter({
      url: "wss://test/feed",
      programId: pk(99),
      marketPda: pk(100),
      keeperLeaderLockPda: pk(0xa1),
      discriminators: {
        market: MARKET_DISC,
        subPool: SUB_POOL_DISC,
        dormantBucket: BUCKET_DISC,
        distributionLedger: LEDGER_DISC,
        position: POSITION_DISC,
      },
      connectionFactory: factory,
    });
    const stop = adapter.start(() => {});
    // Two account subscriptions: the wave-14 market PDA + the
    // wave-17 keeper-leader-lock PDA.
    expect(conn.accountListeners).toHaveLength(2);
    const lockListener = conn.accountListeners.find((l) =>
      l.pubkey.equals(pk(0xa1)),
    );
    expect(lockListener).toBeDefined();
    stop();
    // Both account subs cleaned up.
    expect(conn.removeAccountChangeListenerCalls).toHaveLength(2);
  });

  it("emits leader-lock bytes after the market arrives (wave 17)", () => {
    const conn = makeStubConnection();
    const factory = vi.fn(() => conn) as unknown as FeedConnectionFactory;
    const adapter = new WebSocketFeedAdapter({
      url: "wss://test/feed",
      programId: pk(99),
      marketPda: pk(100),
      keeperLeaderLockPda: pk(0xa1),
      discriminators: {
        market: MARKET_DISC,
        subPool: SUB_POOL_DISC,
        dormantBucket: BUCKET_DISC,
        distributionLedger: LEDGER_DISC,
        position: POSITION_DISC,
      },
      connectionFactory: factory,
    });
    const snapshots: FeedSnapshot[] = [];
    adapter.start((s) => snapshots.push(s));
    // Push a leader-lock payload before the market — should be
    // cached, but no snapshot emitted yet (wave-14 invariant:
    // hold until market PDA arrives).
    const marketListener = conn.accountListeners.find((l) =>
      l.pubkey.equals(pk(100)),
    )!;
    const lockListener = conn.accountListeners.find((l) =>
      l.pubkey.equals(pk(0xa1)),
    )!;
    // Synthesise a 57-byte payload (8 disc + 49 body) — the adapter
    // doesn't decode here, just shovels the bytes through so the
    // wasm side decodes downstream.
    const lockBytes = new Uint8Array(57);
    lockBytes.set([0xfa, 0xce, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06], 0);
    lockListener.cb(makeAccountInfo(lockBytes));
    expect(snapshots).toHaveLength(0);
    // Now push the market; the held-back lock bytes flow through.
    const marketBytes = encodeOnchainMarket(dummyMarket(), MARKET_DISC);
    marketListener.cb(makeAccountInfo(marketBytes));
    expect(snapshots).toHaveLength(1);
    expect(snapshots[0]!.keeperLeaderLockBytes).toBeDefined();
    expect(snapshots[0]!.keeperLeaderLockBytes!.length).toBe(57);
    // And the bytes are a defensive copy, not the original buffer
    // (so a later mutation can't bleed into the React tree).
    expect(snapshots[0]!.keeperLeaderLockBytes!.buffer).not.toBe(
      lockBytes.buffer,
    );
  });

  it("rejects truncated leader-lock payloads (wave 17)", () => {
    const conn = makeStubConnection();
    const factory = vi.fn(() => conn) as unknown as FeedConnectionFactory;
    const adapter = new WebSocketFeedAdapter({
      url: "wss://test/feed",
      programId: pk(99),
      marketPda: pk(100),
      keeperLeaderLockPda: pk(0xa1),
      discriminators: {
        market: MARKET_DISC,
        subPool: SUB_POOL_DISC,
        dormantBucket: BUCKET_DISC,
        distributionLedger: LEDGER_DISC,
        position: POSITION_DISC,
      },
      connectionFactory: factory,
    });
    const snapshots: FeedSnapshot[] = [];
    adapter.start((s) => snapshots.push(s));
    const marketListener = conn.accountListeners.find((l) =>
      l.pubkey.equals(pk(100)),
    )!;
    const lockListener = conn.accountListeners.find((l) =>
      l.pubkey.equals(pk(0xa1)),
    )!;
    // 4-byte garbage — adapter must bump the failure counter and
    // skip emit. We then push a market and confirm the snapshot
    // doesn't carry leader-lock bytes (no good sample yet).
    lockListener.cb(makeAccountInfo(new Uint8Array(4)));
    expect(adapter.decodeFailureCount()).toBe(1);
    const marketBytes = encodeOnchainMarket(dummyMarket(), MARKET_DISC);
    marketListener.cb(makeAccountInfo(marketBytes));
    expect(snapshots).toHaveLength(1);
    expect(snapshots[0]!.keeperLeaderLockBytes).toBeUndefined();
  });

  it("polls cluster slot when trackClusterSlot=true (wave 17)", async () => {
    const conn = makeStubConnection();
    let slotCalls = 0;
    (conn as unknown as { getSlot: () => Promise<number> }).getSlot =
      async () => {
        slotCalls += 1;
        return 1_000 + slotCalls;
      };
    const factory = vi.fn(() => conn) as unknown as FeedConnectionFactory;
    const adapter = new WebSocketFeedAdapter({
      url: "wss://test/feed",
      programId: pk(99),
      marketPda: pk(100),
      discriminators: {
        market: MARKET_DISC,
        subPool: SUB_POOL_DISC,
        dormantBucket: BUCKET_DISC,
        distributionLedger: LEDGER_DISC,
        position: POSITION_DISC,
      },
      connectionFactory: factory,
      trackClusterSlot: true,
      slotPollIntervalMs: 1_000_000, // long; we just want the immediate first call
    });
    const snapshots: FeedSnapshot[] = [];
    const stop = adapter.start((s) => snapshots.push(s));
    // The immediate first call fires sync but resolves async.
    await new Promise((r) => setTimeout(r, 0));
    // Push a market so the snapshot can be aggregated.
    const marketListener = conn.accountListeners.find((l) =>
      l.pubkey.equals(pk(100)),
    )!;
    const marketBytes = encodeOnchainMarket(dummyMarket(), MARKET_DISC);
    marketListener.cb(makeAccountInfo(marketBytes));
    expect(slotCalls).toBe(1);
    const last = snapshots[snapshots.length - 1]!;
    expect(last.currentSlot).toBe(1_001n);
    stop();
  });

  it("routes Position program-account updates into feed.positions with marketPdaHex", () => {
    const conn = makeStubConnection();
    const factory: FeedConnectionFactory = () => conn;
    const marketPda = pk(100);
    const marketHex = Buffer.from(marketPda.toBytes()).toString("hex");
    const subPoolPk = pk(101);
    const subPoolHex = Buffer.from(subPoolPk.toBytes()).toString("hex");
    const adapter = new WebSocketFeedAdapter({
      url: "wss://test/feed",
      programId: pk(99),
      marketPda,
      discriminators: {
        market: MARKET_DISC,
        subPool: SUB_POOL_DISC,
        dormantBucket: BUCKET_DISC,
        distributionLedger: LEDGER_DISC,
        position: POSITION_DISC,
      },
      connectionFactory: factory,
    });
    const snapshots: FeedSnapshot[] = [];
    const stop = adapter.start((s) => snapshots.push(s));
    const marketListener = conn.accountListeners.find((l) =>
      l.pubkey.equals(marketPda),
    )!;
    marketListener.cb(makeAccountInfo(encodeOnchainMarket(dummyMarket(), MARKET_DISC)));
    const programCb = conn.programListeners[0]!.cb;
    const sp = dummySubPool();
    sp.market = { hex: marketHex };
    programCb({
      accountId: subPoolPk,
      accountInfo: makeAccountInfo(encodeOnchainSubPool(sp, SUB_POOL_DISC)),
    });
    const pos: OnchainPosition = {
      owner: pk32(50),
      market: { hex: marketHex },
      sub_pool: { hex: subPoolHex },
      position_id: 1n,
      direction_is_long: true,
      status: 0,
      principal: 1_000_000n,
      leverage_bps: 5000,
      notional: 5_000_000n,
      active_shares: 100n,
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
    programCb({
      accountId: pk(102),
      accountInfo: makeAccountInfo(encodeOnchainPosition(pos, POSITION_DISC)),
    });
    const last = snapshots[snapshots.length - 1]!;
    expect(last.positions).toHaveLength(1);
    expect(last.positions[0]!.marketPdaHex).toBe(marketHex);
    expect(last.positions[0]!.qty).toBe(100n);
    stop();
  });
});
