//! Wave 23 — open-interest aggregation from decoded `Position` PDAs.
//!
//! Wave 21 shipped the `OnchainPosition` Borsh mirror; wave 22 wired
//! it into the live frontend feed. Wave 23 closes the *backend* half
//! of the wave-21 changelog promise: a prober-side open-interest probe
//! that `getProgramAccounts`-scans every `Position` PDA, decodes it,
//! and folds it into a per-market `OpenInterestFacts` aggregate — the
//! authoritative long/short exposure the SRE dashboard and the future
//! `MarketFacts.current_total_principal` reconciliation both consume.
//!
//! ## Why this lives behind the `AccountFetcher` trait, not the
//! `solana-rpc` feature
//!
//! The fetch path is written against `keeper_rpc::fetcher::AccountFetcher`,
//! which is a *default-feature* (host-only) trait with an in-memory
//! `MockAccountFetcher`. That means the entire fetch + decode +
//! aggregate pipeline is unit-testable in the sandbox with zero
//! Solana dependencies — `MockAccountFetcher` stands in for the
//! cluster. Production prober deployments enable `solana-rpc` and pass
//! a `keeper_rpc::solana::SolanaRpcAccountFetcher`, which implements
//! the same trait via a real `getProgramAccounts` memcmp filter. No
//! code in this module is feature-gated; only the production fetcher
//! lives behind the flag.
//!
//! ## Closed positions
//!
//! Closed positions (`status == 2`) are excluded from the aggregate —
//! they no longer represent live exposure. Anchor leaves the closed
//! account on chain (it is not `close`d) until the owner reclaims rent
//! via a future sweep, so a naive `getProgramAccounts` scan WILL return
//! them; the aggregator must drop them to match the frontend's
//! `isDisplayablePosition` semantics.

use keeper_decoder::ix::account_discriminator;
use keeper_decoder::{decode_anchor_account, OnchainPosition, Pubkey32};
use keeper_rpc::{AccountFetcher, RpcError};

use crate::context::PoolFacts;

/// `Position.status` value for a live, freshly-opened position.
pub const POSITION_STATUS_OPEN: u8 = 0;
/// `Position.status` value for a position rotated into a dormant bucket.
pub const POSITION_STATUS_DORMANT: u8 = 1;
/// `Position.status` value for a closed position (excluded from OI).
pub const POSITION_STATUS_CLOSED: u8 = 2;

/// Aggregated open-interest for one market (or a program-wide scan).
///
/// Principal is in microUSDC (`Position.principal`, u64); notional is
/// in microUSDC (`Position.notional`, u128). Counts are position
/// cardinalities, not share quantities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenInterestFacts {
    /// Number of live Long positions.
    pub long_count: u64,
    /// Number of live Short positions.
    pub short_count: u64,
    /// Sum of `principal` across live Long positions (microUSDC).
    pub long_principal: u128,
    /// Sum of `principal` across live Short positions (microUSDC).
    pub short_principal: u128,
    /// Sum of `notional` across live Long positions (microUSDC).
    pub long_notional: u128,
    /// Sum of `notional` across live Short positions (microUSDC).
    pub short_notional: u128,
    /// Positions whose bytes failed to decode during a scan. A
    /// non-zero value means the discriminator filter let through an
    /// account that is not a `Position` (schema drift) — surface it.
    pub decode_failures: u64,
}

impl OpenInterestFacts {
    /// Total live position count (Long + Short).
    pub fn total_count(&self) -> u64 {
        self.long_count + self.short_count
    }

    /// Total principal committed across all live positions (microUSDC).
    pub fn total_principal(&self) -> u128 {
        self.long_principal + self.short_principal
    }

    /// Total notional across all live positions (microUSDC).
    pub fn total_notional(&self) -> u128 {
        self.long_notional + self.short_notional
    }

    /// Signed notional imbalance (`long − short`). Positive means the
    /// book is net-long; the keeper's directional-risk dashboard
    /// drives off the magnitude.
    pub fn net_notional_imbalance(&self) -> i128 {
        self.long_notional as i128 - self.short_notional as i128
    }
}

/// Fold an iterator of decoded positions into an [`OpenInterestFacts`].
///
/// Closed positions (`status == POSITION_STATUS_CLOSED`) are skipped.
/// `decode_failures` is always 0 here — that field is populated by the
/// fetch path ([`decode_positions`]) which sees raw bytes.
pub fn aggregate_open_interest<'a, I>(positions: I) -> OpenInterestFacts
where
    I: IntoIterator<Item = &'a OnchainPosition>,
{
    let mut facts = OpenInterestFacts::default();
    for pos in positions {
        if pos.status == POSITION_STATUS_CLOSED {
            continue;
        }
        if pos.direction_is_long {
            facts.long_count += 1;
            facts.long_principal += pos.principal as u128;
            facts.long_notional += pos.notional;
        } else {
            facts.short_count += 1;
            facts.short_principal += pos.principal as u128;
            facts.short_notional += pos.notional;
        }
    }
    facts
}

/// Like [`aggregate_open_interest`] but restricted to positions whose
/// `market` field equals `market_pda`. Used by the per-market prober
/// reconciliation path: a program-wide `getProgramAccounts` scan
/// returns positions across *all* markets, but each market's
/// `position_principal_drift` check must reconcile only its own slice.
pub fn aggregate_open_interest_for_market<'a, I>(
    positions: I,
    market_pda: &Pubkey32,
) -> OpenInterestFacts
where
    I: IntoIterator<Item = &'a OnchainPosition>,
{
    aggregate_open_interest(
        positions
            .into_iter()
            .filter(|pos| &pos.market == market_pda),
    )
}

/// The 8-byte Anchor account discriminator for `Position`, used as the
/// `getProgramAccounts` memcmp filter (offset 0). Mirrors the
/// frontend's `MOLE_ACCOUNT_DISCRIMINATORS.position`.
pub fn position_account_discriminator() -> [u8; 8] {
    account_discriminator("Position")
}

/// Decode a batch of raw `(pubkey, data)` tuples into
/// [`OnchainPosition`]s. Decode failures are counted, not fatal: a
/// single malformed account must not abort a whole-program scan.
pub fn decode_positions(raw: &[(Pubkey32, Vec<u8>)]) -> (Vec<OnchainPosition>, u64) {
    let mut out = Vec::with_capacity(raw.len());
    let mut failures = 0u64;
    for (_pubkey, data) in raw {
        match decode_anchor_account::<OnchainPosition>(data) {
            Ok(pos) => out.push(pos),
            Err(_) => failures += 1,
        }
    }
    (out, failures)
}

/// Fetch every `Position` PDA owned by `program_id` via a
/// discriminator memcmp filter, decode them, and aggregate into
/// [`OpenInterestFacts`].
///
/// Works with any [`AccountFetcher`]: `MockAccountFetcher` in tests,
/// `SolanaRpcAccountFetcher` (behind the `solana-rpc` feature) in
/// production. The `decode_failures` field is propagated so the
/// caller can alert on schema drift.
pub fn fetch_open_interest<F: AccountFetcher>(
    fetcher: &F,
    program_id: &Pubkey32,
) -> Result<OpenInterestFacts, RpcError> {
    let discriminator = position_account_discriminator();
    let raw = fetcher.fetch_program_accounts_filter(program_id, 0, &discriminator)?;
    let (positions, failures) = decode_positions(&raw);
    let mut facts = aggregate_open_interest(positions.iter());
    facts.decode_failures = failures;
    Ok(facts)
}

/// Like [`fetch_open_interest`] but aggregates only the positions
/// owned by `market_pda`. The `getProgramAccounts` scan is still
/// program-wide (one RPC round-trip), then decoded positions are
/// filtered by their `market` field — so a single scan can feed every
/// market's reconciliation check in a multi-market prober cycle.
pub fn fetch_open_interest_for_market<F: AccountFetcher>(
    fetcher: &F,
    program_id: &Pubkey32,
    market_pda: &Pubkey32,
) -> Result<OpenInterestFacts, RpcError> {
    let discriminator = position_account_discriminator();
    let raw = fetcher.fetch_program_accounts_filter(program_id, 0, &discriminator)?;
    let (positions, failures) = decode_positions(&raw);
    let mut facts = aggregate_open_interest_for_market(positions.iter(), market_pda);
    facts.decode_failures = failures;
    Ok(facts)
}

/// Wave 24 — fold an open-interest aggregate into a [`PoolFacts`] so
/// the `position_principal_drift` health check can reconcile the
/// on-chain truth against the indexer-reported notional. Sets only the
/// `onchain_position_notional_micro_usdc` field; the reported
/// `total_notional_micro_usdc` is owned by the indexer fetch path.
pub fn apply_open_interest_to_pool(pool: &mut PoolFacts, open_interest: &OpenInterestFacts) {
    pool.onchain_position_notional_micro_usdc = open_interest.total_notional();
}

#[cfg(test)]
mod tests {
    use super::*;
    use borsh::BorshSerialize;
    use keeper_rpc::MockAccountFetcher;

    fn sample_position(long: bool, status: u8, principal: u64, notional: u128) -> OnchainPosition {
        OnchainPosition {
            owner: [1u8; 32],
            market: [2u8; 32],
            sub_pool: [3u8; 32],
            position_id: 7,
            direction_is_long: long,
            status,
            principal,
            leverage_bps: 20_000,
            notional,
            active_shares: 1,
            recovery_shares: 0,
            recovery_bucket_tick: 0,
            has_recovery_bucket: false,
            zero_price: 0,
            entry_price: 100,
            last_sync_slot: 0,
            active_generation: 0,
            opened_at: 1_700_000_000,
            updated_at: 1_700_000_000,
            closed_at: 0,
            schema_version: 1,
            bump: 254,
            _pad: [0u8; 5],
        }
    }

    fn encode_position(pos: &OnchainPosition) -> Vec<u8> {
        let mut out = position_account_discriminator().to_vec();
        let mut body = Vec::new();
        pos.serialize(&mut body).expect("borsh serialize");
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn aggregates_long_and_short_separately() {
        let positions = [
            sample_position(true, POSITION_STATUS_OPEN, 1_000, 10_000),
            sample_position(true, POSITION_STATUS_OPEN, 2_000, 20_000),
            sample_position(false, POSITION_STATUS_OPEN, 500, 5_000),
        ];
        let facts = aggregate_open_interest(positions.iter());
        assert_eq!(facts.long_count, 2);
        assert_eq!(facts.short_count, 1);
        assert_eq!(facts.long_principal, 3_000);
        assert_eq!(facts.short_principal, 500);
        assert_eq!(facts.long_notional, 30_000);
        assert_eq!(facts.short_notional, 5_000);
        assert_eq!(facts.total_count(), 3);
        assert_eq!(facts.total_principal(), 3_500);
        assert_eq!(facts.total_notional(), 35_000);
        assert_eq!(facts.net_notional_imbalance(), 25_000);
    }

    #[test]
    fn closed_positions_excluded_from_open_interest() {
        let positions = [
            sample_position(true, POSITION_STATUS_OPEN, 1_000, 10_000),
            sample_position(true, POSITION_STATUS_CLOSED, 9_999, 99_999),
            sample_position(false, POSITION_STATUS_DORMANT, 700, 7_000),
        ];
        let facts = aggregate_open_interest(positions.iter());
        // Closed long is dropped; dormant short still counts (live).
        assert_eq!(facts.long_count, 1);
        assert_eq!(facts.short_count, 1);
        assert_eq!(facts.long_principal, 1_000);
        assert_eq!(facts.short_principal, 700);
        assert_eq!(facts.total_count(), 2);
    }

    #[test]
    fn empty_input_yields_zeroed_facts() {
        let facts = aggregate_open_interest(std::iter::empty());
        assert_eq!(facts, OpenInterestFacts::default());
        assert_eq!(facts.total_count(), 0);
        assert_eq!(facts.net_notional_imbalance(), 0);
    }

    #[test]
    fn short_heavy_book_reports_negative_imbalance() {
        let positions = [
            sample_position(true, POSITION_STATUS_OPEN, 100, 1_000),
            sample_position(false, POSITION_STATUS_OPEN, 100, 9_000),
        ];
        let facts = aggregate_open_interest(positions.iter());
        assert_eq!(facts.net_notional_imbalance(), -8_000);
    }

    #[test]
    fn decode_positions_counts_failures_without_aborting() {
        let good = sample_position(true, POSITION_STATUS_OPEN, 1_000, 10_000);
        let raw = vec![
            ([10u8; 32], encode_position(&good)),
            ([11u8; 32], vec![0u8; 4]), // too short → decode failure
            ([12u8; 32], encode_position(&good)),
        ];
        let (positions, failures) = decode_positions(&raw);
        assert_eq!(positions.len(), 2);
        assert_eq!(failures, 1);
    }

    #[test]
    fn apply_open_interest_to_pool_sets_only_onchain_field() {
        let mut pool = PoolFacts {
            total_notional_micro_usdc: 42,
            recovery_outstanding_micro_usdc: 7,
            onchain_position_notional_micro_usdc: 0,
        };
        let oi = aggregate_open_interest(
            [
                sample_position(true, POSITION_STATUS_OPEN, 1_000, 10_000),
                sample_position(false, POSITION_STATUS_OPEN, 500, 5_000),
            ]
            .iter(),
        );
        apply_open_interest_to_pool(&mut pool, &oi);
        assert_eq!(pool.onchain_position_notional_micro_usdc, 15_000);
        // The reported + recovery fields are untouched.
        assert_eq!(pool.total_notional_micro_usdc, 42);
        assert_eq!(pool.recovery_outstanding_micro_usdc, 7);
    }

    #[test]
    fn fetch_open_interest_scans_program_accounts() {
        let program_id: Pubkey32 = [42u8; 32];
        let mut fetcher = MockAccountFetcher::new();
        let long_pos = sample_position(true, POSITION_STATUS_OPEN, 1_000, 10_000);
        let short_pos = sample_position(false, POSITION_STATUS_OPEN, 400, 4_000);
        let closed_pos = sample_position(true, POSITION_STATUS_CLOSED, 9_000, 90_000);
        fetcher.insert([1u8; 32], program_id, encode_position(&long_pos));
        fetcher.insert([2u8; 32], program_id, encode_position(&short_pos));
        fetcher.insert([3u8; 32], program_id, encode_position(&closed_pos));
        // An account owned by a different program must be ignored.
        fetcher.insert([4u8; 32], [99u8; 32], encode_position(&long_pos));

        let facts = fetch_open_interest(&fetcher, &program_id).expect("fetch ok");
        assert_eq!(facts.long_count, 1);
        assert_eq!(facts.short_count, 1);
        assert_eq!(facts.long_principal, 1_000);
        assert_eq!(facts.short_principal, 400);
        assert_eq!(facts.decode_failures, 0);
    }

    fn position_in_market(
        market: Pubkey32,
        long: bool,
        principal: u64,
        notional: u128,
    ) -> OnchainPosition {
        let mut p = sample_position(long, POSITION_STATUS_OPEN, principal, notional);
        p.market = market;
        p
    }

    #[test]
    fn aggregate_for_market_filters_other_markets() {
        let mkt_a: Pubkey32 = [0xaa; 32];
        let mkt_b: Pubkey32 = [0xbb; 32];
        let positions = [
            position_in_market(mkt_a, true, 1_000, 10_000),
            position_in_market(mkt_a, false, 400, 4_000),
            position_in_market(mkt_b, true, 9_000, 90_000),
        ];
        let a = aggregate_open_interest_for_market(positions.iter(), &mkt_a);
        assert_eq!(a.total_count(), 2);
        assert_eq!(a.total_notional(), 14_000);
        let b = aggregate_open_interest_for_market(positions.iter(), &mkt_b);
        assert_eq!(b.total_count(), 1);
        assert_eq!(b.total_notional(), 90_000);
    }

    #[test]
    fn fetch_for_market_filters_after_program_scan() {
        let program_id: Pubkey32 = [42u8; 32];
        let mkt_a: Pubkey32 = [0xaa; 32];
        let mkt_b: Pubkey32 = [0xbb; 32];
        let mut fetcher = MockAccountFetcher::new();
        fetcher.insert(
            [1u8; 32],
            program_id,
            encode_position(&position_in_market(mkt_a, true, 1_000, 10_000)),
        );
        fetcher.insert(
            [2u8; 32],
            program_id,
            encode_position(&position_in_market(mkt_b, true, 9_000, 90_000)),
        );
        let facts =
            fetch_open_interest_for_market(&fetcher, &program_id, &mkt_a).expect("fetch ok");
        assert_eq!(facts.total_count(), 1);
        assert_eq!(facts.long_notional, 10_000);
        assert_eq!(facts.decode_failures, 0);
    }
}
