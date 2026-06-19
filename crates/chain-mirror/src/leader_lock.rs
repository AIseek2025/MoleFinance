//! Wave 15 — Re-export shim for the keeper-leader-lock state machine.
//!
//! The implementation moved into `keeper-decoder::leader_lock` in
//! wave 15 so the borsh layout, the state machine, and the wasm-
//! shipped frontend mirror all live in one schema-only crate. This
//! file keeps the original `chain_mirror::leader_lock::*` paths
//! linkable for any host-side consumer that already imports through
//! chain-mirror; new code should prefer `keeper_decoder::leader_lock`.
//!
//! The wave-15 audit invariants (host parity tests, borsh layout
//! pin, property test) live alongside the implementation in
//! `keeper-decoder/src/leader_lock.rs`. A small bridge integrity
//! test below confirms the re-export round-trips.

pub use keeper_decoder::leader_lock::{
    encode_keeper_leader_lock_account, keeper_bot_should_submit, HeartbeatOutcome,
    KeeperLeaderLock, ReleaseOutcome, KEEPER_LEADER_LOCK_BODY_LEN,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pubkey32;

    /// Wave 15 — bridge integrity. Constructing through the
    /// `chain_mirror` re-export must produce a value byte-equal to
    /// the one produced via the canonical `keeper_decoder` path.
    #[test]
    fn chain_mirror_reexport_matches_keeper_decoder_canonical_path() {
        let alice: Pubkey32 = [0xa1; 32];
        let from_chain = KeeperLeaderLock::held_by(alice, 100, 75);
        let from_decoder = keeper_decoder::leader_lock::KeeperLeaderLock::held_by(
            alice, 100, 75,
        );
        assert_eq!(
            from_chain.borsh_layout_bytes(),
            from_decoder.borsh_layout_bytes()
        );
    }

    /// Wave 15 — `HeartbeatOutcome::is_leader` is part of the public
    /// re-export surface. Confirm the variant survives the shim.
    #[test]
    fn reexported_outcome_methods_are_callable() {
        let acquired = HeartbeatOutcome::AcquiredFresh;
        assert!(acquired.is_leader());
    }

    /// Wave 15 — `keeper_bot_should_submit` is what the keeper bot's
    /// run-loop calls; ensure it's reachable through the chain-mirror
    /// re-export path so existing call sites don't need changes.
    #[test]
    fn keeper_bot_predicate_round_trip_via_reexport() {
        let alice: Pubkey32 = [0xa1; 32];
        let lock = KeeperLeaderLock::held_by(alice, 100, 75);
        assert!(keeper_bot_should_submit(&lock, &alice, 110));
    }
}
