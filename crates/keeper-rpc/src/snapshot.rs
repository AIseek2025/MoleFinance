//! `ChainSnapshot` — concrete `KeeperChainView` impl backed by a
//! batch of fetched on-chain account data.
//!
//! Consumed shape:
//!
//! ```text
//!   Market PDA              → schema_version, sub_pool_count, …
//!   SubPool[0..N] PDAs      → SubPoolHealth + bucket-count hints
//!   DormantBucket PDAs      → BucketSnapshot (filtered by sub_pool)
//!   DistributionLedger PDAs → LedgerSnapshot
//! ```
//!
//! ## Refresh contract
//!
//! [`ChainSnapshot::refresh`] performs `2 * N + N + 1` RPC calls
//! (`N` SubPool fetches, `N` `getProgramAccounts` for buckets per
//! sub-pool, plus the market account). Production keepers should
//! call this once per scheduling tick and reuse the returned
//! snapshot across the [`Scheduler::plan`](keeper::Scheduler::plan)
//! call. We intentionally do NOT batch the calls into a single
//! `getMultipleAccounts` here — the snapshot is owned by the
//! caller and an RPC adapter is free to pipeline them.
//!
//! ## Schema-version safety
//!
//! `refresh` reads `Market::schema_version` and refuses to surface
//! anything if it doesn't match the keeper's compiled
//! `clearing_core::SCHEMA_VERSION_CURRENT`. This is the wave-9
//! "lockdown window" in the keeper's eyes: a multisig that bumped
//! the market's schema version without shipping a matching keeper
//! binary will see the bot stop emitting actions until upgraded.

use std::collections::HashMap;

use clearing_core::{Direction, SCHEMA_VERSION_CURRENT};
use keeper::{BucketSnapshot, KeeperChainView, LedgerSnapshot, SubPoolHealth};

use crate::accounts::{
    decode_anchor_account, OnchainDistributionLedger, OnchainDormantBucket, OnchainMarket,
    OnchainSubPool,
};
use crate::fetcher::{AccountFetcher, RpcError};
use crate::Pubkey32;

/// What the snapshot needs to know up-front about the market it's
/// observing — pubkeys it can't derive from on-chain data alone.
#[derive(Debug, Clone)]
pub struct MarketContext {
    /// The on-chain program id (mole-option program).
    pub program_id: Pubkey32,
    /// The `Market` PDA pubkey.
    pub market: Pubkey32,
    /// The market's symbol bytes (used by the snapshot to derive
    /// the `Market` PDA seeds, e.g. for cross-checking).
    pub market_symbol: [u8; 16],
    /// Pre-computed pubkeys for every `SubPool` the keeper bot
    /// should observe. The bot derives these once at boot using
    /// [`crate::pda::sub_pool_seeds`] + a real
    /// `Pubkey::find_program_address`; the snapshot doesn't re-
    /// derive on every refresh because PDA derivation isn't free.
    pub sub_pools: Vec<SubPoolEntry>,
}

/// One sub-pool's pubkey + id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubPoolEntry {
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// `SubPool` PDA pubkey.
    pub pubkey: Pubkey32,
}

/// Knobs that adjust the snapshot's behaviour.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotConfig {
    /// When `true` (default), `refresh` returns
    /// [`SnapshotError::SchemaVersionMismatch`] if the on-chain
    /// `Market::schema_version` differs from
    /// `SCHEMA_VERSION_CURRENT`. Set to `false` only for forensic
    /// tooling that *wants* to inspect a mis-aligned market.
    pub enforce_schema_version: bool,
    /// When `true` (default), `refresh` returns
    /// [`SnapshotError::MarketPaused`] if the market is paused.
    /// The keeper bot uses this as a clean exit signal: a paused
    /// market should not have keeper actions submitted.
    pub bail_when_paused: bool,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            enforce_schema_version: true,
            bail_when_paused: true,
        }
    }
}

/// Errors `refresh` may surface.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    /// RPC backend error.
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// Failed to decode a fetched account.
    #[error("decode error for {kind} pubkey {pubkey:?}: {source}")]
    Decode {
        /// Account class (`market`, `sub_pool`, `bucket`, `ledger`).
        kind: &'static str,
        /// Pubkey that failed to decode.
        pubkey: Pubkey32,
        /// Underlying decoder error.
        #[source]
        source: crate::accounts::AccountDecodeError,
    },
    /// `Market` PDA was not found.
    #[error("market account {0:?} not found")]
    MarketNotFound(Pubkey32),
    /// One of the configured sub-pool PDAs was not found.
    #[error("sub_pool account {pubkey:?} (id={sub_pool_id}) not found")]
    SubPoolNotFound {
        /// Sub pool id.
        sub_pool_id: u32,
        /// Sub pool PDA.
        pubkey: Pubkey32,
    },
    /// On-chain schema version differs from the compiled-in current
    /// version. Wave-9 lockdown semantics — the keeper refuses to
    /// produce actions until the binary is upgraded.
    #[error(
        "market schema_version mismatch: on-chain={onchain}, keeper-compiled={compiled}"
    )]
    SchemaVersionMismatch {
        /// `Market.schema_version` as decoded.
        onchain: u16,
        /// `clearing_core::SCHEMA_VERSION_CURRENT`.
        compiled: u16,
    },
    /// Market is paused; keeper bot should exit cleanly.
    #[error("market is paused; refusing to surface keeper actions")]
    MarketPaused,
}

/// Decoded snapshot of every account the scheduler reads. Implements
/// [`KeeperChainView`] so it can be passed straight into
/// `Scheduler::plan`.
#[derive(Debug, Clone, Default)]
pub struct ChainSnapshot {
    /// Decoded `Market` account.
    pub market: Option<OnchainMarket>,
    /// Decoded `SubPool` accounts, keyed by `sub_pool_id`.
    pub sub_pools: HashMap<u32, OnchainSubPool>,
    /// Decoded `DormantBucket` accounts grouped by sub_pool.
    pub buckets: HashMap<u32, Vec<OnchainDormantBucket>>,
    /// Decoded `DistributionLedger` accounts keyed by `(sub_pool_id, direction)`.
    pub ledgers: HashMap<(u32, Direction), OnchainDistributionLedger>,
    /// Pubkey-bound metadata used by the executor (so it doesn't
    /// have to re-derive PDAs for actions targeting accounts we
    /// already fetched).
    pub bucket_pubkeys: HashMap<(u32, Direction, i64), Pubkey32>,
    /// Same idea for `DistributionLedger` accounts.
    pub ledger_pubkeys: HashMap<(u32, Direction), Pubkey32>,
}

impl ChainSnapshot {
    /// Construct an empty snapshot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Refresh against the given fetcher + market. All previously
    /// stored data is discarded.
    pub fn refresh<F: AccountFetcher>(
        &mut self,
        fetcher: &F,
        ctx: &MarketContext,
        cfg: SnapshotConfig,
    ) -> Result<(), SnapshotError> {
        self.clear();

        let raw_market = fetcher
            .fetch_account(&ctx.market)?
            .ok_or(SnapshotError::MarketNotFound(ctx.market))?;
        let market: OnchainMarket =
            decode_anchor_account(&raw_market).map_err(|e| SnapshotError::Decode {
                kind: "market",
                pubkey: ctx.market,
                source: e,
            })?;
        if cfg.enforce_schema_version && market.schema_version != SCHEMA_VERSION_CURRENT {
            return Err(SnapshotError::SchemaVersionMismatch {
                onchain: market.schema_version,
                compiled: SCHEMA_VERSION_CURRENT,
            });
        }
        if cfg.bail_when_paused && market.paused {
            return Err(SnapshotError::MarketPaused);
        }
        self.market = Some(market);

        for entry in &ctx.sub_pools {
            let raw_sp = fetcher
                .fetch_account(&entry.pubkey)?
                .ok_or(SnapshotError::SubPoolNotFound {
                    sub_pool_id: entry.sub_pool_id,
                    pubkey: entry.pubkey,
                })?;
            let sp: OnchainSubPool =
                decode_anchor_account(&raw_sp).map_err(|e| SnapshotError::Decode {
                    kind: "sub_pool",
                    pubkey: entry.pubkey,
                    source: e,
                })?;
            self.sub_pools.insert(entry.sub_pool_id, sp);

            // Distribution ledgers (long, short).
            for direction_is_long in [true, false] {
                let dir = if direction_is_long {
                    Direction::Long
                } else {
                    Direction::Short
                };
                // Filter program accounts owned by `program_id`
                // whose `sub_pool` field equals `entry.pubkey` and
                // whose `direction_is_long` byte matches.
                // Layout: [8 disc][32 sub_pool][1 dir]…
                let mut filter_bytes = Vec::with_capacity(33);
                filter_bytes.extend_from_slice(&entry.pubkey);
                filter_bytes.push(direction_is_long as u8);
                let hits = fetcher.fetch_program_accounts_filter(
                    &ctx.program_id,
                    crate::ANCHOR_DISCRIMINATOR_LEN,
                    &filter_bytes,
                )?;
                // The same memcmp filter would also match
                // `DormantBucket` accounts which start with
                // `(sub_pool, direction_is_long)` too. Disambiguate
                // by trying to decode as a ledger; on borsh failure
                // fall through to bucket decoding.
                for (pk, raw) in hits {
                    if let Ok(led) = decode_anchor_account::<OnchainDistributionLedger>(&raw) {
                        if led.sub_pool == entry.pubkey
                            && led.direction_is_long == direction_is_long
                        {
                            self.ledger_pubkeys.insert((entry.sub_pool_id, dir), pk);
                            self.ledgers.insert((entry.sub_pool_id, dir), led);
                            continue;
                        }
                    }
                    if let Ok(bk) = decode_anchor_account::<OnchainDormantBucket>(&raw) {
                        if bk.sub_pool == entry.pubkey
                            && bk.direction_is_long == direction_is_long
                        {
                            self.bucket_pubkeys
                                .insert((entry.sub_pool_id, dir, bk.zero_price_tick), pk);
                            self.buckets
                                .entry(entry.sub_pool_id)
                                .or_default()
                                .push(bk);
                        }
                    }
                }
            }
            // Deterministic bucket order so the scheduler's tie-
            // breaking is stable across refreshes.
            if let Some(v) = self.buckets.get_mut(&entry.sub_pool_id) {
                v.sort_by_key(|b| (b.direction_is_long, b.zero_price_tick));
            }
        }
        Ok(())
    }

    /// Drop every stored entry. Called by `refresh` and exposed so a
    /// keeper bot can wipe the snapshot on market-pause / shutdown.
    pub fn clear(&mut self) {
        self.market = None;
        self.sub_pools.clear();
        self.buckets.clear();
        self.ledgers.clear();
        self.bucket_pubkeys.clear();
        self.ledger_pubkeys.clear();
    }

    /// Decoded `Market`, panicking if `refresh` hasn't been called.
    /// Most callers will hit this through [`KeeperChainView`] and
    /// won't need it directly.
    pub fn market(&self) -> Option<&OnchainMarket> {
        self.market.as_ref()
    }

    /// Look up the stored bucket pubkey for a given action. The
    /// executor uses this to avoid re-deriving the PDA.
    pub fn bucket_pubkey(&self, sub_pool_id: u32, direction: Direction, tick: i64) -> Option<Pubkey32> {
        self.bucket_pubkeys.get(&(sub_pool_id, direction, tick)).copied()
    }

    /// Look up the stored ledger pubkey for a given direction.
    pub fn ledger_pubkey(&self, sub_pool_id: u32, direction: Direction) -> Option<Pubkey32> {
        self.ledger_pubkeys.get(&(sub_pool_id, direction)).copied()
    }
}

impl KeeperChainView for ChainSnapshot {
    fn sub_pool_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.sub_pools.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    fn buckets(&self, sub_pool_id: u32) -> Vec<BucketSnapshot> {
        let Some(entries) = self.buckets.get(&sub_pool_id) else {
            return Vec::new();
        };
        entries
            .iter()
            .map(|b| BucketSnapshot {
                sub_pool_id,
                direction: if b.direction_is_long {
                    Direction::Long
                } else {
                    Direction::Short
                },
                tick: b.zero_price_tick,
                anchor_price: b.anchor_price,
                total_recovery_shares: b.total_recovery_shares,
                total_recovery_notional: b.total_recovery_notional,
                accrued_value: b.accrued_value,
                position_count: b.position_count,
                last_applied_index: b.last_applied_index,
            })
            .collect()
    }

    fn ledger(&self, sub_pool_id: u32, direction: Direction) -> Option<LedgerSnapshot> {
        let lg = self.ledgers.get(&(sub_pool_id, direction))?;
        let sp = self.sub_pools.get(&sub_pool_id)?;
        let bucket_count_hint = match direction {
            Direction::Long => sp.long_dormant_bucket_count,
            Direction::Short => sp.short_dormant_bucket_count,
        };
        Some(LedgerSnapshot {
            sub_pool_id,
            direction,
            next_event_index: lg.next_event_index,
            bucket_count_hint,
        })
    }

    fn sub_pool_health(&self, sub_pool_id: u32) -> Option<SubPoolHealth> {
        let sp = self.sub_pools.get(&sub_pool_id)?;
        // The on-chain SubPool doesn't store anchor prices for the
        // active generation directly — they're stamped into the
        // `RotateLog` at rotate time. For pristine pre-rotate state,
        // anchor == last_price (matches the chain-mirror MirrorView
        // wave-9 contract).
        let anchor = sp.last_price;
        Some(SubPoolHealth {
            sub_pool_id,
            last_price: sp.last_price,
            long_anchor_price: anchor,
            short_anchor_price: anchor,
            long_pool_equity: sp.long_pool_equity,
            short_pool_equity: sp.short_pool_equity,
            long_active_notional: sp.long_active_notional,
            short_active_notional: sp.short_active_notional,
            long_active_generation: sp.long_active_generation,
            short_active_generation: sp.short_active_generation,
        })
    }
}

// Note: per-account seed-layout helpers used to live here but were
// pure re-exports of `pda::*`. They were removed in wave 10 to stop
// duplicating the public PDA API surface — production callers
// (keeper-bot, the `solana-rpc` feature) import directly from
// `crate::pda` now.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{encode_anchor_account, OnchainDistEntry};
    use crate::fetcher::MockAccountFetcher;

    const PROGRAM_ID: Pubkey32 = [9u8; 32];
    const MARKET_PUBKEY: Pubkey32 = [1u8; 32];
    const SUB_POOL_0_PUBKEY: Pubkey32 = [2u8; 32];

    fn dummy_market(schema_version: u16, paused: bool, sub_pool_count: u32) -> OnchainMarket {
        OnchainMarket {
            global_config: [0u8; 32],
            symbol: [0u8; 16],
            collateral_mint: [0u8; 32],
            vault: [0u8; 32],
            fee_vault: [0u8; 32],
            oracle_price_feed: [0u8; 32],
            oracle_program_id: [0u8; 32],
            leverage_bps: 1_000,
            min_margin: 1,
            max_margin_per_position: u64::MAX,
            max_total_principal: u128::MAX,
            max_total_notional: u128::MAX,
            current_total_principal: 0,
            current_total_notional: 0,
            open_fee_bps: 0,
            max_oracle_age_seconds: 60,
            max_oracle_age_slots: 100,
            max_confidence_bps: 100,
            max_price_move_bps_per_sync: 5_000,
            price_tick: 1,
            tick_aggregation_factor: 1,
            max_dormant_bucket_count_per_direction: 100,
            dilution_safety_bps: 100,
            max_idle_slots: 1_000_000,
            paused,
            frozen_new_position: false,
            schema_version,
            sub_pool_count,
            dormant_distribute_mode: 1,
            max_pending_apply_per_tx: 8,
            max_distribution_ledger_size: 64,
            bump: 255,
            _pad: [0u8; 2],
        }
    }

    fn dummy_sub_pool() -> OnchainSubPool {
        OnchainSubPool {
            market: MARKET_PUBKEY,
            sub_pool_id: 0,
            long_pool_equity: 1_000_000_000,
            short_pool_equity: 1_000_000_000,
            long_active_shares: 100_000,
            short_active_shares: 100_000,
            long_recovery_shares: 0,
            short_recovery_shares: 0,
            long_active_notional: 5_000_000_000,
            short_active_notional: 5_000_000_000,
            long_active_generation: 0,
            short_active_generation: 0,
            last_price: 100_000_000,
            last_sync_slot: 1,
            long_dust: 0,
            short_dust: 0,
            long_dormant_bucket_count: 1,
            short_dormant_bucket_count: 1,
            bump: 255,
            _pad: [0u8; 7],
        }
    }

    fn dummy_ledger(direction_is_long: bool, next_event_index: u64) -> OnchainDistributionLedger {
        OnchainDistributionLedger {
            sub_pool: SUB_POOL_0_PUBKEY,
            direction_is_long,
            max_entries: 64,
            gc_offset: 0,
            next_event_index,
            accrued_value_total: 0,
            pending_distribution_total: 0,
            entry_count: 0,
            entries: vec![OnchainDistEntry {
                event_index: 0,
                p_at_event: 100_000_000,
                total_outstanding_at_event: 1_000_000,
                total_alloc_input: 0,
                allocated_sum_observed: 0,
            }],
            bump: 255,
            _pad: [0u8; 7],
        }
    }

    fn dummy_bucket(
        direction_is_long: bool,
        tick: i64,
        last_applied_index: u64,
    ) -> OnchainDormantBucket {
        OnchainDormantBucket {
            sub_pool: SUB_POOL_0_PUBKEY,
            direction_is_long,
            zero_price_tick: tick,
            anchor_price: 100_000_000,
            total_recovery_shares: 100,
            total_recovery_notional: 1_000_000,
            accrued_value: 0,
            position_count: 1,
            last_applied_index,
            bump: 255,
            _pad: [0u8; 6],
        }
    }

    fn build_fixture_fetcher(
        market: OnchainMarket,
        sub_pool: OnchainSubPool,
        ledger_long: OnchainDistributionLedger,
        ledger_short: OnchainDistributionLedger,
        buckets_long: Vec<(Pubkey32, OnchainDormantBucket)>,
    ) -> MockAccountFetcher {
        let disc = [1u8; 8];
        let mut f = MockAccountFetcher::new();
        f.insert(
            MARKET_PUBKEY,
            PROGRAM_ID,
            encode_anchor_account(&market, &disc).unwrap(),
        );
        f.insert(
            SUB_POOL_0_PUBKEY,
            PROGRAM_ID,
            encode_anchor_account(&sub_pool, &disc).unwrap(),
        );
        let lg_long_pk = [10u8; 32];
        let lg_short_pk = [11u8; 32];
        f.insert(
            lg_long_pk,
            PROGRAM_ID,
            encode_anchor_account(&ledger_long, &disc).unwrap(),
        );
        f.insert(
            lg_short_pk,
            PROGRAM_ID,
            encode_anchor_account(&ledger_short, &disc).unwrap(),
        );
        for (pk, b) in buckets_long {
            f.insert(pk, PROGRAM_ID, encode_anchor_account(&b, &disc).unwrap());
        }
        f
    }

    fn ctx() -> MarketContext {
        MarketContext {
            program_id: PROGRAM_ID,
            market: MARKET_PUBKEY,
            market_symbol: [0u8; 16],
            sub_pools: vec![SubPoolEntry {
                sub_pool_id: 0,
                pubkey: SUB_POOL_0_PUBKEY,
            }],
        }
    }

    /// Happy-path refresh decodes all accounts and surfaces them
    /// through the `KeeperChainView` trait.
    #[test]
    fn refresh_decodes_market_subpool_ledgers_and_buckets() {
        let f = build_fixture_fetcher(
            dummy_market(SCHEMA_VERSION_CURRENT, false, 1),
            dummy_sub_pool(),
            dummy_ledger(true, 5),
            dummy_ledger(false, 0),
            vec![(
                [21u8; 32],
                dummy_bucket(true, -100, 3),
            )],
        );
        let mut snap = ChainSnapshot::new();
        snap.refresh(&f, &ctx(), SnapshotConfig::default()).unwrap();
        assert_eq!(snap.sub_pool_ids(), vec![0]);
        let bks = snap.buckets(0);
        assert_eq!(bks.len(), 1);
        assert_eq!(bks[0].tick, -100);
        let lg = snap.ledger(0, Direction::Long).unwrap();
        assert_eq!(lg.next_event_index, 5);
        assert_eq!(lg.bucket_count_hint, 1);
        let h = snap.sub_pool_health(0).unwrap();
        assert_eq!(h.last_price, 100_000_000);
        assert_eq!(h.long_pool_equity, 1_000_000_000);
        assert_eq!(snap.bucket_pubkey(0, Direction::Long, -100), Some([21u8; 32]));
    }

    /// Wave-9 lockdown: a market whose on-chain `schema_version` ≠
    /// `SCHEMA_VERSION_CURRENT` MUST fail the refresh so the keeper
    /// bot stops emitting actions.
    #[test]
    fn refresh_rejects_schema_mismatch_when_enforced() {
        let f = build_fixture_fetcher(
            dummy_market(SCHEMA_VERSION_CURRENT + 1, false, 1),
            dummy_sub_pool(),
            dummy_ledger(true, 0),
            dummy_ledger(false, 0),
            vec![],
        );
        let mut snap = ChainSnapshot::new();
        let err = snap
            .refresh(&f, &ctx(), SnapshotConfig::default())
            .unwrap_err();
        assert!(matches!(err, SnapshotError::SchemaVersionMismatch { .. }));
    }

    /// `bail_when_paused` exits cleanly so the keeper bot can sleep
    /// while the multisig figures out what to do.
    #[test]
    fn refresh_rejects_paused_market_by_default() {
        let f = build_fixture_fetcher(
            dummy_market(SCHEMA_VERSION_CURRENT, true, 1),
            dummy_sub_pool(),
            dummy_ledger(true, 0),
            dummy_ledger(false, 0),
            vec![],
        );
        let mut snap = ChainSnapshot::new();
        let err = snap
            .refresh(&f, &ctx(), SnapshotConfig::default())
            .unwrap_err();
        assert!(matches!(err, SnapshotError::MarketPaused));
    }

    /// Toggling `enforce_schema_version=false` lets forensic tools
    /// inspect a mis-aligned market without the lockdown.
    #[test]
    fn refresh_can_skip_schema_check_for_forensics() {
        let f = build_fixture_fetcher(
            dummy_market(SCHEMA_VERSION_CURRENT + 1, false, 1),
            dummy_sub_pool(),
            dummy_ledger(true, 0),
            dummy_ledger(false, 0),
            vec![],
        );
        let mut snap = ChainSnapshot::new();
        let cfg = SnapshotConfig {
            enforce_schema_version: false,
            bail_when_paused: false,
        };
        snap.refresh(&f, &ctx(), cfg).unwrap();
        assert!(snap.market().is_some());
    }

    /// Sub-pool fetch failure surfaces with both `pubkey` and id
    /// for actionable error reporting.
    #[test]
    fn refresh_reports_missing_sub_pool() {
        let mut f = build_fixture_fetcher(
            dummy_market(SCHEMA_VERSION_CURRENT, false, 1),
            dummy_sub_pool(),
            dummy_ledger(true, 0),
            dummy_ledger(false, 0),
            vec![],
        );
        // Remove the sub pool to simulate a misconfigured ctx.
        f.accounts.remove(&SUB_POOL_0_PUBKEY);
        let mut snap = ChainSnapshot::new();
        let err = snap
            .refresh(&f, &ctx(), SnapshotConfig::default())
            .unwrap_err();
        assert!(matches!(
            err,
            SnapshotError::SubPoolNotFound { sub_pool_id: 0, .. }
        ));
    }

    /// `clear` resets the snapshot — required so the keeper bot can
    /// drop state when a market gets paused.
    #[test]
    fn clear_drops_every_field() {
        let f = build_fixture_fetcher(
            dummy_market(SCHEMA_VERSION_CURRENT, false, 1),
            dummy_sub_pool(),
            dummy_ledger(true, 0),
            dummy_ledger(false, 0),
            vec![([21u8; 32], dummy_bucket(true, -100, 3))],
        );
        let mut snap = ChainSnapshot::new();
        snap.refresh(&f, &ctx(), SnapshotConfig::default()).unwrap();
        snap.clear();
        assert!(snap.market.is_none());
        assert!(snap.sub_pools.is_empty());
        assert!(snap.buckets.is_empty());
        assert!(snap.ledgers.is_empty());
        assert!(snap.bucket_pubkeys.is_empty());
        assert!(snap.ledger_pubkeys.is_empty());
    }
}
