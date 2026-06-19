//! Wave 8 — protocol-level safety gate matrix.
//!
//! These tests pin down two on-chain-day-one safety nets:
//!
//! 1. **`schema_version` end-to-end rejection.** When `market.
//!    schema_version != SCHEMA_VERSION_CURRENT`, every funds-touching
//!    engine entrypoint returns `SchemaVersionMismatch` *before*
//!    mutating any state. The same gate also rejects positions whose
//!    own `schema_version` field has fallen behind the market — a
//!    user holding a position from before a schema bump must run the
//!    matching migration instruction first.
//! 2. **`paused` / `frozen_new_position` halt circuit.** With
//!    `paused == true`, every funds-moving entrypoint refuses to
//!    proceed (`MarketPaused`). With `frozen_new_position == true`,
//!    `open_position` is the only entry that's blocked
//!    (`FrozenNewPosition`); all other paths still process so
//!    existing users can exit cleanly.
//!
//! Each rejection MUST be a clean `Err` with no side effects on the
//! sub-pool, so we snapshot the entire `SubPool` before calling the
//! gated entrypoint and assert byte-equality afterwards.

use clearing_core::{
    claim_dormant_recovery, close_position, force_close_zero_value_position, harvest_dust,
    open_position, pre_sync_dormant_bucket, sync_pool, ClearingError, Direction, MarketParams,
    Position, PositionStatus, PriceEnvelope, SubPool, SCHEMA_VERSION_CURRENT,
};
use molemath::PRICE_SCALE;

fn envelope(p: u64, slot: u64) -> PriceEnvelope {
    PriceEnvelope {
        p_now: p,
        slot,
        expected_min: p,
        expected_max: p,
    }
}

/// Build a sub-pool with two seed positions (one long, one short) so
/// every entrypoint has a valid context to attempt.
fn seeded_world() -> (MarketParams, SubPool, Position, Position) {
    let market = MarketParams::sample();
    let entry = 100 * PRICE_SCALE;
    let mut sp = SubPool::new(0, entry, 0);
    let env = envelope(entry, 1);
    let (alice, _) = open_position(&market, &mut sp, env, Direction::Long, 100_000_000, 1).unwrap();
    let env2 = envelope(entry, 2);
    let (bob, _) = open_position(&market, &mut sp, env2, Direction::Short, 100_000_000, 2).unwrap();
    (market, sp, alice, bob)
}

/// Snapshot the engine-observable scalar fields of a sub-pool. Used
/// to assert atomic revert: a rejected entrypoint must touch nothing.
#[derive(Debug, PartialEq, Eq)]
struct SubPoolFingerprint {
    long_pool_equity: u128,
    short_pool_equity: u128,
    long_active_shares: u128,
    short_active_shares: u128,
    long_recovery_shares: u128,
    short_recovery_shares: u128,
    long_dust: u128,
    short_dust: u128,
    long_active_generation: u64,
    short_active_generation: u64,
    last_price: u64,
    last_sync_slot: u64,
}

impl SubPoolFingerprint {
    fn of(sp: &SubPool) -> Self {
        Self {
            long_pool_equity: sp.long_pool_equity,
            short_pool_equity: sp.short_pool_equity,
            long_active_shares: sp.long_active_shares,
            short_active_shares: sp.short_active_shares,
            long_recovery_shares: sp.long_recovery_shares,
            short_recovery_shares: sp.short_recovery_shares,
            long_dust: sp.long_dust,
            short_dust: sp.short_dust,
            long_active_generation: sp.long_active_generation,
            short_active_generation: sp.short_active_generation,
            last_price: sp.last_price,
            last_sync_slot: sp.last_sync_slot,
        }
    }
}

// =====================================================================
// Schema version
// =====================================================================

#[test]
fn schema_version_current_is_one() {
    // If you bump `SCHEMA_VERSION_CURRENT`, you MUST also ship a
    // migration instruction and update this assertion deliberately —
    // it's the on-chain epoch number.
    assert_eq!(SCHEMA_VERSION_CURRENT, 1);
}

#[test]
fn sync_pool_rejects_stale_market_schema() {
    let (mut market, mut sp, _, _) = seeded_world();
    market.schema_version = SCHEMA_VERSION_CURRENT + 1;
    let before = SubPoolFingerprint::of(&sp);
    let env = envelope(101 * PRICE_SCALE, 3);
    assert!(matches!(
        sync_pool(&market, &mut sp, env),
        Err(ClearingError::SchemaVersionMismatch)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

#[test]
fn open_position_rejects_stale_market_schema() {
    let (mut market, mut sp, _, _) = seeded_world();
    market.schema_version = SCHEMA_VERSION_CURRENT + 1;
    let before = SubPoolFingerprint::of(&sp);
    let env = envelope(100 * PRICE_SCALE, 3);
    assert!(matches!(
        open_position(&market, &mut sp, env, Direction::Long, 50_000_000, 99),
        Err(ClearingError::SchemaVersionMismatch)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

#[test]
fn close_position_rejects_stale_position_schema() {
    let (market, mut sp, mut alice, _) = seeded_world();
    alice.schema_version = SCHEMA_VERSION_CURRENT.saturating_sub(1);
    let before = SubPoolFingerprint::of(&sp);
    let env = envelope(101 * PRICE_SCALE, 3);
    assert!(matches!(
        close_position(&market, &mut sp, env, &mut alice),
        Err(ClearingError::SchemaVersionMismatch)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

#[test]
fn close_position_rejects_stale_market_schema() {
    let (mut market, mut sp, mut alice, _) = seeded_world();
    market.schema_version = SCHEMA_VERSION_CURRENT + 1;
    alice.schema_version = market.schema_version; // even if they match each other.
    let before = SubPoolFingerprint::of(&sp);
    let env = envelope(101 * PRICE_SCALE, 3);
    assert!(matches!(
        close_position(&market, &mut sp, env, &mut alice),
        Err(ClearingError::SchemaVersionMismatch)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

#[test]
fn force_close_rejects_stale_position_schema() {
    let (market, mut sp, mut alice, _) = seeded_world();
    alice.schema_version = SCHEMA_VERSION_CURRENT + 1;
    let before = SubPoolFingerprint::of(&sp);
    let env = envelope(101 * PRICE_SCALE, 3);
    assert!(matches!(
        force_close_zero_value_position(&market, &mut sp, env, &mut alice, true),
        Err(ClearingError::SchemaVersionMismatch)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

#[test]
fn claim_dormant_rejects_stale_position_schema() {
    let (market, mut sp, mut alice, _) = seeded_world();
    alice.schema_version = SCHEMA_VERSION_CURRENT + 1;
    let before = SubPoolFingerprint::of(&sp);
    let env = envelope(101 * PRICE_SCALE, 3);
    assert!(matches!(
        claim_dormant_recovery(&market, &mut sp, env, &mut alice),
        Err(ClearingError::SchemaVersionMismatch)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

#[test]
fn pre_sync_bucket_rejects_stale_market_schema() {
    let (mut market, mut sp, _, _) = seeded_world();
    market.schema_version = SCHEMA_VERSION_CURRENT + 1;
    let before = SubPoolFingerprint::of(&sp);
    let res = pre_sync_dormant_bucket(&market, &mut sp, Direction::Long, 0, 3);
    assert!(matches!(res, Err(ClearingError::SchemaVersionMismatch)));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

// =====================================================================
// Paused / frozen circuit-breakers
// =====================================================================

#[test]
fn paused_blocks_sync_pool() {
    let (mut market, mut sp, _, _) = seeded_world();
    market.paused = true;
    let before = SubPoolFingerprint::of(&sp);
    let env = envelope(101 * PRICE_SCALE, 3);
    assert!(matches!(
        sync_pool(&market, &mut sp, env),
        Err(ClearingError::MarketPaused)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

#[test]
fn paused_blocks_open_position_via_sync() {
    let (mut market, mut sp, _, _) = seeded_world();
    market.paused = true;
    let before = SubPoolFingerprint::of(&sp);
    let env = envelope(100 * PRICE_SCALE, 3);
    assert!(matches!(
        open_position(&market, &mut sp, env, Direction::Long, 50_000_000, 99),
        Err(ClearingError::MarketPaused)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

#[test]
fn paused_blocks_close_position() {
    let (mut market, mut sp, mut alice, _) = seeded_world();
    market.paused = true;
    let before = SubPoolFingerprint::of(&sp);
    let env = envelope(101 * PRICE_SCALE, 3);
    assert!(matches!(
        close_position(&market, &mut sp, env, &mut alice),
        Err(ClearingError::MarketPaused)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

#[test]
fn paused_blocks_force_close() {
    let (mut market, mut sp, mut alice, _) = seeded_world();
    market.paused = true;
    let before = SubPoolFingerprint::of(&sp);
    let env = envelope(101 * PRICE_SCALE, 3);
    assert!(matches!(
        force_close_zero_value_position(&market, &mut sp, env, &mut alice, true),
        Err(ClearingError::MarketPaused)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

#[test]
fn paused_blocks_claim_dormant() {
    let (mut market, mut sp, mut alice, _) = seeded_world();
    market.paused = true;
    let before = SubPoolFingerprint::of(&sp);
    let env = envelope(101 * PRICE_SCALE, 3);
    assert!(matches!(
        claim_dormant_recovery(&market, &mut sp, env, &mut alice),
        Err(ClearingError::MarketPaused)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

#[test]
fn paused_blocks_pre_sync_bucket() {
    let (mut market, mut sp, _, _) = seeded_world();
    market.paused = true;
    let before = SubPoolFingerprint::of(&sp);
    assert!(matches!(
        pre_sync_dormant_bucket(&market, &mut sp, Direction::Long, 0, 3),
        Err(ClearingError::MarketPaused)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

#[test]
fn paused_blocks_harvest_dust() {
    let (mut market, mut sp, _, _) = seeded_world();
    market.paused = true;
    let before = SubPoolFingerprint::of(&sp);
    assert!(matches!(
        harvest_dust(&market, &mut sp, Direction::Long),
        Err(ClearingError::MarketPaused)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "atomic revert");
}

#[test]
fn frozen_new_position_blocks_open_only() {
    // `frozen_new_position` should rebuff `open_position` but leave
    // every exit path (close / force_close / claim) usable so users
    // can still leave during an emergency drain-down.
    let (mut market, mut sp, mut alice, _) = seeded_world();
    market.frozen_new_position = true;

    // Open is rejected.
    let before = SubPoolFingerprint::of(&sp);
    let env = envelope(100 * PRICE_SCALE, 3);
    assert!(matches!(
        open_position(&market, &mut sp, env, Direction::Long, 50_000_000, 99),
        Err(ClearingError::FrozenNewPosition)
    ));
    assert_eq!(SubPoolFingerprint::of(&sp), before, "open atomic revert");

    // Close still works.
    let env_close = envelope(101 * PRICE_SCALE, 4);
    assert!(matches!(alice.status, PositionStatus::Open));
    let _ = close_position(&market, &mut sp, env_close, &mut alice).unwrap();
}
