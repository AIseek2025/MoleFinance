//! Wave 12 — ops health-check toolkit.
//!
//! Automates the 18 daily-dashboard checks documented in
//! `Docs/Planning/24-operator-runbook.md` §2. Each check takes a
//! [`HealthContext`] (on-chain + keeper + RPC + oracle facts the
//! prober has observed) and returns a [`CheckResult`] tagged with
//! its `Severity` (P0..P3) and `Status` (Pass / Warn / Critical).
//!
//! ## Design choices
//!
//! - **Data is injected, not fetched here.** The toolkit doesn't
//!   talk to a cluster; the prober (a separate process or the
//!   keeper-bot's serve daemon) gathers facts and hands them in as
//!   a `HealthContext`. This keeps the threshold logic pure,
//!   100 % testable, and reproducible across CI runs.
//! - **Thresholds match the runbook table line-for-line.** If the
//!   runbook says P1 fires at `failed_actions > 5/h`, then the
//!   `failed_action_rate` check uses exactly that number — and
//!   `Docs/Planning/24-operator-runbook.md §2` is the source of
//!   truth, this code is the implementation.
//! - **Exit codes are P-tier-aligned.** `0` = all clear,
//!   `1` = ≥ P3 warn, `2` = ≥ P2 critical, `3` = ≥ P1 critical,
//!   `4` = any P0 critical. AlertManager uses the highest exit code
//!   to drive paging tier (see `runbook §4`).
//! - **No `serde` / `serde_json` dependency.** Output is hand-
//!   formatted JSON (~30 fields total). The toolkit must be
//!   buildable from a tightly-locked-down ops VM with no extra crate
//!   dependencies pulled in. We round-trip-test the format in
//!   `tests::json_output_round_trips_through_serde_json` (when
//!   tests run with the host's `cargo` which has serde available
//!   via dev-dependencies elsewhere) — for now we just assert
//!   structural correctness.

pub mod checks;
pub mod cli_loader;
pub mod context;
pub mod multi;
pub mod position_interest;
pub mod prober;
pub mod report;
pub mod rpc_fetcher;

#[cfg(feature = "solana-rpc")]
pub mod solana_rpc;
#[cfg(feature = "solana-rpc")]
pub use solana_rpc::SolanaRpcAccountSource;

pub use checks::{run_all_checks, CHECK_NAMES};
pub use context::{
    HealthContext, KeeperFacts, LeaderLockFacts, MarketFacts, OracleFacts, PoolFacts, RpcFacts,
    SubPoolFacts,
};
pub use multi::{
    ctx_with_leader_lock, render_json_multi, scan_all_markets, MarketEntry, MarketRegistry,
    MarketScanResult, MultiMarketHealthReport, RegistryError, ScanError,
};
pub use position_interest::{
    aggregate_open_interest, aggregate_open_interest_for_market, apply_open_interest_to_pool,
    decode_positions, fetch_open_interest, fetch_open_interest_for_market,
    position_account_discriminator, OpenInterestFacts,
};
pub use prober::{
    CycleOutcome, MarketFetcher, OpenInterestAugmentingFetcher, ProberClock, ProberConfig,
    ProberLoop, ProberSink,
};
pub use rpc_fetcher::{
    FetchedAccount, RpcAccountSource, RpcMarketFetcher, RpcMarketFetcherConfig,
};
pub use report::{
    CheckResult, CheckStatus, HealthReport, Severity, exit_code_for_status, render_json,
    render_prometheus_textfile,
};
