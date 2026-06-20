// Wave 18 — multi-market WebSocket feed adapter.
//
// The wave-14 `WebSocketFeedAdapter` subscribes to ONE market PDA
// + ONE program-account stream + (wave-17) ONE keeper-leader-lock
// PDA. Multi-market deployments outgrow that contract: the same
// console (and the same supervising keeper-bot) handles N markets,
// and the operator wants ONE banner row per market — see
// `LeaderLockGrid`.
//
// This adapter takes a list of `{symbol, marketPda, lockPda}`
// entries (parsed from a wave-18 markets config) and:
//
//   1. Subscribes to every market PDA (per-market) so a future
//      multi-market trader/indexer panel can decode each.
//   2. Subscribes to every `KeeperLeaderLock` PDA so the wave-18
//      grid can render `holder × freshness × stale` per market.
//   3. (Optionally) polls `getSlot()` once for the cluster — every
//      market shares the cluster clock.
//
// The aggregated `FeedSnapshot.marketsView.entries` carries one
// entry per registered market. The wave-17 single-market shape
// (`indexer / keeper / keeperLeaderLockBytes / currentSlot`) is
// preserved for backward compat — we use the FIRST registered
// market as the "primary" market that fills those legacy fields,
// so the wave-17 `LeaderLockBanner` keeps working unchanged when
// the grid isn't shown yet.
//
// ## Why not extend `WebSocketFeedAdapter` directly?
//
// The wave-14 adapter has a tight aggregator that holds back
// snapshots until ONE market PDA arrives. Multi-market doesn't
// have a single "the market"; expressing "any market is enough to
// emit" with the existing aggregator would require invasive
// surgery. A separate adapter is cleaner, and the wave-14 adapter
// stays the single-market default for the trader UI which doesn't
// yet need multi-market awareness.

import {
  Connection,
  PublicKey,
  type AccountInfo,
  type Commitment,
  type KeyedAccountInfo,
} from "@solana/web3.js";

import {
  decodeOnchainDormantBucket,
  decodeOnchainMarket,
  decodeOnchainPosition,
  decodeOnchainSubPool,
  type OnchainDormantBucket,
  type OnchainMarket,
  type OnchainPosition,
  type OnchainSubPool,
} from "../decoder/onchain";
import type { MoleAccountDiscriminators } from "../decoder/discriminators";
import type {
  DormantBucketSummary,
  FeedSnapshot,
  IndexerSnapshot,
  KeeperState,
  MarketSummary,
  MarketViewEntry,
  MultiMarketView,
  PositionSummary,
  Pubkey32,
  SubPoolSummary,
} from "../types";
import {
  bytesEqual,
  isDisplayablePosition,
  onchainDormantBucketToSummary,
  onchainMarketToSummary,
  onchainPositionToSummary,
  onchainSubPoolToSummary,
} from "./decode";

import type { FeedAdapter, FeedStatus } from "./adapter";
import type {
  AccountChangeCallback,
  FeedConnection,
  FeedConnectionFactory,
  ProgramAccountChangeCallback,
} from "./websocketAdapter";

/** Wave 18 — one configured market the adapter watches. */
export interface MultiMarketEntry {
  /** Stable label (e.g. "SOL-USD") — matches keeper-rpc registry. */
  symbol: string;
  /** Market PDA. */
  marketPda: PublicKey;
  /** `KeeperLeaderLock` PDA (pre-derived by the caller). */
  lockPda: PublicKey;
}

/** Wave 18 — configuration for the multi-market adapter. */
export interface MultiMarketFeedAdapterOptions {
  /** RPC endpoint URL. */
  url: string;
  /** Mole-option program id (shared across markets). */
  programId: PublicKey;
  /** Markets to watch. MUST be non-empty. */
  markets: MultiMarketEntry[];
  /**
   * Wave 19 — Anchor account discriminators. When supplied the
   * adapter subscribes to `onProgramAccountChange(programId)` once
   * (shared across markets) and routes each `SubPool` /
   * `DormantBucket` update to the owning market entry. When
   * `undefined`, the wave-18 behaviour is preserved (lock-only
   * grid, no sub-pool / bucket fan-out).
   */
  discriminators?: MoleAccountDiscriminators;
  /** Solana commitment level. Defaults to `confirmed`. */
  commitment?: Commitment;
  /**
   * Whether to poll cluster `getSlot()`. When true, polls every
   * `slotPollIntervalMs` and writes the result to
   * `FeedSnapshot.currentSlot`. Defaults to `false`.
   */
  trackClusterSlot?: boolean;
  /** Cluster-slot poll cadence in ms. Defaults to 4000ms. */
  slotPollIntervalMs?: number;
  /** Test-only — inject a mock connection. */
  connectionFactory?: FeedConnectionFactory;
  /** Test-only — clock injection for `timestampMs`. */
  now?: () => number;
}

const defaultConnectionFactory: FeedConnectionFactory = (endpoint, commitment) =>
  new Connection(endpoint, commitment) as unknown as FeedConnection;

/**
 * Wave 18 — multi-market live feed adapter.
 *
 * Holds back snapshots until at least ONE market has emitted some
 * lock-PDA data. After that, every `accountSubscribe` callback
 * triggers a re-emission with the full `marketsView`.
 */
/**
 * Wave 19 — internal per-market state. Holds raw decoded shapes
 * keyed by pubkey hex; `aggregate()` projects this into the
 * panel-friendly `MarketViewEntry`.
 */
interface MarketState {
  symbol: string;
  marketPdaHex: string;
  lockPdaHex: string;
  /** Raw market PDA bytes (for re-decode / future schema versions). */
  marketBytes: Uint8Array | null;
  /** Decoded market account; `null` until first successful decode. */
  decodedMarket: OnchainMarket | null;
  /** Raw lock PDA bytes (8 disc + 49 body = 57 bytes). */
  lockBytes: Uint8Array | null;
  /** Sub-pools that route to this market, keyed by pubkey hex. */
  subPools: Map<string, OnchainSubPool>;
  /** Dormant buckets that route to this market, keyed by pubkey hex. */
  dormantBuckets: Map<string, OnchainDormantBucket>;
  /** Wave 22 — positions routed to this market, keyed by pubkey hex. */
  positions: Map<string, OnchainPosition>;
}

export class MultiMarketFeedAdapter implements FeedAdapter {
  readonly kind = "websocket" as const;
  private currentStatus: FeedStatus = "idle";
  private readonly opts: {
    url: string;
    programId: PublicKey;
    markets: MultiMarketEntry[];
    discriminators: MoleAccountDiscriminators | null;
    commitment: Commitment;
    trackClusterSlot: boolean;
    slotPollIntervalMs: number;
    connectionFactory: FeedConnectionFactory;
    now: () => number;
  };
  private connection: FeedConnection | null = null;
  private subIds: number[] = [];
  private programSubId: number | null = null;
  private slotPollHandle: ReturnType<typeof setInterval> | null = null;
  /** Wave 19 — internal market state, keyed by symbol. */
  private states: Map<string, MarketState> = new Map();
  /**
   * Wave 19 — `marketPdaHex → symbol` lookup so sub-pool routing
   * runs in O(1).
   */
  private marketHexToSymbol: Map<string, string> = new Map();
  /**
   * Wave 19 — `subPoolPubkeyHex → owning marketSymbol`. Filled as
   * sub-pool updates arrive; bucket routing then resolves
   * `bucket.sub_pool.hex` → market without re-iterating every entry.
   */
  private subPoolToMarket: Map<string, string> = new Map();
  private currentSlot: bigint | null = null;
  /** Set after the first lock-PDA update arrives so we can emit. */
  private primed = false;
  private decodeFailures = 0;

  constructor(opts: MultiMarketFeedAdapterOptions) {
    if (!opts.markets || opts.markets.length === 0) {
      throw new Error(
        "MultiMarketFeedAdapter requires a non-empty markets list",
      );
    }
    const seen = new Set<string>();
    for (const m of opts.markets) {
      if (seen.has(m.symbol)) {
        throw new Error(
          `MultiMarketFeedAdapter: duplicate market symbol '${m.symbol}'`,
        );
      }
      seen.add(m.symbol);
    }
    this.opts = {
      url: opts.url,
      programId: opts.programId,
      markets: [...opts.markets],
      discriminators: opts.discriminators ?? null,
      commitment: opts.commitment ?? "confirmed",
      trackClusterSlot: opts.trackClusterSlot ?? false,
      slotPollIntervalMs: opts.slotPollIntervalMs ?? 4_000,
      connectionFactory: opts.connectionFactory ?? defaultConnectionFactory,
      now: opts.now ?? (() => Date.now()),
    };
    // Pre-seed per-market internal state so the grid can render an
    // "uninitialised" row for every configured market even before
    // its first PDA update arrives.
    for (const m of this.opts.markets) {
      const marketHex = pubkeyToHex(m.marketPda);
      this.states.set(m.symbol, {
        symbol: m.symbol,
        marketPdaHex: marketHex,
        lockPdaHex: pubkeyToHex(m.lockPda),
        marketBytes: null,
        decodedMarket: null,
        lockBytes: null,
        subPools: new Map(),
        dormantBuckets: new Map(),
        positions: new Map(),
      });
      this.marketHexToSymbol.set(marketHex, m.symbol);
    }
  }

  status(): FeedStatus {
    return this.currentStatus;
  }

  /** Test-only — exposes the running decode-failure count. */
  decodeFailureCount(): number {
    return this.decodeFailures;
  }

  start(onSnapshot: (snapshot: FeedSnapshot) => void): () => void {
    if (this.currentStatus !== "idle" && this.currentStatus !== "error") {
      throw new Error(
        `MultiMarketFeedAdapter.start: cannot start while in '${this.currentStatus}'`,
      );
    }
    this.currentStatus = "connecting";
    let connection: FeedConnection;
    try {
      connection = this.opts.connectionFactory(
        this.opts.url,
        this.opts.commitment,
      );
    } catch (e) {
      this.currentStatus = "error";
      console.error(
        `[mole/frontend] MultiMarketFeedAdapter('${this.opts.url}'): connection factory threw —`,
        e,
      );
      return () => {
        this.currentStatus = "idle";
      };
    }
    this.connection = connection;

    // Subscribe per market: market PDA + lock PDA.
    for (const m of this.opts.markets) {
      const onMarket: AccountChangeCallback = (info) =>
        this.handleMarketChange(m.symbol, info, onSnapshot);
      const onLock: AccountChangeCallback = (info) =>
        this.handleLockChange(m.symbol, info, onSnapshot);
      this.subIds.push(
        connection.onAccountChange(m.marketPda, onMarket, this.opts.commitment),
      );
      this.subIds.push(
        connection.onAccountChange(m.lockPda, onLock, this.opts.commitment),
      );
      if (typeof connection.getAccountInfo === "function") {
        void connection
          .getAccountInfo(m.marketPda, this.opts.commitment)
          .then((info) => {
            if (info) this.handleMarketChange(m.symbol, info, onSnapshot);
          })
          .catch((e) => {
            console.warn(
              `[mole/frontend] MultiMarketFeedAdapter: initial market fetch failed for ${m.symbol} —`,
              e,
            );
          });
        void connection
          .getAccountInfo(m.lockPda, this.opts.commitment)
          .then((info) => {
            if (info) this.handleLockChange(m.symbol, info, onSnapshot);
          })
          .catch((e) => {
            console.warn(
              `[mole/frontend] MultiMarketFeedAdapter: initial leader-lock fetch failed for ${m.symbol} —`,
              e,
            );
          });
      }
    }

    // Wave 19 — single shared program-account subscription to fan
    // out sub-pool / bucket updates across markets.
    if (this.opts.discriminators !== null) {
      const onProgramAccount: ProgramAccountChangeCallback = (info) =>
        this.handleProgramAccountChange(info, onSnapshot);
      this.programSubId = connection.onProgramAccountChange(
        this.opts.programId,
        onProgramAccount,
        this.opts.commitment,
      );
    }

    if (
      this.opts.trackClusterSlot &&
      typeof connection.getSlot === "function"
    ) {
      const tick = () => {
        const conn = this.connection;
        if (!conn || typeof conn.getSlot !== "function") return;
        void conn
          .getSlot(this.opts.commitment)
          .then((slot) => {
            this.currentSlot = BigInt(slot);
            this.maybeEmit(onSnapshot);
          })
          .catch((e) => {
            console.warn(
              "[mole/frontend] MultiMarketFeedAdapter: getSlot poll failed —",
              e,
            );
          });
      };
      tick();
      this.slotPollHandle = setInterval(tick, this.opts.slotPollIntervalMs);
    }
    this.currentStatus = "connected";

    return () => {
      this.currentStatus = "idle";
      const conn = this.connection;
      this.connection = null;
      const subIds = this.subIds;
      this.subIds = [];
      const progSub = this.programSubId;
      this.programSubId = null;
      if (this.slotPollHandle !== null) {
        clearInterval(this.slotPollHandle);
        this.slotPollHandle = null;
      }
      if (conn) {
        for (const id of subIds) {
          void conn.removeAccountChangeListener(id);
        }
        if (progSub !== null) {
          void conn.removeProgramAccountChangeListener(progSub);
        }
      }
    };
  }

  /**
   * Wave 19 — used by `App.tsx` to wake `slotPollHandle`-driven
   * re-emits without needing a fresh PDA write. Returns `null`
   * when the adapter hasn't observed the cluster slot yet.
   */
  getCurrentSlot(): bigint | null {
    return this.currentSlot;
  }

  // -------------------------------------------------------------------
  // Account handlers
  // -------------------------------------------------------------------

  private handleMarketChange(
    symbol: string,
    info: AccountInfo<Buffer>,
    emit: (snapshot: FeedSnapshot) => void,
  ): void {
    if (!info.data || info.data.length === 0) {
      this.decodeFailures += 1;
      return;
    }
    const state = this.states.get(symbol);
    if (!state) return;
    const copy = new Uint8Array(info.data.length);
    copy.set(info.data);
    state.marketBytes = copy;
    // Wave 19 — eagerly decode so the panel can render `paused` /
    // `frozen` / `schema_version` immediately. Decode failures
    // bump the failure counter but don't block the lock-driven
    // grid view.
    try {
      state.decodedMarket = decodeOnchainMarket(info.data);
    } catch (e) {
      this.decodeFailures += 1;
      console.warn(
        `[mole/frontend] MultiMarketFeedAdapter: market '${symbol}' decode failed —`,
        e,
      );
    }
    this.maybeEmit(emit);
  }

  private handleLockChange(
    symbol: string,
    info: AccountInfo<Buffer>,
    emit: (snapshot: FeedSnapshot) => void,
  ): void {
    if (!info.data || info.data.length < 8) {
      this.decodeFailures += 1;
      return;
    }
    const state = this.states.get(symbol);
    if (!state) return;
    const copy = new Uint8Array(info.data.length);
    copy.set(info.data);
    state.lockBytes = copy;
    this.primed = true;
    this.maybeEmit(emit);
  }

  /**
   * Wave 19 — fan out sub-pool / dormant-bucket updates to the
   * owning market by inspecting the decoded payload's market
   * pointer (`OnchainSubPool.market`) or the parent sub-pool
   * pointer (`OnchainDormantBucket.sub_pool`).
   */
  private handleProgramAccountChange(
    info: KeyedAccountInfo,
    emit: (snapshot: FeedSnapshot) => void,
  ): void {
    if (this.opts.discriminators === null) return;
    const data = info.accountInfo.data;
    if (data.length < 8) {
      this.decodeFailures += 1;
      return;
    }
    const disc = data.subarray(0, 8);
    const accountHex = info.accountId.toBuffer().toString("hex");
    if (bytesEqual(disc, this.opts.discriminators.subPool)) {
      let sp: OnchainSubPool;
      try {
        sp = decodeOnchainSubPool(data);
      } catch (e) {
        this.decodeFailures += 1;
        console.warn(
          "[mole/frontend] MultiMarketFeedAdapter: sub-pool decode failed —",
          e,
        );
        return;
      }
      const owningSymbol = this.marketHexToSymbol.get(sp.market.hex);
      if (owningSymbol === undefined) {
        // Sub-pool belongs to a market we're not watching. Silently
        // drop — operators commonly run a multi-market console
        // with a *subset* of the program's full market set.
        return;
      }
      const state = this.states.get(owningSymbol);
      if (!state) return;
      state.subPools.set(accountHex, sp);
      this.subPoolToMarket.set(accountHex, owningSymbol);
      this.maybeEmit(emit);
      return;
    }
    if (bytesEqual(disc, this.opts.discriminators.dormantBucket)) {
      let bucket: OnchainDormantBucket;
      try {
        bucket = decodeOnchainDormantBucket(data);
      } catch (e) {
        this.decodeFailures += 1;
        console.warn(
          "[mole/frontend] MultiMarketFeedAdapter: bucket decode failed —",
          e,
        );
        return;
      }
      // Resolve via the parent sub-pool's owning market.
      const owningSymbol = this.subPoolToMarket.get(bucket.sub_pool.hex);
      if (owningSymbol === undefined) {
        // Bucket arrived before its parent sub-pool — buffer at
        // the program-account stream level isn't ordered. We
        // intentionally drop and rely on a future sub-pool tick
        // re-emitting; the keeper bot publishes both close
        // together so the steady-state miss rate is near zero.
        return;
      }
      const state = this.states.get(owningSymbol);
      if (!state) return;
      state.dormantBuckets.set(accountHex, bucket);
      this.maybeEmit(emit);
      return;
    }
    if (bytesEqual(disc, this.opts.discriminators.position)) {
      let pos: OnchainPosition;
      try {
        pos = decodeOnchainPosition(data);
      } catch (e) {
        this.decodeFailures += 1;
        console.warn(
          "[mole/frontend] MultiMarketFeedAdapter: position decode failed —",
          e,
        );
        return;
      }
      const owningSymbol = this.marketHexToSymbol.get(pos.market.hex);
      if (owningSymbol === undefined) return;
      const state = this.states.get(owningSymbol);
      if (!state) return;
      if (isDisplayablePosition(pos.status)) {
        state.positions.set(accountHex, pos);
      } else {
        state.positions.delete(accountHex);
      }
      this.maybeEmit(emit);
      return;
    }
    // Unknown discriminator (e.g. distribution ledger) — ignore.
  }

  private maybeEmit(emit: (snapshot: FeedSnapshot) => void): void {
    if (!this.primed) return;
    emit(this.aggregate());
  }

  private aggregate(): FeedSnapshot {
    // Wave 17 single-market backward compat: the FIRST configured
    // market doubles as the legacy `keeperLeaderLockBytes` source
    // and the source for `indexer / keeper` legacy shapes.
    const primary = this.opts.markets[0]!;
    const primaryState = this.states.get(primary.symbol)!;
    const legacyLockBytes = primaryState.lockBytes ?? undefined;

    const view: MultiMarketView = {
      entries: new Map(),
    };
    let primaryEntry: MarketViewEntry | null = null;
    for (const [symbol, state] of this.states) {
      const entry = this.projectEntry(state);
      view.entries.set(symbol, entry);
      if (symbol === primary.symbol) primaryEntry = entry;
    }

    const market: MarketSummary = primaryEntry?.marketSummary ?? {
      pubkey: pubkey32FromHex(primaryState.marketPdaHex),
      symbol: primary.symbol,
      schemaVersion: 0,
      paused: false,
      pausedGlobally: false,
      frozenNewPosition: false,
      midPriceMicro: 0n,
      lastOracleSlot: 0,
      currentSlot: Number(this.currentSlot ?? 0n),
    };
    const indexer: IndexerSnapshot = {
      slot: primaryEntry?.indexerSlot ?? Number(this.currentSlot ?? 0n),
      market,
      subPools: primaryEntry?.subPools ?? [],
      dormantBuckets: primaryEntry?.dormantBuckets ?? [],
      pendingInitHints: [],
      projectedRecoveryOutstandingMicroUsdc:
        primaryEntry?.projectedRecoveryOutstandingMicroUsdc ?? 0n,
    };
    const keeper: KeeperState = {
      status: market.paused ? "paused" : "running",
      metrics: {
        tickSlot: 0,
        appliedVol: null,
        volSamples: 0,
        cumulative: { submitted: 0, failed: 0, skipped: 0 },
        recent: { submitted: 0, failed: 0, skipped: 0, durationMs: 0 },
        walletBalanceSol: 0,
      },
      predictions: [],
      recentSignatures: [],
    };
    const allPositions: PositionSummary[] = [];
    for (const state of this.states.values()) {
      const subPoolIdByHex = new Map<string, number>();
      for (const [pkHex, sp] of state.subPools) {
        subPoolIdByHex.set(pkHex, sp.sub_pool_id);
      }
      for (const pos of state.positions.values()) {
        if (!isDisplayablePosition(pos.status)) continue;
        allPositions.push(
          onchainPositionToSummary(
            pos,
            subPoolIdByHex.get(pos.sub_pool.hex) ?? 0,
          ),
        );
      }
    }
    return {
      indexer,
      keeper,
      positions: allPositions,
      timestampMs: this.opts.now(),
      ...(legacyLockBytes !== undefined && {
        keeperLeaderLockBytes: legacyLockBytes,
      }),
      ...(this.currentSlot !== null && {
        currentSlot: this.currentSlot,
      }),
      marketsView: view,
    };
  }

  /** Wave 19 — project per-market internal state to a `MarketViewEntry`. */
  private projectEntry(state: MarketState): MarketViewEntry {
    const subPoolSummaries: SubPoolSummary[] = [];
    const subPoolIdByPubkey = new Map<string, number>();
    let indexerSlot = 0;
    for (const [pkHex, sp] of state.subPools) {
      subPoolSummaries.push(onchainSubPoolToSummary(sp, pkHex));
      subPoolIdByPubkey.set(pkHex, sp.sub_pool_id);
      const slot = Number(sp.last_sync_slot);
      if (slot > indexerSlot) indexerSlot = slot;
    }
    const bucketSummaries: DormantBucketSummary[] = [];
    let projectedOutstanding = 0n;
    for (const bucket of state.dormantBuckets.values()) {
      const subPoolId = subPoolIdByPubkey.get(bucket.sub_pool.hex) ?? 0;
      bucketSummaries.push(onchainDormantBucketToSummary(bucket, subPoolId));
      projectedOutstanding += bucket.total_recovery_notional;
    }
    const marketSummary =
      state.decodedMarket !== null
        ? onchainMarketToSummary(
            state.decodedMarket,
            state.marketPdaHex,
            Number(this.currentSlot ?? 0n),
          )
        : undefined;
    const out: MarketViewEntry = {
      symbol: state.symbol,
      marketPdaHex: state.marketPdaHex,
      lockPdaHex: state.lockPdaHex,
    };
    if (state.lockBytes !== null) out.lockBytes = state.lockBytes;
    if (state.marketBytes !== null) out.marketBytes = state.marketBytes;
    if (marketSummary !== undefined) out.marketSummary = marketSummary;
    if (subPoolSummaries.length > 0) out.subPools = subPoolSummaries;
    if (bucketSummaries.length > 0) out.dormantBuckets = bucketSummaries;
    if (state.dormantBuckets.size > 0) {
      out.projectedRecoveryOutstandingMicroUsdc = projectedOutstanding;
    }
    if (indexerSlot > 0) out.indexerSlot = indexerSlot;
    return out;
  }
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

function pubkeyToHex(pk: PublicKey): string {
  return Buffer.from(pk.toBytes()).toString("hex");
}

function pubkey32FromHex(hex: string): Pubkey32 {
  return { hex };
}
