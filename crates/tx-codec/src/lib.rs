//! Wave 15 — Anchor instruction encoder for the mole-option program.
//!
//! ## Why a separate crate
//!
//! Wave 14 split the on-chain account *schemas* into `keeper-decoder`
//! so the same Borsh structs could compile to `wasm32-unknown-unknown`
//! and ship to the frontend. Wave 15 does the symmetric move for the
//! *instruction encoder*: `tx-codec` is zero-Solana-dep, builds clean
//! on `wasm32-unknown-unknown`, and exposes:
//!
//! - The 8 Anchor instruction discriminators (`sha256("global:<ix>")[..8]`),
//!   pinned by `tests::discriminator_constants_match_sha256_of_anchor_namespace`
//!   so a future rename in `programs/mole-option/src/lib.rs` fails CI loudly.
//! - The arg structs the on-chain program expects:
//!   [`PriceEnvelopeArgs`], [`OpenParams`].
//! - Top-level `encode_*_ix(...) -> Vec<u8>` helpers that produce
//!   `discriminator ++ borsh(args)`. Callers (keeper bot, frontend
//!   wallet flow) prepend program id + AccountMeta list.
//!
//! `keeper-rpc::tx` re-exports the 3 keeper-side discriminators from
//! here for backwards compat, and the wave-15 frontend
//! `frontend/src/tx/encode.ts` reproduces the same byte layout via
//! `@coral-xyz/borsh`. Frontend tests pin against fixtures emitted
//! from this crate's host tests; wave-16 `wasm-pack` collapses the
//! dual implementation into a single wasm import.

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(unsafe_code))]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

use borsh::{BorshDeserialize, BorshSerialize};

// =====================================================================
// Anchor discriminators
// =====================================================================
//
// Computed off-chain via `sha256("global:<ix_name>")[..8]`. Pinned by
// the self-test at the bottom of this file.

/// Discriminator for `sync_pool`.
pub const DISC_SYNC_POOL: [u8; 8] =
    [0xdb, 0xfb, 0xd7, 0xfb, 0x2c, 0x25, 0x6c, 0x66];
/// Discriminator for `open_position`.
pub const DISC_OPEN_POSITION: [u8; 8] =
    [0x87, 0x80, 0x2f, 0x4d, 0x0f, 0x98, 0xf0, 0x31];
/// Discriminator for `close_position`.
pub const DISC_CLOSE_POSITION: [u8; 8] =
    [0x7b, 0x86, 0x51, 0x00, 0x31, 0x44, 0x62, 0x62];
/// Discriminator for `claim_dormant_recovery`.
pub const DISC_CLAIM_DORMANT_RECOVERY: [u8; 8] =
    [0x76, 0x55, 0x1a, 0xe5, 0x9c, 0xb7, 0x5e, 0xf7];
/// Discriminator for `harvest_dust`.
pub const DISC_HARVEST_DUST: [u8; 8] =
    [0x28, 0x05, 0xb4, 0xaa, 0x2b, 0x4d, 0x86, 0x34];
/// Discriminator for `pre_sync_dormant_bucket` (keeper-side).
pub const DISC_PRE_SYNC_DORMANT_BUCKET: [u8; 8] =
    [0xd6, 0x62, 0xa8, 0x7a, 0xc1, 0x9c, 0x38, 0x08];
/// Discriminator for `close_dormant_bucket` (keeper-side).
pub const DISC_CLOSE_DORMANT_BUCKET: [u8; 8] =
    [0x16, 0x25, 0x10, 0x86, 0xff, 0x31, 0xf8, 0x83];
/// Discriminator for `initialize_dormant_bucket` (keeper-side).
pub const DISC_INITIALIZE_DORMANT_BUCKET: [u8; 8] =
    [0x8f, 0xcb, 0x7b, 0xd0, 0xc2, 0x40, 0x6e, 0x35];

/// Anchor's `global:` namespace prefix.
pub const ANCHOR_INSTRUCTION_NAMESPACE: &str = "global:";

// =====================================================================
// Arg structs (mirror `programs/mole-option/src/instructions/*.rs`)
// =====================================================================

/// Mirror of `programs/mole-option/src/instructions/mod.rs::PriceEnvelopeArgs`.
/// Borsh-encoded as 4 u64 little-endian. Field order MUST match the
/// on-chain struct.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceEnvelopeArgs {
    /// Current oracle price (PRICE_SCALE = 1e8).
    pub p_now: u64,
    /// Slot the price was fetched at.
    pub slot: u64,
    /// Lower bound the caller asserts the live price must clear.
    pub expected_min: u64,
    /// Upper bound, inclusive.
    pub expected_max: u64,
}

/// Mirror of `programs/mole-option/src/instructions/open.rs::OpenParams`.
/// Borsh-encoded `envelope ++ direction_is_long ++ gross_amount ++ position_id`.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenParams {
    /// Price envelope assertion.
    pub envelope: PriceEnvelopeArgs,
    /// Long-side iff true.
    pub direction_is_long: bool,
    /// Collateral amount in minor units (caller-side bound by program).
    pub gross_amount: u64,
    /// Caller-allocated position id (must be unique within the
    /// trader's signer scope).
    pub position_id: u64,
}

// =====================================================================
// Errors
// =====================================================================

/// Errors produced while encoding an instruction.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// Borsh serialiser failed (effectively never — the args are all
    /// fixed-size primitives — but we surface it instead of panicking
    /// so the wasm consumer never aborts on a degenerate input).
    #[error("borsh encode failed: {0}")]
    Borsh(String),
}

// =====================================================================
// Encoders — `disc ++ borsh(args)`
// =====================================================================

fn build_ix(disc: &[u8; 8], args: &impl BorshSerialize) -> Result<Vec<u8>, EncodeError> {
    let mut data = Vec::with_capacity(8 + 64);
    data.extend_from_slice(disc);
    args.serialize(&mut data)
        .map_err(|e| EncodeError::Borsh(e.to_string()))?;
    Ok(data)
}

/// Encode the `open_position` instruction's `data` blob (no account
/// metas; caller assembles those).
pub fn encode_open_position_ix(params: &OpenParams) -> Result<Vec<u8>, EncodeError> {
    build_ix(&DISC_OPEN_POSITION, params)
}

/// Encode `close_position`.
///
/// The on-chain ix takes 3 positional args — Anchor encodes them in
/// declaration order, so the wire layout is
/// `disc ++ borsh(envelope) ++ borsh(long_bucket_count) ++ borsh(short_bucket_count)`.
pub fn encode_close_position_ix(
    envelope: &PriceEnvelopeArgs,
    long_bucket_count: u32,
    short_bucket_count: u32,
) -> Result<Vec<u8>, EncodeError> {
    encode_envelope_plus_bucket_counts(
        &DISC_CLOSE_POSITION,
        envelope,
        long_bucket_count,
        short_bucket_count,
    )
}

/// Encode `sync_pool`.
pub fn encode_sync_pool_ix(
    envelope: &PriceEnvelopeArgs,
    long_bucket_count: u32,
    short_bucket_count: u32,
) -> Result<Vec<u8>, EncodeError> {
    encode_envelope_plus_bucket_counts(
        &DISC_SYNC_POOL,
        envelope,
        long_bucket_count,
        short_bucket_count,
    )
}

/// Encode `claim_dormant_recovery`.
pub fn encode_claim_dormant_recovery_ix(
    envelope: &PriceEnvelopeArgs,
    long_bucket_count: u32,
    short_bucket_count: u32,
) -> Result<Vec<u8>, EncodeError> {
    encode_envelope_plus_bucket_counts(
        &DISC_CLAIM_DORMANT_RECOVERY,
        envelope,
        long_bucket_count,
        short_bucket_count,
    )
}

fn encode_envelope_plus_bucket_counts(
    disc: &[u8; 8],
    envelope: &PriceEnvelopeArgs,
    long_bucket_count: u32,
    short_bucket_count: u32,
) -> Result<Vec<u8>, EncodeError> {
    let mut data = Vec::with_capacity(8 + 32 + 4 + 4);
    data.extend_from_slice(disc);
    envelope
        .serialize(&mut data)
        .map_err(|e| EncodeError::Borsh(e.to_string()))?;
    long_bucket_count
        .serialize(&mut data)
        .map_err(|e| EncodeError::Borsh(e.to_string()))?;
    short_bucket_count
        .serialize(&mut data)
        .map_err(|e| EncodeError::Borsh(e.to_string()))?;
    Ok(data)
}

/// Encode `harvest_dust(direction_is_long: bool)`.
pub fn encode_harvest_dust_ix(direction_is_long: bool) -> Result<Vec<u8>, EncodeError> {
    let mut data = Vec::with_capacity(8 + 1);
    data.extend_from_slice(&DISC_HARVEST_DUST);
    direction_is_long
        .serialize(&mut data)
        .map_err(|e| EncodeError::Borsh(e.to_string()))?;
    Ok(data)
}

/// Encode `pre_sync_dormant_bucket(direction_is_long, tick, long_bucket_count, short_bucket_count)`.
/// Keeper-side ix; surfaced from this crate so the wave-15 split has
/// a single source of truth for *every* mole-option ix. `keeper-rpc::tx::RpcExecutor`
/// re-uses this helper.
pub fn encode_pre_sync_dormant_bucket_ix(
    direction_is_long: bool,
    tick: i64,
    long_bucket_count: u32,
    short_bucket_count: u32,
) -> Result<Vec<u8>, EncodeError> {
    let mut data = Vec::with_capacity(8 + 1 + 8 + 4 + 4);
    data.extend_from_slice(&DISC_PRE_SYNC_DORMANT_BUCKET);
    direction_is_long
        .serialize(&mut data)
        .map_err(|e| EncodeError::Borsh(e.to_string()))?;
    tick.serialize(&mut data)
        .map_err(|e| EncodeError::Borsh(e.to_string()))?;
    long_bucket_count
        .serialize(&mut data)
        .map_err(|e| EncodeError::Borsh(e.to_string()))?;
    short_bucket_count
        .serialize(&mut data)
        .map_err(|e| EncodeError::Borsh(e.to_string()))?;
    Ok(data)
}

/// Encode `close_dormant_bucket(direction_is_long, tick)`. Keeper-side.
pub fn encode_close_dormant_bucket_ix(
    direction_is_long: bool,
    tick: i64,
) -> Result<Vec<u8>, EncodeError> {
    let mut data = Vec::with_capacity(8 + 1 + 8);
    data.extend_from_slice(&DISC_CLOSE_DORMANT_BUCKET);
    direction_is_long
        .serialize(&mut data)
        .map_err(|e| EncodeError::Borsh(e.to_string()))?;
    tick.serialize(&mut data)
        .map_err(|e| EncodeError::Borsh(e.to_string()))?;
    Ok(data)
}

/// Encode `initialize_dormant_bucket(direction_is_long, tick)`. Keeper-side.
pub fn encode_initialize_dormant_bucket_ix(
    direction_is_long: bool,
    tick: i64,
) -> Result<Vec<u8>, EncodeError> {
    let mut data = Vec::with_capacity(8 + 1 + 8);
    data.extend_from_slice(&DISC_INITIALIZE_DORMANT_BUCKET);
    direction_is_long
        .serialize(&mut data)
        .map_err(|e| EncodeError::Borsh(e.to_string()))?;
    tick.serialize(&mut data)
        .map_err(|e| EncodeError::Borsh(e.to_string()))?;
    Ok(data)
}

// =====================================================================
// Schema descriptor — analogue of `keeper_decoder::schema_descriptor_json`
// =====================================================================

/// Stable JSON-style description of every encoder this crate exposes,
/// so the frontend's wave-15 `tx/encode.ts` can pin against the same
/// shape via `tx_codec::schema_descriptor_json()` snapshot. Format
/// matches `keeper_decoder::schema_descriptor_json()` so both
/// snapshots can be diffed by the same tooling.
pub fn schema_descriptor_json() -> String {
    let mut out = String::with_capacity(2048);
    out.push('{');
    out.push_str("\"PriceEnvelopeArgs\":[");
    push_field(&mut out, true, "p_now", "u64");
    push_field(&mut out, false, "slot", "u64");
    push_field(&mut out, false, "expected_min", "u64");
    push_field(&mut out, false, "expected_max", "u64");
    out.push_str("],\"OpenParams\":[");
    push_field(&mut out, true, "envelope", "PriceEnvelopeArgs");
    push_field(&mut out, false, "direction_is_long", "bool");
    push_field(&mut out, false, "gross_amount", "u64");
    push_field(&mut out, false, "position_id", "u64");
    out.push_str("],\"Discriminators\":[");
    push_field(&mut out, true, "DISC_SYNC_POOL", "8B");
    push_field(&mut out, false, "DISC_OPEN_POSITION", "8B");
    push_field(&mut out, false, "DISC_CLOSE_POSITION", "8B");
    push_field(&mut out, false, "DISC_CLAIM_DORMANT_RECOVERY", "8B");
    push_field(&mut out, false, "DISC_HARVEST_DUST", "8B");
    push_field(&mut out, false, "DISC_PRE_SYNC_DORMANT_BUCKET", "8B");
    push_field(&mut out, false, "DISC_CLOSE_DORMANT_BUCKET", "8B");
    push_field(&mut out, false, "DISC_INITIALIZE_DORMANT_BUCKET", "8B");
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
    use sha2::{Digest, Sha256};

    fn anchor_disc(name: &str) -> [u8; 8] {
        let mut h = Sha256::new();
        h.update(format!("{ANCHOR_INSTRUCTION_NAMESPACE}{name}").as_bytes());
        let r = h.finalize();
        let mut out = [0u8; 8];
        out.copy_from_slice(&r[..8]);
        out
    }

    /// Self-test that pins every hard-coded discriminator constant
    /// to `sha256("global:<ix_name>")[..8]` for the on-chain ix name.
    /// If `programs/mole-option/src/lib.rs` ever renames an
    /// instruction without updating the constant here, this test
    /// fires. Same protection wave 10 added for the keeper-side
    /// discriminators, now extended to all 8.
    #[test]
    fn discriminator_constants_match_sha256_of_anchor_namespace() {
        assert_eq!(DISC_SYNC_POOL, anchor_disc("sync_pool"));
        assert_eq!(DISC_OPEN_POSITION, anchor_disc("open_position"));
        assert_eq!(DISC_CLOSE_POSITION, anchor_disc("close_position"));
        assert_eq!(
            DISC_CLAIM_DORMANT_RECOVERY,
            anchor_disc("claim_dormant_recovery")
        );
        assert_eq!(DISC_HARVEST_DUST, anchor_disc("harvest_dust"));
        assert_eq!(
            DISC_PRE_SYNC_DORMANT_BUCKET,
            anchor_disc("pre_sync_dormant_bucket")
        );
        assert_eq!(
            DISC_CLOSE_DORMANT_BUCKET,
            anchor_disc("close_dormant_bucket")
        );
        assert_eq!(
            DISC_INITIALIZE_DORMANT_BUCKET,
            anchor_disc("initialize_dormant_bucket")
        );
    }

    /// `print_canonical_discriminators` regenerates the constants if
    /// the on-chain ix names ever change. Ignored by default; run via
    /// `cargo test -p tx-codec print_canonical_discriminators -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn print_canonical_discriminators() {
        for name in [
            "sync_pool",
            "open_position",
            "close_position",
            "claim_dormant_recovery",
            "harvest_dust",
            "pre_sync_dormant_bucket",
            "close_dormant_bucket",
            "initialize_dormant_bucket",
        ] {
            let d = anchor_disc(name);
            print!("// {name}: [");
            for b in d {
                print!("0x{b:02x}, ");
            }
            println!("]");
        }
    }

    fn dummy_envelope() -> PriceEnvelopeArgs {
        PriceEnvelopeArgs {
            p_now: 100_000_000,
            slot: 12345,
            expected_min: 99_500_000,
            expected_max: 100_500_000,
        }
    }

    /// `OpenParams` round-trips through Borsh byte-for-byte (no
    /// padding shifts, no field-order drift).
    #[test]
    fn open_params_round_trips_through_borsh() {
        let p = OpenParams {
            envelope: dummy_envelope(),
            direction_is_long: true,
            gross_amount: 1_000_000_000,
            position_id: 42,
        };
        let mut buf = Vec::new();
        p.serialize(&mut buf).unwrap();
        let p2 = OpenParams::try_from_slice(&buf).unwrap();
        assert_eq!(p, p2);
    }

    /// `encode_open_position_ix` produces `disc ++ borsh(args)`.
    #[test]
    fn encode_open_position_ix_layout_is_disc_then_args() {
        let p = OpenParams {
            envelope: dummy_envelope(),
            direction_is_long: false,
            gross_amount: 500_000_000,
            position_id: 7,
        };
        let raw = encode_open_position_ix(&p).unwrap();
        // First 8 bytes are the discriminator.
        assert_eq!(&raw[..8], &DISC_OPEN_POSITION);
        // Remaining bytes are the Borsh-encoded args.
        let body = OpenParams::try_from_slice(&raw[8..]).unwrap();
        assert_eq!(body, p);
        // Length: 8 disc + 32 envelope + 1 dir + 8 amount + 8 id.
        assert_eq!(raw.len(), 8 + 32 + 1 + 8 + 8);
    }

    /// `encode_close_position_ix` lays out
    /// `disc ++ envelope ++ long_bucket_count ++ short_bucket_count`
    /// in that order. Decoder confirms by replaying the read.
    #[test]
    fn encode_close_position_ix_layout_matches_anchor_arg_order() {
        let env = dummy_envelope();
        let raw = encode_close_position_ix(&env, 7, 11).unwrap();
        assert_eq!(&raw[..8], &DISC_CLOSE_POSITION);
        let body = &raw[8..];
        let env_decoded = PriceEnvelopeArgs::try_from_slice(&body[..32]).unwrap();
        assert_eq!(env_decoded, env);
        let long = u32::from_le_bytes(body[32..36].try_into().unwrap());
        let short = u32::from_le_bytes(body[36..40].try_into().unwrap());
        assert_eq!(long, 7);
        assert_eq!(short, 11);
        assert_eq!(raw.len(), 8 + 32 + 4 + 4);
    }

    /// `encode_sync_pool_ix` shares the same layout as close_position
    /// (envelope + 2 u32 bucket counts) and a different discriminator.
    #[test]
    fn encode_sync_pool_ix_uses_sync_pool_discriminator() {
        let raw = encode_sync_pool_ix(&dummy_envelope(), 0, 0).unwrap();
        assert_eq!(&raw[..8], &DISC_SYNC_POOL);
        assert_eq!(raw.len(), 8 + 32 + 4 + 4);
    }

    /// `encode_claim_dormant_recovery_ix` ditto.
    #[test]
    fn encode_claim_dormant_recovery_ix_uses_correct_discriminator() {
        let raw = encode_claim_dormant_recovery_ix(&dummy_envelope(), 1, 2).unwrap();
        assert_eq!(&raw[..8], &DISC_CLAIM_DORMANT_RECOVERY);
        assert_eq!(raw.len(), 8 + 32 + 4 + 4);
    }

    /// `encode_harvest_dust_ix` is the smallest ix: 8 disc + 1 bool.
    #[test]
    fn encode_harvest_dust_ix_long_emits_nine_bytes() {
        let raw = encode_harvest_dust_ix(true).unwrap();
        assert_eq!(raw, {
            let mut v = Vec::with_capacity(9);
            v.extend_from_slice(&DISC_HARVEST_DUST);
            v.push(1u8);
            v
        });
        let raw_short = encode_harvest_dust_ix(false).unwrap();
        assert_eq!(raw_short[8], 0u8);
    }

    /// `encode_pre_sync_dormant_bucket_ix` parity with the wave-10
    /// hand-rolled keeper-rpc encoder (now this is the single source).
    #[test]
    fn encode_pre_sync_dormant_bucket_ix_layout() {
        let raw = encode_pre_sync_dormant_bucket_ix(true, -100, 3, 4).unwrap();
        assert_eq!(&raw[..8], &DISC_PRE_SYNC_DORMANT_BUCKET);
        assert_eq!(raw[8], 1u8);
        assert_eq!(&raw[9..17], &(-100i64).to_le_bytes());
        assert_eq!(&raw[17..21], &3u32.to_le_bytes());
        assert_eq!(&raw[21..25], &4u32.to_le_bytes());
        assert_eq!(raw.len(), 8 + 1 + 8 + 4 + 4);
    }

    /// `encode_close_dormant_bucket_ix` layout: disc + bool + i64.
    #[test]
    fn encode_close_dormant_bucket_ix_layout() {
        let raw = encode_close_dormant_bucket_ix(false, 999).unwrap();
        assert_eq!(&raw[..8], &DISC_CLOSE_DORMANT_BUCKET);
        assert_eq!(raw[8], 0u8);
        assert_eq!(&raw[9..17], &999i64.to_le_bytes());
        assert_eq!(raw.len(), 8 + 1 + 8);
    }

    /// `encode_initialize_dormant_bucket_ix` same layout as close.
    #[test]
    fn encode_initialize_dormant_bucket_ix_layout() {
        let raw = encode_initialize_dormant_bucket_ix(true, -2_000_000_000).unwrap();
        assert_eq!(&raw[..8], &DISC_INITIALIZE_DORMANT_BUCKET);
        assert_eq!(raw[8], 1u8);
        assert_eq!(&raw[9..17], &(-2_000_000_000i64).to_le_bytes());
    }

    /// Negative slot / very large bucket counts encode losslessly —
    /// there's no clamping or validation in the encoder; the on-chain
    /// program does that.
    #[test]
    fn encoder_accepts_full_u32_u64_range_without_clamping() {
        let env = PriceEnvelopeArgs {
            p_now: u64::MAX,
            slot: u64::MAX - 1,
            expected_min: 0,
            expected_max: u64::MAX,
        };
        let raw = encode_sync_pool_ix(&env, u32::MAX, u32::MAX).unwrap();
        let body = PriceEnvelopeArgs::try_from_slice(&raw[8..40]).unwrap();
        assert_eq!(body, env);
        let long = u32::from_le_bytes(raw[40..44].try_into().unwrap());
        let short = u32::from_le_bytes(raw[44..48].try_into().unwrap());
        assert_eq!(long, u32::MAX);
        assert_eq!(short, u32::MAX);
    }

    /// Schema descriptor JSON is structurally valid + lists every
    /// public encoder + arg struct. Pin against frontend's
    /// `frontend/src/tx/encode.ts::SCHEMA_DESCRIPTOR`.
    #[test]
    fn schema_descriptor_lists_every_encoder_and_struct() {
        let json = schema_descriptor_json();
        for marker in [
            "PriceEnvelopeArgs",
            "OpenParams",
            "Discriminators",
            "DISC_OPEN_POSITION",
            "DISC_HARVEST_DUST",
            "p_now",
            "expected_max",
            "direction_is_long",
            "position_id",
        ] {
            assert!(
                json.contains(marker),
                "schema_descriptor_json missing marker: {marker}"
            );
        }
        assert!(json.starts_with('{') && json.ends_with('}'));
    }

    /// `EncodeError::Borsh` is wired even though no current ix arg
    /// layout can produce a borsh failure (all fixed-size primitives).
    /// This pins the type so future `Vec<…>`-bearing args surface
    /// errors via the same channel.
    #[test]
    fn encode_error_borsh_variant_is_constructible() {
        let e = EncodeError::Borsh("test".to_string());
        match e {
            EncodeError::Borsh(msg) => assert_eq!(msg, "test"),
        }
    }
}
