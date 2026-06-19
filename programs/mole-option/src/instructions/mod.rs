//! Instruction handlers.
//!
//! Each module mirrors a public entrypoint declared in [`crate`]. The
//! handlers are intentionally thin: they validate accounts, derive the
//! `clearing_core::SubPool` view, call into the engine, and write the
//! resulting state back. Math is *never* duplicated here.

pub mod admin;
pub mod claim;
pub mod close;
pub mod dormant_bridge;
pub mod harvest;
pub mod init;
pub mod keeper_leader;
pub mod migration;
pub mod open;
pub mod pre_sync;
pub mod sync;

pub use admin::*;
pub use claim::*;
pub use close::*;
pub use harvest::*;
pub use init::*;
pub use keeper_leader::*;
pub use migration::*;
pub use open::*;
pub use pre_sync::*;
pub use sync::*;

use anchor_lang::prelude::*;
use clearing_core::PriceEnvelope;
use pyth_adapter::{validate_price_account, ValidationPolicy};

use crate::error::ProgramError;
use crate::state::Market;

/// Anchor-friendly mirror of [`clearing_core::PriceEnvelope`].
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct PriceEnvelopeArgs {
    pub p_now: u64,
    pub slot: u64,
    pub expected_min: u64,
    pub expected_max: u64,
}

impl From<PriceEnvelopeArgs> for PriceEnvelope {
    fn from(value: PriceEnvelopeArgs) -> Self {
        PriceEnvelope {
            p_now: value.p_now,
            slot: value.slot,
            expected_min: value.expected_min,
            expected_max: value.expected_max,
        }
    }
}

/// Validate a Pyth `oracle_price_feed` AccountInfo against the
/// `Market`'s configured policy and overwrite the caller-supplied
/// envelope's `p_now` / `slot` with the trusted values. Used by every
/// price-driven instruction (`sync_pool`, `close_position`,
/// `force_close_zero_value_position`, `claim_dormant_recovery`).
///
/// The caller-provided envelope's `expected_min` / `expected_max`
/// remain authoritative — they're the front-end's price-protection
/// band; this function only checks that the trusted price lies inside
/// it (slippage protection) and rejects otherwise.
pub(crate) fn validate_oracle_envelope(
    market: &Market,
    oracle_info: &AccountInfo,
    clock_slot: u64,
    envelope: &mut PriceEnvelopeArgs,
) -> Result<()> {
    let oracle_owner_bytes: [u8; 32] = oracle_info.owner.to_bytes();
    let expected_owner_bytes: [u8; 32] = market.oracle_program_id.to_bytes();
    let policy = ValidationPolicy {
        max_age_slots: market.max_oracle_age_slots,
        max_confidence_bps: market.max_confidence_bps as u64,
    };
    let oracle_data = oracle_info
        .try_borrow_data()
        .map_err(|_| ProgramError::OracleAccountUnreadable)?;
    let validated = validate_price_account(
        &oracle_data,
        &oracle_owner_bytes,
        &expected_owner_bytes,
        clock_slot,
        &policy,
    )
    .map_err(|_| ProgramError::OracleValidationFailed)?;
    drop(oracle_data);

    if validated.price < envelope.expected_min || validated.price > envelope.expected_max {
        return Err(error!(ProgramError::OraclePriceOutsideEnvelope));
    }
    envelope.p_now = validated.price;
    envelope.slot = clock_slot;
    Ok(())
}
