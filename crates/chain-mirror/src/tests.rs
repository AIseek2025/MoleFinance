//! Smoke tests for the chain-mirror runtime.
//!
//! These exercise each instruction handler end-to-end through the
//! bridged dormant store. The cross-runtime parity property is in
//! `tests/harness_parity.rs` (much heavier).

use super::*;
use molemath::PRICE_SCALE;

fn envelope(p: u64, slot: u64) -> PriceEnvelope {
    PriceEnvelope {
        p_now: p,
        slot,
        expected_min: p,
        expected_max: p,
    }
}

#[test]
fn open_then_close_round_trip_no_rotation() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut rt = ChainRuntime::new(market);
    rt.add_sub_pool(0, entry, 0);

    let alice = rt.open(0, Direction::Long, 100_000_000, envelope(entry, 1)).unwrap();
    let bob = rt.open(0, Direction::Short, 100_000_000, envelope(entry, 2)).unwrap();
    rt.check_vault_decomposition().unwrap();

    let p = entry + entry / 50;
    rt.sync(0, envelope(p, 3)).unwrap();
    rt.check_vault_decomposition().unwrap();

    rt.close(alice.position_id, envelope(p, 4)).unwrap();
    rt.close(bob.position_id, envelope(p, 5)).unwrap();
    rt.check_vault_decomposition().unwrap();
}

#[test]
fn rotation_creates_persistent_bucket_account() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut rt = ChainRuntime::new(market);
    rt.add_sub_pool(0, entry, 0);

    let alice = rt.open(0, Direction::Long, 100_000_000, envelope(entry, 1)).unwrap();
    let _bob = rt.open(0, Direction::Short, 100_000_000, envelope(entry, 2)).unwrap();
    assert_eq!(rt.bucket_count(0, Direction::Long), 0);

    // Crash long: long pool zero → rotate.
    let p_crash = entry / 2;
    rt.sync(0, envelope(p_crash, 3)).unwrap();
    rt.check_vault_decomposition().unwrap();
    // After the rotate, the chain runtime must hold a *persistent*
    // DormantBucket account for the long side.
    assert_eq!(
        rt.bucket_count(0, Direction::Long),
        1,
        "rotation must materialise a DormantBucket account",
    );

    // Recovery: price comes back, alice's bucket accrues, then she
    // claims and closes.
    let p_back = entry * 3 / 4;
    rt.sync(0, envelope(p_back, 4)).unwrap();
    rt.check_vault_decomposition().unwrap();

    rt.close(alice.position_id, envelope(p_back, 5)).unwrap();
    rt.check_vault_decomposition().unwrap();
    // After close drained the only position in the bucket, the
    // account should have been dropped (mirrors on-chain
    // close_account or the keeper sweeping zero buckets).
    assert_eq!(
        rt.bucket_count(0, Direction::Long),
        0,
        "drained bucket must be removed",
    );
}

#[test]
fn failed_close_with_zero_withdrawable_reverts_chain_runtime() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut rt = ChainRuntime::new(market);
    rt.add_sub_pool(0, entry, 0);

    let bob = rt.open(0, Direction::Long, 100_000_000, envelope(entry, 1)).unwrap();
    let _ = rt.open(0, Direction::Short, 200_000_000, envelope(entry, 2)).unwrap();

    // 90 % crash → long active rotated to recovery.
    let p_crash = entry / 10;
    rt.sync(0, envelope(p_crash, 3)).unwrap();

    // Snapshot the entire mutable state.
    let buckets_before: Vec<_> = rt.buckets.iter().map(|(k, v)| (*k, *v)).collect();
    let ledger_long_before = rt.ledgers.get(&(0, Direction::Long)).cloned();
    let sp_before = rt.sub_pools.get(&0).cloned().unwrap();
    let pos_before = rt.positions.get(&bob.position_id).cloned();
    let vault_before = rt.vault_balance;

    let err = rt
        .close(bob.position_id, envelope(p_crash, 4))
        .expect_err("must fail with WithdrawableZero");
    match err {
        MirrorError::Clearing(ClearingError::WithdrawableZero) => {}
        other => panic!("unexpected err: {other:?}"),
    }

    // Everything should be unchanged.
    let sp_after = rt.sub_pools.get(&0).cloned().unwrap();
    assert_eq!(sp_before.long_pool_equity, sp_after.long_pool_equity);
    assert_eq!(sp_before.short_pool_equity, sp_after.short_pool_equity);
    assert_eq!(sp_before.long_recovery_shares, sp_after.long_recovery_shares);
    let buckets_after: Vec<_> = rt.buckets.iter().map(|(k, v)| (*k, *v)).collect();
    assert_eq!(buckets_before.len(), buckets_after.len());
    for (k, v) in &buckets_before {
        let after = rt.buckets.get(k).expect("bucket still present");
        assert_eq!(v.record, after.record);
    }
    assert_eq!(ledger_long_before.unwrap().ledger.entry_count, rt.ledgers[&(0, Direction::Long)].ledger.entry_count);
    let pos_after = rt.positions.get(&bob.position_id).cloned();
    assert_eq!(
        pos_before.unwrap().position.active_shares,
        pos_after.unwrap().position.active_shares,
    );
    assert_eq!(rt.vault_balance, vault_before);

    rt.check_vault_decomposition().unwrap();
}

#[test]
fn harvest_dust_moves_vault_to_fee_vault() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut rt = ChainRuntime::new(market);
    rt.add_sub_pool(0, entry, 0);

    let _alice = rt
        .open(0, Direction::Long, 12_345_679, envelope(entry, 1))
        .unwrap();
    let _bob = rt
        .open(0, Direction::Short, 12_345_679, envelope(entry, 2))
        .unwrap();
    rt.check_vault_decomposition().unwrap();

    // Drive several syncs to accumulate dust.
    for slot in 3..10u64 {
        let bps: i128 = if slot % 2 == 0 { 31 } else { -27 };
        let p = (entry as i128 + entry as i128 * bps / 10_000) as u64;
        let _ = rt.sync(0, envelope(p, slot));
    }
    let vault_before = rt.vault_balance;
    let fee_before = rt.fee_vault_balance;
    let _ = rt.harvest_dust(0, Direction::Long);
    let _ = rt.harvest_dust(0, Direction::Short);
    rt.check_vault_decomposition().unwrap();
    assert_eq!(
        rt.vault_balance + rt.fee_vault_balance,
        vault_before + fee_before,
        "harvest only shifts funds; never creates or destroys",
    );
}

// ===== Wave 8: keeper-driven PDA lifecycle =================================

/// Discover the engine-side bucket tick that *would* result from a
/// rotate at the given price, by running an out-of-band loose-mode
/// runtime through the same op stream. Lets the strict-mode test
/// pre-init exactly the right tick the engine is about to ask for.
fn predict_rotate_tick(
    market: MarketParams,
    sub_pool_id: u32,
    init_price: u64,
    direction: Direction,
    open_long_amt: u64,
    open_short_amt: u64,
    crash_to: u64,
) -> i64 {
    let mut probe = ChainRuntime::new(market);
    probe.add_sub_pool(sub_pool_id, init_price, 0);
    probe
        .open(sub_pool_id, Direction::Long, open_long_amt, envelope(init_price, 1))
        .unwrap();
    probe
        .open(sub_pool_id, Direction::Short, open_short_amt, envelope(init_price, 2))
        .unwrap();
    probe.sync(sub_pool_id, envelope(crash_to, 3)).unwrap();
    let key = probe
        .buckets
        .keys()
        .find(|(sp, dir, _)| *sp == sub_pool_id && *dir == direction)
        .copied()
        .expect("rotate must materialise a bucket on probe runtime");
    key.2
}

/// Strict-mode rotate without a pre-init'd PDA must surface
/// `BucketSlotExhausted` and atomically revert the sub-pool. After
/// the keeper pre-inits the dead PDA at the predicted tick, the same
/// sync succeeds and the resulting bucket has a non-zero
/// `anchor_price` (Wave 7.2 regression guard) and matches the loose-
/// mode runtime byte-for-byte (modulo the dead-slot overlay).
#[test]
fn strict_mode_rejects_rotate_without_preinit_then_keeper_init_unblocks() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let crash = entry / 2;

    let predicted_tick =
        predict_rotate_tick(market.clone(), 0, entry, Direction::Long, 100_000_000, 100_000_000, crash);

    let mut rt = ChainRuntime::new(market.clone()).with_strict_pda_lifecycle(true);
    rt.add_sub_pool(0, entry, 0);
    let _alice = rt.open(0, Direction::Long, 100_000_000, envelope(entry, 1)).unwrap();
    let _bob = rt.open(0, Direction::Short, 100_000_000, envelope(entry, 2)).unwrap();
    rt.check_vault_decomposition().unwrap();
    let snap_before_sync = (
        rt.sub_pools.get(&0).cloned().unwrap(),
        rt.vault_balance,
        rt.bucket_count(0, Direction::Long),
    );

    // Sync with a price drop that *would* rotate. No PDA pre-init →
    // bridge slot exhausted → atomic revert.
    let err = rt
        .sync(0, envelope(crash, 3))
        .expect_err("rotate without pre-init must fail in strict mode");
    match err {
        MirrorError::BucketSlotExhausted { sub_pool, direction } => {
            assert_eq!(sub_pool, 0);
            assert_eq!(direction, Direction::Long);
        }
        other => panic!("unexpected error: {other:?}"),
    }
    // Atomic revert: state must be byte-identical to pre-sync.
    let snap_after_failed_sync = (
        rt.sub_pools.get(&0).cloned().unwrap(),
        rt.vault_balance,
        rt.bucket_count(0, Direction::Long),
    );
    assert_eq!(snap_before_sync.1, snap_after_failed_sync.1);
    assert_eq!(snap_before_sync.2, snap_after_failed_sync.2);
    assert_eq!(
        snap_before_sync.0.long_pool_equity, snap_after_failed_sync.0.long_pool_equity,
        "atomic revert: long_pool_equity must be untouched after BucketSlotExhausted",
    );
    rt.check_vault_decomposition().unwrap();

    // Keeper pre-inits the predicted PDA — same tick the engine will
    // ask for — and re-runs the sync.
    rt.pre_init_dormant_bucket(0, Direction::Long, predicted_tick)
        .unwrap();
    assert_eq!(rt.bucket_count(0, Direction::Long), 1, "pre-init creates a dead PDA slot");
    let pre_init_record = rt.buckets.get(&(0, Direction::Long, predicted_tick)).unwrap().record;
    assert!(pre_init_record.is_dead(), "pre-init'd PDA must be dead");
    assert_eq!(pre_init_record.anchor_price, 0, "dead PDA has anchor_price=0");

    rt.sync(0, envelope(crash, 4)).unwrap();
    rt.check_vault_decomposition().unwrap();
    let post_sync_record = rt.buckets.get(&(0, Direction::Long, predicted_tick)).unwrap().record;
    assert!(
        !post_sync_record.is_dead(),
        "post-rotate bucket must be live (recovery shares > 0)",
    );
    assert!(
        post_sync_record.anchor_price > 0,
        "Wave 7.2 fix: anchor_price MUST be promoted by insert_or_merge — \
         was {}; if 0, the dead-PDA-skip in unpack_dormant_store regressed",
        post_sync_record.anchor_price,
    );
    assert!(post_sync_record.total_recovery_shares > 0);
    assert!(post_sync_record.position_count > 0);
}

/// Strict-mode parity with loose-mode for the same op stream: as long
/// as the keeper pre-inits each rotate tick before the rotate, the
/// resulting bucket records must equal what loose-mode would produce.
#[test]
fn strict_mode_byte_equal_to_loose_with_keeper_preinit() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let crash = entry / 2;

    let predicted_tick =
        predict_rotate_tick(market.clone(), 0, entry, Direction::Long, 100_000_000, 100_000_000, crash);

    let mut loose = ChainRuntime::new(market.clone());
    loose.add_sub_pool(0, entry, 0);
    let _ = loose.open(0, Direction::Long, 100_000_000, envelope(entry, 1)).unwrap();
    let _ = loose.open(0, Direction::Short, 100_000_000, envelope(entry, 2)).unwrap();
    loose.sync(0, envelope(crash, 3)).unwrap();

    let mut strict = ChainRuntime::new(market).with_strict_pda_lifecycle(true);
    strict.add_sub_pool(0, entry, 0);
    let _ = strict.open(0, Direction::Long, 100_000_000, envelope(entry, 1)).unwrap();
    let _ = strict.open(0, Direction::Short, 100_000_000, envelope(entry, 2)).unwrap();
    strict
        .pre_init_dormant_bucket(0, Direction::Long, predicted_tick)
        .unwrap();
    strict.sync(0, envelope(crash, 3)).unwrap();

    // Bucket records must be equal — strict-mode adds no semantic
    // overlay beyond PDA materialisation gating.
    assert_eq!(
        loose.buckets.get(&(0, Direction::Long, predicted_tick)).unwrap().record,
        strict.buckets.get(&(0, Direction::Long, predicted_tick)).unwrap().record,
        "strict + keeper-preinit must produce byte-identical bucket record",
    );
    // Sub-pool scalars must match.
    let loose_sp = loose.sub_pools.get(&0).unwrap();
    let strict_sp = strict.sub_pools.get(&0).unwrap();
    assert_eq!(loose_sp.long_pool_equity, strict_sp.long_pool_equity);
    assert_eq!(loose_sp.long_recovery_shares, strict_sp.long_recovery_shares);
    assert_eq!(loose_sp.long_active_generation, strict_sp.long_active_generation);
}

/// `close_dormant_bucket` is gated on (a) `record_is_dead` and (b)
/// `last_applied_index >= ledger.next_event_index`. Once both
/// conditions are met by the engine, the keeper can reclaim rent.
#[test]
fn keeper_close_dormant_bucket_after_full_drain() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let crash = entry / 2;

    let predicted_tick =
        predict_rotate_tick(market.clone(), 0, entry, Direction::Long, 100_000_000, 100_000_000, crash);

    let mut rt = ChainRuntime::new(market).with_strict_pda_lifecycle(true);
    rt.add_sub_pool(0, entry, 0);
    let alice = rt.open(0, Direction::Long, 100_000_000, envelope(entry, 1)).unwrap();
    let _bob = rt.open(0, Direction::Short, 100_000_000, envelope(entry, 2)).unwrap();
    rt.pre_init_dormant_bucket(0, Direction::Long, predicted_tick).unwrap();
    rt.sync(0, envelope(crash, 3)).unwrap();

    // Closing a still-live bucket must fail: shares > 0.
    let err = rt
        .close_dormant_bucket(0, Direction::Long, predicted_tick)
        .expect_err("cannot close a live bucket");
    assert!(matches!(err, MirrorError::BucketLifecycleViolation { reason, .. } if reason == "bucket still live"));

    // Drain it: alice claims her recovery shares.
    let _ = rt.claim_recovery(alice.position_id, envelope(crash, 4)).unwrap();
    rt.check_vault_decomposition().unwrap();

    // After full drain, the slot is dead in place (Pass 3 left the
    // PDA but zeroed its observables).
    let drained = rt.buckets.get(&(0, Direction::Long, predicted_tick)).unwrap().record;
    assert!(drained.is_dead(), "drained bucket must register as dead");

    // Now the close succeeds — rent reclaimed, PDA gone.
    rt.close_dormant_bucket(0, Direction::Long, predicted_tick).unwrap();
    assert_eq!(rt.bucket_count(0, Direction::Long), 0, "PDA freed");
    rt.check_vault_decomposition().unwrap();

    // Closing again must fail with BucketNotInitialized.
    let err = rt
        .close_dormant_bucket(0, Direction::Long, predicted_tick)
        .expect_err("second close must fail");
    assert!(matches!(err, MirrorError::BucketNotInitialized { .. }));
}

/// `pre_init_dormant_bucket` rejects a duplicate pre-init.
#[test]
fn keeper_preinit_rejects_duplicate() {
    let market = MarketParams::sample();
    let mut rt = ChainRuntime::new(market).with_strict_pda_lifecycle(true);
    rt.add_sub_pool(0, 100 * PRICE_SCALE, 0);
    rt.pre_init_dormant_bucket(0, Direction::Long, 7).unwrap();
    let err = rt
        .pre_init_dormant_bucket(0, Direction::Long, 7)
        .expect_err("duplicate pre-init must fail");
    assert!(matches!(err, MirrorError::BucketLifecycleViolation { reason, .. } if reason == "pda already initialised"));
}

// =====================================================================
// Wave 9 — governance flips & migration
// =====================================================================

/// After `governance_set_paused(true)`, every funds-touching API in
/// the runtime MUST return `Clearing(MarketPaused)` on its very next
/// invocation. This is the chain-mirror analog of the wave-8
/// safety_gates matrix: it proves that flipping the on-chain flag
/// (whether by emergency multisig or by global kill switch) takes
/// effect by the very next block — no in-flight escape hatches.
#[test]
fn governance_pause_immediately_rejects_every_funds_path() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut rt = ChainRuntime::new(market);
    rt.add_sub_pool(0, entry, 0);

    // Seed: one position per direction so close/claim paths hit
    // engine math (not just the early reject).
    let alice = rt
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let bob = rt
        .open(0, Direction::Short, 100_000_000, envelope(entry, 2))
        .unwrap();
    rt.check_vault_decomposition().unwrap();

    // Flip the kill switch.
    rt.governance_set_paused(true);

    // 1. Sync must reject.
    let err = rt
        .sync(0, envelope(entry + entry / 50, 3))
        .expect_err("sync must reject under paused");
    assert!(matches!(err, MirrorError::Clearing(ClearingError::MarketPaused)));

    // 2. Open must reject.
    let err = rt
        .open(0, Direction::Long, 100_000_000, envelope(entry, 4))
        .expect_err("open must reject under paused");
    assert!(matches!(err, MirrorError::Clearing(ClearingError::MarketPaused)));

    // 3. Close must reject.
    let err = rt
        .close(alice.position_id, envelope(entry, 5))
        .expect_err("close must reject under paused");
    assert!(matches!(err, MirrorError::Clearing(ClearingError::MarketPaused)));

    // 4. Force close must reject.
    let err = rt
        .force_close(bob.position_id, envelope(entry, 6), true)
        .expect_err("force_close must reject under paused");
    assert!(matches!(err, MirrorError::Clearing(ClearingError::MarketPaused)));

    // 5. Harvest_dust must reject.
    let err = rt
        .harvest_dust(0, Direction::Long)
        .expect_err("harvest must reject under paused (wave 8 audit fix)");
    assert!(matches!(err, MirrorError::Clearing(ClearingError::MarketPaused)));

    // Now resume, verify the next normal call goes through.
    rt.governance_set_paused(false);
    rt.sync(0, envelope(entry, 7))
        .expect("sync recovers after resume");
    rt.check_vault_decomposition().unwrap();
}

/// `frozen_new_position` is narrower than `paused`: it only blocks
/// `open_position`. Existing positions must still be closable so
/// users don't get trapped in a deprecated market.
#[test]
fn governance_freeze_blocks_only_open_not_close() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut rt = ChainRuntime::new(market);
    rt.add_sub_pool(0, entry, 0);

    let alice = rt
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let _bob = rt
        .open(0, Direction::Short, 100_000_000, envelope(entry, 2))
        .unwrap();
    rt.governance_set_frozen_new_position(true);

    // Open is rejected.
    let err = rt
        .open(0, Direction::Long, 100_000_000, envelope(entry, 3))
        .expect_err("open must reject under freeze");
    assert!(matches!(
        err,
        MirrorError::Clearing(ClearingError::FrozenNewPosition)
    ));

    // Close is still allowed (existing positions can wind down).
    let p = entry + entry / 50;
    rt.sync(0, envelope(p, 4)).unwrap();
    rt.close(alice.position_id, envelope(p, 5))
        .expect("close still works while frozen");
    rt.check_vault_decomposition().unwrap();

    // Unfreeze restores opens.
    rt.governance_set_frozen_new_position(false);
    rt.open(0, Direction::Long, 100_000_000, envelope(p, 6))
        .expect("open recovers after unfreeze");
}

/// After a `bump_market_schema_version` *without* a matching
/// `SCHEMA_VERSION_CURRENT` const bump (i.e. before the new program
/// binary is deployed), every funds-touching API must fail with
/// `SchemaVersionMismatch`. This is the deliberate "lockdown
/// window" sequencing rule from `Docs/Planning/16-治理与升级.md`
/// §3.1: the on-chain schema epoch may *only* be advanced after the
/// program code that understands it has shipped.
///
/// Why this is the keystone protocol-side protection: it means the
/// admin multisig — even if compromised — cannot rug users by
/// flipping the schema number without the corresponding migration
/// instructions also being on chain. The `SCHEMA_VERSION_CURRENT`
/// constant is part of the deployed BPF; it can't be flipped via
/// any Squads transaction.
#[test]
fn governance_bump_without_program_upgrade_freezes_protocol() {
    let mut market = MarketParams::sample();
    market.max_price_move_bps_per_sync = 50_000;
    let entry = 100 * PRICE_SCALE;
    let mut rt = ChainRuntime::new(market);
    rt.add_sub_pool(0, entry, 0);

    let alice = rt
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    let bob = rt
        .open(0, Direction::Short, 100_000_000, envelope(entry, 2))
        .unwrap();

    // Premature bump — program is still on v1 (SCHEMA_VERSION_CURRENT=1).
    rt.governance_bump_schema_version(2)
        .expect("monotonic bump succeeds at the chain-mirror layer");

    // Every funds-touching path must now reject with
    // SchemaVersionMismatch — the deployed program literally does
    // not know how to interpret v2 data.
    let p = entry + entry / 50;
    macro_rules! must_reject_schema {
        ($label:expr, $expr:expr) => {{
            let err = $expr.expect_err($label);
            assert!(
                matches!(
                    err,
                    MirrorError::Clearing(ClearingError::SchemaVersionMismatch)
                ),
                "{} must reject with SchemaVersionMismatch under premature bump, got {:?}",
                $label, err
            );
        }};
    }
    must_reject_schema!("sync", rt.sync(0, envelope(p, 3)));
    must_reject_schema!("open", rt.open(0, Direction::Long, 100_000_000, envelope(p, 4)));
    must_reject_schema!("close", rt.close(alice.position_id, envelope(p, 5)));
    must_reject_schema!("force_close", rt.force_close(bob.position_id, envelope(p, 6), true));
    must_reject_schema!("claim_recovery", rt.claim_recovery(alice.position_id, envelope(p, 7)));
    must_reject_schema!("harvest_dust", rt.harvest_dust(0, Direction::Long));
    // `pre_sync_bucket` skipped here: the chain-mirror layer
    // returns `BucketNotInitialized` before the engine is even
    // invoked when no bucket exists. Engine-level schema check on
    // pre_sync is exercised by clearing-core's safety_gates.rs
    // matrix; here we only assert the dispatched-to-engine paths.

    // Non-monotonic bump must be rejected at the chain-mirror layer
    // (mirrors the `SchemaBumpMustIncrease` program-side error).
    let err = rt
        .governance_bump_schema_version(2)
        .expect_err("non-monotonic bump must fail");
    assert!(
        matches!(err, MirrorError::Invariant(reason) if reason.contains("strictly increase"))
    );
}

/// `governance_migrate_position` walks a stale position's
/// `schema_version` forward; its on-chain analog is the public
/// `migrate_position` instruction. Verifies the noop-guard and the
/// monotonic walk independent of the engine path.
#[test]
fn governance_migrate_position_walks_schema_forward_with_noop_guard() {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut rt = ChainRuntime::new(market);
    rt.add_sub_pool(0, entry, 0);

    let alice = rt
        .open(0, Direction::Long, 100_000_000, envelope(entry, 1))
        .unwrap();
    assert_eq!(
        rt.positions[&alice.position_id].position.schema_version,
        1,
    );

    // Noop guard fires when position is already at the target.
    let err = rt
        .governance_migrate_position(alice.position_id)
        .expect_err("at-target migrate is a noop");
    assert!(matches!(err, MirrorError::Invariant(reason) if reason.contains("noop")));

    // Bump market to v2 (chain-mirror only — engine still locks).
    rt.governance_bump_schema_version(2).unwrap();
    rt.governance_migrate_position(alice.position_id)
        .expect("walk v1→v2 must succeed");
    assert_eq!(
        rt.positions[&alice.position_id].position.schema_version,
        2,
        "post-migrate position must carry the bumped schema"
    );

    // Unknown position id → PositionNotFound.
    let err = rt
        .governance_migrate_position(999)
        .expect_err("unknown position must fail");
    assert!(matches!(err, MirrorError::PositionNotFound(999)));
}
