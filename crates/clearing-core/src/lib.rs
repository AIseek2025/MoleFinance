//! MoleOption clearing engine reference implementation.
//!
//! Pure-Rust, host-side reference of the on-chain shares model defined in
//! `Docs/Planning/18-shares模型实现细则与边界条件.md`. The Solana program
//! (`programs/mole-option`) wraps these primitives in account-level glue;
//! this crate is the source of truth for the math.
//!
//! High-level layout:
//!
//! - [`types`] — protocol-wide enums (Direction, PositionStatus).
//! - [`error`] — `ClearingError`, the canonical error code list.
//! - [`market`] — `MarketParams`, `SubPool`.
//! - [`position`] — `Position` and lifecycle helpers.
//! - [`dormant`] — `DormantBucket` and `DormantStore` (per-direction tree).
//! - [`engine`] — `sync_pool`, `open_position`, `close_position`,
//!   `force_close_zero_value_position`, `claim_dormant_recovery`,
//!   `harvest_dust`.
//! - [`invariants`] — runtime checks, always called after state mutations.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod dormant;
pub mod engine;
pub mod error;
pub mod event;
pub mod invariants;
pub mod market;
pub mod onchain;
pub mod position;
pub mod types;

pub use dormant::{DistEntry, DistributionReceipt, DormantBucket, DormantStore, RedeemReceipt};
pub use onchain::{
    pack_dormant_store, unpack_dormant_store, OnChainBucketRecord, OnChainLedger,
    OnChainLedgerEntry,
};
pub use engine::{
    claim_dormant_recovery, close_position, force_close_zero_value_position, harvest_dust,
    lazy_migrate_position, open_position, pre_sync_dormant_bucket, sync_pool,
    ClaimRecoveryOutcome, CloseOutcome, ForceCloseOutcome, HarvestOutcome, OpenOutcome,
    PreSyncOutcome, PriceEnvelope, SyncOutcome,
};
pub use error::{ClearingError, ClearingResult};
pub use event::{
    ActiveRotatedToRecoveryEvent, DormantBucketPendingAppliedEvent, DormantRecoveryClaimedEvent,
    DustHarvestedEvent, EngineEvent, PoolSyncEvent, PositionClosedEvent,
    PositionForceClosedEvent, PositionOpenedEvent,
};
pub use market::{
    assert_schema_version, DistributeMode, MarketParams, RotateRecord, SubPool,
    SCHEMA_VERSION_CURRENT,
};
pub use position::{Position, PositionStatus};
pub use types::Direction;

#[cfg(test)]
mod tests;
