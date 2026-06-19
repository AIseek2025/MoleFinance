//! On-chain account-shape POD types and bridge helpers.
//!
//! Defines the **byte-stable** representation of the dormant store
//! that the Anchor program will hold across two account kinds:
//!
//! 1. [`OnChainBucketRecord`] — one record per dormant bucket; lives
//!    inside a per-tick PDA in the Solana program (see
//!    `programs/mole-option/src/state.rs::DormantBucket`).
//! 2. [`OnChainLedger`] — a header + ring buffer of
//!    [`OnChainLedgerEntry`]; one per `(sub_pool, direction)`.
//!
//! These are intentionally `#[repr(C)]` and free of generics so the
//! same struct definitions work both on host (this crate) and inside
//! the Solana program (the Anchor wrapper imports them and adds an
//! `#[account]` attribute via a thin wrapper).
//!
//! ## Pack / unpack contract
//!
//! [`pack_dormant_store`] serialises a host-side
//! [`DormantStore`] into the on-chain shape with capacity checks.
//! [`unpack_dormant_store`] is the inverse and re-validates every
//! invariant the host engine relies on. The
//! `tests/onchain_layout.rs` property tests assert that for any
//! random op stream:
//!
//! ```text
//! pack(unpack(pack(store))) == pack(store)
//! ```
//!
//! and that the resulting `DormantStore` produces identical results
//! to the original under any further engine call. This is the
//! **single source of truth** for the on-chain bridge: any divergence
//! between the host reference and the on-chain layout will be caught
//! by these tests before the Anchor program is built.
//!
//! See `Docs/Planning/23-on-chain-dormant-bridge.md` for the per-
//! instruction account list and CU profile.

use crate::dormant::{DistEntry, DormantBucket, DormantStore};
use crate::error::{ClearingError, ClearingResult};
use crate::types::Direction;

/// On-chain representation of a single [`DormantBucket`].
///
/// Total size: **96 bytes** (88 bytes of data + 8 bytes of trailing
/// padding inserted by Rust to satisfy the 16-byte alignment of the
/// `u128` fields). The Anchor program stores one of these (plus the
/// standard 8-byte account discriminator and `bump` byte) inside each
/// `DormantBucket` PDA. Layout asserted at compile time below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct OnChainBucketRecord {
    /// 0 = Long, 1 = Short. Must match the parent ledger's direction.
    pub direction: u8,
    /// Reserved for future flags / version bumping.
    pub _reserved0: [u8; 7],
    /// Bucket key: `floor(zero_price / price_tick / tick_aggregation_factor)`.
    pub zero_price_tick: i64,
    /// Anchor price for this bucket (the `zero_price` of the first
    /// migrated position).
    pub anchor_price: u64,
    /// Aggregated recovery shares.
    pub total_recovery_shares: u128,
    /// Aggregated dormant notional.
    pub total_recovery_notional: u128,
    /// Cumulative funds attributed to this bucket. Redeemable by
    /// `claim_dormant_recovery`.
    pub accrued_value: u128,
    /// Number of distinct positions whose recovery shares are aggregated.
    pub position_count: u64,
    /// Absolute index of the last [`DistEntry`] applied to this
    /// bucket. Must lie in `[ledger.gc_offset, ledger.next_event_index]`.
    pub last_applied_index: u64,
}

const _: [(); 96] = [(); core::mem::size_of::<OnChainBucketRecord>()];

impl OnChainBucketRecord {
    /// **Wave 8 — single source of truth for "dead PDA" detection.**
    ///
    /// A bucket record is *dead* when none of its engine-observable
    /// fields carry value: no shares, no notional, no accrued value,
    /// no position count. Three distinct origins all satisfy this
    /// predicate and MUST be treated identically by the bridge:
    ///
    /// 1. A keeper just called `init_dormant_bucket(tick)` to
    ///    pre-allocate a PDA for an upcoming rotate. The slot is
    ///    syntactically valid (right `direction`, `tick`) but has
    ///    zero economic content.
    /// 2. The most recent
    ///    [`crate::engine::claim_dormant_recovery`] drained the last
    ///    redeemable share from a bucket — `pack_dormant_store`
    ///    leaves the slot in place (rent stays paid) but zeroes its
    ///    economic fields so a future `insert_or_merge` at the same
    ///    tick can land here without re-allocating.
    /// 3. `pack_direction` Pass 3 just zeroed an engine-removed
    ///    bucket in place because Solana doesn't allow changing the
    ///    seeds of a PDA after creation.
    ///
    /// In all three cases [`unpack_dormant_store`] / the bridge MUST
    /// silently skip the record so [`crate::dormant::DormantStore::
    /// insert_or_merge`] takes the "create new bucket" branch and
    /// sets `anchor_price` correctly. See
    /// `Docs/Planning/23-on-chain-dormant-bridge.md` § wave-7.2 for
    /// the full bug post-mortem.
    #[inline]
    pub const fn is_dead(&self) -> bool {
        self.total_recovery_shares == 0
            && self.total_recovery_notional == 0
            && self.accrued_value == 0
            && self.position_count == 0
    }

    /// **Wave 8.** Construct the canonical "dead" PDA shape that
    /// `init_dormant_bucket` would write on chain. All
    /// engine-observable fields are zero; only the identity fields
    /// (`direction`, `zero_price_tick`) are populated. `anchor_price`
    /// stays `0` until [`crate::dormant::DormantStore::insert_or_merge`]
    /// promotes the slot.
    #[inline]
    pub fn dead(direction: Direction, zero_price_tick: i64) -> Self {
        Self {
            direction: direction as u8,
            _reserved0: [0u8; 7],
            zero_price_tick,
            anchor_price: 0,
            total_recovery_shares: 0,
            total_recovery_notional: 0,
            accrued_value: 0,
            position_count: 0,
            last_applied_index: 0,
        }
    }
}

/// On-chain representation of a single [`DistEntry`].
///
/// Total size: **64 bytes**. The on-chain ring buffer holds an array
/// of these; `max_distribution_ledger_size` controls its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct OnChainLedgerEntry {
    /// Absolute, monotonically-increasing event index.
    pub event_index: u64,
    /// Price at which the distribution was performed.
    pub p_at_event: u64,
    /// Sum of `outstanding_claim_at(p_at_event)` across activated buckets.
    pub total_outstanding_at_event: u128,
    /// Original `total_alloc` argument (numerator for replay).
    pub total_alloc_input: u128,
    /// Sum of per-bucket shares actually added to `accrued_value`.
    pub allocated_sum_observed: u128,
}

const _: [(); 64] = [(); core::mem::size_of::<OnChainLedgerEntry>()];

/// On-chain representation of [`DormantStore`]'s ledger plus the
/// per-direction aggregates the engine needs.
///
/// The Anchor account that backs this is a fixed-size buffer:
/// `header (64 bytes) + entries[max_entries] * 64 bytes`. The
/// `entries` Vec is the host-side equivalent — at runtime it never
/// exceeds `max_entries`.
///
/// ## Ring semantics
///
/// `entries[0..entry_count]` are the live, non-GC'd entries with
/// absolute indices `gc_offset .. gc_offset + entry_count`. Compaction
/// drops a prefix of `entries` and bumps `gc_offset`. The Solana
/// program implements compaction in-place by either shifting (cheap
/// at small entry_count) or by treating `entries` as a wrap-around
/// ring with a head pointer; the host reference uses `Vec::drain` and
/// the property tests in `tests/onchain_layout.rs` only compare the
/// observable fields, never the underlying physical layout.
///
/// ## Pending distribution accounting (wave 5.5)
///
/// `pending_distribution_total` tracks lazy-mode in-flight allocations
/// that have left `pool_equity` (during `distribute_lazy`) but have
/// not yet been attributed to any bucket's `accrued_value`. The
/// invariant `vault_balance == pool_equity + accrued_value_total +
/// pending_distribution_total + dust` holds **at every step** in both
/// modes once this field is wired in. See
/// `Docs/Planning/23-on-chain-dormant-bridge.md` §5 for the discovery
/// and remediation context.
#[derive(Debug, Clone)]
pub struct OnChainLedger {
    /// 0 = Long, 1 = Short.
    pub direction: u8,
    /// Reserved.
    pub _reserved0: [u8; 3],
    /// Hard cap on `entries` length; matches `MarketParams::max_distribution_ledger_size`.
    pub max_entries: u32,
    /// Number of entries GC'd from the front of the ledger.
    pub gc_offset: u64,
    /// Absolute index of the next event to be appended.
    /// Equal to `gc_offset + entry_count`.
    pub next_event_index: u64,
    /// Cached sum of `accrued_value` across every bucket in this direction.
    pub accrued_value_total: u128,
    /// Lazy-mode in-flight allocations: funds the engine routed out
    /// of `pool_equity` via `distribute_lazy` that no bucket has yet
    /// pulled into its `accrued_value` via
    /// `apply_pending_to_bucket`. Always `0` in eager mode.
    pub pending_distribution_total: u128,
    /// Live entry count (`entries.len()`).
    pub entry_count: u32,
    /// Reserved.
    pub _reserved1: [u8; 4],
    /// Live ledger entries, indices `gc_offset .. gc_offset + entry_count`.
    pub entries: Vec<OnChainLedgerEntry>,
}

impl OnChainLedger {
    /// Empty ledger header for a freshly-initialised account.
    pub fn empty(direction: Direction, max_entries: u32) -> Self {
        Self {
            direction: match direction {
                Direction::Long => 0,
                Direction::Short => 1,
            },
            _reserved0: [0u8; 3],
            max_entries,
            gc_offset: 0,
            next_event_index: 0,
            accrued_value_total: 0,
            pending_distribution_total: 0,
            entry_count: 0,
            _reserved1: [0u8; 4],
            entries: Vec::new(),
        }
    }
}

/// Pack a [`DormantStore`] into the on-chain layout.
///
/// Returns the bucket records (one per live tick, sorted ascending)
/// and the ledger header + entries.
///
/// ## Errors
///
/// * [`ClearingError::DormantBucketCapExceeded`] if the store has
///   more buckets than `max_buckets`.
/// * [`ClearingError::LedgerCapacityExceeded`] if the live ledger
///   window exceeds `max_ledger`.
pub fn pack_dormant_store(
    store: &DormantStore,
    max_buckets: u32,
    max_ledger: u32,
) -> ClearingResult<(Vec<OnChainBucketRecord>, OnChainLedger)> {
    if store.bucket_count() > max_buckets {
        return Err(ClearingError::DormantBucketCapExceeded);
    }
    if store.ledger_len() > max_ledger as usize {
        return Err(ClearingError::LedgerCapacityExceeded);
    }
    let direction_u8 = match store.direction() {
        Direction::Long => 0,
        Direction::Short => 1,
    };
    let mut buckets: Vec<OnChainBucketRecord> = store
        .iter_buckets()
        .map(|(tick, b)| OnChainBucketRecord {
            direction: direction_u8,
            _reserved0: [0u8; 7],
            zero_price_tick: *tick,
            anchor_price: b.anchor_price,
            total_recovery_shares: b.total_recovery_shares,
            total_recovery_notional: b.total_recovery_notional,
            accrued_value: b.accrued_value,
            position_count: b.position_count,
            last_applied_index: b.last_applied_index,
        })
        .collect();
    // BTreeMap iteration is already ascending, but we sort
    // defensively so the on-chain order is deterministic regardless
    // of internal storage choice.
    buckets.sort_by_key(|r| r.zero_price_tick);

    let entries: Vec<OnChainLedgerEntry> = store
        .ledger()
        .iter()
        .map(|e| OnChainLedgerEntry {
            event_index: e.event_index,
            p_at_event: e.p_at_event,
            total_outstanding_at_event: e.total_outstanding_at_event,
            total_alloc_input: e.total_alloc_input,
            allocated_sum_observed: e.allocated_sum_observed,
        })
        .collect();
    let entry_count = entries.len() as u32;
    let ledger = OnChainLedger {
        direction: direction_u8,
        _reserved0: [0u8; 3],
        max_entries: max_ledger,
        gc_offset: store.ledger_gc_offset(),
        next_event_index: store.next_event_index(),
        accrued_value_total: store.accrued_value_total(),
        pending_distribution_total: store.pending_distribution_total(),
        entry_count,
        _reserved1: [0u8; 4],
        entries,
    };
    Ok((buckets, ledger))
}

/// Unpack the on-chain layout back into a [`DormantStore`].
///
/// Validates every invariant the engine relies on. Returns
/// [`ClearingError::Invariant`] on any structural failure (these
/// MUST trip auto-pause on chain).
pub fn unpack_dormant_store(
    buckets: &[OnChainBucketRecord],
    ledger: &OnChainLedger,
) -> ClearingResult<DormantStore> {
    let direction = match ledger.direction {
        0 => Direction::Long,
        1 => Direction::Short,
        _ => return Err(ClearingError::Invariant("invalid direction byte")),
    };
    if ledger.entry_count as usize != ledger.entries.len() {
        return Err(ClearingError::Invariant("entry_count != entries.len()"));
    }
    if ledger.entry_count > ledger.max_entries {
        return Err(ClearingError::Invariant("entry_count > max_entries"));
    }
    // The entries must be a contiguous window starting at gc_offset.
    for (i, e) in ledger.entries.iter().enumerate() {
        let expected = ledger
            .gc_offset
            .checked_add(i as u64)
            .ok_or(ClearingError::MathOverflow)?;
        if e.event_index != expected {
            return Err(ClearingError::Invariant(
                "ledger.entries[i].event_index != gc_offset + i",
            ));
        }
    }
    let mut bucket_pairs: Vec<(i64, DormantBucket)> = Vec::with_capacity(buckets.len());
    for b in buckets {
        if b.direction != ledger.direction {
            return Err(ClearingError::Invariant(
                "bucket direction != ledger direction",
            ));
        }
        // Wave 7.2 / Wave 8: silently skip dead PDA slots — see
        // [`OnChainBucketRecord::is_dead`] for the full rationale.
        // Loading them into the engine would cause
        // `DormantStore::insert_or_merge` to hit the "bucket already
        // exists" branch, which never updates `anchor_price`,
        // permanently bricking the bucket. The bridge
        // (`dormant_bridge::unpack_direction`) does the same skip on
        // the Anchor side; this keeps the host-side
        // `unpack_dormant_store` byte-equivalent to the on-chain
        // unpack.
        if b.is_dead() {
            continue;
        }
        bucket_pairs.push((
            b.zero_price_tick,
            DormantBucket {
                direction,
                zero_price_tick: b.zero_price_tick,
                anchor_price: b.anchor_price,
                total_recovery_shares: b.total_recovery_shares,
                total_recovery_notional: b.total_recovery_notional,
                accrued_value: b.accrued_value,
                position_count: b.position_count,
                last_applied_index: b.last_applied_index,
            },
        ));
    }
    let entries: Vec<DistEntry> = ledger
        .entries
        .iter()
        .map(|e| DistEntry {
            event_index: e.event_index,
            p_at_event: e.p_at_event,
            total_outstanding_at_event: e.total_outstanding_at_event,
            total_alloc_input: e.total_alloc_input,
            allocated_sum_observed: e.allocated_sum_observed,
        })
        .collect();
    DormantStore::from_onchain_parts(
        direction,
        bucket_pairs,
        entries,
        ledger.gc_offset,
        ledger.next_event_index,
        ledger.accrued_value_total,
        ledger.pending_distribution_total,
    )
}

#[cfg(test)]
mod size_tests {
    use super::*;

    #[test]
    fn bucket_record_is_96_bytes() {
        assert_eq!(core::mem::size_of::<OnChainBucketRecord>(), 96);
    }

    #[test]
    fn ledger_entry_is_64_bytes() {
        assert_eq!(core::mem::size_of::<OnChainLedgerEntry>(), 64);
    }

    #[test]
    fn round_trip_empty_store() {
        let store = DormantStore::new(Direction::Long);
        let (buckets, ledger) = pack_dormant_store(&store, 1024, 1024).unwrap();
        assert!(buckets.is_empty());
        assert_eq!(ledger.entry_count, 0);
        assert_eq!(ledger.gc_offset, 0);
        assert_eq!(ledger.next_event_index, 0);

        let restored = unpack_dormant_store(&buckets, &ledger).unwrap();
        assert_eq!(restored.bucket_count(), 0);
        assert_eq!(restored.ledger_len(), 0);
        assert_eq!(restored.next_event_index(), 0);
    }
}
