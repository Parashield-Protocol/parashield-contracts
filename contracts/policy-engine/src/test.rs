use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, Client as TokenClient},
    Env,
};

const COVERAGE: i128 = 1_000_000_000; // 100 USDC (7-decimal)
// const PREMIUM:  i128 =    50_000_000; //   5 USDC (5% rate) - now computed with duration

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
        oracle_key:         symbol_short!("kis2606"),
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
fn test_initialize_accepts_any_address_type() {
    // Soroban's Address type is inherently valid; no prefix validation needed.
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let usdc = env.register_stellar_asset_contract_v2(Address::generate(&env)).address();
    let oracle = env.register_stellar_asset_contract_v2(Address::generate(&env)).address();
    let contract_id = env.register(PolicyEngine, ());
    PolicyEngineClient::new(&env, &contract_id)
        .initialize(&admin, &usdc, &oracle);
    assert_eq!(PolicyEngineClient::new(&env, &contract_id).get_admin(), admin);
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

    let duration_days = 30u32;
    let expected_premium = COVERAGE
        .checked_mul(500i128) // premium_rate_bps = 500
        .expect("overflow")
        .checked_mul(duration_days as i128)
        .expect("overflow")
        .checked_div(365)
        .expect("div by zero")
        .checked_div(10_000)
        .expect("div by zero");
    client.buy_policy(&buyer, &pid, &COVERAGE, &duration_days, &symbol_short!("kis2606"));

    let buyer_after   = TokenClient::new(&env, &usdc).balance(&buyer);
    let contract_bal  = client.get_contract_balance();

    assert_eq!(buyer_before - buyer_after, expected_premium);
    assert_eq!(contract_bal, expected_premium);
}

#[test]
fn test_buy_policy_records_correct_fields() {
    let (env, admin, _oracle, usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    let pid    = create_crop_product(&env, &client, &admin);

    let buyer = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &1_000_000_000i128);

    let duration_days = 30u32;
    let expected_premium = COVERAGE
        .checked_mul(500i128) // premium_rate_bps = 500
        .expect("overflow")
        .checked_mul(duration_days as i128)
        .expect("overflow")
        .checked_div(365)
        .expect("div by zero")
        .checked_div(10_000)
        .expect("div by zero");

    env.ledger().with_mut(|l| l.timestamp = 1_748_736_000); // fixed timestamp

    let policy_id = client.buy_policy(&buyer, &pid, &COVERAGE, &duration_days, &symbol_short!("kis2606"));
    let policy    = client.get_policy(&policy_id);

    assert_eq!(policy.policyholder, buyer);
    assert_eq!(policy.coverage_amount, COVERAGE);
    assert_eq!(policy.premium_paid, expected_premium);
    assert_eq!(policy.status, PolicyStatus::Active);
    assert_eq!(policy.end_time, 1_748_736_000 + duration_days * 86_400);
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

    let policies = client.get_user_policies(&buyer, &0u32, &10u32);
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

// ── Re-entrancy / double-processing guard (Issue #1) ─────────────────────────

/// pay_claim on an already-claimed policy must return AlreadyClaimed error,
/// preventing double-payout (re-entrancy guard via state transition check).
#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_double_pay_claim_panics_with_already_claimed() {
    let (env, admin, _oracle, usdc, contract_id) = setup();
    let client   = PolicyEngineClient::new(&env, &contract_id);
    let pid      = create_crop_product(&env, &client, &admin);
    let buyer    = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &10_000_000_000i128);
    // Pre-fund the contract with coverage capital
    StellarAssetClient::new(&env, &usdc).mint(&contract_id, &10_000_000_000i128);

    let claims_processor = Address::generate(&env);
    client.set_claims_processor(&admin, &claims_processor);

    let policy_id = client.buy_policy(&buyer, &pid, &COVERAGE, &30u32, &symbol_short!("kis2606"));

    // First call: succeeds (Active → Claimed)
    client.pay_claim(&claims_processor, &policy_id);
    // Second call: must panic with AlreadyClaimed (#11)
    client.pay_claim(&claims_processor, &policy_id);
}

/// expire_policy on an already-expired policy must return AlreadyExpired error.
#[test]
#[should_panic(expected = "Error(Contract, #12)")]
fn test_double_expire_policy_panics_with_already_expired() {
    let (env, admin, _oracle, usdc, contract_id) = setup();
    let client   = PolicyEngineClient::new(&env, &contract_id);
    let pid      = create_crop_product(&env, &client, &admin);
    let buyer    = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &10_000_000_000i128);

    let claims_processor = Address::generate(&env);
    client.set_claims_processor(&admin, &claims_processor);

    let policy_id = client.buy_policy(&buyer, &pid, &COVERAGE, &30u32, &symbol_short!("kis2606"));

    // First call: succeeds (Active → Expired)
    client.expire_policy(&claims_processor, &policy_id);
    // Second call: must panic with AlreadyExpired (#12)
    client.expire_policy(&claims_processor, &policy_id);
}

// ── Premium transfer verification (Issue #2) ─────────────────────────────────

/// buy_policy without sufficient USDC balance must revert — free policies impossible.
#[test]
#[should_panic]
fn test_buy_policy_without_funds_panics() {
    let (env, admin, _oracle, _usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    let pid    = create_crop_product(&env, &client, &admin);

    // Buyer has zero USDC — no mint, no approval
    let broke_buyer = Address::generate(&env);
    client.buy_policy(&broke_buyer, &pid, &COVERAGE, &30u32, &symbol_short!("kis2606"));
}

// ── trigger_threshold bounds checking (Issue #4) ──────────────────────────────

/// trigger_threshold of zero must be rejected with InvalidTriggerThreshold (#14).
#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_create_product_zero_threshold_panics() {
    let (env, admin, _oracle, _usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    client.create_product(&admin, &CreateProductParams {
        name:               symbol_short!("bad"),
        category:           symbol_short!("crop"),
        oracle_key:         symbol_short!("kis2606"),
        trigger_type:       TriggerType::Threshold,
        oracle_data_type:   symbol_short!("weather"),
        trigger_threshold:  0i128,  // ← invalid: zero
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min:       100_000_000,
        coverage_max:       10_000_000_000,
        premium_rate_bps:   500,
        max_duration_days:  365,
    });
}

/// Negative trigger_threshold must be rejected with InvalidTriggerThreshold (#14).
#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_create_product_negative_threshold_panics() {
    let (env, admin, _oracle, _usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    client.create_product(&admin, &CreateProductParams {
        name:               symbol_short!("bad"),
        category:           symbol_short!("crop"),
        oracle_key:         symbol_short!("kis2606"),
        trigger_type:       TriggerType::Threshold,
        oracle_data_type:   symbol_short!("weather"),
        trigger_threshold:  -1i128,  // ← invalid: negative
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min:       100_000_000,
        coverage_max:       10_000_000_000,
        premium_rate_bps:   500,
        max_duration_days:  365,
    });
}

/// Absurdly large trigger_threshold must be rejected with InvalidTriggerThreshold (#14).
#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_create_product_overflow_threshold_panics() {
    let (env, admin, _oracle, _usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    client.create_product(&admin, &CreateProductParams {
        name:               symbol_short!("bad"),
        category:           symbol_short!("crop"),
        oracle_key:         symbol_short!("kis2606"),
        trigger_type:       TriggerType::Threshold,
        oracle_data_type:   symbol_short!("weather"),
        trigger_threshold:  i128::MAX,  // ← invalid: overflows protocol range
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min:       100_000_000,
        coverage_max:       10_000_000_000,
        premium_rate_bps:   500,
        max_duration_days:  365,
    });
}

// ── Product category/oracle_key uniqueness (Issue #10) ───────────────────────────

/// Creating a product with duplicate (category, oracle_key) must panic with DuplicateProductKey (#15).
#[test]
#[should_panic(expected = "Error(Contract, #15)")]
fn test_duplicate_category_oracle_key_panics() {
    let (env, admin, _oracle, _usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);

    // Create product A with (category: "crop", oracle_key: "kis2606")
    client.create_product(&admin, &CreateProductParams {
        name:               symbol_short!("crop_k_a"),
        category:           symbol_short!("crop"),
        oracle_key:         symbol_short!("kis2606"),
        trigger_type:       TriggerType::Threshold,
        oracle_data_type:   symbol_short!("weather"),
        trigger_threshold:  50_000_000,
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min:       100_000_000,
        coverage_max:       10_000_000_000,
        premium_rate_bps:   500,
        max_duration_days:  365,
    });

    // Attempt to create product B with same (category, oracle_key) — should panic
    client.create_product(&admin, &CreateProductParams {
        name:               symbol_short!("crop_k_b"),
        category:           symbol_short!("crop"),
        oracle_key:         symbol_short!("kis2606"),  // ← duplicate key
        trigger_type:       TriggerType::Threshold,
        oracle_data_type:   symbol_short!("weather"),
        trigger_threshold:  60_000_000,
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min:       100_000_000,
        coverage_max:       10_000_000_000,
        premium_rate_bps:   600,
        max_duration_days:  365,
    });
}

/// Creating products with different oracle_keys but same category should succeed.
#[test]
fn test_different_oracle_keys_same_category_succeeds() {
    let (env, admin, _oracle, _usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);

    // Create product A with (category: "crop", oracle_key: "kis2606")
    client.create_product(&admin, &CreateProductParams {
        name:               symbol_short!("crop_kis"),
        category:           symbol_short!("crop"),
        oracle_key:         symbol_short!("kis2606"),
        trigger_type:       TriggerType::Threshold,
        oracle_data_type:   symbol_short!("weather"),
        trigger_threshold:  50_000_000,
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min:       100_000_000,
        coverage_max:       10_000_000_000,
        premium_rate_bps:   500,
        max_duration_days:  365,
    });

    // Create product B with (category: "crop", oracle_key: "nak2607") — different key, should succeed
    client.create_product(&admin, &CreateProductParams {
        name:               symbol_short!("crop_nak"),
        category:           symbol_short!("crop"),
        oracle_key:         symbol_short!("nak2607"),  // ← different key
        trigger_type:       TriggerType::Threshold,
        oracle_data_type:   symbol_short!("weather"),
        trigger_threshold:  60_000_000,
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min:       100_000_000,
        coverage_max:       10_000_000_000,
        premium_rate_bps:   600,
        max_duration_days:  365,
    });

    assert_eq!(client.get_active_products().len(), 2);
}

/// After deprecating a product, its (category, oracle_key) can be reused.
#[test]
fn test_deprecated_product_key_can_be_reused() {
    let (env, admin, _oracle, _usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);

    // Create product A with (category: "crop", oracle_key: "kis2606")
    let product_a_id = client.create_product(&admin, &CreateProductParams {
        name:               symbol_short!("crop_k_v1"),
        category:           symbol_short!("crop"),
        oracle_key:         symbol_short!("kis2606"),
        trigger_type:       TriggerType::Threshold,
        oracle_data_type:   symbol_short!("weather"),
        trigger_threshold:  50_000_000,
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min:       100_000_000,
        coverage_max:       10_000_000_000,
        premium_rate_bps:   500,
        max_duration_days:  365,
    });

    // Deprecate product A
    client.deprecate_product(&admin, &product_a_id);

    // Create product B with same (category, oracle_key) — should succeed after deprecation
    client.create_product(&admin, &CreateProductParams {
        name:               symbol_short!("crop_k_v2"),
        category:           symbol_short!("crop"),
        oracle_key:         symbol_short!("kis2606"),  // ← reused key
        trigger_type:       TriggerType::Threshold,
        oracle_data_type:   symbol_short!("weather"),
        trigger_threshold:  60_000_000,
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min:       100_000_000,
        coverage_max:       10_000_000_000,
        premium_rate_bps:   600,
        max_duration_days:  365,
    });

    assert_eq!(client.get_active_products().len(), 1);
    }

    #[test]
    fn test_premium_matches_formula() {
        let (env, admin, _oracle, usdc, contract_id) = setup();
#[test]
fn test_premium_matches_formula() {
    let (env, admin, _oracle, usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);
    let pid = create_crop_product(&env, &client, &admin);

    let buyer = Address::generate(&env);
    // 1000 USDC in stroops
    let coverage = 10_000_000_000i128;
    let rate_bps = 1000;
    let duration_days = 30u32;
    let expected_premium = coverage
        .checked_mul(rate_bps as i128)
        .expect("overflow")
        .checked_mul(duration_days as i128)
        .expect("overflow")
        .checked_div(365)
        .expect("div by zero")
        .checked_div(10_000)
        .expect("div by zero");
    // Fund buyer with expected premium plus some extra
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &expected_premium + 1_000_000_000i128);

    let buyer_before = TokenClient::new(&env, &usdc).balance(&buyer);

    client.buy_policy(&buyer, &pid, &coverage, &duration_days, &symbol_short!("kis2606"));

    let buyer_after = TokenClient::new(&env, &usdc).balance(&buyer);
    let contract_bal = client.get_contract_balance();

    assert_eq!(buyer_before - buyer_after, expected_premium);
    assert_eq!(contract_bal, expected_premium);
}
}

// ── Issue #58: atomic product ID generation ────────────────────────────────

#[test]
fn sequential_create_product_ids_are_unique_and_monotone() {
    let (env, admin, _oracle, _usdc, contract_id) = setup();
    let client = PolicyEngineClient::new(&env, &contract_id);

    let params = |n: u32| CreateProductParams {
        name:               symbol_short!("prod"),
        category:           symbol_short!("crop"),
        oracle_key:         symbol_short!("key"),
        trigger_type:       TriggerType::Threshold,
        oracle_data_type:   symbol_short!("weather"),
        trigger_threshold:  50_000_000,
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min:       100_000_000,
        coverage_max:       10_000_000_000,
        // Each call needs a distinct (category, oracle_key) pair to avoid DuplicateProductKey
        premium_rate_bps:   500 + n,
        max_duration_days:  365,
    };

    // Use distinct oracle_key per product to avoid DuplicateProductKey error
    let id1 = client.create_product(&admin, &CreateProductParams {
        oracle_key: symbol_short!("k1"), ..params(0)
    });
    let id2 = client.create_product(&admin, &CreateProductParams {
        oracle_key: symbol_short!("k2"), ..params(1)
    });
    let id3 = client.create_product(&admin, &CreateProductParams {
        oracle_key: symbol_short!("k3"), ..params(2)
    });

    // IDs must be unique
    assert_ne!(id1, id2);
    assert_ne!(id2, id3);
    // IDs must be monotonically increasing (atomic increment property)
    assert!(id2 > id1);
    assert!(id3 > id2);
}
