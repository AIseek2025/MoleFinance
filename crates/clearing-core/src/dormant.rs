//! Dormant bucket store and aggregation.
//!
//! Mirrors `Docs/Planning/18-shares模型实现细则与边界条件.md` §6 and §10.
//!
//! ## Eager update + lazy replay ledger
//!
//! Today the host implementation updates every activated bucket eagerly
//! during [`DormantStore::distribute`]. In addition to that, every
//! distribution appends a [`DistEntry`] to a per-direction **ledger**.
//! Each [`DormantBucket`] records `last_applied_index`, the count of
//! ledger events already reflected in its `accrued_value`.
//!
//! For the existing eager path, `last_applied_index` is bumped to
//! `ledger.len()` at the end of every `distribute` and at every
//! `insert_or_merge` (a freshly-created bucket starts fully synced —
//! it didn't exist for past events). Both invariants are re-verified
//! by [`DormantStore::check_invariants`] after every state-mutating
//! call.
//!
//! The replay logic itself — [`DormantStore::apply_pending_to_bucket`]
//! — is the building block for the on-chain "sync_pool only writes the
//! aggregate and the ledger; users pay to apply pending on touch" path
//! described in `Docs/Planning/18-shares模型实现细则与边界条件.md` §10.3.
//! It is fully exercised by property tests that rewind a bucket to
//! `last_applied_index = 0` and prove the replay reaches the same
//! `accrued_value` the eager path produced, **byte-for-byte**.
//!
//! ## Compaction
//!
//! When every live bucket has `last_applied_index >= K`, ledger entries
//! `[0, K)` are no longer needed and can be GC'd by
//! [`DormantStore::compact_ledger`]. A `ledger_gc_offset` records how
//! many entries have been dropped from the front; bucket indices remain
//! globally monotonic (they use the absolute `event_index`, not the
//! position within the in-memory `Vec`).

use std::collections::BTreeMap;

use molemath::{checked_add, checked_sub, mul_div_floor};

use crate::error::{ClearingError, ClearingResult};
use crate::types::Direction;

/// Aggregated bucket of dormant positions.
#[derive(Debug, Clone)]
pub struct DormantBucket {
    /// Direction of the bucket (`Long` or `Short`).
    pub direction: Direction,
    /// `floor(zero_price / price_tick / tick_aggregation_factor)`.
    pub zero_price_tick: i64,
    /// Anchor price representative of this tick (== `zero_price` of the
    /// first migrated position; subsequent migrations to the same tick keep
    /// the original anchor for fairness).
    pub anchor_price: u64,
    /// Aggregated recovery shares of all positions in this bucket.
    pub total_recovery_shares: u128,
    /// Aggregated dormant notional, used for claim-demand computations.
    pub total_recovery_notional: u128,
    /// Cumulative funds already attributed to this bucket; redeemable by
    /// `claim_dormant_recovery`.
    pub accrued_value: u128,
    /// Number of distinct positions whose recovery shares are aggregated here.
    pub position_count: u64,
    /// Absolute index of the last [`DistEntry`] applied to this bucket's
    /// `accrued_value`. A freshly-created bucket starts at the current
    /// ledger length, so it never replays events that predate it.
    pub last_applied_index: u64,
}

/// One distribution event in the lazy replay ledger.
///
/// Snapshot of the inputs needed to recompute any single bucket's share
/// of a distribution **without** touching the rest of the store. The
/// fields together fully determine `share_i = min(outstanding_i,
/// floor(total_alloc_input * outstanding_i / total_outstanding_at_event))`.
///
/// **Important**: `total_alloc_input` is the *original* `total_alloc`
/// argument passed to [`DormantStore::distribute`] /
/// [`DormantStore::distribute_lazy`] — **before** any per-bucket floor
/// rounding. Replay must use the original input as the numerator so
/// that per-bucket shares match the eager path byte-for-byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DistEntry {
    /// Absolute, monotonically-increasing event index. Buckets compare
    /// their `last_applied_index` against this value to decide whether
    /// the entry has already been applied.
    pub event_index: u64,
    /// Price at which the distribution was performed.
    pub p_at_event: u64,
    /// Sum of `outstanding_claim_at(p_at_event)` across all buckets that
    /// were activated at the moment of the distribution (i.e., the
    /// denominator used to compute proportional shares).
    pub total_outstanding_at_event: u128,
    /// **Original** `total_alloc` input passed to `distribute*`. This
    /// is used as the numerator in the per-bucket floor formula on
    /// both the eager and lazy paths, so replay reproduces eager
    /// allocations exactly.
    pub total_alloc_input: u128,
    /// Sum of per-bucket shares **actually** added to `accrued_value`
    /// at distribute time. May be less than `total_alloc_input` due to
    /// per-bucket flooring (the difference is the dust residual the
    /// caller redirected). Stored for analytics / observability only;
    /// replay does not use this field.
    pub allocated_sum_observed: u128,
}

impl DormantBucket {
    /// Returns this bucket's intrinsic claim value at price `p_now`,
    /// **ignoring** funds already accrued.
    ///
    /// For a Long bucket anchored at `anchor`:
    /// `intrinsic = total_recovery_notional * max(0, p_now - anchor) / anchor`.
    ///
    /// For a Short bucket: `... * max(0, anchor - p_now) / anchor`.
    pub fn intrinsic_claim_at(&self, p_now: u64) -> ClearingResult<u128> {
        if self.anchor_price == 0 || self.total_recovery_notional == 0 {
            return Ok(0);
        }
        let delta = match self.direction {
            Direction::Long => {
                if p_now <= self.anchor_price {
                    return Ok(0);
                }
                (p_now - self.anchor_price) as u128
            }
            Direction::Short => {
                if p_now >= self.anchor_price {
                    return Ok(0);
                }
                (self.anchor_price - p_now) as u128
            }
        };
        let claim = mul_div_floor(self.total_recovery_notional, delta, self.anchor_price as u128)?;
        Ok(claim)
    }

    /// Outstanding claim at `p_now`, equal to intrinsic minus already accrued.
    pub fn outstanding_claim_at(&self, p_now: u64) -> ClearingResult<u128> {
        let intrinsic = self.intrinsic_claim_at(p_now)?;
        Ok(intrinsic.saturating_sub(self.accrued_value))
    }
}

/// Per-direction store of dormant buckets for a single subpool.
#[derive(Debug, Clone)]
pub struct DormantStore {
    direction: Direction,
    buckets: BTreeMap<i64, DormantBucket>,
    /// Cached sum of `accrued_value`, kept in sync with `buckets`.
    accrued_value_total: u128,
    /// Append-only ledger of distribution events. See [`DistEntry`] and
    /// [`apply_pending_to_bucket`](Self::apply_pending_to_bucket).
    ledger: Vec<DistEntry>,
    /// Number of entries GC'd from the front of `ledger` by
    /// [`compact_ledger`](Self::compact_ledger). Bucket
    /// `last_applied_index` is **absolute**: a value of `5` means "events
    /// 0..=4 have been applied", regardless of whether those events still
    /// physically live in the in-memory `Vec`.
    ledger_gc_offset: u64,
    /// Number of distribution events ever recorded (including ones that
    /// have since been GC'd). Equal to `ledger_gc_offset + ledger.len()`.
    next_event_index: u64,
    /// Lazy-mode in-flight allocations: funds committed to the ledger
    /// by [`distribute_lazy`](Self::distribute_lazy) but not yet
    /// attributed to any bucket's `accrued_value` via
    /// [`apply_pending_to_bucket`](Self::apply_pending_to_bucket).
    ///
    /// In eager mode this stays `0` (each `distribute()` call writes
    /// directly into per-bucket `accrued_value` and bumps
    /// `accrued_value_total`).
    ///
    /// Invariant maintained by the engine:
    /// `pending_distribution_total ==`
    ///   Σ over ledger entries `e` of
    ///   `(e.allocated_sum_observed − Σ over buckets b of share_applied(e, b))`
    ///
    /// In particular, after fully draining via
    /// [`apply_pending_to_all`](Self::apply_pending_to_all),
    /// `pending_distribution_total == 0`. The host
    /// [`crate::protocol_harness::Harness`] and
    /// [`chain_mirror::ChainRuntime`] include this term in their vault
    /// decomposition `vault == pool_equity + accrued_value_total +
    /// pending_distribution_total + dust`.
    pending_distribution_total: u128,
}

impl DormantStore {
    /// Construct an empty store for the given direction.
    pub fn new(direction: Direction) -> Self {
        Self {
            direction,
            buckets: BTreeMap::new(),
            accrued_value_total: 0,
            ledger: Vec::new(),
            ledger_gc_offset: 0,
            next_event_index: 0,
            pending_distribution_total: 0,
        }
    }

    /// Reconstruct a store from on-chain account parts.
    ///
    /// Used by [`crate::onchain::unpack_dormant_store`] when bridging
    /// Solana account state into the engine. The caller is responsible
    /// for ensuring the parts are internally consistent — this
    /// constructor performs only the cheap structural checks below
    /// and trusts the caller for the rest. The
    /// [`check_invariants`](Self::check_invariants) method should be
    /// called immediately afterwards in any production-equivalent
    /// path; the on-chain wrapper does this and so do the
    /// `tests/onchain_layout.rs` property tests.
    ///
    /// Cheap checks performed here:
    /// * `next_event_index == ledger_gc_offset + ledger.len()` (every
    ///   in-memory entry is contiguous from `gc_offset`).
    /// * Every bucket's `direction` matches `direction`.
    /// * Every bucket's `last_applied_index` is in
    ///   `[ledger_gc_offset, next_event_index]` (no GC'd-window or
    ///   ahead-of-head replay state).
    /// * `accrued_value_total == sum(bucket.accrued_value)`.
    /// * `pending_distribution_total <= sum(entry.allocated_sum_observed)`
    ///   (pending can never exceed the lifetime-distributed amount in the
    ///   live ledger window).
    ///
    /// Returns [`ClearingError::Invariant`] on any structural failure.
    #[doc(hidden)]
    pub fn from_onchain_parts(
        direction: Direction,
        buckets: Vec<(i64, DormantBucket)>,
        ledger: Vec<DistEntry>,
        ledger_gc_offset: u64,
        next_event_index: u64,
        accrued_value_total: u128,
        pending_distribution_total: u128,
    ) -> ClearingResult<Self> {
        let computed_next = ledger_gc_offset
            .checked_add(ledger.len() as u64)
            .ok_or(ClearingError::MathOverflow)?;
        if computed_next != next_event_index {
            return Err(ClearingError::Invariant(
                "next_event_index != ledger_gc_offset + ledger.len()",
            ));
        }
        let mut sum_accrued: u128 = 0;
        let mut map: BTreeMap<i64, DormantBucket> = BTreeMap::new();
        for (tick, bucket) in buckets {
            if bucket.direction != direction {
                return Err(ClearingError::Invariant(
                    "bucket direction does not match store direction",
                ));
            }
            if bucket.last_applied_index < ledger_gc_offset {
                return Err(ClearingError::Invariant(
                    "bucket.last_applied_index inside GC'd window",
                ));
            }
            if bucket.last_applied_index > next_event_index {
                return Err(ClearingError::Invariant(
                    "bucket.last_applied_index ahead of ledger head",
                ));
            }
            sum_accrued = checked_add(sum_accrued, bucket.accrued_value)?;
            if map.insert(tick, bucket).is_some() {
                return Err(ClearingError::Invariant("duplicate tick in unpack"));
            }
        }
        if sum_accrued != accrued_value_total {
            return Err(ClearingError::Invariant("accrued_value_total mismatch"));
        }
        // Cheap structural cap: pending cannot exceed the cumulative
        // allocated amount across live (non-GC'd) ledger entries. The
        // exact equality `pending == Σ entry.allocated_sum_observed −
        // Σ already_applied_per_entry_per_bucket` is too expensive to
        // recompute on every unpack and is enforced instead by the
        // engine's own bookkeeping (incremented in `distribute_lazy`,
        // decremented in `apply_pending_to_bucket`).
        let mut sum_alloc_in_window: u128 = 0;
        for entry in &ledger {
            sum_alloc_in_window = checked_add(sum_alloc_in_window, entry.allocated_sum_observed)?;
        }
        if pending_distribution_total > sum_alloc_in_window {
            return Err(ClearingError::Invariant(
                "pending_distribution_total exceeds live-window allocated_sum",
            ));
        }
        Ok(Self {
            direction,
            buckets: map,
            accrued_value_total,
            ledger,
            ledger_gc_offset,
            next_event_index,
            pending_distribution_total,
        })
    }

    /// Number of entries currently held in the ledger (post-compaction).
    pub fn ledger_len(&self) -> usize {
        self.ledger.len()
    }

    /// Total number of distribution events ever recorded, including any
    /// already GC'd via [`compact_ledger`](Self::compact_ledger).
    pub fn next_event_index(&self) -> u64 {
        self.next_event_index
    }

    /// Read-only view of the live ledger window.
    pub fn ledger(&self) -> &[DistEntry] {
        &self.ledger
    }

    /// Number of ledger entries GC'd from the front.
    pub fn ledger_gc_offset(&self) -> u64 {
        self.ledger_gc_offset
    }

    /// Direction this store covers.
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Number of distinct buckets.
    pub fn bucket_count(&self) -> u32 {
        self.buckets.len() as u32
    }

    /// Sum of all `accrued_value` in this store.
    pub fn accrued_value_total(&self) -> u128 {
        self.accrued_value_total
    }

    /// Funds in the lazy ledger committed by
    /// [`distribute_lazy`](Self::distribute_lazy) but not yet
    /// attributed to any bucket's `accrued_value`. See the field
    /// docs for the full invariant.
    ///
    /// Always `0` in eager mode. Drained to `0` by
    /// [`apply_pending_to_all`](Self::apply_pending_to_all).
    pub fn pending_distribution_total(&self) -> u128 {
        self.pending_distribution_total
    }

    /// Return a reference to the bucket at the given tick, if any.
    pub fn get(&self, tick: i64) -> Option<&DormantBucket> {
        self.buckets.get(&tick)
    }

    /// Iterate every bucket in tick-ascending order.
    pub fn iter_buckets(&self) -> impl Iterator<Item = (&i64, &DormantBucket)> {
        self.buckets.iter()
    }

    /// Sorted snapshot of all live tick keys.
    pub fn bucket_ticks(&self) -> Vec<i64> {
        self.buckets.keys().copied().collect()
    }

    /// Sum of `total_recovery_shares` across all buckets.
    pub fn total_recovery_shares(&self) -> u128 {
        self.buckets
            .values()
            .map(|b| b.total_recovery_shares)
            .sum()
    }

    /// Add or update a bucket with the given migration values.
    ///
    /// Returns [`ClearingError::DormantBucketCapExceeded`] if a new bucket
    /// would push past `max_bucket_count`.
    ///
    /// **Lazy-replay invariant**: before mutating an existing bucket's
    /// `total_recovery_*` aggregates, the bucket is brought up to the
    /// current ledger head via [`apply_pending_to_bucket`](Self::apply_pending_to_bucket).
    /// This makes structural mutations a hard boundary for replay, so
    /// future replay never sees "post-merge structure ⨯ pre-merge
    /// event" combinations (which would over-count). Freshly-created
    /// buckets are inserted with `last_applied_index = next_event_index`
    /// and so skip all past events by construction.
    pub fn insert_or_merge(
        &mut self,
        tick: i64,
        anchor_price: u64,
        added_shares: u128,
        added_notional: u128,
        added_position_count: u64,
        max_bucket_count: u32,
    ) -> ClearingResult<()> {
        if self.buckets.contains_key(&tick) {
            // Drain pending events before mutating structure; see
            // the "Lazy-replay invariant" above.
            self.apply_pending_to_bucket(tick)?;
            let bucket = self.buckets.get_mut(&tick).expect("just checked");
            bucket.total_recovery_shares = checked_add(bucket.total_recovery_shares, added_shares)?;
            bucket.total_recovery_notional =
                checked_add(bucket.total_recovery_notional, added_notional)?;
            bucket.position_count = bucket
                .position_count
                .checked_add(added_position_count)
                .ok_or(ClearingError::MathOverflow)?;
            return Ok(());
        }

        if (self.buckets.len() as u32) >= max_bucket_count {
            return Err(ClearingError::DormantBucketCapExceeded);
        }

        let bucket = DormantBucket {
            direction: self.direction,
            zero_price_tick: tick,
            anchor_price,
            total_recovery_shares: added_shares,
            total_recovery_notional: added_notional,
            accrued_value: 0,
            position_count: added_position_count,
            // Newly-created buckets skip past events: they didn't exist for them.
            last_applied_index: self.next_event_index,
        };
        self.buckets.insert(tick, bucket);
        Ok(())
    }

    /// Sum of outstanding claim across **activated** buckets at price `p_now`.
    ///
    /// A long bucket is activated when `p_now > anchor_price`.
    /// A short bucket is activated when `p_now < anchor_price`.
    pub fn total_outstanding_claim_at(&self, p_now: u64) -> ClearingResult<u128> {
        let mut total: u128 = 0;
        for bucket in self.activated_buckets(p_now) {
            total = checked_add(total, bucket.outstanding_claim_at(p_now)?)?;
        }
        Ok(total)
    }

    /// Iterate over activated buckets only.
    fn activated_buckets(&self, p_now: u64) -> Vec<&DormantBucket> {
        match self.direction {
            // Long: anchor < p_now (bucket positions zeroed below current price).
            // We want all buckets with anchor strictly less than p_now.
            Direction::Long => self
                .buckets
                .values()
                .filter(|b| p_now > b.anchor_price)
                .collect(),
            // Short: anchor > p_now.
            Direction::Short => self
                .buckets
                .values()
                .filter(|b| p_now < b.anchor_price)
                .collect(),
        }
    }

    /// Distribute `total_alloc` across activated buckets in proportion to
    /// each bucket's outstanding claim — **eager** path.
    ///
    /// Allocation rounds floor; any residual due to rounding is returned
    /// to the caller (so it can be redirected to dust without leaking
    /// shares). Every successful, non-zero distribution is appended to
    /// the lazy replay ledger as a [`DistEntry`]. The host-side
    /// `clearing-core` engine uses this path because it can afford to
    /// touch every activated bucket; the on-chain program will switch
    /// to [`distribute_lazy`](Self::distribute_lazy) when the activated
    /// set exceeds a single transaction's compute budget.
    pub fn distribute(
        &mut self,
        p_now: u64,
        total_alloc: u128,
        max_ledger_size: u32,
    ) -> ClearingResult<DistributionReceipt> {
        if total_alloc == 0 {
            return Ok(DistributionReceipt {
                allocated: 0,
                residual: 0,
            });
        }

        let activated_keys: Vec<i64> = match self.direction {
            Direction::Long => self
                .buckets
                .iter()
                .filter(|(_, b)| p_now > b.anchor_price)
                .map(|(k, _)| *k)
                .collect(),
            Direction::Short => self
                .buckets
                .iter()
                .filter(|(_, b)| p_now < b.anchor_price)
                .map(|(k, _)| *k)
                .collect(),
        };

        // Capacity back-pressure: this call will append exactly one
        // entry to the ledger. We only attempt compaction if we are
        // about to overflow — keeping compaction lazy preserves
        // historical ledger entries for the dual-replay tests in
        // `tests/lazy_ledger.rs` (which deliberately rewind a bucket
        // to its creation index and replay). When the ring is full
        // and compaction can't free anything (some live bucket still
        // has not advanced), we surface
        // [`ClearingError::LedgerCapacityExceeded`] so the keeper
        // can drive `pre_sync_dormant_bucket` for the lagging bucket
        // and retry. The check happens BEFORE any state mutation
        // here — combined with the engine-level snapshot/restore in
        // the protocol harness (mirroring Solana tx-revert semantics)
        // we never leave the store in a partially-mutated state on Err.
        if self.ledger.len() >= max_ledger_size as usize {
            self.compact_ledger();
            if self.ledger.len() >= max_ledger_size as usize {
                return Err(ClearingError::LedgerCapacityExceeded);
            }
        }

        // Drain pending lazy shares for every activated bucket BEFORE
        // computing this entry's outstanding numerator. If we don't,
        // `bucket.accrued_value` still reflects pre-lazy values and
        // any in-flight `pending_distribution_total` slice that was
        // earmarked for this bucket would be silently skipped when
        // we bump `last_applied_index` to `event_index + 1` below.
        //
        // In production (fixed-mode market) this is a no-op: eager-
        // only stores never accumulate `pending_distribution_total`.
        // The cost is paid only by the mixed-mode property tests and
        // by future governance mode-flip migrations.
        for key in &activated_keys {
            self.apply_pending_to_bucket(*key)?;
        }

        let mut total_outstanding: u128 = 0;
        for key in &activated_keys {
            let bucket = self.buckets.get(key).expect("key from iter");
            total_outstanding = checked_add(total_outstanding, bucket.outstanding_claim_at(p_now)?)?;
        }

        if total_outstanding == 0 {
            return Ok(DistributionReceipt {
                allocated: 0,
                residual: total_alloc,
            });
        }

        let event_index = self.next_event_index;

        let mut allocated_sum: u128 = 0;
        for key in &activated_keys {
            let bucket = self.buckets.get(key).expect("key from iter");
            let outstanding = bucket.outstanding_claim_at(p_now)?;
            if outstanding == 0 {
                // Still considered "applied" so the bucket's
                // last_applied_index advances and the lazy replay path
                // remains a no-op for this entry.
                let bucket = self.buckets.get_mut(key).expect("key from iter");
                bucket.last_applied_index = event_index + 1;
                continue;
            }
            let share = mul_div_floor(total_alloc, outstanding, total_outstanding)?;
            // Cap at the bucket's outstanding (avoids over-allocation due to
            // any prior rounding effects).
            let share = share.min(outstanding);
            let bucket = self.buckets.get_mut(key).expect("key from iter");
            if share != 0 {
                bucket.accrued_value = checked_add(bucket.accrued_value, share)?;
                allocated_sum = checked_add(allocated_sum, share)?;
            }
            bucket.last_applied_index = event_index + 1;
        }
        // Non-activated buckets are deliberately NOT touched here.
        //
        // Earlier code "optimised" by also bumping their
        // `last_applied_index` past the new entry on the assumption
        // that this entry is a no-op for them (their activated check
        // at `entry.p_at_event` would always be false for non-activated
        // buckets at the same price). That was correct for eager-only
        // stores but became unsound the moment any lazy entry exists in
        // the same ledger: a non-activated bucket at the eager moment
        // may have been activated at an earlier lazy entry's price and
        // owe a non-zero `pending_distribution_total` slice. Bumping
        // its `last_applied_index` past those lazy entries would
        // permanently strand that slice in pending. The on-touch
        // [`apply_pending_to_bucket`] sweep handles the no-op case
        // O(stale_entries) lazily; the cost is exactly the cost saved
        // by the original optimisation, so removing it is free for
        // pure-eager stores while making mixed-mode (governance
        // mode-flip migrations + the
        // `tests/onchain_layout.rs::pack_unpack_round_trip_under_random_ops`
        // property test) provably leak-free.
        self.accrued_value_total = checked_add(self.accrued_value_total, allocated_sum)?;
        let residual = checked_sub(total_alloc, allocated_sum)?;

        // Append the event to the ledger AFTER applying eagerly so that
        // an outside observer can never see a ledger entry that does not
        // match the per-bucket state. The replay path uses
        // `total_alloc_input` as the numerator, so eager and lazy
        // produce byte-identical per-bucket shares.
        self.ledger.push(DistEntry {
            event_index,
            p_at_event: p_now,
            total_outstanding_at_event: total_outstanding,
            total_alloc_input: total_alloc,
            allocated_sum_observed: allocated_sum,
        });
        self.next_event_index = event_index
            .checked_add(1)
            .ok_or(ClearingError::MathOverflow)?;

        Ok(DistributionReceipt {
            allocated: allocated_sum,
            residual,
        })
    }

    /// Distribute `total_alloc` against activated buckets — **lazy** path.
    ///
    /// Unlike [`distribute`](Self::distribute), this method does **not**
    /// touch any individual bucket's `accrued_value`. It only:
    ///
    /// 1. Computes the global `total_outstanding_at_event` by replaying
    ///    every ledger entry the activated buckets have **not yet
    ///    applied**, on the fly, against the existing aggregate.
    ///    Concretely, we visit only the activated buckets to extract
    ///    `(notional, anchor, accrued_at_event_time)`, where the third
    ///    field is materialised by replaying any pending ledger entries
    ///    that pre-date the new event. The replay is bounded by the
    ///    number of buckets actually visited; under the lazy path the
    ///    on-chain version partitions buckets across multiple txs and
    ///    operates on a small slice each call.
    /// 2. Appends a [`DistEntry`] capturing
    ///    `(p_at_event, total_outstanding_at_event, transfer_total)`.
    /// 3. Bumps `next_event_index` and returns a receipt with the
    ///    capped allocated sum and the rounding residual. Per-bucket
    ///    `accrued_value` is updated lazily — the next time anyone
    ///    touches the bucket via [`redeem`](Self::redeem) or
    ///    [`apply_pending_to_bucket`](Self::apply_pending_to_bucket).
    ///
    /// The lazy path is **observably equivalent** to the eager path
    /// after applying pending to every bucket: the
    /// `lazy_eager_equivalence_under_random_ops` property test asserts
    /// this byte-for-byte. The on-chain Anchor program will use this
    /// path when activated bucket count exceeds a single tx's CU
    /// budget; the keeper / users then top up
    /// `apply_pending_to_bucket` over multiple subsequent txs.
    pub fn distribute_lazy(
        &mut self,
        p_now: u64,
        total_alloc: u128,
        max_ledger_size: u32,
    ) -> ClearingResult<DistributionReceipt> {
        if total_alloc == 0 {
            return Ok(DistributionReceipt {
                allocated: 0,
                residual: 0,
            });
        }

        // See the matching block in `distribute` for context. Lazy
        // mode appends to the ledger but doesn't visit non-activated
        // buckets, so back-pressure here is the only thing keeping
        // a runaway sync rate from blowing past the on-chain account
        // size. Lazy compaction (only when ring is full) preserves
        // historical entries for the dual-replay tests.
        if self.ledger.len() >= max_ledger_size as usize {
            self.compact_ledger();
            if self.ledger.len() >= max_ledger_size as usize {
                return Err(ClearingError::LedgerCapacityExceeded);
            }
        }

        // Replay-and-snapshot every activated bucket so the
        // total_outstanding_at_event we record is exactly the value the
        // eager path would have observed at the same moment. Bucket
        // mutations from pending entries are persistent (they bring the
        // aggregate up to date for free) but the new event's allocation
        // is **not** applied to bucket state yet.
        let activated_keys: Vec<i64> = match self.direction {
            Direction::Long => self
                .buckets
                .iter()
                .filter(|(_, b)| p_now > b.anchor_price)
                .map(|(k, _)| *k)
                .collect(),
            Direction::Short => self
                .buckets
                .iter()
                .filter(|(_, b)| p_now < b.anchor_price)
                .map(|(k, _)| *k)
                .collect(),
        };

        let mut total_outstanding: u128 = 0;
        for key in &activated_keys {
            // Bring the bucket up to the current event boundary: this
            // costs at most O(events_since_last_touch) per bucket and
            // is bounded by ledger length (the on-chain version pages
            // over buckets across multiple txs to keep it within CU).
            self.apply_pending_to_bucket(*key)?;
            let bucket = self.buckets.get(key).expect("key from iter");
            total_outstanding =
                checked_add(total_outstanding, bucket.outstanding_claim_at(p_now)?)?;
        }

        // Match the eager path's short-circuit: when nothing is
        // claimable, do not record a ledger entry. This keeps
        // `next_event_index` identical between eager and lazy stores
        // driven by the same op stream.
        if total_outstanding == 0 {
            return Ok(DistributionReceipt {
                allocated: 0,
                residual: total_alloc,
            });
        }

        // We need to know the exact `allocated_sum` (post-floor) so the
        // caller can route the residual to dust. Computing it requires
        // floor-summing per activated bucket — same cost as the eager
        // path. The lazy savings come from **not** mutating the
        // buckets: their `accrued_value` stays untouched until they
        // are touched by a redeem / explicit apply call.
        let mut allocated_sum: u128 = 0;
        if total_outstanding > 0 {
            for key in &activated_keys {
                let bucket = self.buckets.get(key).expect("key from iter");
                let outstanding = bucket.outstanding_claim_at(p_now)?;
                if outstanding == 0 {
                    continue;
                }
                let share = mul_div_floor(total_alloc, outstanding, total_outstanding)?;
                let share = share.min(outstanding);
                allocated_sum = checked_add(allocated_sum, share)?;
            }
        }
        let residual = checked_sub(total_alloc, allocated_sum)?;

        let event_index = self.next_event_index;
        self.ledger.push(DistEntry {
            event_index,
            p_at_event: p_now,
            total_outstanding_at_event: total_outstanding,
            total_alloc_input: total_alloc,
            allocated_sum_observed: allocated_sum,
        });
        self.next_event_index = event_index
            .checked_add(1)
            .ok_or(ClearingError::MathOverflow)?;
        // The funds we just routed are physically out of the caller's
        // pool_equity but not yet attributed to any bucket's
        // `accrued_value` — they're "in flight" inside this ledger
        // entry until `apply_pending_to_bucket` walks the activated
        // buckets. Track the in-flight balance so the host
        // [`crate::protocol_harness::Harness`] /
        // `chain_mirror::ChainRuntime` vault decomposition stays
        // closed in lazy mode. `apply_pending_to_bucket` decrements
        // by the per-bucket share each time it attributes one.
        self.pending_distribution_total =
            checked_add(self.pending_distribution_total, allocated_sum)?;
        // `last_applied_index` for each bucket stays at `event_index`
        // until apply_pending_to_bucket walks the new entry.
        //
        // Note: we deliberately do NOT eagerly bump
        // `last_applied_index` for inactive buckets here. Doing so
        // would force every distribute to be O(bucket_count) (the
        // whole point of the lazy path is to avoid that), and worse
        // it would *skip* any pending events those inactive buckets
        // hadn't yet replayed. apply_pending_to_bucket handles the
        // new entry correctly as a no-op for inactive buckets at
        // touch time.

        Ok(DistributionReceipt {
            allocated: allocated_sum,
            residual,
        })
    }

    /// Convenience: bring every live bucket fully up to date.
    ///
    /// Useful for tests and for keeper jobs that want to materialise
    /// the lazy state into the eager equivalent before
    /// [`compact_ledger`](Self::compact_ledger).
    pub fn apply_pending_to_all(&mut self) -> ClearingResult<u64> {
        let ticks = self.bucket_ticks();
        let mut total: u64 = 0;
        for tick in ticks {
            total += self.apply_pending_to_bucket(tick)?;
        }
        Ok(total)
    }

    /// Return the **pure-read** total share that the bucket at `tick`
    /// would gain if [`apply_pending_to_bucket`](Self::apply_pending_to_bucket)
    /// were called right now. Mirrors the inner replay loop of
    /// `apply_pending_to_bucket` byte-for-byte but does **not** mutate
    /// any state.
    ///
    /// In eager mode this is always `0` (per-bucket `last_applied_index`
    /// equals `next_event_index` after every `distribute()`). In lazy
    /// mode it equals the bucket's slice of the in-flight
    /// `pending_distribution_total`, computed from the same floored
    /// `mul_div_floor(total_alloc_input * outstanding /
    /// total_outstanding_at_event)` formula the engine uses.
    ///
    /// Used by [`crate::protocol_harness::Harness::check_invariants`]
    /// to verify that `indexer.bucket.accrued_value ==
    /// chain.bucket.accrued_value + pending_for_bucket(tick)` holds at
    /// every step in lazy mode (wave 5.5 indexer parity refinement).
    pub fn pending_for_bucket(&self, tick: i64) -> ClearingResult<u128> {
        let direction = self.direction;
        let gc_offset = self.ledger_gc_offset;
        let next_event = self.next_event_index;
        let bucket = match self.buckets.get(&tick) {
            Some(b) => b,
            None => return Ok(0),
        };
        if bucket.last_applied_index >= next_event {
            return Ok(0);
        }
        if bucket.last_applied_index < gc_offset {
            return Err(ClearingError::Invariant(
                "bucket.last_applied_index points into a GC'd ledger window",
            ));
        }
        let start_in_vec = (bucket.last_applied_index - gc_offset) as usize;
        // Walk the same trail as `apply_pending_to_bucket` but on a
        // simulated copy of `bucket.accrued_value`. We only need the
        // final delta so we never hold a clone of `DormantBucket`.
        let mut sim_accrued = bucket.accrued_value;
        let mut delta: u128 = 0;
        for entry in &self.ledger[start_in_vec..] {
            if entry.total_outstanding_at_event == 0 {
                continue;
            }
            let activated = match direction {
                Direction::Long => entry.p_at_event > bucket.anchor_price,
                Direction::Short => entry.p_at_event < bucket.anchor_price,
            };
            if !activated || bucket.total_recovery_notional == 0 || bucket.anchor_price == 0 {
                continue;
            }
            let intrinsic = compute_intrinsic(direction, bucket, entry.p_at_event)?;
            let outstanding = intrinsic.saturating_sub(sim_accrued);
            if outstanding == 0 {
                continue;
            }
            let share = mul_div_floor(
                entry.total_alloc_input,
                outstanding,
                entry.total_outstanding_at_event,
            )?;
            let share = share.min(outstanding);
            if share == 0 {
                continue;
            }
            sim_accrued = checked_add(sim_accrued, share)?;
            delta = checked_add(delta, share)?;
        }
        Ok(delta)
    }

    /// Replay every ledger entry whose `event_index >= bucket.last_applied_index`
    /// against the bucket at `tick`, bringing its `accrued_value` up to
    /// the current ledger head.
    ///
    /// For the eager-update path (the only one currently used by the
    /// engine), this method is a no-op: every distribute() advances every
    /// bucket's `last_applied_index` to `next_event_index` already. The
    /// method's job is to (a) handle freshly-created buckets that pre-date
    /// some recent compaction-window events, and (b) be the **single
    /// reference implementation** of the lazy replay semantics described
    /// in `Docs/Planning/18-shares模型实现细则与边界条件.md` §10.3 — so
    /// when the on-chain version flips to the lazy path, the math is
    /// already provably equivalent.
    ///
    /// Returns the number of events that were actually applied.
    pub fn apply_pending_to_bucket(&mut self, tick: i64) -> ClearingResult<u64> {
        let direction = self.direction;
        let gc_offset = self.ledger_gc_offset;
        let next_event = self.next_event_index;
        let ledger = &self.ledger;
        let mut net_accrued_delta: u128 = 0;

        let bucket = match self.buckets.get_mut(&tick) {
            Some(b) => b,
            None => return Ok(0),
        };

        if bucket.last_applied_index >= next_event {
            return Ok(0);
        }
        if bucket.last_applied_index < gc_offset {
            return Err(ClearingError::Invariant(
                "bucket.last_applied_index points into a GC'd ledger window",
            ));
        }

        let start_in_vec = (bucket.last_applied_index - gc_offset) as usize;
        let mut applied: u64 = 0;
        for entry in &ledger[start_in_vec..] {
            if entry.total_outstanding_at_event == 0 {
                bucket.last_applied_index = entry.event_index + 1;
                applied += 1;
                continue;
            }
            let activated = match direction {
                Direction::Long => entry.p_at_event > bucket.anchor_price,
                Direction::Short => entry.p_at_event < bucket.anchor_price,
            };
            if activated && bucket.total_recovery_notional > 0 && bucket.anchor_price > 0 {
                let intrinsic = compute_intrinsic(direction, bucket, entry.p_at_event)?;
                let outstanding = intrinsic.saturating_sub(bucket.accrued_value);
                if outstanding > 0 {
                    // Use `total_alloc_input` (the original distribute
                    // input) as the numerator — matches eager exactly.
                    let share = mul_div_floor(
                        entry.total_alloc_input,
                        outstanding,
                        entry.total_outstanding_at_event,
                    )?;
                    let share = share.min(outstanding);
                    if share > 0 {
                        bucket.accrued_value = checked_add(bucket.accrued_value, share)?;
                        net_accrued_delta = checked_add(net_accrued_delta, share)?;
                    }
                }
            }
            bucket.last_applied_index = entry.event_index + 1;
            applied += 1;
        }

        if net_accrued_delta > 0 {
            self.accrued_value_total =
                checked_add(self.accrued_value_total, net_accrued_delta)?;
            // Funds previously parked in `pending_distribution_total`
            // by `distribute_lazy` have just been attributed to this
            // bucket's `accrued_value` — drain them out of the
            // pending pool. In eager mode this branch never fires
            // because `distribute()` writes per-bucket directly and
            // never inflates `pending_distribution_total`. We still
            // saturate-subtract defensively so a corrupted account
            // can never panic the program; an underflow would have
            // already tripped the
            // `from_onchain_parts::pending_distribution_total >
            // sum_alloc_in_window` invariant on unpack.
            self.pending_distribution_total = self
                .pending_distribution_total
                .saturating_sub(net_accrued_delta);
        }
        Ok(applied)
    }

    /// GC ledger entries whose `event_index < min(bucket.last_applied_index)`
    /// across all live buckets. Returns the number of entries dropped.
    ///
    /// After compaction, no live bucket can ever produce work for the
    /// dropped events (they've all been applied past them); the entries
    /// are pure history and can be dropped to bound memory. The
    /// `ledger_gc_offset` advances by the same amount so absolute event
    /// indices remain unambiguous.
    pub fn compact_ledger(&mut self) -> usize {
        let watermark = self
            .buckets
            .values()
            .map(|b| b.last_applied_index)
            .min()
            .unwrap_or(self.next_event_index);
        if watermark <= self.ledger_gc_offset {
            return 0;
        }
        let drop = (watermark - self.ledger_gc_offset) as usize;
        let drop = drop.min(self.ledger.len());
        self.ledger.drain(..drop);
        self.ledger_gc_offset += drop as u64;
        drop
    }

    /// **Test-only**: rewind one bucket so the lazy replay path can be
    /// exercised against an eager reference. Resets the bucket's
    /// `accrued_value` to `0` and its `last_applied_index` to
    /// `target_event_index`, while subtracting the same amount from the
    /// store-level `accrued_value_total` so the per-store sum invariant
    /// still holds.
    ///
    /// `target_event_index` should normally be the value of
    /// `next_event_index` at the moment the bucket was created — i.e. the
    /// earliest legal point a replay could start from.
    #[doc(hidden)]
    pub fn rewind_bucket_for_replay_test(
        &mut self,
        tick: i64,
        target_event_index: u64,
    ) -> ClearingResult<()> {
        let drop = {
            let bucket = self
                .buckets
                .get_mut(&tick)
                .ok_or(ClearingError::Invariant("rewind missing bucket"))?;
            if target_event_index < self.ledger_gc_offset {
                return Err(ClearingError::Invariant(
                    "rewind target inside GC'd window",
                ));
            }
            if target_event_index > bucket.last_applied_index {
                return Err(ClearingError::Invariant(
                    "rewind target ahead of bucket state",
                ));
            }
            let drop = bucket.accrued_value;
            bucket.accrued_value = 0;
            bucket.last_applied_index = target_event_index;
            drop
        };
        self.accrued_value_total = checked_sub(self.accrued_value_total, drop)?;
        // Rewind is "un-applying" `drop` worth of allocations; those
        // allocations originally arrived through `distribute_lazy`,
        // were tracked in pending, then drained into `accrued_value`
        // by `apply_pending_to_bucket`. Rewinding restores the
        // pending balance so re-running apply observes the same
        // intermediate state as the first run.
        self.pending_distribution_total = checked_add(self.pending_distribution_total, drop)?;
        Ok(())
    }

    /// Sanity check executed at the end of every state-mutating call by
    /// [`crate::invariants::check_subpool_invariants`]. Catches
    /// double-applied events, off-by-one in `last_applied_index`, and any
    /// drift between `accrued_value_total` and the per-bucket sum.
    pub fn check_invariants(&self) -> ClearingResult<()> {
        let mut sum_accrued: u128 = 0;
        for bucket in self.buckets.values() {
            sum_accrued = checked_add(sum_accrued, bucket.accrued_value)?;
            if bucket.last_applied_index > self.next_event_index {
                return Err(ClearingError::Invariant(
                    "bucket.last_applied_index ahead of ledger",
                ));
            }
            if bucket.last_applied_index < self.ledger_gc_offset {
                return Err(ClearingError::Invariant(
                    "bucket.last_applied_index inside GC'd window",
                ));
            }
        }
        if sum_accrued != self.accrued_value_total {
            return Err(ClearingError::Invariant(
                "accrued_value_total drift",
            ));
        }
        if self.ledger_gc_offset + self.ledger.len() as u64 != self.next_event_index {
            return Err(ClearingError::Invariant(
                "ledger_gc_offset + ledger.len() != next_event_index",
            ));
        }
        // pending_distribution_total can never exceed the cumulative
        // allocated_sum_observed across the live (non-GC'd) ledger
        // window. compact_ledger only drops entries past which every
        // bucket has applied, so by the time an entry is GC'd its
        // contribution to pending is necessarily 0 — pending is
        // therefore bounded by the live window.
        let mut sum_alloc_in_window: u128 = 0;
        for entry in &self.ledger {
            sum_alloc_in_window = checked_add(sum_alloc_in_window, entry.allocated_sum_observed)?;
        }
        if self.pending_distribution_total > sum_alloc_in_window {
            return Err(ClearingError::Invariant(
                "pending_distribution_total > Σ allocated_sum_observed in live window",
            ));
        }
        Ok(())
    }

    /// Burn the given `recovery_shares` for a single position living in the
    /// bucket at `tick`, returning the redeemable funds.
    ///
    /// This deducts proportionally from `accrued_value` and from
    /// `total_recovery_shares` / `total_recovery_notional`.
    ///
    /// Implicitly calls [`apply_pending_to_bucket`](Self::apply_pending_to_bucket)
    /// first, so the redeemed amount always reflects every distribution
    /// recorded in the ledger up to the moment of the redeem — regardless
    /// of whether the engine is on the eager update path (where pending
    /// is empty) or the lazy "apply on touch" path described in
    /// `Docs/Planning/18-shares模型实现细则与边界条件.md` §10.3.
    pub fn redeem(&mut self, tick: i64, shares_to_burn: u128) -> ClearingResult<RedeemReceipt> {
        // Bring the bucket up to date with any pending ledger entries
        // before reading its accrued_value. No-op for the eager path.
        self.apply_pending_to_bucket(tick)?;

        let bucket = self
            .buckets
            .get_mut(&tick)
            .ok_or(ClearingError::Invariant("redeem from missing bucket"))?;

        if shares_to_burn == 0 {
            return Ok(RedeemReceipt {
                redeemable: 0,
                burned_shares: 0,
                burned_notional: 0,
            });
        }
        if shares_to_burn > bucket.total_recovery_shares {
            return Err(ClearingError::Invariant(
                "redeem shares exceed bucket total",
            ));
        }

        let redeemable = mul_div_floor(
            bucket.accrued_value,
            shares_to_burn,
            bucket.total_recovery_shares,
        )?;
        let notional_share = mul_div_floor(
            bucket.total_recovery_notional,
            shares_to_burn,
            bucket.total_recovery_shares,
        )?;

        bucket.accrued_value = checked_sub(bucket.accrued_value, redeemable)?;
        bucket.total_recovery_shares = checked_sub(bucket.total_recovery_shares, shares_to_burn)?;
        bucket.total_recovery_notional =
            checked_sub(bucket.total_recovery_notional, notional_share)?;
        bucket.position_count = bucket.position_count.saturating_sub(1);

        let bucket_is_empty = bucket.total_recovery_shares == 0 && bucket.accrued_value == 0;
        if bucket_is_empty {
            self.buckets.remove(&tick);
        }

        self.accrued_value_total = checked_sub(self.accrued_value_total, redeemable)?;

        Ok(RedeemReceipt {
            redeemable,
            burned_shares: shares_to_burn,
            burned_notional: notional_share,
        })
    }
}

/// Outcome of [`DormantStore::distribute`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DistributionReceipt {
    /// Sum actually attributed to bucket `accrued_value`.
    pub allocated: u128,
    /// Residual funds the caller must redirect (typically into dust).
    pub residual: u128,
}

/// Outcome of [`DormantStore::redeem`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedeemReceipt {
    /// Funds the user receives.
    pub redeemable: u128,
    /// Shares burned.
    pub burned_shares: u128,
    /// Notional removed from the bucket aggregate.
    pub burned_notional: u128,
}

/// Helper: intrinsic claim for a bucket at price `p_now`, given its
/// direction.
///
/// Pulled out so the lazy replay path
/// ([`DormantStore::apply_pending_to_bucket`]) can reuse the exact same
/// formula the eager path uses inside [`DormantBucket::intrinsic_claim_at`],
/// without needing a separate immutable borrow on the bucket.
fn compute_intrinsic(
    direction: Direction,
    bucket: &DormantBucket,
    p_now: u64,
) -> ClearingResult<u128> {
    if bucket.anchor_price == 0 || bucket.total_recovery_notional == 0 {
        return Ok(0);
    }
    let delta = match direction {
        Direction::Long => {
            if p_now <= bucket.anchor_price {
                return Ok(0);
            }
            (p_now - bucket.anchor_price) as u128
        }
        Direction::Short => {
            if p_now >= bucket.anchor_price {
                return Ok(0);
            }
            (bucket.anchor_price - p_now) as u128
        }
    };
    Ok(mul_div_floor(
        bucket.total_recovery_notional,
        delta,
        bucket.anchor_price as u128,
    )?)
}
