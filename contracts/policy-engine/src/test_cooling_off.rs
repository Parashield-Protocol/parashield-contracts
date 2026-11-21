//! Cooling-off period between policy purchases (issue #671).
//!
//! A buyer must wait `PURCHASE_COOLING_OFF_SECONDS` between two purchases on
//! the same oracle key, so they cannot stack policies on a single oracle
//! reading in rapid succession.
#![cfg(test)]

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Env, Symbol,
};

const START: u64 = 1_748_736_000;

fn setup() -> (Env, Address, PolicyEngineClient<'static>, u128, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = START);
    let admin = Address::generate(&env);
    let usdc = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let id = env.register(PolicyEngine, ());
    let client = PolicyEngineClient::new(&env, &id);
    client.initialize(&admin, &usdc, &Address::generate(&env));

    let product_id = client.create_product(
        &admin,
        &CreateProductParams {
            name: symbol_short!("rain"),
            category: symbol_short!("crop"),
            oracle_key: symbol_short!("kis2606"),
            expected_token: usdc.clone(),
            trigger_type: TriggerType::Threshold,
            oracle_data_type: symbol_short!("weather"),
            trigger_threshold: 50_000_000,
            trigger_comparison: TriggerComparison::LessThan,
            coverage_min: 100_000_000,
            coverage_max: 10_000_000_000,
            premium_rate_bps: 500,
            max_duration_days: 365,
        },
    );

    let buyer = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &10_000_000_000i128);
    (env, usdc, client, product_id, buyer)
}

fn buy(
    client: &PolicyEngineClient,
    buyer: &Address,
    product_id: u128,
    oracle_key: Symbol,
    usdc: &Address,
) -> u128 {
    client.buy_policy(buyer, &product_id, &1_000_000_000i128, &30u32, &oracle_key, usdc)
}

fn advance(env: &Env, secs: u64) {
    env.ledger().with_mut(|l| l.timestamp += secs);
}

#[test]
#[should_panic(expected = "Error(Contract, #39)")]
fn second_purchase_same_key_is_rejected() {
    let (_env, usdc, client, pid, buyer) = setup();
    buy(&client, &buyer, pid, symbol_short!("kis2606"), &usdc);
    buy(&client, &buyer, pid, symbol_short!("kis2606"), &usdc);
}

#[test]
#[should_panic(expected = "Error(Contract, #39)")]
fn purchase_just_before_cooling_off_ends_is_rejected() {
    let (env, usdc, client, pid, buyer) = setup();
    buy(&client, &buyer, pid, symbol_short!("kis2606"), &usdc);
    advance(&env, PURCHASE_COOLING_OFF_SECONDS - 1);
    buy(&client, &buyer, pid, symbol_short!("kis2606"), &usdc);
}

#[test]
fn purchase_after_cooling_off_is_accepted() {
    let (env, usdc, client, pid, buyer) = setup();
    let first = buy(&client, &buyer, pid, symbol_short!("kis2606"), &usdc);
    advance(&env, PURCHASE_COOLING_OFF_SECONDS);
    let second = buy(&client, &buyer, pid, symbol_short!("kis2606"), &usdc);
    assert_ne!(first, second);
}

#[test]
fn different_oracle_keys_are_independent() {
    let (_env, usdc, client, pid, buyer) = setup();
    buy(&client, &buyer, pid, symbol_short!("kis2606"), &usdc);
    buy(&client, &buyer, pid, symbol_short!("kis2607"), &usdc);
}

#[test]
fn different_buyers_are_independent() {
    let (env, usdc, client, pid, buyer) = setup();
    let other = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&other, &10_000_000_000i128);
    buy(&client, &buyer, pid, symbol_short!("kis2606"), &usdc);
    buy(&client, &other, pid, symbol_short!("kis2606"), &usdc);
}

#[test]
#[should_panic(expected = "Error(Contract, #39)")]
fn batch_cannot_stack_same_key() {
    let (env, usdc, client, pid, buyer) = setup();
    let item = BatchBuyItem {
        product_id: pid,
        coverage_amount: 1_000_000_000i128,
        duration_days: 30u32,
        oracle_key: symbol_short!("kis2606"),
        payment_token: usdc.clone(),
    };
    client.batch_buy_policy(&buyer, &soroban_sdk::vec![&env, item.clone(), item]);
}

#[test]
#[should_panic(expected = "Error(Contract, #39)")]
fn scheduled_purchase_does_not_bypass_cooling_off() {
    let (_env, usdc, client, pid, buyer) = setup();
    client.buy_policy_scheduled(
        &buyer,
        &pid,
        &1_000_000_000i128,
        &30u32,
        &symbol_short!("kis2606"),
        &(START + 10 * PURCHASE_COOLING_OFF_SECONDS),
        &usdc,
    );
    buy(&client, &buyer, pid, symbol_short!("kis2606"), &usdc);
}

// ── Scheduled policy start time validation (issue #856) ─────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #19)")]
fn buy_policy_scheduled_rejects_past_start_time() {
    let (env, usdc, client, pid, buyer) = setup();
    // Advance time past START
    advance(&env, 100);
    // Try to schedule a policy with start_time in the past (START is now < current time)
    client.buy_policy_scheduled(
        &buyer,
        &pid,
        &1_000_000_000i128,
        &30u32,
        &symbol_short!("kis2606"),
        &START,  // This is in the past relative to current ledger time
        &usdc,
    );
}

#[test]
fn buy_policy_scheduled_accepts_current_time() {
    let (env, usdc, client, pid, buyer) = setup();
    let now = env.ledger().timestamp();
    let policy_id = client.buy_policy_scheduled(
        &buyer,
        &pid,
        &1_000_000_000i128,
        &30u32,
        &symbol_short!("kis2606"),
        &now,
        &usdc,
    );
    assert!(policy_id > 0);
}

#[test]
fn buy_policy_scheduled_accepts_future_start() {
    let (env, usdc, client, pid, buyer) = setup();
    let future = env.ledger().timestamp() + 86400; // 1 day in future
    let policy_id = client.buy_policy_scheduled(
        &buyer,
        &pid,
        &1_000_000_000i128,
        &30u32,
        &symbol_short!("kis2606"),
        &future,
        &usdc,
    );
    assert!(policy_id > 0);
}
