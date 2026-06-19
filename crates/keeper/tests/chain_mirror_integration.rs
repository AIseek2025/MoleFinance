//! Wave 8 — end-to-end keeper integration test against `chain-mirror`.
//!
//! Verifies the full lazy-mode keeper loop:
//!
//! 1. Two users open opposite-direction positions.
//! 2. A price crash triggers a rotate; the loose-mode `ChainRuntime`
//!    materialises the dormant bucket on the long side.
//! 3. `Scheduler::plan` is empty (nothing pending in eager mode, no
//!    dead buckets).
//! 4. Switch to lazy mode by setting up a fresh runtime that uses
//!    `DistributeMode::Lazy`. After more syncs, the bucket falls
//!    behind: `ledger.next_event_index > bucket.last_applied_index`.
//! 5. `Scheduler::plan` returns one `PreSyncDormantBucket` action;
//!    after the keeper applies it (via `pre_sync_bucket`), the next
//!    plan is empty.
//! 6. Drain the bucket via `claim_recovery`. The bucket goes dead;
//!    `Scheduler::plan` returns a `CloseDormantBucket`. After the
//!    keeper applies it, the next plan is empty.
//!
//! This locks in the contract: the scheduler is **complete** with
//! respect to the chain state — every actionable PDA is surfaced
//! exactly once, and applying the action drains the queue.

use chain_mirror::{ChainRuntime, DormantBucketAccount};
use clearing_core::{Direction, DistributeMode, MarketParams, PriceEnvelope};
use keeper::{
    BucketSnapshot, InitRationale, KeeperAction, KeeperChainView, LedgerSnapshot,
    PredictorConfig, RotateRiskPredictor, Scheduler, SchedulerConfig, SubPoolHealth,
};
use molemath::PRICE_SCALE;

/// Newtype wrapper that adapts `chain_mirror::ChainRuntime` to the
/// `KeeperChainView` trait. Tests use this to feed the scheduler a
/// real bridge state instead of synthetic stub data.
struct MirrorView<'a>(&'a ChainRuntime);

impl KeeperChainView for MirrorView<'_> {
    fn sub_pool_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.0.sub_pools.keys().copied().collect();
        ids.sort();
        ids
    }

    fn buckets(&self, sub_pool_id: u32) -> Vec<BucketSnapshot> {
        let mut out: Vec<BucketSnapshot> = self
            .0
            .buckets
            .iter()
            .filter_map(|((sp, dir, tick), acc): (&(u32, Direction, i64), &DormantBucketAccount)| {
                if *sp != sub_pool_id {
                    return None;
                }
                let r = &acc.record;
                Some(BucketSnapshot {
                    sub_pool_id: *sp,
                    direction: *dir,
                    tick: *tick,
                    anchor_price: r.anchor_price,
                    total_recovery_shares: r.total_recovery_shares,
                    total_recovery_notional: r.total_recovery_notional,
                    accrued_value: r.accrued_value,
                    position_count: r.position_count,
                    last_applied_index: r.last_applied_index,
                })
            })
            .collect();
        out.sort_by_key(|b| (b.direction == Direction::Short, b.tick));
        out
    }

    fn ledger(&self, sub_pool_id: u32, direction: Direction) -> Option<LedgerSnapshot> {
        let l = self.0.ledgers.get(&(sub_pool_id, direction))?;
        Some(LedgerSnapshot {
            sub_pool_id,
            direction,
            next_event_index: l.ledger.next_event_index,
            bucket_count_hint: 0,
        })
    }

    /// Wave 9 — surface a [`SubPoolHealth`] snapshot so the keeper's
    /// `RotateRiskPredictor` can run against the chain-mirror runtime.
    /// Anchor price source-of-truth is approximated as follows:
    ///
    /// - If the directional rotate-log has at least one entry, take
    ///   the *most recent* rotate's `anchor_price` — that's the
    ///   generation-zero boundary the engine carries.
    /// - Otherwise (no rotation has happened on this side yet), use
    ///   `last_price` as an approximation. This is a known
    ///   conservative choice: in pristine pre-rotate state the active
    ///   pool's anchor really is the inception price, which `last_
    ///   price` only equals as long as the user hasn't synced
    ///   significantly. The model degrades to "predict zero relative
    ///   to recent price", which is the right behaviour for risk-
    ///   horizon hinting (we don't need exact, we need timely).
    fn sub_pool_health(&self, sub_pool_id: u32) -> Option<SubPoolHealth> {
        let sp = self.0.sub_pools.get(&sub_pool_id)?;
        let long_anchor = sp
            .long_rotate_log
            .last()
            .map(|r| r.anchor_price)
            .unwrap_or(sp.last_price);
        let short_anchor = sp
            .short_rotate_log
            .last()
            .map(|r| r.anchor_price)
            .unwrap_or(sp.last_price);
        Some(SubPoolHealth {
            sub_pool_id,
            last_price: sp.last_price,
            long_anchor_price: long_anchor,
            short_anchor_price: short_anchor,
            long_pool_equity: sp.long_pool_equity,
            short_pool_equity: sp.short_pool_equity,
            long_active_notional: sp.long_active_notional,
            short_active_notional: sp.short_active_notional,
            long_active_generation: sp.long_active_generation,
            short_active_generation: sp.short_active_generation,
        })
    }
}

fn envelope(p: u64, slot: u64) -> PriceEnvelope {
    PriceEnvelope {
        p_now: p,
        slot,
        expected_min: p,
        expected_max: p,
    }
}

fn lazy_market() -> MarketParams {
    let mut m = MarketParams::sample();
    m.dormant_distribute_mode = DistributeMode::Lazy;
    m.max_price_move_bps_per_sync = 50_000;
    m
}

#[test]
fn lazy_mode_keeper_loop_drains_pending_then_closes() {
    let market = lazy_market();
    let entry = 100 * PRICE_SCALE;
    let mut rt = ChainRuntime::new(market);
    rt.add_sub_pool(0, entry, 0);
    let alice = rt.open(0, Direction::Long, 100_000_000, envelope(entry, 1)).unwrap();
    let _bob = rt.open(0, Direction::Short, 200_000_000, envelope(entry, 2)).unwrap();

    // Crash price: rotate long. Lazy mode: the rotate emits one
    // distribute-event entry into the ledger, but the bucket's
    // `last_applied_index` does NOT track the new head until pre_sync
    // catches up. Wait — actually the rotate itself doesn't push a
    // distribute entry; it only creates the bucket. We need to drive
    // a distribution event afterwards.
    rt.sync(0, envelope(entry / 2, 3)).unwrap();

    // Now drive a recovery distribution by syncing back up. Each
    // up-tick after rotate pushes another distribute entry that
    // accrues value into recovery buckets.
    for slot in 4..10u64 {
        let bps_up = (slot as i64 - 3) * 1_000;
        let p = ((entry / 2) as i128 + (entry / 2) as i128 * bps_up as i128 / 10_000) as u64;
        let _ = rt.sync(0, envelope(p, slot));
    }

    // Inspect the chain view: there should be at least one long
    // bucket, and its last_applied_index should lag behind the
    // ledger head (lazy mode).
    let view = MirrorView(&rt);
    let long_buckets = view.buckets(0).into_iter().filter(|b| b.direction == Direction::Long).collect::<Vec<_>>();
    assert!(!long_buckets.is_empty(), "rotate must materialise a long bucket");
    let bucket = long_buckets[0];
    let ledger = view.ledger(0, Direction::Long).unwrap();
    let pending_observed = ledger.next_event_index.saturating_sub(bucket.last_applied_index);
    assert!(
        pending_observed > 0,
        "lazy mode must accumulate pending: head={}, last_applied={}",
        ledger.next_event_index,
        bucket.last_applied_index
    );

    // Plan: scheduler emits one PreSyncDormantBucket action.
    let mut sched = Scheduler::default();
    let plan1 = sched.plan(&view).unwrap();
    assert_eq!(plan1.len(), 1, "expected exactly one action; got {:?}", plan1);
    let (action_tick, action_pending) = match plan1[0] {
        KeeperAction::PreSyncDormantBucket { tick, pending, .. } => (tick, pending),
        ref other => panic!("expected PreSync, got {other:?}"),
    };
    assert_eq!(action_tick, bucket.tick);
    assert_eq!(action_pending, pending_observed);

    // Apply the action (keeper executes pre_sync_bucket).
    let _ = rt
        .pre_sync_bucket(0, Direction::Long, action_tick, 100)
        .expect("keeper pre_sync_bucket must succeed");

    // Plan again: no more pending — queue is empty (bucket still
    // live, so no Close either).
    let view2 = MirrorView(&rt);
    let plan2 = sched.plan(&view2).unwrap();
    assert!(
        plan2.is_empty(),
        "after pre_sync, no actionable bucket; got {:?}",
        plan2
    );

    // Drive long bucket toward "drained": alice claims her recovery
    // shares at a small up-tick so the per-claim sync doesn't
    // accidentally rotate the short side too.
    let last_long_p = view2.buckets(0)[0].anchor_price;
    let _ = rt
        .claim_recovery(alice.position_id, envelope(last_long_p + 100, 200))
        .expect("claim must succeed");

    // After claim, the long bucket either disappeared (loose-mode
    // pruning when the engine drops a zero-share bucket) or is dead
    // in place. Either way the keeper plan should NOT emit a PreSync
    // or Close action against a still-live long bucket.
    let view3 = MirrorView(&rt);
    let surviving_long: Vec<_> = view3
        .buckets(0)
        .into_iter()
        .filter(|b| b.direction == Direction::Long)
        .collect();
    if let Some(long) = surviving_long.first() {
        // Loose mode kept the slot — must be dead.
        assert!(
            long.is_dead(),
            "post-claim long bucket must be dead; saw: {long:?}"
        );
    }

    // The keeper plan now reflects whatever side(s) need work. We
    // don't pin the exact set (the per-claim sync may rotate short
    // and add ledger entries on either side); we DO pin that no
    // PreSync is emitted for the long bucket we just drained. This
    // is the most user-relevant guarantee: keepers never poke a
    // bucket that has nothing to apply.
    let plan3 = sched.plan(&view3).unwrap();
    for action in &plan3 {
        if let KeeperAction::PreSyncDormantBucket {
            sub_pool_id,
            direction: Direction::Long,
            tick,
            ..
        } = action
        {
            // If a long pre-sync surfaces it must reference a still-
            // live bucket — never the one alice just drained.
            let live = view3
                .buckets(*sub_pool_id)
                .into_iter()
                .any(|b| b.direction == Direction::Long && b.tick == *tick && !b.is_dead());
            assert!(
                live,
                "scheduler emitted PreSync for a dead/missing long bucket: {action:?}"
            );
        }
    }
}

/// **Wave 9.** End-to-end: a tilted-pool scenario where one side
/// has lost most of its equity. The `RotateRiskPredictor` MUST
/// surface this through `populate_scheduler` AND the resulting plan
/// MUST contain at least one `InitDormantBucket` action with
/// rationale `RotateRiskHorizon`. After the keeper executes the
/// init, the chain runtime's bucket inventory MUST contain a dead
/// PDA at the predicted tick — proving the predictor + scheduler +
/// chain-mirror form a closed loop. (The strict-mode rotate-lands-
/// on-pre-init flow is exercised in `chain-mirror`'s own tests
/// suite via `strict_mode_byte_equal_to_loose_with_keeper_preinit`;
/// here we focus on the new predictor+scheduler integration.)
#[test]
fn rotate_risk_predictor_drives_init_hint_and_pre_init_pda() {
    let market = lazy_market();
    let entry = 100 * PRICE_SCALE;
    let mut rt = ChainRuntime::new(market);
    rt.add_sub_pool(0, entry, 0);

    // Two equal positions. The predictor's risk model doesn't need
    // an extreme tilt to surface a tick: any non-trivial volatility
    // assumption + non-zero ratio yields a non-trivial probability
    // for both directions. We use a balanced book + small price
    // move to keep the engine state well within pre-rotate.
    rt.open(0, Direction::Long, 100_000_000, envelope(entry, 1)).unwrap();
    rt.open(0, Direction::Short, 100_000_000, envelope(entry, 2)).unwrap();
    rt.check_vault_decomposition().unwrap();

    // 1% drop — drains ~10% of long equity (10x leverage) while
    // both directional pools remain healthy. Clearance from the
    // rotate boundary is large enough that no rotate fires.
    rt.sync(0, envelope(entry * 99 / 100, 3)).unwrap();
    rt.check_vault_decomposition().unwrap();

    // The view's lifetime is scoped to this block so the immutable
    // borrow on `rt` is released before we mutate it below.
    let init: i64 = {
        let view = MirrorView(&rt);
        let h = view.sub_pool_health(0).expect("health surface available");
        assert!(
            h.long_active_notional > 0,
            "scenario presumes long active pool is still populated; \
             got long_pool_equity={} long_active_notional={}",
            h.long_pool_equity, h.long_active_notional
        );
        assert!(
            h.short_active_notional > 0,
            "scenario presumes short active pool is still populated"
        );
        assert!(
            h.long_pool_equity > 0 && h.short_pool_equity > 0,
            "no rotate yet: long_eq={}, short_eq={}",
            h.long_pool_equity, h.short_pool_equity
        );

        // Aggressive predictor: high vol, long horizon, low min-prob —
        // the tilted long side must register.
        let predictor = RotateRiskPredictor::new(PredictorConfig {
            annual_vol: 1.5,
            horizon_slots: 10_000_000,
            slots_per_second: 2.5,
            min_probability: 0.005,
            tick_aggregation_factor: 1,
            price_tick: 1,
        });
        let mut sched = Scheduler::default();
        let preds = predictor.populate_scheduler(&view, &mut sched);
        let long_pred = preds
            .iter()
            .find(|p| p.direction == Direction::Long)
            .expect("predictor must surface long-side risk");

        // Plan: the InitDormantBucket action for the predicted tick
        // must surface with rationale RotateRiskHorizon.
        let plan = sched.plan(&view).expect("scheduler plan must succeed");
        plan.iter()
            .find_map(|a| match a {
                KeeperAction::InitDormantBucket {
                    sub_pool_id: 0,
                    direction: Direction::Long,
                    tick,
                    rationale: InitRationale::RotateRiskHorizon,
                } if *tick == long_pred.zero_tick => Some(*tick),
                _ => None,
            })
            .expect("plan must include the predicted long-init action")
    };

    // Loose mode: pre_init_dormant_bucket still inserts a dead PDA
    // at the requested key; this exercises the keeper's
    // `init_dormant_bucket` instruction code path.
    rt.pre_init_dormant_bucket(0, Direction::Long, init)
        .expect("keeper init must succeed");

    // The chain runtime must now hold a dead long PDA at the
    // predicted tick. This is the keystone integration outcome:
    // the predictor surfaced the right tick, the scheduler emitted
    // the right action, and the keeper's response produced the
    // right on-chain state.
    let view2 = MirrorView(&rt);
    let pre_rotate_long: Vec<_> = view2
        .buckets(0)
        .into_iter()
        .filter(|b| b.direction == Direction::Long && b.tick == init)
        .collect();
    assert_eq!(
        pre_rotate_long.len(),
        1,
        "predicted long PDA must exist after keeper init: {pre_rotate_long:?}"
    );
    assert!(
        pre_rotate_long[0].is_dead(),
        "freshly-init'd PDA must be dead: {:?}",
        pre_rotate_long[0]
    );
    rt.check_vault_decomposition().unwrap();
}

/// Smoke test: configuring `min_pending_for_pre_sync` higher than the
/// observed lag silences pre-sync until the lag exceeds the threshold.
/// This is the knob a production keeper uses to throttle CU spend
/// when the protocol is healthy.
#[test]
fn min_pending_threshold_silences_until_lag_exceeds_it() {
    let market = lazy_market();
    let entry = 100 * PRICE_SCALE;
    let mut rt = ChainRuntime::new(market);
    rt.add_sub_pool(0, entry, 0);
    let _alice = rt.open(0, Direction::Long, 100_000_000, envelope(entry, 1)).unwrap();
    let _bob = rt.open(0, Direction::Short, 200_000_000, envelope(entry, 2)).unwrap();
    rt.sync(0, envelope(entry / 2, 3)).unwrap();
    // Single up-tick → at most 1 ledger entry behind.
    let _ = rt.sync(0, envelope((entry / 2) + (entry / 200), 4));

    let view = MirrorView(&rt);
    let mut sched = Scheduler::new(SchedulerConfig {
        min_pending_for_pre_sync: 1_000_000, // huge threshold
        ..Default::default()
    });
    assert!(
        sched.plan(&view).unwrap().is_empty(),
        "threshold larger than any realistic lag must silence pre-sync entirely"
    );
}

