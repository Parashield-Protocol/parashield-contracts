//! CoverageMismatch on every settlement path (issue #828).
//!
//! The `claim.coverage_amount != policy.coverage_amount` guard used to live
//! only in `process_claim`, while `auto_process` and `batch_auto_process`
//! reached `evaluate_and_settle` without it. `pay_claim` transfers the
//! *policy's* coverage amount while the claim records its *own*, so a
//! mismatched claim settled through the keeper paths could pay out an amount
//! the claim record does not reflect. The guard now lives in
//! `evaluate_and_settle`, so these tests pin that every path accepts a
//! matching claim exactly as before, and rejects a mismatched one with the
//! same `CoverageMismatch` error, leaving no funds moved and no state changed.
#![cfg(test)]

extern crate std;

use super::*;
use parashield_oracle_verifier::{OracleVerifier, OracleVerifierClient};
use parashield_policy_engine::{
    CreateProductParams, PolicyEngine, PolicyEngineClient, PolicyStatus, TriggerComparison,
    TriggerType,
};
use parashield_risk_pool::{RiskPool, RiskPoolClient};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
};

const COVERAGE: i128 = 1_000_000_000; // 100 USDC
/// A non-zero ledger time: oracle readings with a zero timestamp are refused
/// (oracle-verifier issue #735).
const NOW: u64 = 1_750_000_000;
/// 20mm < 50mm threshold → trigger met.
const DROUGHT_MM: i128 = 20_000_000;

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
    lp: Address,
    product_id: u128,
}

fn deploy() -> World {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();
    env.ledger().set_timestamp(NOW);

    let admin = Address::generate(&env);
    let keeper = Address::generate(&env);
    let oracle_wallet = Address::generate(&env);
    let lp = Address::generate(&env);

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
    ClaimsProcessorClient::new(&env, &claims_id).initialize(
        &admin,
        &policy_id,
        &pool_id,
        &oracle_id,
        &604_800u64,
    );
    ClaimsProcessorClient::new(&env, &claims_id).add_keeper(&admin, &keeper);
    PolicyEngineClient::new(&env, &policy_id).set_claims_processor(&admin, &claims_id);

    let product_id = PolicyEngineClient::new(&env, &policy_id).create_product(
        &admin,
        &CreateProductParams {
            name: soroban_sdk::String::from_str(&env, "cov_mism"),
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
        lp,
        product_id,
    }
}

/// Buy one policy for a fresh buyer (one buyer per policy keeps clear of the
/// per-user claim cooldown) and lock its coverage in an over-funded pool.
fn buy_policy(w: &World) -> (Address, u128) {
    let buyer = Address::generate(&w.env);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&buyer, &5_000_000_000i128);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.lp, &(COVERAGE * 2));
    RiskPoolClient::new(&w.env, &w.pool_id).deposit(&w.lp, &(COVERAGE * 2), &0i128, &false);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.policy_id, &(COVERAGE * 2));

    let policy_id = PolicyEngineClient::new(&w.env, &w.policy_id).buy_policy(
        &buyer,
        &w.product_id,
        &COVERAGE,
        &30u32,
        &symbol_short!("kis2606"),
        &w.usdc,
    );
    RiskPoolClient::new(&w.env, &w.pool_id).lock_for_policy(&w.admin, &policy_id, &COVERAGE);
    (buyer, policy_id)
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

/// Overwrite the stored claim's `coverage_amount` so it no longer matches
/// its policy. No public entry point can produce this today, which is the
/// point: the guard is defence in depth against a record that drifts from
/// its policy, so the test has to plant the drift directly.
fn corrupt_claim_coverage(w: &World, claim_id: u128, coverage_amount: i128) {
    w.env.as_contract(&w.claims_id, || {
        let mut claim: Claim = w
            .env
            .storage()
            .persistent()
            .get(&StorageKey::Claim(claim_id))
            .unwrap();
        claim.coverage_amount = coverage_amount;
        w.env
            .storage()
            .persistent()
            .set(&StorageKey::Claim(claim_id), &claim);
    });
}

fn balance(w: &World, who: &Address) -> i128 {
    TokenClient::new(&w.env, &w.usdc).balance(who)
}

/// The contract error a `try_*` client call raised, or `None` if it did not
/// fail with one.
fn contract_error<T, E>(
    res: Result<Result<T, E>, Result<soroban_sdk::Error, soroban_sdk::InvokeError>>,
) -> Option<soroban_sdk::Error> {
    res.err().and_then(|inner| inner.ok())
}

/// `Error::CoverageMismatch` — the single code every path must raise.
fn coverage_mismatch() -> soroban_sdk::Error {
    soroban_sdk::Error::from_contract_error(Error::CoverageMismatch as u32)
}

/// Everything a rejected settlement must leave exactly as it found it.
#[derive(Debug, PartialEq)]
struct Snapshot {
    claim: Claim,
    policy_status: PolicyStatus,
    buyer_balance: i128,
    policy_engine_balance: i128,
    pool_balance: i128,
    pending: Vec<u128>,
}

fn snapshot(w: &World, claim_id: u128, policy_id: u128, buyer: &Address) -> Snapshot {
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    Snapshot {
        claim: cp.get_claim(&claim_id),
        policy_status: PolicyEngineClient::new(&w.env, &w.policy_id)
            .get_policy(&policy_id)
            .status,
        buyer_balance: balance(w, buyer),
        policy_engine_balance: balance(w, &w.policy_id),
        pool_balance: balance(w, &w.pool_id),
        pending: cp.get_pending_claims(),
    }
}

// ── Matching claims settle on every path, exactly as before ────────────────

#[test]
fn matching_claim_settles_through_process_claim() {
    let w = deploy();
    let (buyer, policy_id) = buy_policy(&w);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &policy_id);
    submit_rainfall(&w, DROUGHT_MM);

    let before = balance(&w, &buyer);
    assert_eq!(cp.process_claim(&w.keeper, &claim_id, &None), ClaimResult::Paid);

    assert_eq!(balance(&w, &buyer), before + COVERAGE);
    let claim = cp.get_claim(&claim_id);
    assert_eq!(claim.status, ClaimStatus::Paid);
    assert_eq!(claim.paid_amount, Some(COVERAGE));
}

#[test]
fn matching_claim_settles_through_auto_process() {
    let w = deploy();
    let (buyer, policy_id) = buy_policy(&w);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &policy_id);
    submit_rainfall(&w, DROUGHT_MM);

    let before = balance(&w, &buyer);
    assert_eq!(cp.auto_process(&w.keeper, &policy_id, &None), ClaimResult::Paid);

    assert_eq!(balance(&w, &buyer), before + COVERAGE);
    assert_eq!(cp.get_claim(&claim_id).status, ClaimStatus::Paid);
}

#[test]
fn matching_claim_settles_through_batch_auto_process() {
    let w = deploy();
    let (buyer, policy_id) = buy_policy(&w);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &policy_id);
    submit_rainfall(&w, DROUGHT_MM);

    let before = balance(&w, &buyer);
    let results = cp.batch_auto_process(&w.keeper, &10u32);

    assert_eq!(results.len(), 1);
    assert_eq!(results.get_unchecked(0), (claim_id, ClaimResult::Paid));
    assert_eq!(balance(&w, &buyer), before + COVERAGE);
    assert_eq!(cp.get_claim(&claim_id).status, ClaimStatus::Paid);
}

// ── Mismatched claims are rejected on every path, with no side effects ─────

#[test]
fn mismatched_claim_is_rejected_by_process_claim() {
    let w = deploy();
    let (buyer, policy_id) = buy_policy(&w);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &policy_id);
    submit_rainfall(&w, DROUGHT_MM);
    corrupt_claim_coverage(&w, claim_id, COVERAGE * 2);

    let before = snapshot(&w, claim_id, policy_id, &buyer);
    assert_eq!(
        contract_error(cp.try_process_claim(&w.keeper, &claim_id, &None)),
        Some(coverage_mismatch()),
    );
    assert_eq!(snapshot(&w, claim_id, policy_id, &buyer), before);
    assert_eq!(before.claim.status, ClaimStatus::Pending);
    assert_eq!(before.policy_status, PolicyStatus::Active);
}

#[test]
fn mismatched_claim_is_rejected_by_auto_process() {
    let w = deploy();
    let (buyer, policy_id) = buy_policy(&w);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &policy_id);
    submit_rainfall(&w, DROUGHT_MM);
    corrupt_claim_coverage(&w, claim_id, COVERAGE / 2);

    let before = snapshot(&w, claim_id, policy_id, &buyer);
    assert_eq!(
        contract_error(cp.try_auto_process(&w.keeper, &policy_id, &None)),
        Some(coverage_mismatch()),
        "auto_process must raise the same error process_claim does",
    );
    assert_eq!(snapshot(&w, claim_id, policy_id, &buyer), before);
    assert_eq!(before.claim.status, ClaimStatus::Pending);
    assert_eq!(before.policy_status, PolicyStatus::Active);
}

/// `batch_auto_process` skips a per-claim failure rather than aborting, so a
/// lone mismatched claim yields an empty result and is left untouched.
#[test]
fn mismatched_claim_is_skipped_by_batch_auto_process() {
    let w = deploy();
    let (buyer, policy_id) = buy_policy(&w);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &policy_id);
    submit_rainfall(&w, DROUGHT_MM);
    corrupt_claim_coverage(&w, claim_id, COVERAGE * 2);

    let before = snapshot(&w, claim_id, policy_id, &buyer);
    let results = cp.batch_auto_process(&w.keeper, &10u32);

    assert_eq!(results.len(), 0, "the mismatched claim must not be settled");
    assert_eq!(snapshot(&w, claim_id, policy_id, &buyer), before);
    assert_eq!(before.claim.status, ClaimStatus::Pending);
    assert!(before.pending.contains(claim_id));
}

/// One bad claim in a batch neither blocks nor corrupts the others: the
/// matching claims on either side of it are paid in full, and the mismatched
/// one stays Pending in the queue with its policy still Active and unpaid.
#[test]
fn batch_with_one_mismatched_claim_settles_the_others() {
    let w = deploy();
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let (buyer_a, policy_a) = buy_policy(&w);
    let (buyer_bad, policy_bad) = buy_policy(&w);
    let (buyer_c, policy_c) = buy_policy(&w);
    let claim_a = cp.submit_claim(&buyer_a, &policy_a);
    let claim_bad = cp.submit_claim(&buyer_bad, &policy_bad);
    let claim_c = cp.submit_claim(&buyer_c, &policy_c);
    submit_rainfall(&w, DROUGHT_MM);
    corrupt_claim_coverage(&w, claim_bad, COVERAGE * 2);

    let a_before = balance(&w, &buyer_a);
    let c_before = balance(&w, &buyer_c);
    let bad_before = snapshot(&w, claim_bad, policy_bad, &buyer_bad);

    let results = cp.batch_auto_process(&w.keeper, &10u32);

    assert_eq!(results.len(), 2);
    assert_eq!(results.get_unchecked(0), (claim_a, ClaimResult::Paid));
    assert_eq!(results.get_unchecked(1), (claim_c, ClaimResult::Paid));

    // The good claims are paid exactly their coverage.
    assert_eq!(balance(&w, &buyer_a), a_before + COVERAGE);
    assert_eq!(balance(&w, &buyer_c), c_before + COVERAGE);
    assert_eq!(cp.get_claim(&claim_a).status, ClaimStatus::Paid);
    assert_eq!(cp.get_claim(&claim_c).status, ClaimStatus::Paid);
    assert_eq!(cp.get_claim(&claim_a).paid_amount, Some(COVERAGE));
    assert_eq!(cp.get_claim(&claim_c).paid_amount, Some(COVERAGE));

    // The mismatched claim is untouched: still Pending, still queued, its
    // policy still Active and its holder unpaid.
    let bad_claim = cp.get_claim(&claim_bad);
    assert_eq!(bad_claim, bad_before.claim);
    assert_eq!(bad_claim.status, ClaimStatus::Pending);
    assert_eq!(bad_claim.paid_amount, None);
    assert_eq!(balance(&w, &buyer_bad), bad_before.buyer_balance);
    assert_eq!(
        PolicyEngineClient::new(&w.env, &w.policy_id)
            .get_policy(&policy_bad)
            .status,
        PolicyStatus::Active
    );
    let pending = cp.get_pending_claims();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending.get_unchecked(0), claim_bad);
}
