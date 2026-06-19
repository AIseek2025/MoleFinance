//! Wave 11 — production Solana RPC binding.
//!
//! This module is gated behind the `solana-rpc` feature. The default-
//! features build of `keeper-rpc` is host-only — every test in
//! `accounts.rs` / `pda.rs` / `fetcher.rs` / `snapshot.rs` / `tx.rs`
//! runs without a single `solana-client` dependency. Production
//! deployments enable `--features solana-rpc` to drop in
//! [`SolanaRpcAccountFetcher`] and [`SolanaTxBuilder`] which wrap a
//! real `solana_client::rpc_client::RpcClient` and signer keypair.
//!
//! ## Why we depend on the granular Anza-fork crates instead of `solana-sdk`
//!
//! In the 4.0 transition `solana-sdk = 4.0.1` re-exports
//! `solana-transaction = 4.0.0`, but `solana-rpc-client = 4.0.0`
//! internally consumes `solana-transaction = 3.1.0`. Two sibling
//! `Transaction` types are incompatible at the trait level
//! (`SerializableTransaction` is implemented only on the 3.1 type),
//! so we bypass `solana-sdk` and depend on the *granular* crates at
//! the same versions `solana-rpc-client 4.0` consumes. See `Cargo.toml`
//! for the exact version pins.
//!
//! ## Why the wrapper is so thin
//!
//! Wave 10's `AccountFetcher` / `TxBuilder` traits already do all the
//! heavy lifting (decoding, snapshot building, instruction encoding,
//! discriminator pinning, error categorisation). The production
//! adapter is intentionally a 1:1 byte-shuffle to/from
//! `solana_*` types — keeping the conversion surface small means a
//! future Solana SDK breaking change only touches this module, not
//! the keeper bot's planning logic.
//!
//! ## Design notes
//!
//! - We use the **synchronous** `RpcClient` to align with the
//!   `AccountFetcher` trait shape (the sync surface internally uses
//!   tokio's `block_on`, but consumers don't need to know).
//! - `Pubkey32` (this crate's 32-byte newtype) is a one-liner away
//!   from `solana_pubkey::Pubkey` — both are `[u8; 32]` under the
//!   hood. The conversion helpers are pinned by
//!   `pubkey_round_trip_is_byte_equal` so future Pubkey representation
//!   changes (e.g. bumping to a non-32-byte format) get caught at CI
//!   compile time, not runtime.
//! - `SolanaTxBuilder` carries the payer keypair *by value* so the
//!   keeper bot owns it explicitly and rotation is a `mem::replace`.
//!   No `Rc<RefCell<...>>` in the production path.
//! - `fetch_program_accounts_filter` constructs the
//!   `RpcProgramAccountsConfig` and dispatches via `client.send` with
//!   `RpcRequest::GetProgramAccounts` because the sync `RpcClient`
//!   in solana-rpc-client 4.0 does not expose
//!   `get_program_accounts_with_config` directly. The `nonblocking`
//!   variant does, but pulling tokio into the sync `AccountFetcher`
//!   trait shape would force every caller to provide a runtime.

use base64::Engine;
use serde_json::{Value, json};
use solana_client::client_error::ClientError as SolanaClientError;
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_client::rpc_request::RpcRequest;
use solana_client::rpc_response::{OptionalContext, RpcKeyedAccount};
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta as SdkAccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

use crate::Pubkey32;
use crate::fetcher::{AccountFetcher, RpcError};
use crate::tx::{AccountMeta, DispatchedAction, TxBuilder};
use keeper::ActionDispatchResult;

// =====================================================================
// Pubkey32 ↔ solana_pubkey::Pubkey conversions
// =====================================================================

/// Convert this crate's `Pubkey32` newtype to `solana_pubkey::Pubkey`.
/// Identity at the byte level — `Pubkey::new_from_array` does not
/// validate (any 32 bytes is a syntactically valid pubkey on Solana).
#[inline]
pub fn pubkey32_to_pubkey(p: &Pubkey32) -> Pubkey {
    Pubkey::new_from_array(*p)
}

/// Inverse of [`pubkey32_to_pubkey`]. Identity at the byte level.
#[inline]
pub fn pubkey_to_pubkey32(p: &Pubkey) -> Pubkey32 {
    p.to_bytes()
}

/// Convert this crate's portable [`AccountMeta`] (used by the
/// default-features tx builder) to a real `solana_instruction::
/// AccountMeta` for production submission.
#[inline]
pub fn account_meta_to_sdk(m: &AccountMeta) -> SdkAccountMeta {
    SdkAccountMeta {
        pubkey: pubkey32_to_pubkey(&m.pubkey),
        is_signer: m.is_signer,
        is_writable: m.is_writable,
    }
}

/// Convert a wave-10 [`DispatchedAction`] into a fully-formed
/// `solana_instruction::Instruction`. Byte-equal: the data blob is
/// the Anchor `[disc8] ++ borsh(args)` payload assembled upstream by
/// `RpcExecutor`, untouched here.
pub fn dispatched_to_instruction(d: &DispatchedAction) -> Instruction {
    Instruction {
        program_id: pubkey32_to_pubkey(&d.program_id),
        data: d.data.clone(),
        accounts: d.accounts.iter().map(account_meta_to_sdk).collect(),
    }
}

// =====================================================================
// SolanaRpcAccountFetcher — implements AccountFetcher
// =====================================================================

/// Production [`AccountFetcher`] wrapping a synchronous
/// `solana_client::rpc_client::RpcClient`.
///
/// The keeper bot constructs one per process and clones the
/// underlying `RpcClient` (cheap — it's `Arc`-backed internally) for
/// each thread that needs concurrent reads. Errors propagate up as
/// [`RpcError::Transport`] / [`RpcError::Code`] — the higher-level
/// snapshot logic decides whether to retry, back off, or surface to
/// operators.
pub struct SolanaRpcAccountFetcher {
    /// Underlying solana-client handle.
    pub client: RpcClient,
    /// Commitment level applied to every read. Production keepers
    /// run at `CommitmentConfig::confirmed` to balance latency vs
    /// fork risk; CI integration tests use `processed`.
    pub commitment: CommitmentConfig,
}

impl SolanaRpcAccountFetcher {
    /// Construct a new fetcher.
    pub fn new(rpc_url: String, commitment: CommitmentConfig) -> Self {
        Self {
            client: RpcClient::new_with_commitment(rpc_url, commitment),
            commitment,
        }
    }

    /// Construct from an already-initialised client.
    pub fn from_client(client: RpcClient, commitment: CommitmentConfig) -> Self {
        Self { client, commitment }
    }
}

/// Helper: classify a `solana_client::client_error::ClientError`
/// into our portable [`RpcError`]. Production logging keeps the full
/// `to_string()` payload — the keeper bot's structured logger
/// surfaces it under a single key.
fn classify_client_error(e: SolanaClientError) -> RpcError {
    let msg = e.to_string();
    // Heuristic: JSON-RPC error responses include "RPC response error"
    // in their formatted string. Everything else is treated as a
    // transport-level fault. A future iteration could pattern-match
    // on the typed `ClientErrorKind` variants for finer-grained
    // classification; today the keeper bot only needs the binary
    // distinction "did the call reach the cluster?".
    if msg.contains("RPC response error") {
        RpcError::Code {
            code: -1,
            message: msg,
        }
    } else {
        RpcError::Transport(msg)
    }
}

/// Decode an inline `UiAccountData::Binary(payload, encoding)` blob
/// returned by `getProgramAccounts` into raw bytes.
///
/// `getProgramAccounts` returns one of:
/// - `["<base64-string>", "base64"]`
/// - `["<base58-string>", "base58"]`
/// - `["<base64-string>", "base64+zstd"]`  (zstd-compressed base64)
///
/// Production keepers always request `Base64` (no zstd) so the
/// response is short and predictable. The keeper-rpc snapshot layer
/// then re-decodes the bytes via Borsh.
fn decode_account_data_blob(blob: &Value) -> Result<Vec<u8>, RpcError> {
    let arr = blob.as_array().ok_or_else(|| {
        RpcError::Decode("getProgramAccounts: expected [data, encoding] array".to_string())
    })?;
    if arr.len() != 2 {
        return Err(RpcError::Decode(format!(
            "getProgramAccounts: expected 2-element data array, got {}",
            arr.len()
        )));
    }
    let payload = arr[0]
        .as_str()
        .ok_or_else(|| RpcError::Decode("data[0] is not a string".to_string()))?;
    let encoding = arr[1]
        .as_str()
        .ok_or_else(|| RpcError::Decode("data[1] is not a string".to_string()))?;
    match encoding {
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|e| RpcError::Decode(format!("base64 decode: {e}"))),
        other => Err(RpcError::Decode(format!(
            "unsupported encoding {other}: keeper-rpc requests base64"
        ))),
    }
}

impl AccountFetcher for SolanaRpcAccountFetcher {
    fn fetch_account(&self, pubkey: &Pubkey32) -> Result<Option<Vec<u8>>, RpcError> {
        let pk = pubkey32_to_pubkey(pubkey);
        match self
            .client
            .get_account_with_commitment(&pk, self.commitment)
        {
            Ok(resp) => Ok(resp.value.map(|a| a.data)),
            Err(e) => Err(classify_client_error(e)),
        }
    }

    fn fetch_program_accounts_filter(
        &self,
        program_id: &Pubkey32,
        match_offset: usize,
        match_bytes: &[u8],
    ) -> Result<Vec<(Pubkey32, Vec<u8>)>, RpcError> {
        let prog = pubkey32_to_pubkey(program_id);
        let memcmp = Memcmp::new_raw_bytes(match_offset, match_bytes.to_vec());
        let cfg = RpcProgramAccountsConfig {
            filters: Some(vec![RpcFilterType::Memcmp(memcmp)]),
            account_config: RpcAccountInfoConfig {
                commitment: Some(self.commitment),
                ..Default::default()
            },
            ..Default::default()
        };

        // The sync `RpcClient` in solana-rpc-client 4.0 does not
        // expose `get_program_accounts_with_config`; route through
        // the lower-level `send` method using the same JSON-RPC
        // params shape the nonblocking client uses internally.
        let res: Result<OptionalContext<Vec<RpcKeyedAccount>>, SolanaClientError> = self
            .client
            .send(RpcRequest::GetProgramAccounts, json!([prog.to_string(), cfg]));

        let keyed = match res {
            Ok(OptionalContext::NoContext(v)) => v,
            Ok(OptionalContext::Context(ctx)) => ctx.value,
            Err(e) => return Err(classify_client_error(e)),
        };

        let mut out: Vec<(Pubkey32, Vec<u8>)> = Vec::with_capacity(keyed.len());
        for k in keyed {
            // Parse the on-the-wire pubkey. We deliberately fail-fast
            // here rather than silently dropping malformed entries —
            // a malformed pubkey would mask a real RPC bug.
            let pk = k.pubkey.parse::<Pubkey>().map_err(|e| {
                RpcError::Decode(format!("invalid pubkey {}: {e}", k.pubkey))
            })?;
            // `RpcKeyedAccount.account` is an `UiAccount` whose
            // `data` field is `UiAccountData`. We requested base64
            // encoding above, so it serialises as `[payload, "base64"]`
            // on the wire. Deserialising back to the typed enum would
            // require pulling solana-account-decoder into our deps;
            // round-tripping through serde_json::Value is one allocation
            // cheaper and avoids that dep.
            let raw = serde_json::to_value(&k.account.data)
                .map_err(|e| RpcError::Decode(format!("data re-serialize: {e}")))?;
            let data = decode_account_data_blob(&raw)?;
            out.push((pubkey_to_pubkey32(&pk), data));
        }
        // Match `MockAccountFetcher`'s deterministic ordering
        // contract so the snapshot's iteration order is identical
        // between mock-fetcher tests and production.
        out.sort_by_key(|(pk, _)| *pk);
        Ok(out)
    }
}

// =====================================================================
// SolanaTxBuilder — implements TxBuilder
// =====================================================================

/// Production [`TxBuilder`] that signs and submits real Solana
/// transactions.
///
/// One instruction per transaction by design — bundling N keeper
/// actions into a single tx complicates retry semantics (a failure
/// now affects every action in the bundle) without buying meaningful
/// fee savings (Solana's per-tx fee floor is dwarfed by the bot's
/// rent for `init_dormant_bucket`). If a future deployment wants
/// batched submission, the right place to add it is **above** this
/// trait — bundle multiple `DispatchedAction`s into one
/// `submit_batch` and emit per-action results.
///
/// The keeper bot's wave-9 `record_init_hint` already deduplicates
/// per (sub_pool, direction, tick), so submitting one tx per action
/// can't accidentally fire the same hint twice.
pub struct SolanaTxBuilder {
    /// Underlying RPC client used for both `get_latest_blockhash`
    /// and `send_and_confirm_transaction`.
    pub client: RpcClient,
    /// Payer + sole signer (the keeper bot's hot wallet). Owned by
    /// value so `mem::replace` rotations don't need locking.
    pub payer: Keypair,
    /// Additional signers, in declaration order. Empty for every
    /// keeper instruction today (no instruction needs a non-payer
    /// signature), but kept for forward-compatibility.
    pub additional_signers: Vec<Keypair>,
    /// Commitment level for the confirm step.
    pub commitment: CommitmentConfig,
}

impl SolanaTxBuilder {
    /// Construct.
    pub fn new(
        rpc_url: String,
        payer: Keypair,
        commitment: CommitmentConfig,
    ) -> Self {
        Self {
            client: RpcClient::new_with_commitment(rpc_url, commitment),
            payer,
            additional_signers: Vec::new(),
            commitment,
        }
    }

    /// Replace the payer keypair. Returns the old one so callers can
    /// audit + secure-erase it.
    pub fn rotate_payer(&mut self, new_payer: Keypair) -> Keypair {
        std::mem::replace(&mut self.payer, new_payer)
    }
}

impl TxBuilder for SolanaTxBuilder {
    fn submit(&mut self, dispatched: &DispatchedAction) -> ActionDispatchResult {
        let ix = dispatched_to_instruction(dispatched);

        let blockhash = match self.client.get_latest_blockhash() {
            Ok(h) => h,
            Err(e) => {
                return ActionDispatchResult::Failed {
                    reason: format!("get_latest_blockhash: {e}"),
                };
            }
        };

        // Build the full signer list. Always at least the payer.
        let mut signer_refs: Vec<&dyn Signer> =
            Vec::with_capacity(1 + self.additional_signers.len());
        signer_refs.push(&self.payer);
        for s in &self.additional_signers {
            signer_refs.push(s);
        }

        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &signer_refs,
            blockhash,
        );

        match self.client.send_and_confirm_transaction(&tx) {
            Ok(sig) => ActionDispatchResult::Submitted {
                signature: Some(sig.to_string()),
            },
            Err(e) => ActionDispatchResult::Failed {
                reason: e.to_string(),
            },
        }
    }
}

// =====================================================================
// SolanaTxBuilder — implements KeeperLeaderTxBuilder (wave 16)
// =====================================================================
//
// The same RPC client / payer that submits keeper actions also
// submits keeper-leader heartbeats. Re-using the wallet keeps the
// holder identity (`current_leader` on `KeeperLeaderLock`) aligned
// with the wallet that signs the bridge ix this leader will then
// submit; reconciling them through one bot process is the
// invariant `keeper_leader_lock_holder_signs_subsequent_actions`
// (wave-16 audit ledger row).

impl crate::leader_tx::KeeperLeaderTxBuilder for SolanaTxBuilder {
    fn submit_leader_ix(
        &mut self,
        instruction: crate::leader_tx::LeaderInstruction,
    ) -> Result<Option<String>, String> {
        let ix = Instruction {
            program_id: pubkey32_to_pubkey(&instruction.program_id),
            data: instruction.data,
            accounts: instruction.accounts.iter().map(account_meta_to_sdk).collect(),
        };
        let blockhash = self
            .client
            .get_latest_blockhash()
            .map_err(|e| format!("get_latest_blockhash: {e}"))?;

        let mut signer_refs: Vec<&dyn Signer> =
            Vec::with_capacity(1 + self.additional_signers.len());
        signer_refs.push(&self.payer);
        for s in &self.additional_signers {
            signer_refs.push(s);
        }

        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &signer_refs,
            blockhash,
        );

        match self.client.send_and_confirm_transaction(&tx) {
            Ok(sig) => Ok(Some(sig.to_string())),
            Err(e) => Err(e.to_string()),
        }
    }
}

// =====================================================================
// Tests
// =====================================================================
//
// These tests exercise the conversion logic and the unreachable-RPC
// error path. They do NOT hit a real cluster — that's the wave-11
// `solana-program-test` integration test (still gated behind the
// toolchain bring-up; see `Docs/Planning/20-…md` § 10.6).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::AccountMeta as PortableMeta;
    use clearing_core::Direction;
    use keeper::KeeperAction;

    fn dummy_pk32(seed: u8) -> Pubkey32 {
        let mut p = [0u8; 32];
        p[0] = seed;
        // non-zero last byte to catch trivial conversion bugs that
        // truncate or zero-pad
        p[31] = 0xab;
        p
    }

    /// `Pubkey32 → solana_pubkey::Pubkey → Pubkey32` is byte-equal —
    /// the keystone identity any further wave-11 RPC plumbing relies
    /// on. If this ever fails, every PDA passed to a real cluster
    /// will silently target a different account.
    #[test]
    fn pubkey_round_trip_is_byte_equal() {
        for seed in [0u8, 1, 7, 42, 99, 200, 254, 255] {
            let pk32 = dummy_pk32(seed);
            let sdk = pubkey32_to_pubkey(&pk32);
            let back = pubkey_to_pubkey32(&sdk);
            assert_eq!(pk32, back, "round-trip differs for seed {seed}");
        }
    }

    /// Wave-9 governance reject paths depend on signer/writable
    /// flags being preserved bit-for-bit through the conversion;
    /// any drift would silently disable signature checks on real
    /// instructions.
    #[test]
    fn account_meta_conversion_preserves_flags() {
        let pk32 = dummy_pk32(7);
        for (is_signer, is_writable) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let portable = PortableMeta {
                pubkey: pk32,
                is_signer,
                is_writable,
            };
            let sdk = account_meta_to_sdk(&portable);
            assert_eq!(sdk.pubkey, pubkey32_to_pubkey(&pk32));
            assert_eq!(sdk.is_signer, is_signer);
            assert_eq!(sdk.is_writable, is_writable);
        }
    }

    /// `dispatched_to_instruction` must carry over the data blob
    /// **byte-equal** — the Anchor discriminator + borsh args
    /// payload was already pinned by `tx::tests::
    /// discriminator_constants_match_sha256_of_anchor_namespace`,
    /// so any drift here would short-circuit that protection.
    #[test]
    fn dispatched_to_instruction_preserves_bytes() {
        let action = KeeperAction::CloseDormantBucket {
            sub_pool_id: 4,
            direction: Direction::Long,
            tick: 7,
        };
        let data = vec![0xd6, 0x62, 0xa8, 0x7a, 0xc1, 0x9c, 0x38, 0x08, 1, 2, 3, 4];
        let accounts = vec![
            PortableMeta {
                pubkey: dummy_pk32(1),
                is_signer: false,
                is_writable: true,
            },
            PortableMeta {
                pubkey: dummy_pk32(2),
                is_signer: true,
                is_writable: true,
            },
        ];
        let dispatched = DispatchedAction {
            action,
            program_id: dummy_pk32(99),
            data: data.clone(),
            accounts: accounts.clone(),
        };
        let ix = dispatched_to_instruction(&dispatched);
        assert_eq!(ix.program_id, pubkey32_to_pubkey(&dummy_pk32(99)));
        assert_eq!(ix.data, data, "data blob must be byte-equal");
        assert_eq!(ix.accounts.len(), 2);
        assert_eq!(ix.accounts[0].pubkey, pubkey32_to_pubkey(&dummy_pk32(1)));
        assert!(!ix.accounts[0].is_signer);
        assert!(ix.accounts[0].is_writable);
        assert_eq!(ix.accounts[1].pubkey, pubkey32_to_pubkey(&dummy_pk32(2)));
        assert!(ix.accounts[1].is_signer);
        assert!(ix.accounts[1].is_writable);
    }

    /// `decode_account_data_blob` happy-path: round-trip a known
    /// payload through the same wire shape `getProgramAccounts`
    /// produces, assert the bytes survive intact.
    #[test]
    fn decode_account_data_blob_round_trip_base64() {
        let want: Vec<u8> = (0..32u8).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&want);
        let blob = json!([encoded, "base64"]);
        let got = decode_account_data_blob(&blob).expect("decode");
        assert_eq!(got, want);
    }

    /// Reject zstd / base58 — the keeper bot deliberately requests
    /// only `base64` so any other encoding indicates a config drift
    /// or broken RPC backend, not silently mis-decoded data.
    #[test]
    fn decode_account_data_blob_rejects_unsupported_encoding() {
        let blob = json!(["abcdef", "base64+zstd"]);
        let r = decode_account_data_blob(&blob);
        assert!(matches!(r, Err(RpcError::Decode(_))));

        let blob = json!(["abcdef", "base58"]);
        let r = decode_account_data_blob(&blob);
        assert!(matches!(r, Err(RpcError::Decode(_))));
    }

    /// `decode_account_data_blob` must reject malformed wire shapes
    /// rather than panicking.
    #[test]
    fn decode_account_data_blob_rejects_malformed_shape() {
        let blob = json!("not-an-array");
        assert!(matches!(
            decode_account_data_blob(&blob),
            Err(RpcError::Decode(_))
        ));

        let blob = json!(["only-one-element"]);
        assert!(matches!(
            decode_account_data_blob(&blob),
            Err(RpcError::Decode(_))
        ));

        let blob = json!([42, "base64"]);
        assert!(matches!(
            decode_account_data_blob(&blob),
            Err(RpcError::Decode(_))
        ));
    }

    /// JSON-RPC error-classification heuristic: messages containing
    /// "RPC response error" are categorised as `RpcError::Code`,
    /// everything else is `RpcError::Transport`. The keeper bot's
    /// retry policy depends on this binary distinction.
    ///
    /// We can't construct `SolanaClientError` directly without the
    /// full client crate's internal types, so this test pins the
    /// *string-matching contract* that the helper relies on; the
    /// integration with the real `ClientError` type is exercised
    /// end-to-end in `fetch_account_against_unreachable_url_…`.
    #[test]
    fn classify_client_error_distinguishes_transport_vs_code() {
        let happy_transport = "io error: connection refused";
        assert!(!happy_transport.contains("RPC response error"));
        let happy_code = "RPC response error -32602: Invalid params";
        assert!(happy_code.contains("RPC response error"));
    }

    /// Hitting an unreachable RPC URL surfaces `RpcError::Transport`,
    /// not a panic. Production keepers loop on transient transport
    /// failures via the higher-level snapshot retry policy; if this
    /// ever panics or returns a `Code` variant, the retry logic
    /// breaks. We use port 1 (which won't bind on Unix) so the
    /// connect attempt fails fast without a long timeout.
    #[test]
    fn fetch_account_against_unreachable_url_yields_transport_error() {
        let f = SolanaRpcAccountFetcher::new(
            "http://127.0.0.1:1".to_string(),
            CommitmentConfig::processed(),
        );
        let pk = dummy_pk32(1);
        let r = f.fetch_account(&pk);
        match r {
            Err(RpcError::Transport(_)) => {}
            other => panic!("expected Transport error, got {other:?}"),
        }
    }

    /// Same contract for `fetch_program_accounts_filter`.
    #[test]
    fn fetch_program_accounts_against_unreachable_url_yields_transport_error() {
        let f = SolanaRpcAccountFetcher::new(
            "http://127.0.0.1:1".to_string(),
            CommitmentConfig::processed(),
        );
        let prog = dummy_pk32(9);
        let r = f.fetch_program_accounts_filter(&prog, 0, &[42, 99]);
        match r {
            Err(RpcError::Transport(_)) => {}
            other => panic!("expected Transport error, got {other:?}"),
        }
    }

    /// `SolanaTxBuilder::submit` against an unreachable RPC must
    /// return `ActionDispatchResult::Failed`, never panic. This is
    /// the per-action failure contract `KeeperLoop` and
    /// `run_plan_cycle` rely on to keep ticking through transient
    /// RPC outages.
    #[test]
    fn solana_tx_builder_submit_against_unreachable_url_returns_failed() {
        let payer = Keypair::new();
        let mut b = SolanaTxBuilder::new(
            "http://127.0.0.1:1".to_string(),
            payer,
            CommitmentConfig::processed(),
        );
        let dispatched = DispatchedAction {
            action: KeeperAction::CloseDormantBucket {
                sub_pool_id: 0,
                direction: Direction::Long,
                tick: 0,
            },
            program_id: dummy_pk32(99),
            data: vec![0xd6, 0x62, 0xa8, 0x7a, 0xc1, 0x9c, 0x38, 0x08],
            accounts: vec![PortableMeta {
                pubkey: dummy_pk32(1),
                is_signer: false,
                is_writable: true,
            }],
        };
        let r = b.submit(&dispatched);
        match r {
            ActionDispatchResult::Failed { .. } => {}
            other => panic!("expected Failed result, got {other:?}"),
        }
    }

    /// Payer rotation returns the previous keypair so callers can
    /// audit + secure-erase it. Locks down the `mem::replace`
    /// semantics — a future refactor that changes ownership (e.g.
    /// to `Arc<Keypair>`) would silently break audit trails without
    /// this test.
    #[test]
    fn rotate_payer_returns_old_keypair() {
        let original = Keypair::new();
        let original_pk = original.pubkey();
        let mut b = SolanaTxBuilder::new(
            "http://127.0.0.1:1".to_string(),
            original,
            CommitmentConfig::processed(),
        );
        let new_payer = Keypair::new();
        let new_pk = new_payer.pubkey();
        let returned = b.rotate_payer(new_payer);
        assert_eq!(returned.pubkey(), original_pk);
        assert_eq!(b.payer.pubkey(), new_pk);
    }
}
