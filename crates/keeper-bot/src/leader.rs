//! Wave 15 — host-side leader-lock policy for the keeper run-loop.
//!
//! ## Why this exists
//!
//! Wave 12 introduced a `LeaderStatus` metric so ops could observe
//! which replica thought it was the leader, but the bot itself
//! always submitted transactions (the `set_leader_status` call in
//! `main.rs` was hard-coded to `LeaderStatus::Leader`). The
//! observability was correct; the gate was missing.
//!
//! Wave 15 closes the loop. The bot now:
//!
//! 1. Reads the on-chain `KeeperLeaderLock` PDA (or, in tests,
//!    a host-mirrored copy).
//! 2. Calls [`KeeperLeaderLock::try_heartbeat`] to compute the
//!    expected outcome of the next on-chain
//!    `keeper_leader_heartbeat` ix. Mutates the cached mirror in
//!    place, so subsequent ticks see the updated heartbeat slot.
//! 3. If the outcome indicates the bot is now (or remains) leader,
//!    `should_submit` returns `true` → run-loop proceeds with the
//!    actions tx. The on-chain `keeper_leader_heartbeat` is
//!    submitted as a separate tx in the same tick (or batched into
//!    the actions tx by the wave-16 wiring; either way the lock
//!    state is reconciled with chain after every tick).
//! 4. If the outcome rejects (`RejectedHeldByOther` /
//!    `RejectedClockSkew`), `should_submit` returns `false` →
//!    run-loop skips dispatch this tick AND the metric flips to
//!    `LeaderStatus::Standby`.
//!
//! ## Test surface
//!
//! `LeaderPolicy` is a trait so wave-15 host tests can drive the
//! run-loop with a deterministic policy without standing up an RPC.
//! `HostMirrorLeaderPolicy` is the production-bound impl used in
//! both real deployments (mirror is reconciled with RPC each tick)
//! and the wave-15 integration tests (mirror is fed manually).
//!
//! ## Wave-12 file-lock fallback
//!
//! When `LeaderPolicy` is not configured (the default
//! `BotConfig::default()` keeps it disabled), the run-loop behaves
//! the same as wave-12 — every tick submits, the metric stays at
//! whatever ops configured externally. Wave 15 production
//! deployments wire the lock; wave-12 file-lock-only deployments
//! continue to work unchanged.

use std::sync::Mutex;

use keeper_decoder::Pubkey32;
use keeper_decoder::leader_lock::{HeartbeatOutcome, KeeperLeaderLock};

/// Decision interface invoked by the run-loop before every tick.
pub trait LeaderPolicy: Send + Sync {
    /// Should the run-loop dispatch actions this tick?
    fn should_submit(&self, current_slot: u64) -> bool;

    /// Notify the policy that an attempted submission produced an
    /// `outcome` (e.g. via the wave-15 `keeper_leader_heartbeat` ix).
    /// Used by `HostMirrorLeaderPolicy` to update its cached mirror.
    fn record_outcome(&self, _outcome: HeartbeatOutcome) {}
}

/// Wave-15 default: a host-mirrored `KeeperLeaderLock` driven by the
/// run-loop. Each tick runs `try_heartbeat` against the cached
/// mirror; production deployments additionally reconcile the mirror
/// with the on-chain lock account every N ticks.
pub struct HostMirrorLeaderPolicy {
    signer: Pubkey32,
    inner: Mutex<KeeperLeaderLock>,
}

impl HostMirrorLeaderPolicy {
    /// Construct a new policy with `signer` and an initial mirror.
    pub fn new(signer: Pubkey32, initial: KeeperLeaderLock) -> Self {
        Self {
            signer,
            inner: Mutex::new(initial),
        }
    }

    /// Replace the cached mirror — production keeper bots call this
    /// after every successful RPC reconcile of the on-chain lock.
    pub fn reconcile(&self, snapshot: KeeperLeaderLock) {
        *self.inner.lock().expect("HostMirrorLeaderPolicy lock poisoned") = snapshot;
    }

    /// Read-only borrow of the cached mirror (testing helper).
    pub fn snapshot(&self) -> KeeperLeaderLock {
        self.inner
            .lock()
            .expect("HostMirrorLeaderPolicy lock poisoned")
            .clone()
    }
}

impl LeaderPolicy for HostMirrorLeaderPolicy {
    fn should_submit(&self, current_slot: u64) -> bool {
        // We pessimistically lock + run try_heartbeat; the side-
        // effect on the mirror is the in-memory equivalent of the
        // chain-side ix the bot is about to send. If
        // `record_outcome` is later called with the chain-confirmed
        // result, that overrides the speculative state.
        let mut lock = self
            .inner
            .lock()
            .expect("HostMirrorLeaderPolicy lock poisoned");
        let outcome = lock.try_heartbeat(self.signer, current_slot);
        outcome.is_leader()
    }

    fn record_outcome(&self, outcome: HeartbeatOutcome) {
        // The wave-15 production loop calls this after the chain
        // confirms the heartbeat tx. We update the mirror to the
        // chain-authoritative state so future ticks decide off
        // the actual on-chain truth.
        let mut lock = self
            .inner
            .lock()
            .expect("HostMirrorLeaderPolicy lock poisoned");
        match outcome {
            HeartbeatOutcome::AcquiredFresh
            | HeartbeatOutcome::AcquiredFromStale { .. }
            | HeartbeatOutcome::Refreshed => {
                // Mirror already reflects the success; nothing more to do.
            }
            HeartbeatOutcome::RejectedHeldByOther { current_leader, .. } => {
                lock.has_leader = true;
                lock.current_leader = current_leader;
                // We don't know the chain's last_heartbeat_slot from
                // a rejection alone; production reconcile() will
                // overwrite this on the next RPC fetch.
            }
            HeartbeatOutcome::RejectedClockSkew { recorded_slot, .. } => {
                // Make sure the cached mirror reflects the recorded
                // slot at minimum.
                if lock.last_heartbeat_slot < recorded_slot {
                    lock.last_heartbeat_slot = recorded_slot;
                }
            }
        }
    }
}

/// Test-only policy that always returns the configured boolean.
/// Used by `keeper_bot::run::tests` to exercise the gate without
/// pulling in the full chain-mirror dependency.
pub struct FixedLeaderPolicy {
    submit: bool,
}

impl FixedLeaderPolicy {
    /// Always-leader policy.
    pub fn always_leader() -> Self {
        Self { submit: true }
    }

    /// Always-standby policy.
    pub fn always_standby() -> Self {
        Self { submit: false }
    }
}

impl LeaderPolicy for FixedLeaderPolicy {
    fn should_submit(&self, _current_slot: u64) -> bool {
        self.submit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ALICE: Pubkey32 = [0xa1; 32];
    const BOB: Pubkey32 = [0xb0; 32];
    const TAKEOVER: u64 = 75;

    /// Wave 15 — fresh-lock holder gets `should_submit = true`.
    #[test]
    fn host_mirror_leader_returns_true_for_holder() {
        let policy = HostMirrorLeaderPolicy::new(
            ALICE,
            KeeperLeaderLock::held_by(ALICE, 100, TAKEOVER),
        );
        assert!(policy.should_submit(110));
        // After try_heartbeat the mirror records slot 110.
        assert_eq!(policy.snapshot().last_heartbeat_slot, 110);
    }

    /// Wave 15 — fresh lock held by SOMEONE ELSE rejects this signer.
    #[test]
    fn host_mirror_leader_rejects_non_holder_when_fresh() {
        let policy = HostMirrorLeaderPolicy::new(
            BOB,
            KeeperLeaderLock::held_by(ALICE, 100, TAKEOVER),
        );
        assert!(!policy.should_submit(110));
    }

    /// Wave 15 — stale lock allows takeover; the mirror reflects the
    /// new holder.
    #[test]
    fn host_mirror_leader_takes_over_when_stale() {
        let policy = HostMirrorLeaderPolicy::new(
            BOB,
            KeeperLeaderLock::held_by(ALICE, 100, TAKEOVER),
        );
        assert!(policy.should_submit(100 + TAKEOVER + 1));
        assert_eq!(policy.snapshot().current_leader_pubkey(), Some(BOB));
    }

    /// Wave 15 — `record_outcome(RejectedHeldByOther)` syncs the
    /// mirror with the chain-confirmed leader.
    #[test]
    fn record_outcome_updates_mirror_on_rejection() {
        let policy = HostMirrorLeaderPolicy::new(
            ALICE,
            KeeperLeaderLock::fresh(0, TAKEOVER),
        );
        // First should_submit acquires fresh.
        assert!(policy.should_submit(50));
        // Now imagine chain reports BOB took over.
        policy.record_outcome(HeartbeatOutcome::RejectedHeldByOther {
            current_leader: BOB,
            slots_until_stale: 60,
        });
        let snap = policy.snapshot();
        assert_eq!(snap.current_leader_pubkey(), Some(BOB));
        assert!(snap.has_leader);
    }

    /// Wave 15 — `reconcile()` overrides the cached mirror.
    #[test]
    fn reconcile_replaces_mirror() {
        let policy = HostMirrorLeaderPolicy::new(
            ALICE,
            KeeperLeaderLock::fresh(0, TAKEOVER),
        );
        let chain = KeeperLeaderLock::held_by(BOB, 999, TAKEOVER);
        policy.reconcile(chain.clone());
        assert_eq!(policy.snapshot(), chain);
    }

    /// Wave 15 — `FixedLeaderPolicy` ignores `current_slot`.
    #[test]
    fn fixed_leader_policy_returns_configured_value() {
        let always = FixedLeaderPolicy::always_leader();
        assert!(always.should_submit(0));
        assert!(always.should_submit(u64::MAX));
        let standby = FixedLeaderPolicy::always_standby();
        assert!(!standby.should_submit(0));
        assert!(!standby.should_submit(u64::MAX));
    }
}
