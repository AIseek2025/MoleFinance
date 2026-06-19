//! Wave 10 — Solana RPC adapter for the wave-9 [`keeper`] crate.
//!
//! The wave-9 keeper crate is intentionally pure-host Rust: it
//! exposes the [`KeeperChainView`] / [`ActionExecutor`] traits and
//! leaves *how* you fill them open. This crate closes that gap by
//! providing:
//!
//! 1. **Borsh-decoded account mirrors** of the four Anchor accounts
//!    the scheduler reads (`SubPool`, `DormantBucket`,
//!    `DistributionLedger`, `Market`). Byte-equivalent to
//!    `programs/mole-option/src/state.rs`.
//! 2. **[`AccountFetcher`]** — the sole RPC trait the rest of the
//!    crate depends on. Default-mode supplies an in-memory
//!    [`MockAccountFetcher`] for host-side tests; the `solana-rpc`
//!    feature plugs a real `solana-client` adapter on top.
//! 3. **[`ChainSnapshot`]** — concrete `KeeperChainView` impl that
//!    decodes a batch of fetched accounts into the snapshot shape
//!    the [`Scheduler`](keeper::Scheduler) consumes.
//! 4. **[`TxBuilder`]** + **[`RpcExecutor`]** — the action-execution
//!    side of the bridge. The default [`MockTxBuilder`] records what
//!    the executor *would* submit; the `solana-rpc` feature wires it
//!    to a real Solana transaction submitter.
//! 5. **[`pda`]** — helpers that mirror the Anchor `seeds = [...]`
//!    declarations so the snapshot refresher and the executor agree
//!    on which PDAs to fetch / sign.
//!
//! ## Why two layers
//!
//! Putting the abstract trait surface in default-features keeps the
//! crate's host-only test loop fast and reproducible: every test
//! exercises the *full* Borsh decode + trait wiring path with no
//! `solana-client` runtime, which means CI never depends on the
//! 50+-crate agave dependency tree. Production deployments enable
//! `--features solana-rpc` to pull in the real binding.
//!
//! See `Docs/Planning/23-on-chain-dormant-bridge.md` § wave-10 for
//! the rollout plan.

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(unsafe_code))]

pub mod accounts;
pub mod leader_tx;
pub mod market_registry;
pub mod pda;

mod fetcher;
mod snapshot;
mod tx;

#[cfg(feature = "solana-rpc")]
pub mod solana;

pub use fetcher::{AccountFetcher, MockAccountFetcher, RpcError};
pub use leader_tx::{
    build_keeper_leader_acquire, build_keeper_leader_heartbeat, build_keeper_leader_release,
    fetch_keeper_leader_lock, KeeperLeaderTxBuilder, LeaderInstruction, LeaderReconcileError,
    MockKeeperLeaderTxBuilder,
};
pub use market_registry::{MarketEntry, MarketRegistry, RegistryError};
pub use snapshot::{ChainSnapshot, MarketContext, SnapshotConfig, SnapshotError, SubPoolEntry};
pub use tx::{
    DISC_CLOSE_DORMANT_BUCKET, DISC_INITIALIZE_DORMANT_BUCKET, DISC_PRE_SYNC_DORMANT_BUCKET,
    DispatchedAction, MockTxBuilder, RpcExecutor, SubmittedTx, TxBuildError, TxBuilder,
};

// Re-export from `keeper-decoder` so existing callers
// (`keeper_rpc::Pubkey32`, `keeper_rpc::ANCHOR_DISCRIMINATOR_LEN`,
// `keeper_rpc::accounts::OnchainSubPool`) keep their compile-time
// paths after the wave-14 schema split.
pub use keeper_decoder::{ANCHOR_DISCRIMINATOR_LEN, Pubkey32};

/// Anchor's "global:" namespace used to derive instruction
/// discriminators (`sha256("global:<ix_name>")[..8]`). Exposed so
/// downstream tooling can recompute the discriminators if a future
/// instruction is added without forcing a `keeper-rpc` upgrade.
pub const ANCHOR_INSTRUCTION_NAMESPACE: &str = "global:";
