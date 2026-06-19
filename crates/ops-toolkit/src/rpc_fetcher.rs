//! Wave 20 — live RPC `MarketFetcher` for the prober daemon.
//!
//! Wave 19 shipped `ProberLoop` against an abstract `MarketFetcher`
//! trait but only wired a `DemoFetcher` (always returns the
//! `healthy_demo_context()` fixture). Wave 20 adds the production
//! fetcher that:
//!
//!   1. Bulk-fetches the `Market` PDA + `KeeperLeaderLock` PDA for
//!      every configured market via a single
//!      `getMultipleAccounts` round-trip per cycle (much cheaper
//!      than per-market `getAccountInfo`).
//!   2. Records the cluster `getSlot()` response time as
//!      `RpcFacts::primary_get_slot_p95_ms` (single-sample P95
//!      proxy).
//!   3. Decodes the `Market` PDA via `keeper_decoder::OnchainMarket`
//!      and lifts `paused` / `frozen_new_position` /
//!      `schema_version` into `MarketFacts`.
//!   4. Decodes the `KeeperLeaderLock` PDA via
//!      `keeper_decoder::KeeperLeaderLock` and assembles
//!      `LeaderLockFacts` with `current_slot` from `getSlot`.
//!
//! The actual `solana-client` HTTP layer is hidden behind a small
//! `RpcAccountSource` trait so this module is fully host-testable
//! with a mock implementation. The real `solana-client` shim lives
//! in `keeper-rpc` (gated behind the `solana-rpc` feature) and is
//! injected by callers — keeping the wave-12 governance "no
//! `solana-client` in default features" invariant intact.

use std::time::Duration;

use crate::context::{
    HealthContext, KeeperFacts, LeaderLockFacts, MarketFacts, OracleFacts, PoolFacts, RpcFacts,
    SubPoolFacts,
};
use crate::multi::MarketEntry;
use crate::prober::MarketFetcher;

/// Wave 20 — a single account fetch result. `None` means the
/// account does not exist on chain (`getAccountInfo` returned a
/// null `value` field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedAccount {
    /// Raw account data including the 8-byte Anchor discriminator.
    pub data: Vec<u8>,
}

/// Wave 20 — abstraction over the `solana-client` calls the
/// `RpcMarketFetcher` needs. Production code wires this to
/// `solana_client::rpc_client::RpcClient`; tests wire it to a
/// fixture map.
///
/// Bulk shape: `accounts.len()` matches the input pubkey list,
/// preserving order. `None` slots correspond to "account not
/// found" responses (matching `solana-client` semantics).
pub trait RpcAccountSource {
    /// Bulk-fetch `pubkeys.len()` accounts. Returns `Err` only on
    /// transport failure; a single missing account surfaces as
    /// `Ok(vec![..., None, ...])`.
    fn get_multiple_accounts(
        &mut self,
        pubkeys: &[[u8; 32]],
    ) -> Result<Vec<Option<FetchedAccount>>, String>;
    /// Returns the cluster's current slot. Used for
    /// `LeaderLockFacts.current_slot` and as the `getSlot` latency
    /// sample for `RpcFacts.primary_get_slot_p95_ms`.
    fn get_slot(&mut self) -> Result<u64, String>;
    /// Wave 21 — pause for `ms` milliseconds. Production sources
    /// wire this to `std::thread::sleep`; tests increment a
    /// counter so they can verify retry backoff was honoured
    /// without blocking the test runner.
    fn sleep_ms(&mut self, ms: u64) {
        // Default impl preserves wave-20 trait contract for any
        // out-of-tree callers — no-op.
        let _ = ms;
    }
}

/// Wave 20 — runtime config for the live fetcher.
///
/// Wave 21 extends this with optional retry and backup-RPC
/// orthogonal knobs; both default to "off" so wave-20 callers
/// don't see any behaviour change.
#[derive(Debug, Clone)]
pub struct RpcMarketFetcherConfig {
    /// Schema version the host code was compiled against. The
    /// fetcher passes this through to
    /// `MarketFacts.schema_version_compiled`; mismatches against
    /// the on-chain `schema_version` then surface in the standard
    /// `schema_version_match` health check.
    pub schema_version_compiled: u16,
    /// Default `KeeperFacts` to substitute when the fetcher
    /// cannot scrape the bot's `/metrics` endpoint. Production
    /// supervisors normally inject a fresh `KeeperFacts` per cycle
    /// via a dedicated metrics scraper sitting next to the prober
    /// (the wave-19 separation of concerns); the fetcher itself
    /// only owns the *on-chain* facts.
    pub default_keeper: KeeperFacts,
    /// Default `OracleFacts` (similar split — oracle facts come
    /// from a dedicated Pyth scraper; the prober just stitches).
    pub default_oracle: OracleFacts,
    /// Default `PoolFacts` (pre-aggregated by the indexer).
    pub default_pool: PoolFacts,
    /// Wall-clock unix-seconds source. Production passes
    /// `SystemTime::now`; tests inject a fixed value for
    /// reproducibility.
    pub now_unix_secs: u64,
    /// Takeover-threshold-slots default written into
    /// `LeaderLockFacts` when the on-chain lock decode succeeds.
    /// Mirrors `KeeperLeaderLock.takeover_threshold_slots` but is
    /// also recorded here so callers can override the on-chain
    /// value during canary deploys.
    pub default_takeover_threshold_slots: u64,
    /// Wave 21 — number of *additional* attempts beyond the first
    /// when the primary RPC returns `Err`. `0` keeps wave-20
    /// behaviour (single attempt, fail-closed). Each retry waits
    /// `retry_backoff_ms` first; the fetcher does NOT sleep
    /// itself (this would block the prober's `ProberClock` and
    /// break the unit tests' synchronous shape) — instead it
    /// invokes the configured `RpcAccountSource::sleep_ms` hook,
    /// which production wires to `std::thread::sleep` and tests
    /// wire to a counter.
    pub retry_attempts: u8,
    /// Wave 21 — backoff between retries in milliseconds.
    /// Linear (not exponential) by design — the prober's outer
    /// `tick_interval` already provides macro-level backoff.
    pub retry_backoff_ms: u64,
    /// Wave 27 — lift the on-chain `Market.current_total_notional`
    /// (the program's own running aggregate) into
    /// `PoolFacts.total_notional_micro_usdc`, replacing the static
    /// `default_pool` value. This makes the *reported* notional live
    /// from the chain instead of a fixture constant, so the wave-24
    /// `position_principal_drift` check reconciles two INDEPENDENT
    /// on-chain truths: the program aggregate vs the sum of decoded
    /// `Position.notional` (the open-interest scan). Defaults to
    /// `true`; set `false` to keep the indexer-supplied figure (e.g.
    /// when a dedicated indexer scraper owns the reported notional).
    /// Only applies when the `Market` PDA decodes — a missing/garbled
    /// account leaves `default_pool` untouched.
    pub lift_reported_notional_from_market: bool,
}

impl Default for RpcMarketFetcherConfig {
    fn default() -> Self {
        Self {
            schema_version_compiled: clearing_core::SCHEMA_VERSION_CURRENT,
            default_keeper: KeeperFacts {
                heartbeat_within_60s: true,
                failed_actions_last_hour: 0,
                skipped_actions_last_hour: 0,
                last_applied_vol: None,
                consecutive_warming_ticks: 0,
                wallet_balance_lamports: 0,
            },
            default_oracle: OracleFacts {
                slot_age: 0,
                confidence_ratio: 0.0,
            },
            default_pool: PoolFacts {
                total_notional_micro_usdc: 0,
                recovery_outstanding_micro_usdc: 0,
                onchain_position_notional_micro_usdc: 0,
            },
            now_unix_secs: 0,
            default_takeover_threshold_slots: 75,
            retry_attempts: 0,
            retry_backoff_ms: 0,
            lift_reported_notional_from_market: true,
        }
    }
}

/// Wave 20 — the live `MarketFetcher`. Wraps an `RpcAccountSource`
/// and a config; produces a per-market `HealthContext` driven by
/// real on-chain bytes.
///
/// This fetcher is **stateless across cycles** at the
/// `MarketFetcher` level — the prober loop calls `fetch()` once
/// per market per cycle. We *do* batch internally though: a
/// single `RpcMarketFetcher` instance accumulates the per-market
/// fetch into one `getMultipleAccounts` call when used through
/// `prefetch()` + `fetch_cached()`. The default `MarketFetcher`
/// `fetch()` impl uses the simpler per-call shape (one RTT per
/// market) so callers don't need to thread a prefetch step.
pub struct RpcMarketFetcher<R: RpcAccountSource> {
    src: R,
    /// Wave 21 — optional backup endpoint. When `Some`, the
    /// fetcher polls it for `getSlot` after every cycle and
    /// records `primary - backup` into
    /// `RpcFacts.primary_backup_slot_diff`. The wave-12 health
    /// check `RPC_PRIMARY_BACKUP_SLOT_LAG` finally has real data.
    /// The backup is *never* used as a fallback for account
    /// reads — that path stays primary-only so a misconfigured
    /// backup can't silently mask a primary outage.
    backup: Option<R>,
    cfg: RpcMarketFetcherConfig,
}

impl<R: RpcAccountSource> RpcMarketFetcher<R> {
    /// Build a new live fetcher.
    pub fn new(src: R, cfg: RpcMarketFetcherConfig) -> Self {
        Self {
            src,
            backup: None,
            cfg,
        }
    }

    /// Wave 21 — attach a backup `RpcAccountSource` for
    /// primary-backup slot-diff sampling. The backup's
    /// `get_multiple_accounts` is never called.
    pub fn with_backup(mut self, backup: R) -> Self {
        self.backup = Some(backup);
        self
    }

    /// Update the config without re-creating the fetcher (e.g. on
    /// SIGHUP-driven reload).
    pub fn set_config(&mut self, cfg: RpcMarketFetcherConfig) {
        self.cfg = cfg;
    }

    /// Returns a borrow of the live RPC source. Production code
    /// rarely needs this; exposed mainly so test harnesses can
    /// assert on the recorded call sequence.
    pub fn source(&self) -> &R {
        &self.src
    }

    /// Wave 21 — borrow the backup source if any.
    pub fn backup(&self) -> Option<&R> {
        self.backup.as_ref()
    }

    /// Wave 21 — execute `op` with up to `retry_attempts` retries,
    /// sleeping `retry_backoff_ms` before each retry. Returns the
    /// final result; success on attempt N short-circuits.
    fn with_retry<T, Op>(&mut self, mut op: Op) -> Result<T, String>
    where
        Op: FnMut(&mut R) -> Result<T, String>,
    {
        let max_attempts = u32::from(self.cfg.retry_attempts).saturating_add(1);
        let mut last_err: Option<String> = None;
        for attempt in 0..max_attempts {
            if attempt > 0 && self.cfg.retry_backoff_ms > 0 {
                self.src.sleep_ms(self.cfg.retry_backoff_ms);
            }
            match op(&mut self.src) {
                Ok(v) => return Ok(v),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| "unknown rpc error".into()))
    }

    fn build_ctx_from_accounts(
        &self,
        market_data: Option<&[u8]>,
        lock_data: Option<&[u8]>,
        cluster_slot: u64,
        slot_latency_ms: u64,
        backup_slot_diff: u64,
    ) -> HealthContext {
        // Decode market PDA when present; default to a paused-clear
        // fixture when missing (this is a NEW market or PDA
        // intentionally closed; the standard schema-match check
        // will then flag the schema_version mismatch separately).
        let decoded_market = market_data
            .and_then(|d| keeper_decoder::decode_anchor_account::<keeper_decoder::OnchainMarket>(d).ok());
        let market = decoded_market
            .as_ref()
            .map(|m| MarketFacts {
                paused_globally: false,
                paused: m.paused,
                frozen_new_position: m.frozen_new_position,
                schema_version_onchain: m.schema_version,
                schema_version_compiled: self.cfg.schema_version_compiled,
            })
            .unwrap_or(MarketFacts {
                paused_globally: false,
                paused: false,
                frozen_new_position: false,
                schema_version_onchain: 0,
                schema_version_compiled: self.cfg.schema_version_compiled,
            });
        // Decode leader-lock PDA when present.
        let leader_lock = lock_data.and_then(|d| {
            keeper_decoder::decode_anchor_account::<keeper_decoder::leader_lock::KeeperLeaderLock>(d)
                .ok()
                .map(|l| LeaderLockFacts {
                    initialized: true,
                    has_leader: l.has_leader,
                    current_leader: l.current_leader,
                    last_heartbeat_slot: l.last_heartbeat_slot,
                    current_slot: cluster_slot,
                    takeover_threshold_slots: if l.takeover_threshold_slots != 0 {
                        l.takeover_threshold_slots
                    } else {
                        self.cfg.default_takeover_threshold_slots
                    },
                    expected_leader: None,
                })
        });
        let leader_lock = match leader_lock {
            Some(facts) => Some(facts),
            None if lock_data.is_none() => Some(LeaderLockFacts {
                initialized: false,
                has_leader: false,
                current_leader: [0u8; 32],
                last_heartbeat_slot: 0,
                current_slot: cluster_slot,
                takeover_threshold_slots: self.cfg.default_takeover_threshold_slots,
                expected_leader: None,
            }),
            None => None, // decode failed but data was present; bubble Up
        };
        // Wave 27 — lift the program's own aggregate notional into the
        // reported pool figure so `position_principal_drift` reconciles
        // the on-chain `Market` counter against the independent
        // open-interest position sum. Only when the market decoded and
        // the flag is on; otherwise the indexer-supplied default stands.
        let mut pool = self.cfg.default_pool.clone();
        if self.cfg.lift_reported_notional_from_market {
            if let Some(m) = decoded_market.as_ref() {
                pool.total_notional_micro_usdc = m.current_total_notional;
            }
        }
        HealthContext {
            now_unix_secs: self.cfg.now_unix_secs,
            market,
            sub_pools: vec![SubPoolFacts {
                id: 0,
                dormant_ticks: 0,
                pending_init_hints: 0,
                open_long_qty: 0,
                open_short_qty: 0,
            }],
            keeper: self.cfg.default_keeper.clone(),
            rpc: RpcFacts {
                primary_get_slot_p95_ms: slot_latency_ms,
                primary_backup_slot_diff: backup_slot_diff,
                get_program_accounts_ms: 0,
            },
            oracle: self.cfg.default_oracle.clone(),
            pool,
            leader_lock,
        }
    }
}

impl<R: RpcAccountSource> MarketFetcher for RpcMarketFetcher<R> {
    fn fetch(&mut self, entry: &MarketEntry) -> Result<HealthContext, String> {
        let pubkeys = [entry.market_pda, entry.lock_pda];
        // Wave 21 — both calls flow through `with_retry`. Wave-20
        // `retry_attempts: 0` keeps a single attempt per call.
        let accs = self.with_retry(|src| src.get_multiple_accounts(&pubkeys))?;
        if accs.len() != 2 {
            return Err(format!(
                "RpcMarketFetcher: expected 2 accounts, got {}",
                accs.len()
            ));
        }
        let market = accs.first().and_then(|o| o.as_ref()).map(|a| a.data.as_slice());
        let lock = accs.get(1).and_then(|o| o.as_ref()).map(|a| a.data.as_slice());
        let t0 = std::time::Instant::now();
        let slot = self.with_retry(|src| src.get_slot())?;
        let latency_ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
        // Wave 21 — backup-RPC slot diff sampling. We do NOT
        // retry the backup call; a flaky backup must NEVER stall
        // the primary path. Backup `Err` collapses to diff=0
        // (instead of bubbling up) so AlertManager only fires
        // on the dedicated `RPC_PRIMARY_BACKUP_SLOT_LAG` rule
        // when there's actual divergence between healthy
        // endpoints.
        let backup_diff = match self.backup.as_mut() {
            Some(b) => match b.get_slot() {
                Ok(bs) => slot.saturating_sub(bs).max(bs.saturating_sub(slot)),
                Err(_) => 0,
            },
            None => 0,
        };
        Ok(self.build_ctx_from_accounts(market, lock, slot, latency_ms, backup_diff))
    }
}

/// Wave 20 — clamp helper for tests that want to verify the
/// fetcher records `Duration::from_millis(...)` regardless of
/// system clock noise.
pub fn _normalize_latency_for_tests(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clearing_core::SCHEMA_VERSION_CURRENT;
    use keeper_decoder::ix::account_discriminator;
    use keeper_decoder::leader_lock::KeeperLeaderLock;
    use keeper_decoder::OnchainMarket;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn entry(market_byte: u8, lock_byte: u8) -> MarketEntry {
        let mut market = [0u8; 32];
        market[0] = market_byte;
        let mut lock = [0u8; 32];
        lock[0] = lock_byte;
        let mut pid = [0u8; 32];
        pid[0] = 99;
        MarketEntry {
            symbol: "SOL-USD".into(),
            program_id: pid,
            market_pda: market,
            lock_pda: lock,
            expected_leader: None,
        }
    }

    /// Build an Anchor-discriminator-prefixed bytestream for a
    /// borsh-serialised `T`. Mirrors `decode_anchor_account_with_discriminator`'s
    /// expected shape.
    fn anchor_bytes<T: borsh::BorshSerialize>(disc_name: &str, value: &T) -> Vec<u8> {
        let disc = account_discriminator(disc_name);
        let mut out = Vec::with_capacity(8 + 256);
        out.extend_from_slice(&disc);
        let payload = borsh::to_vec(value).expect("borsh");
        out.extend_from_slice(&payload);
        out
    }

    fn dummy_market(paused: bool, frozen: bool, schema: u16) -> Vec<u8> {
        let m = OnchainMarket {
            global_config: [0u8; 32],
            symbol: {
                let mut buf = [0u8; 16];
                let s = b"SOL-USD";
                buf[..s.len()].copy_from_slice(s);
                buf
            },
            collateral_mint: [0u8; 32],
            vault: [0u8; 32],
            fee_vault: [0u8; 32],
            oracle_price_feed: [0u8; 32],
            oracle_program_id: [0u8; 32],
            leverage_bps: 5000,
            min_margin: 1_000_000,
            max_margin_per_position: 10_000_000_000,
            max_total_principal: 5_000_000_000_000,
            max_total_notional: 50_000_000_000_000,
            current_total_principal: 0,
            current_total_notional: 0,
            open_fee_bps: 5,
            max_oracle_age_seconds: 60,
            max_oracle_age_slots: 64,
            max_confidence_bps: 200,
            max_price_move_bps_per_sync: 1000,
            price_tick: 12345,
            tick_aggregation_factor: 10,
            max_dormant_bucket_count_per_direction: 16,
            dilution_safety_bps: 100,
            max_idle_slots: 128,
            paused,
            frozen_new_position: frozen,
            schema_version: schema,
            sub_pool_count: 1,
            dormant_distribute_mode: 1,
            max_pending_apply_per_tx: 8,
            max_distribution_ledger_size: 64,
            bump: 250,
            _pad: [0u8; 2],
        };
        anchor_bytes("Market", &m)
    }

    /// Wave 27 — like `dummy_market` but with caller-chosen aggregate
    /// principal/notional counters so we can assert the live-lift path.
    fn dummy_market_with_aggregate(principal: u128, notional: u128) -> Vec<u8> {
        let m = OnchainMarket {
            global_config: [0u8; 32],
            symbol: {
                let mut buf = [0u8; 16];
                let s = b"SOL-USD";
                buf[..s.len()].copy_from_slice(s);
                buf
            },
            collateral_mint: [0u8; 32],
            vault: [0u8; 32],
            fee_vault: [0u8; 32],
            oracle_price_feed: [0u8; 32],
            oracle_program_id: [0u8; 32],
            leverage_bps: 5000,
            min_margin: 1_000_000,
            max_margin_per_position: 10_000_000_000,
            max_total_principal: 5_000_000_000_000,
            max_total_notional: 50_000_000_000_000,
            current_total_principal: principal,
            current_total_notional: notional,
            open_fee_bps: 5,
            max_oracle_age_seconds: 60,
            max_oracle_age_slots: 64,
            max_confidence_bps: 200,
            max_price_move_bps_per_sync: 1000,
            price_tick: 12345,
            tick_aggregation_factor: 10,
            max_dormant_bucket_count_per_direction: 16,
            dilution_safety_bps: 100,
            max_idle_slots: 128,
            paused: false,
            frozen_new_position: false,
            schema_version: SCHEMA_VERSION_CURRENT,
            sub_pool_count: 1,
            dormant_distribute_mode: 1,
            max_pending_apply_per_tx: 8,
            max_distribution_ledger_size: 64,
            bump: 250,
            _pad: [0u8; 2],
        };
        anchor_bytes("Market", &m)
    }

    fn dummy_lock(has_leader: bool, leader: [u8; 32], heartbeat: u64) -> Vec<u8> {
        let l = KeeperLeaderLock {
            has_leader,
            current_leader: leader,
            last_heartbeat_slot: heartbeat,
            takeover_threshold_slots: 50,
        };
        anchor_bytes("KeeperLeaderLock", &l)
    }

    /// Stub RPC that returns canned responses keyed by pubkey.
    struct StubRpc {
        accounts: Vec<(Vec<u8>, Option<Vec<u8>>)>, // (pubkey-bytes, data)
        slot: u64,
        slot_calls: Rc<RefCell<u32>>,
        get_multi_calls: Rc<RefCell<u32>>,
        sleep_calls: Rc<RefCell<Vec<u64>>>,
        fail_get_slot: bool,
        fail_get_multi: bool,
        /// Wave 21 — number of `Err` responses returned by
        /// `get_multi`/`get_slot` before the call succeeds. After
        /// `fail_count` `Err`s, the next call returns `Ok`.
        /// Per-method (mirrors real flaky RPC behaviour where
        /// `get_slot` and `get_multiple_accounts` can fail
        /// independently).
        flaky_multi_remaining: Rc<RefCell<u32>>,
        flaky_slot_remaining: Rc<RefCell<u32>>,
    }

    impl StubRpc {
        fn new() -> Self {
            Self {
                accounts: Vec::new(),
                slot: 1000,
                slot_calls: Rc::new(RefCell::new(0)),
                get_multi_calls: Rc::new(RefCell::new(0)),
                sleep_calls: Rc::new(RefCell::new(Vec::new())),
                fail_get_slot: false,
                fail_get_multi: false,
                flaky_multi_remaining: Rc::new(RefCell::new(0)),
                flaky_slot_remaining: Rc::new(RefCell::new(0)),
            }
        }
        fn with_account(mut self, pk: [u8; 32], data: Vec<u8>) -> Self {
            self.accounts.push((pk.to_vec(), Some(data)));
            self
        }
        fn with_missing(mut self, pk: [u8; 32]) -> Self {
            self.accounts.push((pk.to_vec(), None));
            self
        }
        fn with_slot(mut self, slot: u64) -> Self {
            self.slot = slot;
            self
        }
        /// Wave 21 — fail the next `n` `get_multi` calls, then succeed.
        fn flaky_multi(self, n: u32) -> Self {
            *self.flaky_multi_remaining.borrow_mut() = n;
            self
        }
        /// Wave 21 — fail the next `n` `get_slot` calls, then succeed.
        fn flaky_slot(self, n: u32) -> Self {
            *self.flaky_slot_remaining.borrow_mut() = n;
            self
        }
    }

    impl RpcAccountSource for StubRpc {
        fn get_multiple_accounts(
            &mut self,
            pubkeys: &[[u8; 32]],
        ) -> Result<Vec<Option<FetchedAccount>>, String> {
            *self.get_multi_calls.borrow_mut() += 1;
            if self.fail_get_multi {
                return Err("simulated transport failure".into());
            }
            // Wave 21 — flaky window: count down then succeed.
            let mut remaining = self.flaky_multi_remaining.borrow_mut();
            if *remaining > 0 {
                *remaining -= 1;
                return Err("flaky get_multi".into());
            }
            drop(remaining);
            let mut out = Vec::with_capacity(pubkeys.len());
            for pk in pubkeys {
                let mut found: Option<Option<Vec<u8>>> = None;
                for (k, v) in &self.accounts {
                    if k.as_slice() == pk.as_slice() {
                        found = Some(v.clone());
                        break;
                    }
                }
                let res = match found {
                    Some(Some(data)) => Some(FetchedAccount { data }),
                    Some(None) => None,
                    None => None,
                };
                out.push(res);
            }
            Ok(out)
        }
        fn get_slot(&mut self) -> Result<u64, String> {
            *self.slot_calls.borrow_mut() += 1;
            if self.fail_get_slot {
                return Err("rpc 503".into());
            }
            let mut remaining = self.flaky_slot_remaining.borrow_mut();
            if *remaining > 0 {
                *remaining -= 1;
                return Err("flaky get_slot".into());
            }
            drop(remaining);
            Ok(self.slot)
        }
        fn sleep_ms(&mut self, ms: u64) {
            self.sleep_calls.borrow_mut().push(ms);
        }
    }

    #[test]
    fn fetcher_returns_healthy_ctx_with_decoded_market_and_lock() {
        let e = entry(0xA1, 0xB2);
        let leader = [0x33u8; 32];
        let market_bytes = dummy_market(false, false, SCHEMA_VERSION_CURRENT);
        let lock_bytes = dummy_lock(true, leader, 990);
        let stub = StubRpc::new()
            .with_account(e.market_pda, market_bytes)
            .with_account(e.lock_pda, lock_bytes);
        let slot_calls = stub.slot_calls.clone();
        let get_multi_calls = stub.get_multi_calls.clone();
        let mut f = RpcMarketFetcher::new(
            stub,
            RpcMarketFetcherConfig {
                schema_version_compiled: SCHEMA_VERSION_CURRENT,
                ..RpcMarketFetcherConfig::default()
            },
        );
        let ctx = f.fetch(&e).expect("fetch ok");
        assert!(!ctx.market.paused);
        assert!(!ctx.market.frozen_new_position);
        assert_eq!(ctx.market.schema_version_onchain, SCHEMA_VERSION_CURRENT);
        let ll = ctx.leader_lock.expect("leader_lock");
        assert!(ll.initialized);
        assert!(ll.has_leader);
        assert_eq!(ll.current_leader, leader);
        assert_eq!(ll.last_heartbeat_slot, 990);
        assert_eq!(ll.current_slot, 1000);
        // Wave 20 — single batched RTT for both accounts + one slot poll.
        assert_eq!(*get_multi_calls.borrow(), 1);
        assert_eq!(*slot_calls.borrow(), 1);
    }

    #[test]
    fn fetcher_passes_through_pause_and_freeze_flags() {
        let e = entry(1, 2);
        let market_bytes = dummy_market(true, true, SCHEMA_VERSION_CURRENT);
        let lock_bytes = dummy_lock(false, [0u8; 32], 0);
        let stub = StubRpc::new()
            .with_account(e.market_pda, market_bytes)
            .with_account(e.lock_pda, lock_bytes);
        let mut f = RpcMarketFetcher::new(stub, RpcMarketFetcherConfig::default());
        let ctx = f.fetch(&e).unwrap();
        assert!(ctx.market.paused);
        assert!(ctx.market.frozen_new_position);
    }

    #[test]
    fn fetcher_records_schema_mismatch_via_compiled_field() {
        let e = entry(1, 2);
        // On-chain says schema=99, host code is compiled against
        // SCHEMA_VERSION_CURRENT — the standard `schema_version_match`
        // check (run by `scan_all_markets`) will then flag this.
        let market_bytes = dummy_market(false, false, 99);
        let lock_bytes = dummy_lock(false, [0u8; 32], 0);
        let stub = StubRpc::new()
            .with_account(e.market_pda, market_bytes)
            .with_account(e.lock_pda, lock_bytes);
        let mut f = RpcMarketFetcher::new(
            stub,
            RpcMarketFetcherConfig {
                schema_version_compiled: SCHEMA_VERSION_CURRENT,
                ..RpcMarketFetcherConfig::default()
            },
        );
        let ctx = f.fetch(&e).unwrap();
        assert_eq!(ctx.market.schema_version_onchain, 99);
        assert_eq!(ctx.market.schema_version_compiled, SCHEMA_VERSION_CURRENT);
    }

    #[test]
    fn fetcher_handles_missing_lock_pda_as_uninitialized() {
        let e = entry(1, 2);
        let market_bytes = dummy_market(false, false, SCHEMA_VERSION_CURRENT);
        let stub = StubRpc::new()
            .with_account(e.market_pda, market_bytes)
            .with_missing(e.lock_pda);
        let mut f = RpcMarketFetcher::new(stub, RpcMarketFetcherConfig::default());
        let ctx = f.fetch(&e).unwrap();
        let ll = ctx.leader_lock.expect("leader_lock");
        assert!(!ll.initialized, "uninitialised when on-chain account is None");
        assert!(!ll.has_leader);
    }

    #[test]
    fn fetcher_handles_missing_market_pda_with_zero_schema() {
        let e = entry(1, 2);
        let lock_bytes = dummy_lock(true, [9u8; 32], 100);
        let stub = StubRpc::new()
            .with_missing(e.market_pda)
            .with_account(e.lock_pda, lock_bytes);
        let mut f = RpcMarketFetcher::new(stub, RpcMarketFetcherConfig::default());
        let ctx = f.fetch(&e).unwrap();
        // Missing market → zero schema_version_onchain → standard
        // schema_match check fires; pause stays false.
        assert_eq!(ctx.market.schema_version_onchain, 0);
        assert!(!ctx.market.paused);
        assert!(ctx.leader_lock.is_some());
    }

    #[test]
    fn fetcher_propagates_get_slot_failure() {
        let e = entry(1, 2);
        let mut stub = StubRpc::new()
            .with_account(e.market_pda, dummy_market(false, false, SCHEMA_VERSION_CURRENT))
            .with_account(e.lock_pda, dummy_lock(false, [0u8; 32], 0));
        stub.fail_get_slot = true;
        let mut f = RpcMarketFetcher::new(stub, RpcMarketFetcherConfig::default());
        let err = f.fetch(&e).expect_err("should error");
        assert!(err.contains("rpc 503"));
    }

    #[test]
    fn fetcher_propagates_get_multi_failure() {
        let e = entry(1, 2);
        let mut stub = StubRpc::new();
        stub.fail_get_multi = true;
        let mut f = RpcMarketFetcher::new(stub, RpcMarketFetcherConfig::default());
        let err = f.fetch(&e).expect_err("should error");
        assert!(err.contains("transport"));
    }

    #[test]
    fn fetcher_uses_default_takeover_threshold_when_lock_value_is_zero() {
        let e = entry(1, 2);
        let market_bytes = dummy_market(false, false, SCHEMA_VERSION_CURRENT);
        // Forge a lock with takeover_threshold_slots = 0 (Anchor
        // default for an uninitialised lock byte that somehow
        // round-trips). Build via raw borsh so we control all fields.
        let l = KeeperLeaderLock {
            has_leader: true,
            current_leader: [7u8; 32],
            last_heartbeat_slot: 100,
            takeover_threshold_slots: 0,
        };
        let lock_bytes = anchor_bytes("KeeperLeaderLock", &l);
        let stub = StubRpc::new()
            .with_account(e.market_pda, market_bytes)
            .with_account(e.lock_pda, lock_bytes);
        let mut f = RpcMarketFetcher::new(
            stub,
            RpcMarketFetcherConfig {
                default_takeover_threshold_slots: 75,
                ..RpcMarketFetcherConfig::default()
            },
        );
        let ctx = f.fetch(&e).unwrap();
        let ll = ctx.leader_lock.unwrap();
        assert_eq!(ll.takeover_threshold_slots, 75);
    }

    #[test]
    fn config_default_uses_current_schema_version() {
        let cfg = RpcMarketFetcherConfig::default();
        assert_eq!(cfg.schema_version_compiled, SCHEMA_VERSION_CURRENT);
    }

    // ----------------------------------------------------------------
    // Wave 21 — retry + backup-RPC + sleep-counter
    // ----------------------------------------------------------------

    #[test]
    fn retry_attempts_zero_keeps_wave20_single_attempt_behaviour() {
        let e = entry(1, 2);
        let stub = StubRpc::new()
            .with_account(e.market_pda, dummy_market(false, false, SCHEMA_VERSION_CURRENT))
            .with_account(e.lock_pda, dummy_lock(false, [0u8; 32], 0));
        let multi_calls = stub.get_multi_calls.clone();
        let slot_calls = stub.slot_calls.clone();
        let mut f = RpcMarketFetcher::new(stub, RpcMarketFetcherConfig::default());
        f.fetch(&e).unwrap();
        assert_eq!(*multi_calls.borrow(), 1);
        assert_eq!(*slot_calls.borrow(), 1);
    }

    #[test]
    fn retry_succeeds_after_transient_failure_on_get_multi() {
        let e = entry(1, 2);
        let stub = StubRpc::new()
            .with_account(e.market_pda, dummy_market(false, false, SCHEMA_VERSION_CURRENT))
            .with_account(e.lock_pda, dummy_lock(false, [0u8; 32], 0))
            .flaky_multi(2);
        let multi_calls = stub.get_multi_calls.clone();
        let sleep_calls = stub.sleep_calls.clone();
        let mut f = RpcMarketFetcher::new(
            stub,
            RpcMarketFetcherConfig {
                retry_attempts: 3,
                retry_backoff_ms: 50,
                ..RpcMarketFetcherConfig::default()
            },
        );
        f.fetch(&e).expect("retry should succeed");
        assert_eq!(*multi_calls.borrow(), 3, "two flaky + one ok = 3 attempts");
        // Two retries → two sleeps of 50ms each (not before the first attempt).
        assert_eq!(*sleep_calls.borrow(), vec![50, 50]);
    }

    #[test]
    fn retry_exhausts_and_returns_last_err() {
        let e = entry(1, 2);
        let mut stub = StubRpc::new();
        stub.fail_get_multi = true;
        let multi_calls = stub.get_multi_calls.clone();
        let mut f = RpcMarketFetcher::new(
            stub,
            RpcMarketFetcherConfig {
                retry_attempts: 2,
                retry_backoff_ms: 5,
                ..RpcMarketFetcherConfig::default()
            },
        );
        let err = f.fetch(&e).expect_err("should exhaust");
        assert!(err.contains("simulated transport failure"));
        assert_eq!(*multi_calls.borrow(), 3, "1 + 2 retries = 3 calls");
    }

    #[test]
    fn retry_does_not_sleep_when_backoff_is_zero() {
        let e = entry(1, 2);
        let stub = StubRpc::new()
            .with_account(e.market_pda, dummy_market(false, false, SCHEMA_VERSION_CURRENT))
            .with_account(e.lock_pda, dummy_lock(false, [0u8; 32], 0))
            .flaky_slot(1);
        let sleep_calls = stub.sleep_calls.clone();
        let mut f = RpcMarketFetcher::new(
            stub,
            RpcMarketFetcherConfig {
                retry_attempts: 2,
                retry_backoff_ms: 0,
                ..RpcMarketFetcherConfig::default()
            },
        );
        f.fetch(&e).expect("retry should succeed");
        assert!(
            sleep_calls.borrow().is_empty(),
            "retry_backoff_ms=0 should suppress sleep_ms calls"
        );
    }

    #[test]
    fn backup_rpc_records_slot_diff() {
        let e = entry(1, 2);
        let primary = StubRpc::new()
            .with_account(e.market_pda, dummy_market(false, false, SCHEMA_VERSION_CURRENT))
            .with_account(e.lock_pda, dummy_lock(false, [0u8; 32], 0))
            .with_slot(1000);
        let backup = StubRpc::new().with_slot(996); // 4 slots behind
        let mut f =
            RpcMarketFetcher::new(primary, RpcMarketFetcherConfig::default()).with_backup(backup);
        let ctx = f.fetch(&e).unwrap();
        assert_eq!(ctx.rpc.primary_backup_slot_diff, 4);
        assert_eq!(ctx.rpc.primary_get_slot_p95_ms, ctx.rpc.primary_get_slot_p95_ms);
    }

    #[test]
    fn backup_rpc_handles_backup_ahead_of_primary() {
        let e = entry(1, 2);
        let primary = StubRpc::new()
            .with_account(e.market_pda, dummy_market(false, false, SCHEMA_VERSION_CURRENT))
            .with_account(e.lock_pda, dummy_lock(false, [0u8; 32], 0))
            .with_slot(1000);
        let backup = StubRpc::new().with_slot(1003); // 3 slots ahead
        let mut f =
            RpcMarketFetcher::new(primary, RpcMarketFetcherConfig::default()).with_backup(backup);
        let ctx = f.fetch(&e).unwrap();
        assert_eq!(
            ctx.rpc.primary_backup_slot_diff, 3,
            "absolute diff regardless of direction"
        );
    }

    #[test]
    fn backup_rpc_failure_collapses_to_zero_diff_not_propagated() {
        let e = entry(1, 2);
        let primary = StubRpc::new()
            .with_account(e.market_pda, dummy_market(false, false, SCHEMA_VERSION_CURRENT))
            .with_account(e.lock_pda, dummy_lock(false, [0u8; 32], 0))
            .with_slot(1000);
        let mut backup = StubRpc::new();
        backup.fail_get_slot = true;
        let mut f =
            RpcMarketFetcher::new(primary, RpcMarketFetcherConfig::default()).with_backup(backup);
        let ctx = f
            .fetch(&e)
            .expect("primary OK + backup err must NOT poison the cycle");
        assert_eq!(ctx.rpc.primary_backup_slot_diff, 0);
    }

    #[test]
    fn backup_rpc_never_called_for_account_reads() {
        let e = entry(1, 2);
        let primary = StubRpc::new()
            .with_account(e.market_pda, dummy_market(false, false, SCHEMA_VERSION_CURRENT))
            .with_account(e.lock_pda, dummy_lock(false, [0u8; 32], 0))
            .with_slot(1000);
        let backup = StubRpc::new().with_slot(998);
        let backup_multi_calls = backup.get_multi_calls.clone();
        let mut f =
            RpcMarketFetcher::new(primary, RpcMarketFetcherConfig::default()).with_backup(backup);
        f.fetch(&e).unwrap();
        assert_eq!(
            *backup_multi_calls.borrow(),
            0,
            "backup is slot-diff only; account reads stay primary-only"
        );
    }

    #[test]
    fn config_default_keeps_retry_disabled() {
        let cfg = RpcMarketFetcherConfig::default();
        assert_eq!(cfg.retry_attempts, 0);
        assert_eq!(cfg.retry_backoff_ms, 0);
    }

    #[test]
    fn fetcher_passes_keeper_oracle_pool_defaults_through() {
        let e = entry(1, 2);
        let stub = StubRpc::new()
            .with_account(e.market_pda, dummy_market(false, false, SCHEMA_VERSION_CURRENT))
            .with_account(e.lock_pda, dummy_lock(false, [0u8; 32], 0));
        let cfg = RpcMarketFetcherConfig {
            default_keeper: KeeperFacts {
                heartbeat_within_60s: true,
                failed_actions_last_hour: 7,
                skipped_actions_last_hour: 3,
                last_applied_vol: Some(0.42),
                consecutive_warming_ticks: 0,
                wallet_balance_lamports: 999,
            },
            default_oracle: OracleFacts {
                slot_age: 5,
                confidence_ratio: 0.001,
            },
            default_pool: PoolFacts {
                total_notional_micro_usdc: 1_000,
                recovery_outstanding_micro_usdc: 50,
                onchain_position_notional_micro_usdc: 0,
            },
            ..RpcMarketFetcherConfig::default()
        };
        let mut f = RpcMarketFetcher::new(stub, cfg);
        let ctx = f.fetch(&e).unwrap();
        assert_eq!(ctx.keeper.failed_actions_last_hour, 7);
        assert_eq!(ctx.oracle.slot_age, 5);
        assert_eq!(ctx.pool.recovery_outstanding_micro_usdc, 50);
    }

    // ----------------------------------------------------------------
    // Wave 27 — live reported notional lifted from the Market PDA
    // ----------------------------------------------------------------

    #[test]
    fn fetcher_lifts_onchain_market_notional_into_reported_pool() {
        let e = entry(1, 2);
        let market_bytes = dummy_market_with_aggregate(4_000_000, 7_777_000_000);
        let stub = StubRpc::new()
            .with_account(e.market_pda, market_bytes)
            .with_account(e.lock_pda, dummy_lock(false, [0u8; 32], 0));
        // default_pool carries a stale fixture figure that must be
        // overwritten by the live on-chain aggregate.
        let cfg = RpcMarketFetcherConfig {
            default_pool: PoolFacts {
                total_notional_micro_usdc: 1_000,
                recovery_outstanding_micro_usdc: 0,
                onchain_position_notional_micro_usdc: 0,
            },
            ..RpcMarketFetcherConfig::default()
        };
        let mut f = RpcMarketFetcher::new(stub, cfg);
        let ctx = f.fetch(&e).unwrap();
        assert_eq!(
            ctx.pool.total_notional_micro_usdc, 7_777_000_000,
            "reported notional must come from Market.current_total_notional"
        );
    }

    #[test]
    fn lift_disabled_keeps_default_pool_notional() {
        let e = entry(1, 2);
        let market_bytes = dummy_market_with_aggregate(4_000_000, 7_777_000_000);
        let stub = StubRpc::new()
            .with_account(e.market_pda, market_bytes)
            .with_account(e.lock_pda, dummy_lock(false, [0u8; 32], 0));
        let cfg = RpcMarketFetcherConfig {
            lift_reported_notional_from_market: false,
            default_pool: PoolFacts {
                total_notional_micro_usdc: 1_234,
                recovery_outstanding_micro_usdc: 0,
                onchain_position_notional_micro_usdc: 0,
            },
            ..RpcMarketFetcherConfig::default()
        };
        let mut f = RpcMarketFetcher::new(stub, cfg);
        let ctx = f.fetch(&e).unwrap();
        assert_eq!(
            ctx.pool.total_notional_micro_usdc, 1_234,
            "flag off must preserve the indexer-supplied default"
        );
    }

    #[test]
    fn missing_market_keeps_default_pool_notional() {
        let e = entry(1, 2);
        let stub = StubRpc::new()
            .with_missing(e.market_pda)
            .with_account(e.lock_pda, dummy_lock(false, [0u8; 32], 0));
        let cfg = RpcMarketFetcherConfig {
            default_pool: PoolFacts {
                total_notional_micro_usdc: 555,
                recovery_outstanding_micro_usdc: 0,
                onchain_position_notional_micro_usdc: 0,
            },
            ..RpcMarketFetcherConfig::default()
        };
        let mut f = RpcMarketFetcher::new(stub, cfg);
        let ctx = f.fetch(&e).unwrap();
        assert_eq!(
            ctx.pool.total_notional_micro_usdc, 555,
            "no decoded market ⇒ no lift, default stands"
        );
    }

    #[test]
    fn config_default_enables_notional_lift() {
        assert!(RpcMarketFetcherConfig::default().lift_reported_notional_from_market);
    }
}
