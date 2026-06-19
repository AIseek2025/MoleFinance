//! Protocol-level harness for MoleOption.
//!
//! Wraps `clearing-core`, `indexer`, and a simulated SPL vault into one
//! end-to-end state machine, so we can run adversarial multi-trader
//! scenarios entirely on the host while still exercising the exact
//! algorithms the on-chain program runs.
//!
//! ## Vault accounting model (whitepaper §4 & spec §18)
//!
//! Two notional token accounts are simulated:
//!
//! - `vault_balance` holds **everything backing user claims**:
//!   `sum(pool_equity) + sum(dormant_accrued) + sum(dust_unswept)`.
//! - `fee_vault_balance` holds protocol revenue:
//!   `sum(open_fees) + sum(swept_dust)`.
//!
//! Every state-changing operation must preserve the invariant:
//!
//! ```text
//! total_deposits == total_withdrawals + vault_balance + fee_vault_balance
//! vault_balance == sum_pool_equity + sum_dormant_accrued + sum_dust
//! ```
//!
//! [`Harness::check_invariants`] is callable after every op and is wired
//! into the random-walk property tests.
//!
//! ## What is NOT modeled
//!
//! - Solana account lifecycle (PDAs, rent, init), SPL CPI mechanics,
//!   Anchor account validation. Those are exercised by the on-chain
//!   integration suite (future wave) and by the `pyth-adapter` unit
//!   tests for oracle bytes.
//! - Compute units. A separate benchmark crate will profile clearing-core
//!   ops as a CU proxy.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::HashMap;

use clearing_core::{
    claim_dormant_recovery, close_position, force_close_zero_value_position, harvest_dust,
    open_position, pre_sync_dormant_bucket, sync_pool, ClearingError, Direction, EngineEvent,
    MarketParams, Position, PreSyncOutcome, PriceEnvelope, SubPool,
};
use indexer::IndexerState;
use thiserror::Error;

/// Errors returned by the harness.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HarnessError {
    /// Sub pool id not registered.
    #[error("sub pool {0} not registered")]
    SubPoolNotFound(u32),
    /// Position id not tracked.
    #[error("position {0} not found")]
    PositionNotFound(u64),
    /// Caller-supplied position id collides with an existing one.
    #[error("position id {0} already exists")]
    DuplicatePositionId(u64),
    /// Underlying clearing-core error.
    #[error("clearing: {0}")]
    Clearing(ClearingError),
    /// Indexer rejected an event.
    #[error("indexer: {0}")]
    Indexer(indexer::IndexerError),
    /// Conservation invariant violated.
    #[error(
        "conservation broken: deposits={deposits} withdrawals={withdrawals} state={state}"
    )]
    InvariantConservation {
        /// Total tokens deposited via `open`.
        deposits: u128,
        /// Total tokens released via `close` / `claim_recovery`.
        withdrawals: u128,
        /// Total state value (`vault + fee_vault`).
        state: u128,
    },
    /// `vault_balance` does not match `sum(pool_equity) + sum(dormant) + sum(dust)`.
    #[error(
        "vault state mismatch: vault={vault} expected={expected} pool_equity={pool_equity} dormant={dormant} pending={pending} dust={dust}"
    )]
    InvariantVault {
        /// Tracked vault balance.
        vault: u128,
        /// Expected balance derived from sub pool aggregates.
        expected: u128,
        /// Sum of all sub pool `long_pool_equity + short_pool_equity`.
        pool_equity: u128,
        /// Sum of all dormant `accrued_value_total` across both sides
        /// (already-attributed-to-bucket recovery balance).
        dormant: u128,
        /// Sum of all dormant `pending_distribution_total` across both
        /// sides (lazy-mode in-flight allocations not yet applied to
        /// any bucket's `accrued_value`).
        pending: u128,
        /// Sum of all `<dir>_dust`.
        dust: u128,
    },
    /// Indexer view drifted from chain `withdrawable` past the configured threshold.
    #[error(
        "indexer drift exceeded: indexer={indexer} chain={chain} drift={drift} threshold={threshold}"
    )]
    IndexerDrift {
        /// Indexer pre-close equity.
        indexer: u128,
        /// Chain reported withdrawable.
        chain: u128,
        /// Absolute drift.
        drift: u128,
        /// Caller-set tolerance.
        threshold: u128,
    },
    /// A clearing-core invariant tripped that the harness considers fatal.
    #[error("invariant: {0}")]
    Invariant(&'static str),
}

impl From<ClearingError> for HarnessError {
    fn from(e: ClearingError) -> Self {
        HarnessError::Clearing(e)
    }
}

impl From<indexer::IndexerError> for HarnessError {
    fn from(e: indexer::IndexerError) -> Self {
        HarnessError::Indexer(e)
    }
}

/// Snapshot of state useful for tests and assertions.
#[derive(Debug, Clone, Copy, Default)]
pub struct StateSummary {
    /// Total deposits the harness has ever observed.
    pub total_deposits: u128,
    /// Total withdrawals the harness has ever paid out.
    pub total_withdrawals: u128,
    /// Tokens currently held to back open user claims (pool / dormant / dust).
    pub vault_balance: u128,
    /// Tokens currently held as protocol revenue.
    pub fee_vault_balance: u128,
    /// Sum of `(long_pool_equity + short_pool_equity)` across all sub pools.
    pub pool_equity_total: u128,
    /// Sum of `(long_dormant + short_dormant)` accrued value across all sub pools.
    pub dormant_accrued_total: u128,
    /// Sum of `(long_dormant + short_dormant)` lazy-mode pending
    /// distribution total across all sub pools. In eager mode this is
    /// always 0; in lazy mode it captures the allocation amount that
    /// has left `pool_equity` via `distribute_lazy` but has not yet
    /// been attributed to any bucket's `accrued_value` via
    /// `apply_pending_to_bucket`. Required for vault decomposition
    /// to balance under lazy mode at every step (wave 5.5).
    pub dormant_pending_total: u128,
    /// Sum of `(long_dust + short_dust)` across all sub pools.
    pub dust_total: u128,
    /// Number of registered sub pools.
    pub sub_pool_count: usize,
    /// Number of registered positions (open + closed).
    pub position_count: usize,
}

/// Outcome of an `open` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenSummary {
    /// Newly assigned position id.
    pub position_id: u64,
    /// Active shares minted.
    pub shares_minted: u128,
    /// Open fee credited to fee vault.
    pub open_fee: u64,
}

/// Result of [`Harness::close`] or [`Harness::claim_recovery`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseSummary {
    /// Tokens released to the user.
    pub withdrawable: u128,
    /// Indexer's pre-call equity for the position (drift bound).
    pub indexer_pre_equity: u128,
}

/// End-to-end harness. See module docs.
#[derive(Debug)]
pub struct Harness {
    market: MarketParams,
    sub_pools: HashMap<u32, SubPool>,
    positions: HashMap<u64, Position>,
    next_position_id: u64,

    vault_balance: u128,
    fee_vault_balance: u128,

    indexer: IndexerState,

    total_deposits: u128,
    total_withdrawals: u128,

    /// Per-position cumulative open_fee + dust contributions, used by tests.
    fees_collected: u128,
    /// Per-position cumulative swept dust.
    dust_swept: u128,
}

impl Harness {
    /// Construct a fresh harness for a given market configuration.
    pub fn new(market: MarketParams) -> Self {
        Self {
            market,
            sub_pools: HashMap::new(),
            positions: HashMap::new(),
            next_position_id: 1,
            vault_balance: 0,
            fee_vault_balance: 0,
            indexer: IndexerState::new(),
            total_deposits: 0,
            total_withdrawals: 0,
            fees_collected: 0,
            dust_swept: 0,
        }
    }

    /// Borrow the underlying market params.
    pub fn market(&self) -> &MarketParams {
        &self.market
    }

    /// Borrow the indexer state (read-only).
    pub fn indexer(&self) -> &IndexerState {
        &self.indexer
    }

    /// Returns the most recent vault and fee-vault balances.
    pub fn balances(&self) -> (u128, u128) {
        (self.vault_balance, self.fee_vault_balance)
    }

    /// Number of currently tracked sub pools.
    pub fn sub_pool_count(&self) -> usize {
        self.sub_pools.len()
    }

    /// Read-only access to a sub pool snapshot.
    pub fn sub_pool(&self, id: u32) -> Option<&SubPool> {
        self.sub_pools.get(&id)
    }

    /// Read-only access to a position snapshot.
    pub fn position(&self, id: u64) -> Option<&Position> {
        self.positions.get(&id)
    }

    /// Whole-state summary, useful for assertions.
    pub fn summary(&self) -> StateSummary {
        let mut s = StateSummary {
            total_deposits: self.total_deposits,
            total_withdrawals: self.total_withdrawals,
            vault_balance: self.vault_balance,
            fee_vault_balance: self.fee_vault_balance,
            pool_equity_total: 0,
            dormant_accrued_total: 0,
            dormant_pending_total: 0,
            dust_total: 0,
            sub_pool_count: self.sub_pools.len(),
            position_count: self.positions.len(),
        };
        for sp in self.sub_pools.values() {
            s.pool_equity_total += sp.long_pool_equity + sp.short_pool_equity;
            s.dormant_accrued_total += sp.long_dormant.accrued_value_total()
                + sp.short_dormant.accrued_value_total();
            s.dormant_pending_total += sp.long_dormant.pending_distribution_total()
                + sp.short_dormant.pending_distribution_total();
            s.dust_total += sp.long_dust + sp.short_dust;
        }
        s
    }

    /// Register a new sub pool. Idempotent (overwrites if id already exists).
    pub fn add_sub_pool(&mut self, sub_pool_id: u32, init_price: u64, init_slot: u64) {
        let sp = SubPool::new(sub_pool_id, init_price, init_slot);
        self.sub_pools.insert(sub_pool_id, sp);
    }

    /// Open a new position with a fresh, harness-assigned id.
    pub fn open(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
        gross_amount: u64,
        envelope: PriceEnvelope,
    ) -> Result<OpenSummary, HarnessError> {
        let sp = self
            .sub_pools
            .get_mut(&sub_pool_id)
            .ok_or(HarnessError::SubPoolNotFound(sub_pool_id))?;
        let position_id = self.next_position_id;
        self.next_position_id += 1;
        if self.positions.contains_key(&position_id) {
            return Err(HarnessError::DuplicatePositionId(position_id));
        }

        // Solana tx-revert semantics: snapshot before any mutation. If the
        // engine call returns Err, the on-chain runtime atomically reverts
        // every account write. The host-side reference must emulate this
        // — otherwise the chain mutates while the indexer never sees the
        // (failed) events, producing a real divergence between chain and
        // indexer state.
        let sp_snap = sp.clone();
        let next_id_snap = self.next_position_id;

        let (mut position, outcome) = match open_position(
            &self.market,
            sp,
            envelope,
            direction,
            gross_amount,
            position_id,
        ) {
            Ok(r) => r,
            Err(e) => {
                *sp = sp_snap;
                self.next_position_id = next_id_snap.saturating_sub(1);
                return Err(HarnessError::Clearing(e));
            }
        };
        let _ = next_id_snap;
        // Persist position (engine wrote `position_id` and direction already).
        position.sub_pool_id = sub_pool_id;
        self.positions.insert(position_id, position);

        // Vault accounting: the user paid `gross_amount`. Of that:
        //   - `principal_into_pool + dust` backs user state -> vault
        //   - `open_fee` is protocol revenue                -> fee_vault
        let principal_part = outcome
            .principal_into_pool
            .checked_add(outcome.dust)
            .ok_or(HarnessError::Invariant("open principal + dust overflow"))?;
        let fee_part = outcome.open_fee as u128;
        let total = principal_part
            .checked_add(fee_part)
            .ok_or(HarnessError::Invariant("open total overflow"))?;
        if total != gross_amount as u128 {
            return Err(HarnessError::Invariant(
                "open accounting: principal + dust + open_fee != gross_amount",
            ));
        }
        self.vault_balance += principal_part;
        self.fee_vault_balance += fee_part;
        self.fees_collected += fee_part;
        self.total_deposits += gross_amount as u128;

        self.indexer.apply_all(&outcome.events)?;

        Ok(OpenSummary {
            position_id,
            shares_minted: outcome.shares_minted,
            open_fee: outcome.open_fee,
        })
    }

    /// Close a position. Returns the withdrawable amount.
    pub fn close(
        &mut self,
        position_id: u64,
        envelope: PriceEnvelope,
    ) -> Result<CloseSummary, HarnessError> {
        let mut position = self
            .positions
            .remove(&position_id)
            .ok_or(HarnessError::PositionNotFound(position_id))?;
        let sub_pool_id = position.sub_pool_id;
        let sp = self
            .sub_pools
            .get_mut(&sub_pool_id)
            .ok_or(HarnessError::SubPoolNotFound(sub_pool_id))?;

        // Capture indexer's pre-call view for drift bookkeeping.
        let indexer_pre_equity = self
            .indexer
            .position(position_id)
            .map(|v| v.equity())
            .unwrap_or(0);

        // Solana tx-revert semantics — see `open` for context.
        let sp_snap = sp.clone();
        let pos_snap = position.clone();

        let outcome = match close_position(&self.market, sp, envelope, &mut position) {
            Ok(o) => o,
            Err(e) => {
                *sp = sp_snap;
                position = pos_snap;
                self.positions.insert(position_id, position);
                return Err(HarnessError::Clearing(e));
            }
        };

        self.vault_balance = self
            .vault_balance
            .checked_sub(outcome.withdrawable)
            .ok_or(HarnessError::Invariant("close drained vault below zero"))?;
        self.total_withdrawals += outcome.withdrawable;

        self.positions.insert(position_id, position);
        self.indexer.apply_all(&outcome.events)?;

        Ok(CloseSummary {
            withdrawable: outcome.withdrawable,
            indexer_pre_equity,
        })
    }

    /// Force-close a zero-value position. Returns 0 because no funds flow
    /// to the user; recovery (if any) is forfeited to dust.
    pub fn force_close(
        &mut self,
        position_id: u64,
        envelope: PriceEnvelope,
        acknowledge_forfeit: bool,
    ) -> Result<(), HarnessError> {
        let mut position = self
            .positions
            .remove(&position_id)
            .ok_or(HarnessError::PositionNotFound(position_id))?;
        let sub_pool_id = position.sub_pool_id;
        let sp = self
            .sub_pools
            .get_mut(&sub_pool_id)
            .ok_or(HarnessError::SubPoolNotFound(sub_pool_id))?;

        // Solana tx-revert semantics — see `open` for context.
        let sp_snap = sp.clone();
        let pos_snap = position.clone();

        let outcome = match force_close_zero_value_position(
            &self.market,
            sp,
            envelope,
            &mut position,
            acknowledge_forfeit,
        ) {
            Ok(o) => o,
            Err(e) => {
                *sp = sp_snap;
                position = pos_snap;
                self.positions.insert(position_id, position);
                return Err(HarnessError::Clearing(e));
            }
        };

        // No vault movement: forfeited recovery moves bucket.accrued -> subpool.dust,
        // both inside `vault_balance`'s state-side total.
        self.positions.insert(position_id, position);
        self.indexer.apply_all(&outcome.events)?;
        Ok(())
    }

    /// Claim recovery accrual without closing.
    pub fn claim_recovery(
        &mut self,
        position_id: u64,
        envelope: PriceEnvelope,
    ) -> Result<CloseSummary, HarnessError> {
        let mut position = self
            .positions
            .remove(&position_id)
            .ok_or(HarnessError::PositionNotFound(position_id))?;
        let sub_pool_id = position.sub_pool_id;
        let sp = self
            .sub_pools
            .get_mut(&sub_pool_id)
            .ok_or(HarnessError::SubPoolNotFound(sub_pool_id))?;

        let indexer_pre_equity = self
            .indexer
            .position(position_id)
            .map(|v| v.equity())
            .unwrap_or(0);

        // Solana tx-revert semantics — see `open` for context.
        let sp_snap = sp.clone();
        let pos_snap = position.clone();

        let outcome = match claim_dormant_recovery(&self.market, sp, envelope, &mut position) {
            Ok(o) => o,
            Err(e) => {
                *sp = sp_snap;
                position = pos_snap;
                self.positions.insert(position_id, position);
                return Err(HarnessError::Clearing(e));
            }
        };

        self.vault_balance = self
            .vault_balance
            .checked_sub(outcome.redeemable)
            .ok_or(HarnessError::Invariant("claim drained vault below zero"))?;
        self.total_withdrawals += outcome.redeemable;

        self.positions.insert(position_id, position);
        self.indexer.apply_all(&outcome.events)?;
        Ok(CloseSummary {
            withdrawable: outcome.redeemable,
            indexer_pre_equity,
        })
    }

    /// Drive a `sync_pool` step.
    pub fn sync(
        &mut self,
        sub_pool_id: u32,
        envelope: PriceEnvelope,
    ) -> Result<(), HarnessError> {
        let sp = self
            .sub_pools
            .get_mut(&sub_pool_id)
            .ok_or(HarnessError::SubPoolNotFound(sub_pool_id))?;

        // Solana tx-revert semantics — see `open` for context.
        let sp_snap = sp.clone();

        let outcome = match sync_pool(&self.market, sp, envelope) {
            Ok(o) => o,
            Err(e) => {
                *sp = sp_snap;
                return Err(HarnessError::Clearing(e));
            }
        };
        self.indexer.apply_all(&outcome.events)?;
        // sync_pool only redistributes funds among pool/dormant/dust slots that
        // already live inside `vault_balance`. The vault balance is unchanged.
        Ok(())
    }

    /// Lazy-mode keeper catch-up: drain pending distribution-ledger
    /// entries onto a single dormant bucket. Mirrors the on-chain
    /// `pre_sync_dormant_bucket` instruction handler.
    ///
    /// ### Vault accounting
    ///
    /// `pre_sync_dormant_bucket` only migrates value from
    /// `pending_distribution_total` into `bucket.accrued_value`; both
    /// already live inside `vault_balance`, so vault scalars are
    /// untouched.
    pub fn pre_sync_bucket(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
        bucket_tick: i64,
        slot: u64,
    ) -> Result<PreSyncOutcome, HarnessError> {
        let sp = self
            .sub_pools
            .get_mut(&sub_pool_id)
            .ok_or(HarnessError::SubPoolNotFound(sub_pool_id))?;
        let sp_snap = sp.clone();
        let outcome = match pre_sync_dormant_bucket(&self.market, sp, direction, bucket_tick, slot)
        {
            Ok(o) => o,
            Err(e) => {
                *sp = sp_snap;
                return Err(HarnessError::Clearing(e));
            }
        };
        self.indexer.apply_all(&outcome.events)?;
        Ok(outcome)
    }

    /// Drain pending lazy-mode entries from EVERY live bucket on
    /// `sub_pool_id`, both directions. Returns the total event count
    /// applied. After this call, the per-bucket `accrued_value`
    /// matches what an eager run would have produced inline.
    ///
    /// Used by the wave 7 keeper-drain equivalence property test
    /// in `crates/protocol-harness/tests/keeper_drain_equivalence.rs`.
    pub fn drain_all_buckets(&mut self, sub_pool_id: u32, slot: u64) -> Result<u64, HarnessError> {
        let ticks: Vec<(Direction, i64)> = {
            let sp = self
                .sub_pools
                .get(&sub_pool_id)
                .ok_or(HarnessError::SubPoolNotFound(sub_pool_id))?;
            let mut out = Vec::new();
            for (t, _) in sp.long_dormant.iter_buckets() {
                out.push((Direction::Long, *t));
            }
            for (t, _) in sp.short_dormant.iter_buckets() {
                out.push((Direction::Short, *t));
            }
            out
        };
        let mut total: u64 = 0;
        for (dir, tick) in ticks {
            // A redeem during a previous step may have removed the
            // bucket; pre_sync_bucket would then return
            // `DormantBucketMissing`. Skip those — they have no
            // pending allocations to drain.
            match self.pre_sync_bucket(sub_pool_id, dir, tick, slot) {
                Ok(o) => total = total.saturating_add(o.events_applied),
                Err(HarnessError::Clearing(ClearingError::DormantBucketMissing)) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(total)
    }

    /// Sweep dust into the protocol fee vault.
    pub fn harvest_dust(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
    ) -> Result<u128, HarnessError> {
        let sp = self
            .sub_pools
            .get_mut(&sub_pool_id)
            .ok_or(HarnessError::SubPoolNotFound(sub_pool_id))?;
        // Solana tx-revert semantics — see `open` for context.
        let sp_snap = sp.clone();
        let outcome = match harvest_dust(&self.market, sp, direction) {
            Ok(o) => o,
            Err(e) => {
                *sp = sp_snap;
                return Err(HarnessError::Clearing(e));
            }
        };
        // Dust moves from vault -> fee_vault.
        self.vault_balance = self
            .vault_balance
            .checked_sub(outcome.amount)
            .ok_or(HarnessError::Invariant("harvest drained vault below zero"))?;
        self.fee_vault_balance += outcome.amount;
        self.fees_collected += outcome.amount;
        self.dust_swept += outcome.amount;
        self.indexer.apply_all(&outcome.events)?;
        Ok(outcome.amount)
    }

    /// Run all harness-level invariants. Cheap; safe to call after every op.
    pub fn check_invariants(&self) -> Result<(), HarnessError> {
        let s = self.summary();

        // 1. Conservation: deposits = withdrawals + state.
        let state = s.vault_balance + s.fee_vault_balance;
        if s.total_deposits != s.total_withdrawals + state {
            return Err(HarnessError::InvariantConservation {
                deposits: s.total_deposits,
                withdrawals: s.total_withdrawals,
                state,
            });
        }

        // 2. Vault decomposition.
        //
        // The four-term identity holds in BOTH eager and lazy mode:
        //
        //   vault == pool_equity + dormant_accrued + dormant_pending + dust
        //
        // In eager mode, `dormant_pending` is always 0; the engine
        // routes funds through `distribute()` which immediately attaches
        // them to per-bucket `accrued_value`. In lazy mode,
        // `distribute_lazy()` parks `allocated_sum` in
        // `pending_distribution_total` until `apply_pending_to_bucket`
        // (or `apply_pending_to_all`) walks the matching ledger entry
        // and migrates per-bucket shares into `accrued_value`.
        let expected = s.pool_equity_total
            + s.dormant_accrued_total
            + s.dormant_pending_total
            + s.dust_total;
        if s.vault_balance != expected {
            return Err(HarnessError::InvariantVault {
                vault: s.vault_balance,
                expected,
                pool_equity: s.pool_equity_total,
                dormant: s.dormant_accrued_total,
                pending: s.dormant_pending_total,
                dust: s.dust_total,
            });
        }

        // 3. Fee vault is the running sum of fees collected and dust swept.
        if s.fee_vault_balance != self.fees_collected {
            return Err(HarnessError::Invariant(
                "fee vault out of sync with fees_collected counter",
            ));
        }
        let _ = self.dust_swept; // tracked separately for diagnostics

        // 3.5 NOTE: per-position chain.pos vs indexer.pos diverge by design.
        // Chain uses LAZY migration: it leaves pos.active_shares stale until
        // the position is touched (close / force_close / claim). The indexer
        // uses EAGER migration via `apply_rotate`, so it updates every
        // matching position immediately. As long as bucket aggregates match
        // and event emission is correct, both arrive at the same withdrawable
        // when the position is finally touched. We therefore do NOT compare
        // per-position fields here — only the bucket aggregates below.

        // 4. Chain dormant buckets must mirror indexer dormant buckets.
        //
        // Strict identities checked here:
        //   indexer.shares    == chain.shares
        //   indexer.notional  == chain.notional
        //   indexer.anchor    == chain.anchor
        //   indexer.accrued   == chain.accrued + chain.pending_for_bucket(tick)
        //
        // The last identity is the wave-5.5 refinement: the indexer
        // accrues per-position recovery profit eagerly on every
        // `PoolSync` (so the front-end shows realized profit
        // immediately), while the chain in lazy mode parks the same
        // per-bucket share in `pending_distribution_total` until
        // `apply_pending_to_bucket` migrates it. The pure-read
        // `DormantStore::pending_for_bucket` mirrors that replay
        // formula byte-for-byte.
        for (sub_pool_id, chain_sp) in &self.sub_pools {
            let inv = self.indexer.dormant_inventory(*sub_pool_id).unwrap_or_default();
            check_dir(
                "long",
                *sub_pool_id,
                &chain_sp.long_dormant,
                clearing_core::Direction::Long,
                &inv,
            )?;
            check_dir(
                "short",
                *sub_pool_id,
                &chain_sp.short_dormant,
                clearing_core::Direction::Short,
                &inv,
            )?;
        }

        Ok(())
    }

    /// Internal helper for the bucket invariant check in
    /// [`Harness::check_invariants`]. Logs the diverging values to stderr
    /// before returning the structured error so test failures show the
    /// concrete diff rather than only the variant.
    #[allow(dead_code)]
    fn _bucket_invariant_marker(&self) {}

    /// Apply a flat list of `EngineEvent`s to the indexer (for testing
    /// reconstruction from a serialized event log; not normally needed
    /// because the harness already feeds events as they're produced).
    pub fn replay_events_into_indexer(
        &mut self,
        events: &[EngineEvent],
    ) -> Result<(), HarnessError> {
        Ok(self.indexer.apply_all(events)?)
    }
}

fn check_dir(
    label: &'static str,
    sub_pool_id: u32,
    chain_store: &clearing_core::DormantStore,
    dir: clearing_core::Direction,
    inv: &[indexer::DormantBucketSnapshot],
) -> Result<(), HarnessError> {
    let chain_buckets: Vec<_> = chain_store.iter_buckets().collect();
    // Symmetric check: also flag indexer-only buckets that chain has
    // already deleted (the original bug surface — indexer kept ghost
    // shares after chain drained the bucket via redeem).
    for x in inv.iter().filter(|s| s.direction == dir) {
        let in_chain = chain_buckets.iter().any(|(t, _)| **t == x.bucket_tick);
        if !in_chain {
            eprintln!(
                "[bucket-diverged] {} sp={} tick={} INDEX(s={} n={} a={} anchor={}) CHAIN=missing/removed",
                label, sub_pool_id, x.bucket_tick,
                x.total_recovery_shares, x.total_recovery_notional, x.accrued_value, x.anchor_price
            );
            return Err(HarnessError::Invariant(
                "indexer has bucket that chain removed",
            ));
        }
    }
    let mut diverged = false;
    for (tick, b) in &chain_buckets {
        let ib = inv.iter().find(|s| s.direction == dir && s.bucket_tick == **tick);
        match ib {
            None => {
                diverged = true;
                eprintln!(
                    "[bucket-diverged] {} sp={} tick={} CHAIN(s={} n={} a={}) INDEX=missing",
                    label, sub_pool_id, tick, b.total_recovery_shares,
                    b.total_recovery_notional, b.accrued_value
                );
            }
            Some(idx) => {
                let pending = chain_store
                    .pending_for_bucket(**tick)
                    .map_err(HarnessError::Clearing)?;
                let chain_eager_equiv = b.accrued_value.saturating_add(pending);
                if idx.total_recovery_shares != b.total_recovery_shares
                    || idx.total_recovery_notional != b.total_recovery_notional
                    || idx.accrued_value != chain_eager_equiv
                    || idx.anchor_price != b.anchor_price
                {
                    diverged = true;
                    eprintln!(
                        "[bucket-diverged] {} sp={} tick={}\n  CHAIN(s={} n={} a={} pending={} eager_equiv={} anchor={})\n  INDEX(s={} n={} a={} anchor={})",
                        label, sub_pool_id, tick,
                        b.total_recovery_shares, b.total_recovery_notional, b.accrued_value, pending, chain_eager_equiv, b.anchor_price,
                        idx.total_recovery_shares, idx.total_recovery_notional, idx.accrued_value, idx.anchor_price,
                    );
                }
            }
        }
    }
    if diverged {
        eprintln!("--- full {} bucket inventory at divergence (sub_pool={}) ---", label, sub_pool_id);
        for (tick, b) in &chain_buckets {
            let pending = chain_store.pending_for_bucket(**tick).unwrap_or(0);
            eprintln!(
                "  CHAIN tick={} anchor={} s={} n={} a={} pending={} eager_equiv={}",
                tick, b.anchor_price, b.total_recovery_shares, b.total_recovery_notional,
                b.accrued_value, pending, b.accrued_value.saturating_add(pending),
            );
        }
        for x in inv.iter().filter(|s| s.direction == dir) {
            eprintln!(
                "  INDEX tick={} anchor={} s={} n={} a={}",
                x.bucket_tick, x.anchor_price, x.total_recovery_shares, x.total_recovery_notional, x.accrued_value
            );
        }
        return Err(HarnessError::Invariant(
            "indexer bucket diverged from chain",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
