//! Product category validation (issue #549).
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
fn every_supported_category_is_accepted() {
    let (_, admin, client) = setup();
    client.create_product(&admin, &params(symbol_short!("p_crop"), symbol_short!("crop"), symbol_short!("key_a")));
    client.create_product(&admin, &params(symbol_short!("p_flight"), symbol_short!("flight"), symbol_short!("key_b")));
    client.create_product(&admin, &params(symbol_short!("p_disast"), symbol_short!("disaster"), symbol_short!("key_c")));
    client.create_product(&admin, &params(symbol_short!("p_health"), symbol_short!("health"), symbol_short!("key_d")));
    client.create_product(&admin, &params(symbol_short!("p_defi"), symbol_short!("defi"), symbol_short!("key_e")));
}

#[test]
#[should_panic(expected = "Error(Contract, #36)")]
fn unknown_category_is_rejected() {
    let (_, admin, client) = setup();
    client.create_product(&admin, &params(symbol_short!("p_bad"), symbol_short!("invalid"), symbol_short!("key_a")));
}
