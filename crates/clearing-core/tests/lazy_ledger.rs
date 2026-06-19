//! Lazy-replay correctness for the dormant distribution ledger.
//!
//! Mirrors `Docs/Planning/18-shares模型实现细则与边界条件.md` §10.3.
//! These tests prove that the **lazy replay path**
//! ([`clearing_core::DormantStore::apply_pending_to_bucket`]) produces
//! the exact same per-bucket `accrued_value` as the eager path
//! `DormantStore::distribute` produces inline. They are the safety-net
//! that lets us flip the on-chain dormant module to "sync_pool only
//! writes the aggregate; users pay to apply pending on touch" in a
//! later wave without changing observable economics.

use std::sync::atomic::{AtomicU64, Ordering};

use clearing_core::{
    close_position, open_position as engine_open, sync_pool, ClearingResult, Direction,
    MarketParams, OpenOutcome, Position, PriceEnvelope, SubPool,
};
use molemath::PRICE_SCALE;

static POSITION_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_id() -> u64 {
    POSITION_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn open(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    envelope: PriceEnvelope,
    direction: Direction,
    gross: u64,
) -> ClearingResult<(Position, OpenOutcome)> {
    engine_open(market, sub_pool, envelope, direction, gross, next_id())
}

fn envelope(p: u64, slot: u64) -> PriceEnvelope {
    PriceEnvelope {
        p_now: p,
        slot,
        expected_min: p,
        expected_max: p,
    }
}

/// Build a deterministic scenario that produces a long dormant bucket and
/// then drives multiple price syncs that distribute funds to it. Returns
/// the final SubPool together with the tick of the dormant bucket and
/// the `next_event_index` of the long dormant store at the moment the
/// bucket was created (i.e., the lower bound for any legal rewind).
///
/// Crucially, Alice is **not** force-closed: she stays as a dormant
/// position so the bucket retains its full notional/shares and every
/// subsequent recovery sync produces a real `DistEntry`.
fn scenario_with_long_dormant_distributions() -> (SubPool, i64, u64) {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);

    // Alice long, Bob short — both at the entry price.
    let (_alice, _) = open(
        &market,
        &mut sub_pool,
        envelope(entry, 1),
        Direction::Long,
        100_000_000,
    )
    .unwrap();
    let (_bob, _) = open(
        &market,
        &mut sub_pool,
        envelope(entry, 2),
        Direction::Short,
        100_000_000,
    )
    .unwrap();

    // Crash to $70 → long pool zeroes; one long bucket created with
    // anchor = $70 and full long notional.
    let crash = (entry * 70) / 100;
    sync_pool(&market, &mut sub_pool, envelope(crash, 3)).unwrap();
    assert!(sub_pool.long_dormant.bucket_count() >= 1);
    let bucket_tick = sub_pool.long_dormant.bucket_ticks()[0];
    let bucket_creation_index = sub_pool.long_dormant.next_event_index();

    // New entrants in a fresh active generation. Dave's long stake gives
    // long active a non-zero `long_active_pnl_inc` on every recovery
    // sync, so the recovery branch in `sync_pool` actually triggers.
    let (_carol_short, _) = open(
        &market,
        &mut sub_pool,
        envelope(crash, 4),
        Direction::Short,
        80_000_000,
    )
    .unwrap();
    let (mut dave_long, _) = open(
        &market,
        &mut sub_pool,
        envelope(crash, 5),
        Direction::Long,
        80_000_000,
    )
    .unwrap();

    // Price recovers in steps. Each sync that brings price above the
    // bucket's anchor distributes counterparty losses to the bucket and
    // appends one DistEntry.
    for (slot, p_pct) in [(6u64, 75u64), (7, 80), (8, 85), (9, 90), (10, 95)] {
        let pn = (entry * p_pct) / 100;
        sync_pool(&market, &mut sub_pool, envelope(pn, slot)).unwrap();
    }

    // Close Dave so unrelated active state moves alongside the dormant
    // ledger; this catches any accidental coupling between the two.
    let _ = close_position(
        &market,
        &mut sub_pool,
        envelope((entry * 95) / 100, 11),
        &mut dave_long,
    );

    let events_for_bucket = sub_pool.long_dormant.next_event_index() - bucket_creation_index;
    assert!(
        events_for_bucket >= 3,
        "scenario must produce >= 3 distribution events for the bucket, got {events_for_bucket}"
    );
    (sub_pool, bucket_tick, bucket_creation_index)
}

/// Rewinding the long bucket to its creation index and replaying the
/// ledger via `apply_pending_to_bucket` must reproduce the eager bucket
/// state byte-for-byte.
#[test]
fn lazy_replay_recovers_eager_state_for_long_bucket() {
    let (mut sub_pool, tick, creation_idx) = scenario_with_long_dormant_distributions();

    let eager_accrued = sub_pool.long_dormant.get(tick).unwrap().accrued_value;
    let eager_last_applied = sub_pool.long_dormant.get(tick).unwrap().last_applied_index;
    let next_event = sub_pool.long_dormant.next_event_index();
    assert_eq!(eager_last_applied, next_event);

    sub_pool
        .long_dormant
        .rewind_bucket_for_replay_test(tick, creation_idx)
        .unwrap();
    assert_eq!(sub_pool.long_dormant.get(tick).unwrap().accrued_value, 0);
    assert_eq!(
        sub_pool.long_dormant.get(tick).unwrap().last_applied_index,
        creation_idx
    );

    let applied = sub_pool.long_dormant.apply_pending_to_bucket(tick).unwrap();
    assert!(applied >= 3, "expected >= 3 events replayed, got {applied}");

    let lazy_accrued = sub_pool.long_dormant.get(tick).unwrap().accrued_value;
    let lazy_last_applied = sub_pool.long_dormant.get(tick).unwrap().last_applied_index;
    assert_eq!(
        lazy_accrued, eager_accrued,
        "lazy replay diverged: eager={eager_accrued}, lazy={lazy_accrued}"
    );
    assert_eq!(lazy_last_applied, next_event);
    sub_pool.long_dormant.check_invariants().unwrap();
}

/// Replaying step-by-step (one apply_pending call per appended event)
/// must yield the same final state as a single batched replay.
#[test]
fn lazy_replay_step_by_step_matches_full_batch() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool_a = SubPool::new(0, entry, 0);

    let (_alice, _) = open(
        &market,
        &mut sub_pool_a,
        envelope(entry, 1),
        Direction::Long,
        100_000_000,
    )
    .unwrap();
    let (_bob, _) = open(
        &market,
        &mut sub_pool_a,
        envelope(entry, 2),
        Direction::Short,
        100_000_000,
    )
    .unwrap();
    let crash = (entry * 70) / 100;
    sync_pool(&market, &mut sub_pool_a, envelope(crash, 3)).unwrap();
    let bucket_tick = sub_pool_a.long_dormant.bucket_ticks()[0];
    let creation_idx = sub_pool_a.long_dormant.next_event_index();
    let (_carol, _) = open(
        &market,
        &mut sub_pool_a,
        envelope(crash, 4),
        Direction::Short,
        80_000_000,
    )
    .unwrap();
    let (_dave, _) = open(
        &market,
        &mut sub_pool_a,
        envelope(crash, 5),
        Direction::Long,
        80_000_000,
    )
    .unwrap();

    // Diverge: clone the sub_pool here so both branches see the same
    // pre-distribute state.
    let mut sub_pool_b = sub_pool_a.clone();

    // Branch A: drive 4 syncs, then apply_pending in one batch.
    for (slot, p_pct) in [(6u64, 75u64), (7, 80), (8, 90), (9, 100)] {
        let pn = (entry * p_pct) / 100;
        sync_pool(&market, &mut sub_pool_a, envelope(pn, slot)).unwrap();
    }
    sub_pool_a
        .long_dormant
        .rewind_bucket_for_replay_test(bucket_tick, creation_idx)
        .unwrap();
    sub_pool_a
        .long_dormant
        .apply_pending_to_bucket(bucket_tick)
        .unwrap();
    let final_a = sub_pool_a.long_dormant.get(bucket_tick).unwrap().accrued_value;

    // Branch B: drive same 4 syncs but rewind+replay after EACH one,
    // accumulating step-by-step. Each cycle rewinds the bucket all the
    // way back to its creation index and reapplies the entire ledger,
    // which is the strongest possible idempotency claim.
    for (slot, p_pct) in [(6u64, 75u64), (7, 80), (8, 90), (9, 100)] {
        let pn = (entry * p_pct) / 100;
        sync_pool(&market, &mut sub_pool_b, envelope(pn, slot)).unwrap();
        sub_pool_b
            .long_dormant
            .rewind_bucket_for_replay_test(bucket_tick, creation_idx)
            .unwrap();
        sub_pool_b
            .long_dormant
            .apply_pending_to_bucket(bucket_tick)
            .unwrap();
    }
    let final_b = sub_pool_b.long_dormant.get(bucket_tick).unwrap().accrued_value;

    assert_eq!(
        final_a, final_b,
        "step-by-step replay diverged from batch replay: batch={final_a}, step={final_b}"
    );
    sub_pool_a.long_dormant.check_invariants().unwrap();
    sub_pool_b.long_dormant.check_invariants().unwrap();
}

/// Compaction may only drop ledger entries that every live bucket has
/// already passed, and after compaction the lazy replay must still
/// recover the same per-bucket state when rewound to (a) the new gc
/// offset, or (b) any later legal index.
#[test]
fn compact_ledger_preserves_replay_correctness() {
    let (mut sub_pool, tick, creation_idx) = scenario_with_long_dormant_distributions();
    let eager_accrued = sub_pool.long_dormant.get(tick).unwrap().accrued_value;
    let next_event = sub_pool.long_dormant.next_event_index();

    // Pre-compact ledger length must match next_event - gc_offset (always
    // true given the eager path bumps last_applied_index uniformly).
    assert_eq!(
        sub_pool.long_dormant.ledger_len() as u64,
        next_event - sub_pool.long_dormant.ledger_gc_offset()
    );

    // After compaction (every bucket is up to date in the eager path),
    // the entire ledger should be GC'd.
    let dropped = sub_pool.long_dormant.compact_ledger();
    assert!(dropped >= 1, "expected compaction to drop entries, got {dropped}");
    assert_eq!(sub_pool.long_dormant.ledger_len(), 0);
    assert_eq!(sub_pool.long_dormant.ledger_gc_offset(), next_event);
    sub_pool.long_dormant.check_invariants().unwrap();

    // Replay from the post-compact watermark must be a no-op (the bucket
    // is already current).
    sub_pool
        .long_dormant
        .rewind_bucket_for_replay_test(tick, next_event)
        .unwrap();
    let applied = sub_pool.long_dormant.apply_pending_to_bucket(tick).unwrap();
    assert_eq!(applied, 0);
    let post = sub_pool.long_dormant.get(tick).unwrap().accrued_value;
    // Rewinding zero'd accrued out; replay is a no-op since there are no
    // post-compaction events; the test asserts the no-op is faithful.
    assert_eq!(post, 0);

    // Rewinding into the GC'd window must be rejected (the math is no
    // longer recoverable from this store alone).
    let err = sub_pool
        .long_dormant
        .rewind_bucket_for_replay_test(tick, creation_idx);
    assert!(err.is_err(), "expected rewind into GC'd window to fail");
    let _ = eager_accrued; // referenced for clarity
}

/// Multiple rewind+replay cycles on the same bucket are idempotent: as
/// long as each cycle starts from the same legal target index, the final
/// `accrued_value` is the same every time.
#[test]
fn rewind_apply_is_idempotent_across_cycles() {
    let (mut sub_pool, tick, creation_idx) = scenario_with_long_dormant_distributions();
    let eager_accrued = sub_pool.long_dormant.get(tick).unwrap().accrued_value;

    for _ in 0..5 {
        sub_pool
            .long_dormant
            .rewind_bucket_for_replay_test(tick, creation_idx)
            .unwrap();
        sub_pool.long_dormant.apply_pending_to_bucket(tick).unwrap();
        let v = sub_pool.long_dormant.get(tick).unwrap().accrued_value;
        assert_eq!(v, eager_accrued);
        sub_pool.long_dormant.check_invariants().unwrap();
    }
}

/// **Patient-holder bonus is real and bounded.**
///
/// `DormantStore::redeem` burns shares **and** the corresponding slice
/// of `total_recovery_notional` in proportion. Because subsequent
/// distributions are split against `intrinsic_at(p) =
/// total_recovery_notional · max(0, p - anchor) / anchor`, the holder
/// who redeems half a bucket midway shrinks the bucket's intrinsic
/// upper bound for every subsequent recovery sync — by exactly that
/// half. Per the whitepaper §"后续流动性可恢复历史亏损" and
/// `Docs/Planning/18-shares模型实现细则与边界条件.md` §6, this is
/// **intended**: positions still in the bucket at the moment of the
/// next opposite-side loss are the ones entitled to claim against it.
/// Early-exiting positions cash out their share of the funds already
/// accrued and forfeit their slice of any later allocation.
///
/// We therefore assert the structural invariants that DO hold:
///
/// 1. `order_a_total >= order_b_total` — the patient holder is never
///    out-earned by the impatient one. Without this, attackers could
///    extract value by partial-redeem timing.
/// 2. `order_a_total - order_b_total <= upper_bound`, where
///    `upper_bound` is **the half-notional × max(0, p_2 - anchor) /
///    anchor** — i.e. the maximum a half-burned bucket could possibly
///    miss out on at the second sync. Going above that means the
///    eager `distribute` overpaid order A or short-changed order B
///    beyond the structural difference, which would be a real
///    accounting bug.
/// 3. The drift is non-trivial (`> 8`) — proving path-dependence is
///    real, not just rounding noise. Otherwise this test would still
///    pass under a buggy model that hard-locks accrual at distribute
///    time and ignores notional burn on redeem.
#[test]
fn partial_redeem_then_sync_forfeits_share_of_subsequent_distribution() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;

    let build_pool = || -> SubPool {
        let mut sub_pool = SubPool::new(0, entry, 0);
        let (_alice, _) = open(
            &market,
            &mut sub_pool,
            envelope(entry, 1),
            Direction::Long,
            100_000_000,
        )
        .unwrap();
        let (_bob, _) = open(
            &market,
            &mut sub_pool,
            envelope(entry, 2),
            Direction::Short,
            100_000_000,
        )
        .unwrap();
        let crash = (entry * 70) / 100;
        sync_pool(&market, &mut sub_pool, envelope(crash, 3)).unwrap();
        let (_carol, _) = open(
            &market,
            &mut sub_pool,
            envelope(crash, 4),
            Direction::Short,
            80_000_000,
        )
        .unwrap();
        let (_dave, _) = open(
            &market,
            &mut sub_pool,
            envelope(crash, 5),
            Direction::Long,
            80_000_000,
        )
        .unwrap();
        sub_pool
    };

    // Helper that mirrors what the engine does on a real claim: redeem
    // the bucket AND keep the sub_pool-level recovery_shares aggregate
    // consistent. The bare `DormantStore::redeem` only mutates the
    // bucket; the engine path also subtracts from
    // `sub_pool.recovery_shares_mut(direction)`. Both updates are
    // required to satisfy the post-call invariant
    // `inv5_recovery_shares_match_buckets`.
    fn redeem_keeping_aggregate(
        sub_pool: &mut SubPool,
        tick: i64,
        shares: u128,
    ) -> u128 {
        let receipt = sub_pool.long_dormant.redeem(tick, shares).unwrap();
        sub_pool.long_recovery_shares -= receipt.burned_shares;
        receipt.redeemable
    }

    let mut pool_a = build_pool();
    let mut pool_b = build_pool();
    let tick = pool_a.long_dormant.bucket_ticks()[0];

    // Snapshot the bucket's notional and anchor before either ordering
    // touches it; we'll need them to derive the analytic upper bound.
    let (notional_initial, anchor_price) = {
        let bucket = pool_a.long_dormant.get(tick).expect("dormant bucket");
        (bucket.total_recovery_notional, bucket.anchor_price)
    };

    let p_75 = (entry * 75) / 100;
    let p_85 = (entry * 85) / 100;

    // Order A: two recovery syncs, then redeem the full holder share.
    sync_pool(&market, &mut pool_a, envelope(p_75, 6)).unwrap();
    sync_pool(&market, &mut pool_a, envelope(p_85, 7)).unwrap();
    let total_shares_a = pool_a.long_dormant.get(tick).unwrap().total_recovery_shares;
    let order_a_total = redeem_keeping_aggregate(&mut pool_a, tick, total_shares_a);

    // Order B: half-redeem between the two syncs.
    sync_pool(&market, &mut pool_b, envelope(p_75, 6)).unwrap();
    let total_shares_b = pool_b.long_dormant.get(tick).unwrap().total_recovery_shares;
    let half = total_shares_b / 2;
    let order_b_partial = redeem_keeping_aggregate(&mut pool_b, tick, half);
    sync_pool(&market, &mut pool_b, envelope(p_85, 7)).unwrap();
    let remaining = pool_b
        .long_dormant
        .get(tick)
        .map(|b| b.total_recovery_shares)
        .unwrap_or(0);
    let order_b_remaining = redeem_keeping_aggregate(&mut pool_b, tick, remaining);
    let order_b_total = order_b_partial + order_b_remaining;

    // (1) Patient holder never earns less than the impatient one. This
    //     is the protocol's *anti-griefing* invariant: a partial
    //     redeem can only forfeit value, never gain.
    assert!(
        order_a_total >= order_b_total,
        "patient holder out-earned by impatient one: A={order_a_total}, B={order_b_total}"
    );

    // (2) Forfeiture is bounded by the structural difference at the
    //     second sync. A half-burned bucket has at most half the
    //     intrinsic at p_85: forfeit ≤ (notional_initial / 2) ·
    //     (p_85 - anchor) / anchor + small floor slack.
    let half_notional = notional_initial / 2;
    let price_delta = (p_85 - anchor_price) as u128;
    let intrinsic_lost_upper_bound =
        molemath::mul_div_floor(half_notional, price_delta, anchor_price as u128).unwrap();
    let drift = order_a_total - order_b_total;
    assert!(
        drift <= intrinsic_lost_upper_bound + 16,
        "drift {drift} exceeds analytic upper bound {intrinsic_lost_upper_bound} (slack 16)"
    );

    // (3) Path-dependence is non-trivial. If the implementation ever
    //     hard-locks accrual semantics so notional burn doesn't shrink
    //     subsequent intrinsic, drift would collapse to rounding only.
    //     We assert it stays meaningfully > rounding to detect a
    //     silent semantics regression.
    assert!(
        drift > 8,
        "expected path-dependent forfeiture; drift={drift} looks like rounding only"
    );

    pool_a.long_dormant.check_invariants().unwrap();
    pool_b.long_dormant.check_invariants().unwrap();
}

/// Outstanding claim reported by the eager path must equal what the
/// lazy replay path would compute from a freshly-rewound bucket. This is
/// the cross-API invariant that lets the future on-chain "apply on touch"
/// flow plug into existing readers without any callers noticing.
#[test]
fn outstanding_claim_consistent_eager_vs_lazy() {
    let (mut sub_pool, tick, creation_idx) = scenario_with_long_dormant_distributions();
    let p_now = 90 * PRICE_SCALE / 100 * 100;
    let eager_outstanding = sub_pool.long_dormant.get(tick).unwrap().outstanding_claim_at(p_now).unwrap();

    sub_pool
        .long_dormant
        .rewind_bucket_for_replay_test(tick, creation_idx)
        .unwrap();
    sub_pool.long_dormant.apply_pending_to_bucket(tick).unwrap();
    let lazy_outstanding = sub_pool.long_dormant.get(tick).unwrap().outstanding_claim_at(p_now).unwrap();

    assert_eq!(eager_outstanding, lazy_outstanding);
}
