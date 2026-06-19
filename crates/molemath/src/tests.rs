//! Unit and property tests for `molemath`.

use super::*;
use proptest::prelude::*;

#[test]
fn mul_div_floor_basic() {
    assert_eq!(mul_div_floor(100, 2, 4).unwrap(), 50);
    assert_eq!(mul_div_floor(0, 999, 7).unwrap(), 0);
    assert_eq!(mul_div_floor(7, 0, 7).unwrap(), 0);
}

#[test]
fn mul_div_floor_rounds_down() {
    assert_eq!(mul_div_floor(10, 3, 4).unwrap(), 7); // 30/4 = 7.5 -> 7
}

#[test]
fn mul_div_ceil_rounds_up() {
    assert_eq!(mul_div_ceil(10, 3, 4).unwrap(), 8); // 30/4 = 7.5 -> 8
    assert_eq!(mul_div_ceil(8, 1, 4).unwrap(), 2); // exact, no ceil
}

#[test]
fn mul_div_div_by_zero() {
    assert!(matches!(mul_div_floor(1, 1, 0), Err(MathError::DivByZero)));
    assert!(matches!(mul_div_ceil(1, 1, 0), Err(MathError::DivByZero)));
}

#[test]
fn mul_div_handles_huge_intermediate() {
    // a * b would overflow u128 (a ~ 2^120, b ~ 2^120) but the quotient still fits.
    let a: u128 = 1u128 << 120;
    let b: u128 = 1u128 << 120;
    let denom: u128 = 1u128 << 120;
    assert_eq!(mul_div_floor(a, b, denom).unwrap(), 1u128 << 120);
}

#[test]
fn mul_div_quotient_overflow() {
    let a: u128 = u128::MAX;
    let b: u128 = u128::MAX;
    // a * b / 1 cannot fit in u128.
    assert!(matches!(mul_div_floor(a, b, 1), Err(MathError::Overflow)));
}

#[test]
fn signed_pnl_long_up() {
    let notional: u128 = 1_000 * PRICE_SCALE as u128; // 1000 USDC notional
    let p0 = 100 * PRICE_SCALE;
    let p1 = 110 * PRICE_SCALE;
    let pnl = signed_pnl_increment(1, notional, p0, p1).unwrap();
    // 10% gain -> 100 USDC * PRICE_SCALE
    assert_eq!(pnl, 100 * PRICE_SCALE as i128);
}

#[test]
fn signed_pnl_short_up_is_loss() {
    let notional: u128 = 1_000 * PRICE_SCALE as u128;
    let p0 = 100 * PRICE_SCALE;
    let p1 = 110 * PRICE_SCALE;
    let pnl = signed_pnl_increment(-1, notional, p0, p1).unwrap();
    assert_eq!(pnl, -(100 * PRICE_SCALE as i128));
}

#[test]
fn signed_pnl_long_down_is_loss() {
    let notional: u128 = 1_000 * PRICE_SCALE as u128;
    let p0 = 100 * PRICE_SCALE;
    let p1 = 90 * PRICE_SCALE;
    let pnl = signed_pnl_increment(1, notional, p0, p1).unwrap();
    assert_eq!(pnl, -(100 * PRICE_SCALE as i128));
}

#[test]
fn signed_pnl_no_change() {
    let pnl = signed_pnl_increment(1, 1_000, 100, 100).unwrap();
    assert_eq!(pnl, 0);
}

#[test]
fn signed_pnl_div_zero() {
    assert!(matches!(
        signed_pnl_increment(1, 1_000, 0, 100),
        Err(MathError::DivByZero)
    ));
}

#[test]
fn price_move_bps_basic() {
    let p0 = 100 * PRICE_SCALE;
    assert_eq!(price_move_bps(p0, p0).unwrap(), 0);
    assert_eq!(price_move_bps(p0, p0 + p0 / 100).unwrap(), 100); // 1%
    assert_eq!(price_move_bps(p0, p0 - p0 / 100).unwrap(), 100); // -1%
}

proptest! {
    /// `mul_div_floor(a, b, d) <= a * b / d` and `mul_div_ceil` upper-bounds it by 1.
    #[test]
    fn prop_floor_ceil_relationship(
        a in 0u128..=(1u128 << 100),
        b in 0u128..=(1u128 << 100),
        d in 1u128..=(1u128 << 100),
    ) {
        let f = mul_div_floor(a, b, d).unwrap();
        let c = mul_div_ceil(a, b, d).unwrap();
        prop_assert!(c >= f);
        prop_assert!(c - f <= 1);
    }

    /// `mul_div_floor(a, d, d) == a` for `d > 0` when result fits.
    #[test]
    fn prop_identity(a in 0u128..=(1u128 << 100), d in 1u128..=(1u128 << 100)) {
        prop_assert_eq!(mul_div_floor(a, d, d).unwrap(), a);
    }

    /// `signed_pnl_increment` is anti-symmetric in direction.
    #[test]
    fn prop_pnl_antisymmetric(
        notional in 0u128..=(1u128 << 80),
        p_last in 1u64..=(1_000_000 * PRICE_SCALE),
        p_now in 1u64..=(1_000_000 * PRICE_SCALE),
    ) {
        let long = signed_pnl_increment(1, notional, p_last, p_now).unwrap();
        let short = signed_pnl_increment(-1, notional, p_last, p_now).unwrap();
        prop_assert_eq!(long, -short);
    }

    /// `price_move_bps(p, q)` matches `|q - p| * BPS / p`.
    #[test]
    fn prop_price_move_definition(
        p in 1u64..=(1_000_000 * PRICE_SCALE),
        q in 0u64..=(1_000_000 * PRICE_SCALE),
    ) {
        let result = price_move_bps(p, q).unwrap();
        let delta = if q >= p { (q - p) as u128 } else { (p - q) as u128 };
        let expected = (delta * BPS_SCALE as u128) / p as u128;
        prop_assert_eq!(result as u128, expected);
    }

    /// Zero delta -> zero bps.
    #[test]
    fn prop_price_move_zero(p in 1u64..=(1_000_000 * PRICE_SCALE)) {
        prop_assert_eq!(price_move_bps(p, p).unwrap(), 0);
    }
}
