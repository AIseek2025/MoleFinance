//! MoleOption math primitives.
//!
//! Money-grade fixed-point arithmetic for Solana programs and host-side
//! simulators. All operations use checked math and explicit rounding to
//! avoid overflow, sign confusion, and silent dust accrual.
//!
//! Design rules (mirrors `Docs/Planning/11-工程规范与开发指引.md` §5):
//!
//! - Token amounts are `u64` raw, scaled by token decimals.
//! - Prices use `PRICE_SCALE = 1e8` (`u64`).
//! - Notional & pool equity use `u128`.
//! - Signed PnL deltas use `i128`.
//! - Every multiplication/division is checked.
//! - Every division names its rounding direction (`floor` vs `ceil`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use thiserror::Error;

/// Price scale factor. A price of `1.00 USD` is encoded as `1 * PRICE_SCALE`.
pub const PRICE_SCALE: u64 = 100_000_000; // 1e8

/// Basis points scale. `10_000` bps == 100%.
pub const BPS_SCALE: u64 = 10_000;

/// Rate scale, used for ratios like profit_realization_rate.
pub const RATE_SCALE: u128 = 1_000_000_000_000; // 1e12

/// Errors returned by math primitives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MathError {
    /// Generic checked arithmetic overflow.
    #[error("math overflow")]
    Overflow,
    /// Division by zero detected.
    #[error("division by zero")]
    DivByZero,
    /// Signed conversion would lose information.
    #[error("sign conversion overflow")]
    SignOverflow,
}

/// Compute `a * b / denominator`, rounding the result toward zero (floor).
///
/// Uses 256-bit-ish intermediate via `u128` widening, but still requires
/// `a * b` to fit `u256` only conceptually. To stay strictly within `u128`
/// safety we require `a` to fit `u128` and use `u128` widening; this is
/// sufficient for all MoleOption monetary domains because token amounts
/// are bounded by collateral supply.
///
/// Returns [`MathError::DivByZero`] if `denominator == 0`, and
/// [`MathError::Overflow`] on intermediate overflow.
pub fn mul_div_floor(a: u128, b: u128, denominator: u128) -> Result<u128, MathError> {
    if denominator == 0 {
        return Err(MathError::DivByZero);
    }
    full_mul_div(a, b, denominator, false)
}

/// Compute `ceil(a * b / denominator)`.
pub fn mul_div_ceil(a: u128, b: u128, denominator: u128) -> Result<u128, MathError> {
    if denominator == 0 {
        return Err(MathError::DivByZero);
    }
    full_mul_div(a, b, denominator, true)
}

/// Internal helper that performs `a * b / denominator` with optional ceiling.
///
/// Splits each `u128` into two `u64` halves and multiplies them as a 256-bit
/// integer to avoid intermediate `a * b` overflow. Then performs long-division
/// down to `u128`.
fn full_mul_div(a: u128, b: u128, denominator: u128, ceil: bool) -> Result<u128, MathError> {
    if a == 0 || b == 0 {
        return Ok(0);
    }
    // Compute 256-bit product (hi, lo) of a * b.
    let (lo, hi) = mul_to_u256(a, b);

    if hi == 0 {
        // Fast path: a * b fits in u128.
        let q = lo / denominator;
        let r = lo % denominator;
        if ceil && r > 0 {
            q.checked_add(1).ok_or(MathError::Overflow)
        } else {
            Ok(q)
        }
    } else {
        // Slow path: 256-bit / 128-bit long division.
        let (q, r) = div256(hi, lo, denominator)?;
        if ceil && r > 0 {
            q.checked_add(1).ok_or(MathError::Overflow)
        } else {
            Ok(q)
        }
    }
}

/// Multiply two `u128` values into a 256-bit number stored as `(low, high)`.
fn mul_to_u256(a: u128, b: u128) -> (u128, u128) {
    // Split into 64-bit halves: a = aH * 2^64 + aL, b = bH * 2^64 + bL
    let a_hi = a >> 64;
    let a_lo = a & u64::MAX as u128;
    let b_hi = b >> 64;
    let b_lo = b & u64::MAX as u128;

    // Cross products (each fits in u128 because operands are <= 2^64 - 1).
    let ll = a_lo * b_lo;
    let lh = a_lo * b_hi;
    let hl = a_hi * b_lo;
    let hh = a_hi * b_hi;

    // Combine with carry tracking.
    // result_low  = ll_low + (lh + hl) << 64 (low part)
    // result_high = hh + (lh + hl) >> 64 + carry
    let ll_low = ll & u64::MAX as u128;
    let ll_high = ll >> 64;

    let mid = ll_high + (lh & u64::MAX as u128) + (hl & u64::MAX as u128);
    let mid_low = mid & u64::MAX as u128;
    let mid_high = mid >> 64;

    let low = (mid_low << 64) | ll_low;
    let high = hh + (lh >> 64) + (hl >> 64) + mid_high;

    (low, high)
}

/// Divide a 256-bit value `(hi, lo)` by a `u128` divisor, returning `(quotient, remainder)`.
///
/// Quotient must fit in `u128`; otherwise `MathError::Overflow` is returned.
fn div256(hi: u128, lo: u128, divisor: u128) -> Result<(u128, u128), MathError> {
    if hi >= divisor {
        // Quotient would not fit in u128.
        return Err(MathError::Overflow);
    }

    // Bit-by-bit long division. 256 bit operand vs 128 bit divisor.
    // We follow the standard "shift remainder, OR next bit, conditional subtract" approach.
    let mut rem = hi;
    let mut quot: u128 = 0;
    for i in (0..128).rev() {
        // Pull next bit of `lo` into the bottom of `rem`.
        let next_bit = (lo >> i) & 1;
        // rem = rem << 1 | next_bit. We must guard against overflow.
        let rem_top = rem >> 127;
        rem = (rem << 1) | next_bit;
        // If rem_top was 1, then conceptually rem is `2^128 + rem`. We must subtract divisor
        // and set the quotient bit if `2^128 + rem >= divisor`.
        if rem_top == 1 || rem >= divisor {
            // (2^128 + rem) - divisor == rem.wrapping_sub(divisor) when rem_top == 1,
            // because divisor < 2^128.
            rem = rem.wrapping_sub(divisor);
            quot |= 1u128 << i;
        }
    }

    Ok((quot, rem))
}

/// Compute `a + b` checked.
#[inline]
pub fn checked_add(a: u128, b: u128) -> Result<u128, MathError> {
    a.checked_add(b).ok_or(MathError::Overflow)
}

/// Compute `a - b` checked.
#[inline]
pub fn checked_sub(a: u128, b: u128) -> Result<u128, MathError> {
    a.checked_sub(b).ok_or(MathError::Overflow)
}

/// Compute `a * b` checked.
#[inline]
pub fn checked_mul(a: u128, b: u128) -> Result<u128, MathError> {
    a.checked_mul(b).ok_or(MathError::Overflow)
}

/// Compute `direction * notional * (P_now - P_last) / P_last` returning a signed PnL increment
/// in raw collateral units.
///
/// `direction = +1` for long, `-1` for short. Operates entirely in checked arithmetic.
pub fn signed_pnl_increment(
    direction: i8,
    notional: u128,
    p_last: u64,
    p_now: u64,
) -> Result<i128, MathError> {
    if p_last == 0 {
        return Err(MathError::DivByZero);
    }
    let (delta_abs, sign) = if p_now >= p_last {
        ((p_now - p_last) as u128, 1i8)
    } else {
        ((p_last - p_now) as u128, -1i8)
    };

    let abs_pnl_u128 = mul_div_floor(notional, delta_abs, p_last as u128)?;
    let abs_pnl_i128: i128 = abs_pnl_u128
        .try_into()
        .map_err(|_| MathError::SignOverflow)?;

    let combined_sign = sign as i32 * direction as i32;
    if combined_sign == 0 || abs_pnl_i128 == 0 {
        return Ok(0);
    }
    if combined_sign > 0 {
        Ok(abs_pnl_i128)
    } else {
        Ok(-abs_pnl_i128)
    }
}

/// Compute `abs(P_new - P_old) * BPS_SCALE / P_old` as bps.
///
/// Returns [`MathError::DivByZero`] when `P_old == 0`, and
/// [`MathError::Overflow`] when the result exceeds `u64::MAX` (which would
/// require >184e16% movement, far beyond reality but still checked).
pub fn price_move_bps(p_old: u64, p_new: u64) -> Result<u64, MathError> {
    if p_old == 0 {
        return Err(MathError::DivByZero);
    }
    let delta = if p_new >= p_old {
        (p_new - p_old) as u128
    } else {
        (p_old - p_new) as u128
    };
    let scaled = mul_div_floor(delta, BPS_SCALE as u128, p_old as u128)?;
    u64::try_from(scaled).map_err(|_| MathError::Overflow)
}

#[cfg(test)]
mod tests;
