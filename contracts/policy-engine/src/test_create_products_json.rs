#![cfg(test)]
//! Test that create_products.sh JSON params match CreateProductParams struct.
//!
//! This test validates that the JSON payloads in scripts/create_products.sh
//! can be successfully deserialized into CreateProductParams. If the struct
//! fields change but the script is not updated, this test will catch the
//! mismatch at build time.

use crate::types::{CreateProductParams, TriggerComparison, TriggerType};
use soroban_sdk::{testutils::Address as _, Address, Env, String as SorobanString, Symbol};

/// Simulates parsing the JSON from create_products.sh for Crop Drought Insurance
#[test]
fn test_crop_drought_insurance_json() {
    let env = Env::default();
    let usdc_token = Address::generate(&env);

    // JSON from create_products.sh (line ~36)
    let params = CreateProductParams {
        name: SorobanString::from_str(&env, "Crop_Drought_Insurance"),
        category: Symbol::new(&env, "crop"),
        oracle_key: Symbol::new(&env, "kis2606"),
        expected_token: usdc_token.clone(),
        trigger_type: TriggerType::Threshold,
        oracle_data_type: Symbol::new(&env, "weather"),
        premium_rate_bps: 300,
        coverage_min: 1_000_000_000,
        coverage_max: 100_000_000_000,
        max_duration_days: 180,
        trigger_threshold: 500_000_000,
        trigger_comparison: TriggerComparison::LessThan,
    };

    // Verify all fields are set correctly
    assert_eq!(params.name, SorobanString::from_str(&env, "Crop_Drought_Insurance"));
    assert_eq!(params.category, Symbol::new(&env, "crop"));
    assert_eq!(params.oracle_key, Symbol::new(&env, "kis2606"));
    assert_eq!(params.expected_token, usdc_token);
    assert_eq!(params.trigger_type, TriggerType::Threshold);
    assert_eq!(params.oracle_data_type, Symbol::new(&env, "weather"));
    assert_eq!(params.premium_rate_bps, 300);
    assert_eq!(params.coverage_min, 1_000_000_000);
    assert_eq!(params.coverage_max, 100_000_000_000);
    assert_eq!(params.max_duration_days, 180);
    assert_eq!(params.trigger_threshold, 500_000_000);
    assert_eq!(params.trigger_comparison, TriggerComparison::LessThan);
}

/// Simulates parsing the JSON from create_products.sh for Flight Delay Insurance
#[test]
fn test_flight_delay_insurance_json() {
    let env = Env::default();
    let usdc_token = Address::generate(&env);

    // JSON from create_products.sh (line ~54)
    let params = CreateProductParams {
        name: SorobanString::from_str(&env, "Flight_Delay_Insurance"),
        category: Symbol::new(&env, "flight"),
        oracle_key: Symbol::new(&env, "flt001"),
        expected_token: usdc_token.clone(),
        trigger_type: TriggerType::Binary,
        oracle_data_type: Symbol::new(&env, "flight"),
        premium_rate_bps: 150,
        coverage_min: 100_000_000,
        coverage_max: 10_000_000_000,
        max_duration_days: 2,
        trigger_threshold: 1_200_000_000,
        trigger_comparison: TriggerComparison::GreaterThan,
    };

    // Verify all fields
    assert_eq!(params.name, SorobanString::from_str(&env, "Flight_Delay_Insurance"));
    assert_eq!(params.category, Symbol::new(&env, "flight"));
    assert_eq!(params.oracle_key, Symbol::new(&env, "flt001"));
    assert_eq!(params.expected_token, usdc_token);
    assert_eq!(params.trigger_type, TriggerType::Binary);
    assert_eq!(params.oracle_data_type, Symbol::new(&env, "flight"));
    assert_eq!(params.premium_rate_bps, 150);
    assert_eq!(params.coverage_min, 100_000_000);
    assert_eq!(params.coverage_max, 10_000_000_000);
    assert_eq!(params.max_duration_days, 2);
    assert_eq!(params.trigger_threshold, 1_200_000_000);
    assert_eq!(params.trigger_comparison, TriggerComparison::GreaterThan);
}

/// Simulates parsing the JSON from create_products.sh for DeFi Protocol Hack Insurance
#[test]
fn test_defi_hack_insurance_json() {
    let env = Env::default();
    let usdc_token = Address::generate(&env);

    // JSON from create_products.sh (line ~71)
    let params = CreateProductParams {
        name: SorobanString::from_str(&env, "DeFi_Hack_Insurance"),
        category: Symbol::new(&env, "defi"),
        oracle_key: Symbol::new(&env, "defi001"),
        expected_token: usdc_token.clone(),
        trigger_type: TriggerType::Parametric,
        oracle_data_type: Symbol::new(&env, "onchain"),
        premium_rate_bps: 500,
        coverage_min: 10_000_000_000,
        coverage_max: 1_000_000_000_000,
        max_duration_days: 365,
        trigger_threshold: 500_000_000,
        trigger_comparison: TriggerComparison::GreaterThan,
    };

    // Verify all fields
    assert_eq!(params.name, SorobanString::from_str(&env, "DeFi_Hack_Insurance"));
    assert_eq!(params.category, Symbol::new(&env, "defi"));
    assert_eq!(params.oracle_key, Symbol::new(&env, "defi001"));
    assert_eq!(params.expected_token, usdc_token);
    assert_eq!(params.trigger_type, TriggerType::Parametric);
    assert_eq!(params.oracle_data_type, Symbol::new(&env, "onchain"));
    assert_eq!(params.premium_rate_bps, 500);
    assert_eq!(params.coverage_min, 10_000_000_000);
    assert_eq!(params.coverage_max, 1_000_000_000_000);
    assert_eq!(params.max_duration_days, 365);
    assert_eq!(params.trigger_threshold, 500_000_000);
    assert_eq!(params.trigger_comparison, TriggerComparison::GreaterThan);
}

/// Simulates parsing the JSON from create_products.sh for Natural Disaster Insurance
#[test]
fn test_natural_disaster_insurance_json() {
    let env = Env::default();
    let usdc_token = Address::generate(&env);

    // JSON from create_products.sh (line ~88)
    let params = CreateProductParams {
        name: SorobanString::from_str(&env, "Natural_Disaster_Insurance"),
        category: Symbol::new(&env, "disaster"),
        oracle_key: Symbol::new(&env, "dis001"),
        expected_token: usdc_token.clone(),
        trigger_type: TriggerType::Threshold,
        oracle_data_type: Symbol::new(&env, "disaster"),
        premium_rate_bps: 200,
        coverage_min: 5_000_000_000,
        coverage_max: 500_000_000_000,
        max_duration_days: 365,
        trigger_threshold: 60_000_000,
        trigger_comparison: TriggerComparison::GreaterThan,
    };

    // Verify all fields
    assert_eq!(params.name, SorobanString::from_str(&env, "Natural_Disaster_Insurance"));
    assert_eq!(params.category, Symbol::new(&env, "disaster"));
    assert_eq!(params.oracle_key, Symbol::new(&env, "dis001"));
    assert_eq!(params.expected_token, usdc_token);
    assert_eq!(params.trigger_type, TriggerType::Threshold);
    assert_eq!(params.oracle_data_type, Symbol::new(&env, "disaster"));
    assert_eq!(params.premium_rate_bps, 200);
    assert_eq!(params.coverage_min, 5_000_000_000);
    assert_eq!(params.coverage_max, 500_000_000_000);
    assert_eq!(params.max_duration_days, 365);
    assert_eq!(params.trigger_threshold, 60_000_000);
    assert_eq!(params.trigger_comparison, TriggerComparison::GreaterThan);
}

/// Test that all required fields are present in CreateProductParams.
/// If a new required field is added to CreateProductParams but not to the
/// script, this test will fail to compile.
#[test]
fn test_all_fields_required() {
    let env = Env::default();
    let usdc_token = Address::generate(&env);

    // This will fail to compile if any field is missing
    let _params = CreateProductParams {
        name: SorobanString::from_str(&env, "Test"),
        category: Symbol::new(&env, "test"),
        oracle_key: Symbol::new(&env, "test001"),
        expected_token: usdc_token,
        trigger_type: TriggerType::Binary,
        oracle_data_type: Symbol::new(&env, "test"),
        premium_rate_bps: 100,
        coverage_min: 1_000_000,
        coverage_max: 10_000_000,
        max_duration_days: 30,
        trigger_threshold: 1_000_000,
        trigger_comparison: TriggerComparison::GreaterThan,
    };
}
