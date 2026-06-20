//! Devnet-only mock Pythnet-v2 price oracle.
//!
//! The mole-option program's `pyth-adapter` only accepts **legacy
//! Pythnet v2** price accounts (magic `0xa1b2c3d4`, version 2, atype 3)
//! and rejects anything staler than `max_oracle_age_slots`. Devnet's
//! real legacy Pyth feeds (e.g. `J83w…` SOL/USD) are frozen ~2 years
//! stale, and the new Pyth pull layout (`PriceUpdateV2`) is a different
//! shape the adapter won't parse. So to demo live prices on devnet we
//! own a tiny account and stamp it with the current slot on every push.
//!
//! ## Interface
//!
//! Single instruction (no discriminator):
//!   data    = price:i64 (LE, 8 bytes) ++ conf:u64 (LE, 8 bytes)
//!   accounts[0] = price account — writable, owned by THIS program,
//!                 at least 240 bytes (created client-side as a plain
//!                 keypair account with owner = this program id).
//!
//! Every call rewrites the v2 header fields the adapter reads and sets
//! `agg.pub_slot = Clock::slot`, so staleness checks always pass.
//!
//! ⚠️ This program performs NO authority checks — anyone may push any
//! price. It exists purely for devnet demos and must never be wired
//! into a mainnet market.
#![allow(unexpected_cfgs)]

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint,
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::Sysvar,
};

/// Pythnet v2 constants the mole-option `pyth-adapter` pins.
const PYTH_MAGIC: u32 = 0xa1b2_c3d4;
const PYTH_VERSION: u32 = 2;
const PYTH_ATYPE_PRICE: u32 = 3;
const PYTH_STATUS_TRADING: u32 = 1;
/// Price is reported scaled by 10^EXPO; -8 matches the adapter's target.
const PYTH_EXPO: i32 = -8;

/// Field offsets within a Pythnet v2 price account (mirror of
/// `pyth_adapter::offsets`).
const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 4;
const OFF_ATYPE: usize = 8;
const OFF_EXPO: usize = 20;
const OFF_AGG_PRICE: usize = 208;
const OFF_AGG_CONF: usize = 216;
const OFF_AGG_STATUS: usize = 224;
const OFF_AGG_PUB_SLOT: usize = 232;
/// Adapter reads up to offset 240 (`MIN_HEADER_BYTES`).
const MIN_ACCOUNT_LEN: usize = 240;

entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let it = &mut accounts.iter();
    let price_acc = next_account_info(it)?;

    if price_acc.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    if !price_acc.is_writable {
        return Err(ProgramError::InvalidArgument);
    }
    if data.len() < 16 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let price = i64::from_le_bytes(data[0..8].try_into().unwrap());
    let conf = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let slot = Clock::get()?.slot;

    let mut buf = price_acc.try_borrow_mut_data()?;
    if buf.len() < MIN_ACCOUNT_LEN {
        return Err(ProgramError::AccountDataTooSmall);
    }

    buf[OFF_MAGIC..OFF_MAGIC + 4].copy_from_slice(&PYTH_MAGIC.to_le_bytes());
    buf[OFF_VERSION..OFF_VERSION + 4].copy_from_slice(&PYTH_VERSION.to_le_bytes());
    buf[OFF_ATYPE..OFF_ATYPE + 4].copy_from_slice(&PYTH_ATYPE_PRICE.to_le_bytes());
    buf[OFF_EXPO..OFF_EXPO + 4].copy_from_slice(&PYTH_EXPO.to_le_bytes());
    buf[OFF_AGG_PRICE..OFF_AGG_PRICE + 8].copy_from_slice(&price.to_le_bytes());
    buf[OFF_AGG_CONF..OFF_AGG_CONF + 8].copy_from_slice(&conf.to_le_bytes());
    buf[OFF_AGG_STATUS..OFF_AGG_STATUS + 4].copy_from_slice(&PYTH_STATUS_TRADING.to_le_bytes());
    buf[OFF_AGG_PUB_SLOT..OFF_AGG_PUB_SLOT + 8].copy_from_slice(&slot.to_le_bytes());

    Ok(())
}
