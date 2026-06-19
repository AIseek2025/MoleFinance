//! Wave 14 — thin re-export layer over [`keeper_decoder`].
//!
//! Until wave 13 the `Onchain*` Borsh schemas lived inline here;
//! wave 14 moved them into the standalone `keeper-decoder` crate so
//! the same schema source builds on both host AND
//! `wasm32-unknown-unknown` (which lets wave 15 ship them to the
//! frontend via `wasm-pack` without a parallel reimplementation).
//!
//! The re-exports below preserve every external call site:
//! `keeper_rpc::accounts::OnchainSubPool`,
//! `keeper_rpc::accounts::decode_anchor_account`, etc. — these
//! continue to work unchanged. The wave-14 unit tests live in the
//! decoder crate; this module's tests focus on **bridge integrity**
//! (the re-export still resolves the same struct identity, so any
//! caller that fans out from `keeper-rpc::accounts` matches the
//! crate's own decoders byte-for-byte).
//!
//! Layout MUST match `programs/mole-option/src/state.rs` byte-for-
//! byte. Field-level accountability lives in
//! `Docs/SCHEMA-MAPPING.md`; runtime-size invariants are pinned by
//! `crates/clearing-core/tests/onchain_layout.rs`.

pub use keeper_decoder::{
    AccountDecodeError, OnchainDistEntry, OnchainDistributionLedger, OnchainDormantBucket,
    OnchainMarket, OnchainSubPool, decode_anchor_account, decode_anchor_account_with_discriminator,
    encode_anchor_account, schema_descriptor_json,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pubkey32;

    fn dummy_pubkey(seed: u8) -> Pubkey32 {
        let mut p = [0u8; 32];
        p[0] = seed;
        p
    }

    /// Bridge integrity: a struct constructed via the re-exported
    /// `keeper_rpc::accounts::OnchainSubPool` round-trips through
    /// the re-exported encode / decode helpers byte-for-byte. If a
    /// future refactor accidentally splits the type (two distinct
    /// definitions) this test catches the drift.
    #[test]
    fn re_exported_decoder_round_trips_sub_pool() {
        let sp = OnchainSubPool {
            market: dummy_pubkey(1),
            sub_pool_id: 7,
            long_pool_equity: 1_234_567_890,
            short_pool_equity: 234_567_890,
            long_active_shares: 100_000,
            short_active_shares: 90_000,
            long_recovery_shares: 50,
            short_recovery_shares: 40,
            long_active_notional: 5_000_000,
            short_active_notional: 4_000_000,
            long_active_generation: 3,
            short_active_generation: 2,
            last_price: 100_000_000,
            last_sync_slot: 12345,
            long_dust: 7,
            short_dust: 9,
            long_dormant_bucket_count: 12,
            short_dormant_bucket_count: 8,
            bump: 254,
            _pad: [0; 7],
        };
        let raw = encode_anchor_account(&sp, &[1u8; 8]).unwrap();
        let sp2: OnchainSubPool = decode_anchor_account(&raw).unwrap();
        assert_eq!(sp, sp2);
    }

    /// Discriminator-strict path is identically wired through.
    #[test]
    fn re_exported_strict_decoder_rejects_mismatch() {
        let bucket = OnchainDormantBucket {
            sub_pool: dummy_pubkey(2),
            direction_is_long: false,
            zero_price_tick: 99,
            anchor_price: 1_000_000,
            total_recovery_shares: 1,
            total_recovery_notional: 1,
            accrued_value: 1,
            position_count: 1,
            last_applied_index: 0,
            bump: 1,
            _pad: [0; 6],
        };
        let raw = encode_anchor_account(&bucket, &[7u8; 8]).unwrap();
        let err = decode_anchor_account_with_discriminator::<OnchainDormantBucket>(&raw, &[8u8; 8])
            .unwrap_err();
        assert!(matches!(err, AccountDecodeError::DiscriminatorMismatch { .. }));
    }

    /// Schema descriptor JSON is reachable through the re-export
    /// path. Frontend tooling should import it via either path.
    #[test]
    fn re_exported_schema_descriptor_is_available() {
        let json = schema_descriptor_json();
        assert!(json.contains("OnchainMarket"));
    }
}
