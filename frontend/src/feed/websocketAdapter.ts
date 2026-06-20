// Wave 14 — real WebSocket / RPC FeedAdapter.
//
// Until wave 13 this was a placeholder that surfaced an "offline"
// banner in the UI. Wave 14 wires it to a real Solana RPC websocket:
//   • `Connection.onAccountChange` for the market PDA
//   • `Connection.onProgramAccountChange` for sub-pools, dormant
//     buckets, distribution ledgers
//   • Decode every account via the wave-14 Borsh decoder in
//     `frontend/src/decoder/onchain.ts`
//   • Aggregate into a `FeedSnapshot` and dispatch on every change
//
// ## Live wiring vs. unit tests
//
// `Connection` from `@solana/web3.js` is non-trivial to instantiate
// in a unit test (it spins a websocket on construction). The adapter
// therefore takes an injectable `connectionFactory` whose default
// constructs a real `Connection`; the test suite replaces this with
// a mock that returns a stub Connection-like object.
//
// ## Aggregator state
//
// The adapter keeps an internal `state` mirroring `FeedSnapshot` so
// each per-account update only touches the relevant slice and re-
// emits a fully populated snapshot. Until the market PDA arrives the
// adapter holds back snapshots (no point publishing half-baked
// state); after the first market update it emits on every account
// callback.

import {
  Connection,
  PublicKey,
  type Commitment,
  type AccountInfo,
  type KeyedAccountInfo,
} from "@solana/web3.js";

import type {
  DormantBucketSummary,
  FeedSnapshot,
  IndexerSnapshot,
  KeeperState,
  MarketSummary,
  PositionSummary,
  Pubkey32,
  SubPoolSummary,
} from "../types";

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

import {
  isDisplayablePosition,
  latestSubPoolPrice,
  onchainDormantBucketToSummary,
  onchainMarketToSummary,
  onchainPositionToSummary,
  onchainSubPoolToSummary,
  pubkey32FromBase58,
} from "./decode";

import type { FeedAdapter, FeedStatus } from "./adapter";

// ---------------------------------------------------------------------
// Shared connection abstraction (so tests don't need a real socket)
// ---------------------------------------------------------------------

export interface AccountChangeCallback {
  (info: AccountInfo<Buffer>): void;
}
export interface ProgramAccountChangeCallback {
  (info: KeyedAccountInfo): void;
}

export interface FeedConnection {
  getAccountInfo?(
    pubkey: PublicKey,
    commitment?: Commitment,
  ): Promise<AccountInfo<Buffer> | null>;
  onAccountChange(
    pubkey: PublicKey,
    cb: AccountChangeCallback,
    commitment?: Commitment,
  ): number;
  onProgramAccountChange(
    programId: PublicKey,
    cb: ProgramAccountChangeCallback,
    commitment?: Commitment,
    filters?: unknown[],
  ): number;
  removeAccountChangeListener(id: number): Promise<void>;
  removeProgramAccountChangeListener(id: number): Promise<void>;
  /**
   * Wave 17 — `Connection.getSlot()` parity. Returns the cluster's
   * current slot under the configured commitment. We type the return
   * loosely (`number | bigint`) because `@solana/web3.js` returns
   * `number` for clusters with slot < 2^53 but production callers
   * should treat the value as `bigint`-compatible.
   */
  getSlot?(commitment?: Commitment): Promise<number | bigint>;
}

export type FeedConnectionFactory = (
  endpoint: string,
  commitment: Commitment,
) => FeedConnection;

const defaultConnectionFactory: FeedConnectionFactory = (
  endpoint,
  commitment,
) => new Connection(endpoint, commitment) as unknown as FeedConnection;

// ---------------------------------------------------------------------
// Discriminators — the keeper bot derives these from
// `sha256("account:<TypeName>")[..8]`. Frontend hard-codes the same
// values; the wave-13 governance verifier
// (`scripts/verify-security-references.sh`) covers the Rust side. A
// future wave can fetch them from `keeper_decoder` via the
// wasm-pack artifact.
// ---------------------------------------------------------------------

export interface AccountDiscriminators {
  market: Uint8Array;
  subPool: Uint8Array;
  dormantBucket: Uint8Array;
  distributionLedger: Uint8Array;
  /** Wave 22 — `Position` account discriminator. */
  position: Uint8Array;
}

// ---------------------------------------------------------------------
// Adapter options + class
// ---------------------------------------------------------------------

export interface WebSocketFeedAdapterOptions {
  /** Endpoint URL (e.g. `wss://api.example.com/feed`). */
  url: string;
  /** Solana program id of the on-chain mole-option program. */
  programId: PublicKey;
  /** Market PDA we subscribe to via `onAccountChange`. */
  marketPda: PublicKey;
  /** Discriminators per account type (8-byte sha256 prefixes). */
  discriminators: AccountDiscriminators;
  /** Solana commitment level for subscriptions. */
  commitment?: Commitment;
  /**
   * Maximum reconnect backoff in ms; the adapter doubles each
   * attempt up to this cap. Defaults to 30s.
   */
  maxBackoffMs?: number;
  /** Test-only: inject a mock connection. */
  connectionFactory?: FeedConnectionFactory;
  /** Test-only: clock injection for the keeper-tick timestamp. */
  now?: () => number;
  /**
   * Wave 17 — the on-chain `KeeperLeaderLock` PDA for the wired
   * market. When set, the adapter subscribes to it via
   * `onAccountChange` and emits the raw account bytes on
   * `FeedSnapshot.keeperLeaderLockBytes` so the wave-16
   * `LeaderLockBanner` can decode + render live data.
   *
   * Production wiring computes this off chain at boot:
   *
   * ```ts
   * const [lock] = PublicKey.findProgramAddressSync(
   *   [Buffer.from("keeper_leader_lock"), marketPda.toBytes()],
   *   programId,
   * );
   * ```
   *
   * `undefined` keeps the wave-14/15/16 behaviour: no leader-lock
   * subscription, banner stays at `uninitialised`. This makes the
   * field opt-in so existing consumers don't break.
   */
  keeperLeaderLockPda?: PublicKey;
  /**
   * Wave 17 — when true, the adapter polls `getSlot()` once per
   * `slotPollIntervalMs` and exposes the latest reading on
   * `FeedSnapshot.currentSlot`. The `LeaderLockBanner` uses this
   * to age the lock against the cluster clock rather than the
   * (possibly stale) keeper bot tick slot. Defaults to `false` so
   * the wave-14 minimum-RPC contract isn't broken.
   */
  trackClusterSlot?: boolean;
  /** Wave 17 — cluster-slot poll cadence; defaults to 4000ms (≈5 slots). */
  slotPollIntervalMs?: number;
}

interface AggregatorState {
  market: OnchainMarket | null;
  subPools: Map<string, OnchainSubPool>;
  dormantBuckets: Map<string, OnchainDormantBucket>;
  /** Wave 22 — open/dormant positions keyed by account pubkey hex. */
  positions: Map<string, OnchainPosition>;
  /** Wave 17 — most-recent raw bytes of the leader-lock PDA. */
  keeperLeaderLockBytes: Uint8Array | null;
  /** Wave 17 — most-recent cluster slot from `getSlot()` polling. */
  currentSlot: bigint | null;
}

export class WebSocketFeedAdapter implements FeedAdapter {
  readonly kind = "websocket" as const;
  private currentStatus: FeedStatus = "idle";
  private readonly opts: {
    url: string;
    programId: PublicKey;
    marketPda: PublicKey;
    discriminators: AccountDiscriminators;
    commitment: Commitment;
    maxBackoffMs?: number;
    connectionFactory: FeedConnectionFactory;
    now: () => number;
    keeperLeaderLockPda?: PublicKey;
    trackClusterSlot: boolean;
    slotPollIntervalMs: number;
  };
  private connection: FeedConnection | null = null;
  private accountSubId: number | null = null;
  private programSubId: number | null = null;
  private leaderLockSubId: number | null = null;
  private slotPollHandle: ReturnType<typeof setInterval> | null = null;
  private state: AggregatorState = {
    market: null,
    subPools: new Map(),
    dormantBuckets: new Map(),
    positions: new Map(),
    keeperLeaderLockBytes: null,
    currentSlot: null,
  };
  private decodeFailures = 0;

  constructor(opts: WebSocketFeedAdapterOptions) {
    this.opts = {
      url: opts.url,
      programId: opts.programId,
      marketPda: opts.marketPda,
      discriminators: opts.discriminators,
      commitment: opts.commitment ?? "confirmed",
      ...(opts.maxBackoffMs !== undefined && { maxBackoffMs: opts.maxBackoffMs }),
      connectionFactory: opts.connectionFactory ?? defaultConnectionFactory,
      now: opts.now ?? (() => Date.now()),
      ...(opts.keeperLeaderLockPda !== undefined && {
        keeperLeaderLockPda: opts.keeperLeaderLockPda,
      }),
      trackClusterSlot: opts.trackClusterSlot ?? false,
      slotPollIntervalMs: opts.slotPollIntervalMs ?? 4_000,
    };
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
        `WebSocketFeedAdapter.start: cannot start while in '${this.currentStatus}'`,
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
        `[mole/frontend] WebSocketFeedAdapter('${this.opts.url}'): connection factory threw —`,
        e,
      );
      return () => {
        this.currentStatus = "idle";
      };
    }
    this.connection = connection;
    this.accountSubId = connection.onAccountChange(
      this.opts.marketPda,
      (info) => this.handleAccountChange(info, onSnapshot),
      this.opts.commitment,
    );
    this.programSubId = connection.onProgramAccountChange(
      this.opts.programId,
      (info) => this.handleProgramAccountChange(info, onSnapshot),
      this.opts.commitment,
    );
    // Wave 17 — opt-in subscription for the keeper-leader-lock PDA.
    if (this.opts.keeperLeaderLockPda) {
      this.leaderLockSubId = connection.onAccountChange(
        this.opts.keeperLeaderLockPda,
        (info) => this.handleLeaderLockChange(info, onSnapshot),
        this.opts.commitment,
      );
    }
    if (typeof connection.getAccountInfo === "function") {
      void connection
        .getAccountInfo(this.opts.marketPda, this.opts.commitment)
        .then((info) => {
          if (info) this.handleAccountChange(info, onSnapshot);
        })
        .catch((e) => {
          console.warn(
            "[mole/frontend] WebSocketFeedAdapter: initial market fetch failed —",
            e,
          );
        });
      if (this.opts.keeperLeaderLockPda) {
        void connection
          .getAccountInfo(this.opts.keeperLeaderLockPda, this.opts.commitment)
          .then((info) => {
            if (info) this.handleLeaderLockChange(info, onSnapshot);
          })
          .catch((e) => {
            console.warn(
              "[mole/frontend] WebSocketFeedAdapter: initial leader-lock fetch failed —",
              e,
            );
          });
      }
    }
    // Wave 17 — opt-in cluster-slot polling.
    if (this.opts.trackClusterSlot && typeof connection.getSlot === "function") {
      const tick = () => {
        const conn = this.connection;
        if (!conn || typeof conn.getSlot !== "function") return;
        // Fire-and-forget; transient failures (RPC blip) shouldn't
        // tear down the adapter. We swallow the error and keep the
        // last known slot value so the banner stays on the previous
        // sample instead of regressing to "uninitialised".
        void conn
          .getSlot(this.opts.commitment)
          .then((slot) => {
            this.state.currentSlot = BigInt(slot);
            this.maybeEmit(onSnapshot);
          })
          .catch((e) => {
            console.warn(
              "[mole/frontend] WebSocketFeedAdapter: getSlot poll failed —",
              e,
            );
          });
      };
      tick();
      this.slotPollHandle = setInterval(tick, this.opts.slotPollIntervalMs);
    }
    this.currentStatus = "connected";

    return () => {
      // Best-effort sync teardown; async cleanup fires-and-forgets.
      this.currentStatus = "idle";
      const conn = this.connection;
      this.connection = null;
      const accountSubId = this.accountSubId;
      const programSubId = this.programSubId;
      const leaderLockSubId = this.leaderLockSubId;
      this.accountSubId = null;
      this.programSubId = null;
      this.leaderLockSubId = null;
      if (this.slotPollHandle !== null) {
        clearInterval(this.slotPollHandle);
        this.slotPollHandle = null;
      }
      if (conn) {
        if (accountSubId !== null) {
          void conn.removeAccountChangeListener(accountSubId);
        }
        if (programSubId !== null) {
          void conn.removeProgramAccountChangeListener(programSubId);
        }
        if (leaderLockSubId !== null) {
          void conn.removeAccountChangeListener(leaderLockSubId);
        }
      }
    };
  }

  // -------------------------------------------------------------------
  // Wave 17 — leader-lock account handler
  // -------------------------------------------------------------------

  private handleLeaderLockChange(
    info: AccountInfo<Buffer>,
    emit: (snapshot: FeedSnapshot) => void,
  ): void {
    // The Anchor account is `8 disc + 49 body = 57 bytes`. Anything
    // shorter means either the PDA hasn't been initialised (RPC
    // returns a zero-length payload for missing accounts in some
    // wallets) or wire corruption. Either way, the banner stays at
    // its previous decoded view rather than crashing the adapter.
    if (!info.data || info.data.length < 8) {
      this.decodeFailures += 1;
      return;
    }
    // Buffer subarray shares the underlying ArrayBuffer; we must
    // copy bytes into a fresh Uint8Array so a later RPC update
    // can't retroactively mutate the value the React tree is
    // reading.
    const copy = new Uint8Array(info.data.length);
    copy.set(info.data);
    this.state.keeperLeaderLockBytes = copy;
    this.maybeEmit(emit);
  }

  // -------------------------------------------------------------------
  // Account change handlers
  // -------------------------------------------------------------------

  private handleAccountChange(
    info: AccountInfo<Buffer>,
    emit: (snapshot: FeedSnapshot) => void,
  ): void {
    try {
      this.state.market = decodeOnchainMarket(info.data);
    } catch (e) {
      this.decodeFailures += 1;
      console.warn(
        "[mole/frontend] WebSocketFeedAdapter: market decode failed —",
        e,
      );
      return;
    }
    this.maybeEmit(emit);
  }

  private handleProgramAccountChange(
    info: KeyedAccountInfo,
    emit: (snapshot: FeedSnapshot) => void,
  ): void {
    const { accountId, accountInfo } = info;
    const data = accountInfo.data;
    if (data.length < 8) {
      this.decodeFailures += 1;
      return;
    }
    const disc = data.subarray(0, 8);
    if (matches(disc, this.opts.discriminators.subPool)) {
      try {
        const sp = decodeOnchainSubPool(data);
        this.state.subPools.set(accountId.toBase58(), sp);
      } catch (e) {
        this.decodeFailures += 1;
        console.warn(
          "[mole/frontend] WebSocketFeedAdapter: sub-pool decode failed —",
          e,
        );
        return;
      }
    } else if (matches(disc, this.opts.discriminators.dormantBucket)) {
      try {
        const b = decodeOnchainDormantBucket(data);
        this.state.dormantBuckets.set(accountId.toBase58(), b);
      } catch (e) {
        this.decodeFailures += 1;
        console.warn(
          "[mole/frontend] WebSocketFeedAdapter: bucket decode failed —",
          e,
        );
        return;
      }
    } else if (matches(disc, this.opts.discriminators.position)) {
      try {
        const pos = decodeOnchainPosition(data);
        const accountHex = info.accountId.toBuffer().toString("hex");
        if (isDisplayablePosition(pos.status)) {
          this.state.positions.set(accountHex, pos);
        } else {
          this.state.positions.delete(accountHex);
        }
      } catch (e) {
        this.decodeFailures += 1;
        console.warn(
          "[mole/frontend] WebSocketFeedAdapter: position decode failed —",
          e,
        );
        return;
      }
    } else {
      // Distribution ledger or unknown account; ignored at this
      // wave (the keeper bot owns this loop on the Rust side).
      return;
    }
    this.maybeEmit(emit);
  }

  // -------------------------------------------------------------------
  // Snapshot aggregation
  // -------------------------------------------------------------------

  private maybeEmit(emit: (snapshot: FeedSnapshot) => void): void {
    const market = this.state.market;
    if (!market) {
      // Hold until the market PDA arrives so consumers never see a
      // snapshot with `null` market.
      return;
    }
    emit(this.aggregate(market));
  }

  private aggregate(market: OnchainMarket): FeedSnapshot {
    const ts = this.opts.now();
    const marketPdaHex = pubkeyForMarket(this.opts.marketPda).hex;
    const marketSummary: MarketSummary = onchainMarketToSummary(
      market,
      marketPdaHex,
      Number(this.state.currentSlot ?? 0n),
    );
    const subPoolIdByHex = new Map<string, number>();
    const subPools: SubPoolSummary[] = Array.from(this.state.subPools).map(
      ([pk, sp]) => {
        const pkHex = pubkey32FromBase58(pk).hex;
        subPoolIdByHex.set(pkHex, sp.sub_pool_id);
        return onchainSubPoolToSummary(sp, pkHex);
      },
    );
    const latestPrice = latestSubPoolPrice(this.state.subPools.values());
    if (latestPrice) {
      marketSummary.midPriceMicro = latestPrice.midPriceMicro;
      marketSummary.lastOracleSlot = latestPrice.lastOracleSlot;
    }
    const dormantBuckets: DormantBucketSummary[] = Array.from(
      this.state.dormantBuckets.values(),
    ).map((b) =>
      onchainDormantBucketToSummary(
        b,
        subPoolIdByHex.get(b.sub_pool.hex) ?? 0,
      ),
    );
    let projectedOutstanding = 0n;
    for (const b of this.state.dormantBuckets.values()) {
      projectedOutstanding += b.total_recovery_notional;
    }
    const positions: PositionSummary[] = [];
    for (const pos of this.state.positions.values()) {
      if (!isDisplayablePosition(pos.status)) continue;
      positions.push(
        onchainPositionToSummary(
          pos,
          subPoolIdByHex.get(pos.sub_pool.hex) ?? 0,
        ),
      );
    }
    const indexer: IndexerSnapshot = {
      slot: 0,
      market: marketSummary,
      subPools,
      dormantBuckets,
      pendingInitHints: [],
      projectedRecoveryOutstandingMicroUsdc: projectedOutstanding,
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
    return {
      indexer,
      keeper,
      positions,
      timestampMs: ts,
      ...(this.state.keeperLeaderLockBytes !== null && {
        keeperLeaderLockBytes: this.state.keeperLeaderLockBytes,
      }),
      ...(this.state.currentSlot !== null && {
        currentSlot: this.state.currentSlot,
      }),
    };
  }
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

function matches(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

function pubkeyForMarket(marketPda: PublicKey): Pubkey32 {
  return { hex: Buffer.from(marketPda.toBytes()).toString("hex") };
}
