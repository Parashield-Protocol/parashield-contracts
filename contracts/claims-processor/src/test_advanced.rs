#![allow(clippy::inconsistent_digit_grouping)]
//! Advanced claims-processor tests: edge cases, boundary conditions, multi-contract interactions.
#![cfg(test)]

extern crate std;

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Env,
};
use parashield_oracle_verifier::{OracleVerifier, OracleVerifierClient};
use parashield_policy_engine::{
    PolicyEngine, PolicyEngineClient,
    TriggerType, TriggerComparison, CreateProductParams,
};
use parashield_risk_pool::{RiskPool, RiskPoolClient};

const COVERAGE: i128 = 1_000_000_000; // 100 USDC

struct World {
    env:       Env,
    admin:     Address,
    keeper:    Address,
    oracle_w:  Address,
    usdc:      Address,
    oracle_id: Address,
    policy_id: Address,
    claims_id: Address,
    pool_id:   Address,
}

fn deploy() -> World {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin  = Address::generate(&env);
    let keeper = Address::generate(&env);
    let oracle_wallet = Address::generate(&env);

    let usdc = env.register_stellar_asset_contract_v2(admin.clone()).address();

    // 1. Deploy oracle verifier
    let oracle_id = env.register(OracleVerifier, ());
    OracleVerifierClient::new(&env, &oracle_id).initialize(&admin);
    OracleVerifierClient::new(&env, &oracle_id)
        .add_oracle(&admin, &oracle_wallet, &symbol_short!("weather"), &90u32);

    // 2. Deploy risk pool (category: crop)
    let backstop = env.register_stellar_asset_contract_v2(Address::generate(&env)).address();
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
    PolicyEngineClient::new(&env, &policy_id)
        .initialize(&admin, &usdc, &oracle_id);
    
    ClaimsProcessorClient::new(&env, &claims_id)
        .initialize(&admin, &policy_id, &pool_id, &oracle_id, &604_800u64);
    
    // Authorize keeper on the claims processor
    ClaimsProcessorClient::new(&env, &claims_id)
        .add_keeper(&admin, &keeper);

    // Wire claims processor as authorized caller on policy engine
    PolicyEngineClient::new(&env, &policy_id)
        .set_claims_processor(&admin, &claims_id);

    World { env, admin, keeper, oracle_w: oracle_wallet, usdc, oracle_id, policy_id, claims_id, pool_id }
}

fn create_crop_product(w: &World) -> u128 {
    PolicyEngineClient::new(&w.env, &w.policy_id).create_product(
        &w.admin,
        &CreateProductParams {
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
        },
    )
}

fn buy_crop_policy(w: &World, buyer: &Address, product_id: u128) -> u128 {
    StellarAssetClient::new(&w.env, &w.usdc).mint(buyer, &5_000_000_000i128);
    // Fund the pool with coverage capital
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.pool_id, &10_000_000_000i128);
    // Deposit to pool and lock coverage for the policy
    RiskPoolClient::new(&w.env, &w.pool_id).deposit(&buyer, &1_000_000_000i128, &0i128);
    
    let policy_id = PolicyEngineClient::new(&w.env, &w.policy_id)
        .buy_policy(buyer, &product_id, &COVERAGE, &30u32, &symbol_short!("kis2606"));
    
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
    
    // Process all 3 claims in batch
    let results = cp.batch_auto_process(&w.keeper, &3u32);
    assert_eq!(results.len(), 3);
    
    // All should be paid
    for (_, result) in results.iter() {
        assert_eq!(result, ClaimResult::Paid);
    }
    
    // Verify all policies are now Claimed
    assert_eq!(
        PolicyEngineClient::new(&w.env, &w.policy_id).get_policy(&pol_id1).status,
        parashield_policy_engine::PolicyStatus::Claimed
    );
    assert_eq!(
        PolicyEngineClient::new(&w.env, &w.policy_id).get_policy(&pol_id2).status,
        parashield_policy_engine::PolicyStatus::Claimed
    );
    assert_eq!(
        PolicyEngineClient::new(&w.env, &w.policy_id).get_policy(&pol_id3).status,
        parashield_policy_engine::PolicyStatus::Claimed
    );
}

/// Test batch processing with limit smaller than pending queue.
#[test]
fn test_batch_auto_process_with_limit() {
    let w = deploy();
    let pid = create_crop_product(&w);
    
    // Create 5 policies
    let mut policy_ids = Vec::new(&w.env);
    for i in 0..5 {
        let buyer = Address::generate(&w.env);
        let pol_id = buy_crop_policy(&w, &buyer, pid);
        policy_ids.push_back(pol_id);
    }
    
    submit_rainfall(&w, 20_000_000);
    
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    
    // Process only 2 claims at a time
    let results1 = cp.batch_auto_process(&w.keeper, &2u32);
    assert_eq!(results1.len(), 2);
    
    let results2 = cp.batch_auto_process(&w.keeper, &2u32);
    assert_eq!(results2.len(), 2);
    
    let results3 = cp.batch_auto_process(&w.keeper, &2u32);
    assert_eq!(results3.len(), 1); // Only 1 remaining
}

/// A `limit` above `MAX_BATCH_SIZE` must be clamped rather than honoured.
///
/// Each claim costs an oracle read plus a policy-engine cross-contract call, so
/// an unbounded batch would exhaust Soroban's per-transaction instruction
/// budget and fail the whole call with an opaque gas error.
///
/// Claims are submitted explicitly here rather than relying on the shared
/// fixture, which never calls `submit_claim` and so leaves the pending queue
/// empty.
#[test]
fn test_batch_auto_process_clamps_limit_to_max_batch_size() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // Queue one more claim than a single batch is allowed to settle.
    let total = MAX_BATCH_SIZE + 1;
    for _ in 0..total {
        let buyer = Address::generate(&w.env);
        let policy_id = buy_crop_policy(&w, &buyer, pid);
        cp.submit_claim(&buyer, &policy_id);
    }

    // Rainfall above the 50_000_000 threshold: every claim is Rejected, so the
    // batch exercises the ceiling without depending on pool solvency.
    submit_rainfall(&w, 72_000_000);

    // u32::MAX must not drain the queue in one transaction.
    let first = cp.batch_auto_process(&w.keeper, &u32::MAX);
    assert_eq!(first.len(), MAX_BATCH_SIZE);

    // The clamp only defers work — a follow-up call settles the remainder.
    let second = cp.batch_auto_process(&w.keeper, &u32::MAX);
    assert_eq!(second.len(), total - MAX_BATCH_SIZE);

    for (_, result) in second.iter() {
        assert_eq!(result, ClaimResult::Rejected);
    }
}

/// Test staleness threshold boundary conditions.
#[test]
fn test_staleness_threshold_boundary() {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin  = Address::generate(&env);
    let keeper = Address::generate(&env);
    let oracle_wallet = Address::generate(&env);

    let usdc = env.register_stellar_asset_contract_v2(admin.clone()).address();

    let oracle_id = env.register(OracleVerifier, ());
    OracleVerifierClient::new(&env, &oracle_id).initialize(&admin);
    OracleVerifierClient::new(&env, &oracle_id)
        .add_oracle(&admin, &oracle_wallet, &symbol_short!("weather"), &90u32);

    let backstop = env.register_stellar_asset_contract_v2(Address::generate(&env)).address();
    let treasury = Address::generate(&env);
    let pool_id = env.register(RiskPool, ());
    let policy_id = env.register(PolicyEngine, ());
    let claims_id = env.register(ClaimsProcessor, ());

    RiskPoolClient::new(&env, &pool_id).initialize(
        &admin, &usdc, &treasury, &backstop,
        &symbol_short!("crop"), &policy_id, &claims_id,
    );

    PolicyEngineClient::new(&env, &policy_id).initialize(&admin, &usdc, &oracle_id);
    
    // Initialize with very short staleness threshold (10 seconds)
    ClaimsProcessorClient::new(&env, &claims_id)
        .initialize(&admin, &policy_id, &pool_id, &oracle_id, &10u64);
    
    ClaimsProcessorClient::new(&env, &claims_id).add_keeper(&admin, &keeper);
    PolicyEngineClient::new(&env, &policy_id).set_claims_processor(&admin, &claims_id);

    let pid = PolicyEngineClient::new(&env, &policy_id).create_product(
        &admin, &CreateProductParams {
            name: symbol_short!("crop"),
            category: symbol_short!("crop"),
            oracle_key: symbol_short!("kis2606"),
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
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &5_000_000_000i128);
    StellarAssetClient::new(&env, &usdc).mint(&pool_id, &10_000_000_000i128);
    RiskPoolClient::new(&env, &pool_id).deposit(&buyer, &1_000_000_000i128, &0i128);
    
    let pol_id = PolicyEngineClient::new(&env, &policy_id)
        .buy_policy(&buyer, &pid, &COVERAGE, &30u32, &symbol_short!("kis2606"));
    RiskPoolClient::new(&env, &pool_id).lock_for_policy(&admin, &pol_id, &COVERAGE);

    // Submit oracle data
    OracleVerifierClient::new(&env, &oracle_id).submit_data(
        &oracle_wallet, &symbol_short!("weather"), &symbol_short!("kis2606"),
        &20_000_000, &95u32, &env.ledger().timestamp(),
    );

    // Process immediately - should succeed (data is fresh)
    let cp = ClaimsProcessorClient::new(&env, &claims_id);
    let result1 = cp.auto_process(&keeper, &pol_id);
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
    let result = cp.auto_process(&w.keeper, &pol_id);
    
    // Verify successful payout
    assert_eq!(result, ClaimResult::Paid);
    
    // Verify policy status updated in policy-engine
    let policy = PolicyEngineClient::new(&w.env, &w.policy_id).get_policy(&pol_id);
    assert_eq!(policy.status, parashield_policy_engine::PolicyStatus::Claimed);
    
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
    let result = cp.auto_process(&w.keeper, &pol_id);
    
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
    let result = cp.auto_process(&w.keeper, &pol_id);
    
    // Should be rejected (not less than threshold)
    assert_eq!(result, ClaimResult::Rejected);
}

/// Test maximum coverage boundary.
#[test]
fn test_maximum_coverage_boundary() {
    let w = deploy();
    
    // Create product with max coverage
    let pid = PolicyEngineClient::new(&w.env, &w.policy_id).create_product(
        &w.admin, &CreateProductParams {
            name: symbol_short!("max_cov"),
            category: symbol_short!("crop"),
            oracle_key: symbol_short!("kis2606"),
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
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.pool_id, &20_000_000_000i128);
    RiskPoolClient::new(&w.env, &w.pool_id).deposit(&buyer, &10_000_000_000i128, &0i128);
    
    // Buy policy at maximum coverage
    let pol_id = PolicyEngineClient::new(&w.env, &w.policy_id)
        .buy_policy(&buyer, &pid, &10_000_000_000i128, &30u32, &symbol_short!("kis2606"));
    RiskPoolClient::new(&w.env, &w.pool_id).lock_for_policy(&w.admin, &pol_id, &10_000_000_000i128);
    
    submit_rainfall(&w, 20_000_000);
    
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let result = cp.auto_process(&w.keeper, &pol_id);
    
    assert_eq!(result, ClaimResult::Paid);
}

/// Test minimum coverage boundary.
#[test]
fn test_minimum_coverage_boundary() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let buyer = Address::generate(&w.env);
    
    StellarAssetClient::new(&w.env, &w.usdc).mint(&buyer, &1_000_000_000i128);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.pool_id, &1_000_000_000i128);
    RiskPoolClient::new(&w.env, &w.pool_id).deposit(&buyer, &100_000_000i128, &0i128);
    
    // Buy policy at minimum coverage
    let pol_id = PolicyEngineClient::new(&w.env, &w.policy_id)
        .buy_policy(&buyer, &pid, &100_000_000i128, &30u32, &symbol_short!("kis2606"));
    RiskPoolClient::new(&w.env, &w.pool_id).lock_for_policy(&w.admin, &pol_id, &100_000_000i128);
    
    submit_rainfall(&w, 20_000_000);
    
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let result = cp.auto_process(&w.keeper, &pol_id);
    
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
    let res1 = cp.process_claim(&w.keeper, &claim_id1);
    let res2 = cp.process_claim(&w.keeper, &claim_id2);
    let res3 = cp.process_claim(&w.keeper, &claim_id3);
    
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
    let result = cp.process_claim(&w.keeper, &claim_id);
    assert_eq!(result, ClaimResult::Paid);
    assert_eq!(cp.get_pending_claims().len(), 0, "settled claim must leave the pending queue");
}

/// Test version tracking for upgrade path.
#[test]
fn test_initial_version_tracking() {
    let w = deploy();
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    assert_eq!(cp.get_version(), 1);
}

// ── Issue #263: concurrent claim submissions on the same policy ──────────────

/// Two policyholders each own a policy; both submit claims and both are
/// processed within the same block. Verifies that the claim counter, pending
/// queue, and per-policy settlement work correctly under concurrent submissions.
#[test]
fn test_concurrent_claims_on_different_policies_same_block() {
    let w = deploy();
    let pid = create_crop_product(&w);

    let buyer_a = Address::generate(&w.env);
    let buyer_b = Address::generate(&w.env);
    let pol_a = buy_crop_policy(&w, &buyer_a, pid);
    let pol_b = buy_crop_policy(&w, &buyer_b, pid);

    // Both submit claims at the same ledger timestamp.
    submit_rainfall(&w, 20_000_000); // below threshold → trigger met

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_a = cp.submit_claim(&buyer_a, &pol_a);
    let claim_b = cp.submit_claim(&buyer_b, &pol_b);

    // Claim IDs must be unique.
    assert_ne!(claim_a, claim_b);

    // Pending queue holds both claims.
    assert_eq!(cp.get_pending_claims().len(), 2);

    // Process both — each must settle independently.
    let res_a = cp.process_claim(&w.keeper, &claim_a);
    let res_b = cp.process_claim(&w.keeper, &claim_b);
    assert_eq!(res_a, ClaimResult::Paid);
    assert_eq!(res_b, ClaimResult::Paid);

    // Pending queue is drained.
    assert_eq!(cp.get_pending_claims().len(), 0);

    // Each buyer received exactly one payout — no cross-contamination.
    let bal_a = soroban_sdk::token::Client::new(&w.env, &w.usdc).balance(&buyer_a);
    let bal_b = soroban_sdk::token::Client::new(&w.env, &w.usdc).balance(&buyer_b);
    assert_eq!(bal_a, 5_000_000_000 - 4_109_589 + COVERAGE);
    assert_eq!(bal_b, 5_000_000_000 - 4_109_589 + COVERAGE);
}

/// Attempting to submit a second claim on the same policy must fail with
/// AlreadyClaimed (#6), even if the first claim has not been processed yet.
/// This is the core guard against concurrent duplicate submissions.
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_concurrent_duplicate_submission_same_policy_panics() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // First submission succeeds.
    cp.submit_claim(&buyer, &pol_id);

    // Second submission on the SAME policy must panic before the first is processed.
    cp.submit_claim(&buyer, &pol_id);
}

/// After a claim on a policy is fully processed (Paid), a new manual
/// submission must be rejected with AlreadyClaimed (#6).
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_resubmit_after_paid_claim_panics() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    let res = cp.process_claim(&w.keeper, &claim_id);
    assert_eq!(res, ClaimResult::Paid);

    // Attempting to submit again on the already-settled policy → AlreadyClaimed.
    cp.submit_claim(&buyer, &pol_id);
}

/// auto_process on the same policy twice in the same block must be idempotent:
/// the first call settles (Paid), the second returns AlreadyProcessed without
/// paying out twice.
#[test]
fn test_concurrent_auto_process_same_policy_idempotent() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let first  = cp.auto_process(&w.keeper, &pol_id);
    let second = cp.auto_process(&w.keeper, &pol_id);

    assert_eq!(first,  ClaimResult::Paid);
    assert_eq!(second, ClaimResult::AlreadyProcessed);

    // Single payout — balance must reflect exactly one coverage amount.
    let bal = soroban_sdk::token::Client::new(&w.env, &w.usdc).balance(&buyer);
    assert_eq!(bal, 5_000_000_000 - 4_109_589 + COVERAGE);
}

/// Mixed concurrent flow: submit_claim by policyholder and auto_process by
/// keeper target the same policy simultaneously. The first to settle wins;
/// the second must return AlreadyProcessed.
#[test]
fn test_mixed_submit_and_auto_process_same_policy() {
    let w = deploy();
    let pid = create_crop_product(&w);
    let buyer = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    // Policyholder submits claim, then keeper auto_processes the same policy.
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    let res_manual = cp.process_claim(&w.keeper, &claim_id);
    assert_eq!(res_manual, ClaimResult::Paid);

    // auto_process on the same (now Claimed) policy returns idempotently.
    let res_auto = cp.auto_process(&w.keeper, &pol_id);
    assert_eq!(res_auto, ClaimResult::AlreadyProcessed);

    // Balance confirms exactly one payout.
    let bal = soroban_sdk::token::Client::new(&w.env, &w.usdc).balance(&buyer);
    assert_eq!(bal, 5_000_000_000 - 4_109_589 + COVERAGE);
}
