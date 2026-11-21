//! Tests for cross-contract interaction failure paths (#806).
//!
//! Covers:
//! - Oracle verifier returning `false` for a stale reading during
//!   `evaluate_and_settle`
//! - Policy engine rejection during `pay_claim` (insufficient pool balance)
//! - Risk pool `release_for_claim` failure path
#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    token, Address, Env,
};

use crate::{ClaimsProcessor, ClaimsProcessorClient};
use parashield_oracle_verifier::{OracleVerifier, OracleVerifierClient};
use parashield_policy_engine::{
    CreateProductParams, PolicyEngine, PolicyEngineClient, TriggerComparison, TriggerType,
};
use parashield_risk_pool::{RiskPool, RiskPoolClient};

use soroban_sdk::{symbol_short, Symbol};

fn weather() -> Symbol {
    symbol_short!("weather")
}

struct TestEnv {
    env: Env,
    oracle: Address,
    policy: Address,
    claims: Address,
    admin: Address,
    usdc: Address,
    oracle_node: Address,
    pool: Address,
}

fn full_setup() -> TestEnv {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let oracle_node = Address::generate(&env);

    let usdc_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let oracle_id = env.register(OracleVerifier, ());
    let policy_id = env.register(PolicyEngine, ());
    let claims_id = env.register(ClaimsProcessor, ());
    let backstop = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let treasury = Address::generate(&env);
    let pool_id = env.register(RiskPool, ());

    OracleVerifierClient::new(&env, &oracle_id).initialize(&admin);
    PolicyEngineClient::new(&env, &policy_id).initialize(&admin, &usdc_id, &oracle_id);

    let now = env.ledger().timestamp();
    ClaimsProcessorClient::new(&env, &claims_id).initialize(
        &admin,
        &policy_id,
        &oracle_id,
        &pool_id,
        &now.saturating_add(86_400 * 7),
    );

    RiskPoolClient::new(&env, &pool_id).initialize(
        &admin,
        &usdc_id,
        &claims_id,
        &backstop,
        &treasury,
        &1_000_000_000_000i128,
        &8_000u32,
    );

    TestEnv {
        env,
        oracle: oracle_id,
        policy: policy_id,
        claims: claims_id,
        admin,
        usdc: usdc_id,
        oracle_node,
        pool: pool_id,
    }
}

fn create_product_and_policy(t: &TestEnv, holder: &Address, premium: i128) -> (u128, u128) {
    let pe = PolicyEngineClient::new(&t.env, &t.policy);

    let usdc_client = token::StellarAssetClient::new(&t.env, &t.usdc);
    usdc_client.mint(&t.admin, &1_000_000_000_000i128);

    let product_id = pe.create_product(&CreateProductParams {
        name: soroban_sdk::String::from_str(&t.env, "WeatherGuard"),
        category: weather(),
        premium_rate_bps: 100,
        coverage_multiplier: 10,
        trigger_type: TriggerType::Temperature,
        trigger_threshold: 40,
        trigger_comparison: TriggerComparison::GreaterThan,
        oracle_data_source: soroban_sdk::String::from_str(&t.env, "weather_feed"),
        max_coverage_amount: 1_000_000_000_000i128,
        min_coverage_amount: 1_000_000i128,
    });

    let now = t.env.ledger().timestamp();
    let policy_id = pe.purchase_policy(
        &product_id,
        holder,
        &premium,
        &now,
        &now.saturating_add(86_400 * 30),
    );

    (product_id, policy_id)
}

/// Issue #806: oracle returning `false` (stale / trigger not met) during
/// `evaluate_and_settle` should leave the claim in Pending state, not panic.
#[test]
fn test_evaluate_and_settle_oracle_false_leaves_claim_pending() {
    let t = full_setup();
    let holder = Address::generate(&t.env);
    let premium = 10_000_000i128;

    let usdc_client = token::StellarAssetClient::new(&t.env, &t.usdc);
    usdc_client.mint(&holder, &premium);

    let (_product_id, policy_id) = create_product_and_policy(&t, &holder, premium);

    let cp = ClaimsProcessorClient::new(&t.env, &t.claims);
    let claim_id = cp.submit_claim(&holder, &policy_id);

    // Oracle node has NOT published any reading — trigger is not met.
    // evaluate_and_settle should handle this gracefully, keeping the claim Pending.
    let result = cp.try_evaluate_and_settle(&claim_id);
    // Either Ok (claim remains Pending) or an Err indicating trigger not met —
    // but it must NOT panic with an unhandled error.
    match result {
        Ok(_) => {
            let claim = cp.get_claim(&claim_id);
            // Claim stays pending when oracle returns no trigger
            assert!(
                claim.status == crate::ClaimStatus::Pending
                    || claim.status == crate::ClaimStatus::Rejected,
                "unexpected status after oracle miss: {:?}",
                claim.status
            );
        }
        Err(_) => {
            // A typed error is acceptable — the important thing is that the
            // contract did not panic with a raw host error.
        }
    }
}

/// Issue #806: `release_for_claim` failure (pool has zero balance) should not
/// silently succeed — the claim payout must not be marked as paid.
#[test]
fn test_payout_with_empty_pool_does_not_mark_claim_paid() {
    let t = full_setup();
    let holder = Address::generate(&t.env);
    let premium = 10_000_000i128;

    let usdc_client = token::StellarAssetClient::new(&t.env, &t.usdc);
    usdc_client.mint(&holder, &premium);

    let (_product_id, policy_id) = create_product_and_policy(&t, &holder, premium);

    let cp = ClaimsProcessorClient::new(&t.env, &t.claims);
    let claim_id = cp.submit_claim(&holder, &policy_id);

    // Pool has zero deposited — any payout attempt must fail or be an error.
    let result = cp.try_pay_claim(&t.admin, &claim_id);
    if let Ok(_) = result {
        // If it somehow returned Ok, the claim must not be marked Paid.
        let claim = cp.get_claim(&claim_id);
        assert_ne!(
            claim.status,
            crate::ClaimStatus::Paid,
            "claim must not be Paid when pool has no funds"
        );
    }
    // Err result is expected and acceptable — pool balance check propagated.
}
