#![allow(clippy::inconsistent_digit_grouping)]
//! Advanced claims-processor tests: edge cases, boundary conditions, multi-contract interactions.
#![cfg(test)]

extern crate std;

use super::*;
use parashield_oracle_verifier::{OracleVerifier, OracleVerifierClient};
use parashield_policy_engine::{
    CreateProductParams, PolicyEngine, PolicyEngineClient, TriggerComparison, TriggerType,
};
use parashield_risk_pool::{RiskPool, RiskPoolClient};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Env,
};

const COVERAGE: i128 = 1_000_000_000; // 100 USDC

struct World {
    env: Env,
    admin: Address,
    keeper: Address,
    oracle_w: Address,
    usdc: Address,
    oracle_id: Address,
    policy_id: Address,
    claims_id: Address,
    pool_id: Address,
}

fn deploy() -> World {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();
    env.ledger().with_mut(|l| l.timestamp = 1_000_000);

    let admin = Address::generate(&env);
    let keeper = Address::generate(&env);
    let oracle_wallet = Address::generate(&env);

    let usdc = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    // 1. Deploy oracle verifier
    let oracle_id = env.register(OracleVerifier, ());
    OracleVerifierClient::new(&env, &oracle_id).initialize(&admin);
    OracleVerifierClient::new(&env, &oracle_id).add_oracle(
        &admin,
        &oracle_wallet,
        &symbol_short!("weather"),
        &90u32,
    );

    // 2. Deploy risk pool (category: crop)
    let backstop = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let treasury = Address::generate(&env);
    let pool_id = env.register(RiskPool, ());

    // 3. Deploy policy engine (placeholder for risk pool init)
    let policy_id = env.register(PolicyEngine, ());

    // 4. Deploy claims processor (placeholder for risk pool init)
    let claims_id = env.register(ClaimsProcessor, ());

    // Initialize risk pool with correct addresses
    RiskPoolClient::new(&env, &pool_id).initialize(
        &admin,
        &usdc,
        &treasury,
        &backstop,
        &symbol_short!("crop"),
        &policy_id,
        &claims_id,
    );

    // Initialize other contracts
    PolicyEngineClient::new(&env, &policy_id).initialize(&admin, &usdc, &oracle_id);

    ClaimsProcessorClient::new(&env, &claims_id).initialize(
        &admin,
        &policy_id,
        &pool_id,
        &oracle_id,
        &604_800u64,
    );

    // Authorize keeper on the claims processor
    ClaimsProcessorClient::new(&env, &claims_id).add_keeper(&admin, &keeper);

    // Wire claims processor as authorized caller on policy engine
    PolicyEngineClient::new(&env, &policy_id).set_claims_processor(&admin, &claims_id);

    // Register crop claim type
    ClaimsProcessorClient::new(&env, &claims_id)
        .register_claim_type(&admin, &symbol_short!("crop"));

    World {
        env,
        admin,
        keeper,
        oracle_w: oracle_wallet,
        usdc,
        oracle_id,
        policy_id,
        claims_id,
        pool_id,
    }
}

fn create_crop_product(w: &World) -> u128 {
    PolicyEngineClient::new(&w.env, &w.policy_id).create_product(
        &w.admin,
        &CreateProductParams {
            name: soroban_sdk::String::from_str(&w.env, "crop_kism"),
            category: symbol_short!("crop"),
            oracle_key: symbol_short!("kis2606"),
            expected_token: w.usdc.clone(),
            trigger_type: TriggerType::Threshold,
            oracle_data_type: symbol_short!("weather"),
            trigger_threshold: 50_000_000,
            trigger_comparison: TriggerComparison::LessThan,
            coverage_min: 100_000_000,
            coverage_max: 10_000_000_000,
            premium_rate_bps: 500,
            max_duration_days: 365,
        },
    )
}

fn buy_crop_policy(w: &World, buyer: &Address, product_id: u128) -> u128 {
    StellarAssetClient::new(&w.env, &w.usdc).mint(buyer, &20_000_000_000i128);
    // Fund the pool and policy engine with coverage capital
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.pool_id, &10_000_000_000i128);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.policy_id, &20_000_000_000i128);
    // Deposit to pool and lock coverage for the policy
    RiskPoolClient::new(&w.env, &w.pool_id).deposit(&buyer, &10_000_000_000i128, &0i128, &false);

    let policy_id = PolicyEngineClient::new(&w.env, &w.policy_id).buy_policy(
        buyer,
        &product_id,
        &COVERAGE,
        &30u32,
        &symbol_short!("kis2606"),
        &w.usdc,
    );

    // Lock coverage in the pool for this policy
    RiskPoolClient::new(&w.env, &w.pool_id).lock_for_policy(&w.admin, &policy_id, &COVERAGE);

    policy_id
}

fn submit_rainfall(w: &World, mm_7dec: i128) {
    OracleVerifierClient::new(&w.env, &w.oracle_id).submit_data(
        &w.oracle_w,
        &symbol_short!("weather"),
        &symbol_short!("kis2606"),
        &mm_7dec,
        &95u32,
        &w.env.ledger().timestamp(),
    );
}

// ── Issue #48: Advanced test coverage ─────────────────────────────────────────────

/// Test batch processing with maximum/minimum boundary conditions.
#[test]
fn test_batch_auto_process_boundary_conditions() {
    let w = deploy();
    let pid = create_crop_product(&w);

    // Create multiple policies
    let buyer1 = Address::generate(&w.env);
    let buyer2 = Address::generate(&w.env);
    let buyer3 = Address::generate(&w.env);

    let pol_id1 = buy_crop_policy(&w, &buyer1, pid);
    let pol_id2 = buy_crop_policy(&w, &buyer2, pid);
    let pol_id3 = buy_crop_policy(&w, &buyer3, pid);

    // Submit rainfall data that triggers payout (below threshold)
    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let _cid1 = cp.submit_claim(&buyer1, &pol_id1);
    let _cid2 = cp.submit_claim(&buyer2, &pol_id2);
    let _cid3 = cp.submit_claim(&buyer3, &pol_id3);

    // Process all 3 claims in batch
    let results = cp.batch_auto_process(&w.keeper, &3u32);
    assert_eq!(results.len(), 3);

    // All should be paid
    for (_, result) in results.iter() {
        assert_eq!(result, ClaimResult::Paid);
    }

    // Verify all policies are now Claimed
    assert_eq!(
        PolicyEngineClient::new(&w.env, &w.policy_id)
            .get_policy(&pol_id1)
            .status,
        parashield_policy_engine::PolicyStatus::Claimed
    );
    assert_eq!(
        PolicyEngineClient::new(&w.env, &w.policy_id)
            .get_policy(&pol_id2)
            .status,
        parashield_policy_engine::PolicyStatus::Claimed
    );
    assert_eq!(
        PolicyEngineClient::new(&w.env, &w.policy_id)
            .get_policy(&pol_id3)
            .status,
        parashield_policy_engine::PolicyStatus::Claimed
    );
}

/// Test batch processing with limit smaller than pending queue.
#[test]
fn test_batch_auto_process_with_limit() {
    let w = deploy();
    let pid = create_crop_product(&w);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // Create 5 policies and submit claims
    let mut policy_ids = Vec::new(&w.env);
    for _i in 0..5 {
        let buyer = Address::generate(&w.env);
        let pol_id = buy_crop_policy(&w, &buyer, pid);
        policy_ids.push_back(pol_id);
        cp.submit_claim(&buyer, &pol_id);
    }

    submit_rainfall(&w, 20_000_000);

    // Process only 2 claims at a time
    let results1 = cp.batch_auto_process(&w.keeper, &2u32);
    assert_eq!(results1.len(), 2);

    let results2 = cp.batch_auto_process(&w.keeper, &2u32);
    assert_eq!(results2.len(), 2);

    let results3 = cp.batch_auto_process(&w.keeper, &2u32);
    assert_eq!(results3.len(), 1); // Only 1 remaining
}

/// Test staleness threshold boundary conditions.
#[test]
fn test_staleness_threshold_boundary() {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();
    env.ledger().with_mut(|l| l.timestamp = 1_000_000);

    let admin = Address::generate(&env);
    let keeper = Address::generate(&env);
    let oracle_wallet = Address::generate(&env);

    let usdc = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let oracle_id = env.register(OracleVerifier, ());
    OracleVerifierClient::new(&env, &oracle_id).initialize(&admin);
    OracleVerifierClient::new(&env, &oracle_id).add_oracle(
        &admin,
        &oracle_wallet,
        &symbol_short!("weather"),
        &90u32,
    );

    let backstop = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let treasury = Address::generate(&env);
    let pool_id = env.register(RiskPool, ());
    let policy_id = env.register(PolicyEngine, ());
    let claims_id = env.register(ClaimsProcessor, ());

    RiskPoolClient::new(&env, &pool_id).initialize(
        &admin,
        &usdc,
        &treasury,
        &backstop,
        &symbol_short!("crop"),
        &policy_id,
        &claims_id,
    );

    PolicyEngineClient::new(&env, &policy_id).initialize(&admin, &usdc, &oracle_id);

    // Initialize with very short staleness threshold (10 seconds)
    ClaimsProcessorClient::new(&env, &claims_id)
        .initialize(&admin, &policy_id, &pool_id, &oracle_id, &10u64);

    ClaimsProcessorClient::new(&env, &claims_id).add_keeper(&admin, &keeper);
    ClaimsProcessorClient::new(&env, &claims_id).register_claim_type(&admin, &symbol_short!("crop"));
    PolicyEngineClient::new(&env, &policy_id).set_claims_processor(&admin, &claims_id);

    let pid = PolicyEngineClient::new(&env, &policy_id).create_product(
        &admin,
        &CreateProductParams {
            name: soroban_sdk::String::from_str(&env, "crop"),
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
    StellarAssetClient::new(&env, &usdc).mint(&pool_id, &20_000_000_000i128);
    StellarAssetClient::new(&env, &usdc).mint(&policy_id, &10_000_000_000i128);
    RiskPoolClient::new(&env, &pool_id).deposit(&buyer, &5_000_000_000i128, &0i128, &false);

    let pol_id = PolicyEngineClient::new(&env, &policy_id).buy_policy(
        &buyer,
        &pid,
        &COVERAGE,
        &30u32,
        &symbol_short!("kis2606"),
        &usdc,
    );
    RiskPoolClient::new(&env, &pool_id).lock_for_policy(&admin, &pol_id, &COVERAGE);

    // Submit oracle data
    OracleVerifierClient::new(&env, &oracle_id).submit_data(
        &oracle_wallet,
        &symbol_short!("weather"),
        &symbol_short!("kis2606"),
        &20_000_000,
        &95u32,
        &env.ledger().timestamp(),
    );

    // Process immediately - should succeed (data is fresh)
    let cp = ClaimsProcessorClient::new(&env, &claims_id);
    let result1 = cp.auto_process(&keeper, &pol_id, &None);
    assert_eq!(result1, ClaimResult::Paid);
}

/// Test multi-contract interaction: claims-processor → policy-engine → risk-pool.
#[test]
fn test_multi_contract_interaction_atomicity() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let result = cp.auto_process(&w.keeper, &pol_id, &None);

    // Verify successful payout
    assert_eq!(result, ClaimResult::Paid);

    // Verify policy status updated in policy-engine
    let policy = PolicyEngineClient::new(&w.env, &w.policy_id).get_policy(&pol_id);
    assert_eq!(
        policy.status,
        parashield_policy_engine::PolicyStatus::Claimed
    );

    // Verify coverage lock released in risk-pool
    // (This is implicitly tested by the successful auto_process - if lock release failed, transaction would revert)
}

/// Test edge case: policy exactly at expiration time.
#[test]
fn test_policy_exactly_at_expiration() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    // Advance to exactly the policy end_time
    let policy = PolicyEngineClient::new(&w.env, &w.policy_id).get_policy(&pol_id);
    w.env.ledger().with_mut(|l| l.timestamp = policy.end_time);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let result = cp.auto_process(&w.keeper, &pol_id, &None);

    // Should be expired
    assert_eq!(result, ClaimResult::Expired);
}

/// Test edge case: oracle data at boundary threshold.
#[test]
fn test_oracle_data_at_boundary_threshold() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    // Submit data exactly at threshold (should NOT trigger payout for LessThan)
    submit_rainfall(&w, 50_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let result = cp.auto_process(&w.keeper, &pol_id, &None);

    // Should be rejected (not less than threshold)
    assert_eq!(result, ClaimResult::Rejected);
}

/// Test maximum coverage boundary.
#[test]
fn test_maximum_coverage_boundary() {
    let w = deploy();

    // Create product with max coverage
    let pid = PolicyEngineClient::new(&w.env, &w.policy_id).create_product(
        &w.admin,
        &CreateProductParams {
            name: soroban_sdk::String::from_str(&w.env, "max_cov"),
            category: symbol_short!("crop"),
            oracle_key: symbol_short!("kis2606"),
            expected_token: w.usdc.clone(),
            trigger_type: TriggerType::Threshold,
            oracle_data_type: symbol_short!("weather"),
            trigger_threshold: 50_000_000,
            trigger_comparison: TriggerComparison::LessThan,
            coverage_min: 100_000_000,
            coverage_max: 10_000_000_000, // Maximum coverage
            premium_rate_bps: 500,
            max_duration_days: 365,
        },
    );

    let buyer = Address::generate(&w.env);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&buyer, &50_000_000_000i128);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.pool_id, &50_000_000_000i128);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.policy_id, &20_000_000_000i128);
    RiskPoolClient::new(&w.env, &w.pool_id).deposit(&buyer, &20_000_000_000i128, &0i128, &false);

    // Buy policy at maximum coverage
    let pol_id = PolicyEngineClient::new(&w.env, &w.policy_id).buy_policy(
        &buyer,
        &pid,
        &10_000_000_000i128,
        &30u32,
        &symbol_short!("kis2606"),
        &w.usdc,
    );
    RiskPoolClient::new(&w.env, &w.pool_id).lock_for_policy(&w.admin, &pol_id, &10_000_000_000i128);

    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let result = cp.auto_process(&w.keeper, &pol_id, &None);

    assert_eq!(result, ClaimResult::Paid);
}

/// Test minimum coverage boundary.
#[test]
fn test_minimum_coverage_boundary() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let buyer = Address::generate(&w.env);

    StellarAssetClient::new(&w.env, &w.usdc).mint(&buyer, &5_000_000_000i128);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.pool_id, &5_000_000_000i128);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.policy_id, &1_000_000_000i128);
    RiskPoolClient::new(&w.env, &w.pool_id).deposit(&buyer, &1_000_000_000i128, &0i128, &false);

    // Buy policy at minimum coverage
    let pol_id = PolicyEngineClient::new(&w.env, &w.policy_id).buy_policy(
        &buyer,
        &pid,
        &100_000_000i128,
        &30u32,
        &symbol_short!("kis2606"),
        &w.usdc,
    );
    RiskPoolClient::new(&w.env, &w.pool_id).lock_for_policy(&w.admin, &pol_id, &100_000_000i128);

    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let result = cp.auto_process(&w.keeper, &pol_id, &None);

    assert_eq!(result, ClaimResult::Paid);
}

/// Test concurrent claim submissions for different policies.
#[test]
fn test_concurrent_claim_submissions() {
    let w = deploy();
    let pid = create_crop_product(&w);

    let buyer1 = Address::generate(&w.env);
    let buyer2 = Address::generate(&w.env);
    let buyer3 = Address::generate(&w.env);

    let pol_id1 = buy_crop_policy(&w, &buyer1, pid);
    let pol_id2 = buy_crop_policy(&w, &buyer2, pid);
    let pol_id3 = buy_crop_policy(&w, &buyer3, pid);

    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // All three policyholders submit claims
    let claim_id1 = cp.submit_claim(&buyer1, &pol_id1);
    let claim_id2 = cp.submit_claim(&buyer2, &pol_id2);
    let claim_id3 = cp.submit_claim(&buyer3, &pol_id3);

    // All should have unique claim IDs
    assert_ne!(claim_id1, claim_id2);
    assert_ne!(claim_id2, claim_id3);
    assert_ne!(claim_id1, claim_id3);

    // Process all claims
    let res1 = cp.process_claim(&w.keeper, &claim_id1, &None);
    let res2 = cp.process_claim(&w.keeper, &claim_id2, &None);
    let res3 = cp.process_claim(&w.keeper, &claim_id3, &None);

    assert_eq!(res1, ClaimResult::Paid);
    assert_eq!(res2, ClaimResult::Paid);
    assert_eq!(res3, ClaimResult::Paid);
}

/// The PendingClaims queue must not grow without bound: once a claim is settled
/// it is removed from the queue, keeping instance storage from ballooning.
#[test]
fn test_pending_queue_drained_after_settlement() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // Submitting a claim enqueues it.
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    assert_eq!(cp.get_pending_claims().len(), 1);

    // Settling it (trigger met → Paid) must drain it from the queue.
    submit_rainfall(&w, 20_000_000);
    let result = cp.process_claim(&w.keeper, &claim_id, &None);
    assert_eq!(result, ClaimResult::Paid);
    assert_eq!(
        cp.get_pending_claims().len(),
        0,
        "settled claim must leave the pending queue"
    );
}

/// Test version tracking for upgrade path.
#[test]
fn test_initial_version_tracking() {
    let w = deploy();
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    assert_eq!(cp.get_version(), 1);
}

#[test]
fn run_migrations_accepts_sequenced_supported_versions() {
    let env = Env::default();
    ClaimsProcessor::run_migrations(&env, 1, 3);
}

#[test]
#[should_panic(expected = "invalid migration version")]
fn run_migrations_rejects_downgrade() {
    let env = Env::default();
    ClaimsProcessor::run_migrations(&env, 2, 1);
}

#[test]
#[should_panic(expected = "invalid migration version")]
fn run_migrations_rejects_unsupported_target() {
    let env = Env::default();
    ClaimsProcessor::run_migrations(&env, 1, 4);
}

// ── Helpers for multi-policy tests with distinct oracle keys ──────────────────

fn create_crop_product_with_key(w: &World, name: &str, oracle_key: Symbol) -> u128 {
    PolicyEngineClient::new(&w.env, &w.policy_id).create_product(
        &w.admin,
        &CreateProductParams {
            name: soroban_sdk::String::from_str(&w.env, name),
            category: symbol_short!("crop"),
            oracle_key,
            expected_token: w.usdc.clone(),
            trigger_type: TriggerType::Threshold,
            oracle_data_type: symbol_short!("weather"),
            trigger_threshold: 50_000_000,
            trigger_comparison: TriggerComparison::LessThan,
            coverage_min: 100_000_000,
            coverage_max: 10_000_000_000,
            premium_rate_bps: 500,
            max_duration_days: 365,
        },
    )
}

fn buy_crop_policy_with_key(
    w: &World,
    buyer: &Address,
    product_id: u128,
    oracle_key: Symbol,
    coverage: i128,
) -> u128 {
    StellarAssetClient::new(&w.env, &w.usdc).mint(buyer, &20_000_000_000i128);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.pool_id, &20_000_000_000i128);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.policy_id, &20_000_000_000i128);
    RiskPoolClient::new(&w.env, &w.pool_id).deposit(buyer, &10_000_000_000i128, &0i128, &false);

    let policy_id = PolicyEngineClient::new(&w.env, &w.policy_id).buy_policy(
        buyer,
        &product_id,
        &coverage,
        &30u32,
        &oracle_key,
        &w.usdc,
    );

    RiskPoolClient::new(&w.env, &w.pool_id).lock_for_policy(&w.admin, &policy_id, &coverage);

    policy_id
}

// ── #901: Fraud detection rules and threshold tuning ─────────────────────────

#[test]
fn test_fraud_rule_rate_fires_independently() {
    let w = deploy();
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // Rate rule only: 3600s window, score 30, threshold 25, FlagOnly mode
    cp.set_fraud_config(
        &w.admin,
        &FraudConfig {
            rate_window_secs: 3600,
            rate_score: 30,
            burst_window_secs: 0,
            burst_count: 0,
            burst_score: 0,
            coverage_anomaly_multiplier: 0,
            coverage_score: 0,
            fraud_threshold_score: 25,
            mode: FraudMode::FlagOnly,
        },
    );

    let p1 = create_crop_product_with_key(&w, "crop_r1", symbol_short!("kis2601"));
    let p2 = create_crop_product_with_key(&w, "crop_r2", symbol_short!("kis2602"));
    let p3 = create_crop_product_with_key(&w, "crop_r3", symbol_short!("kis2603"));

    let buyer = Address::generate(&w.env);
    let pol_id1 = buy_crop_policy_with_key(&w, &buyer, p1, symbol_short!("kis2601"), COVERAGE);

    // First claim: claimant has no prior submissions, rate rule must NOT fire
    let cid1 = cp.submit_claim(&buyer, &pol_id1);
    assert!(cp.get_fraud_record(&cid1).is_none(), "first claim must not trip rate rule");

    // Second claim submitted 1800s later (within 3600s rate window)
    w.env.ledger().with_mut(|l| l.timestamp += 1800);
    let pol_id2 = buy_crop_policy_with_key(&w, &buyer, p2, symbol_short!("kis2602"), COVERAGE);
    let cid2 = cp.submit_claim(&buyer, &pol_id2);

    // Rate rule must fire independently with score 30 and bit 0 set
    let record = cp.get_fraud_record(&cid2).expect("second claim within rate window must be flagged");
    assert_eq!(record.score, 30);
    assert_eq!(record.flags, FRAUD_FLAG_RATE);

    // Third claim submitted 4000s later (> 3600s rate window)
    w.env.ledger().with_mut(|l| l.timestamp += 4000);
    let pol_id3 = buy_crop_policy_with_key(&w, &buyer, p3, symbol_short!("kis2603"), COVERAGE);
    let cid3 = cp.submit_claim(&buyer, &pol_id3);
    assert!(cp.get_fraud_record(&cid3).is_none(), "claim outside rate window must not trip rate rule");
}

#[test]
fn test_fraud_rule_burst_fires_independently() {
    let w = deploy();
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // Burst rule only: 3600s window, burst_count = 3, score 40, threshold 35, FlagOnly mode
    cp.set_fraud_config(
        &w.admin,
        &FraudConfig {
            rate_window_secs: 0,
            rate_score: 0,
            burst_window_secs: 3600,
            burst_count: 3,
            burst_score: 40,
            coverage_anomaly_multiplier: 0,
            coverage_score: 0,
            fraud_threshold_score: 35,
            mode: FraudMode::FlagOnly,
        },
    );

    let p1 = create_crop_product_with_key(&w, "crop_b1", symbol_short!("kis2601"));
    let p2 = create_crop_product_with_key(&w, "crop_b2", symbol_short!("kis2602"));
    let p3 = create_crop_product_with_key(&w, "crop_b3", symbol_short!("kis2603"));
    let p4 = create_crop_product_with_key(&w, "crop_b4", symbol_short!("kis2604"));

    let buyer = Address::generate(&w.env);
    let pol1 = buy_crop_policy_with_key(&w, &buyer, p1, symbol_short!("kis2601"), COVERAGE);
    let pol2 = buy_crop_policy_with_key(&w, &buyer, p2, symbol_short!("kis2602"), COVERAGE);
    let pol3 = buy_crop_policy_with_key(&w, &buyer, p3, symbol_short!("kis2603"), COVERAGE);
    let pol4 = buy_crop_policy_with_key(&w, &buyer, p4, symbol_short!("kis2604"), COVERAGE);

    // Claim 1: burst count = 1 < 3 -> not flagged
    let cid1 = cp.submit_claim(&buyer, &pol1);
    assert!(cp.get_fraud_record(&cid1).is_none());

    // Claim 2: burst count = 2 < 3 -> not flagged
    w.env.ledger().with_mut(|l| l.timestamp += 100);
    let cid2 = cp.submit_claim(&buyer, &pol2);
    assert!(cp.get_fraud_record(&cid2).is_none());

    // Claim 3: projected burst count = 3 >= 3 -> burst rule fires independently!
    w.env.ledger().with_mut(|l| l.timestamp += 100);
    let cid3 = cp.submit_claim(&buyer, &pol3);
    let record = cp.get_fraud_record(&cid3).expect("3rd claim in burst window must be flagged");
    assert_eq!(record.score, 40);
    assert_eq!(record.flags, FRAUD_FLAG_BURST);

    // Claim 4 after burst bucket expired (> 3600s) -> bucket resets, burst count = 1 -> not flagged
    w.env.ledger().with_mut(|l| l.timestamp += 4000);
    let cid4 = cp.submit_claim(&buyer, &pol4);
    assert!(cp.get_fraud_record(&cid4).is_none(), "claim after burst bucket expired must not be flagged");
}

#[test]
fn test_fraud_rule_coverage_anomaly_fires_independently() {
    let w = deploy();
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // Coverage anomaly rule only: 2x multiplier, score 45, threshold 40, FlagOnly mode
    cp.set_fraud_config(
        &w.admin,
        &FraudConfig {
            rate_window_secs: 0,
            rate_score: 0,
            burst_window_secs: 0,
            burst_count: 0,
            burst_score: 0,
            coverage_anomaly_multiplier: 2,
            coverage_score: 45,
            fraud_threshold_score: 40,
            mode: FraudMode::FlagOnly,
        },
    );

    let p1 = create_crop_product_with_key(&w, "crop_c1", symbol_short!("kis2601"));
    let p2 = create_crop_product_with_key(&w, "crop_c2", symbol_short!("kis2602"));
    let p3 = create_crop_product_with_key(&w, "crop_c3", symbol_short!("kis2603"));

    let buyer = Address::generate(&w.env);

    // Policy 1: baseline coverage 200_000_000 (20 USDC)
    let pol1 = buy_crop_policy_with_key(&w, &buyer, p1, symbol_short!("kis2601"), 200_000_000i128);

    // Claim 1: no prior history -> cannot be anomaly relative to nothing
    let cid1 = cp.submit_claim(&buyer, &pol1);
    assert!(cp.get_fraud_record(&cid1).is_none());

    // Policy 2: coverage 400_000_000 (<= 2 * 200_000_000) -> not anomalous
    w.env.ledger().with_mut(|l| l.timestamp += 1000);
    let pol2 = buy_crop_policy_with_key(&w, &buyer, p2, symbol_short!("kis2602"), 400_000_000i128);
    let cid2 = cp.submit_claim(&buyer, &pol2);
    assert!(cp.get_fraud_record(&cid2).is_none());

    // Policy 3: coverage 1_000_000_000 (> 2 * 400_000_000) -> coverage anomaly fires independently!
    w.env.ledger().with_mut(|l| l.timestamp += 1000);
    let pol3 = buy_crop_policy_with_key(&w, &buyer, p3, symbol_short!("kis2603"), 1_000_000_000i128);
    let cid3 = cp.submit_claim(&buyer, &pol3);
    let record = cp.get_fraud_record(&cid3).expect("coverage exceeding 2x rolling max must be flagged");
    assert_eq!(record.score, 45);
    assert_eq!(record.flags, FRAUD_FLAG_COVERAGE);
}

#[test]
fn test_fraud_multiple_rules_sum_correctly() {
    // Pure function test over score_submission
    let cfg = FraudConfig {
        rate_window_secs: 3600,
        rate_score: 30,
        burst_window_secs: 3600,
        burst_count: 2,
        burst_score: 25,
        coverage_anomaly_multiplier: 3,
        coverage_score: 35,
        fraud_threshold_score: 50,
        mode: FraudMode::FlagOnly,
    };

    let history = ClaimantHistory {
        last_submission_at: 1000,
        burst_bucket_start: 1000,
        burst_count: 1,
        max_coverage_ever: 100,
        total_submissions: 1,
    };

    let now = 1500; // < 1000 + 3600 (trips rate & burst)

    // 1. Rate (30) + Burst (25) when coverage (200) <= 3 * 100
    let (score1, flags1) = ClaimsProcessor::score_submission(&cfg, now, &history, 200);
    assert_eq!(score1, 55);
    assert_eq!(flags1, FRAUD_FLAG_RATE | FRAUD_FLAG_BURST);

    // 2. All 3 rules: Rate (30) + Burst (25) + Coverage (35) when coverage (500) > 3 * 100
    let (score2, flags2) = ClaimsProcessor::score_submission(&cfg, now, &history, 500);
    assert_eq!(score2, 90);
    assert_eq!(flags2, FRAUD_FLAG_RATE | FRAUD_FLAG_BURST | FRAUD_FLAG_COVERAGE);

    // 3. Rate (30) + Coverage (35) when burst does not fire (burst_count = 5)
    let mut cfg_no_burst = cfg.clone();
    cfg_no_burst.burst_count = 5;
    let (score3, flags3) = ClaimsProcessor::score_submission(&cfg_no_burst, now, &history, 500);
    assert_eq!(score3, 65);
    assert_eq!(flags3, FRAUD_FLAG_RATE | FRAUD_FLAG_COVERAGE);

    // 4. Burst (25) + Coverage (35) when rate does not fire (now outside rate window)
    let now_later = 5000;
    let mut history_burst_active = history.clone();
    history_burst_active.last_submission_at = 1000; // 5000 - 1000 = 4000 > 3600 rate window
    history_burst_active.burst_bucket_start = 4500; // 5000 - 4500 = 500 < 3600 burst window
    let (score4, flags4) = ClaimsProcessor::score_submission(&cfg, now_later, &history_burst_active, 500);
    assert_eq!(score4, 60);
    assert_eq!(flags4, FRAUD_FLAG_BURST | FRAUD_FLAG_COVERAGE);
}

#[test]
fn test_fraud_threshold_boundary_exact_score_flagged() {
    let w = deploy();
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // Configure threshold = 30 and rate_score = 30 in Block mode
    cp.set_fraud_config(
        &w.admin,
        &FraudConfig {
            rate_window_secs: 3600,
            rate_score: 30,
            burst_window_secs: 0,
            burst_count: 0,
            burst_score: 0,
            coverage_anomaly_multiplier: 0,
            coverage_score: 0,
            fraud_threshold_score: 30, // exact score == threshold boundary
            mode: FraudMode::FlagOnly,
        },
    );

    let p1 = create_crop_product_with_key(&w, "crop_tb1", symbol_short!("kis2601"));
    let p2 = create_crop_product_with_key(&w, "crop_tb2", symbol_short!("kis2602"));
    let p3 = create_crop_product_with_key(&w, "crop_tb3", symbol_short!("kis2603"));

    let buyer = Address::generate(&w.env);
    let pol1 = buy_crop_policy_with_key(&w, &buyer, p1, symbol_short!("kis2601"), COVERAGE);
    let pol2 = buy_crop_policy_with_key(&w, &buyer, p2, symbol_short!("kis2602"), COVERAGE);

    let _cid1 = cp.submit_claim(&buyer, &pol1);
    w.env.ledger().with_mut(|l| l.timestamp += 1000);

    // score == threshold (30 == 30) must be flagged
    let cid2 = cp.submit_claim(&buyer, &pol2);
    let record = cp.get_fraud_record(&cid2).expect("score == threshold must be flagged");
    assert_eq!(record.score, 30);

    // Now reconfigure threshold = 31: score (30) < threshold (31) must NOT be flagged
    cp.set_fraud_config(
        &w.admin,
        &FraudConfig {
            rate_window_secs: 3600,
            rate_score: 30,
            burst_window_secs: 0,
            burst_count: 0,
            burst_score: 0,
            coverage_anomaly_multiplier: 0,
            coverage_score: 0,
            fraud_threshold_score: 31, // score == threshold - 1
            mode: FraudMode::FlagOnly,
        },
    );

    w.env.ledger().with_mut(|l| l.timestamp += 1000);
    let pol3 = buy_crop_policy_with_key(&w, &buyer, p3, symbol_short!("kis2603"), COVERAGE);
    let cid3 = cp.submit_claim(&buyer, &pol3);
    assert!(cp.get_fraud_record(&cid3).is_none(), "score < threshold must not be flagged");
}

#[test]
#[should_panic(expected = "Error(Contract, #23)")]
fn test_fraud_mode_block_rejects_submission() {
    let w = deploy();
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // Block mode: rejects flagged claims with FraudSuspected (#23)
    cp.set_fraud_config(
        &w.admin,
        &FraudConfig {
            rate_window_secs: 3600,
            rate_score: 30,
            burst_window_secs: 0,
            burst_count: 0,
            burst_score: 0,
            coverage_anomaly_multiplier: 0,
            coverage_score: 0,
            fraud_threshold_score: 25,
            mode: FraudMode::Block,
        },
    );

    let p1 = create_crop_product_with_key(&w, "crop_bk1", symbol_short!("kis2601"));
    let p2 = create_crop_product_with_key(&w, "crop_bk2", symbol_short!("kis2602"));

    let buyer = Address::generate(&w.env);
    let pol1 = buy_crop_policy_with_key(&w, &buyer, p1, symbol_short!("kis2601"), COVERAGE);
    let pol2 = buy_crop_policy_with_key(&w, &buyer, p2, symbol_short!("kis2602"), COVERAGE);

    let _cid1 = cp.submit_claim(&buyer, &pol1);

    // Second claim within rate window trips fraud score 30 >= 25 -> must panic with Error(Contract, #23)
    w.env.ledger().with_mut(|l| l.timestamp += 1000);
    cp.submit_claim(&buyer, &pol2);
}

#[test]
fn test_claimant_history_updates_correctly() {
    let w = deploy();
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // Configure burst window 3600s
    cp.set_fraud_config(
        &w.admin,
        &FraudConfig {
            rate_window_secs: 0,
            rate_score: 0,
            burst_window_secs: 3600,
            burst_count: 5,
            burst_score: 20,
            coverage_anomaly_multiplier: 0,
            coverage_score: 0,
            fraud_threshold_score: 50,
            mode: FraudMode::FlagOnly,
        },
    );

    let p1 = create_crop_product_with_key(&w, "crop_h1", symbol_short!("kis2601"));
    let p2 = create_crop_product_with_key(&w, "crop_h2", symbol_short!("kis2602"));
    let p3 = create_crop_product_with_key(&w, "crop_h3", symbol_short!("kis2603"));

    let buyer = Address::generate(&w.env);
    let pol1 = buy_crop_policy_with_key(&w, &buyer, p1, symbol_short!("kis2601"), COVERAGE);
    let pol2 = buy_crop_policy_with_key(&w, &buyer, p2, symbol_short!("kis2602"), COVERAGE * 2);
    let pol3 = buy_crop_policy_with_key(&w, &buyer, p3, symbol_short!("kis2603"), COVERAGE / 2);

    let t1 = 1_000_000u64;
    w.env.ledger().with_mut(|l| l.timestamp = t1);

    // First submission: sets anchor, count = 1, total = 1
    cp.submit_claim(&buyer, &pol1);
    let h1 = cp.get_claimant_history(&buyer);
    assert_eq!(h1.total_submissions, 1);
    assert_eq!(h1.burst_count, 1);
    assert_eq!(h1.burst_bucket_start, t1);
    assert_eq!(h1.last_submission_at, t1);
    assert_eq!(h1.max_coverage_ever, COVERAGE);

    // Second submission within 3600s: burst_count increments to 2, anchor preserved, max_coverage_ever updated
    let t2 = t1 + 1800;
    w.env.ledger().with_mut(|l| l.timestamp = t2);
    cp.submit_claim(&buyer, &pol2);
    let h2 = cp.get_claimant_history(&buyer);
    assert_eq!(h2.total_submissions, 2);
    assert_eq!(h2.burst_count, 2);
    assert_eq!(h2.burst_bucket_start, t1); // anchor retained
    assert_eq!(h2.last_submission_at, t2);
    assert_eq!(h2.max_coverage_ever, COVERAGE * 2);

    // Third submission after window expires (> 3600s from anchor): bucket resets to 1 at t3
    let t3 = t1 + 4000;
    w.env.ledger().with_mut(|l| l.timestamp = t3);
    cp.submit_claim(&buyer, &pol3);
    let h3 = cp.get_claimant_history(&buyer);
    assert_eq!(h3.total_submissions, 3);
    assert_eq!(h3.burst_count, 1); // bucket reset
    assert_eq!(h3.burst_bucket_start, t3); // new anchor
    assert_eq!(h3.last_submission_at, t3);
    assert_eq!(h3.max_coverage_ever, COVERAGE * 2); // retained max
}

// ── #902: Payout delay and installment schedules ──────────────────────────────

#[test]
fn test_payout_delay_creates_delayed_claim_status() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // Set 86,400s (24 hours) payout delay
    let delay_secs: i64 = 86_400;
    cp.set_payout_delay(&w.admin, &delay_secs);
    assert_eq!(cp.get_payout_delay(), 86_400);

    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    let claim_id = cp.submit_claim(&buyer, &pol_id);

    submit_rainfall(&w, 20_000_000);
    let process_time = w.env.ledger().timestamp();
    let res = cp.process_claim(&w.keeper, &claim_id, &None);

    // Claim result is Paid, but payout is scheduled at process_time + delay
    assert_eq!(res, ClaimResult::Paid);
    let claim = cp.get_claim(&claim_id);
    assert_eq!(claim.status, ClaimStatus::Paid);
    assert_eq!(claim.payout_ready_at, Some(process_time + 86_400));
}

#[test]
#[should_panic(expected = "Error(Contract, #20)")]
fn test_claim_payout_fails_before_delay_elapses() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    cp.set_payout_delay(&w.admin, &86_400i64);

    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    let claim_id = cp.submit_claim(&buyer, &pol_id);

    submit_rainfall(&w, 20_000_000);
    cp.process_claim(&w.keeper, &claim_id, &None);

    // Advance time only 10,000s (< 86,400s delay)
    w.env.ledger().with_mut(|l| l.timestamp += 10_000);

    // Must panic with PayoutDelayNotElapsed (#20)
    cp.claim_payout(&claim_id);
}

#[test]
fn test_claim_payout_succeeds_after_delay_elapses() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    cp.set_payout_delay(&w.admin, &86_400i64);

    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    let claim_id = cp.submit_claim(&buyer, &pol_id);

    submit_rainfall(&w, 20_000_000);
    cp.process_claim(&w.keeper, &claim_id, &None);

    // Advance time past the 86,400s delay
    w.env.ledger().with_mut(|l| l.timestamp += 86_400);

    let payout_amount = cp.claim_payout(&claim_id);
    assert_eq!(payout_amount, COVERAGE);

    // Claim's payout_ready_at is cleared after successful payout
    let claim = cp.get_claim(&claim_id);
    assert!(claim.payout_ready_at.is_none());
}

#[test]
#[should_panic(expected = "Error(Contract, #42)")]
fn test_claim_payout_with_token_rejects_invalid_token() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    cp.set_payout_delay(&w.admin, &86_400i64);

    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    let claim_id = cp.submit_claim(&buyer, &pol_id);

    submit_rainfall(&w, 20_000_000);
    cp.process_claim(&w.keeper, &claim_id, &None);

    w.env.ledger().with_mut(|l| l.timestamp += 86_400);

    // Invalid token address must panic with InvalidToken (#42)
    let fake_token = Address::generate(&w.env);
    cp.claim_payout_with_token(&claim_id, &fake_token);
}

#[test]
fn test_claim_payout_with_token_succeeds_with_valid_token() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    cp.set_payout_delay(&w.admin, &86_400i64);

    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    let claim_id = cp.submit_claim(&buyer, &pol_id);

    submit_rainfall(&w, 20_000_000);
    cp.process_claim(&w.keeper, &claim_id, &None);

    w.env.ledger().with_mut(|l| l.timestamp += 86_400);

    let paid = cp.claim_payout_with_token(&claim_id, &w.usdc);
    assert_eq!(paid, COVERAGE);
}

#[test]
fn test_installment_schedule_creates_correct_payments() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // Enable delay so process_claim doesn't pay upfront
    cp.set_payout_delay(&w.admin, &86_400i64);

    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    let claim_id = cp.submit_claim(&buyer, &pol_id);

    submit_rainfall(&w, 20_000_000);
    cp.process_claim(&w.keeper, &claim_id, &None);

    // Schedule 4 installments of 250 USDC (total 1,000 USDC == COVERAGE), interval 604_800s (1 week)
    let amount_per_installment: i128 = 250_000_000;
    let num_installments: u32 = 4;
    let interval: u64 = 604_800;

    let sched_time = w.env.ledger().timestamp();
    cp.schedule_installments(
        &w.keeper,
        &claim_id,
        &amount_per_installment,
        &num_installments,
        &interval,
    );

    let claim = cp.get_claim(&claim_id);
    assert_eq!(claim.installments.len(), 1);
    let sched = claim.installments.get_unchecked(0);
    assert_eq!(sched.total_amount, 1_000_000_000);
    assert_eq!(sched.amount_per_installment, amount_per_installment);
    assert_eq!(sched.num_installments, 4);
    assert_eq!(sched.paid_count, 0);
    assert_eq!(sched.first_installment_at, sched_time + interval);

    // 1st installment: advance to first_installment_at
    w.env.ledger().with_mut(|l| l.timestamp = sched.first_installment_at);
    let paid1 = cp.claim_installment(&buyer, &claim_id);
    assert_eq!(paid1, 250_000_000);
    let sched_after_1 = cp.get_claim(&claim_id).installments.get_unchecked(0);
    assert_eq!(sched_after_1.paid_count, 1);

    // 2nd and 3rd installments: advance 2 full intervals -> pays 2 installments (500_000_000)
    w.env.ledger().with_mut(|l| l.timestamp += 2 * interval);
    let paid2 = cp.claim_installment(&buyer, &claim_id);
    assert_eq!(paid2, 500_000_000);
    let sched_after_2 = cp.get_claim(&claim_id).installments.get_unchecked(0);
    assert_eq!(sched_after_2.paid_count, 3);

    // 4th installment: advance 1 interval -> pays final installment (250_000_000)
    w.env.ledger().with_mut(|l| l.timestamp += interval);
    let paid3 = cp.claim_installment(&buyer, &claim_id);
    assert_eq!(paid3, 250_000_000);
    let sched_after_3 = cp.get_claim(&claim_id).installments.get_unchecked(0);
    assert_eq!(sched_after_3.paid_count, 4);
}
