//! Wave 15 — Host- and wasm-buildable state machine for the
//! `KeeperLeaderLock` PDA.
//!
//! Originally introduced inside `chain-mirror::leader_lock` while the
//! frontend wasm artifact was still a wave-14 stop-gap. Wave 15
//! relocates it here so the **state machine, the borsh layout, AND
//! the on-chain account discriminator are all served by the same
//! crate that the frontend `wasm-pack`s.** That makes the leader
//! lock auditable end-to-end through one file:
//!
//! - The on-chain Anchor account (added in wave 15 to
//!   `programs/mole-option/src/state.rs`) MUST reproduce the borsh
//!   layout pinned by [`KeeperLeaderLock::borsh_layout_bytes`] —
//!   length 49, leading-byte option marker, then leader pubkey,
//!   `last_heartbeat_slot`, `takeover_threshold_slots`.
//! - The Anchor instruction `keeper_leader_heartbeat` (added in
//!   wave 15) MUST evaluate the same takeover predicate
//!   ([`KeeperLeaderLock::is_stale`]) and emit the same outcome
//!   variants ([`HeartbeatOutcome`]). The on-chain reject paths
//!   ([`HeartbeatOutcome::RejectedHeldByOther`],
//!   [`HeartbeatOutcome::RejectedClockSkew`]) are surfaced via
//!   `KeeperLeaderError` in `programs/mole-option/src/error.rs`.
//! - The keeper bot (`keeper-bot::run::run_loop_with_factory`) calls
//!   [`KeeperLeaderLock::try_heartbeat`] on the **host-side** mirror
//!   to predict whether the next on-chain ix submission will succeed,
//!   so a non-leader replica can short-circuit the tx and back off
//!   without paying signature fees.
//!
//! ## Invariants pinned by tests
//!
//! 1. At any slot `S`, exactly zero or one keeper holds the lock,
//!    where "holds" means `current_leader == Some(K) &&
//!    S - last_heartbeat_slot <= takeover_threshold_slots`. Pinned
//!    by [`tests::property_at_most_one_holder_under_arbitrary_heartbeat_sequence`].
//! 2. A keeper never has its heartbeat regress
//!    (`last_heartbeat_slot` only ever increases). Pinned by
//!    [`tests::backward_time_heartbeat_rejected`].
//! 3. Takeover only happens when the previous leader's heartbeat is
//!    strictly stale; fresh locks reject every signer except the
//!    current leader. Pinned by
//!    [`tests::non_leader_rejected_while_lock_fresh`] and
//!    [`tests::lock_at_exact_threshold_is_still_fresh`].
//! 4. The lock can never enter an "uninitialised but pretends to be
//!    fresh" state: a `current_leader == None` lock always counts as
//!    stale. Pinned by [`tests::empty_lock_is_always_stale`].

extern crate alloc;

use alloc::vec::Vec;

use borsh::{BorshDeserialize, BorshSerialize};

use crate::Pubkey32;

/// Length of the `KeeperLeaderLock` Borsh body (no discriminator).
/// Pinned by [`tests::borsh_layout_is_49_bytes_with_signed_leader_marker`]
/// and asserted on the on-chain Anchor account size in
/// `programs/mole-option/src/state.rs::KeeperLeaderLock::LEN`.
pub const KEEPER_LEADER_LOCK_BODY_LEN: usize = 49;

/// On-chain `KeeperLeaderLock` PDA, mirrored on the host.
///
/// Wave 15: the BPF account is the **fixed** 49-byte layout
/// `has_leader[1] ++ current_leader[32] ++ last_heartbeat_slot[8] ++
/// takeover_threshold_slots[8]`. We deliberately don't use
/// `Option<Pubkey32>` here because Borsh's option encoding is
/// variable-length (1 byte for `None`, 33 for `Some`), which would
/// force the on-chain Anchor account to either re-allocate per
/// transition or pre-allocate slack — neither is desirable.
/// The fixed layout matches Anchor's `space = LEN` semantics directly.
///
/// `current_leader` bytes are zeroed when `has_leader == false`; the
/// `current_leader_pubkey()` accessor returns `Option<Pubkey32>` for
/// callers that want pattern-matching ergonomics.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct KeeperLeaderLock {
    /// `true` when the lock currently records a leader. `false`
    /// before the very first heartbeat AND after a graceful release.
    pub has_leader: bool,
    /// Pubkey of the keeper currently holding the lock. All-zero
    /// when `has_leader == false`.
    pub current_leader: Pubkey32,
    /// Slot stamp of the most recent successful heartbeat by the
    /// current leader. Monotonically non-decreasing.
    pub last_heartbeat_slot: u64,
    /// How many slots can elapse before any keeper is allowed to
    /// take over. Sized in wave 15 to ~30 s = 75 slots on Solana
    /// mainnet (400 ms slot time); production-configurable via
    /// `GlobalConfig` once on chain.
    pub takeover_threshold_slots: u64,
}

impl KeeperLeaderLock {
    /// New, unowned lock. `current_slot` is recorded as the
    /// `last_heartbeat_slot` so that the lock is *immediately stale*
    /// (any keeper can claim it on the very next heartbeat) — this
    /// matches the "create-if-missing" Anchor flow where the lock is
    /// initialised by the first keeper to call the heartbeat ix.
    pub fn fresh(current_slot: u64, takeover_threshold_slots: u64) -> Self {
        Self {
            has_leader: false,
            current_leader: [0u8; 32],
            last_heartbeat_slot: current_slot,
            takeover_threshold_slots,
        }
    }

    /// Construct a lock already held by `leader` at `slot`.
    pub fn held_by(leader: Pubkey32, slot: u64, takeover_threshold_slots: u64) -> Self {
        Self {
            has_leader: true,
            current_leader: leader,
            last_heartbeat_slot: slot,
            takeover_threshold_slots,
        }
    }

    /// `Option<Pubkey32>` view of the current leader. `None` iff
    /// `has_leader == false`. Convenient for pattern-matching call
    /// sites; the underlying byte layout is the fixed 49-byte form.
    pub fn current_leader_pubkey(&self) -> Option<Pubkey32> {
        if self.has_leader { Some(self.current_leader) } else { None }
    }

    /// True iff the lock is currently held by `signer` at the given
    /// slot. Becomes false once the heartbeat goes stale, even before
    /// anyone calls `try_heartbeat`.
    pub fn is_held_by(&self, signer: &Pubkey32, current_slot: u64) -> bool {
        if !self.has_leader {
            return false;
        }
        self.current_leader == *signer && !self.is_stale(current_slot)
    }

    /// True iff the lock has no leader OR its current leader's
    /// heartbeat is older than `takeover_threshold_slots`.
    pub fn is_stale(&self, current_slot: u64) -> bool {
        if !self.has_leader {
            return true;
        }
        let elapsed = current_slot.saturating_sub(self.last_heartbeat_slot);
        elapsed > self.takeover_threshold_slots
    }

    /// Slots elapsed since last heartbeat, saturating at 0 if the
    /// caller's slot is somehow before our recorded heartbeat.
    pub fn slots_since_heartbeat(&self, current_slot: u64) -> u64 {
        current_slot.saturating_sub(self.last_heartbeat_slot)
    }

    /// Fixed 49-byte borsh-derive layout dump. Identical to the
    /// `BorshSerialize` derive output (pinned by
    /// [`tests::borsh_derive_matches_hand_rolled_layout`]). The on-
    /// chain Anchor account loader produces the same bytes after the
    /// 8-byte discriminator. Layout:
    ///
    /// `has_leader[1] ++ current_leader[32] ++ last_heartbeat_slot[8] ++ takeover_threshold_slots[8]`
    pub fn borsh_layout_bytes(&self) -> [u8; KEEPER_LEADER_LOCK_BODY_LEN] {
        let mut out = [0u8; KEEPER_LEADER_LOCK_BODY_LEN];
        out[0] = u8::from(self.has_leader);
        out[1..33].copy_from_slice(&self.current_leader);
        out[33..41].copy_from_slice(&self.last_heartbeat_slot.to_le_bytes());
        out[41..49].copy_from_slice(&self.takeover_threshold_slots.to_le_bytes());
        out
    }
}

/// Outcome of a `try_heartbeat` attempt. Mirrors the on-chain ix's
/// success/failure logging surface so the keeper bot's watchdog can
/// emit the right metric on every result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    /// First-ever heartbeat on this lock — the signer is now leader.
    AcquiredFresh,
    /// Signer took over from a stale previous leader. `previous_leader`
    /// may equal `signer` (self-recovery from a stall).
    AcquiredFromStale {
        /// The previous leader, whose heartbeat had gone stale.
        previous_leader: Pubkey32,
        /// How many slots had elapsed since the previous leader's
        /// last heartbeat. Useful for telemetry / RCA.
        elapsed_slots: u64,
    },
    /// Signer was already the leader and just refreshed the lock.
    Refreshed,
    /// The lock is currently held fresh by another keeper. Signer
    /// stays out.
    RejectedHeldByOther {
        /// Who currently holds the lock.
        current_leader: Pubkey32,
        /// How many slots until the lock goes stale. The keeper bot
        /// uses this to back off precisely.
        slots_until_stale: u64,
    },
    /// Caller's slot is non-monotonic (older than the recorded
    /// heartbeat slot). The wave-15 ix enforces strict monotonicity
    /// to prevent a malicious keeper from rolling the lock backwards
    /// after observing a stale state.
    RejectedClockSkew {
        /// The slot the caller passed.
        caller_slot: u64,
        /// The lock's recorded last_heartbeat_slot.
        recorded_slot: u64,
    },
}

impl HeartbeatOutcome {
    /// True iff the outcome indicates the signer now holds the
    /// lock. Used by [`crate::leader_lock::keeper_bot_should_submit`]
    /// to decide whether to attempt a transaction this tick.
    pub fn is_leader(&self) -> bool {
        matches!(
            self,
            HeartbeatOutcome::AcquiredFresh
                | HeartbeatOutcome::AcquiredFromStale { .. }
                | HeartbeatOutcome::Refreshed
        )
    }

    /// True iff the outcome was a write (state was mutated).
    pub fn mutated_lock(&self) -> bool {
        self.is_leader()
    }
}

impl KeeperLeaderLock {
    /// Attempt to heartbeat-as-`signer` at `current_slot`. Returns
    /// the outcome and (on success only) updates the lock in place.
    ///
    /// Behaviour matrix:
    ///
    /// | State                                   | Outcome                  | Lock mutation |
    /// |-----------------------------------------|--------------------------|---------------|
    /// | leader = None                           | `AcquiredFresh`          | leader, slot  |
    /// | leader = Some(X), stale, signer = X     | `AcquiredFromStale`      | slot          |
    /// | leader = Some(X), stale, signer != X    | `AcquiredFromStale`      | leader, slot  |
    /// | leader = Some(X), fresh, signer = X     | `Refreshed`              | slot          |
    /// | leader = Some(X), fresh, signer != X    | `RejectedHeldByOther`    | none          |
    /// | current_slot < last_heartbeat_slot      | `RejectedClockSkew`      | none          |
    pub fn try_heartbeat(
        &mut self,
        signer: Pubkey32,
        current_slot: u64,
    ) -> HeartbeatOutcome {
        if current_slot < self.last_heartbeat_slot {
            return HeartbeatOutcome::RejectedClockSkew {
                caller_slot: current_slot,
                recorded_slot: self.last_heartbeat_slot,
            };
        }

        if !self.has_leader {
            self.has_leader = true;
            self.current_leader = signer;
            self.last_heartbeat_slot = current_slot;
            return HeartbeatOutcome::AcquiredFresh;
        }

        let leader = self.current_leader;
        let stale = self.is_stale(current_slot);
        let elapsed = current_slot.saturating_sub(self.last_heartbeat_slot);
        if leader == signer {
            self.last_heartbeat_slot = current_slot;
            if stale {
                HeartbeatOutcome::AcquiredFromStale {
                    previous_leader: leader,
                    elapsed_slots: elapsed,
                }
            } else {
                HeartbeatOutcome::Refreshed
            }
        } else if stale {
            self.current_leader = signer;
            self.last_heartbeat_slot = current_slot;
            HeartbeatOutcome::AcquiredFromStale {
                previous_leader: leader,
                elapsed_slots: elapsed,
            }
        } else {
            let slots_until_stale =
                self.takeover_threshold_slots.saturating_sub(elapsed);
            HeartbeatOutcome::RejectedHeldByOther {
                current_leader: leader,
                slots_until_stale,
            }
        }
    }

    /// Attempt to release the lock as `signer` at `current_slot`. Only
    /// succeeds when `signer` is the current leader. After release the
    /// lock returns to the unowned state and is immediately stale, so
    /// any keeper (including the previous holder) can re-acquire on
    /// the next heartbeat.
    ///
    /// Wave 15 production usage: a keeper drains its outstanding tx
    /// queue then calls release on graceful shutdown so a standby
    /// replica doesn't have to wait `takeover_threshold_slots` slots
    /// before claiming leadership.
    pub fn try_release(
        &mut self,
        signer: Pubkey32,
        current_slot: u64,
    ) -> ReleaseOutcome {
        if !self.has_leader {
            return ReleaseOutcome::RejectedNotHeld;
        }
        if self.current_leader != signer {
            return ReleaseOutcome::RejectedNotHolder {
                current_leader: self.current_leader,
            };
        }
        self.has_leader = false;
        self.current_leader = [0u8; 32];
        self.last_heartbeat_slot = current_slot;
        ReleaseOutcome::Released
    }
}

/// Outcome of a `try_release` attempt. Mirrors the on-chain
/// `keeper_leader_release` ix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseOutcome {
    /// Lock was held by signer; now unowned and immediately stale.
    Released,
    /// Lock was held by someone else; no mutation.
    RejectedNotHolder {
        /// Who currently holds the lock.
        current_leader: Pubkey32,
    },
    /// Lock had no leader to release.
    RejectedNotHeld,
}

/// Convenience: should the keeper bot attempt a write tx this tick?
/// `lock` is the most recent on-chain mirror, `signer` is the keeper's
/// pubkey, `current_slot` is the latest observed Solana slot.
///
/// The keeper bot calls this at the top of every tick to decide
/// whether to even build a transaction — saving the cost of an
/// otherwise-rejected on-chain submission. The chain's own
/// `keeper_leader_heartbeat` ix runs the same predicate as the
/// authoritative gate.
pub fn keeper_bot_should_submit(
    lock: &KeeperLeaderLock,
    signer: &Pubkey32,
    current_slot: u64,
) -> bool {
    if lock.is_held_by(signer, current_slot) {
        return true;
    }
    // Stale lock: any signer is allowed to acquire on the next
    // heartbeat, so the bot proceeds (it'll race other keepers via
    // the chain ix). Fresh lock held by someone else: skip.
    lock.is_stale(current_slot)
}

/// Borsh-encode `lock` to the 49-byte body shape, prefixed with a
/// caller-supplied 8-byte Anchor account discriminator. Useful for
/// tests assembling a `MockAccountFetcher` payload that the keeper
/// bot's snapshot layer can decode.
pub fn encode_keeper_leader_lock_account(
    lock: &KeeperLeaderLock,
    discriminator: &[u8; 8],
) -> Vec<u8> {
    let body = lock.borsh_layout_bytes();
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(discriminator);
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const ALICE: Pubkey32 = [0xa1; 32];
    const BOB: Pubkey32 = [0xb0; 32];
    const CARL: Pubkey32 = [0xc4; 32];
    const TAKEOVER: u64 = 75;

    fn fresh() -> KeeperLeaderLock {
        KeeperLeaderLock::fresh(0, TAKEOVER)
    }

    #[test]
    fn empty_lock_is_always_stale() {
        let lock = fresh();
        assert!(lock.is_stale(0));
        assert!(lock.is_stale(u64::MAX));
        assert!(!lock.is_held_by(&ALICE, 0));
    }

    #[test]
    fn first_heartbeat_acquires_lock_for_signer() {
        let mut lock = fresh();
        let r = lock.try_heartbeat(ALICE, 100);
        assert_eq!(r, HeartbeatOutcome::AcquiredFresh);
        assert_eq!(lock.current_leader_pubkey(), Some(ALICE));
        assert_eq!(lock.last_heartbeat_slot, 100);
        assert!(lock.is_held_by(&ALICE, 100));
    }

    #[test]
    fn fresh_leader_refresh_extends_slot_without_handoff() {
        let mut lock = KeeperLeaderLock::held_by(ALICE, 100, TAKEOVER);
        let r = lock.try_heartbeat(ALICE, 100 + TAKEOVER);
        assert_eq!(r, HeartbeatOutcome::Refreshed);
        assert_eq!(lock.last_heartbeat_slot, 100 + TAKEOVER);
        assert_eq!(lock.current_leader_pubkey(), Some(ALICE));
    }

    #[test]
    fn non_leader_rejected_while_lock_fresh() {
        let mut lock = KeeperLeaderLock::held_by(ALICE, 100, TAKEOVER);
        let r = lock.try_heartbeat(BOB, 100 + 30);
        match r {
            HeartbeatOutcome::RejectedHeldByOther {
                current_leader,
                slots_until_stale,
            } => {
                assert_eq!(current_leader, ALICE);
                assert_eq!(slots_until_stale, TAKEOVER - 30);
            }
            other => panic!("expected RejectedHeldByOther, got {other:?}"),
        }
        assert_eq!(lock.current_leader_pubkey(), Some(ALICE));
        assert_eq!(lock.last_heartbeat_slot, 100);
    }

    #[test]
    fn stale_lock_takeover_promotes_new_signer() {
        let mut lock = KeeperLeaderLock::held_by(ALICE, 100, TAKEOVER);
        let r = lock.try_heartbeat(BOB, 100 + TAKEOVER + 1);
        match r {
            HeartbeatOutcome::AcquiredFromStale {
                previous_leader,
                elapsed_slots,
            } => {
                assert_eq!(previous_leader, ALICE);
                assert_eq!(elapsed_slots, TAKEOVER + 1);
            }
            other => panic!("expected AcquiredFromStale, got {other:?}"),
        }
        assert_eq!(lock.current_leader_pubkey(), Some(BOB));
        assert_eq!(lock.last_heartbeat_slot, 100 + TAKEOVER + 1);
        assert!(lock.is_held_by(&BOB, 100 + TAKEOVER + 1));
        assert!(!lock.is_held_by(&ALICE, 100 + TAKEOVER + 1));
    }

    #[test]
    fn lock_at_exact_threshold_is_still_fresh() {
        let mut lock = KeeperLeaderLock::held_by(ALICE, 100, TAKEOVER);
        let r = lock.try_heartbeat(BOB, 100 + TAKEOVER);
        match r {
            HeartbeatOutcome::RejectedHeldByOther { .. } => {}
            other => panic!("expected RejectedHeldByOther at boundary, got {other:?}"),
        }
        assert!(!lock.is_stale(100 + TAKEOVER));
        assert!(lock.is_stale(100 + TAKEOVER + 1));
    }

    #[test]
    fn backward_time_heartbeat_rejected() {
        let mut lock = KeeperLeaderLock::held_by(ALICE, 100, TAKEOVER);
        let r = lock.try_heartbeat(ALICE, 50);
        assert_eq!(
            r,
            HeartbeatOutcome::RejectedClockSkew {
                caller_slot: 50,
                recorded_slot: 100,
            }
        );
        assert_eq!(lock.last_heartbeat_slot, 100);
    }

    #[test]
    fn three_keeper_rotation_chain_records_each_predecessor() {
        let mut lock = fresh();
        assert_eq!(lock.try_heartbeat(ALICE, 100), HeartbeatOutcome::AcquiredFresh);
        let after_a = lock.last_heartbeat_slot;
        let r = lock.try_heartbeat(BOB, after_a + TAKEOVER + 1);
        match r {
            HeartbeatOutcome::AcquiredFromStale { previous_leader, .. } => {
                assert_eq!(previous_leader, ALICE);
            }
            other => panic!("step B: {other:?}"),
        }
        let after_b = lock.last_heartbeat_slot;
        let r = lock.try_heartbeat(CARL, after_b + TAKEOVER + 5);
        match r {
            HeartbeatOutcome::AcquiredFromStale { previous_leader, .. } => {
                assert_eq!(previous_leader, BOB);
            }
            other => panic!("step C: {other:?}"),
        }
        assert_eq!(lock.current_leader_pubkey(), Some(CARL));
    }

    #[test]
    fn stale_leader_recovers_via_acquired_from_stale_with_self_as_predecessor() {
        let mut lock = KeeperLeaderLock::held_by(ALICE, 100, TAKEOVER);
        let slot = 100 + TAKEOVER + 10;
        let r = lock.try_heartbeat(ALICE, slot);
        match r {
            HeartbeatOutcome::AcquiredFromStale {
                previous_leader,
                elapsed_slots,
            } => {
                assert_eq!(previous_leader, ALICE);
                assert_eq!(elapsed_slots, TAKEOVER + 10);
            }
            other => panic!("expected stale-self AcquiredFromStale, got {other:?}"),
        }
        assert_eq!(lock.current_leader_pubkey(), Some(ALICE));
        assert_eq!(lock.last_heartbeat_slot, slot);
    }

    #[test]
    fn borsh_layout_is_49_bytes_with_signed_leader_marker() {
        let bytes = fresh().borsh_layout_bytes();
        assert_eq!(bytes.len(), KEEPER_LEADER_LOCK_BODY_LEN);
        assert_eq!(bytes[0], 0);
        assert_eq!(&bytes[1..33], &[0u8; 32]);
        let held = KeeperLeaderLock::held_by(ALICE, 7, TAKEOVER);
        let bytes = held.borsh_layout_bytes();
        assert_eq!(bytes[0], 1);
        assert_eq!(&bytes[1..33], &ALICE);
        assert_eq!(&bytes[33..41], &7u64.to_le_bytes());
        assert_eq!(&bytes[41..49], &TAKEOVER.to_le_bytes());
    }

    /// Wave 15 — the BorshSerialize derive must produce the same 49
    /// bytes as the hand-rolled `borsh_layout_bytes`. This is the
    /// invariant the on-chain Anchor account loader relies on. Pinned
    /// for both the held and unowned states because Borsh's encoding
    /// of `bool + [u8;32]` is fixed-size, unlike the variable-size
    /// `Option<Pubkey>` form we deliberately avoided.
    #[test]
    fn borsh_derive_matches_hand_rolled_layout() {
        for lock in [
            KeeperLeaderLock::held_by(ALICE, 0xdead_beef, TAKEOVER),
            KeeperLeaderLock::fresh(0, TAKEOVER),
        ] {
            let mut derived = Vec::new();
            lock.serialize(&mut derived).unwrap();
            let hand = lock.borsh_layout_bytes();
            assert_eq!(
                derived.as_slice(),
                &hand[..],
                "mismatch for has_leader = {}",
                lock.has_leader
            );
            assert_eq!(derived.len(), KEEPER_LEADER_LOCK_BODY_LEN);
            // Round trip through derive deserialize.
            let parsed = KeeperLeaderLock::try_from_slice(&derived).unwrap();
            assert_eq!(parsed, lock);
        }
    }

    /// Wave 15 release path — only the holder can release; release
    /// returns the lock to the unowned state.
    #[test]
    fn release_only_succeeds_for_holder() {
        let mut lock = KeeperLeaderLock::held_by(ALICE, 100, TAKEOVER);
        // Non-holder can't release.
        let r = lock.try_release(BOB, 110);
        match r {
            ReleaseOutcome::RejectedNotHolder { current_leader } => {
                assert_eq!(current_leader, ALICE);
            }
            other => panic!("expected RejectedNotHolder, got {other:?}"),
        }
        assert_eq!(lock.current_leader_pubkey(), Some(ALICE));
        // Holder can.
        let r = lock.try_release(ALICE, 110);
        assert_eq!(r, ReleaseOutcome::Released);
        assert_eq!(lock.current_leader_pubkey(), None);
        // After release the next heartbeat is `AcquiredFresh`, even
        // from the previous holder.
        let r = lock.try_heartbeat(ALICE, 111);
        assert_eq!(r, HeartbeatOutcome::AcquiredFresh);
    }

    /// Wave 15 release path — releasing an unowned lock is a no-op
    /// rejection (idempotent).
    #[test]
    fn release_on_unowned_lock_rejects() {
        let mut lock = fresh();
        let r = lock.try_release(ALICE, 100);
        assert_eq!(r, ReleaseOutcome::RejectedNotHeld);
        assert_eq!(lock.current_leader_pubkey(), None);
    }

    /// Wave 15 — `keeper_bot_should_submit` mirrors the on-chain
    /// gate so a non-leader replica doesn't even build a tx.
    #[test]
    fn keeper_bot_should_submit_predicates() {
        let lock = KeeperLeaderLock::held_by(ALICE, 100, TAKEOVER);
        // Holder, fresh: submit.
        assert!(keeper_bot_should_submit(&lock, &ALICE, 110));
        // Non-holder, fresh: skip.
        assert!(!keeper_bot_should_submit(&lock, &BOB, 110));
        // Non-holder, stale: submit (will race for takeover).
        assert!(keeper_bot_should_submit(&lock, &BOB, 100 + TAKEOVER + 1));
        // Unowned: anyone may submit.
        let unowned = KeeperLeaderLock::fresh(0, TAKEOVER);
        assert!(keeper_bot_should_submit(&unowned, &BOB, 0));
    }

    /// Wave 15 — `encode_keeper_leader_lock_account` produces 8 +
    /// 49 = 57 bytes with a caller discriminator and the 49-byte
    /// body. Keeps mock account fetchers honest.
    #[test]
    fn encoded_account_payload_is_57_bytes() {
        let lock = KeeperLeaderLock::held_by(ALICE, 9, TAKEOVER);
        let raw = encode_keeper_leader_lock_account(&lock, &[7u8; 8]);
        assert_eq!(raw.len(), 8 + KEEPER_LEADER_LOCK_BODY_LEN);
        assert_eq!(&raw[..8], &[7u8; 8]);
        assert_eq!(&raw[8..], &lock.borsh_layout_bytes()[..]);
    }

    /// Wave 15 — outcome introspection: `is_leader` and `mutated_lock`
    /// agree on every variant. Used by the bot's run-loop to decide
    /// whether to proceed with a tx submission this tick.
    #[test]
    fn outcome_is_leader_matches_mutation() {
        let acquired = HeartbeatOutcome::AcquiredFresh;
        assert!(acquired.is_leader());
        assert!(acquired.mutated_lock());
        let stale = HeartbeatOutcome::AcquiredFromStale {
            previous_leader: ALICE,
            elapsed_slots: 1,
        };
        assert!(stale.is_leader());
        let refreshed = HeartbeatOutcome::Refreshed;
        assert!(refreshed.is_leader());
        let other = HeartbeatOutcome::RejectedHeldByOther {
            current_leader: ALICE,
            slots_until_stale: 5,
        };
        assert!(!other.is_leader());
        assert!(!other.mutated_lock());
        let skew = HeartbeatOutcome::RejectedClockSkew {
            caller_slot: 1,
            recorded_slot: 2,
        };
        assert!(!skew.is_leader());
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 256,
            ..ProptestConfig::default()
        })]
        #[test]
        fn property_at_most_one_holder_under_arbitrary_heartbeat_sequence(
            seq in proptest::collection::vec((0u8..3u8, 0u64..200u64), 0..32)
        ) {
            let signers = [ALICE, BOB, CARL];
            let mut lock = KeeperLeaderLock::fresh(0, TAKEOVER);
            let mut current_slot = 0u64;
            for (idx, delta) in seq {
                current_slot = current_slot.saturating_add(delta);
                let signer = signers[idx as usize];
                let _ = lock.try_heartbeat(signer, current_slot);
            }
            let final_slot = current_slot.saturating_add(1);
            let mut held_count = 0u32;
            for s in &signers {
                if lock.is_held_by(s, final_slot) {
                    held_count += 1;
                }
            }
            prop_assert!(held_count <= 1);
        }
    }
}
