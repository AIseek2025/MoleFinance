//! Wave 16 — keeper-leader instruction tx builders.
//!
//! Wave 15 added the on-chain `KeeperLeaderLock` PDA + 4 instruction
//! handlers (`initialize / acquire / heartbeat / release`) and the
//! pure-Rust encoders that produce the byte-exact instruction body
//! (`keeper_decoder::ix::encode_keeper_leader_*`). Wave 16 wraps
//! those encoders with the Solana account-meta layout the bot needs
//! to actually submit the transaction over RPC.
//!
//! ## Why this lives separate from `RpcExecutor`
//!
//! `RpcExecutor` is action-driven — it consumes a `KeeperAction`
//! produced by `Scheduler::plan` and dispatches it. The
//! keeper-leader ix are **not** scheduler actions; they're the
//! gate that decides whether the action queue runs at all. Mixing
//! them into `KeeperAction` would muddy the wave-9 invariant that
//! `Scheduler::plan` only emits state-changing protocol actions.
//!
//! Instead the bot's run-loop calls these helpers directly:
//!
//! ```ignore
//! let (program_id, ix_data, accounts) = leader_tx::build_keeper_leader_heartbeat(
//!     program_id, market, lock_pda, keeper, KeeperLeaderHeartbeatArgs { observed_slot },
//! );
//! let sig = builder.submit_keeper_leader(program_id, ix_data, accounts)?;
//! ```
//!
//! The default `submit_keeper_leader` impl on `TxBuilder` errors
//! out so existing wave-12 / 13 / 14 builders that only handle
//! keeper actions surface the missing wiring loudly. Wave-16
//! production deployments override it (or use the
//! `MockKeeperLeaderTxBuilder` for host tests).

use keeper_decoder::ix::{
    account_discriminator, encode_keeper_leader_acquire, encode_keeper_leader_heartbeat,
    encode_keeper_leader_release, KeeperLeaderAcquireArgs, KeeperLeaderHeartbeatArgs,
    KeeperLeaderReleaseArgs,
};
use keeper_decoder::leader_lock::KeeperLeaderLock;
use keeper_decoder::{decode_anchor_account_with_discriminator, AccountDecodeError};

use crate::fetcher::AccountFetcher;
use crate::tx::AccountMeta;
use crate::{Pubkey32, RpcError};

/// Materialised keeper-leader instruction shape — what the wallet
/// (or the production tx-builder) signs and submits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderInstruction {
    /// Program id of `mole-option`.
    pub program_id: Pubkey32,
    /// Anchor instruction-data blob (`disc ++ borsh(args)`).
    pub data: Vec<u8>,
    /// Account meta list, in the order the Anchor handler expects.
    pub accounts: Vec<AccountMeta>,
}

/// Wave 16 — build the `keeper_leader_heartbeat` instruction.
///
/// Account order matches `programs/mole-option::KeeperLeaderHeartbeat`:
///
/// 1. `market` (read)
/// 2. `keeper_leader_lock` (write — PDA at `keeper_leader_lock_seeds(market)`)
/// 3. `keeper` (signer, write — pays compute)
///
/// Production callers compute the lock PDA via
/// `Pubkey::find_program_address(&keeper_leader_lock_seeds(market).as_refs(), program_id)`
/// (under `--features solana-rpc`) or hard-code the deterministic
/// PDA at deploy time for offline construction.
pub fn build_keeper_leader_heartbeat(
    program_id: Pubkey32,
    market: Pubkey32,
    lock_pda: Pubkey32,
    keeper: Pubkey32,
    args: KeeperLeaderHeartbeatArgs,
) -> LeaderInstruction {
    let data = encode_keeper_leader_heartbeat(&args);
    let accounts = vec![
        AccountMeta::readonly(market),
        AccountMeta::writable(lock_pda),
        AccountMeta::signer_writable(keeper),
    ];
    LeaderInstruction {
        program_id,
        data,
        accounts,
    }
}

/// Wave 16 — build the `keeper_leader_acquire` instruction.
///
/// Same account layout as `keeper_leader_heartbeat`. The only
/// difference is the discriminator + the chain-side check that the
/// lock is currently *stale* (rejects with `KeeperLeaderAcquireWhileFresh`
/// otherwise — that's `keeper_leader_heartbeat`'s job).
pub fn build_keeper_leader_acquire(
    program_id: Pubkey32,
    market: Pubkey32,
    lock_pda: Pubkey32,
    keeper: Pubkey32,
    args: KeeperLeaderAcquireArgs,
) -> LeaderInstruction {
    let data = encode_keeper_leader_acquire(&args);
    let accounts = vec![
        AccountMeta::readonly(market),
        AccountMeta::writable(lock_pda),
        AccountMeta::signer_writable(keeper),
    ];
    LeaderInstruction {
        program_id,
        data,
        accounts,
    }
}

/// Wave 16 — build the `keeper_leader_release` instruction.
///
/// Account layout matches `KeeperLeaderRelease`: `market` (read) +
/// `keeper_leader_lock` (write) + `keeper` (signer, write). The
/// chain-side handler additionally enforces that
/// `keeper.key() == lock.current_leader` (rejects with
/// `KeeperLeaderNotHolder` otherwise) — host code doesn't pre-validate
/// because the tx is cheap to bounce and the gas cost is on the
/// caller.
pub fn build_keeper_leader_release(
    program_id: Pubkey32,
    market: Pubkey32,
    lock_pda: Pubkey32,
    keeper: Pubkey32,
) -> LeaderInstruction {
    let data = encode_keeper_leader_release(&KeeperLeaderReleaseArgs {});
    let accounts = vec![
        AccountMeta::readonly(market),
        AccountMeta::writable(lock_pda),
        AccountMeta::signer_writable(keeper),
    ];
    LeaderInstruction {
        program_id,
        data,
        accounts,
    }
}

/// Re-export the wave-15 PDA seed helper so callers don't need a
/// `keeper-decoder` dependency just for the seed bytes.
pub use crate::pda::keeper_leader_lock_seeds as leader_lock_seeds;

/// Wave 16 — pluggable keeper-leader tx submission backend.
///
/// `TxBuilder` is action-driven (`KeeperAction → DispatchedAction →
/// submit`); keeper-leader ix are non-actions, so this trait is the
/// dedicated seam. Production deployments enable the
/// `solana-rpc` feature which provides `SolanaTxBuilder` impl;
/// host tests use [`MockKeeperLeaderTxBuilder`] for parity.
pub trait KeeperLeaderTxBuilder {
    /// Submit one keeper-leader instruction. Returns the optional
    /// signature on success (None for dry-runs / mocks). On failure
    /// the implementor stringifies the underlying RPC error so
    /// upstream surfaces stay backend-agnostic.
    fn submit_leader_ix(
        &mut self,
        instruction: LeaderInstruction,
    ) -> Result<Option<String>, String>;
}

/// In-memory `KeeperLeaderTxBuilder` for host tests.
#[derive(Debug, Clone, Default)]
pub struct MockKeeperLeaderTxBuilder {
    /// Recorded submissions, in order.
    pub submitted: Vec<LeaderInstruction>,
    /// When `Some`, every `submit_leader_ix` returns
    /// `Err(force_err.clone())`. Lets tests simulate RPC failures.
    pub force_err: Option<String>,
}

impl MockKeeperLeaderTxBuilder {
    /// Construct an empty mock.
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeeperLeaderTxBuilder for MockKeeperLeaderTxBuilder {
    fn submit_leader_ix(
        &mut self,
        instruction: LeaderInstruction,
    ) -> Result<Option<String>, String> {
        if let Some(err) = &self.force_err {
            return Err(err.clone());
        }
        self.submitted.push(instruction);
        Ok(None)
    }
}

// =====================================================================
// RPC reconcile helpers (wave 16)
// =====================================================================

/// Errors from [`fetch_keeper_leader_lock`].
#[derive(Debug, thiserror::Error)]
pub enum LeaderReconcileError {
    /// Underlying RPC fetch failed (timeout / 5xx / etc.).
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// PDA does not exist on chain. Wave-15
    /// `initialize_keeper_leader_lock` is permissionless; if the bot
    /// hits this, ops needs to send the init tx (see
    /// `Docs/Planning/24-operator-runbook.md` § keeper-leader).
    #[error("keeper-leader-lock PDA {0:?} not found on chain")]
    NotFound(Pubkey32),
    /// On-chain account decoded with the wrong discriminator
    /// (someone wrote an unrelated account at this PDA — should be
    /// impossible if the program's `init` invariant holds, but we
    /// surface it instead of silently mis-decoding).
    #[error("keeper-leader-lock account decode failed: {0}")]
    Decode(#[from] AccountDecodeError),
}

/// Wave 16 — fetch the on-chain `KeeperLeaderLock` PDA via
/// `fetcher` and decode it into the host state-machine type. Validates
/// the Anchor account discriminator (`account:KeeperLeaderLock`) so
/// a future schema rename surfaces loudly instead of silently
/// mis-decoding.
///
/// Production wiring calls this every N ticks before the leader
/// gate runs, then `policy.reconcile(snapshot)`s the cached mirror.
pub fn fetch_keeper_leader_lock<F: AccountFetcher>(
    fetcher: &F,
    lock_pda: &Pubkey32,
) -> Result<KeeperLeaderLock, LeaderReconcileError> {
    let raw = fetcher
        .fetch_account(lock_pda)?
        .ok_or(LeaderReconcileError::NotFound(*lock_pda))?;
    let disc = account_discriminator("KeeperLeaderLock");
    let decoded =
        decode_anchor_account_with_discriminator::<KeeperLeaderLock>(&raw, &disc)?;
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetcher::MockAccountFetcher;
    use keeper_decoder::ix::instruction_discriminator;
    use keeper_decoder::leader_lock::encode_keeper_leader_lock_account;

    const PROGRAM_ID: Pubkey32 = [9u8; 32];
    const MARKET: Pubkey32 = [1u8; 32];
    const LOCK: Pubkey32 = [2u8; 32];
    const KEEPER_PK: Pubkey32 = [3u8; 32];

    /// Wave 16 — `build_keeper_leader_heartbeat` produces a 16-byte
    /// instruction body (8 disc + 8 observed_slot) and the right
    /// 3-account meta layout (market read / lock write / keeper
    /// signer-write).
    #[test]
    fn heartbeat_builder_layout_is_pinned() {
        let ix = build_keeper_leader_heartbeat(
            PROGRAM_ID,
            MARKET,
            LOCK,
            KEEPER_PK,
            KeeperLeaderHeartbeatArgs {
                observed_slot: 12_345,
            },
        );
        assert_eq!(ix.program_id, PROGRAM_ID);
        assert_eq!(ix.data.len(), 16);
        assert_eq!(
            &ix.data[..8],
            &instruction_discriminator("keeper_leader_heartbeat")
        );
        assert_eq!(ix.accounts.len(), 3);
        assert_eq!(ix.accounts[0].pubkey, MARKET);
        assert!(!ix.accounts[0].is_writable && !ix.accounts[0].is_signer);
        assert_eq!(ix.accounts[1].pubkey, LOCK);
        assert!(ix.accounts[1].is_writable && !ix.accounts[1].is_signer);
        assert_eq!(ix.accounts[2].pubkey, KEEPER_PK);
        assert!(ix.accounts[2].is_writable && ix.accounts[2].is_signer);
    }

    /// Wave 16 — `build_keeper_leader_acquire` shares heartbeat's
    /// account layout but uses the acquire discriminator.
    #[test]
    fn acquire_builder_uses_acquire_discriminator() {
        let ix = build_keeper_leader_acquire(
            PROGRAM_ID,
            MARKET,
            LOCK,
            KEEPER_PK,
            KeeperLeaderAcquireArgs {
                observed_slot: 99,
            },
        );
        assert_eq!(
            &ix.data[..8],
            &instruction_discriminator("keeper_leader_acquire")
        );
        assert_eq!(ix.data.len(), 16);
        assert_eq!(ix.accounts.len(), 3);
    }

    /// Wave 16 — `build_keeper_leader_release` is an 8-byte ix body
    /// (just the discriminator; no args).
    #[test]
    fn release_builder_emits_eight_byte_payload() {
        let ix = build_keeper_leader_release(PROGRAM_ID, MARKET, LOCK, KEEPER_PK);
        assert_eq!(ix.data.len(), 8);
        assert_eq!(
            &ix.data[..8],
            &instruction_discriminator("keeper_leader_release")
        );
        assert_eq!(ix.accounts.len(), 3);
    }

    /// Wave 16 — the PDA seed helper round-trips through the keeper-rpc
    /// re-export and the underlying `pda` module.
    #[test]
    fn seed_helper_re_export_resolves_to_canonical_seeds() {
        let seeds = leader_lock_seeds(&MARKET);
        assert_eq!(seeds.segments.len(), 2);
        assert_eq!(seeds.segments[0], b"keeper_leader_lock");
        assert_eq!(seeds.segments[1], MARKET.to_vec());
    }

    /// Wave 16 — happy path on `MockKeeperLeaderTxBuilder`. Records
    /// the ix shape and reports a dry-run signature.
    #[test]
    fn mock_builder_records_submissions_and_reports_dry_run_signature() {
        let mut builder = MockKeeperLeaderTxBuilder::new();
        let ix = build_keeper_leader_release(PROGRAM_ID, MARKET, LOCK, KEEPER_PK);
        let sig = builder.submit_leader_ix(ix.clone()).unwrap();
        assert_eq!(sig, None);
        assert_eq!(builder.submitted.len(), 1);
        assert_eq!(builder.submitted[0], ix);
    }

    /// Wave 16 — `force_err` lets tests simulate transient RPC
    /// failures without touching the real network. The recorded
    /// submission list stays empty when an error is forced.
    #[test]
    fn mock_builder_force_err_short_circuits_submission() {
        let mut builder = MockKeeperLeaderTxBuilder {
            force_err: Some("simulated rpc 503".into()),
            ..Default::default()
        };
        let ix = build_keeper_leader_release(PROGRAM_ID, MARKET, LOCK, KEEPER_PK);
        let err = builder.submit_leader_ix(ix).unwrap_err();
        assert_eq!(err, "simulated rpc 503");
        assert_eq!(builder.submitted.len(), 0);
    }

    /// Wave 16 — `fetch_keeper_leader_lock` happy path: account
    /// present + valid discriminator + valid Borsh body decodes
    /// back to the input lock state.
    #[test]
    fn fetch_lock_round_trips_through_mock_fetcher() {
        let lock = KeeperLeaderLock::held_by(KEEPER_PK, 100, 75);
        let disc = account_discriminator("KeeperLeaderLock");
        let raw = encode_keeper_leader_lock_account(&lock, &disc);
        let mut f = MockAccountFetcher::new();
        f.insert(LOCK, PROGRAM_ID, raw);
        let decoded = fetch_keeper_leader_lock(&f, &LOCK).expect("decoded lock");
        assert_eq!(decoded, lock);
    }

    /// Wave 16 — missing PDA surfaces `NotFound`, not silent zero.
    /// Ops needs to know to send the init tx.
    #[test]
    fn fetch_lock_reports_not_found_when_pda_missing() {
        let f = MockAccountFetcher::new();
        let err = fetch_keeper_leader_lock(&f, &LOCK).unwrap_err();
        assert!(matches!(err, LeaderReconcileError::NotFound(_)));
    }

    /// Wave 16 — wrong discriminator (someone wrote an unrelated
    /// account at the PDA) surfaces `Decode`, never silently
    /// returning a fabricated `KeeperLeaderLock`.
    #[test]
    fn fetch_lock_rejects_wrong_discriminator() {
        let lock = KeeperLeaderLock::held_by(KEEPER_PK, 100, 75);
        let raw = encode_keeper_leader_lock_account(&lock, &[0xff; 8]);
        let mut f = MockAccountFetcher::new();
        f.insert(LOCK, PROGRAM_ID, raw);
        let err = fetch_keeper_leader_lock(&f, &LOCK).unwrap_err();
        assert!(matches!(err, LeaderReconcileError::Decode(_)));
    }
}
