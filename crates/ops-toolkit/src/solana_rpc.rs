//! Wave 21 — production `RpcAccountSource` impl backed by
//! `solana_client::rpc_client::RpcClient`.
//!
//! Gated behind the `solana-rpc` feature so default builds (CI,
//! sandbox host-only smoke) stay tiny. Production prober
//! deployments enable the feature and inject this struct into
//! `RpcMarketFetcher::new(...)`.
//!
//! Why a separate adapter instead of "just use keeper-rpc's
//! `SolanaRpcAccountFetcher`"? Two reasons. First, the wave-20
//! `RpcAccountSource` trait is *narrower* than
//! `keeper_rpc::AccountFetcher`: it bundles the two RPC calls
//! the prober needs (`getMultipleAccounts` and `getSlot`) into a
//! single trait. Reusing the keeper trait would force the prober
//! to depend on `getProgramAccounts` plumbing it never exercises
//! and would expand the test surface. Second, the prober's retry
//! and backoff logic lives in `RpcMarketFetcher` (wave 21);
//! putting another layer between it and `solana-client` would
//! just re-test the same retry logic.
//!
//! The adapter is intentionally thin (~100 lines including
//! comments). The unit tests live in
//! `crates/ops-toolkit/src/rpc_fetcher.rs` against the trait;
//! this module is only `cargo build`-tested behind the feature
//! flag (we do *not* spawn a real `solana-test-validator` in CI).

use std::time::Duration;

use keeper_rpc::solana::pubkey32_to_pubkey;
use solana_client::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;

use crate::rpc_fetcher::{FetchedAccount, RpcAccountSource};

/// Production `RpcAccountSource` powered by `solana-client`.
///
/// Stateful: holds a `solana_client::RpcClient` (which itself is
/// `Arc`-backed internally so multiple `SolanaRpcAccountSource`
/// instances pointing at the same cluster share connection
/// pools).
pub struct SolanaRpcAccountSource {
    client: RpcClient,
    commitment: CommitmentConfig,
}

impl SolanaRpcAccountSource {
    /// Build from an RPC URL + commitment level. Production
    /// keepers use `Confirmed`; CI integration tests use
    /// `Processed`.
    pub fn new(rpc_url: String, commitment: CommitmentConfig) -> Self {
        Self {
            client: RpcClient::new_with_commitment(rpc_url, commitment),
            commitment,
        }
    }

    /// Build with an explicit timeout. Production wraps a
    /// `Duration::from_secs(8)` so `getMultipleAccounts` doesn't
    /// stall a prober cycle.
    pub fn new_with_timeout(
        rpc_url: String,
        commitment: CommitmentConfig,
        timeout: Duration,
    ) -> Self {
        Self {
            client: RpcClient::new_with_timeout_and_commitment(rpc_url, timeout, commitment),
            commitment,
        }
    }

    /// Build from an already-initialised `RpcClient` (e.g. when
    /// the caller wants to share a client between the prober and
    /// the keeper-bot, or wants to inject a custom timeout /
    /// header).
    pub fn from_client(client: RpcClient, commitment: CommitmentConfig) -> Self {
        Self { client, commitment }
    }

    /// Borrow the underlying client for cases where the caller
    /// needs a non-`RpcAccountSource` operation (e.g. health-
    /// status pings during cluster startup).
    pub fn client(&self) -> &RpcClient {
        &self.client
    }

    /// Currently-configured commitment.
    pub fn commitment(&self) -> CommitmentConfig {
        self.commitment
    }
}

impl RpcAccountSource for SolanaRpcAccountSource {
    fn get_multiple_accounts(
        &mut self,
        pubkeys: &[[u8; 32]],
    ) -> Result<Vec<Option<FetchedAccount>>, String> {
        // `solana-client`'s `get_multiple_accounts` accepts
        // `&[Pubkey]`. Convert in-place; production prober loops
        // over a small registry (typically <20 markets), so the
        // allocation cost is negligible vs the RTT.
        let pks: Vec<_> = pubkeys.iter().map(pubkey32_to_pubkey).collect();
        let resp = self
            .client
            .get_multiple_accounts(&pks)
            .map_err(|e| format!("get_multiple_accounts: {e}"))?;
        // `solana-client 4.0` returns `Vec<Option<Account>>` — the
        // explicit closure type makes inference behave on stable.
        let mapped: Vec<Option<FetchedAccount>> = resp
            .into_iter()
            .map(|opt| opt.map(|acc| FetchedAccount { data: acc.data }))
            .collect();
        Ok(mapped)
    }

    fn get_slot(&mut self) -> Result<u64, String> {
        self.client.get_slot().map_err(|e| format!("get_slot: {e}"))
    }

    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

// Tests live in `rpc_fetcher.rs` against the trait; this module
// is build-tested behind the feature flag. The CI matrix runs:
//
//   cargo build -p ops-toolkit --features solana-rpc
//
// which compile-checks the module without needing a real cluster.
