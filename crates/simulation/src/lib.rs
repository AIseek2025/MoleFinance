//! Offline per-position clearing oracle.
//!
//! Implements the whitepaper §3 algorithm in its plainest form: each
//! position carries its own `locked_loss` and `realized_profit_balance`.
//! On every "reset" the oracle:
//!
//! 1. Computes `delta_pnl_i = D_i * N_i * (P_now - P_r) / P_r`.
//! 2. Aggregates `TOTAL_PROFIT` and `TOTAL_REALIZABLE_LOSS`.
//! 3. Computes `ACTUAL_TRANSFER = min(TOTAL_PROFIT,
//!    TOTAL_REALIZABLE_LOSS)`.
//! 4. Distributes proportionally to each profit/loss position.
//! 5. Applies the recovery rule: `realized_profit_balance` is consumed
//!    before increasing `locked_loss`.
//!
//! This crate is **not** a runtime path. It exists to:
//!
//! - serve as the ground-truth for clearing-core equivalence tests, and
//! - back the off-chain indexer's "equivalent locked_loss" view.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use molemath::{checked_add, checked_sub, mul_div_floor, signed_pnl_increment, BPS_SCALE};
use thiserror::Error;

pub use clearing_core::Direction;

/// Errors returned by the oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum OracleError {
    /// Math overflow.
    #[error("math overflow")]
    MathOverflow,
    /// Division by zero (typically from a zero-price feed).
    #[error("division by zero")]
    DivByZero,
    /// Internal invariant violation.
    #[error("invariant: {0}")]
    Invariant(&'static str),
}

impl From<molemath::MathError> for OracleError {
    fn from(value: molemath::MathError) -> Self {
        match value {
            molemath::MathError::Overflow => OracleError::MathOverflow,
            molemath::MathError::DivByZero => OracleError::DivByZero,
            molemath::MathError::SignOverflow => OracleError::MathOverflow,
        }
    }
}

/// Per-position state inside the oracle. Mirrors the conceptual struct in
/// `Docs/Planning/05-核心机制与数学模型.md` §2.2.
#[derive(Debug, Clone)]
pub struct OraclePosition {
    /// Stable id for accounting.
    pub id: u64,
    /// Long or short.
    pub direction: Direction,
    /// Initial principal (raw collateral units).
    pub principal: u128,
    /// `notional = principal * leverage_bps / BPS_SCALE`.
    pub notional: u128,
    /// Reset price last applied to this position. Initially equals the
    /// open price.
    pub reset_price: u64,
    /// Cumulative `locked_loss` (monotone non-decreasing).
    pub locked_loss: u128,
    /// Profits accrued from later counterparty losses, withdrawable on
    /// close. Consumed by future losses before increasing `locked_loss`.
    pub realized_profit_balance: u128,
    /// `true` once the position has been closed.
    pub closed: bool,
}

impl OraclePosition {
    /// Helper constructor mirroring [`clearing_core::open_position`].
    pub fn new(
        id: u64,
        direction: Direction,
        principal: u128,
        leverage_bps: u32,
        entry_price: u64,
    ) -> Result<Self, OracleError> {
        let notional = mul_div_floor(principal, leverage_bps as u128, BPS_SCALE as u128)?;
        Ok(Self {
            id,
            direction,
            principal,
            notional,
            reset_price: entry_price,
            locked_loss: 0,
            realized_profit_balance: 0,
            closed: false,
        })
    }

    /// Remaining margin = principal - locked_loss.
    pub fn remaining_margin(&self) -> u128 {
        self.principal.saturating_sub(self.locked_loss)
    }

    /// Withdrawable equity at any given moment.
    pub fn equity(&self) -> u128 {
        self.principal.saturating_sub(self.locked_loss) + self.realized_profit_balance
    }
}

/// Snapshot of a single oracle settlement step.
#[derive(Debug, Clone, Default)]
pub struct SettleSummary {
    /// `TOTAL_PROFIT` over the step.
    pub total_profit: u128,
    /// `TOTAL_REALIZABLE_LOSS` over the step.
    pub total_realizable_loss: u128,
    /// Funds actually transferred (the smaller side).
    pub actual_transfer: u128,
}

/// Run one settlement step at price `p_now`.
///
/// `positions` is mutated in place: `locked_loss`, `realized_profit_balance`,
/// and `reset_price` are updated.
pub fn settle(positions: &mut [OraclePosition], p_now: u64) -> Result<SettleSummary, OracleError> {
    if p_now == 0 {
        return Err(OracleError::DivByZero);
    }

    // Step 1+2: per-position delta_pnl, aggregated profits/losses.
    let mut profit_increments: Vec<(usize, u128)> = Vec::new();
    let mut loss_increments: Vec<(usize, u128)> = Vec::new();
    let mut total_profit: u128 = 0;
    let mut total_realizable_loss: u128 = 0;

    for (idx, pos) in positions.iter().enumerate() {
        if pos.closed {
            continue;
        }
        if pos.notional == 0 {
            continue;
        }
        let delta = signed_pnl_increment(
            pos.direction.sign(),
            pos.notional,
            pos.reset_price,
            p_now,
        )?;
        if delta > 0 {
            let inc = delta as u128;
            profit_increments.push((idx, inc));
            total_profit = checked_add(total_profit, inc)?;
        } else if delta < 0 {
            let raw_loss = (-delta) as u128;
            let realizable = raw_loss.min(pos.remaining_margin());
            if realizable > 0 {
                loss_increments.push((idx, realizable));
                total_realizable_loss = checked_add(total_realizable_loss, realizable)?;
            }
        }
    }

    let actual_transfer = total_profit.min(total_realizable_loss);
    let summary = SettleSummary {
        total_profit,
        total_realizable_loss,
        actual_transfer,
    };

    if actual_transfer == 0 {
        for pos in positions.iter_mut() {
            if !pos.closed {
                pos.reset_price = p_now;
            }
        }
        return Ok(summary);
    }

    // Step 4: distribute realized profit / loss.
    // We use floor rounding everywhere; residuals from rounding stay with
    // the protocol (the caller can read them via `summary` and dust-track).
    let mut allocated_profit: u128 = 0;
    for (idx, inc) in &profit_increments {
        let share = mul_div_floor(actual_transfer, *inc, total_profit)?;
        positions[*idx].realized_profit_balance =
            checked_add(positions[*idx].realized_profit_balance, share)?;
        allocated_profit = checked_add(allocated_profit, share)?;
    }
    debug_assert!(allocated_profit <= actual_transfer);

    let mut allocated_loss: u128 = 0;
    for (idx, realizable) in &loss_increments {
        let share = mul_div_floor(actual_transfer, *realizable, total_realizable_loss)?;
        // Apply recovery rule: consume realized_profit_balance first.
        let pos = &mut positions[*idx];
        let credit_used = pos.realized_profit_balance.min(share);
        pos.realized_profit_balance = checked_sub(pos.realized_profit_balance, credit_used)?;
        let lock_inc = checked_sub(share, credit_used)?;
        // Cap by remaining margin (should already be guaranteed by the
        // realizable cap above, but we keep the explicit check).
        let cap = pos.remaining_margin();
        let actual_lock = lock_inc.min(cap);
        pos.locked_loss = checked_add(pos.locked_loss, actual_lock)?;
        allocated_loss = checked_add(allocated_loss, actual_lock + credit_used)?;
    }
    debug_assert!(allocated_loss <= actual_transfer);

    // Step 5: roll the reset price for everyone.
    for pos in positions.iter_mut() {
        if !pos.closed {
            pos.reset_price = p_now;
        }
    }

    Ok(summary)
}

/// Close a position at `p_now` after running [`settle`]. Returns the
/// withdrawable amount and marks the position as `closed`.
pub fn close(positions: &mut [OraclePosition], idx: usize, p_now: u64) -> Result<u128, OracleError> {
    settle(positions, p_now)?;
    let pos = positions
        .get_mut(idx)
        .ok_or(OracleError::Invariant("idx out of range"))?;
    if pos.closed {
        return Ok(0);
    }
    let withdrawable = pos.equity();
    pos.closed = true;
    pos.realized_profit_balance = 0;
    pos.locked_loss = pos.principal; // all remaining principal locked at close
    Ok(withdrawable)
}
