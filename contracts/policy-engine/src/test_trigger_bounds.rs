//! Tests for trigger_threshold validation within product bounds (Issue #773).
//!
//! Acceptance criteria:
//! - Validate within product bounds (0 < threshold <= 10^21)
//! - Reject out-of-range thresholds (zero, negative, > 10^21)
//! - Reject mismatched threshold upon policy purchase
//! - Test covers bounds
//! - Document threshold rules
#![cfg(test)]

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::Address as _,
    token::StellarAssetClient,
    Address, Env, Symbol,
};

const PROTOCOL_MAX_THRESHOLD: i128 = 1_000_000_000_000_000_000_000i128; // 10^21

fn setup() -> (Env, Address, Address, PolicyEngineClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let usdc = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let id = env.register(PolicyEngine, ());
    let client = PolicyEngineClient::new(&env, &id);
    client.initialize(&admin, &usdc, &Address::generate(&env));
    (env, admin, usdc, client)
}

fn make_product_params(
    name: Symbol,
    category: Symbol,
    oracle_key: Symbol,
    expected_token: Address,
    trigger_threshold: i128,
) -> CreateProductParams {
    CreateProductParams {
        name,
        category,
        oracle_key,
        expected_token,
        trigger_type: TriggerType::Threshold,
        oracle_data_type: symbol_short!("weather"),
        trigger_threshold,
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min: 100_000_000,
        coverage_max: 10_000_000_000,
        premium_rate_bps: 500,
        max_duration_days: 365,
    }
}

#[test]
fn valid_trigger_threshold_within_bounds_succeeds() {
    let (env, admin, usdc, client) = setup();
    let product_id = client.create_product(
        &admin,
        &make_product_params(
            symbol_short!("crop_ok"),
            symbol_short!("crop"),
            symbol_short!("rain1"),
            usdc.clone(),
            50_000_000,
        ),
    );

    let product = client.get_product(&product_id);
    assert_eq!(product.trigger_threshold, 50_000_000);

    let buyer = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &10_000_000_000i128);

    // buy_policy_with_threshold succeeds when matching product bounds
    let policy_id = client.buy_policy_with_threshold(
        &buyer,
        &product_id,
        &500_000_000i128,
        &30u32,
        &symbol_short!("rain1"),
        &usdc,
        &50_000_000i128,
    );
    let policy = client.get_policy(&policy_id);
    assert_eq!(policy.trigger_threshold, 50_000_000);
}

#[test]
fn min_boundary_trigger_threshold_succeeds() {
    let (_env, admin, usdc, client) = setup();
    // Lower bound: strictly positive (1 is valid)
    let product_id = client.create_product(
        &admin,
        &make_product_params(
            symbol_short!("crop_min"),
            symbol_short!("crop"),
            symbol_short!("rainmin"),
            usdc.clone(),
            1i128,
        ),
    );
    assert_eq!(client.get_product(&product_id).trigger_threshold, 1);
}

#[test]
fn max_boundary_trigger_threshold_succeeds() {
    let (_env, admin, usdc, client) = setup();
    // Upper bound: PROTOCOL_MAX_THRESHOLD (10^21) is valid
    let product_id = client.create_product(
        &admin,
        &make_product_params(
            symbol_short!("crop_max"),
            symbol_short!("crop"),
            symbol_short!("rainmax"),
            usdc.clone(),
            PROTOCOL_MAX_THRESHOLD,
        ),
    );
    assert_eq!(
        client.get_product(&product_id).trigger_threshold,
        PROTOCOL_MAX_THRESHOLD
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn zero_trigger_threshold_is_rejected() {
    let (_env, admin, usdc, client) = setup();
    client.create_product(
        &admin,
        &make_product_params(
            symbol_short!("crop_zero"),
            symbol_short!("crop"),
            symbol_short!("rainz"),
            usdc,
            0i128,
        ),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn negative_trigger_threshold_is_rejected() {
    let (_env, admin, usdc, client) = setup();
    client.create_product(
        &admin,
        &make_product_params(
            symbol_short!("crop_neg"),
            symbol_short!("crop"),
            symbol_short!("rainneg"),
            usdc,
            -1i128,
        ),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn exceeding_max_trigger_threshold_is_rejected() {
    let (_env, admin, usdc, client) = setup();
    client.create_product(
        &admin,
        &make_product_params(
            symbol_short!("crop_huge"),
            symbol_short!("crop"),
            symbol_short!("rainhuge"),
            usdc,
            PROTOCOL_MAX_THRESHOLD + 1,
        ),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn buy_policy_with_mismatched_threshold_is_rejected() {
    let (env, admin, usdc, client) = setup();
    let product_id = client.create_product(
        &admin,
        &make_product_params(
            symbol_short!("crop_p"),
            symbol_short!("crop"),
            symbol_short!("rainp"),
            usdc.clone(),
            50_000_000,
        ),
    );

    let buyer = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &10_000_000_000i128);

    // Mismatched threshold (60_000_000 != 50_000_000) must be rejected with InvalidTriggerThreshold (#14)
    client.buy_policy_with_threshold(
        &buyer,
        &product_id,
        &500_000_000i128,
        &30u32,
        &symbol_short!("rainp"),
        &usdc,
        &60_000_000i128,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn buy_policy_with_out_of_bounds_threshold_is_rejected() {
    let (env, admin, usdc, client) = setup();
    let product_id = client.create_product(
        &admin,
        &make_product_params(
            symbol_short!("crop_b"),
            symbol_short!("crop"),
            symbol_short!("rainb"),
            usdc.clone(),
            50_000_000,
        ),
    );

    let buyer = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &10_000_000_000i128);

    // Negative threshold must be rejected
    client.buy_policy_with_threshold(
        &buyer,
        &product_id,
        &500_000_000i128,
        &30u32,
        &symbol_short!("rainb"),
        &usdc,
        &-50_000_000i128,
    );
}
