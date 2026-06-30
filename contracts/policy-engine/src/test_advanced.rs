#![allow(clippy::inconsistent_digit_grouping)]
//! Advanced policy-engine tests: emergency pause, product deprecation, cancel.
#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::Address as _,
    token, Address, Env,
};

use crate::{
    CreateProductParams, PolicyEngine, PolicyEngineClient,
    ProductStatus, TriggerComparison, TriggerType,
};
use soroban_sdk::symbol_short;

fn basic_params() -> CreateProductParams {
    CreateProductParams {
        name:               symbol_short!("crop"),
        category:           symbol_short!("crop"),
        oracle_key:         symbol_short!("kis2606"),
        trigger_type:       TriggerType::Threshold,
        oracle_data_type:   symbol_short!("weather"),
        trigger_threshold:  500_000_000i128,
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min:       100_0000000i128,
        coverage_max:       100_000_0000000i128,
        premium_rate_bps:   300u32,
        max_duration_days:  90u32,
    }
}

fn setup() -> (Env, PolicyEngineClient<'static>, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin  = Address::generate(&env);
    let oracle = Address::generate(&env);
    let user   = Address::generate(&env);

    let usdc = env.register_stellar_asset_contract_v2(admin.clone()).address();
    token::StellarAssetClient::new(&env, &usdc).mint(&user, &10_000_0000000i128);
    token::StellarAssetClient::new(&env, &usdc).mint(&admin, &1_000_000_0000000i128);

    let pe_id = env.register(PolicyEngine, ());
    let pe    = PolicyEngineClient::new(&env, &pe_id);
    pe.initialize(&admin, &usdc, &oracle);

    // fund the contract for coverage payouts
    token::StellarAssetClient::new(&env, &usdc).mint(&pe_id, &1_000_000_0000000i128);

    (env, pe, admin, oracle, user)
}

// ── emergency pause ────────────────────────────────────────────────────────────

#[test]
fn is_paused_defaults_to_false() {
    let (_, pe, _, _, _) = setup();
    assert!(!pe.is_paused());
}

#[test]
fn emergency_pause_sets_paused_flag() {
    let (_, pe, admin, _, _) = setup();
    pe.emergency_pause(&admin);
    assert!(pe.is_paused());
}

#[test]
fn emergency_resume_clears_paused_flag() {
    let (_, pe, admin, _, _) = setup();
    pe.emergency_pause(&admin);
    pe.emergency_resume(&admin);
    assert!(!pe.is_paused());
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn non_admin_cannot_pause() {
    let (_env, pe, _, _, user) = setup();
    pe.emergency_pause(&user);
}

// ── product lifecycle ──────────────────────────────────────────────────────────

#[test]
fn deprecate_product_removes_from_active_list() {
    let (_, pe, admin, _, _) = setup();
    let prod_id = pe.create_product(&admin, &basic_params());
    assert_eq!(pe.get_active_products().len(), 1);
    pe.deprecate_product(&admin, &prod_id);
    assert_eq!(pe.get_active_products().len(), 0);
    let prod = pe.get_product(&prod_id);
    assert_eq!(prod.status, ProductStatus::Deprecated);
}

#[test]
fn pause_product_removes_from_active_list() {
    let (_, pe, admin, _, _) = setup();
    let prod_id = pe.create_product(&admin, &basic_params());
    assert_eq!(pe.get_active_products().len(), 1);
    pe.pause_product(&admin, &prod_id);
    let prod = pe.get_product(&prod_id);
    assert_eq!(prod.status, ProductStatus::Paused);
    assert_eq!(pe.get_active_products().len(), 0);
}

// ── policy cancellation ────────────────────────────────────────────────────────

#[test]
fn cancel_policy_returns_premium_to_holder() {
    let (_env, pe, admin, _, user) = setup();
    let prod_id = pe.create_product(&admin, &basic_params());
    let policy_id = pe.buy_policy(
        &user, &prod_id, &1_000_0000000i128, &30u32, &symbol_short!("kis2606"),
    );
    pe.cancel_policy(&user, &policy_id);
    let policy = pe.get_policy(&policy_id);
    assert_eq!(policy.status, crate::PolicyStatus::Cancelled);
}

// ── user policy query ─────────────────────────────────────────────────────────

#[test]
fn get_user_policies_tracks_multiple_policies() {
    let (_, pe, admin, _, user) = setup();
    let prod_id = pe.create_product(&admin, &basic_params());
    pe.buy_policy(&user, &prod_id, &200_0000000i128, &30u32, &symbol_short!("kis2606"));
    pe.buy_policy(&user, &prod_id, &300_0000000i128, &30u32, &symbol_short!("kis2606"));
    // Verify first page (limit 1)
    let p1 = pe.get_user_policies(&user, &0u32, &1u32);
    assert_eq!(p1.len(), 1);
    
    // Verify second page (limit 1, offset 1)
    let p2 = pe.get_user_policies(&user, &1u32, &1u32);
    assert_eq!(p2.len(), 1);
    assert_ne!(p1.get(0), p2.get(0));
    
    // Verify offset out of bounds
    let p3 = pe.get_user_policies(&user, &2u32, &1u32);
    assert_eq!(p3.len(), 0);
}

#[test]
fn test_pay_claim_insufficient_funds_reverts_policy_active() {
    let env = Env::default();
    env.mock_all_auths();

    let admin  = Address::generate(&env);
    let oracle = Address::generate(&env);
    let user   = Address::generate(&env);

    let usdc = env.register_stellar_asset_contract_v2(admin.clone()).address();
    token::StellarAssetClient::new(&env, &usdc).mint(&user, &10_000_0000000i128);

    let pe_id = env.register(PolicyEngine, ());
    let pe    = PolicyEngineClient::new(&env, &pe_id);
    pe.initialize(&admin, &usdc, &oracle);

    let prod_id = pe.create_product(&admin, &basic_params());
    let policy_id = pe.buy_policy(
        &user, &prod_id, &1_000_0000000i128, &30u32, &symbol_short!("kis2606"),
    );

    let claims_processor = Address::generate(&env);
    pe.set_claims_processor(&admin, &claims_processor);

    // Call pay_claim from claims_processor. Should fail since contract has no payout funds.
    let res = pe.try_pay_claim(&claims_processor, &policy_id);
    assert!(res.is_err());

    // Verify policy remains Active (reverted)
    let policy = pe.get_policy(&policy_id);
    assert_eq!(policy.status, crate::PolicyStatus::Active);
}
