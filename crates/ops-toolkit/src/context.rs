//! Input data structures for the health prober.
//!
//! The prober (or keeper-bot's serve daemon) populates one
//! [`HealthContext`] per probe cycle and hands it to
//! [`crate::run_all_checks`]. Every field is what an operator with
//! `solana account` + `curl /metrics` access can observe; nothing
//! depends on private keeper state.

/// Single-context snapshot the prober gathers per probe cycle.
#[derive(Debug, Clone)]
pub struct HealthContext {
    /// Wall-clock unix-seconds the snapshot was taken.
    pub now_unix_secs: u64,
    /// On-chain market facts.
    pub market: MarketFacts,
    /// Aggregated sub-pool facts (one entry per active sub-pool).
    pub sub_pools: Vec<SubPoolFacts>,
    /// Aggregated keeper-bot facts (typically scraped from the
    /// `/metrics` endpoint of one or both keeper replicas).
    pub keeper: KeeperFacts,
    /// RPC endpoint health.
    pub rpc: RpcFacts,
    /// Pyth / oracle health.
    pub oracle: OracleFacts,
    /// Aggregated pool-level totals (computed by the prober from
    /// the per-sub-pool facts but materialised here for perf —
    /// some checks need the totals only).
    pub pool: PoolFacts,
    /// Wave 17 — keeper-leader-lock facts (None when the prober
    /// runs in single-replica mode where leader-lock isn't wired).
    pub leader_lock: Option<LeaderLockFacts>,
}

/// Wave 17 — facts about the on-chain `KeeperLeaderLock` PDA.
///
/// The prober fetches the lock PDA + the cluster's current slot and
/// fills this struct. Three booleans plus two slot fields give the
/// health checks everything they need to flag a stale, missing, or
/// unexpected holder.
#[derive(Debug, Clone)]
pub struct LeaderLockFacts {
    /// `false` means the PDA returned `account_not_found` from the
    /// cluster — the wave-15 / wave-16 init ix has never run for
    /// this market. Operator action: send
    /// `initialize_keeper_leader_lock` (runbook KL-01).
    pub initialized: bool,
    /// `KeeperLeaderLock.has_leader` from the on-chain payload.
    /// `false` after a graceful release or after takeover before
    /// the standby's first heartbeat. `initialized=true` AND
    /// `has_leader=false` means "PDA is healthy but currently
    /// unowned" — a transient state that should resolve within a
    /// few standby reconcile cadences.
    pub has_leader: bool,
    /// `KeeperLeaderLock.current_leader` (32 bytes). Garbage when
    /// `has_leader = false`; the prober should still pass through
    /// the raw bytes for debugging.
    pub current_leader: [u8; 32],
    /// `KeeperLeaderLock.last_heartbeat_slot`. Used together with
    /// `current_slot` and `takeover_threshold_slots` to age the
    /// lock; a slot delta near the threshold is the early warning
    /// for "the leader bot is unhealthy".
    pub last_heartbeat_slot: u64,
    /// `KeeperLeaderLock.takeover_threshold_slots` — the wave-15
    /// configurable that determines when a standby may force-
    /// acquire. Wave-15 default is 75 slots ≈ 30 s.
    pub takeover_threshold_slots: u64,
    /// Cluster `current_slot` reading (from `getSlot`).
    pub current_slot: u64,
    /// Wave-12 ops-toolkit only — the keeper pubkey the operator
    /// EXPECTS to currently hold the lock. The prober reads this
    /// from a config file (typically the active replica's wallet
    /// pubkey). When `current_leader != expected_leader` the
    /// `keeper_leader_holder_matches_expected` check goes critical.
    /// `None` means "operator hasn't designated an expected holder
    /// yet" — the check then degrades to a warn-level "no expected
    /// holder configured" advisory.
    pub expected_leader: Option<[u8; 32]>,
}

/// On-chain `Market` + `GlobalConfig` snapshot.
#[derive(Debug, Clone)]
pub struct MarketFacts {
    /// `GlobalConfig.paused_globally`.
    pub paused_globally: bool,
    /// `Market.paused`.
    pub paused: bool,
    /// `Market.frozen_new_position`.
    pub frozen_new_position: bool,
    /// On-chain `Market.schema_version`.
    pub schema_version_onchain: u16,
    /// `clearing_core::SCHEMA_VERSION_CURRENT` at probe build time.
    pub schema_version_compiled: u16,
}

/// Per sub-pool snapshot.
#[derive(Debug, Clone)]
pub struct SubPoolFacts {
    /// Sub-pool id.
    pub id: u32,
    /// Number of dormant ticks (Long + Short).
    pub dormant_ticks: u32,
    /// Pending init hints queued on `DistributionLedger`.
    pub pending_init_hints: u32,
    /// Total open Long quantity.
    pub open_long_qty: u128,
    /// Total open Short quantity.
    pub open_short_qty: u128,
}

/// Cross-pool aggregated facts.
#[derive(Debug, Clone, Default)]
pub struct PoolFacts {
    /// Total notional principal in USDC microUSDC across all
    /// open positions. Used as the denominator for the recovery-
    /// outstanding ratio.
    pub total_notional_micro_usdc: u128,
    /// Sum of `pending_recovery` across every dormant bucket, in
    /// microUSDC.
    pub recovery_outstanding_micro_usdc: u128,
    /// Wave 24 — total notional aggregated *from decoded on-chain
    /// `Position` PDAs* (`OpenInterestFacts::total_notional`),
    /// fed by the prober's `fetch_open_interest` scan. This is the
    /// independent on-chain truth the `position_principal_drift`
    /// check reconciles against the indexer-reported
    /// `total_notional_micro_usdc`. `0` means the open-interest
    /// probe was not run this cycle — the drift check then *skips*
    /// (returns `Pass`) so single-source probers don't false-alarm.
    pub onchain_position_notional_micro_usdc: u128,
}

/// Keeper-bot facts (scraped from `/metrics` or aggregated from
/// multiple replicas).
#[derive(Debug, Clone, Default)]
pub struct KeeperFacts {
    /// Has at least one keeper replica reported a heartbeat in the
    /// past 60s?
    pub heartbeat_within_60s: bool,
    /// Failed actions observed in the past 1 hour.
    pub failed_actions_last_hour: u64,
    /// Skipped actions observed in the past 1 hour.
    pub skipped_actions_last_hour: u64,
    /// `applied_vol` value on the most recent tick (None == warming up).
    pub last_applied_vol: Option<f64>,
    /// Number of consecutive ticks where `applied_vol` was None.
    /// Triggers `KEEPER_VOL_DEGRADED` at >= 3.
    pub consecutive_warming_ticks: u32,
    /// Wallet balance in lamports.
    pub wallet_balance_lamports: u64,
}

/// RPC fleet facts.
#[derive(Debug, Clone, Default)]
pub struct RpcFacts {
    /// p95 `getSlot` latency on the primary endpoint, in ms.
    pub primary_get_slot_p95_ms: u64,
    /// Slot delta between primary and backup endpoints.
    pub primary_backup_slot_diff: u64,
    /// `getProgramAccounts` round-trip time in ms.
    pub get_program_accounts_ms: u64,
}

/// Oracle facts.
#[derive(Debug, Clone, Default)]
pub struct OracleFacts {
    /// `current_slot - last_oracle_slot` for the active price feed.
    pub slot_age: u64,
    /// `confidence_interval / mid_price`. 0.005 == 0.5 %.
    pub confidence_ratio: f64,
}
