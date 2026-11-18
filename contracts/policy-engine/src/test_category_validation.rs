//! Product category validation and registry tests (issue #549, #682).
#![cfg(test)]

use super::*;
use soroban_sdk::{symbol_short, testutils::Address as _, Env, Symbol};

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

fn params(name: Symbol, category: Symbol, oracle_key: Symbol, expected_token: Address) -> CreateProductParams {
    CreateProductParams {
        name,
        category,
        oracle_key,
        expected_token,
        trigger_type: TriggerType::Threshold,
        oracle_data_type: symbol_short!("weather"),
        trigger_threshold: 50_000_000,
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min: 100_000_000,
        coverage_max: 10_000_000_000,
        premium_rate_bps: 500,
        max_duration_days: 365,
    }
}

#[test]
fn every_supported_category_is_accepted() {
    let (_, admin, usdc, client) = setup();
    client.create_product(&admin, &params(symbol_short!("p_crop"), symbol_short!("crop"), symbol_short!("keya"), usdc.clone()));
    client.create_product(&admin, &params(symbol_short!("p_flight"), symbol_short!("flight"), symbol_short!("keyb"), usdc.clone()));
    client.create_product(&admin, &params(symbol_short!("p_disast"), symbol_short!("disaster"), symbol_short!("keyc"), usdc.clone()));
    client.create_product(&admin, &params(symbol_short!("p_health"), symbol_short!("health"), symbol_short!("keyd"), usdc.clone()));
    client.create_product(&admin, &params(symbol_short!("p_defi"), symbol_short!("defi"), symbol_short!("keye"), usdc));
}

#[test]
#[should_panic(expected = "Error(Contract, #36)")]
fn unknown_category_is_rejected() {
    let (_, admin, usdc, client) = setup();
    client.create_product(&admin, &params(symbol_short!("p_bad"), symbol_short!("invalid"), symbol_short!("keya"), usdc));
}

#[test]
fn dynamic_category_registration_and_creation() {
    let (_, admin, usdc, client) = setup();
    let new_cat = symbol_short!("marine");
    
    // Initially not registered
    assert!(!client.is_category_registered(&new_cat));
    let err = client.try_create_product(&admin, &params(symbol_short!("p_marine"), new_cat.clone(), symbol_short!("keym"), usdc.clone()));
    assert!(err.is_err(), "Creation with unregistered category should fail");

    // Admin registers category
    client.register_category(&admin, &new_cat);
    assert!(client.is_category_registered(&new_cat));

    // Now creation succeeds
    let pid = client.create_product(&admin, &params(symbol_short!("p_marine"), new_cat.clone(), symbol_short!("keym"), usdc.clone()));
    assert!(pid > 0);

    // Unregister category
    client.unregister_category(&admin, &new_cat);
    assert!(!client.is_category_registered(&new_cat));

    // Further product creation for this category is now rejected
    let err2 = client.try_create_product(&admin, &params(symbol_short!("p_m2"), new_cat, symbol_short!("keym2"), usdc));
    assert!(err2.is_err(), "Creation after unregistering category should fail");
}

#[test]
fn get_registered_categories_contains_defaults() {
    let (_, _, _, client) = setup();
    let categories = client.get_registered_categories();
    assert!(categories.len() >= 5);
}
