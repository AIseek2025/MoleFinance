//! On-chain account-runtime mirror for the Anchor program.
//!
//! The Solana program in `programs/mole-option/` stores its mutable
//! state as a collection of independent accounts: one `Market`, one
//! `GlobalConfig`, N `SubPool` accounts (one per shard), N×K
//! `DormantBucket` accounts (per-direction, per-tick PDAs), 2 N
//! `DistributionLedger` accounts (per-direction), and one `Position`
//! account per user position. Instructions read a *subset* of these
//! accounts via Anchor's account-list and `remaining_accounts`
//! mechanisms, run the host-tested clearing engine on the unpacked
//! state, and write the results back to the same accounts atomically.
//!
//! `chain-mirror` reproduces this *account-level* runtime on the host.
//! It is **not** a Solana BPF emulator — it does not interpret
//! sBPF bytecode, nor does it round-trip through Borsh wire format.
//! What it *does* faithfully model is:
//!
//! * Each on-chain account as an owned host-side struct.
//! * `remaining_accounts`: every potentially-touched account is
//!   resolved and locked at instruction entry.
//! * Account materialisation: rotation creates a new
//!   [`DormantBucketAccount`]; we treat this as Anchor `init_if_needed`
//!   or "the user pre-initialised the PDA" — either way the mirror
//!   handler ensures the account exists at the moment the engine
//!   needs to write it.
//! * **Atomic transaction semantics**: every public entry point
//!   snapshots all touched accounts before calling the engine and
//!   restores them on `Err`. Mirrors Solana's
//!   tx-commit-or-rollback runtime exactly (and the host
//!   [`protocol_harness`] equivalent landed in wave 4).
//!
//! ### Why this exists
//!
//! Before wave 5, the actual Solana program in
//! `programs/mole-option/src/instructions/sync.rs` had a
//! production-blocking gap: `clearing_view(sp_acc)` constructs an
//! empty `DormantStore` on every call and discards any rotation
//! result on write-back. The host engine and harness were 100 %
//! tested but **the on-chain instruction wiring of the dormant
//! lifecycle was a no-op**. `chain-mirror` closes that gap on the
//! host: instructions here use the existing
//! [`clearing_core::pack_dormant_store`] /
//! [`clearing_core::unpack_dormant_store`] bridge to read and write
//! the per-bucket / per-ledger accounts. The same
//! `protocol_harness::Harness` workloads run against this runtime
//! as a parity oracle, proving the bridged path is byte-equal to
//! the in-memory reference.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::{BTreeMap, HashMap};

use clearing_core::{
    claim_dormant_recovery, close_position, force_close_zero_value_position, harvest_dust,
    open_position, pack_dormant_store, pre_sync_dormant_bucket, sync_pool, unpack_dormant_store,
    ClaimRecoveryOutcome, ClearingError, CloseOutcome, Direction, DormantStore, EngineEvent,
    ForceCloseOutcome, HarvestOutcome, MarketParams, OnChainBucketRecord, OnChainLedger, Position,
    PreSyncOutcome, PriceEnvelope, SubPool as CoreSubPool, SyncOutcome,
};

/// Errors raised by the chain mirror runtime.
#[derive(Debug, thiserror::Error)]
pub enum MirrorError {
    /// Underlying engine error.
    #[error("clearing engine error: {0:?}")]
    Clearing(ClearingError),
    /// Sub pool account not found in the runtime.
    #[error("sub pool not found: {0}")]
    SubPoolNotFound(u32),
    /// Position account not found in the runtime.
    #[error("position not found: {0}")]
    PositionNotFound(u64),
    /// Caller asked for a bucket account that has not been
    /// initialized. On chain this would be a missing PDA in
    /// `remaining_accounts`.
    #[error("dormant bucket account not initialized: sub_pool={sub_pool} dir={direction:?} tick={tick}")]
    BucketNotInitialized {
        /// Sub pool id.
        sub_pool: u32,
        /// Direction.
        direction: Direction,
        /// Bucket tick.
        tick: i64,
    },
    /// **Wave 8.** Strict-PDA-lifecycle mode is enabled and the engine
    /// produced a new dormant bucket but no pre-init'd dead PDA slot
    /// was available. Mirrors `dormant_bridge::pack_direction`'s
    /// `DormantBridgeBucketSlotExhausted` failure path. The on-chain
    /// keeper recovery is to call `init_dormant_bucket(tick)` and
    /// retry the engine instruction; in chain-mirror, callers
    /// reproduce the same flow by calling
    /// [`ChainRuntime::pre_init_dormant_bucket`] before retrying.
    #[error("dormant bridge: bucket slot exhausted (sub_pool={sub_pool} dir={direction:?})")]
    BucketSlotExhausted {
        /// Sub pool id.
        sub_pool: u32,
        /// Direction.
        direction: Direction,
    },
    /// **Wave 8.** Caller tried to pre-init or close a bucket whose
    /// PDA already exists in some state. Pre-init: same `(sub_pool,
    /// direction, tick)` already has a record (live OR dead). Close:
    /// the bucket still has live shares / accrued / position_count —
    /// matches Anchor's `DormantBucketStillLive` guard.
    #[error("dormant bucket pda lifecycle violation: {reason} (sub_pool={sub_pool} dir={direction:?} tick={tick})")]
    BucketLifecycleViolation {
        /// Sub pool id.
        sub_pool: u32,
        /// Direction.
        direction: Direction,
        /// Bucket tick.
        tick: i64,
        /// Human-readable reason.
        reason: &'static str,
    },
    /// A sanity check on the runtime invariants failed.
    #[error("runtime invariant violated: {0}")]
    Invariant(&'static str),
}

impl From<ClearingError> for MirrorError {
    fn from(value: ClearingError) -> Self {
        Self::Clearing(value)
    }
}

/// Anchor-side `Market` account, projected to the fields the engine
/// actually reads. Stored in the runtime as a single owned struct.
#[derive(Debug, Clone)]
pub struct MarketAccount {
    /// Static engine parameters.
    pub params: MarketParams,
}

/// Anchor-side `SubPool` account fields not already covered by
/// per-direction dormant bucket / ledger accounts.
#[derive(Debug, Clone)]
pub struct SubPoolAccount {
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// Long-side directional pool equity.
    pub long_pool_equity: u128,
    /// Short-side directional pool equity.
    pub short_pool_equity: u128,
    /// Long-side active shares.
    pub long_active_shares: u128,
    /// Short-side active shares.
    pub short_active_shares: u128,
    /// Long-side recovery shares.
    pub long_recovery_shares: u128,
    /// Short-side recovery shares.
    pub short_recovery_shares: u128,
    /// Long-side active notional.
    pub long_active_notional: u128,
    /// Short-side active notional.
    pub short_active_notional: u128,
    /// Long-side dust accumulator.
    pub long_dust: u128,
    /// Short-side dust accumulator.
    pub short_dust: u128,
    /// Long-side dormant bucket count cap accounting.
    pub long_dormant_bucket_count: u32,
    /// Short-side dormant bucket count cap accounting.
    pub short_dormant_bucket_count: u32,
    /// Long-side active generation.
    pub long_active_generation: u64,
    /// Short-side active generation.
    pub short_active_generation: u64,
    /// Long-side rotate log (PDA-stored ring buffer on chain).
    pub long_rotate_log: Vec<RotateRecordAccount>,
    /// Short-side rotate log.
    pub short_rotate_log: Vec<RotateRecordAccount>,
    /// Last synced oracle price.
    pub last_price: u64,
    /// Slot at which `last_price` was set.
    pub last_sync_slot: u64,
}

/// Anchor-side rotate-log record. Mirrors
/// `programs/mole-option::state::RotateRecordPacked` 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotateRecordAccount {
    /// Generation that just ended.
    pub generation_just_ended: u64,
    /// Tick of the bucket created or merged into during the rotate.
    pub bucket_tick: i64,
    /// Anchor price at the rotate.
    pub anchor_price: u64,
}

impl SubPoolAccount {
    /// Fresh account at construction time.
    pub fn new(sub_pool_id: u32, init_price: u64, init_slot: u64) -> Self {
        Self {
            sub_pool_id,
            long_pool_equity: 0,
            short_pool_equity: 0,
            long_active_shares: 0,
            short_active_shares: 0,
            long_recovery_shares: 0,
            short_recovery_shares: 0,
            long_active_notional: 0,
            short_active_notional: 0,
            long_dust: 0,
            short_dust: 0,
            long_dormant_bucket_count: 0,
            short_dormant_bucket_count: 0,
            long_active_generation: 0,
            short_active_generation: 0,
            long_rotate_log: Vec::new(),
            short_rotate_log: Vec::new(),
            last_price: init_price,
            last_sync_slot: init_slot,
        }
    }
}

/// Anchor-side `DormantBucket` account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DormantBucketAccount {
    /// Sub pool id this bucket belongs to.
    pub sub_pool_id: u32,
    /// POD bucket record (96 bytes on chain). The runtime keeps it as
    /// the source of truth and unpacks/repacks via
    /// [`clearing_core::pack_dormant_store`].
    pub record: OnChainBucketRecord,
}

/// Anchor-side `DistributionLedger` account (one per
/// `(sub_pool_id, direction)`).
#[derive(Debug, Clone)]
pub struct DistributionLedgerAccount {
    /// Sub pool id.
    pub sub_pool_id: u32,
    /// POD ledger header + entries. Direction is encoded inside
    /// [`OnChainLedger::direction`].
    pub ledger: OnChainLedger,
}

/// Anchor-side `Position` account.
#[derive(Debug, Clone)]
pub struct PositionAccount {
    /// Sub pool this position lives in.
    pub sub_pool_id: u32,
    /// The full engine-side position state.
    pub position: Position,
}

/// On-chain runtime: one [`MarketAccount`], a map of
/// [`SubPoolAccount`] / [`DormantBucketAccount`] /
/// [`DistributionLedgerAccount`] / [`PositionAccount`] entries, and
/// vault book-keeping for collateral conservation.
#[derive(Debug, Clone)]
pub struct ChainRuntime {
    /// The single market account.
    pub market: MarketAccount,
    /// Per-shard sub pool accounts.
    pub sub_pools: HashMap<u32, SubPoolAccount>,
    /// Per (sub_pool_id, direction, tick) dormant bucket account.
    pub buckets: HashMap<(u32, Direction, i64), DormantBucketAccount>,
    /// Per (sub_pool_id, direction) ledger account.
    pub ledgers: HashMap<(u32, Direction), DistributionLedgerAccount>,
    /// Per position_id position account.
    pub positions: HashMap<u64, PositionAccount>,
    /// SPL collateral vault balance (as if a token account on chain).
    pub vault_balance: u128,
    /// SPL fee vault balance (open_fee + harvested dust).
    pub fee_vault_balance: u128,
    /// Total deposits seen across all opens.
    pub total_deposits: u128,
    /// Total withdrawals seen across closes / claims / harvests.
    pub total_withdrawals: u128,
    /// Counter for harness-assigned position ids.
    pub next_position_id: u64,
    /// **Wave 8.** When `false` (default, retains wave-5 behaviour),
    /// `write_core_sub_pool` auto-grows `buckets` for any new tick the
    /// engine produces — convenient for parity tests that don't care
    /// about PDA materialisation. When `true`, the runtime emulates
    /// Anchor's strict PDA lifecycle: every new tick MUST first be
    /// pre-init'd via [`ChainRuntime::pre_init_dormant_bucket`]
    /// (matching the on-chain `init_dormant_bucket` instruction);
    /// `pack_direction`'s Pass-1/2/3 model places engine records into
    /// pre-init'd dead slots, zeroes engine-removed buckets in place,
    /// and refuses to grow beyond the slot budget — returning
    /// [`MirrorError::BucketSlotExhausted`] verbatim. This is the
    /// host-side oracle for the Wave 7.2 `record_is_dead` bridge fix.
    pub strict_pda_lifecycle: bool,
}

impl ChainRuntime {
    /// Construct a fresh runtime with the given market parameters.
    /// Strict PDA lifecycle is **off** by default for backward
    /// compatibility with wave-5 parity tests; enable it via
    /// [`ChainRuntime::with_strict_pda_lifecycle`].
    pub fn new(params: MarketParams) -> Self {
        Self {
            market: MarketAccount { params },
            sub_pools: HashMap::new(),
            buckets: HashMap::new(),
            ledgers: HashMap::new(),
            positions: HashMap::new(),
            vault_balance: 0,
            fee_vault_balance: 0,
            total_deposits: 0,
            total_withdrawals: 0,
            next_position_id: 1,
            strict_pda_lifecycle: false,
        }
    }

    /// Builder-style toggle for [`ChainRuntime::strict_pda_lifecycle`].
    /// See the field docs for semantics.
    #[must_use]
    pub fn with_strict_pda_lifecycle(mut self, strict: bool) -> Self {
        self.strict_pda_lifecycle = strict;
        self
    }

    /// **Wave 8.** Mirror of the on-chain `init_dormant_bucket`
    /// instruction. Inserts a fully-dead [`OnChainBucketRecord`] PDA
    /// at `(sub_pool_id, direction, tick)`. The engine will treat
    /// such a slot as empty (matching `dormant_bridge::record_is_dead`
    /// semantics) and `pack_direction` Pass 2 will fill it on the
    /// next instruction that needs a new bucket.
    ///
    /// Errors:
    /// - [`MirrorError::SubPoolNotFound`] if the sub pool does not exist.
    /// - [`MirrorError::BucketLifecycleViolation`] if a PDA already
    ///   exists at the target key (live or dead) — Anchor's
    ///   `init_if_needed` would either no-op or fail; chain-mirror
    ///   surfaces it as an explicit error so tests can't hide it.
    pub fn pre_init_dormant_bucket(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
        tick: i64,
    ) -> Result<(), MirrorError> {
        if !self.sub_pools.contains_key(&sub_pool_id) {
            return Err(MirrorError::SubPoolNotFound(sub_pool_id));
        }
        if self.buckets.contains_key(&(sub_pool_id, direction, tick)) {
            return Err(MirrorError::BucketLifecycleViolation {
                sub_pool: sub_pool_id,
                direction,
                tick,
                reason: "pda already initialised",
            });
        }
        self.buckets.insert(
            (sub_pool_id, direction, tick),
            DormantBucketAccount {
                sub_pool_id,
                record: OnChainBucketRecord::dead(direction, tick),
            },
        );
        Ok(())
    }

    /// **Wave 8.** Mirror of the on-chain `close_dormant_bucket`
    /// instruction. Refuses to close a bucket that:
    /// - does not exist (`BucketNotInitialized`),
    /// - is still logically live (any of `total_recovery_shares`,
    ///   `total_recovery_notional`, `accrued_value`, `position_count`
    ///   non-zero — same `record_is_dead` predicate the bridge uses),
    /// - has not caught up with its ledger (`last_applied_index <
    ///   ledger.next_event_index`, matching Anchor's
    ///   `DormantBucketHasPendingApply`).
    pub fn close_dormant_bucket(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
        tick: i64,
    ) -> Result<(), MirrorError> {
        let bucket = self.buckets.get(&(sub_pool_id, direction, tick)).ok_or(
            MirrorError::BucketNotInitialized {
                sub_pool: sub_pool_id,
                direction,
                tick,
            },
        )?;
        if !bucket.record.is_dead() {
            return Err(MirrorError::BucketLifecycleViolation {
                sub_pool: sub_pool_id,
                direction,
                tick,
                reason: "bucket still live",
            });
        }
        let ledger = self.ledgers.get(&(sub_pool_id, direction)).ok_or(
            MirrorError::Invariant("ledger missing on close_dormant_bucket"),
        )?;
        if bucket.record.last_applied_index < ledger.ledger.next_event_index {
            return Err(MirrorError::BucketLifecycleViolation {
                sub_pool: sub_pool_id,
                direction,
                tick,
                reason: "bucket has pending lazy distributions",
            });
        }
        self.buckets.remove(&(sub_pool_id, direction, tick));
        Ok(())
    }

    // ----------------------------------------------------------------
    // Wave 9 — governance setters.
    //
    // These mirror the on-chain `pause_market` / `resume_market` /
    // `freeze_new_position` / `unfreeze_new_position` /
    // `bump_market_schema_version` admin instructions. chain-mirror
    // does NOT model Anchor's `Signer<'info>` + `address = ...`
    // gating; access control is the program crate's responsibility
    // (verified statically by `cargo check -p mole-option`). What
    // this layer DOES model is the engine-side response to the flag
    // flip: every funds-touching instruction must immediately bounce
    // off the new state on the *next* call. Tests exercise that
    // contract.
    // ----------------------------------------------------------------

    /// Wave 9 — flip the `paused` flag on the underlying
    /// `MarketParams`. Subsequent `sync_pool` / `open_position` /
    /// `close_position` / `force_close` / `claim_recovery` /
    /// `pre_sync_bucket` / `harvest_dust` calls must return
    /// `Clearing(MarketPaused)` until cleared.
    pub fn governance_set_paused(&mut self, paused: bool) {
        self.market.params.paused = paused;
    }

    /// Wave 9 — flip the `frozen_new_position` flag. Only
    /// `open_position` is gated by this; close/claim/sync remain
    /// available so existing positions can wind down.
    pub fn governance_set_frozen_new_position(&mut self, frozen: bool) {
        self.market.params.frozen_new_position = frozen;
    }

    /// Wave 9 — bump the market's `schema_version` by exactly one,
    /// rejecting any non-monotonic change. Mirrors the on-chain
    /// `bump_market_schema_version` admin handler in
    /// `programs/mole-option/src/instructions/admin.rs`.
    ///
    /// Errors:
    /// - [`MirrorError::Invariant`] with reason `"schema bump must
    ///   strictly increase"` for `new_version <= current`.
    ///
    /// After a successful bump, every position whose
    /// `schema_version` is still at the old value will fail any
    /// subsequent `close_position` / `force_close` /
    /// `claim_recovery` with `Clearing(SchemaVersionMismatch)`. To
    /// rescue them, callers must walk each position forward via
    /// [`ChainRuntime::governance_migrate_position`].
    pub fn governance_bump_schema_version(
        &mut self,
        new_version: u16,
    ) -> Result<(), MirrorError> {
        if new_version <= self.market.params.schema_version {
            return Err(MirrorError::Invariant("schema bump must strictly increase"));
        }
        self.market.params.schema_version = new_version;
        Ok(())
    }

    /// Wave 9 — migrate a single `Position`'s `schema_version` to
    /// match `MarketParams::schema_version`. Mirrors the permission-
    /// less `migrate_position` instruction. v1 has no concrete
    /// migration steps registered, so positions already at the
    /// target version return `MirrorError::Invariant("schema
    /// migration noop")`; positions strictly behind are walked
    /// forward one step at a time.
    pub fn governance_migrate_position(
        &mut self,
        position_id: u64,
    ) -> Result<(), MirrorError> {
        let pos = self
            .positions
            .get_mut(&position_id)
            .ok_or(MirrorError::PositionNotFound(position_id))?;
        let target = self.market.params.schema_version;
        if pos.position.schema_version == target {
            return Err(MirrorError::Invariant("schema migration noop"));
        }
        // No registered v1+ migration paths in Wave 9. Future bumps
        // will land step-functions here (matching the program-side
        // `SchemaMigrationStep` registry). Until then we only allow
        // monotonic-up walks (which currently never fire).
        while pos.position.schema_version < target {
            pos.position.schema_version = pos
                .position
                .schema_version
                .checked_add(1)
                .ok_or(MirrorError::Invariant("schema migration overflow"))?;
        }
        Ok(())
    }

    /// Initialise a sub pool. Mirrors `init_sub_pool` instruction.
    /// Allocates the matching ledger PDAs (one per direction).
    pub fn add_sub_pool(&mut self, sub_pool_id: u32, init_price: u64, init_slot: u64) {
        let max_ledger = self.market.params.max_distribution_ledger_size;
        self.sub_pools
            .insert(sub_pool_id, SubPoolAccount::new(sub_pool_id, init_price, init_slot));
        self.ledgers.insert(
            (sub_pool_id, Direction::Long),
            DistributionLedgerAccount {
                sub_pool_id,
                ledger: OnChainLedger::empty(Direction::Long, max_ledger),
            },
        );
        self.ledgers.insert(
            (sub_pool_id, Direction::Short),
            DistributionLedgerAccount {
                sub_pool_id,
                ledger: OnChainLedger::empty(Direction::Short, max_ledger),
            },
        );
    }

    /// Get a sub pool snapshot.
    pub fn sub_pool(&self, sub_pool_id: u32) -> Option<&SubPoolAccount> {
        self.sub_pools.get(&sub_pool_id)
    }

    /// Get a position snapshot.
    pub fn position(&self, position_id: u64) -> Option<&Position> {
        self.positions.get(&position_id).map(|a| &a.position)
    }

    /// Number of dormant bucket accounts currently materialised in the
    /// given direction.
    pub fn bucket_count(&self, sub_pool_id: u32, direction: Direction) -> usize {
        self.buckets
            .keys()
            .filter(|(sp, dir, _)| *sp == sub_pool_id && *dir == direction)
            .count()
    }

    /// Sweep the runtime's "vault" decomposition. Required identity at
    /// every step in BOTH eager and lazy mode:
    ///
    /// ```text
    /// vault_balance == pool_equity + Σ bucket.accrued_value
    ///                + Σ ledger.pending_distribution_total
    ///                + dust
    /// ```
    ///
    /// `pending_distribution_total` (one per `(sub_pool, direction)`
    /// `DistributionLedger` PDA) is the lazy-mode in-flight allocation
    /// amount: funds that left `pool_equity` via `distribute_lazy` but
    /// have not yet been migrated into a bucket's `accrued_value` by
    /// `apply_pending_to_bucket`. In eager mode this term is always 0.
    ///
    /// Mirrors `protocol_harness::Harness::check_invariants` (wave 5.5).
    pub fn check_vault_decomposition(&self) -> Result<(), MirrorError> {
        let mut pool_equity: u128 = 0;
        let mut dust: u128 = 0;
        for sp in self.sub_pools.values() {
            pool_equity += sp.long_pool_equity + sp.short_pool_equity;
            dust += sp.long_dust + sp.short_dust;
        }
        let mut accrued: u128 = 0;
        for b in self.buckets.values() {
            accrued += b.record.accrued_value;
        }
        let mut pending: u128 = 0;
        for ledger in self.ledgers.values() {
            pending += ledger.ledger.pending_distribution_total;
        }
        let expected = pool_equity + accrued + pending + dust;
        if self.vault_balance != expected {
            return Err(MirrorError::Invariant(
                "vault_balance != pool_equity + dormant_accrued + dormant_pending + dust",
            ));
        }
        Ok(())
    }

    // --- Bridge helpers ---------------------------------------------

    /// Build a `clearing_core::SubPool` view by unpacking the sub pool
    /// account fields plus all matching dormant bucket + ledger
    /// accounts. Mirrors what an Anchor `instruction handler` would
    /// do at the start of every entrypoint.
    fn build_core_sub_pool(&self, sub_pool_id: u32) -> Result<CoreSubPool, MirrorError> {
        let sp = self
            .sub_pools
            .get(&sub_pool_id)
            .ok_or(MirrorError::SubPoolNotFound(sub_pool_id))?;
        let mut core =
            CoreSubPool::new(sp.sub_pool_id, sp.last_price, sp.last_sync_slot);
        core.long_pool_equity = sp.long_pool_equity;
        core.short_pool_equity = sp.short_pool_equity;
        core.long_active_shares = sp.long_active_shares;
        core.short_active_shares = sp.short_active_shares;
        core.long_recovery_shares = sp.long_recovery_shares;
        core.short_recovery_shares = sp.short_recovery_shares;
        core.long_active_notional = sp.long_active_notional;
        core.short_active_notional = sp.short_active_notional;
        core.long_dust = sp.long_dust;
        core.short_dust = sp.short_dust;
        core.long_dormant_bucket_count = sp.long_dormant_bucket_count;
        core.short_dormant_bucket_count = sp.short_dormant_bucket_count;
        core.long_active_generation = sp.long_active_generation;
        core.short_active_generation = sp.short_active_generation;
        core.long_rotate_log = sp
            .long_rotate_log
            .iter()
            .map(|r| clearing_core::RotateRecord {
                generation_just_ended: r.generation_just_ended,
                bucket_tick: r.bucket_tick,
                anchor_price: r.anchor_price,
            })
            .collect();
        core.short_rotate_log = sp
            .short_rotate_log
            .iter()
            .map(|r| clearing_core::RotateRecord {
                generation_just_ended: r.generation_just_ended,
                bucket_tick: r.bucket_tick,
                anchor_price: r.anchor_price,
            })
            .collect();
        // Bridge dormant store: pull every matching bucket + ledger.
        core.long_dormant = self.unpack_dormant(sub_pool_id, Direction::Long)?;
        core.short_dormant = self.unpack_dormant(sub_pool_id, Direction::Short)?;
        Ok(core)
    }

    fn unpack_dormant(
        &self,
        sub_pool_id: u32,
        direction: Direction,
    ) -> Result<DormantStore, MirrorError> {
        let ledger = self
            .ledgers
            .get(&(sub_pool_id, direction))
            .ok_or(MirrorError::Invariant(
                "ledger account missing — must init sub_pool first",
            ))?;
        // Buckets are sorted ascending by tick in the on-chain layout.
        let mut bucket_records: BTreeMap<i64, OnChainBucketRecord> = BTreeMap::new();
        for ((sp, dir, tick), acc) in &self.buckets {
            if *sp == sub_pool_id && *dir == direction {
                bucket_records.insert(*tick, acc.record);
            }
        }
        let bucket_vec: Vec<OnChainBucketRecord> = bucket_records.into_values().collect();
        Ok(unpack_dormant_store(&bucket_vec, &ledger.ledger)?)
    }

    /// Persist a `clearing_core::SubPool` (post-engine-call) back into
    /// the on-chain account storage. Mirrors the write-back half of
    /// every Anchor instruction handler.
    ///
    /// Two modes (controlled by [`ChainRuntime::strict_pda_lifecycle`]):
    ///
    /// - **Loose** (`false`, default, wave-5 behaviour): freely
    ///   materialise new bucket accounts and drop disappeared ticks.
    ///   Used by the original parity tests where the focus is engine
    ///   semantics, not PDA materialisation.
    /// - **Strict** (`true`, wave-8): mirror the Anchor
    ///   `dormant_bridge::pack_direction` Pass 1/2/3 model.
    ///     - **Pass 1** — for each engine record, find the existing
    ///       PDA at key `(sub_pool, direction, record.tick)` that is
    ///       NOT dead and overwrite it.
    ///     - **Pass 2** — for any record still unplaced, find the
    ///       PDA at key `(sub_pool, direction, record.tick)` that IS
    ///       dead (pre-init'd by `pre_init_dormant_bucket`) and write
    ///       it. If no such PDA exists, fail with
    ///       [`MirrorError::BucketSlotExhausted`] — exactly the same
    ///       failure mode the on-chain bridge surfaces.
    ///     - **Pass 3** — every leftover live PDA (whose tick is no
    ///       longer in the engine's record set) is zeroed *in place*,
    ///       preserving the PDA address for keeper close-out.
    fn write_core_sub_pool(
        &mut self,
        sub_pool_id: u32,
        core: &CoreSubPool,
    ) -> Result<(), MirrorError> {
        let max_buckets = self
            .market
            .params
            .max_dormant_bucket_count_per_direction;
        let max_ledger = self.market.params.max_distribution_ledger_size;
        for direction in [Direction::Long, Direction::Short] {
            let store = match direction {
                Direction::Long => &core.long_dormant,
                Direction::Short => &core.short_dormant,
            };
            let (records, ledger) = pack_dormant_store(store, max_buckets, max_ledger)?;
            let ledger_acc = self.ledgers.get_mut(&(sub_pool_id, direction)).ok_or(
                MirrorError::Invariant("ledger account missing on write-back"),
            )?;
            ledger_acc.ledger = ledger;
            if self.strict_pda_lifecycle {
                self.write_buckets_strict(sub_pool_id, direction, &records)?;
            } else {
                self.write_buckets_loose(sub_pool_id, direction, records);
            }
        }
        // Now write back the SubPoolAccount scalar fields.
        let sp = self
            .sub_pools
            .get_mut(&sub_pool_id)
            .ok_or(MirrorError::SubPoolNotFound(sub_pool_id))?;
        sp.long_pool_equity = core.long_pool_equity;
        sp.short_pool_equity = core.short_pool_equity;
        sp.long_active_shares = core.long_active_shares;
        sp.short_active_shares = core.short_active_shares;
        sp.long_recovery_shares = core.long_recovery_shares;
        sp.short_recovery_shares = core.short_recovery_shares;
        sp.long_active_notional = core.long_active_notional;
        sp.short_active_notional = core.short_active_notional;
        sp.long_dust = core.long_dust;
        sp.short_dust = core.short_dust;
        sp.long_dormant_bucket_count = core.long_dormant_bucket_count;
        sp.short_dormant_bucket_count = core.short_dormant_bucket_count;
        sp.long_active_generation = core.long_active_generation;
        sp.short_active_generation = core.short_active_generation;
        sp.long_rotate_log = core
            .long_rotate_log
            .iter()
            .map(|r| RotateRecordAccount {
                generation_just_ended: r.generation_just_ended,
                bucket_tick: r.bucket_tick,
                anchor_price: r.anchor_price,
            })
            .collect();
        sp.short_rotate_log = core
            .short_rotate_log
            .iter()
            .map(|r| RotateRecordAccount {
                generation_just_ended: r.generation_just_ended,
                bucket_tick: r.bucket_tick,
                anchor_price: r.anchor_price,
            })
            .collect();
        sp.last_price = core.last_price;
        sp.last_sync_slot = core.last_sync_slot;
        Ok(())
    }

    /// **Loose-mode** bucket reconciliation (wave-5): drop stale
    /// ticks, materialise/refresh ticks from the engine record list.
    /// Convenient when the test cares only about engine semantics.
    fn write_buckets_loose(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
        records: Vec<OnChainBucketRecord>,
    ) {
        let live_ticks: std::collections::HashSet<i64> =
            records.iter().map(|r| r.zero_price_tick).collect();
        self.buckets.retain(|(sp, dir, tick), _| {
            !(*sp == sub_pool_id && *dir == direction && !live_ticks.contains(tick))
        });
        for r in records {
            self.buckets.insert(
                (sub_pool_id, direction, r.zero_price_tick),
                DormantBucketAccount {
                    sub_pool_id,
                    record: r,
                },
            );
        }
    }

    /// **Strict-mode** bucket reconciliation (wave-8): mirror the
    /// Anchor `dormant_bridge::pack_direction` Pass 1/2/3 model. PDAs
    /// are keyed by `(sub_pool, direction, tick)` exactly as on chain,
    /// so the bridge's "match by tick first, then claim a dead slot
    /// at the same tick, then zero leftovers in place" rule lowers
    /// directly to a per-tick lookup here. Returns
    /// [`MirrorError::BucketSlotExhausted`] if the engine produced a
    /// record at a tick whose PDA was never pre-init'd.
    fn write_buckets_strict(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
        records: &[OnChainBucketRecord],
    ) -> Result<(), MirrorError> {
        // Track every PDA we touched so Pass 3 can identify leftovers.
        let mut placed: std::collections::HashSet<i64> =
            std::collections::HashSet::with_capacity(records.len());
        // Pass 1+2 fused: every record's PDA must already exist at
        // (sub_pool, direction, record.tick), live OR dead. Any PDA
        // for that tick is a write target — Pass 1 (live → refresh)
        // and Pass 2 (dead → fill) collapse into the same code path
        // because chain-mirror's `buckets` map is keyed by tick.
        for record in records {
            let key = (sub_pool_id, direction, record.zero_price_tick);
            match self.buckets.get_mut(&key) {
                Some(slot) => {
                    slot.record = *record;
                    placed.insert(record.zero_price_tick);
                }
                None => {
                    return Err(MirrorError::BucketSlotExhausted {
                        sub_pool: sub_pool_id,
                        direction,
                    });
                }
            }
        }
        // Pass 3: every PDA whose tick is not in the record set is a
        // leftover live bucket the engine just removed. Zero its
        // engine-observable fields in place; preserve identity (the
        // PDA address) so a future keeper-init'd allocation, or a
        // future engine record at the same tick, can re-use the
        // slot.
        let leftover_keys: Vec<(u32, Direction, i64)> = self
            .buckets
            .keys()
            .filter(|(sp, dir, tick)| {
                *sp == sub_pool_id && *dir == direction && !placed.contains(tick)
            })
            .copied()
            .collect();
        for key in leftover_keys {
            if let Some(slot) = self.buckets.get_mut(&key) {
                if !slot.record.is_dead() {
                    slot.record.total_recovery_shares = 0;
                    slot.record.total_recovery_notional = 0;
                    slot.record.accrued_value = 0;
                    slot.record.position_count = 0;
                    // anchor_price is intentionally preserved: the
                    // bridge does the same so that a same-tick
                    // re-allocation can sanity-check it. Engine
                    // semantics are unaffected because
                    // `OnChainBucketRecord::is_dead` is anchor_price
                    // independent.
                }
            }
        }
        Ok(())
    }

    /// Snapshot every account that an instruction may touch on the
    /// given sub pool, so we can roll back atomically on Err. Mirrors
    /// Solana's tx-revert semantics (parent of `protocol-harness`'s
    /// wave-4 fix).
    fn snapshot_sub_pool(&self, sub_pool_id: u32) -> SubPoolSnapshot {
        let sp = self.sub_pools.get(&sub_pool_id).cloned();
        let mut buckets = HashMap::new();
        for (k, v) in &self.buckets {
            if k.0 == sub_pool_id {
                buckets.insert(*k, *v);
            }
        }
        let ledgers = HashMap::from([
            (
                (sub_pool_id, Direction::Long),
                self.ledgers
                    .get(&(sub_pool_id, Direction::Long))
                    .cloned()
                    .unwrap(),
            ),
            (
                (sub_pool_id, Direction::Short),
                self.ledgers
                    .get(&(sub_pool_id, Direction::Short))
                    .cloned()
                    .unwrap(),
            ),
        ]);
        SubPoolSnapshot {
            sub_pool: sp.unwrap(),
            buckets,
            ledgers,
        }
    }

    fn restore_sub_pool(&mut self, snap: SubPoolSnapshot) {
        let sp_id = snap.sub_pool.sub_pool_id;
        // Drop every bucket on this sub pool, then restore from snap.
        self.buckets.retain(|(sp, _, _), _| *sp != sp_id);
        for (k, v) in snap.buckets {
            self.buckets.insert(k, v);
        }
        for (k, v) in snap.ledgers {
            self.ledgers.insert(k, v);
        }
        self.sub_pools.insert(sp_id, snap.sub_pool);
    }

    // --- Instruction handlers ---------------------------------------

    /// `sync_pool` instruction.
    pub fn sync(
        &mut self,
        sub_pool_id: u32,
        envelope: PriceEnvelope,
    ) -> Result<SyncOutcome, MirrorError> {
        let snap = self.snapshot_sub_pool(sub_pool_id);
        let mut core = self.build_core_sub_pool(sub_pool_id)?;
        match sync_pool(&self.market.params, &mut core, envelope) {
            Ok(outcome) => {
                self.write_core_sub_pool(sub_pool_id, &core)?;
                Ok(outcome)
            }
            Err(e) => {
                self.restore_sub_pool(snap);
                Err(e.into())
            }
        }
    }

    /// `open_position` instruction. Allocates a fresh position id.
    pub fn open(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
        gross_amount: u64,
        envelope: PriceEnvelope,
    ) -> Result<OpenSummary, MirrorError> {
        let snap = self.snapshot_sub_pool(sub_pool_id);
        let position_id = self.next_position_id;
        self.next_position_id += 1;

        let mut core = self.build_core_sub_pool(sub_pool_id)?;
        match open_position(
            &self.market.params,
            &mut core,
            envelope,
            direction,
            gross_amount,
            position_id,
        ) {
            Ok((mut position, outcome)) => {
                if let Err(e) = self.write_core_sub_pool(sub_pool_id, &core) {
                    self.restore_sub_pool(snap);
                    self.next_position_id -= 1;
                    return Err(e);
                }
                position.sub_pool_id = sub_pool_id;
                self.positions.insert(
                    position_id,
                    PositionAccount {
                        sub_pool_id,
                        position,
                    },
                );
                // Vault book-keeping: principal_into_pool + dust → vault;
                // open_fee → fee_vault.
                let principal_part = (outcome.principal_into_pool)
                    .checked_add(outcome.dust)
                    .ok_or(MirrorError::Invariant("open: principal+dust overflow"))?;
                let fee_part = outcome.open_fee as u128;
                self.vault_balance += principal_part;
                self.fee_vault_balance += fee_part;
                self.total_deposits += gross_amount as u128;
                Ok(OpenSummary {
                    position_id,
                    shares_minted: outcome.shares_minted,
                    open_fee: outcome.open_fee,
                    events: outcome.events,
                })
            }
            Err(e) => {
                self.restore_sub_pool(snap);
                self.next_position_id -= 1;
                Err(e.into())
            }
        }
    }

    /// `close_position` instruction.
    pub fn close(
        &mut self,
        position_id: u64,
        envelope: PriceEnvelope,
    ) -> Result<CloseSummary, MirrorError> {
        let pos_acc = self
            .positions
            .get(&position_id)
            .ok_or(MirrorError::PositionNotFound(position_id))?;
        let sub_pool_id = pos_acc.sub_pool_id;
        let snap = self.snapshot_sub_pool(sub_pool_id);
        let pos_snap = pos_acc.clone();

        let mut core = self.build_core_sub_pool(sub_pool_id)?;
        // Take the position out of the map for &mut access, restore on Err.
        let mut position = self
            .positions
            .remove(&position_id)
            .expect("just checked")
            .position;

        let outcome = match close_position(&self.market.params, &mut core, envelope, &mut position)
        {
            Ok(o) => o,
            Err(e) => {
                self.restore_sub_pool(snap);
                self.positions.insert(position_id, pos_snap);
                return Err(e.into());
            }
        };

        if let Err(e) = self.write_core_sub_pool(sub_pool_id, &core) {
            self.restore_sub_pool(snap);
            self.positions.insert(position_id, pos_snap);
            return Err(e);
        }
        // Persist the mutated position.
        self.positions.insert(
            position_id,
            PositionAccount {
                sub_pool_id,
                position,
            },
        );
        let withdrawable = outcome.withdrawable;
        self.vault_balance = self
            .vault_balance
            .checked_sub(withdrawable)
            .ok_or(MirrorError::Invariant("close: vault underflow"))?;
        self.total_withdrawals += withdrawable;
        Ok(CloseSummary {
            withdrawable,
            outcome,
        })
    }

    /// `force_close_zero_value_position` instruction.
    pub fn force_close(
        &mut self,
        position_id: u64,
        envelope: PriceEnvelope,
        acknowledge_forfeit: bool,
    ) -> Result<ForceCloseOutcome, MirrorError> {
        let pos_acc = self
            .positions
            .get(&position_id)
            .ok_or(MirrorError::PositionNotFound(position_id))?;
        let sub_pool_id = pos_acc.sub_pool_id;
        let snap = self.snapshot_sub_pool(sub_pool_id);
        let pos_snap = pos_acc.clone();

        let mut core = self.build_core_sub_pool(sub_pool_id)?;
        let mut position = self
            .positions
            .remove(&position_id)
            .expect("just checked")
            .position;

        let outcome = match force_close_zero_value_position(
            &self.market.params,
            &mut core,
            envelope,
            &mut position,
            acknowledge_forfeit,
        ) {
            Ok(o) => o,
            Err(e) => {
                self.restore_sub_pool(snap);
                self.positions.insert(position_id, pos_snap);
                return Err(e.into());
            }
        };
        if let Err(e) = self.write_core_sub_pool(sub_pool_id, &core) {
            self.restore_sub_pool(snap);
            self.positions.insert(position_id, pos_snap);
            return Err(e);
        }
        self.positions.insert(
            position_id,
            PositionAccount {
                sub_pool_id,
                position,
            },
        );
        // Force-close moves any forfeited recovery into dust; both
        // pool_equity and dust live inside vault_balance, so no movement.
        Ok(outcome)
    }

    /// `claim_dormant_recovery` instruction.
    pub fn claim_recovery(
        &mut self,
        position_id: u64,
        envelope: PriceEnvelope,
    ) -> Result<ClaimRecoveryOutcome, MirrorError> {
        let pos_acc = self
            .positions
            .get(&position_id)
            .ok_or(MirrorError::PositionNotFound(position_id))?;
        let sub_pool_id = pos_acc.sub_pool_id;
        let snap = self.snapshot_sub_pool(sub_pool_id);
        let pos_snap = pos_acc.clone();

        let mut core = self.build_core_sub_pool(sub_pool_id)?;
        let mut position = self
            .positions
            .remove(&position_id)
            .expect("just checked")
            .position;
        let outcome = match claim_dormant_recovery(
            &self.market.params,
            &mut core,
            envelope,
            &mut position,
        ) {
            Ok(o) => o,
            Err(e) => {
                self.restore_sub_pool(snap);
                self.positions.insert(position_id, pos_snap);
                return Err(e.into());
            }
        };
        if let Err(e) = self.write_core_sub_pool(sub_pool_id, &core) {
            self.restore_sub_pool(snap);
            self.positions.insert(position_id, pos_snap);
            return Err(e);
        }
        self.positions.insert(
            position_id,
            PositionAccount {
                sub_pool_id,
                position,
            },
        );
        let redeemable = outcome.redeemable;
        self.vault_balance = self
            .vault_balance
            .checked_sub(redeemable)
            .ok_or(MirrorError::Invariant("claim: vault underflow"))?;
        self.total_withdrawals += redeemable;
        Ok(outcome)
    }

    /// `harvest_dust` instruction.
    pub fn harvest_dust(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
    ) -> Result<HarvestOutcome, MirrorError> {
        let snap = self.snapshot_sub_pool(sub_pool_id);
        let mut core = self.build_core_sub_pool(sub_pool_id)?;
        let outcome = match harvest_dust(&self.market.params, &mut core, direction) {
            Ok(o) => o,
            Err(e) => {
                self.restore_sub_pool(snap);
                return Err(e.into());
            }
        };
        if let Err(e) = self.write_core_sub_pool(sub_pool_id, &core) {
            self.restore_sub_pool(snap);
            return Err(e);
        }
        let amount = outcome.amount;
        self.vault_balance = self
            .vault_balance
            .checked_sub(amount)
            .ok_or(MirrorError::Invariant("harvest: vault underflow"))?;
        self.fee_vault_balance += amount;
        Ok(outcome)
    }

    /// `pre_sync_dormant_bucket` instruction (lazy-mode keeper path).
    pub fn pre_sync_bucket(
        &mut self,
        sub_pool_id: u32,
        direction: Direction,
        bucket_tick: i64,
        slot: u64,
    ) -> Result<PreSyncOutcome, MirrorError> {
        // Validate the bucket exists; on chain this would be a
        // missing-PDA error from Anchor.
        if !self.buckets.contains_key(&(sub_pool_id, direction, bucket_tick)) {
            return Err(MirrorError::BucketNotInitialized {
                sub_pool: sub_pool_id,
                direction,
                tick: bucket_tick,
            });
        }
        let snap = self.snapshot_sub_pool(sub_pool_id);
        let mut core = self.build_core_sub_pool(sub_pool_id)?;
        let outcome = match pre_sync_dormant_bucket(
            &self.market.params,
            &mut core,
            direction,
            bucket_tick,
            slot,
        ) {
            Ok(o) => o,
            Err(e) => {
                self.restore_sub_pool(snap);
                return Err(e.into());
            }
        };
        if let Err(e) = self.write_core_sub_pool(sub_pool_id, &core) {
            self.restore_sub_pool(snap);
            return Err(e);
        }
        Ok(outcome)
    }
}

/// Snapshot of all accounts that an instruction on `sub_pool_id`
/// could mutate, used for atomic rollback.
struct SubPoolSnapshot {
    sub_pool: SubPoolAccount,
    buckets: HashMap<(u32, Direction, i64), DormantBucketAccount>,
    ledgers: HashMap<(u32, Direction), DistributionLedgerAccount>,
}

/// Result of [`ChainRuntime::open`].
#[derive(Debug, Clone)]
pub struct OpenSummary {
    /// Allocated position id.
    pub position_id: u64,
    /// Shares minted to the position.
    pub shares_minted: u128,
    /// Protocol open fee taken (transferred to fee vault).
    pub open_fee: u64,
    /// Engine events emitted.
    pub events: Vec<EngineEvent>,
}

/// Result of [`ChainRuntime::close`] (and friends that pay out).
#[derive(Debug, Clone)]
pub struct CloseSummary {
    /// Amount transferred from vault to user.
    pub withdrawable: u128,
    /// Full engine outcome (events + per-direction breakdown).
    pub outcome: CloseOutcome,
}

/// Forwarded engine outcome aliases for downstream callers.
pub use clearing_core::PreSyncOutcome as PreSyncBucketOutcome;

/// 32-byte pubkey type re-exported from `keeper-decoder` so the
/// host-side leader-lock model and any future keeper-side mirror
/// can speak the same wire format as the Solana program without
/// pulling in `solana-sdk`.
pub use keeper_decoder::Pubkey32;

/// Wave 15 — host-side state machine for the on-chain
/// `KeeperLeaderLock` PDA.
pub mod leader_lock;

#[cfg(test)]
mod tests;

/// Re-export of [`clearing_core::PositionStatus`] so callers don't need
/// to depend on `clearing-core` directly when they just want to assert
/// position state in tests.
pub use clearing_core::PositionStatus;
