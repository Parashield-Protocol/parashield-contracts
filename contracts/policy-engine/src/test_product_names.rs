//! Product name uniqueness (issue #514).
#![cfg(test)]

use super::*;
use soroban_sdk::{symbol_short, testutils::Address as _, Env, Symbol};

fn setup() -> (Env, Address, PolicyEngineClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let usdc = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let id = env.register(PolicyEngine, ());
    let client = PolicyEngineClient::new(&env, &id);
    client.initialize(&admin, &usdc, &Address::generate(&env));
    (env, admin, client)
}

fn params(name: Symbol, category: Symbol, oracle_key: Symbol) -> CreateProductParams {
    CreateProductParams {
        name,
        category,
        oracle_key,
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
fn distinct_names_are_accepted() {
    let (_, admin, client) = setup();
    let a = client.create_product(&admin, &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")));
    let b = client.create_product(&admin, &params(symbol_short!("drought"), symbol_short!("crop"), symbol_short!("key_b")));
    assert_ne!(a, b);
}

#[test]
#[should_panic(expected = "Error(Contract, #34)")]
fn duplicate_name_same_category_is_rejected() {
    let (_, admin, client) = setup();
    client.create_product(&admin, &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")));
    client.create_product(&admin, &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_b")));
}

#[test]
#[should_panic(expected = "Error(Contract, #34)")]
fn duplicate_name_across_categories_is_rejected() {
    let (_, admin, client) = setup();
    client.create_product(&admin, &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")));
    client.create_product(&admin, &params(symbol_short!("rain"), symbol_short!("flight"), symbol_short!("key_b")));
}

#[test]
fn rejected_duplicate_does_not_consume_a_slot_or_id() {
    let (_, admin, client) = setup();
    let first = client.create_product(&admin, &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")));
    let dup = client.try_create_product(&admin, &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_b")));
    assert!(dup.is_err());
    // The failed attempt must not have reserved the oracle key or an id.
    let next = client.create_product(&admin, &params(symbol_short!("hail"), symbol_short!("crop"), symbol_short!("key_b")));
    assert_eq!(next, first + 1);
}
