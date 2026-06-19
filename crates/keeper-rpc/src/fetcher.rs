//! Account-fetcher trait + in-memory mock.
//!
//! `keeper-rpc` is intentionally side-effect-free in default
//! features: every concrete RPC call goes through this trait, so
//! tests can substitute a fixture with no `solana-client` runtime.
//! The `solana-rpc` feature provides a real binding in
//! [`crate::solana`].

use std::collections::HashMap;

use crate::Pubkey32;

/// Errors any fetcher may surface.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum RpcError {
    /// Backend transport error (HTTP layer, connection refused, …).
    /// Carries a string so it's `Eq` and host-test-friendly.
    #[error("rpc transport error: {0}")]
    Transport(String),
    /// Backend returned an explicit error code (e.g. JSON-RPC error
    /// object).
    #[error("rpc error code {code}: {message}")]
    Code {
        /// Backend-specific code.
        code: i64,
        /// Human-readable message.
        message: String,
    },
    /// Backend returned data but it failed to deserialize.
    #[error("decode error: {0}")]
    Decode(String),
}

/// Read-only account-fetching surface needed by [`crate::ChainSnapshot`].
///
/// Implementations MUST be deterministic per-(snapshot-instant): if
/// the keeper bot calls `fetch_account` followed by
/// `fetch_program_accounts_filter`, the two reads should observe the
/// same chain state. Real RPC backends should snapshot at a single
/// commitment level (e.g. "confirmed") to honour this contract.
pub trait AccountFetcher {
    /// Fetch a single account by pubkey. Returns `None` when the
    /// account does not exist.
    fn fetch_account(&self, pubkey: &Pubkey32) -> Result<Option<Vec<u8>>, RpcError>;

    /// Fetch every program-owned account for which the bytes at
    /// `match_offset .. match_offset+match_bytes.len()` equal
    /// `match_bytes`. Mirrors `solana-client`'s `getProgramAccounts`
    /// with a single `memcmp` filter.
    ///
    /// `program_id` is the on-chain program's pubkey.
    fn fetch_program_accounts_filter(
        &self,
        program_id: &Pubkey32,
        match_offset: usize,
        match_bytes: &[u8],
    ) -> Result<Vec<(Pubkey32, Vec<u8>)>, RpcError>;
}

/// In-memory `AccountFetcher` for tests. Each entry maps a pubkey
/// to its (program_id, raw_data) tuple. `fetch_program_accounts_filter`
/// linear-scans the entries.
#[derive(Debug, Clone, Default)]
pub struct MockAccountFetcher {
    /// Stored entries, keyed by pubkey.
    pub accounts: HashMap<Pubkey32, MockAccount>,
}

/// One entry in [`MockAccountFetcher`].
#[derive(Debug, Clone)]
pub struct MockAccount {
    /// Owning program id.
    pub owner: Pubkey32,
    /// Raw account data.
    pub data: Vec<u8>,
}

impl MockAccountFetcher {
    /// Construct an empty mock fetcher.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) one account.
    pub fn insert(&mut self, pubkey: Pubkey32, owner: Pubkey32, data: Vec<u8>) {
        self.accounts.insert(pubkey, MockAccount { owner, data });
    }

    /// Number of stored accounts.
    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    /// `true` iff no accounts are stored.
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }
}

impl AccountFetcher for MockAccountFetcher {
    fn fetch_account(&self, pubkey: &Pubkey32) -> Result<Option<Vec<u8>>, RpcError> {
        Ok(self.accounts.get(pubkey).map(|a| a.data.clone()))
    }

    fn fetch_program_accounts_filter(
        &self,
        program_id: &Pubkey32,
        match_offset: usize,
        match_bytes: &[u8],
    ) -> Result<Vec<(Pubkey32, Vec<u8>)>, RpcError> {
        let mut out = Vec::new();
        for (pk, acc) in &self.accounts {
            if &acc.owner != program_id {
                continue;
            }
            let end = match_offset + match_bytes.len();
            if acc.data.len() < end {
                continue;
            }
            if &acc.data[match_offset..end] == match_bytes {
                out.push((*pk, acc.data.clone()));
            }
        }
        // Deterministic order so callers don't accidentally rely on
        // hash-map iteration order.
        out.sort_by_key(|(pk, _)| *pk);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_fetcher_returns_none_for_unknown_pubkey() {
        let f = MockAccountFetcher::new();
        let pk = [1u8; 32];
        assert_eq!(f.fetch_account(&pk).unwrap(), None);
    }

    #[test]
    fn mock_fetcher_program_accounts_filter_memcmp() {
        let mut f = MockAccountFetcher::new();
        let owner = [9u8; 32];
        let other = [7u8; 32];
        // A: owned by `owner`, payload starts with [42, 99]
        f.insert([1u8; 32], owner, vec![42, 99, 1, 2, 3]);
        // B: owned by `owner`, payload starts with [42, 0]
        f.insert([2u8; 32], owner, vec![42, 0, 4, 5]);
        // C: owned by `other`, payload starts with [42, 99]
        f.insert([3u8; 32], other, vec![42, 99, 6]);
        let hits = f.fetch_program_accounts_filter(&owner, 0, &[42, 99]).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, [1u8; 32]);
    }

    /// `fetch_program_accounts_filter` MUST skip accounts whose
    /// payload is shorter than `match_offset + match_bytes.len()` —
    /// otherwise we'd index out of bounds.
    #[test]
    fn mock_fetcher_skips_short_accounts() {
        let mut f = MockAccountFetcher::new();
        let owner = [9u8; 32];
        f.insert([1u8; 32], owner, vec![1u8, 2u8]);
        let hits = f
            .fetch_program_accounts_filter(&owner, 10, &[1u8])
            .unwrap();
        assert!(hits.is_empty());
    }
}
