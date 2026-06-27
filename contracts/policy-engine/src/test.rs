use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, Client as TokenClient},
    Env,
};

const COVERAGE: i128 = 1_000_000_000; // 100 USDC (7-decimal)
const PREMIUM:  i128 =    50_000_000; //   5 USDC (5% rate)

fn setup() -> (Env, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin  = Address::generate(&env);
    let oracle = Address::generate(&env);

    // Deploy a real SAC test token for USDC
    let usdc = env.register_stellar_asset_contract_v2(admin.clone()).address();

    let contract_id = env.register(PolicyEngine, ());
    PolicyEngineClient::new(&env, &contract_id)
        .initialize(&admin, &usdc, &oracle);

    (env, admin, oracle, usdc, contract_id)
}

fn create_crop_product(_env: &Env, client: &PolicyEngineClient, admin: &Address) -> u128 {
    client.create_product(admin, &CreateProductParams {
        name:               symbol_short!("crop_kism"),
        category:           symbol_short!("crop"),
        trigger_type:       TriggerType::Threshold,
        oracle_data_type:   symbol_short!("weather"),
        trigger_threshold:  50_000_000,
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min:       100_000_000,
        coverage_max:       10_000_000_000,
        premium_rate_bps:   500,
        max_duration_days:  365,
    })
}

// ── Initialization ────────────────────────────────────────────────────────────

#[test]
fn test_initialize_sets_admin_and_oracle() {
    let (env, admin, oracle, _usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    assert_eq!(client.get_admin(), admin);
    assert_eq!(client.get_oracle(), oracle);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_double_initialize_panics() {
    let (env, admin, oracle, usdc, contract_id) = setup();
    PolicyEngineClient::new(&env, &contract_id).initialize(&admin, &usdc, &oracle);
}

// ── Product management ────────────────────────────────────────────────────────

#[test]
fn test_create_product_returns_id() {
    let (env, admin, _oracle, _usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    let id = create_crop_product(&env, &client, &admin);
    assert_eq!(id, 1);
    let products = client.get_active_products();
    assert_eq!(products.len(), 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_non_admin_cannot_create_product() {
    let (env, _admin, _oracle, _usdc, contract_id) = setup();
    let client   = PolicyEngineClient::new(&env, &contract_id);
    let impostor = Address::generate(&env);
    create_crop_product(&env, &client, &impostor);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_pause_product_blocks_purchase() {
    let (env, admin, _oracle, usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    let pid = create_crop_product(&env, &client, &admin);
    client.pause_product(&admin, &pid);

    let buyer = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &1_000_000_000i128);
    client.buy_policy(&buyer, &pid, &COVERAGE, &30u32, &symbol_short!("kis2606"));
}

// ── Policy purchase ───────────────────────────────────────────────────────────

#[test]
fn test_buy_policy_transfers_premium() {
    let (env, admin, _oracle, usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    let pid    = create_crop_product(&env, &client, &admin);

    let buyer = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &1_000_000_000i128);
    let buyer_before = TokenClient::new(&env, &usdc).balance(&buyer);

    client.buy_policy(&buyer, &pid, &COVERAGE, &30u32, &symbol_short!("kis2606"));

    let buyer_after   = TokenClient::new(&env, &usdc).balance(&buyer);
    let contract_bal  = client.get_contract_balance();

    assert_eq!(buyer_before - buyer_after, PREMIUM);
    assert_eq!(contract_bal, PREMIUM);
}

#[test]
fn test_buy_policy_records_correct_fields() {
    let (env, admin, _oracle, usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    let pid    = create_crop_product(&env, &client, &admin);

    let buyer = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &1_000_000_000i128);

    env.ledger().with_mut(|l| l.timestamp = 1_748_736_000); // fixed timestamp

    let policy_id = client.buy_policy(&buyer, &pid, &COVERAGE, &30u32, &symbol_short!("kis2606"));
    let policy    = client.get_policy(&policy_id);

    assert_eq!(policy.policyholder, buyer);
    assert_eq!(policy.coverage_amount, COVERAGE);
    assert_eq!(policy.premium_paid, PREMIUM);
    assert_eq!(policy.status, PolicyStatus::Active);
    assert_eq!(policy.end_time, 1_748_736_000 + 30 * 86_400);
}

#[test]
fn test_buy_policy_appears_in_user_list() {
    let (env, admin, _oracle, usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    let pid    = create_crop_product(&env, &client, &admin);

    let buyer = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &5_000_000_000i128);

    client.buy_policy(&buyer, &pid, &COVERAGE, &30u32, &symbol_short!("kis2606"));
    client.buy_policy(&buyer, &pid, &COVERAGE, &60u32, &symbol_short!("kis2607"));

    let policies = client.get_user_policies(&buyer);
    assert_eq!(policies.len(), 2);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_coverage_below_min_panics() {
    let (env, admin, _oracle, usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    let pid    = create_crop_product(&env, &client, &admin);

    let buyer = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &1_000_000_000i128);
    // coverage_min is 100_000_000; send 50_000_000 (5 USDC) — should panic
    client.buy_policy(&buyer, &pid, &50_000_000i128, &30u32, &symbol_short!("kis2606"));
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_duration_above_max_panics() {
    let (env, admin, _oracle, usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    let pid    = create_crop_product(&env, &client, &admin);

    let buyer = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &1_000_000_000i128);
    // max_duration_days is 365; request 400 days
    client.buy_policy(&buyer, &pid, &COVERAGE, &400u32, &symbol_short!("kis2606"));
}

// ── Policy cancellation ───────────────────────────────────────────────────────

#[test]
fn test_cancel_policy_refunds_premium() {
    let (env, admin, _oracle, usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    let pid    = create_crop_product(&env, &client, &admin);

    let buyer = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &1_000_000_000i128);
    let buyer_before = TokenClient::new(&env, &usdc).balance(&buyer);

    let policy_id = client.buy_policy(&buyer, &pid, &COVERAGE, &30u32, &symbol_short!("kis2606"));
    client.cancel_policy(&buyer, &policy_id);

    let buyer_after = TokenClient::new(&env, &usdc).balance(&buyer);
    assert_eq!(buyer_after, buyer_before); // premium returned in full
    assert_eq!(client.get_policy(&policy_id).status, PolicyStatus::Cancelled);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_non_policyholder_cannot_cancel() {
    let (env, admin, _oracle, usdc, contract_id) = setup();
    let client   = PolicyEngineClient::new(&env, &contract_id);
    let pid      = create_crop_product(&env, &client, &admin);
    let buyer    = Address::generate(&env);
    let impostor = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &1_000_000_000i128);
    let policy_id = client.buy_policy(&buyer, &pid, &COVERAGE, &30u32, &symbol_short!("kis2606"));
    client.cancel_policy(&impostor, &policy_id);
}
