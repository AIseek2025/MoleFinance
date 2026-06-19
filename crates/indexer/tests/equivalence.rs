//! Strong equivalence: chain events → indexer projection ≈ simulation oracle.
//!
//! For a sequence of `(open, sync, close)` operations, the on-chain
//! shares-model engine emits `EngineEvent`s that the indexer replays
//! into `(locked_loss, realized_profit_balance)` per position. The
//! `simulation` crate maintains the same per-position quantities by
//! running the whitepaper's per-position proportional clearing model
//! directly. We claim these two views agree on a per-position basis
//! within bounded rounding error.
//!
//! Symmetric two-trader scenarios with equal notional should match
//! exactly. Heterogeneous scenarios are bounded by floor-rounding from
//! different anchor prices, captured separately as conservation tests.

use std::sync::atomic::{AtomicU64, Ordering};

use clearing_core::{
    close_position, open_position as engine_open, sync_pool, Direction, MarketParams, OpenOutcome,
    Position, PriceEnvelope, SubPool,
};
use indexer::IndexerState;
use molemath::PRICE_SCALE;
use simulation::{settle as oracle_settle, OraclePosition};

static POSITION_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_id() -> u64 {
    POSITION_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn envelope(p: u64, slot: u64) -> PriceEnvelope {
    PriceEnvelope {
        p_now: p,
        slot,
        expected_min: p,
        expected_max: p,
    }
}

fn open_chain(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    indexer: &mut IndexerState,
    direction: Direction,
    gross: u64,
    slot: u64,
    p: u64,
) -> (Position, OpenOutcome, u64) {
    let id = next_id();
    let (pos, outcome) = engine_open(
        market,
        sub_pool,
        envelope(p, slot),
        direction,
        gross,
        id,
    )
    .unwrap();
    indexer.apply_all(&outcome.events).unwrap();
    (pos, outcome, id)
}

fn run_sync(
    market: &MarketParams,
    sub_pool: &mut SubPool,
    indexer: &mut IndexerState,
    p: u64,
    slot: u64,
) {
    let outcome = sync_pool(market, sub_pool, envelope(p, slot)).unwrap();
    indexer.apply_all(&outcome.events).unwrap();
}

fn make_oracle(market: &MarketParams, direction: Direction, principal: u128, p: u64) -> OraclePosition {
    OraclePosition::new(
        0,
        direction,
        principal,
        market.leverage_bps,
        p,
    )
    .unwrap()
}

#[test]
fn alice_long_bob_short_two_step_indexer_matches_oracle() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);
    let mut indexer = IndexerState::new();

    let stake = 100_000_000u64;
    let principal = stake as u128;

    let (mut alice, _, alice_id) = open_chain(
        &market,
        &mut sub_pool,
        &mut indexer,
        Direction::Long,
        stake,
        1,
        entry,
    );
    let (mut bob, _, bob_id) = open_chain(
        &market,
        &mut sub_pool,
        &mut indexer,
        Direction::Short,
        stake,
        2,
        entry,
    );

    let mut oracles = vec![
        make_oracle(&market, Direction::Long, principal, entry),
        make_oracle(&market, Direction::Short, principal, entry),
    ];

    // Step 1: +1% sync.
    let p1 = entry + entry / 100;
    run_sync(&market, &mut sub_pool, &mut indexer, p1, 3);
    oracle_settle(&mut oracles, p1).unwrap();

    let av = indexer.position(alice_id).unwrap();
    let bv = indexer.position(bob_id).unwrap();
    assert_eq!(av.realized_profit_balance, oracles[0].realized_profit_balance);
    assert_eq!(av.locked_loss, oracles[0].locked_loss);
    assert_eq!(bv.realized_profit_balance, oracles[1].realized_profit_balance);
    assert_eq!(bv.locked_loss, oracles[1].locked_loss);

    // Step 2: -0.5% sync.
    let p2 = p1 - p1 / 200;
    run_sync(&market, &mut sub_pool, &mut indexer, p2, 4);
    oracle_settle(&mut oracles, p2).unwrap();

    let av = indexer.position(alice_id).unwrap();
    let bv = indexer.position(bob_id).unwrap();
    assert_eq!(av.realized_profit_balance, oracles[0].realized_profit_balance);
    assert_eq!(av.locked_loss, oracles[0].locked_loss);
    assert_eq!(bv.realized_profit_balance, oracles[1].realized_profit_balance);
    assert_eq!(bv.locked_loss, oracles[1].locked_loss);

    // Close both. Chain withdrawable equals indexer pre-close equity.
    let alice_eq_before = av.equity();
    let bob_eq_before = bv.equity();
    let close_a = close_position(&market, &mut sub_pool, envelope(p2, 5), &mut alice).unwrap();
    indexer.apply_all(&close_a.events).unwrap();
    let close_b = close_position(&market, &mut sub_pool, envelope(p2, 6), &mut bob).unwrap();
    indexer.apply_all(&close_b.events).unwrap();

    assert_eq!(alice_eq_before, close_a.withdrawable);
    assert_eq!(bob_eq_before, close_b.withdrawable);

    // Conservation.
    let total = close_a.withdrawable + close_b.withdrawable;
    assert!(total <= 2 * principal);
    assert!(2 * principal - total <= 4);
}

#[test]
fn three_step_oscillation_indexer_matches_oracle_per_position() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);
    let mut indexer = IndexerState::new();

    let stake = 100_000_000u64;
    let principal = stake as u128;

    let (_alice, _, alice_id) = open_chain(
        &market,
        &mut sub_pool,
        &mut indexer,
        Direction::Long,
        stake,
        1,
        entry,
    );
    let (_bob, _, bob_id) = open_chain(
        &market,
        &mut sub_pool,
        &mut indexer,
        Direction::Short,
        stake,
        2,
        entry,
    );

    let mut oracles = vec![
        make_oracle(&market, Direction::Long, principal, entry),
        make_oracle(&market, Direction::Short, principal, entry),
    ];

    let prices = [
        entry + entry / 50,
        entry - entry / 50 - entry / 50,
        entry + entry / 100,
    ];

    let mut slot = 3u64;
    for &p in &prices {
        run_sync(&market, &mut sub_pool, &mut indexer, p, slot);
        oracle_settle(&mut oracles, p).unwrap();
        slot += 1;

        let av = indexer.position(alice_id).unwrap();
        let bv = indexer.position(bob_id).unwrap();
        assert_eq!(
            av.realized_profit_balance, oracles[0].realized_profit_balance,
            "alice rpb after price {}",
            p
        );
        assert_eq!(av.locked_loss, oracles[0].locked_loss);
        assert_eq!(bv.realized_profit_balance, oracles[1].realized_profit_balance);
        assert_eq!(bv.locked_loss, oracles[1].locked_loss);
    }
}

#[test]
fn random_walk_chain_events_indexer_consistent_with_oracle() {
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha20Rng;

    for seed in [1u64, 7, 42, 1024] {
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let market = MarketParams::sample();
        let entry = 100 * PRICE_SCALE;
        let mut sub_pool = SubPool::new(0, entry, 0);
        let mut indexer = IndexerState::new();

        let stake = 100_000_000u64;
        let principal = stake as u128;

        let (_alice, _, alice_id) = open_chain(
            &market,
            &mut sub_pool,
            &mut indexer,
            Direction::Long,
            stake,
            1,
            entry,
        );
        let (_bob, _, bob_id) = open_chain(
            &market,
            &mut sub_pool,
            &mut indexer,
            Direction::Short,
            stake,
            2,
            entry,
        );

        let mut oracles = vec![
            make_oracle(&market, Direction::Long, principal, entry),
            make_oracle(&market, Direction::Short, principal, entry),
        ];

        let mut p_now = entry;
        for slot in 3u64..53 {
            let bps: i64 = rng.gen_range(-50..=50);
            let delta = (p_now as i128 * bps as i128 / 10_000) as i64;
            let p_next = (p_now as i128 + delta as i128).max(1) as u64;
            if p_next == p_now {
                continue;
            }
            // Stop if either pool already zeroed (rotation handled separately).
            if sub_pool.long_pool_equity == 0 || sub_pool.short_pool_equity == 0 {
                break;
            }

            let outcome = sync_pool(&market, &mut sub_pool, envelope(p_next, slot));
            if outcome.is_err() {
                continue;
            }
            indexer.apply_all(&outcome.unwrap().events).unwrap();
            oracle_settle(&mut oracles, p_next).unwrap();
            p_now = p_next;

            let av = indexer.position(alice_id).unwrap();
            let bv = indexer.position(bob_id).unwrap();
            assert_eq!(
                av.realized_profit_balance, oracles[0].realized_profit_balance,
                "seed {} slot {}: alice rpb",
                seed, slot
            );
            assert_eq!(av.locked_loss, oracles[0].locked_loss);
            assert_eq!(
                bv.realized_profit_balance, oracles[1].realized_profit_balance,
                "seed {} slot {}: bob rpb",
                seed, slot
            );
            assert_eq!(bv.locked_loss, oracles[1].locked_loss);
        }
    }
}

#[test]
fn three_traders_total_withdrawable_conserves_via_indexer() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sub_pool = SubPool::new(0, entry, 0);
    let mut indexer = IndexerState::new();

    let stake_a = 100_000_000u64;
    let (mut a, _, a_id) = open_chain(
        &market,
        &mut sub_pool,
        &mut indexer,
        Direction::Long,
        stake_a,
        1,
        entry,
    );

    let stake_b = 250_000_000u64;
    let (mut b, _, b_id) = open_chain(
        &market,
        &mut sub_pool,
        &mut indexer,
        Direction::Short,
        stake_b,
        2,
        entry,
    );

    let p1 = entry + entry * 7 / 1000;
    run_sync(&market, &mut sub_pool, &mut indexer, p1, 3);

    let stake_c = 50_000_000u64;
    let (mut c, _, c_id) = open_chain(
        &market,
        &mut sub_pool,
        &mut indexer,
        Direction::Long,
        stake_c,
        4,
        p1,
    );

    let p2 = p1 - p1 * 4 / 1000;
    run_sync(&market, &mut sub_pool, &mut indexer, p2, 5);

    // Per-position projection is allowed to diverge from chain `withdrawable`
    // by a few raw units in heterogeneous-open scenarios: the indexer
    // accumulates per-step floor truncations while the engine performs a
    // single floor at close time. The conservation invariant below pins
    // the aggregate gap.
    for (pos, id) in [(&mut a, a_id), (&mut b, b_id), (&mut c, c_id)] {
        let view = indexer.position(id).unwrap();
        let pre_eq = view.equity();
        let outcome = close_position(&market, &mut sub_pool, envelope(p2, 10), pos).unwrap();
        indexer.apply_all(&outcome.events).unwrap();
        let drift = pre_eq.abs_diff(outcome.withdrawable);
        // Empirically bounded above by `O(num_sync_steps)` for this scenario;
        // 16 leaves comfortable headroom.
        assert!(
            drift <= 16,
            "id {}: indexer pre-close equity {} drifted from chain withdrawable {} by {}",
            id,
            pre_eq,
            outcome.withdrawable,
            drift
        );
    }

    let total_principal = (stake_a as u128) + (stake_b as u128) + (stake_c as u128);
    let total_indexer_equity: u128 = indexer
        .positions()
        .map(|p| p.principal.saturating_sub(p.locked_loss) + p.realized_profit_balance)
        .sum();
    // Drift on either side is bounded by floor residue accumulated across
    // sync steps. With at most ~2 floor truncations per step and 2 steps,
    // 32 raw units leaves comfortable headroom either direction.
    let drift = total_indexer_equity.abs_diff(total_principal);
    assert!(
        drift <= 32,
        "conservation drift {} (principal={}, indexer={})",
        drift,
        total_principal,
        total_indexer_equity
    );
}
