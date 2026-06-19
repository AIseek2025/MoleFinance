// Wave 11 frontend types — mirror the on-chain account fields the
// keeper-rpc snapshot exposes.
//
// Today these are populated by `mocks/feed.ts`; wave 12 will replace
// the mock feed with a live RPC subscriber that decodes Borsh
// payloads via `keeper-rpc::accounts` (compiled to wasm).

export type Direction = "Long" | "Short";

export interface Pubkey32 {
  /** 32-byte hex-encoded pubkey, lowercase, no 0x prefix. */
  hex: string;
}

export interface MarketSummary {
  pubkey: Pubkey32;
  symbol: string;
  schemaVersion: number;
  paused: boolean;
  pausedGlobally: boolean;
  frozenNewPosition: boolean;
  /** Current oracle price scaled by 1e6. */
  midPriceMicro: bigint;
  /** Slot of last oracle update. */
  lastOracleSlot: number;
  /** Current cluster slot. */
  currentSlot: number;
  /**
   * Wave 27 — the program's own running aggregate principal
   * (`Market.current_total_principal`, microUSDC). Optional for
   * back-compat with fixtures / fallback snapshots that don't decode
   * the full market. Reconciled against the live position-collateral
   * sum to detect a program-counter ↔ position-set divergence — the
   * frontend mirror of the backend's wave-27 live reported notional.
   */
  currentTotalPrincipal?: bigint;
  /**
   * Wave 27 — the program's running aggregate notional
   * (`Market.current_total_notional`, microUSDC).
   */
  currentTotalNotional?: bigint;
}

export interface SubPoolSummary {
  id: number;
  pubkey: Pubkey32;
  totalOpenLongQty: bigint;
  totalOpenShortQty: bigint;
  longCollateral: bigint;
  shortCollateral: bigint;
  /** Number of dormant ticks per direction. */
  dormantInventory: Record<Direction, number>;
}

export interface DormantBucketSummary {
  subPoolId: number;
  direction: Direction;
  tick: number;
  totalShares: bigint;
  /** Pending recovery amount in 1e6 microUSDC. */
  pendingRecoveryMicroUsdc: bigint;
  /** True if the bucket is fully synced and ready to close. */
  readyToClose: boolean;
}

export interface PendingInitHint {
  subPoolId: number;
  direction: Direction;
  tick: number;
  /** Slot the hint was first observed on-chain. */
  hintSlot: number;
}

export interface KeeperLoopMetrics {
  /** Slot at the start of the most recent tick. */
  tickSlot: number;
  /** Realized volatility estimate (None when warming up). */
  appliedVol: number | null;
  /** Number of price samples accumulated by the estimator. */
  volSamples: number;
  /** Cumulative submitted/failed/skipped action counts. */
  cumulative: {
    submitted: number;
    failed: number;
    skipped: number;
  };
  /** Most-recent tick deltas. */
  recent: {
    submitted: number;
    failed: number;
    skipped: number;
    durationMs: number;
  };
  walletBalanceSol: number;
}

export interface RotatePrediction {
  subPoolId: number;
  direction: Direction;
  tick: number;
  /** Score in [0, 1] — higher = more urgent. */
  score: number;
  /** Triggered the scheduler to plan an InitDormantBucket action. */
  triggered: boolean;
}

export interface PositionSummary {
  owner: Pubkey32;
  subPoolId: number;
  direction: Direction;
  qty: bigint;
  collateral: bigint;
  /** UNIX seconds when the position was opened. */
  openedAt: number;
  /**
   * Wave 20 — owning market PDA pubkey (32-byte hex, lower-case,
   * matches `MarketViewEntry.marketPdaHex`). Optional for back-compat
   * with single-market snapshots; multi-market adapters populate it
   * so `selectActiveMarketSnapshot` can filter the global
   * `feed.positions` list down to the active market's positions.
   */
  marketPdaHex?: string;
}

export interface IndexerSnapshot {
  slot: number;
  market: MarketSummary;
  subPools: SubPoolSummary[];
  dormantBuckets: DormantBucketSummary[];
  pendingInitHints: PendingInitHint[];
  /** Total recovery outstanding in microUSDC across all buckets. */
  projectedRecoveryOutstandingMicroUsdc: bigint;
}

export interface KeeperState {
  status: "running" | "warming_up" | "paused";
  metrics: KeeperLoopMetrics;
  predictions: RotatePrediction[];
  /** Submitted tx signatures from the most recent tick (newest first). */
  recentSignatures: string[];
}

export interface FeedSnapshot {
  indexer: IndexerSnapshot;
  keeper: KeeperState;
  positions: PositionSummary[];
  timestampMs: number;
  /**
   * Wave 17 — raw bytes of the on-chain `KeeperLeaderLock` PDA
   * (8-byte Anchor account discriminator + 49-byte body). The
   * adapter populates this when subscribed to the lock PDA;
   * frontend renderers decode via wasm `decodeKeeperLeaderLockBytes`.
   *
   * `undefined` means the adapter doesn't yet have a sample (cold
   * start / mock adapter / wave-15 mock-only fixtures). Live
   * adapters MUST set this on every account-change callback for
   * the lock PDA so the `LeaderLockBanner` reflects the current
   * holder identity at the same cadence as the rest of the UI.
   */
  keeperLeaderLockBytes?: Uint8Array;
  /**
   * Wave 17 — current cluster slot reported by the live RPC. When
   * the adapter doesn't track slots (mock fixtures), this stays at
   * 0 and the `LeaderLockBanner` falls back to `indexer.slot`. Kept
   * separate so renderers can correctly age the lock against the
   * **chain's view of "now"** rather than the (possibly stale)
   * keeper bot's tick slot.
   */
  currentSlot?: bigint;
  /**
   * Wave 18 — multi-market view. `MultiMarketFeedAdapter` populates
   * this map keyed by `symbol`; each entry carries the market's
   * stable label + raw `KeeperLeaderLock` bytes. The wave-18
   * `LeaderLockGrid` renders one row per entry. Single-market
   * `WebSocketFeedAdapter` leaves this `undefined`; renderers
   * should fall back to the wave-17 single-market banner shape.
   */
  marketsView?: MultiMarketView;
}

/** Wave 18 — multi-market view payload. */
export interface MultiMarketView {
  /**
   * Per-market entries keyed by stable `symbol` (e.g. "SOL-USD").
   * The adapter populates an entry on the FIRST account update for
   * that market; the entry's `lockBytes` may be `undefined` until
   * the lock PDA's first sample arrives (the grid renderer treats
   * that as "uninitialised" — same semantics as the wave-17 banner).
   */
  entries: Map<string, MarketViewEntry>;
}

/** Wave 18+19 — one market's live snapshot inside `MultiMarketView.entries`. */
export interface MarketViewEntry {
  /** Stable label (matches the keeper-rpc `MarketEntry.symbol`). */
  symbol: string;
  /** `Market` PDA pubkey (32-byte hex, lower-case, no `0x`). */
  marketPdaHex: string;
  /** `KeeperLeaderLock` PDA pubkey (32-byte hex, lower-case). */
  lockPdaHex: string;
  /**
   * Most-recent raw bytes from the lock PDA's `accountSubscribe`
   * callback (8 disc + 49 body = 57 bytes). `undefined` until the
   * first update arrives — renderers MUST treat that as
   * `LeaderLockState::uninitialised`.
   */
  lockBytes?: Uint8Array;
  /**
   * Most-recent raw bytes from the market PDA's `accountSubscribe`
   * callback. Wave-19 decodes this into `marketSummary`; raw bytes
   * remain available so future decoders (e.g. an extended schema)
   * can reuse the same bytes without re-fetching.
   */
  marketBytes?: Uint8Array;
  /**
   * Wave 19 — decoded per-market `OnchainMarket` view. Set when
   * the adapter has the discriminators wired and the market PDA
   * has emitted at least one `accountSubscribe` callback whose
   * payload decoded successfully.
   */
  marketSummary?: MarketSummary;
  /**
   * Wave 19 — per-market sub-pool roster. Adapter routes each
   * `onProgramAccountChange(programId)` sub-pool update to the
   * owning market by matching `OnchainSubPool.market` against
   * `marketPdaHex`. Empty array until the first sub-pool arrives.
   */
  subPools?: SubPoolSummary[];
  /**
   * Wave 19 — per-market dormant-bucket roster. Adapter routes via
   * each bucket's `OnchainDormantBucket.sub_pool` field — looking
   * up the parent sub-pool's owning market.
   */
  dormantBuckets?: DormantBucketSummary[];
  /**
   * Wave 19 — per-market projected-recovery total (sum of every
   * dormant bucket's `total_recovery_notional` for this market).
   * Drives the indexer panel's "outstanding recovery" KPI.
   */
  projectedRecoveryOutstandingMicroUsdc?: bigint;
  /**
   * Wave 19 — per-market indexer slot. Computed as
   * `max(sub_pool.last_sync_slot)` so the trader / indexer panel
   * can age its data against the most recent on-chain write.
   */
  indexerSlot?: number;
  /**
   * Wave 20 — per-market keeper state. When the keeper-bot
   * publishes per-market `KeeperLoopMetrics` (multi-market run
   * loop scenario), the multi-market adapter / mock generator
   * populates this field. `KeeperPanel` reads it via
   * `selectActiveMarketSnapshot` so multi-market deployments can
   * see which market is consuming most of the CU / wallet budget.
   * `undefined` when the bot is still single-market or hasn't
   * published per-market metrics yet — consumers should fall back
   * to the global `feed.keeper` shape.
   */
  keeperState?: KeeperState;
}
