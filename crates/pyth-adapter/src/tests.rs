//! Tests against synthetic Pythnet v2 mock account bytes.

use super::*;

const OWNER_OK: [u8; 32] = [7u8; 32];
const OWNER_BAD: [u8; 32] = [8u8; 32];

/// Build a synthetic Pyth account with the supplied core fields.
/// Bytes outside the documented offsets are zeroed.
#[allow(clippy::too_many_arguments)]
fn mock_account(
    magic: u32,
    version: u32,
    atype: u32,
    expo: i32,
    agg_price: i64,
    agg_conf: u64,
    agg_status: u32,
    agg_pub_slot: u64,
) -> Vec<u8> {
    let mut buf = vec![0u8; MIN_HEADER_BYTES];
    buf[offsets::MAGIC..offsets::MAGIC + 4].copy_from_slice(&magic.to_le_bytes());
    buf[offsets::VERSION..offsets::VERSION + 4].copy_from_slice(&version.to_le_bytes());
    buf[offsets::ATYPE..offsets::ATYPE + 4].copy_from_slice(&atype.to_le_bytes());
    buf[offsets::EXPO..offsets::EXPO + 4].copy_from_slice(&expo.to_le_bytes());
    buf[offsets::AGG_PRICE..offsets::AGG_PRICE + 8].copy_from_slice(&agg_price.to_le_bytes());
    buf[offsets::AGG_CONF..offsets::AGG_CONF + 8].copy_from_slice(&agg_conf.to_le_bytes());
    buf[offsets::AGG_STATUS..offsets::AGG_STATUS + 4].copy_from_slice(&agg_status.to_le_bytes());
    buf[offsets::AGG_PUB_SLOT..offsets::AGG_PUB_SLOT + 8]
        .copy_from_slice(&agg_pub_slot.to_le_bytes());
    buf
}

fn happy_account() -> Vec<u8> {
    // BTC-USD-style: price = 60_000.12345678 USD with expo = -8 ⇒ raw price = 6_000_012_345_678.
    mock_account(
        PYTH_MAGIC,
        PYTH_VERSION,
        PYTH_ACCOUNT_TYPE_PRICE,
        -8,
        6_000_012_345_678,
        50_000, // 5e4 raw conf, ~ 0.001 % of price
        PYTH_STATUS_TRADING,
        1_000,
    )
}

#[test]
fn happy_path_validates() {
    let bytes = happy_account();
    let policy = ValidationPolicy::default();
    let v =
        validate_price_account(&bytes, &OWNER_OK, &OWNER_OK, 1_010, &policy).expect("valid");
    assert_eq!(v.price, 6_000_012_345_678);
    assert_eq!(v.confidence, 50_000);
    assert_eq!(v.expo, -8);
    assert_eq!(v.publish_slot, 1_000);
}

#[test]
fn rescale_when_expo_minus_six() {
    // Raw price 60_000_123_456 with expo = -6 ⇒ rescale to 1e8 needs *100.
    let bytes = mock_account(
        PYTH_MAGIC,
        PYTH_VERSION,
        PYTH_ACCOUNT_TYPE_PRICE,
        -6,
        60_000_123_456,
        12_345,
        PYTH_STATUS_TRADING,
        500,
    );
    let policy = ValidationPolicy::default();
    let v = validate_price_account(&bytes, &OWNER_OK, &OWNER_OK, 510, &policy).unwrap();
    assert_eq!(v.price, 60_000_123_456 * 100);
    assert_eq!(v.confidence, 12_345 * 100);
    assert_eq!(v.expo, -6);
}

#[test]
fn rescale_when_expo_minus_ten() {
    // Raw price 600_001_234_567_890 with expo = -10 ⇒ rescale to 1e8 needs /100 (floor).
    let bytes = mock_account(
        PYTH_MAGIC,
        PYTH_VERSION,
        PYTH_ACCOUNT_TYPE_PRICE,
        -10,
        600_001_234_567_890,
        12_345_678,
        PYTH_STATUS_TRADING,
        1,
    );
    let policy = ValidationPolicy::default();
    let v = validate_price_account(&bytes, &OWNER_OK, &OWNER_OK, 5, &policy).unwrap();
    assert_eq!(v.price, 600_001_234_567_890 / 100);
    assert_eq!(v.confidence, 12_345_678 / 100);
    assert_eq!(v.expo, -10);
}

#[test]
fn rejects_wrong_owner() {
    let bytes = happy_account();
    let policy = ValidationPolicy::default();
    let err = validate_price_account(&bytes, &OWNER_BAD, &OWNER_OK, 1_010, &policy).unwrap_err();
    assert_eq!(err, OracleError::WrongOwner);
}

#[test]
fn rejects_bad_magic() {
    let bytes = mock_account(
        0xdead_beef,
        PYTH_VERSION,
        PYTH_ACCOUNT_TYPE_PRICE,
        -8,
        100,
        1,
        PYTH_STATUS_TRADING,
        100,
    );
    let policy = ValidationPolicy::default();
    let err = validate_price_account(&bytes, &OWNER_OK, &OWNER_OK, 110, &policy).unwrap_err();
    assert!(matches!(err, OracleError::MagicMismatch(0xdeadbeef)));
}

#[test]
fn rejects_wrong_version() {
    let bytes = mock_account(
        PYTH_MAGIC,
        1,
        PYTH_ACCOUNT_TYPE_PRICE,
        -8,
        100,
        1,
        PYTH_STATUS_TRADING,
        100,
    );
    let policy = ValidationPolicy::default();
    let err = validate_price_account(&bytes, &OWNER_OK, &OWNER_OK, 110, &policy).unwrap_err();
    assert_eq!(err, OracleError::VersionMismatch(1));
}

#[test]
fn rejects_non_price_account() {
    let bytes = mock_account(
        PYTH_MAGIC,
        PYTH_VERSION,
        2, /* product account */
        -8,
        100,
        1,
        PYTH_STATUS_TRADING,
        100,
    );
    let policy = ValidationPolicy::default();
    let err = validate_price_account(&bytes, &OWNER_OK, &OWNER_OK, 110, &policy).unwrap_err();
    assert_eq!(err, OracleError::NotPriceAccount(2));
}

#[test]
fn rejects_non_trading_status() {
    let bytes = mock_account(
        PYTH_MAGIC,
        PYTH_VERSION,
        PYTH_ACCOUNT_TYPE_PRICE,
        -8,
        100_000,
        100,
        2, /* halted */
        100,
    );
    let policy = ValidationPolicy::default();
    let err = validate_price_account(&bytes, &OWNER_OK, &OWNER_OK, 110, &policy).unwrap_err();
    assert_eq!(err, OracleError::NotTrading(2));
}

#[test]
fn rejects_expo_out_of_range() {
    for bad_expo in [1, -19, -100, i32::MIN] {
        let bytes = mock_account(
            PYTH_MAGIC,
            PYTH_VERSION,
            PYTH_ACCOUNT_TYPE_PRICE,
            bad_expo,
            100_000,
            100,
            PYTH_STATUS_TRADING,
            100,
        );
        let policy = ValidationPolicy::default();
        let err =
            validate_price_account(&bytes, &OWNER_OK, &OWNER_OK, 110, &policy).unwrap_err();
        assert!(matches!(err, OracleError::ExpoOutOfRange(_)));
    }
}

#[test]
fn rejects_non_positive_price() {
    for bad_price in [0i64, -1, i64::MIN] {
        let bytes = mock_account(
            PYTH_MAGIC,
            PYTH_VERSION,
            PYTH_ACCOUNT_TYPE_PRICE,
            -8,
            bad_price,
            100,
            PYTH_STATUS_TRADING,
            100,
        );
        let policy = ValidationPolicy::default();
        let err =
            validate_price_account(&bytes, &OWNER_OK, &OWNER_OK, 110, &policy).unwrap_err();
        assert!(matches!(err, OracleError::NonPositivePrice(_)));
    }
}

#[test]
fn rejects_stale_price() {
    let bytes = mock_account(
        PYTH_MAGIC,
        PYTH_VERSION,
        PYTH_ACCOUNT_TYPE_PRICE,
        -8,
        100_000,
        100,
        PYTH_STATUS_TRADING,
        500,
    );
    let policy = ValidationPolicy {
        max_age_slots: 25,
        max_confidence_bps: 100,
    };
    // Current slot is 600 ⇒ age = 100 slots > 25.
    let err = validate_price_account(&bytes, &OWNER_OK, &OWNER_OK, 600, &policy).unwrap_err();
    assert_eq!(err, OracleError::Stale(100));
}

#[test]
fn rejects_wide_confidence() {
    // Price = 100_000, conf = 10_000 ⇒ 10_000 / 100_000 = 1000 bps = 10 %.
    // Default policy max is 100 bps (1 %).
    let bytes = mock_account(
        PYTH_MAGIC,
        PYTH_VERSION,
        PYTH_ACCOUNT_TYPE_PRICE,
        -8,
        100_000,
        10_000,
        PYTH_STATUS_TRADING,
        50,
    );
    let policy = ValidationPolicy::default();
    let err = validate_price_account(&bytes, &OWNER_OK, &OWNER_OK, 60, &policy).unwrap_err();
    assert!(matches!(err, OracleError::ConfidenceTooWide(_)));
}

#[test]
fn accepts_when_at_age_boundary() {
    // age = max_age_slots exactly should pass.
    let bytes = mock_account(
        PYTH_MAGIC,
        PYTH_VERSION,
        PYTH_ACCOUNT_TYPE_PRICE,
        -8,
        100_000,
        100,
        PYTH_STATUS_TRADING,
        500,
    );
    let policy = ValidationPolicy {
        max_age_slots: 25,
        max_confidence_bps: 100,
    };
    let v = validate_price_account(&bytes, &OWNER_OK, &OWNER_OK, 525, &policy).unwrap();
    assert_eq!(v.publish_slot, 500);
}

#[test]
fn account_too_small() {
    let bytes = vec![0u8; 100];
    let policy = ValidationPolicy::default();
    let err = validate_price_account(&bytes, &OWNER_OK, &OWNER_OK, 1, &policy).unwrap_err();
    assert!(matches!(err, OracleError::TooSmall(100, _)));
}
