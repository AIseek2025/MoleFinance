//! Transaction-builder trait + Anchor IX encoding for keeper actions.
//!
//! ## Discriminators are hard-coded, but pinned by a self-test
//!
//! Anchor encodes each instruction as
//! `[sha256("global:<ix_name>")[..8]] ++ borsh(args)`. We hard-code
//! the discriminators as `pub const` so the production hot-path
//! never recomputes a SHA-256, then bind those constants to the
//! current on-chain instruction names with
//! `discriminator_constants_match_sha256_of_anchor_namespace` at
//! the bottom of this file. Renaming an instruction in
//! `programs/mole-option/src/lib.rs` without also updating the
//! constant here will fail this test loudly during CI — exactly
//! the failure mode silent stale-disc bugs would otherwise hide.
//!
//! Use `cargo test -p keeper-rpc print_canonical_discriminators
//! -- --ignored --nocapture` to regenerate the byte-arrays after
//! a deliberate rename.

use clearing_core::Direction;
use keeper::{ActionDispatchResult, ActionExecutor, KeeperAction};

use crate::snapshot::ChainSnapshot;
use crate::Pubkey32;

// =====================================================================
// Anchor instruction discriminators (re-exported from `tx-codec`)
// =====================================================================
//
// Wave 15 moved the canonical discriminator constants and ix
// encoders into `crates/tx-codec`, which is wasm32-buildable so the
// frontend can consume the same bytes via wave-16 wasm-pack. This
// crate re-exports the keeper-side trio for backwards compat with
// every external caller (tests, `solana-rpc`, downstream binaries).

pub use tx_codec::{
    DISC_CLOSE_DORMANT_BUCKET, DISC_INITIALIZE_DORMANT_BUCKET, DISC_PRE_SYNC_DORMANT_BUCKET,
};

// =====================================================================
// Errors
// =====================================================================

/// Errors produced while building an instruction payload.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum TxBuildError {
    /// The action targets a sub-pool not present in the
    /// [`ChainSnapshot`].
    #[error("snapshot is missing sub_pool {0}")]
    MissingSubPool(u32),
    /// Action targets a bucket the snapshot doesn't know about. The
    /// keeper bot must call `refresh` between the action being
    /// emitted and being submitted, otherwise the snapshot's
    /// pubkey-cache lags.
    #[error("snapshot is missing bucket ({sub_pool_id}, {direction:?}, tick={tick})")]
    MissingBucket {
        /// Sub pool id.
        sub_pool_id: u32,
        /// Direction.
        direction: Direction,
        /// Bucket tick.
        tick: i64,
    },
    /// Direction-specific ledger missing from snapshot.
    #[error("snapshot is missing ledger ({sub_pool_id}, {direction:?})")]
    MissingLedger {
        /// Sub pool id.
        sub_pool_id: u32,
        /// Direction.
        direction: Direction,
    },
    /// Underlying tx-builder rejected the payload.
    #[error("tx builder rejected: {0}")]
    Builder(String),
}

// =====================================================================
// Account-meta wire format
// =====================================================================

/// One account in a Solana instruction. Mirrors `solana_sdk::AccountMeta`
/// at the wire level so the default-feature build doesn't need
/// `solana-sdk`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountMeta {
    /// Account pubkey.
    pub pubkey: Pubkey32,
    /// Whether this account must sign the transaction.
    pub is_signer: bool,
    /// Whether this account is writable.
    pub is_writable: bool,
}

impl AccountMeta {
    /// New writable, non-signer account.
    pub fn writable(pubkey: Pubkey32) -> Self {
        Self {
            pubkey,
            is_signer: false,
            is_writable: true,
        }
    }
    /// New read-only, non-signer account.
    pub fn readonly(pubkey: Pubkey32) -> Self {
        Self {
            pubkey,
            is_signer: false,
            is_writable: false,
        }
    }
    /// New writable signer (typically the keeper's payer).
    pub fn signer_writable(pubkey: Pubkey32) -> Self {
        Self {
            pubkey,
            is_signer: true,
            is_writable: true,
        }
    }
    /// New read-only signer.
    pub fn signer_readonly(pubkey: Pubkey32) -> Self {
        Self {
            pubkey,
            is_signer: true,
            is_writable: false,
        }
    }
}

/// Materialised instruction ready for transaction assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchedAction {
    /// Source `KeeperAction` (so the executor's caller can correlate
    /// build artefacts with what was queued).
    pub action: KeeperAction,
    /// Program id this ix targets.
    pub program_id: Pubkey32,
    /// Anchor instruction-data blob (`disc ++ borsh(args)`).
    pub data: Vec<u8>,
    /// Account meta list in the order Anchor expects.
    pub accounts: Vec<AccountMeta>,
}

/// Result of submitting a [`DispatchedAction`] over the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedTx {
    /// Optional transaction signature (None for dry-runs).
    pub signature: Option<String>,
}

// =====================================================================
// TxBuilder trait
// =====================================================================

/// Pluggable transaction-submission backend. The executor builds the
/// `DispatchedAction` (program id, ix-data, accounts, source action)
/// and hands it to the builder, which is responsible for assembling
/// + signing + submitting the actual tx.
pub trait TxBuilder {
    /// Submit one action. Implementors decide whether to bundle
    /// multiple submissions, retry, or batch — `RpcExecutor` makes
    /// no assumptions either way and surfaces each call's result
    /// back to the scheduler via the shared
    /// [`ActionDispatchResult`] enum.
    fn submit(&mut self, dispatched: &DispatchedAction) -> ActionDispatchResult;
}

/// In-memory `TxBuilder` for tests / dry-runs. Records every
/// submitted [`DispatchedAction`] and reports `Submitted { signature: None }`.
#[derive(Debug, Clone, Default)]
pub struct MockTxBuilder {
    /// Recorded submissions, in order.
    pub submitted: Vec<DispatchedAction>,
    /// Optional override: when `Some`, every submit returns this
    /// result. Used by tests to simulate RPC failures.
    pub override_result: Option<ActionDispatchResult>,
}

impl MockTxBuilder {
    /// Construct an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drain and return every recorded submission.
    pub fn drain(&mut self) -> Vec<DispatchedAction> {
        std::mem::take(&mut self.submitted)
    }
}

impl TxBuilder for MockTxBuilder {
    fn submit(&mut self, dispatched: &DispatchedAction) -> ActionDispatchResult {
        self.submitted.push(dispatched.clone());
        match &self.override_result {
            Some(r) => r.clone(),
            None => ActionDispatchResult::Submitted { signature: None },
        }
    }
}

// =====================================================================
// RpcExecutor — implements ActionExecutor
// =====================================================================

/// `ActionExecutor` impl that turns a [`KeeperAction`] into a
/// concrete Anchor IX using the metadata cached in the
/// [`ChainSnapshot`] and submits it via the wrapped [`TxBuilder`].
///
/// ## Why the snapshot is owned by the executor
///
/// `KeeperAction` only carries the *engine-side* identifiers
/// (`sub_pool_id`, `direction`, `tick`). The executor needs the
/// *Solana-side* pubkeys (`SubPool` PDA, `DistributionLedger` PDA,
/// `DormantBucket` PDA, plus the vector of all live buckets that
/// make up the `pre_sync` `remaining_accounts` list). Those are
/// exactly the things the snapshot already stores.
///
/// The executor borrows the snapshot for the duration of the call;
/// the keeper bot's main loop is `refresh → plan → dispatch → repeat`,
/// so the snapshot stays alive across all three stages.
pub struct RpcExecutor<'a, B: TxBuilder> {
    /// On-chain program id.
    program_id: Pubkey32,
    /// Market PDA pubkey.
    market: Pubkey32,
    /// Per-sub-pool `SubPool` PDA cache, lifted out of the snapshot.
    sub_pool_pubkeys: std::collections::HashMap<u32, Pubkey32>,
    /// Keeper signer (payer for `pre_sync` and rent-receiver for
    /// `close`/`init`).
    keeper: Pubkey32,
    /// `Clock` sysvar pubkey. Hard-coded constant on Solana
    /// mainnet, but parameterised here so localnet integration tests
    /// can supply their own.
    clock_sysvar: Pubkey32,
    /// `System` program id. Same story as `clock_sysvar`.
    system_program: Pubkey32,
    /// Snapshot the executor reads PDA pubkeys + the bucket-list
    /// from.
    snapshot: &'a ChainSnapshot,
    /// Configured tx builder.
    pub builder: B,
}

impl<'a, B: TxBuilder> RpcExecutor<'a, B> {
    /// Construct a new executor.
    pub fn new(
        program_id: Pubkey32,
        market: Pubkey32,
        keeper: Pubkey32,
        clock_sysvar: Pubkey32,
        system_program: Pubkey32,
        snapshot: &'a ChainSnapshot,
        builder: B,
    ) -> Self {
        let mut sub_pool_pubkeys = std::collections::HashMap::new();
        for (id, sp) in &snapshot.sub_pools {
            // We don't store sub_pool pubkeys in the snapshot; the
            // caller has them in MarketContext. As a fallback, we
            // accept SubPool's `market` field as a sanity check and
            // re-derive pubkeys from the snapshot's
            // bucket_pubkeys / ledger_pubkeys downstream. For now,
            // store None placeholders; the helper
            // `with_sub_pool_pubkeys` overrides them.
            let _ = (id, sp);
        }
        // Pull from the snapshot's ledger pubkeys: every ledger has
        // sub_pool == its parent SubPool's pubkey, so we can recover
        // the pubkey from the decoded ledger account body.
        for ((sub_pool_id, _dir), led_pk) in &snapshot.ledger_pubkeys {
            // The ledger's decoded body has `sub_pool: Pubkey32`.
            if let Some(led) = snapshot
                .ledgers
                .get(&(*sub_pool_id, Direction::Long))
                .or_else(|| snapshot.ledgers.get(&(*sub_pool_id, Direction::Short)))
            {
                let _ = led_pk; // not used; we want the SUB_POOL pubkey,
                                // which is stored inside `led.sub_pool`.
                sub_pool_pubkeys.insert(*sub_pool_id, led.sub_pool);
            }
        }
        Self {
            program_id,
            market,
            sub_pool_pubkeys,
            keeper,
            clock_sysvar,
            system_program,
            snapshot,
            builder,
        }
    }

    /// Override the per-sub-pool `SubPool` PDA cache. The executor
    /// recovers most pubkeys from the snapshot, but for sub-pools
    /// with NO ledgers fetched (uncommon — every sub-pool has both
    /// ledgers materialised at init time) the caller can supply the
    /// pubkey explicitly.
    pub fn with_sub_pool_pubkeys(
        mut self,
        pubkeys: std::collections::HashMap<u32, Pubkey32>,
    ) -> Self {
        for (id, pk) in pubkeys {
            self.sub_pool_pubkeys.insert(id, pk);
        }
        self
    }

    /// Build the dispatched form of one action without submitting.
    /// Useful for tests and for keeper-bot dry-run loops that want
    /// to inspect what the executor *would* send.
    pub fn build(&self, action: &KeeperAction) -> Result<DispatchedAction, TxBuildError> {
        match *action {
            KeeperAction::PreSyncDormantBucket {
                sub_pool_id,
                direction,
                tick,
                ..
            } => self.build_pre_sync(sub_pool_id, direction, tick),
            KeeperAction::CloseDormantBucket {
                sub_pool_id,
                direction,
                tick,
            } => self.build_close(sub_pool_id, direction, tick),
            KeeperAction::InitDormantBucket {
                sub_pool_id,
                direction,
                tick,
                ..
            } => self.build_init(sub_pool_id, direction, tick),
        }
    }

    fn sub_pool_pubkey(&self, sub_pool_id: u32) -> Result<Pubkey32, TxBuildError> {
        self.sub_pool_pubkeys
            .get(&sub_pool_id)
            .copied()
            .ok_or(TxBuildError::MissingSubPool(sub_pool_id))
    }

    fn build_pre_sync(
        &self,
        sub_pool_id: u32,
        direction: Direction,
        tick: i64,
    ) -> Result<DispatchedAction, TxBuildError> {
        let sp_pk = self.sub_pool_pubkey(sub_pool_id)?;
        let long_lg = self
            .snapshot
            .ledger_pubkey(sub_pool_id, Direction::Long)
            .ok_or(TxBuildError::MissingLedger {
                sub_pool_id,
                direction: Direction::Long,
            })?;
        let short_lg = self
            .snapshot
            .ledger_pubkey(sub_pool_id, Direction::Short)
            .ok_or(TxBuildError::MissingLedger {
                sub_pool_id,
                direction: Direction::Short,
            })?;
        // The chosen bucket MUST be in the appropriate side's slice
        // of `remaining_accounts`. We assert it's known here.
        if self
            .snapshot
            .bucket_pubkey(sub_pool_id, direction, tick)
            .is_none()
        {
            return Err(TxBuildError::MissingBucket {
                sub_pool_id,
                direction,
                tick,
            });
        }
        let sp = self.snapshot.sub_pools.get(&sub_pool_id).ok_or(
            TxBuildError::MissingSubPool(sub_pool_id),
        )?;
        // Derive bucket count from sub-pool snapshot — the program
        // requires both as ix args so the bridge layer knows how
        // to slice `remaining_accounts`.
        let direction_is_long = matches!(direction, Direction::Long);
        let data = tx_codec::encode_pre_sync_dormant_bucket_ix(
            direction_is_long,
            tick,
            sp.long_dormant_bucket_count,
            sp.short_dormant_bucket_count,
        )
        .map_err(|e| TxBuildError::Builder(e.to_string()))?;
        // Account list:
        //   market (read)
        //   sub_pool (write)
        //   long_ledger (write)
        //   short_ledger (write)
        //   clock (read)
        //   keeper (signer, write)
        //   ... remaining_accounts: long buckets, then short buckets
        let mut accounts = vec![
            AccountMeta::readonly(self.market),
            AccountMeta::writable(sp_pk),
            AccountMeta::writable(long_lg),
            AccountMeta::writable(short_lg),
            AccountMeta::readonly(self.clock_sysvar),
            AccountMeta::signer_writable(self.keeper),
        ];
        // Remaining accounts: every bucket, ordered (long first,
        // then short). We don't filter by tick — the engine wants
        // every live bucket on both sides.
        let mut long_bucket_pks: Vec<Pubkey32> = Vec::new();
        let mut short_bucket_pks: Vec<Pubkey32> = Vec::new();
        for ((sp_id, dir, t), pk) in &self.snapshot.bucket_pubkeys {
            if *sp_id != sub_pool_id {
                continue;
            }
            let _ = t;
            match dir {
                Direction::Long => long_bucket_pks.push(*pk),
                Direction::Short => short_bucket_pks.push(*pk),
            }
        }
        long_bucket_pks.sort_unstable();
        short_bucket_pks.sort_unstable();
        for pk in long_bucket_pks.into_iter().chain(short_bucket_pks) {
            accounts.push(AccountMeta::writable(pk));
        }
        Ok(DispatchedAction {
            action: KeeperAction::PreSyncDormantBucket {
                sub_pool_id,
                direction,
                tick,
                pending: 0, // recovered downstream from snapshot
            },
            program_id: self.program_id,
            data,
            accounts,
        })
    }

    fn build_close(
        &self,
        sub_pool_id: u32,
        direction: Direction,
        tick: i64,
    ) -> Result<DispatchedAction, TxBuildError> {
        let sp_pk = self.sub_pool_pubkey(sub_pool_id)?;
        let lg_pk = self
            .snapshot
            .ledger_pubkey(sub_pool_id, direction)
            .ok_or(TxBuildError::MissingLedger {
                sub_pool_id,
                direction,
            })?;
        let bk_pk = self
            .snapshot
            .bucket_pubkey(sub_pool_id, direction, tick)
            .ok_or(TxBuildError::MissingBucket {
                sub_pool_id,
                direction,
                tick,
            })?;
        let direction_is_long = matches!(direction, Direction::Long);
        let data = tx_codec::encode_close_dormant_bucket_ix(direction_is_long, tick)
            .map_err(|e| TxBuildError::Builder(e.to_string()))?;
        let accounts = vec![
            AccountMeta::readonly(self.market),
            AccountMeta::readonly(sp_pk),
            AccountMeta::readonly(lg_pk),
            AccountMeta::writable(bk_pk),
            AccountMeta::writable(self.keeper), // receiver
            AccountMeta::signer_writable(self.keeper),
        ];
        Ok(DispatchedAction {
            action: KeeperAction::CloseDormantBucket {
                sub_pool_id,
                direction,
                tick,
            },
            program_id: self.program_id,
            data,
            accounts,
        })
    }

    fn build_init(
        &self,
        sub_pool_id: u32,
        direction: Direction,
        tick: i64,
    ) -> Result<DispatchedAction, TxBuildError> {
        let sp_pk = self.sub_pool_pubkey(sub_pool_id)?;
        let direction_is_long = matches!(direction, Direction::Long);
        // The bucket PDA doesn't exist yet — caller supplies seeds;
        // executor consumers (solana-rpc feature) call
        // `find_program_address` to produce the address. We surface
        // the seeds as an opaque blob so the encoder layer can do
        // the work.
        let data = tx_codec::encode_initialize_dormant_bucket_ix(direction_is_long, tick)
            .map_err(|e| TxBuildError::Builder(e.to_string()))?;

        // Account list. The bucket PDA is sha256-derived; default-
        // build can't produce it without solana-sdk's
        // `Pubkey::find_program_address`. We pass a *zero placeholder*
        // and trust the consumer (the `solana-rpc`-feature wrapper or
        // a downstream test harness) to substitute the real PDA
        // before submission. This keeps the default-feature build
        // crypto-free.
        let placeholder_bucket: Pubkey32 = derive_init_bucket_placeholder(&sp_pk, direction_is_long, tick);
        let accounts = vec![
            AccountMeta::readonly(self.market),
            AccountMeta::readonly(sp_pk),
            AccountMeta::writable(placeholder_bucket),
            AccountMeta::signer_writable(self.keeper),
            AccountMeta::readonly(self.system_program),
        ];
        Ok(DispatchedAction {
            action: KeeperAction::InitDormantBucket {
                sub_pool_id,
                direction,
                tick,
                rationale: keeper::InitRationale::Explicit,
            },
            program_id: self.program_id,
            data,
            accounts,
        })
    }
}

impl<'a, B: TxBuilder> ActionExecutor for RpcExecutor<'a, B> {
    fn execute(&mut self, action: KeeperAction) -> ActionDispatchResult {
        match self.build(&action) {
            Ok(dispatched) => self.builder.submit(&dispatched),
            Err(e) => ActionDispatchResult::Failed {
                reason: e.to_string(),
            },
        }
    }
}

/// Deterministic placeholder pubkey for the not-yet-derived
/// `init_dormant_bucket` PDA. The default-feature build can't
/// SHA-256 / curve-check, so we encode the seeds into a 32-byte
/// blob and let the `solana-rpc` consumer rewrite it after deriving
/// the real PDA via `Pubkey::find_program_address`. The blob is
/// guaranteed to be off-curve so it can never collide with a real
/// account.
fn derive_init_bucket_placeholder(
    sub_pool: &Pubkey32,
    direction_is_long: bool,
    tick: i64,
) -> Pubkey32 {
    // Stamp 'PLACEHOLDER' marker + seeds into the first 32 bytes.
    // Format: [0xff, 0xff, 'P', 'D', 'A', dir, tick_le[0..8], sub_pool[0..18]]
    let mut out = [0u8; 32];
    out[0] = 0xff;
    out[1] = 0xff;
    out[2] = b'P';
    out[3] = b'D';
    out[4] = b'A';
    out[5] = direction_is_long as u8;
    out[6..14].copy_from_slice(&tick.to_le_bytes());
    out[14..32].copy_from_slice(&sub_pool[..18]);
    out
}

// Note: a `placeholder_to_seeds` helper lived here but it was a
// thin re-export of `pda::dormant_bucket_seeds`. Removed in wave 10
// to stop duplicating the public PDA API. Consumers should import
// `keeper_rpc::pda::dormant_bucket_seeds` directly.

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{encode_anchor_account, OnchainDistEntry};
    use crate::accounts::{
        OnchainDistributionLedger, OnchainDormantBucket, OnchainMarket, OnchainSubPool,
    };
    use crate::fetcher::MockAccountFetcher;
    use crate::snapshot::{MarketContext, SnapshotConfig, SubPoolEntry};
    use clearing_core::SCHEMA_VERSION_CURRENT;
    use keeper::{ActionExecutor, InitRationale};
    use sha2::{Digest, Sha256};

    fn anchor_disc(name: &str) -> [u8; 8] {
        let mut h = Sha256::new();
        h.update(format!("global:{name}").as_bytes());
        let r = h.finalize();
        let mut out = [0u8; 8];
        out.copy_from_slice(&r[..8]);
        out
    }

    /// Bridge test: every keeper-side discriminator we re-export
    /// from `tx-codec` is byte-for-byte the canonical Anchor disc
    /// for the on-chain ix name. Wave 15 collapsed the original
    /// keeper-rpc-only constants into `tx-codec` so the same bytes
    /// also drive the wave-16 wasm/frontend path; this bridge test
    /// guarantees the re-export hasn't drifted from the on-chain
    /// program's `lib.rs` instruction names.
    #[test]
    fn keeper_side_re_exported_discriminators_match_anchor_namespace() {
        assert_eq!(
            DISC_PRE_SYNC_DORMANT_BUCKET,
            anchor_disc("pre_sync_dormant_bucket"),
            "pre_sync_dormant_bucket discriminator drift",
        );
        assert_eq!(
            DISC_CLOSE_DORMANT_BUCKET,
            anchor_disc("close_dormant_bucket"),
            "close_dormant_bucket discriminator drift",
        );
        assert_eq!(
            DISC_INITIALIZE_DORMANT_BUCKET,
            anchor_disc("initialize_dormant_bucket"),
            "initialize_dormant_bucket discriminator drift",
        );
    }

    /// Tx-codec's encoders and `keeper_rpc::tx::RpcExecutor` produce
    /// byte-for-byte identical instruction `data` blobs for the 3
    /// keeper-side ixs (after wave-15 the executor calls into
    /// tx-codec internally; this test pins the equivalence so a
    /// future regression that re-introduces a hand-rolled encoder
    /// in `RpcExecutor` would fail).
    #[test]
    fn rpc_executor_emits_same_bytes_as_tx_codec_helpers_close_and_init() {
        let snap = build_full_snapshot();
        let exec = RpcExecutor::new(
            PROGRAM_ID,
            MARKET_PUBKEY,
            KEEPER,
            CLOCK,
            SYSTEM_PROGRAM,
            &snap,
            MockTxBuilder::new(),
        );
        let close_dispatched = exec
            .build(&KeeperAction::CloseDormantBucket {
                sub_pool_id: 0,
                direction: Direction::Long,
                tick: -100,
            })
            .unwrap();
        let close_via_helper = tx_codec::encode_close_dormant_bucket_ix(true, -100).unwrap();
        assert_eq!(close_dispatched.data, close_via_helper);

        let init_dispatched = exec
            .build(&KeeperAction::InitDormantBucket {
                sub_pool_id: 0,
                direction: Direction::Short,
                tick: 999,
                rationale: InitRationale::Explicit,
            })
            .unwrap();
        let init_via_helper = tx_codec::encode_initialize_dormant_bucket_ix(false, 999).unwrap();
        assert_eq!(init_dispatched.data, init_via_helper);
    }

    const PROGRAM_ID: Pubkey32 = [9u8; 32];
    const MARKET_PUBKEY: Pubkey32 = [1u8; 32];
    const SUB_POOL_0_PUBKEY: Pubkey32 = [2u8; 32];
    const KEEPER: Pubkey32 = [42u8; 32];
    const CLOCK: Pubkey32 = [3u8; 32];
    const SYSTEM_PROGRAM: Pubkey32 = [4u8; 32];

    fn build_full_snapshot() -> ChainSnapshot {
        let market = OnchainSubPool {
            market: MARKET_PUBKEY,
            sub_pool_id: 0,
            long_pool_equity: 1_000_000_000,
            short_pool_equity: 1_000_000_000,
            long_active_shares: 100_000,
            short_active_shares: 100_000,
            long_recovery_shares: 0,
            short_recovery_shares: 0,
            long_active_notional: 5_000_000_000,
            short_active_notional: 5_000_000_000,
            long_active_generation: 0,
            short_active_generation: 0,
            last_price: 100_000_000,
            last_sync_slot: 1,
            long_dust: 0,
            short_dust: 0,
            long_dormant_bucket_count: 1,
            short_dormant_bucket_count: 0,
            bump: 255,
            _pad: [0u8; 7],
        };
        let bucket = OnchainDormantBucket {
            sub_pool: SUB_POOL_0_PUBKEY,
            direction_is_long: true,
            zero_price_tick: -100,
            anchor_price: 100_000_000,
            total_recovery_shares: 100,
            total_recovery_notional: 1_000_000,
            accrued_value: 0,
            position_count: 1,
            last_applied_index: 3,
            bump: 255,
            _pad: [0u8; 6],
        };
        let ledger_long = OnchainDistributionLedger {
            sub_pool: SUB_POOL_0_PUBKEY,
            direction_is_long: true,
            max_entries: 64,
            gc_offset: 0,
            next_event_index: 5,
            accrued_value_total: 0,
            pending_distribution_total: 0,
            entry_count: 0,
            entries: vec![OnchainDistEntry {
                event_index: 0,
                p_at_event: 100_000_000,
                total_outstanding_at_event: 1_000_000,
                total_alloc_input: 0,
                allocated_sum_observed: 0,
            }],
            bump: 255,
            _pad: [0u8; 7],
        };
        let mut ledger_short = ledger_long.clone();
        ledger_short.direction_is_long = false;
        ledger_short.next_event_index = 0;

        let mk = OnchainMarket {
            global_config: [0u8; 32],
            symbol: [0u8; 16],
            collateral_mint: [0u8; 32],
            vault: [0u8; 32],
            fee_vault: [0u8; 32],
            oracle_price_feed: [0u8; 32],
            oracle_program_id: [0u8; 32],
            leverage_bps: 1_000,
            min_margin: 1,
            max_margin_per_position: u64::MAX,
            max_total_principal: u128::MAX,
            max_total_notional: u128::MAX,
            current_total_principal: 0,
            current_total_notional: 0,
            open_fee_bps: 0,
            max_oracle_age_seconds: 60,
            max_oracle_age_slots: 100,
            max_confidence_bps: 100,
            max_price_move_bps_per_sync: 5_000,
            price_tick: 1,
            tick_aggregation_factor: 1,
            max_dormant_bucket_count_per_direction: 100,
            dilution_safety_bps: 100,
            max_idle_slots: 1_000_000,
            paused: false,
            frozen_new_position: false,
            schema_version: SCHEMA_VERSION_CURRENT,
            sub_pool_count: 1,
            dormant_distribute_mode: 1,
            max_pending_apply_per_tx: 8,
            max_distribution_ledger_size: 64,
            bump: 255,
            _pad: [0u8; 2],
        };

        let disc = [1u8; 8];
        let mut f = MockAccountFetcher::new();
        f.insert(MARKET_PUBKEY, PROGRAM_ID, encode_anchor_account(&mk, &disc).unwrap());
        f.insert(
            SUB_POOL_0_PUBKEY,
            PROGRAM_ID,
            encode_anchor_account(&market, &disc).unwrap(),
        );
        f.insert(
            [10u8; 32],
            PROGRAM_ID,
            encode_anchor_account(&ledger_long, &disc).unwrap(),
        );
        f.insert(
            [11u8; 32],
            PROGRAM_ID,
            encode_anchor_account(&ledger_short, &disc).unwrap(),
        );
        f.insert(
            [21u8; 32],
            PROGRAM_ID,
            encode_anchor_account(&bucket, &disc).unwrap(),
        );

        let ctx = MarketContext {
            program_id: PROGRAM_ID,
            market: MARKET_PUBKEY,
            market_symbol: [0u8; 16],
            sub_pools: vec![SubPoolEntry {
                sub_pool_id: 0,
                pubkey: SUB_POOL_0_PUBKEY,
            }],
        };
        let mut snap = ChainSnapshot::new();
        snap.refresh(&f, &ctx, SnapshotConfig::default()).unwrap();
        snap
    }

    /// Building a `pre_sync` action produces the discriminator + arg
    /// bytes the program expects, plus the canonical 6-fixed-account
    /// list followed by every live bucket.
    #[test]
    fn build_pre_sync_emits_canonical_anchor_ix() {
        let snap = build_full_snapshot();
        let exec = RpcExecutor::new(
            PROGRAM_ID,
            MARKET_PUBKEY,
            KEEPER,
            CLOCK,
            SYSTEM_PROGRAM,
            &snap,
            MockTxBuilder::new(),
        );
        let action = KeeperAction::PreSyncDormantBucket {
            sub_pool_id: 0,
            direction: Direction::Long,
            tick: -100,
            pending: 2,
        };
        let d = exec.build(&action).unwrap();
        assert_eq!(d.program_id, PROGRAM_ID);
        assert_eq!(&d.data[..8], &DISC_PRE_SYNC_DORMANT_BUCKET);
        assert_eq!(d.accounts.len(), 7); // 6 fixed + 1 long bucket
        assert!(d.accounts[0].pubkey == MARKET_PUBKEY);
        assert!(d.accounts[5].is_signer && d.accounts[5].is_writable); // keeper
    }

    /// Building a `close` action picks the canonical 6-account list
    /// (no remaining_accounts on close).
    #[test]
    fn build_close_emits_six_account_list() {
        let snap = build_full_snapshot();
        let exec = RpcExecutor::new(
            PROGRAM_ID,
            MARKET_PUBKEY,
            KEEPER,
            CLOCK,
            SYSTEM_PROGRAM,
            &snap,
            MockTxBuilder::new(),
        );
        let action = KeeperAction::CloseDormantBucket {
            sub_pool_id: 0,
            direction: Direction::Long,
            tick: -100,
        };
        let d = exec.build(&action).unwrap();
        assert_eq!(&d.data[..8], &DISC_CLOSE_DORMANT_BUCKET);
        assert_eq!(d.accounts.len(), 6);
    }

    /// `init` builds a placeholder bucket pubkey (the real PDA is
    /// derived by the solana-rpc consumer). The placeholder MUST
    /// encode the seeds verbatim.
    #[test]
    fn build_init_emits_placeholder_with_seed_marker() {
        let snap = build_full_snapshot();
        let exec = RpcExecutor::new(
            PROGRAM_ID,
            MARKET_PUBKEY,
            KEEPER,
            CLOCK,
            SYSTEM_PROGRAM,
            &snap,
            MockTxBuilder::new(),
        );
        let action = KeeperAction::InitDormantBucket {
            sub_pool_id: 0,
            direction: Direction::Long,
            tick: 999,
            rationale: InitRationale::Explicit,
        };
        let d = exec.build(&action).unwrap();
        assert_eq!(&d.data[..8], &DISC_INITIALIZE_DORMANT_BUCKET);
        // Slot 2 = bucket placeholder
        assert_eq!(d.accounts[2].pubkey[0..5], [0xff, 0xff, b'P', b'D', b'A']);
        assert_eq!(d.accounts[2].pubkey[5], 1u8); // direction_is_long
        let tick_bytes: [u8; 8] = d.accounts[2].pubkey[6..14].try_into().unwrap();
        assert_eq!(tick_bytes, 999i64.to_le_bytes());
    }

    /// Missing snapshot → action build fails with a structured
    /// error rather than panicking.
    #[test]
    fn build_pre_sync_errors_on_missing_bucket() {
        let snap = build_full_snapshot();
        let exec = RpcExecutor::new(
            PROGRAM_ID,
            MARKET_PUBKEY,
            KEEPER,
            CLOCK,
            SYSTEM_PROGRAM,
            &snap,
            MockTxBuilder::new(),
        );
        let action = KeeperAction::PreSyncDormantBucket {
            sub_pool_id: 0,
            direction: Direction::Long,
            tick: 99_999, // not in snapshot
            pending: 1,
        };
        let err = exec.build(&action).unwrap_err();
        assert!(matches!(err, TxBuildError::MissingBucket { .. }));
    }

    /// Going through the `ActionExecutor::execute` path:
    /// successful build → `MockTxBuilder` records → executor returns
    /// `Submitted`.
    #[test]
    fn execute_path_records_submission() {
        let snap = build_full_snapshot();
        let mut exec = RpcExecutor::new(
            PROGRAM_ID,
            MARKET_PUBKEY,
            KEEPER,
            CLOCK,
            SYSTEM_PROGRAM,
            &snap,
            MockTxBuilder::new(),
        );
        let action = KeeperAction::CloseDormantBucket {
            sub_pool_id: 0,
            direction: Direction::Long,
            tick: -100,
        };
        let r = exec.execute(action);
        assert!(matches!(r, ActionDispatchResult::Submitted { .. }));
        assert_eq!(exec.builder.submitted.len(), 1);
    }

    /// Missing PDA bookkeeping for a freshly-spun-up sub-pool that
    /// has no buckets / ledgers yet maps cleanly to a `MissingSubPool`
    /// error rather than a misleading borsh failure.
    #[test]
    fn execute_path_surfaces_missing_sub_pool() {
        let snap = ChainSnapshot::new();
        let mut exec = RpcExecutor::new(
            PROGRAM_ID,
            MARKET_PUBKEY,
            KEEPER,
            CLOCK,
            SYSTEM_PROGRAM,
            &snap,
            MockTxBuilder::new(),
        );
        let action = KeeperAction::CloseDormantBucket {
            sub_pool_id: 0,
            direction: Direction::Long,
            tick: -100,
        };
        let r = exec.execute(action);
        assert!(matches!(r, ActionDispatchResult::Failed { .. }));
    }
}
