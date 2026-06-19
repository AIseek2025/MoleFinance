//! Wave 15 — Pure-Rust Anchor instruction encoders.
//!
//! Anchor encodes every instruction's body as
//! `[8-byte discriminator] ++ borsh(args)`,
//! where the discriminator is `sha256("global:<ix_name>")[..8]`. This
//! module ships byte-exact encoders for the wave-15 keeper instructions
//! plus the user-facing `open_position` / `close_position` so the
//! frontend wasm artifact can build full Solana `Instruction` payloads
//! without depending on `solana-sdk` (or even on JS shim libraries
//! like `@coral-xyz/anchor` whose build tree is much heavier).
//!
//! ## Why pure-Rust + wasm-pack rather than Anchor IDL JS bindings
//!
//! 1. **One source of truth.** The Borsh schemas in `lib.rs` and the
//!    instruction encoders here both live in this crate, so a wave-N
//!    schema bump propagates with one edit and one `wasm-pack build`.
//! 2. **Auditability.** The frontend's tx-construction path goes
//!    through Rust code that's already fuzzed and proptest-pinned;
//!    auditors don't have to read both Rust and TypeScript implementations.
//! 3. **Smaller bundle.** No `borsh-ts`, no `@coral-xyz/anchor`, no
//!    full IDL parser shipped to the browser. Only the discriminator
//!    constants and the field-by-field encoders we actually call.
//!
//! ## Account discriminator vs instruction discriminator
//!
//! Anchor uses **two** sha256-prefixed namespaces:
//!
//! - `account:<TypeName>` — for account discriminators (validated on
//!   read; computed in the frontend by `frontend/src/decoder/discriminators.ts`).
//! - `global:<ix_name>` — for instruction discriminators (computed
//!   here, since they are the first 8 bytes of every instruction
//!   payload the frontend signs).
//!
//! Both come from the same `sha256` primitive, so the wasm artifact's
//! cost is one `sha2` dep that runs offline at module init.

extern crate alloc;

use alloc::vec::Vec;

use borsh::BorshSerialize;
use sha2::{Digest, Sha256};

use crate::Pubkey32;

/// Anchor instruction-discriminator namespace.
pub const ANCHOR_INSTRUCTION_NAMESPACE: &str = "global:";

/// Compute the 8-byte Anchor instruction discriminator for the
/// instruction named `ix_name`. Equivalent to
/// `sha256("global:<ix_name>")[..8]`.
pub fn instruction_discriminator(ix_name: &str) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(ANCHOR_INSTRUCTION_NAMESPACE.as_bytes());
    hasher.update(ix_name.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

/// Compute the 8-byte Anchor account discriminator for the account
/// type named `account_name`. Equivalent to
/// `sha256("account:<account_name>")[..8]`. Mirrors the frontend's
/// `deriveAnchorAccountDiscriminator` so wasm consumers don't need
/// `@noble/hashes`.
pub fn account_discriminator(account_name: &str) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(b"account:");
    hasher.update(account_name.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

// =====================================================================
// PriceEnvelopeArgs (shared by open/close/sync_pool/claim_dormant_recovery)
// =====================================================================

/// Mirror of `programs/mole-option/src/instructions/mod.rs::PriceEnvelopeArgs`.
/// 4 × u64 = 32 bytes Borsh-encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize)]
pub struct PriceEnvelopeArgs {
    /// Current oracle price (PRICE_SCALE = 1e8).
    pub p_now: u64,
    /// Slot the price was fetched at.
    pub slot: u64,
    /// Lower bound of the assertion (inclusive).
    pub expected_min: u64,
    /// Upper bound of the assertion (inclusive).
    pub expected_max: u64,
}

// =====================================================================
// open_position
// =====================================================================

/// Argument tuple for `open_position`. Mirrors the on-chain
/// `OpenParams` struct (`programs/mole-option/src/instructions/open.rs`)
/// field-for-field; the BPF `try_from_slice` MUST produce the same
/// values.
///
/// Borsh layout:
/// `envelope[32] ++ direction_is_long[1] ++ gross_amount[8] ++ position_id[8]`
/// = 49 bytes body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize)]
pub struct OpenPositionArgs {
    /// Caller-side oracle-bound price envelope. The on-chain handler
    /// rejects if the oracle has drifted out of this band.
    pub envelope: PriceEnvelopeArgs,
    /// `true` for long, `false` for short.
    pub direction_is_long: bool,
    /// Gross collateral the trader is locking up (minor units).
    pub gross_amount: u64,
    /// Caller-provided position id — the on-chain handler derives
    /// the position PDA from `(owner, position_id)`.
    pub position_id: u64,
}

/// Encode the full `open_position` instruction body — discriminator
/// followed by Borsh(args). Returns `Vec<u8>` so wasm consumers can
/// move it across the FFI boundary as a `Uint8Array`.
pub fn encode_open_position(args: &OpenPositionArgs) -> Vec<u8> {
    encode_with_discriminator("open_position", args)
}

// =====================================================================
// close_position
// =====================================================================

/// Argument tuple for `close_position`.
///
/// Mirrors `programs/mole-option/src/lib.rs::close_position`:
/// `(envelope, long_bucket_count, short_bucket_count)`.
///
/// Borsh layout: `envelope[32] ++ long_bucket_count[4] ++ short_bucket_count[4]`
/// = 40 bytes body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize)]
pub struct ClosePositionArgs {
    /// Caller-side oracle-bound price envelope.
    pub envelope: PriceEnvelopeArgs,
    /// Number of long-side dormant buckets passed in
    /// `remaining_accounts`. Matches the wave-7 contract.
    pub long_bucket_count: u32,
    /// Number of short-side dormant buckets passed in
    /// `remaining_accounts`.
    pub short_bucket_count: u32,
}

/// Encode the full `close_position` instruction body.
pub fn encode_close_position(args: &ClosePositionArgs) -> Vec<u8> {
    encode_with_discriminator("close_position", args)
}

// =====================================================================
// keeper_leader_heartbeat / release / acquire
// =====================================================================

/// Argument tuple for `keeper_leader_heartbeat`. Wave 15 keepers
/// pass the current slot they observed (the on-chain handler also
/// reads `Clock::get()`; the slot is sent for telemetry + replay
/// detection).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize)]
pub struct KeeperLeaderHeartbeatArgs {
    /// Slot the keeper observed off-chain. The on-chain handler
    /// rejects if `args.observed_slot < lock.last_heartbeat_slot`
    /// (clock skew defence).
    pub observed_slot: u64,
}

/// Argument tuple for `keeper_leader_acquire`. Identical to the
/// heartbeat args — semantically just "claim while stale". Kept as
/// a separate type so call sites are explicit about intent and so
/// the on-chain ix can reject if `is_stale == false`.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize)]
pub struct KeeperLeaderAcquireArgs {
    /// Slot the keeper observed off-chain.
    pub observed_slot: u64,
}

/// Argument tuple for `keeper_leader_release`. No payload — the
/// signer is the holder, and the slot is read on-chain. We still
/// emit a one-byte (zero-len) Borsh body to keep Anchor's argument
/// decoder happy.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize)]
pub struct KeeperLeaderReleaseArgs {}

/// Encode the full `keeper_leader_heartbeat` instruction body.
pub fn encode_keeper_leader_heartbeat(args: &KeeperLeaderHeartbeatArgs) -> Vec<u8> {
    encode_with_discriminator("keeper_leader_heartbeat", args)
}

/// Encode the full `keeper_leader_acquire` instruction body.
pub fn encode_keeper_leader_acquire(args: &KeeperLeaderAcquireArgs) -> Vec<u8> {
    encode_with_discriminator("keeper_leader_acquire", args)
}

/// Encode the full `keeper_leader_release` instruction body.
pub fn encode_keeper_leader_release(args: &KeeperLeaderReleaseArgs) -> Vec<u8> {
    encode_with_discriminator("keeper_leader_release", args)
}

// =====================================================================
// PDA derivation helpers (purely off-chain; on-chain validation lives
// in Anchor's `seeds` constraint).
// =====================================================================

/// PDA seed prefix for `KeeperLeaderLock`. The Anchor account is
/// `seeds = [b"keeper_leader_lock", market.key().as_ref()]`.
pub const KEEPER_LEADER_LOCK_SEED: &[u8] = b"keeper_leader_lock";

/// PDA seed prefix for the `Market` account. Documented here so wasm
/// consumers don't have to thread strings through the FFI boundary
/// for every PDA derivation. Mirrors
/// `programs/mole-option/src/instructions/init.rs`.
pub const MARKET_SEED: &[u8] = b"market";

/// PDA seed prefix for `Position`.
pub const POSITION_SEED: &[u8] = b"position";

/// Returns the seeds for the `KeeperLeaderLock` PDA. Wasm consumers
/// pass these into `solana-web3.js`'s `PublicKey.findProgramAddressSync`
/// to compute the canonical pubkey.
pub fn keeper_leader_lock_seeds(market: &Pubkey32) -> [&[u8]; 2] {
    [KEEPER_LEADER_LOCK_SEED, market.as_slice()]
}

// =====================================================================
// internals
// =====================================================================

fn encode_with_discriminator<T: BorshSerialize>(ix_name: &str, args: &T) -> Vec<u8> {
    let disc = instruction_discriminator(ix_name);
    let mut out = Vec::with_capacity(8 + 64);
    out.extend_from_slice(&disc);
    args.serialize(&mut out)
        .expect("Borsh serialize of fixed-size args cannot fail");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wave 15 — instruction discriminator is exactly 8 bytes and
    /// stable across calls.
    #[test]
    fn instruction_discriminator_is_eight_bytes_and_stable() {
        let a = instruction_discriminator("open_position");
        let b = instruction_discriminator("open_position");
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
    }

    /// Wave 15 — account discriminator namespace differs from
    /// instruction namespace (collision-free).
    #[test]
    fn account_namespace_differs_from_instruction_namespace() {
        let ix = instruction_discriminator("KeeperLeaderLock");
        let acc = account_discriminator("KeeperLeaderLock");
        assert_ne!(ix, acc);
    }

    /// Wave 15 — golden discriminator vectors for the user-facing
    /// instructions. If any of these change we've broken every wallet
    /// that already pre-signed a tx; pin the byte sequence in CI.
    /// Computed via `printf 'global:<ix>' | shasum -a 256 | head -c 16`.
    #[test]
    fn instruction_discriminator_golden_vectors() {
        // sha256("global:open_position")[..8] = 87802f4d0f98f031
        assert_eq!(
            instruction_discriminator("open_position"),
            [0x87, 0x80, 0x2f, 0x4d, 0x0f, 0x98, 0xf0, 0x31]
        );
        // sha256("global:close_position")[..8] = 7b86510031446262
        assert_eq!(
            instruction_discriminator("close_position"),
            [0x7b, 0x86, 0x51, 0x00, 0x31, 0x44, 0x62, 0x62]
        );
        // sha256("global:keeper_leader_heartbeat")[..8] = 2f0b5a8bb7a4081c
        assert_eq!(
            instruction_discriminator("keeper_leader_heartbeat"),
            [0x2f, 0x0b, 0x5a, 0x8b, 0xb7, 0xa4, 0x08, 0x1c]
        );
    }

    fn dummy_envelope() -> PriceEnvelopeArgs {
        PriceEnvelopeArgs {
            p_now: 100_000_000,
            slot: 12_345,
            expected_min: 99_500_000,
            expected_max: 100_500_000,
        }
    }

    /// Wave 15 — open_position encoder: 8-byte discriminator + 49-byte
    /// Borsh args (envelope[32] + direction[1] + gross_amount[8] +
    /// position_id[8] = 49). Total encoded: 8 + 49 = 57 bytes.
    /// Mirrors `programs/mole-option/src/instructions/open.rs::OpenParams`.
    #[test]
    fn open_position_encoder_emits_57_bytes() {
        let args = OpenPositionArgs {
            envelope: dummy_envelope(),
            direction_is_long: true,
            gross_amount: 1_000_000,
            position_id: 0xdead_beef,
        };
        let raw = encode_open_position(&args);
        assert_eq!(raw.len(), 8 + 49);
        assert_eq!(&raw[..8], &instruction_discriminator("open_position"));
        // Envelope is 4 little-endian u64s starting at byte 8.
        assert_eq!(&raw[8..16], &args.envelope.p_now.to_le_bytes());
        assert_eq!(&raw[16..24], &args.envelope.slot.to_le_bytes());
        assert_eq!(&raw[24..32], &args.envelope.expected_min.to_le_bytes());
        assert_eq!(&raw[32..40], &args.envelope.expected_max.to_le_bytes());
        // direction_is_long is at byte 40.
        assert_eq!(raw[40], 0x01);
        // gross_amount, position_id are at 41..49 and 49..57.
        assert_eq!(&raw[41..49], &args.gross_amount.to_le_bytes());
        assert_eq!(&raw[49..57], &args.position_id.to_le_bytes());
    }

    /// Wave 15 — close_position encoder: 8-byte discriminator + 40-byte
    /// args (envelope[32] + long_bucket[4] + short_bucket[4] = 40).
    /// Total encoded: 48 bytes.
    #[test]
    fn close_position_encoder_emits_48_bytes() {
        let args = ClosePositionArgs {
            envelope: dummy_envelope(),
            long_bucket_count: 3,
            short_bucket_count: 0,
        };
        let raw = encode_close_position(&args);
        assert_eq!(raw.len(), 8 + 40);
        assert_eq!(&raw[..8], &instruction_discriminator("close_position"));
        assert_eq!(&raw[40..44], &3u32.to_le_bytes());
        assert_eq!(&raw[44..48], &0u32.to_le_bytes());
    }

    /// Wave 15 — keeper_leader_heartbeat encoder: 8 + 8 = 16 bytes.
    #[test]
    fn keeper_leader_heartbeat_encoder_emits_16_bytes() {
        let args = KeeperLeaderHeartbeatArgs { observed_slot: 1_000 };
        let raw = encode_keeper_leader_heartbeat(&args);
        assert_eq!(raw.len(), 8 + 8);
        assert_eq!(
            &raw[..8],
            &instruction_discriminator("keeper_leader_heartbeat")
        );
        // Slot bytes are little-endian.
        assert_eq!(&raw[8..16], &1_000u64.to_le_bytes());
    }

    /// Wave 15 — keeper_leader_release encoder: 8 + 0 = 8 bytes (no
    /// args).
    #[test]
    fn keeper_leader_release_encoder_emits_eight_bytes() {
        let raw = encode_keeper_leader_release(&KeeperLeaderReleaseArgs {});
        assert_eq!(raw.len(), 8);
        assert_eq!(
            raw,
            instruction_discriminator("keeper_leader_release").to_vec()
        );
    }

    /// Wave 15 — encoders are deterministic (same args → same bytes).
    /// Required so a wasm consumer can hash-compare tx payloads
    /// across keeper replicas.
    #[test]
    fn encoders_are_deterministic() {
        let args = OpenPositionArgs {
            envelope: dummy_envelope(),
            direction_is_long: true,
            gross_amount: 1_000_000,
            position_id: 0xdead_beef,
        };
        let a = encode_open_position(&args);
        let b = encode_open_position(&args);
        assert_eq!(a, b);
    }

    /// Wave 15 — keeper_leader_lock_seeds returns the canonical seed
    /// pair. The on-chain `seeds = [b"keeper_leader_lock",
    /// market.key().as_ref()]` constraint must observe the same
    /// bytes.
    #[test]
    fn keeper_leader_lock_seeds_returns_canonical_pair() {
        let market = [42u8; 32];
        let seeds = keeper_leader_lock_seeds(&market);
        assert_eq!(seeds[0], KEEPER_LEADER_LOCK_SEED);
        assert_eq!(seeds[1], &market[..]);
    }
}
