//! PDA seed builders mirroring `programs/mole-option`'s Anchor
//! `seeds = [...]` declarations.
//!
//! Each function returns the *seeds* — the actual `find_program_address`
//! call requires SHA-256 + ed25519 curve checks, which we only do
//! under the `solana-rpc` feature (where `solana-sdk` provides
//! `Pubkey::find_program_address`). The default-feature build keeps
//! the surface side-effect-free so host tests can use known fixtures
//! without re-deriving anything.
//!
//! ## Seed contracts
//!
//! ```text
//! global_config        = [b"global_config"]
//! market               = [b"market", symbol]
//! sub_pool             = [b"sub_pool", market, sub_pool_id_le]
//! distribution_ledger  = [b"dist_ledger", sub_pool, &[direction_is_long as u8]]
//! dormant_bucket       = [b"dormant_bucket", sub_pool, &[direction_is_long as u8], tick_le]
//! ```
//!
//! Any drift between this file and `programs/mole-option/src/instructions/init.rs`
//! is a wire-format break; see also wave-9 governance §2 for how
//! the bridge enforces version-locked migrations.

use crate::Pubkey32;

/// Seed prefix for the singleton `GlobalConfig` PDA.
pub const SEED_GLOBAL_CONFIG: &[u8] = b"global_config";
/// Seed prefix for `Market` PDAs.
pub const SEED_MARKET: &[u8] = b"market";
/// Seed prefix for `SubPool` PDAs.
pub const SEED_SUB_POOL: &[u8] = b"sub_pool";
/// Seed prefix for `DistributionLedger` PDAs.
pub const SEED_DIST_LEDGER: &[u8] = b"dist_ledger";
/// Seed prefix for `DormantBucket` PDAs.
pub const SEED_DORMANT_BUCKET: &[u8] = b"dormant_bucket";
/// Seed prefix for the per-`Market` vault authority PDA. Not used by
/// the keeper directly but reserved here so a future executor can
/// reference it without redefining the constant.
pub const SEED_MARKET_VAULT_AUTHORITY: &[u8] = b"market_vault_authority";
/// Seed prefix for the per-`Market` keeper-leader-lock PDA (wave 15).
/// Mirrors `keeper_decoder::ix::KEEPER_LEADER_LOCK_SEED`. Re-exported
/// here so wave-16 RPC builders don't have to reach into
/// `keeper-decoder` for a literal byte string.
pub const SEED_KEEPER_LEADER_LOCK: &[u8] = b"keeper_leader_lock";

/// Owned-buffer wrapper for a single PDA's seed list. Heap-allocated
/// so the seeds outlive the call stack — the `solana-rpc` feature
/// hands them to `Pubkey::find_program_address` which takes
/// `&[&[u8]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdaSeeds {
    /// One owned `Vec<u8>` per seed segment, in seed order.
    pub segments: Vec<Vec<u8>>,
}

impl PdaSeeds {
    /// Borrow the segments as `&[&[u8]]` for
    /// `Pubkey::find_program_address` consumption.
    pub fn as_refs(&self) -> Vec<&[u8]> {
        self.segments.iter().map(|s| s.as_slice()).collect()
    }
}

/// Build seeds for the singleton `GlobalConfig` PDA.
pub fn global_config_seeds() -> PdaSeeds {
    PdaSeeds {
        segments: vec![SEED_GLOBAL_CONFIG.to_vec()],
    }
}

/// Build seeds for a `Market` PDA. `symbol` MUST be exactly 16 bytes
/// (zero-padded), matching `Market::symbol`.
pub fn market_seeds(symbol: &[u8; 16]) -> PdaSeeds {
    PdaSeeds {
        segments: vec![SEED_MARKET.to_vec(), symbol.to_vec()],
    }
}

/// Build seeds for a `SubPool` PDA.
pub fn sub_pool_seeds(market: &Pubkey32, sub_pool_id: u32) -> PdaSeeds {
    PdaSeeds {
        segments: vec![
            SEED_SUB_POOL.to_vec(),
            market.to_vec(),
            sub_pool_id.to_le_bytes().to_vec(),
        ],
    }
}

/// Build seeds for a `DistributionLedger` PDA.
pub fn distribution_ledger_seeds(sub_pool: &Pubkey32, direction_is_long: bool) -> PdaSeeds {
    PdaSeeds {
        segments: vec![
            SEED_DIST_LEDGER.to_vec(),
            sub_pool.to_vec(),
            vec![direction_is_long as u8],
        ],
    }
}

/// Build seeds for a `DormantBucket` PDA.
pub fn dormant_bucket_seeds(
    sub_pool: &Pubkey32,
    direction_is_long: bool,
    zero_price_tick: i64,
) -> PdaSeeds {
    PdaSeeds {
        segments: vec![
            SEED_DORMANT_BUCKET.to_vec(),
            sub_pool.to_vec(),
            vec![direction_is_long as u8],
            zero_price_tick.to_le_bytes().to_vec(),
        ],
    }
}

/// Build seeds for the per-`Market` vault-authority PDA.
pub fn market_vault_authority_seeds(market: &Pubkey32) -> PdaSeeds {
    PdaSeeds {
        segments: vec![SEED_MARKET_VAULT_AUTHORITY.to_vec(), market.to_vec()],
    }
}

/// Wave 15/16 — build seeds for a `Market`'s `KeeperLeaderLock` PDA.
/// `seeds = [b"keeper_leader_lock", market.key()]`. Per-market scoping
/// means a keeper failure in one market never starves another.
pub fn keeper_leader_lock_seeds(market: &Pubkey32) -> PdaSeeds {
    PdaSeeds {
        segments: vec![SEED_KEEPER_LEADER_LOCK.to_vec(), market.to_vec()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the seed structure for every PDA. If anyone ever changes
    /// the seeds, this test forces them to update wave-10's
    /// keeper-rpc adapter in lockstep with the program.
    #[test]
    fn seeds_layout_pinned() {
        let market = [9u8; 32];
        let sp_seeds = sub_pool_seeds(&market, 7);
        assert_eq!(sp_seeds.segments[0], b"sub_pool");
        assert_eq!(sp_seeds.segments[1], market.to_vec());
        assert_eq!(sp_seeds.segments[2], 7u32.to_le_bytes().to_vec());

        let sp = [3u8; 32];
        let lg_seeds = distribution_ledger_seeds(&sp, false);
        assert_eq!(lg_seeds.segments[0], b"dist_ledger");
        assert_eq!(lg_seeds.segments[2], vec![0u8]);

        let bk_seeds = dormant_bucket_seeds(&sp, true, -100i64);
        assert_eq!(bk_seeds.segments[0], b"dormant_bucket");
        assert_eq!(bk_seeds.segments[2], vec![1u8]);
        assert_eq!(bk_seeds.segments[3], (-100i64).to_le_bytes().to_vec());

        let mk_seeds = market_seeds(&[7u8; 16]);
        assert_eq!(mk_seeds.segments[0], b"market");
        assert_eq!(mk_seeds.segments[1], [7u8; 16].to_vec());
    }

    /// `as_refs` produces correct borrows that round-trip through
    /// `find_program_address`-shaped consumers.
    #[test]
    fn as_refs_matches_segments() {
        let seeds = global_config_seeds();
        let refs = seeds.as_refs();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0], b"global_config");
    }
}
