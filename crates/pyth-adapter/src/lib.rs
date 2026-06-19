//! Host-testable validator for Pythnet v2 price accounts.
//!
//! The on-chain MoleOption program treats the oracle as adversarial:
//! before a `sync_pool` consumes a price, every byte of the price
//! account is validated against:
//!
//! 1. **Owner program id** — must equal the configured `pyth_program`
//!    (passed by the caller; this crate does not own a key constant).
//! 2. **Header magic / version / account_type** — pinned to the Pyth
//!    Pythnet v2 layout (`magic = 0xa1b2c3d4`, `version = 2`, `atype = 3`).
//! 3. **Status** — `agg.status == 1` (Trading). Anything else (Halted,
//!    Auction, Unknown) is rejected.
//! 4. **Exponent bounds** — `expo ∈ [-18, 0]`; positive exponents and
//!    extreme negatives are rejected to prevent over- / under-flow when
//!    re-scaling to `PRICE_SCALE`.
//! 5. **Confidence band** — `conf / |price| ≤ max_confidence_bps`.
//! 6. **Staleness** — `current_slot - publish_slot ≤ max_age_slots`.
//! 7. **Sign** — `price > 0`. The protocol's accounting assumes a strictly
//!    positive price; zero or negative is rejected.
//!
//! Successful validation returns a [`ValidatedPrice`] with the price
//! re-scaled to `molemath::PRICE_SCALE` (1e8), where the rescale itself
//! is performed via `mul_div_floor` against an integer power-of-ten so
//! no floating-point error sneaks in.
//!
//! This crate is **host-only**: it has no Solana dependencies, parses
//! account bytes via fixed-size byte arrays, and is fully tested with
//! synthetic mock accounts. The on-chain program embeds this crate as
//! a regular dependency and calls [`validate_price_account`] from its
//! `sync_pool` instruction handler.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use molemath::{mul_div_floor, MathError, PRICE_SCALE};
use thiserror::Error;

/// Required magic number at the head of every Pythnet v2 price account.
pub const PYTH_MAGIC: u32 = 0xa1b2_c3d4;
/// Required Pythnet account version.
pub const PYTH_VERSION: u32 = 2;
/// `atype = 3` means "price account" in the Pythnet schema.
pub const PYTH_ACCOUNT_TYPE_PRICE: u32 = 3;
/// `agg.status = 1` means "Trading".
pub const PYTH_STATUS_TRADING: u32 = 1;
/// Minimum acceptable exponent (price is scaled by `10^expo`, with
/// `expo ≤ 0`); -18 covers every realistic Pyth feed and stops a
/// malicious feed from overflowing the rescale.
pub const MIN_EXPO: i32 = -18;
/// Maximum acceptable exponent (must be `≤ 0`).
pub const MAX_EXPO: i32 = 0;

/// Errors returned by [`validate_price_account`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum OracleError {
    /// Byte slice shorter than the minimum header.
    #[error("price account too small: got {0} bytes, need at least {1}")]
    TooSmall(usize, usize),
    /// Owner program id mismatch.
    #[error("wrong owner program id")]
    WrongOwner,
    /// Magic constant did not match [`PYTH_MAGIC`].
    #[error("magic mismatch: got 0x{0:08x}")]
    MagicMismatch(u32),
    /// Version mismatch.
    #[error("version mismatch: got {0}, expected {expected}", expected = PYTH_VERSION)]
    VersionMismatch(u32),
    /// Account type was not "price".
    #[error("not a price account: atype={0}")]
    NotPriceAccount(u32),
    /// Aggregate status was not `Trading`.
    #[error("not trading: status={0}")]
    NotTrading(u32),
    /// Exponent out of acceptable range.
    #[error("expo out of range: {0}")]
    ExpoOutOfRange(i32),
    /// Confidence > `max_confidence_bps` of price.
    #[error("confidence too wide: bps={0}")]
    ConfidenceTooWide(u128),
    /// Account staler than `max_age_slots`.
    #[error("price stale: age_slots={0}")]
    Stale(u64),
    /// Non-positive price.
    #[error("non-positive price: {0}")]
    NonPositivePrice(i64),
    /// Math overflow during rescale.
    #[error("math overflow")]
    MathOverflow,
}

impl From<MathError> for OracleError {
    fn from(_: MathError) -> Self {
        OracleError::MathOverflow
    }
}

/// Validated, normalized price ready for use by the clearing engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedPrice {
    /// Price rescaled to `molemath::PRICE_SCALE` (1e8).
    pub price: u64,
    /// Confidence rescaled to `PRICE_SCALE`.
    pub confidence: u64,
    /// Original Pyth exponent.
    pub expo: i32,
    /// Pyth-reported publish slot of the latest aggregate.
    pub publish_slot: u64,
}

/// Caller-supplied validation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationPolicy {
    /// Maximum staleness, in Solana slots, between `publish_slot` and `current_slot`.
    pub max_age_slots: u64,
    /// Maximum allowed `confidence / price` ratio in basis points.
    pub max_confidence_bps: u64,
}

impl Default for ValidationPolicy {
    /// Defaults: 25 slots ≈ 10 seconds at 0.4 s/slot, ≤ 1 % confidence.
    fn default() -> Self {
        Self {
            max_age_slots: 25,
            max_confidence_bps: 100,
        }
    }
}

/// Minimum bytes we read from a Pyth price account. The on-chain Pyth
/// account is much larger; we only ingest a fixed prefix because every
/// field we need lives at a deterministic offset.
pub const MIN_HEADER_BYTES: usize = 240;

/// Field offsets within a Pythnet v2 price account.
mod offsets {
    pub const MAGIC: usize = 0;
    pub const VERSION: usize = 4;
    pub const ATYPE: usize = 8;
    pub const _SIZE: usize = 12;
    pub const _PTYPE: usize = 16;
    pub const EXPO: usize = 20;
    /// `agg` price component starts at offset 208.
    pub const AGG_PRICE: usize = 208;
    /// `agg.conf` follows immediately after `agg.price` (i64 takes 8 bytes).
    pub const AGG_CONF: usize = 216;
    /// `agg.status: u32` follows `agg.conf`.
    pub const AGG_STATUS: usize = 224;
    /// `agg.pub_slot: u64` follows `agg.status` (skipping a 4-byte corp_act
    /// that is unused by us, plus 4 bytes of padding) at offset 232.
    pub const AGG_PUB_SLOT: usize = 232;
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_i32_le(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64_le(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn read_i64_le(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

/// Validate a Pyth price account and return a normalized [`ValidatedPrice`].
///
/// `account_owner` is the Solana account owner pubkey reported by the
/// runtime; `expected_owner` is the operator-configured Pyth program id
/// stored in `GlobalConfig.pyth_program`. The match is byte-exact.
///
/// `account_data` must be at least [`MIN_HEADER_BYTES`] long.
pub fn validate_price_account(
    account_data: &[u8],
    account_owner: &[u8; 32],
    expected_owner: &[u8; 32],
    current_slot: u64,
    policy: &ValidationPolicy,
) -> Result<ValidatedPrice, OracleError> {
    if account_data.len() < MIN_HEADER_BYTES {
        return Err(OracleError::TooSmall(account_data.len(), MIN_HEADER_BYTES));
    }
    if account_owner != expected_owner {
        return Err(OracleError::WrongOwner);
    }

    let magic = read_u32_le(account_data, offsets::MAGIC);
    if magic != PYTH_MAGIC {
        return Err(OracleError::MagicMismatch(magic));
    }
    let version = read_u32_le(account_data, offsets::VERSION);
    if version != PYTH_VERSION {
        return Err(OracleError::VersionMismatch(version));
    }
    let atype = read_u32_le(account_data, offsets::ATYPE);
    if atype != PYTH_ACCOUNT_TYPE_PRICE {
        return Err(OracleError::NotPriceAccount(atype));
    }

    let expo = read_i32_le(account_data, offsets::EXPO);
    if !(MIN_EXPO..=MAX_EXPO).contains(&expo) {
        return Err(OracleError::ExpoOutOfRange(expo));
    }

    let status = read_u32_le(account_data, offsets::AGG_STATUS);
    if status != PYTH_STATUS_TRADING {
        return Err(OracleError::NotTrading(status));
    }

    let raw_price = read_i64_le(account_data, offsets::AGG_PRICE);
    if raw_price <= 0 {
        return Err(OracleError::NonPositivePrice(raw_price));
    }
    let raw_conf = read_u64_le(account_data, offsets::AGG_CONF);

    let publish_slot = read_u64_le(account_data, offsets::AGG_PUB_SLOT);
    let age = current_slot.saturating_sub(publish_slot);
    if age > policy.max_age_slots {
        return Err(OracleError::Stale(age));
    }

    // confidence_bps = conf * 10_000 / |price| — checked against policy.
    let raw_price_u = raw_price as u128;
    let conf_bps = mul_div_floor(raw_conf as u128, 10_000u128, raw_price_u)?;
    if conf_bps > policy.max_confidence_bps as u128 {
        return Err(OracleError::ConfidenceTooWide(conf_bps));
    }

    let price = rescale_to_price_scale(raw_price as u128, expo)?;
    let confidence = rescale_to_price_scale(raw_conf as u128, expo)?;

    Ok(ValidatedPrice {
        price: u64::try_from(price).map_err(|_| OracleError::MathOverflow)?,
        confidence: u64::try_from(confidence).map_err(|_| OracleError::MathOverflow)?,
        expo,
        publish_slot,
    })
}

/// Rescale a raw integer of exponent `expo` into `PRICE_SCALE = 1e8`.
///
/// `target_expo = -8`. The rescale factor is `10^(expo - target_expo)`:
///
/// - if `expo > target_expo` (e.g. expo = -6), we multiply by `10^(expo -
///   target_expo)` to widen.
/// - if `expo < target_expo` (e.g. expo = -10), we divide (floor) by
///   `10^(target_expo - expo)` to compress.
fn rescale_to_price_scale(raw: u128, expo: i32) -> Result<u128, OracleError> {
    const TARGET_EXPO: i32 = -8;
    const _: () = {
        assert!(PRICE_SCALE == 100_000_000, "rescale assumes PRICE_SCALE = 1e8");
    };
    if expo == TARGET_EXPO {
        return Ok(raw);
    }
    if expo > TARGET_EXPO {
        let factor = pow10(expo - TARGET_EXPO)?;
        Ok(raw.checked_mul(factor).ok_or(OracleError::MathOverflow)?)
    } else {
        let factor = pow10(TARGET_EXPO - expo)?;
        Ok(raw / factor)
    }
}

fn pow10(n: i32) -> Result<u128, OracleError> {
    if !(0..=18).contains(&n) {
        return Err(OracleError::ExpoOutOfRange(n));
    }
    let mut acc: u128 = 1;
    for _ in 0..n {
        acc = acc.checked_mul(10).ok_or(OracleError::MathOverflow)?;
    }
    Ok(acc)
}

#[cfg(test)]
mod tests;
