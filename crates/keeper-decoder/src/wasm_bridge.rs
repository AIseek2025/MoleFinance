//! Wave 15 — Wasm-bindgen FFI surface.
//!
//! Compiled only under `--features wasm` (the wave-15 wasm-pack target).
//! Host-side crates (`keeper-rpc`, `chain-mirror`, `keeper-bot`)
//! consume this crate without the `wasm` feature, so this module
//! stays out of their compile graph.
//!
//! ## Surface
//!
//! - `wasm_init()` — wires up `console_error_panic_hook` for prettier
//!   panics in the browser console. Idempotent.
//! - `instruction_discriminator(name)` — returns the 8-byte
//!   discriminator as a `Uint8Array`.
//! - `account_discriminator(name)` — same for account discriminators.
//! - `encode_open_position(...)` / `encode_close_position(...)` —
//!   pure-Rust ix encoders returning `Uint8Array`.
//! - `encode_keeper_leader_heartbeat(...)` — keeper-side encoder.
//! - `decode_keeper_leader_lock(bytes)` — returns a JS object with
//!   the lock fields decoded (omits the 8-byte discriminator if
//!   present; accepts both raw-49 and full-57 byte payloads).
//! - `keeper_leader_lock_seed_prefix()` — the canonical PDA seed.
//!
//! ## Why JS-friendly types
//!
//! `Uint8Array` and primitive `BigInt` round-trip cleanly across the
//! wasm-bindgen ABI; `[u8; 8]` would land as a generic `JsValue` and
//! force the TS adapter into manual length-checks. Returning
//! `Uint8Array` keeps the TS call sites short:
//!
//! ```ignore
//! const ixBytes = encode_open_position(7, true, ...);  // Uint8Array
//! const tx = new Transaction().add({ data: Buffer.from(ixBytes), ... });
//! ```

use alloc::string::ToString;
use alloc::vec::Vec;

use wasm_bindgen::prelude::*;

use crate::ix::{
    self, ClosePositionArgs, KeeperLeaderAcquireArgs, KeeperLeaderHeartbeatArgs,
    KeeperLeaderReleaseArgs, KEEPER_LEADER_LOCK_SEED, OpenPositionArgs, PriceEnvelopeArgs,
};
use crate::leader_lock::{KEEPER_LEADER_LOCK_BODY_LEN, KeeperLeaderLock};
use crate::ANCHOR_DISCRIMINATOR_LEN;

/// Idempotent bootstrap. Frontend calls this once on page load.
#[wasm_bindgen(js_name = "wasmInit")]
pub fn wasm_init() {
    console_error_panic_hook::set_once();
}

/// Compute the 8-byte Anchor instruction discriminator for `name`.
#[wasm_bindgen(js_name = "instructionDiscriminator")]
pub fn instruction_discriminator(name: &str) -> Vec<u8> {
    ix::instruction_discriminator(name).to_vec()
}

/// Compute the 8-byte Anchor account discriminator for `name`.
#[wasm_bindgen(js_name = "accountDiscriminator")]
pub fn account_discriminator(name: &str) -> Vec<u8> {
    ix::account_discriminator(name).to_vec()
}

/// Wasm-friendly form of `encode_open_position`. Mirrors the
/// on-chain `OpenParams` field layout (envelope + direction +
/// gross_amount + position_id). Plumbed via primitive-only args
/// because wasm-bindgen doesn't accept Rust structs across the
/// FFI boundary; JS-side `bigint` values come across as `u64`.
///
/// Wire layout (the returned `Uint8Array`):
/// `disc[8] ++ p_now[8] ++ slot[8] ++ expected_min[8] ++ expected_max[8] ++ direction[1] ++ gross_amount[8] ++ position_id[8]`
#[wasm_bindgen(js_name = "encodeOpenPosition")]
#[allow(clippy::too_many_arguments)]
pub fn encode_open_position(
    envelope_p_now: u64,
    envelope_slot: u64,
    envelope_expected_min: u64,
    envelope_expected_max: u64,
    direction_is_long: bool,
    gross_amount: u64,
    position_id: u64,
) -> Vec<u8> {
    ix::encode_open_position(&OpenPositionArgs {
        envelope: PriceEnvelopeArgs {
            p_now: envelope_p_now,
            slot: envelope_slot,
            expected_min: envelope_expected_min,
            expected_max: envelope_expected_max,
        },
        direction_is_long,
        gross_amount,
        position_id,
    })
}

/// Wasm-friendly form of `encode_close_position`. Mirrors the
/// on-chain `close_position(envelope, long_bucket_count, short_bucket_count)`
/// signature.
#[wasm_bindgen(js_name = "encodeClosePosition")]
#[allow(clippy::too_many_arguments)]
pub fn encode_close_position(
    envelope_p_now: u64,
    envelope_slot: u64,
    envelope_expected_min: u64,
    envelope_expected_max: u64,
    long_bucket_count: u32,
    short_bucket_count: u32,
) -> Vec<u8> {
    ix::encode_close_position(&ClosePositionArgs {
        envelope: PriceEnvelopeArgs {
            p_now: envelope_p_now,
            slot: envelope_slot,
            expected_min: envelope_expected_min,
            expected_max: envelope_expected_max,
        },
        long_bucket_count,
        short_bucket_count,
    })
}

/// Wasm-friendly form of `encode_keeper_leader_heartbeat`.
#[wasm_bindgen(js_name = "encodeKeeperLeaderHeartbeat")]
pub fn encode_keeper_leader_heartbeat(observed_slot: u64) -> Vec<u8> {
    ix::encode_keeper_leader_heartbeat(&KeeperLeaderHeartbeatArgs { observed_slot })
}

/// Wave 17 — wasm-friendly form of `encode_keeper_leader_acquire`.
/// Same byte layout as heartbeat (8 disc + 8 LE u64) but a different
/// discriminator. The frontend's manual ops console uses this when
/// the operator wants to force-take a stale lock from the browser
/// rather than from the `ops-toolkit/ts/keeper-leader-acquire.ts`
/// CLI.
#[wasm_bindgen(js_name = "encodeKeeperLeaderAcquire")]
pub fn encode_keeper_leader_acquire(observed_slot: u64) -> Vec<u8> {
    ix::encode_keeper_leader_acquire(&KeeperLeaderAcquireArgs { observed_slot })
}

/// Wasm-friendly form of `encode_keeper_leader_release`.
#[wasm_bindgen(js_name = "encodeKeeperLeaderRelease")]
pub fn encode_keeper_leader_release() -> Vec<u8> {
    ix::encode_keeper_leader_release(&KeeperLeaderReleaseArgs {})
}

/// JS-friendly view of a decoded `KeeperLeaderLock`.
///
/// Wave 15 wasm-bindgen produces JS getters for each field, so the
/// frontend can pattern-match a `decodeKeeperLeaderLock` result without
/// pulling Borsh client-side.
#[wasm_bindgen]
pub struct KeeperLeaderLockView {
    has_leader: bool,
    current_leader: [u8; 32],
    last_heartbeat_slot: u64,
    takeover_threshold_slots: u64,
}

#[wasm_bindgen]
impl KeeperLeaderLockView {
    /// `true` iff the lock currently records a leader.
    #[wasm_bindgen(getter, js_name = "hasLeader")]
    pub fn has_leader(&self) -> bool {
        self.has_leader
    }

    /// Current leader pubkey (32 bytes). When `hasLeader == false`
    /// the bytes are all-zero.
    #[wasm_bindgen(getter, js_name = "currentLeader")]
    pub fn current_leader(&self) -> Vec<u8> {
        self.current_leader.to_vec()
    }

    /// Slot of the last heartbeat.
    #[wasm_bindgen(getter, js_name = "lastHeartbeatSlot")]
    pub fn last_heartbeat_slot(&self) -> u64 {
        self.last_heartbeat_slot
    }

    /// Configured takeover threshold (slots).
    #[wasm_bindgen(getter, js_name = "takeoverThresholdSlots")]
    pub fn takeover_threshold_slots(&self) -> u64 {
        self.takeover_threshold_slots
    }
}

/// Decode a `KeeperLeaderLock` payload. Accepts:
/// - the 49-byte borsh body (no discriminator), OR
/// - the full 57-byte Anchor account payload (8-byte disc prefix + body), OR
/// - any larger buffer where the first `disc + body` bytes are the
///   payload (Anchor pads accounts to 8-byte multiples in some
///   versions; 64 bytes is common in the wild).
///
/// On error, throws a JsError with a description string the frontend
/// can surface in the keeper console panel.
#[wasm_bindgen(js_name = "decodeKeeperLeaderLock")]
pub fn decode_keeper_leader_lock(bytes: &[u8]) -> Result<KeeperLeaderLockView, JsError> {
    let decoded: KeeperLeaderLock = if bytes.len() == KEEPER_LEADER_LOCK_BODY_LEN {
        borsh::BorshDeserialize::try_from_slice(bytes)
            .map_err(|e| JsError::new(&e.to_string()))?
    } else if bytes.len() >= ANCHOR_DISCRIMINATOR_LEN + KEEPER_LEADER_LOCK_BODY_LEN {
        // Anchor account form: skip the 8-byte discriminator and
        // borsh-decode the next 49 bytes. We use slice-then-decode
        // rather than `decode_anchor_account` because the latter
        // calls `try_from_slice` on the whole tail, which fails
        // ("Not all bytes read") when the on-chain account is padded
        // to 8-byte multiples by Anchor's loader.
        let body = &bytes[ANCHOR_DISCRIMINATOR_LEN
            ..ANCHOR_DISCRIMINATOR_LEN + KEEPER_LEADER_LOCK_BODY_LEN];
        borsh::BorshDeserialize::try_from_slice(body)
            .map_err(|e| JsError::new(&e.to_string()))?
    } else {
        return Err(JsError::new(&alloc::format!(
            "keeper-leader-lock payload too short: got {} bytes, need {} or {}",
            bytes.len(),
            KEEPER_LEADER_LOCK_BODY_LEN,
            ANCHOR_DISCRIMINATOR_LEN + KEEPER_LEADER_LOCK_BODY_LEN
        )));
    };

    Ok(KeeperLeaderLockView {
        has_leader: decoded.has_leader,
        current_leader: decoded.current_leader,
        last_heartbeat_slot: decoded.last_heartbeat_slot,
        takeover_threshold_slots: decoded.takeover_threshold_slots,
    })
}

/// PDA seed prefix `b"keeper_leader_lock"` for wasm consumers.
#[wasm_bindgen(js_name = "keeperLeaderLockSeedPrefix")]
pub fn keeper_leader_lock_seed_prefix() -> Vec<u8> {
    KEEPER_LEADER_LOCK_SEED.to_vec()
}
