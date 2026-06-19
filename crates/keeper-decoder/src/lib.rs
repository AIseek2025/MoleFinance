//! Wave 14 — host- and wasm32-buildable schema crate.
//!
//! This crate is a **schema-only** extraction of what wave 10 / 11 had
//! lived inline in `keeper-rpc::accounts`. Two design constraints:
//!
//! 1. **Zero Solana dependency.** No `solana-sdk`, no `solana-pubkey`,
//!    no `solana-client`. Just `borsh` (with `derive`) and `thiserror`.
//!    This is what makes the crate compile cleanly on
//!    `wasm32-unknown-unknown` so wave 15 can `wasm-pack build`
//!    without any further refactoring.
//! 2. **Single source of truth.** `keeper-rpc::accounts` re-exports
//!    everything here, so existing call sites
//!    (`keeper_rpc::accounts::OnchainSubPool`,
//!    `keeper_rpc::Pubkey32`) keep working unchanged. Future Borsh
//!    schema bumps land here exactly once and propagate to both the
//!    Rust keeper bot and the wasm-shipped frontend decoder.
//!
//! Layout MUST match `programs/mole-option/src/state.rs` byte-for-
//! byte. See `crates/clearing-core/tests/onchain_layout.rs` for the
//! property tests that pin the on-chain account size.

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(unsafe_code))]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use borsh::{BorshDeserialize, BorshSerialize};

/// Wave 15 — Anchor instruction encoders + discriminator helpers.
/// Exposed at the crate root so consumers can `use keeper_decoder::ix;`
/// without going through a deeper path.
pub mod ix;

/// Wave 15 — host-side state machine for the `KeeperLeaderLock` PDA.
/// Migrated here from `chain-mirror::leader_lock` so the layout, the
/// state machine, and the wasm-shipped frontend mirror all live in
/// one schema-only crate.
pub mod leader_lock;

/// Wave 15 — wasm-bindgen FFI surface for the frontend
/// `wasm-pack`-built artifact. Compiled only under `--features wasm`.
#[cfg(feature = "wasm")]
pub mod wasm_bridge;

/// 32-byte Solana pubkey, kept newtype-flat (`[u8; 32]`) so the
/// crate's public API doesn't leak `solana-pubkey` into wasm builds.
/// `keeper-rpc::solana` (host-only, `--features solana-rpc`) provides
/// `From<Pubkey32> for solana_pubkey::Pubkey` and the inverse.
pub type Pubkey32 = [u8; 32];

/// Anchor's standard account-discriminator length (bytes).
pub const ANCHOR_DISCRIMINATOR_LEN: usize = 8;

/// Decode `data` (an account's full `data` blob) as a borsh-encoded
/// Anchor account by stripping the 8-byte discriminator prefix.
///
/// The discriminator itself is intentionally not validated against
/// `sha256("account:<TypeName>")[..8]`: production keepers fetch
/// accounts via PDA derivation, which already binds the account
/// type. Callers that need extra paranoia can pass an explicit
/// discriminator into [`decode_anchor_account_with_discriminator`].
pub fn decode_anchor_account<T: BorshDeserialize>(data: &[u8]) -> Result<T, AccountDecodeError> {
    if data.len() < ANCHOR_DISCRIMINATOR_LEN {
        return Err(AccountDecodeError::TooShort {
            len: data.len(),
            min: ANCHOR_DISCRIMINATOR_LEN,
        });
    }
    let body = &data[ANCHOR_DISCRIMINATOR_LEN..];
    T::try_from_slice(body).map_err(|e| AccountDecodeError::Borsh(e.to_string()))
}

/// Like [`decode_anchor_account`] but additionally verifies the
/// discriminator equals `expected`.
pub fn decode_anchor_account_with_discriminator<T: BorshDeserialize>(
    data: &[u8],
    expected: &[u8; 8],
) -> Result<T, AccountDecodeError> {
    if data.len() < ANCHOR_DISCRIMINATOR_LEN {
        return Err(AccountDecodeError::TooShort {
            len: data.len(),
            min: ANCHOR_DISCRIMINATOR_LEN,
        });
    }
    let (disc, body) = data.split_at(ANCHOR_DISCRIMINATOR_LEN);
    if disc != expected {
        return Err(AccountDecodeError::DiscriminatorMismatch {
            got: disc.try_into().expect("8 bytes"),
            expected: *expected,
        });
    }
    T::try_from_slice(body).map_err(|e| AccountDecodeError::Borsh(e.to_string()))
}

/// Encode an Anchor account `value` with the given discriminator,
/// the same way the on-chain program does. Useful for tests that
/// need to assemble a `MockAccountFetcher` payload.
pub fn encode_anchor_account<T: BorshSerialize>(
    value: &T,
    discriminator: &[u8; 8],
) -> Result<Vec<u8>, AccountDecodeError> {
    let mut out = Vec::with_capacity(ANCHOR_DISCRIMINATOR_LEN + 256);
    out.extend_from_slice(discriminator);
    value
        .serialize(&mut out)
        .map_err(|e| AccountDecodeError::Borsh(e.to_string()))?;
    Ok(out)
}

/// Errors produced by the account decoders.
///
/// These are deliberately stringly-typed (the Borsh variant carries
/// `String`) rather than re-exporting `borsh::io::Error` so the
/// crate's public surface stays Borsh-version-stable. Auditors can
/// pattern-match on the variants without committing to a specific
/// borsh release.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum AccountDecodeError {
    /// Account data is shorter than 8 bytes (the discriminator
    /// prefix). Either the account doesn't exist or the fetcher
    /// returned an unrelated payload.
    #[error("anchor account too short: got {len} bytes, need at least {min}")]
    TooShort {
        /// Bytes actually returned.
        len: usize,
        /// Minimum required (always 8).
        min: usize,
    },
    /// Discriminator did not match the expected pattern.
    #[error("anchor discriminator mismatch: got {got:?}, expected {expected:?}")]
    DiscriminatorMismatch {
        /// Discriminator bytes parsed off the account.
        got: [u8; 8],
        /// Discriminator the caller asserted.
        expected: [u8; 8],
    },
    /// Body failed to deserialize.
    #[error("borsh decode failed: {0}")]
    Borsh(String),
}

// =====================================================================
// Account mirrors
// =====================================================================

/// Mirror of the on-chain `SubPool` account
/// (`programs/mole-option/src/state.rs`). Field order MUST match the
/// program; do not reorder without bumping the program-side
/// `schema_version`.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct OnchainSubPool {
    /// Parent `Market` PDA pubkey.
    pub market: Pubkey32,
    /// Sub pool id within the market.
    pub sub_pool_id: u32,
    /// Long-side directional pool equity (collateral minor units).
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
    /// Long-side active generation tag.
    pub long_active_generation: u64,
    /// Short-side active generation tag.
    pub short_active_generation: u64,
    /// Most recent oracle price written by `sync_pool` (PRICE_SCALE).
    pub last_price: u64,
    /// Slot of the most recent `sync_pool`.
    pub last_sync_slot: u64,
    /// Long-side dust accumulator.
    pub long_dust: u128,
    /// Short-side dust accumulator.
    pub short_dust: u128,
    /// Long-side dormant bucket count cap accounting.
    pub long_dormant_bucket_count: u32,
    /// Short-side dormant bucket count cap accounting.
    pub short_dormant_bucket_count: u32,
    /// PDA bump.
    pub bump: u8,
    /// Padding to 8-byte align next account.
    pub _pad: [u8; 7],
}

/// Mirror of the on-chain `DormantBucket` account.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct OnchainDormantBucket {
    /// Parent `SubPool` PDA pubkey.
    pub sub_pool: Pubkey32,
    /// True iff this bucket holds long-side recovery shares.
    pub direction_is_long: bool,
    /// Tick index this bucket aggregates.
    pub zero_price_tick: i64,
    /// Anchor price this bucket was opened at.
    pub anchor_price: u64,
    /// Total recovery shares held by this bucket.
    pub total_recovery_shares: u128,
    /// Total recovery notional held by this bucket.
    pub total_recovery_notional: u128,
    /// Funds attributed to this bucket; redeemable by claim.
    pub accrued_value: u128,
    /// Number of distinct dormant positions in this bucket.
    pub position_count: u64,
    /// Index of the last `DistEntry` applied to this bucket.
    pub last_applied_index: u64,
    /// PDA bump.
    pub bump: u8,
    /// Padding.
    pub _pad: [u8; 6],
}

/// Mirror of the on-chain `DistEntry` element.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnchainDistEntry {
    /// Absolute event index.
    pub event_index: u64,
    /// Pool price stamped on this entry.
    pub p_at_event: u64,
    /// Total outstanding shares in the affected direction at the
    /// time the entry was appended.
    pub total_outstanding_at_event: u128,
    /// Total amount the engine routed to this direction's dormant
    /// share class (input).
    pub total_alloc_input: u128,
    /// Cumulative amount actually allocated to buckets so far.
    pub allocated_sum_observed: u128,
}

/// Mirror of the on-chain `DistributionLedger` account.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct OnchainDistributionLedger {
    /// Parent `SubPool` PDA pubkey.
    pub sub_pool: Pubkey32,
    /// Ledger direction.
    pub direction_is_long: bool,
    /// Hard cap on `entries.len()`.
    pub max_entries: u32,
    /// Number of GC'd events at the front of the logical ring.
    pub gc_offset: u64,
    /// Absolute index of the next event to be appended.
    pub next_event_index: u64,
    /// Cached `accrued_value` sum across this direction's buckets.
    pub accrued_value_total: u128,
    /// Lazy-mode in-flight allocations.
    pub pending_distribution_total: u128,
    /// Live entry count.
    pub entry_count: u32,
    /// Live entries.
    pub entries: Vec<OnchainDistEntry>,
    /// PDA bump.
    pub bump: u8,
    /// Padding.
    pub _pad: [u8; 7],
}

/// Mirror of the on-chain `Market` account — only the subset the
/// keeper bot needs (`sub_pool_count`, `paused`, `frozen_new_position`,
/// `schema_version`, `tick_aggregation_factor`, `price_tick`). Other
/// fields are decoded but unused.
///
/// Kept as a *complete* mirror so future schema bumps can be
/// detected by Borsh-decode failures rather than silently misaligning
/// the keeper bot's view of the market.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct OnchainMarket {
    /// Parent `GlobalConfig` PDA.
    pub global_config: Pubkey32,
    /// Symbol bytes.
    pub symbol: [u8; 16],
    /// Collateral mint pubkey.
    pub collateral_mint: Pubkey32,
    /// Vault token account pubkey.
    pub vault: Pubkey32,
    /// Fee vault token account pubkey.
    pub fee_vault: Pubkey32,
    /// Oracle price feed pubkey.
    pub oracle_price_feed: Pubkey32,
    /// Oracle program id.
    pub oracle_program_id: Pubkey32,
    /// Leverage in basis points.
    pub leverage_bps: u32,
    /// Minimum margin per position.
    pub min_margin: u64,
    /// Max margin per position.
    pub max_margin_per_position: u64,
    /// Max total principal market-wide.
    pub max_total_principal: u128,
    /// Max total notional market-wide.
    pub max_total_notional: u128,
    /// Currently outstanding total principal.
    pub current_total_principal: u128,
    /// Currently outstanding total notional.
    pub current_total_notional: u128,
    /// Open fee in basis points.
    pub open_fee_bps: u16,
    /// Max oracle age (seconds).
    pub max_oracle_age_seconds: i64,
    /// Max oracle age (slots).
    pub max_oracle_age_slots: u64,
    /// Max oracle confidence in basis points.
    pub max_confidence_bps: u16,
    /// Max single-step price move in basis points per `sync_pool`.
    pub max_price_move_bps_per_sync: u32,
    /// Price tick (PRICE_SCALE units).
    pub price_tick: u64,
    /// Tick aggregation factor.
    pub tick_aggregation_factor: u32,
    /// Max dormant bucket count per direction.
    pub max_dormant_bucket_count_per_direction: u32,
    /// Dilution safety basis points.
    pub dilution_safety_bps: u32,
    /// Max idle slots.
    pub max_idle_slots: u64,
    /// Pause flag.
    pub paused: bool,
    /// Freeze-new-position flag.
    pub frozen_new_position: bool,
    /// Schema version.
    pub schema_version: u16,
    /// Number of sub pools.
    pub sub_pool_count: u32,
    /// Distribute mode (0=eager, 1=lazy).
    pub dormant_distribute_mode: u8,
    /// Max pending entries applied per `pre_sync_dormant_bucket` tx.
    pub max_pending_apply_per_tx: u32,
    /// Hard cap on per-direction distribution ledger ring size.
    pub max_distribution_ledger_size: u32,
    /// PDA bump.
    pub bump: u8,
    /// Padding.
    pub _pad: [u8; 2],
}

/// **Wave 21.** Mirror of the on-chain `Position` PDA. Byte-for-
/// byte aligned with `programs/mole-option/src/state.rs::Position`.
///
/// Unlocks two production paths:
///
///   1. **`websocketAdapter` per-position routing.** The frontend's
///      `accountSubscribe` for `Position` PDAs decodes via this
///      mirror, lifts `position.market` into
///      `PositionSummary.marketPdaHex`, and the wave-20
///      `selectActiveMarketSnapshot` filter starts working on
///      live data instead of just the mock generator.
///   2. **`ops-toolkit prober` per-market open-interest probe.**
///      A future check (wave 22+) can decode every `Position` via
///      `getProgramAccounts` (memcmp on discriminator) and feed
///      `MarketFacts.current_total_principal` etc. straight from
///      the on-chain record rather than trusting the indexer's
///      cached projection.
///
/// Padding-layout note: matches Anchor's `space = LEN` semantics.
/// The on-chain `Position::LEN` = `8 + 32*3 + 8 + 1 + 1 + 8 + 4 +
/// 16 + 16*2 + 8 + 1 + 8 + 8 + 8 + 8 + 8*3 + 2 + 1 + 5 = 247`
/// bytes; this mirror's borsh-encoded body is exactly 239 bytes
/// (247 - 8 disc).
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct OnchainPosition {
    /// Owner wallet.
    pub owner: Pubkey32,
    /// Owning `Market` PDA. Frontend lifts this into
    /// `PositionSummary.marketPdaHex` for wave-20 multi-market
    /// position routing.
    pub market: Pubkey32,
    /// Owning `SubPool` PDA.
    pub sub_pool: Pubkey32,
    /// Per-market monotonic position id.
    pub position_id: u64,
    /// Direction flag — true == Long, false == Short.
    pub direction_is_long: bool,
    /// Lifecycle status: 0 = Open, 1 = Dormant, 2 = Closed.
    pub status: u8,
    /// Principal collateral committed by the trader (microUSDC).
    pub principal: u64,
    /// Leverage in basis points.
    pub leverage_bps: u32,
    /// Position notional (microUSDC).
    pub notional: u128,
    /// Active equity shares — claim against the live pool.
    pub active_shares: u128,
    /// Recovery equity shares — claim against the dormant bucket
    /// the position was rotated into (if any).
    pub recovery_shares: u128,
    /// Bucket tick the position last rotated into. Meaningful
    /// only when `has_recovery_bucket == true`.
    pub recovery_bucket_tick: i64,
    /// Whether `recovery_shares` and `recovery_bucket_tick` are
    /// live. Becomes false again once the dormant bucket
    /// distributes / closes.
    pub has_recovery_bucket: bool,
    /// "Locked-loss" reference price — the side that lost set
    /// this once and it never changes. Used by the equity-
    /// recoverable invariant.
    pub zero_price: u64,
    /// Mid-price at open (PRICE_SCALE units).
    pub entry_price: u64,
    /// Slot of the most recent `sync_pool` that affected this
    /// position. Lets indexers age individual positions.
    pub last_sync_slot: u64,
    /// Generation number of the active pool the position belongs
    /// to (rotates monotonically).
    pub active_generation: u64,
    /// UNIX seconds at `open_position`.
    pub opened_at: i64,
    /// UNIX seconds at the most recent state-changing tx.
    pub updated_at: i64,
    /// UNIX seconds at `close_position`. Zero while open.
    pub closed_at: i64,
    /// On-chain schema version this position was opened against.
    /// `migrate_position` bumps this when the global market
    /// schema bumps.
    pub schema_version: u16,
    /// PDA bump.
    pub bump: u8,
    /// Padding.
    pub _pad: [u8; 5],
}

/// JSON-style schema descriptor — emitted by the host-only
/// [`schema_descriptor_json`] helper so the frontend's wave-14
/// TypeScript Borsh decoder can sanity-check its hand-written
/// schemas against this crate's authoritative shape at build time.
///
/// Field listing order matches the Borsh on-wire order. The output
/// is intentionally ASCII-only and stable: the frontend's CI parses
/// it and `diff`s against the committed `frontend/src/decoder/
/// onchain.schema.json`.
pub fn schema_descriptor_json() -> String {
    // Hand-rolled to avoid a `serde_json` dep — keeping this crate's
    // tree minimal is the whole point of the wasm32 split.
    let mut out = String::with_capacity(2048);
    out.push('{');
    out.push_str("\"OnchainSubPool\":[");
    push_field(&mut out, true, "market", "Pubkey32");
    push_field(&mut out, false, "sub_pool_id", "u32");
    push_field(&mut out, false, "long_pool_equity", "u128");
    push_field(&mut out, false, "short_pool_equity", "u128");
    push_field(&mut out, false, "long_active_shares", "u128");
    push_field(&mut out, false, "short_active_shares", "u128");
    push_field(&mut out, false, "long_recovery_shares", "u128");
    push_field(&mut out, false, "short_recovery_shares", "u128");
    push_field(&mut out, false, "long_active_notional", "u128");
    push_field(&mut out, false, "short_active_notional", "u128");
    push_field(&mut out, false, "long_active_generation", "u64");
    push_field(&mut out, false, "short_active_generation", "u64");
    push_field(&mut out, false, "last_price", "u64");
    push_field(&mut out, false, "last_sync_slot", "u64");
    push_field(&mut out, false, "long_dust", "u128");
    push_field(&mut out, false, "short_dust", "u128");
    push_field(&mut out, false, "long_dormant_bucket_count", "u32");
    push_field(&mut out, false, "short_dormant_bucket_count", "u32");
    push_field(&mut out, false, "bump", "u8");
    push_field(&mut out, false, "_pad", "array<u8,7>");
    out.push_str("],\"OnchainDormantBucket\":[");
    push_field(&mut out, true, "sub_pool", "Pubkey32");
    push_field(&mut out, false, "direction_is_long", "bool");
    push_field(&mut out, false, "zero_price_tick", "i64");
    push_field(&mut out, false, "anchor_price", "u64");
    push_field(&mut out, false, "total_recovery_shares", "u128");
    push_field(&mut out, false, "total_recovery_notional", "u128");
    push_field(&mut out, false, "accrued_value", "u128");
    push_field(&mut out, false, "position_count", "u64");
    push_field(&mut out, false, "last_applied_index", "u64");
    push_field(&mut out, false, "bump", "u8");
    push_field(&mut out, false, "_pad", "array<u8,6>");
    out.push_str("],\"OnchainDistEntry\":[");
    push_field(&mut out, true, "event_index", "u64");
    push_field(&mut out, false, "p_at_event", "u64");
    push_field(&mut out, false, "total_outstanding_at_event", "u128");
    push_field(&mut out, false, "total_alloc_input", "u128");
    push_field(&mut out, false, "allocated_sum_observed", "u128");
    out.push_str("],\"OnchainDistributionLedger\":[");
    push_field(&mut out, true, "sub_pool", "Pubkey32");
    push_field(&mut out, false, "direction_is_long", "bool");
    push_field(&mut out, false, "max_entries", "u32");
    push_field(&mut out, false, "gc_offset", "u64");
    push_field(&mut out, false, "next_event_index", "u64");
    push_field(&mut out, false, "accrued_value_total", "u128");
    push_field(&mut out, false, "pending_distribution_total", "u128");
    push_field(&mut out, false, "entry_count", "u32");
    push_field(&mut out, false, "entries", "vec<OnchainDistEntry>");
    push_field(&mut out, false, "bump", "u8");
    push_field(&mut out, false, "_pad", "array<u8,7>");
    out.push_str("],\"OnchainMarket\":[");
    push_field(&mut out, true, "global_config", "Pubkey32");
    push_field(&mut out, false, "symbol", "array<u8,16>");
    push_field(&mut out, false, "collateral_mint", "Pubkey32");
    push_field(&mut out, false, "vault", "Pubkey32");
    push_field(&mut out, false, "fee_vault", "Pubkey32");
    push_field(&mut out, false, "oracle_price_feed", "Pubkey32");
    push_field(&mut out, false, "oracle_program_id", "Pubkey32");
    push_field(&mut out, false, "leverage_bps", "u32");
    push_field(&mut out, false, "min_margin", "u64");
    push_field(&mut out, false, "max_margin_per_position", "u64");
    push_field(&mut out, false, "max_total_principal", "u128");
    push_field(&mut out, false, "max_total_notional", "u128");
    push_field(&mut out, false, "current_total_principal", "u128");
    push_field(&mut out, false, "current_total_notional", "u128");
    push_field(&mut out, false, "open_fee_bps", "u16");
    push_field(&mut out, false, "max_oracle_age_seconds", "i64");
    push_field(&mut out, false, "max_oracle_age_slots", "u64");
    push_field(&mut out, false, "max_confidence_bps", "u16");
    push_field(&mut out, false, "max_price_move_bps_per_sync", "u32");
    push_field(&mut out, false, "price_tick", "u64");
    push_field(&mut out, false, "tick_aggregation_factor", "u32");
    push_field(&mut out, false, "max_dormant_bucket_count_per_direction", "u32");
    push_field(&mut out, false, "dilution_safety_bps", "u32");
    push_field(&mut out, false, "max_idle_slots", "u64");
    push_field(&mut out, false, "paused", "bool");
    push_field(&mut out, false, "frozen_new_position", "bool");
    push_field(&mut out, false, "schema_version", "u16");
    push_field(&mut out, false, "sub_pool_count", "u32");
    push_field(&mut out, false, "dormant_distribute_mode", "u8");
    push_field(&mut out, false, "max_pending_apply_per_tx", "u32");
    push_field(&mut out, false, "max_distribution_ledger_size", "u32");
    push_field(&mut out, false, "bump", "u8");
    push_field(&mut out, false, "_pad", "array<u8,2>");
    out.push_str("],\"OnchainPosition\":[");
    push_field(&mut out, true, "owner", "Pubkey32");
    push_field(&mut out, false, "market", "Pubkey32");
    push_field(&mut out, false, "sub_pool", "Pubkey32");
    push_field(&mut out, false, "position_id", "u64");
    push_field(&mut out, false, "direction_is_long", "bool");
    push_field(&mut out, false, "status", "u8");
    push_field(&mut out, false, "principal", "u64");
    push_field(&mut out, false, "leverage_bps", "u32");
    push_field(&mut out, false, "notional", "u128");
    push_field(&mut out, false, "active_shares", "u128");
    push_field(&mut out, false, "recovery_shares", "u128");
    push_field(&mut out, false, "recovery_bucket_tick", "i64");
    push_field(&mut out, false, "has_recovery_bucket", "bool");
    push_field(&mut out, false, "zero_price", "u64");
    push_field(&mut out, false, "entry_price", "u64");
    push_field(&mut out, false, "last_sync_slot", "u64");
    push_field(&mut out, false, "active_generation", "u64");
    push_field(&mut out, false, "opened_at", "i64");
    push_field(&mut out, false, "updated_at", "i64");
    push_field(&mut out, false, "closed_at", "i64");
    push_field(&mut out, false, "schema_version", "u16");
    push_field(&mut out, false, "bump", "u8");
    push_field(&mut out, false, "_pad", "array<u8,5>");
    out.push_str("]}");
    out
}

fn push_field(out: &mut String, first: bool, name: &str, ty: &str) {
    if !first {
        out.push(',');
    }
    let row = format!("{{\"name\":\"{name}\",\"type\":\"{ty}\"}}");
    out.push_str(&row);
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::vec;

    fn dummy_pubkey(seed: u8) -> Pubkey32 {
        let mut p = [0u8; 32];
        p[0] = seed;
        p
    }

    fn dummy_sub_pool() -> OnchainSubPool {
        OnchainSubPool {
            market: dummy_pubkey(1),
            sub_pool_id: 7,
            long_pool_equity: 1_234_567_890,
            short_pool_equity: 234_567_890,
            long_active_shares: 100_000,
            short_active_shares: 90_000,
            long_recovery_shares: 50,
            short_recovery_shares: 40,
            long_active_notional: 5_000_000,
            short_active_notional: 4_000_000,
            long_active_generation: 3,
            short_active_generation: 2,
            last_price: 100_000_000,
            last_sync_slot: 12345,
            long_dust: 7,
            short_dust: 9,
            long_dormant_bucket_count: 12,
            short_dormant_bucket_count: 8,
            bump: 254,
            _pad: [0; 7],
        }
    }

    fn dummy_bucket() -> OnchainDormantBucket {
        OnchainDormantBucket {
            sub_pool: dummy_pubkey(2),
            direction_is_long: true,
            zero_price_tick: -1234,
            anchor_price: 50_000_000,
            total_recovery_shares: 7_777,
            total_recovery_notional: 8_888,
            accrued_value: 9_999,
            position_count: 4,
            last_applied_index: 12,
            bump: 253,
            _pad: [0; 6],
        }
    }

    fn dummy_ledger() -> OnchainDistributionLedger {
        OnchainDistributionLedger {
            sub_pool: dummy_pubkey(3),
            direction_is_long: false,
            max_entries: 64,
            gc_offset: 5,
            next_event_index: 17,
            accrued_value_total: 100,
            pending_distribution_total: 12,
            entry_count: 1,
            entries: vec![OnchainDistEntry {
                event_index: 16,
                p_at_event: 50_000_000,
                total_outstanding_at_event: 1_000_000,
                total_alloc_input: 100,
                allocated_sum_observed: 100,
            }],
            bump: 252,
            _pad: [0; 7],
        }
    }

    fn dummy_market() -> OnchainMarket {
        OnchainMarket {
            global_config: dummy_pubkey(4),
            symbol: *b"SOL-USD\0\0\0\0\0\0\0\0\0",
            collateral_mint: dummy_pubkey(5),
            vault: dummy_pubkey(6),
            fee_vault: dummy_pubkey(7),
            oracle_price_feed: dummy_pubkey(8),
            oracle_program_id: dummy_pubkey(9),
            leverage_bps: 5_000,
            min_margin: 1_000_000,
            max_margin_per_position: 100_000_000_000,
            max_total_principal: 5_000_000_000_000,
            max_total_notional: 50_000_000_000_000,
            current_total_principal: 1_234_567_890,
            current_total_notional: 12_345_678_900,
            open_fee_bps: 5,
            max_oracle_age_seconds: 60,
            max_oracle_age_slots: 64,
            max_confidence_bps: 200,
            max_price_move_bps_per_sync: 1_000,
            price_tick: 10_000,
            tick_aggregation_factor: 10,
            max_dormant_bucket_count_per_direction: 16,
            dilution_safety_bps: 100,
            max_idle_slots: 128,
            paused: false,
            frozen_new_position: false,
            schema_version: 1,
            sub_pool_count: 4,
            dormant_distribute_mode: 1,
            max_pending_apply_per_tx: 8,
            max_distribution_ledger_size: 64,
            bump: 251,
            _pad: [0; 2],
        }
    }

    fn dummy_position() -> OnchainPosition {
        OnchainPosition {
            owner: dummy_pubkey(11),
            market: dummy_pubkey(12),
            sub_pool: dummy_pubkey(13),
            position_id: 42,
            direction_is_long: true,
            status: 0,
            principal: 1_000_000,
            leverage_bps: 5_000,
            notional: 5_000_000,
            active_shares: 100,
            recovery_shares: 0,
            recovery_bucket_tick: 0,
            has_recovery_bucket: false,
            zero_price: 0,
            entry_price: 60_123_456_000,
            last_sync_slot: 217_000_000,
            active_generation: 3,
            opened_at: 1_700_000_000,
            updated_at: 1_700_000_120,
            closed_at: 0,
            schema_version: 1,
            bump: 252,
            _pad: [0; 5],
        }
    }

    /// All four major mirrors round-trip cleanly through Borsh +
    /// the discriminator wrapper.
    #[test]
    fn account_mirrors_round_trip_through_borsh() {
        let disc = [1u8; 8];
        let raw_sp = encode_anchor_account(&dummy_sub_pool(), &disc).unwrap();
        let raw_b = encode_anchor_account(&dummy_bucket(), &disc).unwrap();
        let raw_l = encode_anchor_account(&dummy_ledger(), &disc).unwrap();
        let raw_m = encode_anchor_account(&dummy_market(), &disc).unwrap();
        let sp2: OnchainSubPool = decode_anchor_account(&raw_sp).unwrap();
        assert_eq!(sp2, dummy_sub_pool());
        let b2: OnchainDormantBucket = decode_anchor_account(&raw_b).unwrap();
        assert_eq!(b2, dummy_bucket());
        let l2: OnchainDistributionLedger = decode_anchor_account(&raw_l).unwrap();
        assert_eq!(l2, dummy_ledger());
        let m2: OnchainMarket = decode_anchor_account(&raw_m).unwrap();
        assert_eq!(m2, dummy_market());
    }

    /// Discriminator-strict path rejects mismatch and never panics.
    #[test]
    fn strict_decoder_rejects_discriminator_mismatch() {
        let raw = encode_anchor_account(&dummy_sub_pool(), &[1u8; 8]).unwrap();
        let bad = [2u8; 8];
        let err = decode_anchor_account_with_discriminator::<OnchainSubPool>(&raw, &bad)
            .unwrap_err();
        assert!(matches!(err, AccountDecodeError::DiscriminatorMismatch { .. }));
    }

    /// Truncated-payload path returns `TooShort` rather than panicking.
    #[test]
    fn truncated_payload_returns_too_short() {
        let err = decode_anchor_account::<OnchainSubPool>(&[0u8; 4]).unwrap_err();
        assert!(matches!(err, AccountDecodeError::TooShort { .. }));
    }

    /// Empty payload — the most degenerate case — also returns
    /// `TooShort` instead of an arithmetic underflow.
    #[test]
    fn empty_payload_returns_too_short() {
        let err = decode_anchor_account::<OnchainSubPool>(&[]).unwrap_err();
        assert!(matches!(err, AccountDecodeError::TooShort { len: 0, min: 8 }));
    }

    /// Body-decode failure surfaces as the `Borsh` variant carrying
    /// a non-empty error string.
    #[test]
    fn malformed_body_surfaces_borsh_variant() {
        // 8 bytes of disc + 1 byte of body — too short for any
        // OnchainSubPool field.
        let raw = [0u8; 9];
        let err = decode_anchor_account::<OnchainSubPool>(&raw).unwrap_err();
        match err {
            AccountDecodeError::Borsh(msg) => assert!(!msg.is_empty()),
            other => panic!("expected Borsh, got {other:?}"),
        }
    }

    /// `OnchainDistEntry` has no `_pad` field; the wave-14 mapping
    /// doc confirms it's pure data. Round-trip standalone.
    #[test]
    fn dist_entry_round_trips_without_padding() {
        let e = OnchainDistEntry {
            event_index: 42,
            p_at_event: 100,
            total_outstanding_at_event: 200,
            total_alloc_input: 300,
            allocated_sum_observed: 250,
        };
        let mut buf = Vec::new();
        e.serialize(&mut buf).unwrap();
        let e2 = OnchainDistEntry::try_from_slice(&buf).unwrap();
        assert_eq!(e, e2);
    }

    /// `Vec<OnchainDistEntry>` round-trips inside the parent ledger
    /// even when `entry_count` and `entries.len()` disagree (Borsh
    /// uses the explicit `entries` length prefix; `entry_count` is
    /// the on-chain *logical* count).
    #[test]
    fn ledger_entries_vec_independent_of_entry_count() {
        let mut l = dummy_ledger();
        l.entry_count = 7; // advertise more than we actually carry
        let mut buf = Vec::new();
        l.serialize(&mut buf).unwrap();
        let l2 = OnchainDistributionLedger::try_from_slice(&buf).unwrap();
        assert_eq!(l, l2);
    }

    /// Pubkey32 is `[u8; 32]` — exactly 32 bytes encoded inline by
    /// Borsh (no length prefix). Pin this since the wasm consumer
    /// relies on it.
    #[test]
    fn pubkey32_is_thirty_two_bytes_inline() {
        let pk: Pubkey32 = [9u8; 32];
        let mut buf = Vec::new();
        pk.serialize(&mut buf).unwrap();
        assert_eq!(buf.len(), 32);
        assert_eq!(buf, vec![9u8; 32]);
    }

    /// `ANCHOR_DISCRIMINATOR_LEN` is the constant the on-chain
    /// program assumes; locking it here so a future bump in the
    /// keeper crate would have to consciously override.
    #[test]
    fn anchor_discriminator_len_is_eight() {
        assert_eq!(ANCHOR_DISCRIMINATOR_LEN, 8);
    }

    /// `schema_descriptor_json` emits the complete field listing for
    /// every `Onchain*` struct in declaration order. The TypeScript
    /// frontend's wave-14 `verify-schema-parity-ts.sh` parses this.
    #[test]
    fn schema_descriptor_json_lists_every_struct_and_field() {
        let json = schema_descriptor_json();
        for name in [
            "OnchainSubPool",
            "OnchainDormantBucket",
            "OnchainDistEntry",
            "OnchainDistributionLedger",
            "OnchainMarket",
            "OnchainPosition",
        ] {
            assert!(
                json.contains(&format!("\"{name}\":[")),
                "missing struct {name} in schema descriptor"
            );
        }
        for field in [
            "long_pool_equity",
            "short_dormant_bucket_count",
            "zero_price_tick",
            "max_distribution_ledger_size",
            "schema_version",
            "direction_is_long",
            "has_recovery_bucket",
            "active_generation",
        ] {
            assert!(
                json.contains(&format!("\"name\":\"{field}\"")),
                "missing field {field} in schema descriptor"
            );
        }
        assert!(json.starts_with('{') && json.ends_with('}'));
    }

    /// Schema descriptor contains the same field count the
    /// `scripts/verify-schema-parity.sh` script counts in
    /// `accounts.rs` — wave-21 brings the total from 80 to 103
    /// (+23 `OnchainPosition` fields: owner / market / sub_pool /
    /// position_id / direction_is_long / status / principal /
    /// leverage_bps / notional / active_shares / recovery_shares /
    /// recovery_bucket_tick / has_recovery_bucket / zero_price /
    /// entry_price / last_sync_slot / active_generation /
    /// opened_at / updated_at / closed_at / schema_version /
    /// bump / _pad).
    #[test]
    fn schema_descriptor_contains_one_hundred_three_field_entries() {
        let json = schema_descriptor_json();
        let count = json.matches("\"name\":\"").count();
        assert_eq!(count, 103, "expected 103 fields, descriptor has {count}");
    }

    /// Wave 21 — `OnchainPosition` round-trips byte-for-byte
    /// through the discriminator-prefixed encoder.
    #[test]
    fn position_mirror_round_trips_through_borsh() {
        let raw = encode_anchor_account(&dummy_position(), &[7u8; 8]).unwrap();
        let p2: OnchainPosition = decode_anchor_account(&raw).unwrap();
        assert_eq!(p2, dummy_position());
    }

    /// Wave 21 — body length is exactly 239 bytes (Position::LEN
    /// = 247, minus 8 disc). Pinning this makes a future schema
    /// bump observable as a test failure, mirroring the same
    /// guard the on-chain `Position::LEN` constant provides.
    #[test]
    fn position_borsh_body_is_two_hundred_thirty_nine_bytes() {
        let mut buf = Vec::new();
        dummy_position().serialize(&mut buf).unwrap();
        assert_eq!(
            buf.len(),
            239,
            "OnchainPosition borsh body length drifted from 239"
        );
    }

    /// Wave 21 — `position.market` survives the round-trip with
    /// every byte intact. This is the field the wave-22 frontend
    /// `websocketAdapter` lifts into `PositionSummary.marketPdaHex`,
    /// so the byte-level guarantee is the wave-20 multi-market
    /// filter's correctness backstop.
    #[test]
    fn position_market_pubkey_round_trips_intact() {
        let mut p = dummy_position();
        let target: Pubkey32 = [0xABu8; 32];
        p.market = target;
        let raw = encode_anchor_account(&p, &[1u8; 8]).unwrap();
        let p2: OnchainPosition = decode_anchor_account(&raw).unwrap();
        assert_eq!(p2.market, target);
    }

    /// Wave 21 — `direction_is_long` and `status` use Borsh's
    /// 1-byte bool / u8 encodings; pinning so a future "use
    /// `Direction` enum on chain" change can't silently shift
    /// downstream bytes.
    #[test]
    fn position_status_and_direction_are_one_byte_each() {
        let mut p = dummy_position();
        p.direction_is_long = false;
        p.status = 1;
        let mut buf = Vec::new();
        p.serialize(&mut buf).unwrap();
        // owner(32) + market(32) + sub_pool(32) + position_id(8) = 104
        // direction at offset 104 (1 byte) + status at offset 105 (1 byte)
        assert_eq!(buf[104], 0, "direction_is_long false should serialize as 0");
        assert_eq!(buf[105], 1, "status 1 should serialize as 1");
    }

    /// Wave 21 — discriminator-strict path on `OnchainPosition`
    /// rejects a wrong discriminator the same way it does on
    /// `OnchainSubPool` etc.
    #[test]
    fn position_strict_decoder_rejects_discriminator_mismatch() {
        let raw = encode_anchor_account(&dummy_position(), &[1u8; 8]).unwrap();
        let err = decode_anchor_account_with_discriminator::<OnchainPosition>(
            &raw,
            &[2u8; 8],
        )
        .unwrap_err();
        assert!(matches!(err, AccountDecodeError::DiscriminatorMismatch { .. }));
    }

    /// Borsh-decode with a discriminator that *does* match returns
    /// the same value as the un-strict decoder. Pin for the
    /// invariant that strict decode is a strict superset of basic.
    #[test]
    fn strict_decoder_accepts_matching_discriminator() {
        let disc = [42u8; 8];
        let raw = encode_anchor_account(&dummy_market(), &disc).unwrap();
        let m = decode_anchor_account_with_discriminator::<OnchainMarket>(&raw, &disc).unwrap();
        assert_eq!(m, dummy_market());
    }
}
