//! Dormant store ↔ Anchor account bridge.
//!
//! The host engine ([`clearing_core`]) treats a per-direction dormant
//! store as an in-memory [`clearing_core::DormantStore`]. The on-chain
//! representation splits the same data across two account *kinds*:
//!
//!   * One [`DistributionLedger`] PDA per `(sub_pool, direction)`,
//!     storing the ring buffer + scalar aggregates (`gc_offset`,
//!     `next_event_index`, `accrued_value_total`,
//!     `pending_distribution_total`).
//!   * Zero or more [`DormantBucket`] PDAs, one per *live* bucket
//!     `(sub_pool, direction, bucket_tick)`.
//!
//! This module is the single bridge between the two:
//!
//!   * [`unpack_direction`] reads the ledger PDA + a slice of bucket
//!     PDAs into a `DormantStore` the engine can mutate.
//!   * [`pack_direction`] flushes the mutated `DormantStore` back into
//!     the same accounts. Buckets that the engine removed are
//!     zeroed-out; buckets the engine left in place have their fields
//!     overwritten byte-for-byte.
//!
//! ## Account layout contract
//!
//! Each instruction handler that needs to drain or update dormant
//! state passes through `ctx.remaining_accounts` in the following
//! order:
//!
//! ```text
//!   [0]                     DistributionLedger long  (mut)  [optional]
//!   [1]                     DistributionLedger short (mut)  [optional]
//!   [2 .. 2+L]              DormantBucket long   PDAs (mut)
//!   [2+L .. 2+L+S]          DormantBucket short  PDAs (mut)
//! ```
//!
//! The exact slice each handler needs is documented per-handler. This
//! module exposes [`DormantHandle`] which the handler builds once and
//! then asks for `unpack_direction(Direction::Long)` etc.
//!
//! ## Why not store everything in `SubPool`?
//!
//! Solana account size limits make a single `SubPool` PDA holding
//! every bucket impossible at scale. The split also enables
//! `pre_sync_dormant_bucket` to operate on a single bucket inside a
//! single tx — bounded CU, regardless of how many live buckets the
//! direction has accumulated. See
//! `Docs/Planning/23-on-chain-dormant-bridge.md` for the full design
//! rationale.

use anchor_lang::prelude::*;

use clearing_core::onchain::{
    pack_dormant_store, unpack_dormant_store, OnChainBucketRecord, OnChainLedger,
    OnChainLedgerEntry,
};
use clearing_core::{ClearingError, Direction, DormantStore};

use crate::error::{map_err, ProgramError};
use crate::state::{
    DistEntryPacked, DistributionLedger, DormantBucket, SubPool as SubPoolAccount,
};

use super::sync::{apply_clearing_view, clearing_view};

/// Return code for [`pack_direction`]: which buckets were removed (so
/// the handler can zero them or leave them inert) and which were
/// touched.
#[derive(Debug, Default, Clone)]
pub struct PackOutcome {
    /// Bucket-account indices (within the slice the handler passed
    /// in) whose engine-side bucket was deleted. The handler MAY
    /// zero / close these accounts in a follow-up step.
    pub removed_indices: Vec<usize>,
    /// Number of buckets the engine wrote.
    pub written_count: usize,
}

/// Predicate: a `DormantBucket` PDA is **dead** iff every observable
/// the engine cares about is zero — no recovery shares, no recovery
/// notional, no accrued value, and no positions hooked to it.
///
/// Dead PDAs are intentionally allowed in the [`unpack_direction`]
/// account slice because of the keeper-side workflow:
///
///   1. The engine's `insert_or_merge` produces a new bucket at tick
///      `T`. `pack_direction` Pass 2 needs an empty PDA slot to
///      land it; if no slot is available the tx reverts with
///      `DormantBridgeBucketSlotExhausted`.
///   2. The keeper observes the revert (or pre-simulates the same op)
///      and calls `initialize_dormant_bucket(T)` to materialise an
///      empty PDA at the right seed.
///   3. The next sync_pool / close / claim tx includes that empty
///      PDA in `remaining_accounts`. **Crucially**, [`unpack_direction`]
///      MUST skip dead PDAs — feeding a zero-`anchor_price` bucket
///      into the engine would taint the store: `insert_or_merge`'s
///      already-exists branch (`self.buckets.contains_key(&tick)`
///      true) does NOT update `anchor_price`, leaving the bucket
///      permanently unactivatable.
///
/// `pack_direction` independently identifies dead slots via the same
/// predicate (Pass 2's `dead` check) so the engine's new record can
/// land in the freshly-init'd PDA on the next pack.
pub fn record_is_dead(b: &DormantBucket) -> bool {
    b.total_recovery_shares == 0
        && b.total_recovery_notional == 0
        && b.accrued_value == 0
        && b.position_count == 0
}

/// Unpack a per-direction `DormantStore` from the on-chain ledger PDA
/// plus the live-bucket PDA slice.
///
/// The caller must ensure:
///   * `ledger.sub_pool == sub_pool_key`
///   * `ledger.direction_is_long == matches!(direction, Direction::Long)`
///   * Every bucket account's `sub_pool` matches `sub_pool_key` and
///     `direction_is_long` matches `direction`.
///   * `bucket_accounts` are sorted by `zero_price_tick` ascending —
///     not load-bearing for the engine but matches `pack_direction`'s
///     output ordering, which is what the indexer / event stream
///     consumers expect.
///
/// Bucket accounts that pass [`record_is_dead`] are **silently
/// skipped**; see that function's doc-comment for the rationale.
///
/// Returns the engine-ready store and re-checks all of
/// `clearing_core::DormantStore::check_invariants` on the result.
pub fn unpack_direction<'info>(
    ledger: &Account<'info, DistributionLedger>,
    bucket_accounts: &[Account<'info, DormantBucket>],
    sub_pool_key: Pubkey,
    direction: Direction,
) -> Result<DormantStore> {
    require_eq!(
        ledger.sub_pool,
        sub_pool_key,
        ProgramError::DormantBridgeAccountMismatch
    );
    let direction_is_long = matches!(direction, Direction::Long);
    require_eq!(
        ledger.direction_is_long,
        direction_is_long,
        ProgramError::DormantBridgeAccountMismatch
    );

    let mut bucket_records: Vec<OnChainBucketRecord> = Vec::with_capacity(bucket_accounts.len());
    let direction_byte: u8 = if direction_is_long { 0 } else { 1 };
    for b in bucket_accounts {
        require_eq!(
            b.sub_pool,
            sub_pool_key,
            ProgramError::DormantBridgeAccountMismatch
        );
        require_eq!(
            b.direction_is_long,
            direction_is_long,
            ProgramError::DormantBridgeAccountMismatch
        );
        if record_is_dead(b) {
            // Pre-init'd empty slot, or freshly redeem-emptied bucket.
            // The engine MUST NOT see it — see `record_is_dead`.
            continue;
        }
        bucket_records.push(OnChainBucketRecord {
            direction: direction_byte,
            _reserved0: [0u8; 7],
            zero_price_tick: b.zero_price_tick,
            anchor_price: b.anchor_price,
            total_recovery_shares: b.total_recovery_shares,
            total_recovery_notional: b.total_recovery_notional,
            accrued_value: b.accrued_value,
            position_count: b.position_count,
            last_applied_index: b.last_applied_index,
        });
    }

    let entries: Vec<OnChainLedgerEntry> = ledger
        .entries
        .iter()
        .map(|e| OnChainLedgerEntry {
            event_index: e.event_index,
            p_at_event: e.p_at_event,
            total_outstanding_at_event: e.total_outstanding_at_event,
            total_alloc_input: e.total_alloc_input,
            allocated_sum_observed: e.allocated_sum_observed,
        })
        .collect();

    let on_chain_ledger = OnChainLedger {
        direction: direction_byte,
        _reserved0: [0u8; 3],
        max_entries: ledger.max_entries,
        gc_offset: ledger.gc_offset,
        next_event_index: ledger.next_event_index,
        accrued_value_total: ledger.accrued_value_total,
        pending_distribution_total: ledger.pending_distribution_total,
        entry_count: ledger.entry_count,
        _reserved1: [0u8; 4],
        entries,
    };

    let store = unpack_dormant_store(&bucket_records, &on_chain_ledger).map_err(map_err)?;
    store.check_invariants().map_err(map_err)?;
    Ok(store)
}

/// Flush a mutated `DormantStore` back into the ledger PDA + bucket
/// PDA slice. The caller is responsible for passing the **same**
/// account slice it gave to [`unpack_direction`]; engine mutations
/// must never grow or shrink the bucket count beyond what the caller
/// has accounts for.
///
/// Buckets the engine deleted (e.g. via `redeem` to zero balance)
/// have their account fields **zeroed** here; the handler's caller
/// is then free to close those accounts (refund rent) in a separate
/// admin tx — we do NOT close them here because that would change
/// `account_infos` from the engine's point of view inside a single
/// instruction.
///
/// Engine-created buckets (via `insert_or_merge`) require the caller
/// to have pre-allocated a free `DormantBucket` PDA in
/// `bucket_accounts` and passed it in. If the engine grows the
/// bucket count beyond `bucket_accounts.len()`, this function
/// returns
/// [`ProgramError::DormantBridgeBucketSlotExhausted`] — the
/// instruction reverts and the caller must retry with a fresh
/// PDA slot pre-initialised via `init_dormant_bucket`.
pub fn pack_direction<'info>(
    ledger: &mut Account<'info, DistributionLedger>,
    bucket_accounts: &mut [Account<'info, DormantBucket>],
    sub_pool_key: Pubkey,
    direction: Direction,
    store: &DormantStore,
    max_buckets: u32,
    max_ledger: u32,
) -> Result<PackOutcome> {
    let direction_is_long = matches!(direction, Direction::Long);
    let direction_byte: u8 = if direction_is_long { 0 } else { 1 };
    let (records, on_chain_ledger) =
        pack_dormant_store(store, max_buckets, max_ledger).map_err(map_err)?;

    if records.len() > bucket_accounts.len() {
        return Err(error!(ProgramError::DormantBridgeBucketSlotExhausted));
    }

    // Index existing accounts by `(zero_price_tick)` so we can either
    // (a) overwrite a bucket whose tick stays the same, or (b)
    // overwrite a previously-emptied slot to hold a new bucket. We
    // explicitly DON'T mutate accounts whose tick disappears from the
    // store — those are reported via `removed_indices` and zeroed.
    let mut available_slots: Vec<usize> = (0..bucket_accounts.len()).collect();
    let mut written_count = 0usize;
    let mut removed_indices: Vec<usize> = Vec::new();

    // Pass 1: match each engine record to the existing account with
    // the same zero_price_tick (if any). "Live" matches the same
    // predicate `record_is_dead` negates so dead slots cannot
    // accidentally win a match against a stale tick value.
    for record in &records {
        let mut matched: Option<usize> = None;
        for &idx in &available_slots {
            let acct = &bucket_accounts[idx];
            if !record_is_dead(acct)
                && acct.zero_price_tick == record.zero_price_tick
                && acct.direction_is_long == direction_is_long
            {
                matched = Some(idx);
                break;
            }
        }
        if let Some(idx) = matched {
            apply_record(&mut bucket_accounts[idx], sub_pool_key, record);
            available_slots.retain(|&i| i != idx);
            written_count += 1;
        }
    }

    // Pass 2: any record we haven't placed yet goes into a free slot
    // (one that is currently "dead" — every observable is zero).
    'outer: for record in &records {
        // Skip records that already landed in pass 1.
        let already_placed = bucket_accounts
            .iter()
            .any(|a| a.zero_price_tick == record.zero_price_tick && a.anchor_price == record.anchor_price && a.last_applied_index == record.last_applied_index);
        if already_placed {
            continue;
        }
        for (i, &idx) in available_slots.iter().enumerate() {
            if record_is_dead(&bucket_accounts[idx]) {
                apply_record(&mut bucket_accounts[idx], sub_pool_key, record);
                available_slots.remove(i);
                written_count += 1;
                continue 'outer;
            }
        }
        // No free slot — every passed-in account is occupied by a
        // different live bucket. The engine produced more buckets
        // than the handler reserved space for. Surface the
        // explicit error so the caller knows to retry with a fresh
        // pre-init'd PDA.
        return Err(error!(ProgramError::DormantBridgeBucketSlotExhausted));
    }

    // Pass 3: any leftover slot held a live bucket the engine has
    // since deleted. Zero it so the next unpack ignores it.
    for &idx in &available_slots {
        let acct = &mut bucket_accounts[idx];
        if !record_is_dead(acct) {
            // Preserve identity fields (sub_pool, direction, tick,
            // bump) so the same PDA can later be re-used by a
            // future insert_or_merge — Solana doesn't allow
            // changing a PDA's seeds without `close + reinit`. The
            // engine-observable fields all go to zero.
            acct.total_recovery_shares = 0;
            acct.total_recovery_notional = 0;
            acct.accrued_value = 0;
            acct.position_count = 0;
            acct.last_applied_index = on_chain_ledger.next_event_index;
            removed_indices.push(idx);
        }
    }

    // Flush ledger header.
    ledger.gc_offset = on_chain_ledger.gc_offset;
    ledger.next_event_index = on_chain_ledger.next_event_index;
    ledger.accrued_value_total = on_chain_ledger.accrued_value_total;
    ledger.pending_distribution_total = on_chain_ledger.pending_distribution_total;
    ledger.entry_count = on_chain_ledger.entry_count;
    ledger.entries = on_chain_ledger
        .entries
        .iter()
        .map(|e| DistEntryPacked {
            event_index: e.event_index,
            p_at_event: e.p_at_event,
            total_outstanding_at_event: e.total_outstanding_at_event,
            total_alloc_input: e.total_alloc_input,
            allocated_sum_observed: e.allocated_sum_observed,
        })
        .collect();
    let _ = direction_byte; // direction is implicit in `direction_is_long`

    Ok(PackOutcome {
        removed_indices,
        written_count,
    })
}

/// Overwrite an existing `DormantBucket` account with a new record.
fn apply_record<'info>(
    acct: &mut Account<'info, DormantBucket>,
    sub_pool_key: Pubkey,
    record: &OnChainBucketRecord,
) {
    // Identity fields are pinned by the PDA's seeds (sub_pool +
    // direction + tick) and are set once by `init_dormant_bucket`. We
    // re-write `sub_pool` defensively in case the slot was
    // re-used by a later allocation under a different seed scheme.
    acct.sub_pool = sub_pool_key;
    acct.direction_is_long = record.direction == 0;
    acct.zero_price_tick = record.zero_price_tick;
    acct.anchor_price = record.anchor_price;
    acct.total_recovery_shares = record.total_recovery_shares;
    acct.total_recovery_notional = record.total_recovery_notional;
    acct.accrued_value = record.accrued_value;
    acct.position_count = record.position_count;
    acct.last_applied_index = record.last_applied_index;
}

/// Convenience: `ClearingError::Invariant` shorthand for the
/// per-handler "you forgot to pass the long ledger" path. Use this
/// when the handler determines via remaining_accounts decoding that
/// the wrong number of accounts were provided, before going through
/// the full unpack.
pub fn missing_account() -> ClearingError {
    ClearingError::Invariant("dormant bridge: required account missing")
}

/// Two-direction split of the bucket-PDA slice produced by
/// [`split_remaining_buckets`]. `long` and `short` keep their order
/// of appearance in `remaining_accounts`, which the engine's
/// `pack_dormant_store` relies on for deterministic placement.
pub type BucketSplit<'info> = (
    Vec<Account<'info, DormantBucket>>,
    Vec<Account<'info, DormantBucket>>,
);

/// Decode `remaining_accounts` into `(long_buckets, short_buckets)`
/// according to the Wave 6 contract: the first
/// `long_bucket_count` AccountInfos are long-side `DormantBucket`
/// PDAs, the rest are short-side. Used by every handler that runs
/// the engine's `sync_pool` transitively (which can mutate any live
/// bucket via rotation / lazy distribute).
pub fn split_remaining_buckets<'info>(
    remaining_accounts: &'info [AccountInfo<'info>],
    long_bucket_count: u32,
    short_bucket_count: u32,
) -> Result<BucketSplit<'info>> {
    let expected = (long_bucket_count as usize) + (short_bucket_count as usize);
    require_eq!(
        remaining_accounts.len(),
        expected,
        ProgramError::DormantBridgeAccountMismatch
    );
    let mut longs: Vec<Account<DormantBucket>> = Vec::with_capacity(long_bucket_count as usize);
    for info in &remaining_accounts[..long_bucket_count as usize] {
        longs.push(Account::try_from(info)?);
    }
    let mut shorts: Vec<Account<DormantBucket>> =
        Vec::with_capacity(short_bucket_count as usize);
    for info in &remaining_accounts[long_bucket_count as usize..] {
        shorts.push(Account::try_from(info)?);
    }
    Ok((longs, shorts))
}

/// Force-flush every passed-through bucket Account back to its
/// underlying buffer. Required because `Account::try_from(info)`
/// reborrows the account; Anchor only auto-flushes accounts declared
/// as named fields in `#[derive(Accounts)]`.
///
/// MUST be called after [`pack_direction`] writes per-bucket
/// mutations and before the instruction returns. Skipping this leaves
/// the buckets' on-chain bytes stale, silently breaking parity with
/// `chain-mirror` and the indexer.
pub fn exit_all_buckets<'info>(
    program_id: &Pubkey,
    long_buckets: &[Account<'info, DormantBucket>],
    short_buckets: &[Account<'info, DormantBucket>],
) -> Result<()> {
    for b in long_buckets.iter() {
        b.exit(program_id)?;
    }
    for b in short_buckets.iter() {
        b.exit(program_id)?;
    }
    Ok(())
}

/// Run a `clearing_core` engine call inside the dormant bridge.
///
/// This is the single entrypoint every Wave 6 / Wave 7 handler
/// (`sync_pool`, `close_position`, `force_close_zero_value_position`,
/// `claim_dormant_recovery`, `pre_sync_dormant_bucket`) goes through:
///
/// 1. Decode `remaining_accounts` into `(long_buckets, short_buckets)`.
/// 2. Build a `clearing_core::SubPool` view via [`clearing_view`].
/// 3. Hydrate `sp_view.{long,short}_dormant` from the ledger PDAs +
///    bucket PDAs via [`unpack_direction`] (both directions, even
///    when a handler logically touches only one side — the engine's
///    cross-direction invariant check
///    [`clearing_core::invariants::check_subpool_invariants`] needs
///    BOTH stores populated).
/// 4. Invoke `engine_call` with `(market_params, &mut sp_view)`.
/// 5. Flush both stores back via [`pack_direction`].
/// 6. Persist every bucket account via [`exit_all_buckets`].
/// 7. Mirror engine-side scalar updates back to `sub_pool` via
///    [`apply_clearing_view`].
///
/// On any `Err` the function returns immediately; Solana's transaction-
/// revert semantics undo all account writes from steps 5–7. The
/// `clearing_core::SubPool` allocated on the stack is dropped without
/// leaking account state.
///
/// ### Why a single helper
///
/// Each handler used to repeat steps 1–7 verbatim — 50 lines of
/// boilerplate per handler × 5 handlers = 250 lines of duplicated
/// bridging logic. A single bug (e.g. forgetting to call
/// `exit_all_buckets`, or swapping long/short ledger accounts) would
/// trip independently in 5 places. Centralising the flow here ensures
/// any future bridge change (e.g. adding a new pre-pack invariant
/// check) lands in exactly one place.
///
/// ### Borrow safety
///
/// Anchor's `Context` exposes `accounts` as a struct of disjoint
/// fields, so the caller may freely pass `&mut ctx.accounts.sub_pool`,
/// `&mut ctx.accounts.long_ledger`, `&mut ctx.accounts.short_ledger`,
/// `ctx.remaining_accounts`, and `ctx.program_id` simultaneously —
/// the borrow checker recognises these as non-overlapping. Inside
/// `engine_call`, callers can move locals (e.g. `envelope`) and
/// borrow other locals mutably (e.g. `&mut core_pos`); the closure is
/// `FnOnce`, so it can consume non-`Copy` captures.
#[allow(clippy::too_many_arguments)]
pub fn run_bridged<'info, F, R>(
    sub_pool: &mut Account<'info, SubPoolAccount>,
    long_ledger: &mut Account<'info, DistributionLedger>,
    short_ledger: &mut Account<'info, DistributionLedger>,
    market_params: &clearing_core::MarketParams,
    program_id: &Pubkey,
    remaining_accounts: &'info [AccountInfo<'info>],
    long_bucket_count: u32,
    short_bucket_count: u32,
    engine_call: F,
) -> Result<R>
where
    F: FnOnce(&clearing_core::MarketParams, &mut clearing_core::SubPool) -> Result<R>,
{
    let (mut long_buckets, mut short_buckets) =
        split_remaining_buckets(remaining_accounts, long_bucket_count, short_bucket_count)?;
    let sub_pool_key = sub_pool.key();

    let mut sp_view = clearing_view(sub_pool);
    sp_view.long_dormant =
        unpack_direction(long_ledger, &long_buckets, sub_pool_key, Direction::Long)?;
    sp_view.short_dormant =
        unpack_direction(short_ledger, &short_buckets, sub_pool_key, Direction::Short)?;

    let outcome = engine_call(market_params, &mut sp_view)?;

    pack_direction(
        long_ledger,
        &mut long_buckets,
        sub_pool_key,
        Direction::Long,
        &sp_view.long_dormant,
        market_params.max_dormant_bucket_count_per_direction,
        market_params.max_distribution_ledger_size,
    )?;
    pack_direction(
        short_ledger,
        &mut short_buckets,
        sub_pool_key,
        Direction::Short,
        &sp_view.short_dormant,
        market_params.max_dormant_bucket_count_per_direction,
        market_params.max_distribution_ledger_size,
    )?;
    exit_all_buckets(program_id, &long_buckets, &short_buckets)?;
    apply_clearing_view(sub_pool, &sp_view);

    Ok(outcome)
}
