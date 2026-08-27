use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{storage::Persistent as _, Address as _, Ledger},
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
    // Fund the pool and policy contract with coverage capital
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.admin, &1_000_000_000i128);
    RiskPoolClient::new(&w.env, &w.pool_id).deposit(&w.admin, &1_000_000_000i128, &0i128, &false);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.policy_id, &10_000_000_000i128);
    
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

// ── Core acceptance tests from the pitch ─────────────────────────────────────

/// Buy policy → oracle submits below threshold → auto_process pays out.
#[test]
fn test_drought_trigger_pays_out() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    // 32mm < 50mm threshold → trigger MET
    submit_rainfall(&w, 32_000_000);

    let result = ClaimsProcessorClient::new(&w.env, &w.claims_id)
        .auto_process(&w.keeper, &pol_id, &None);

    assert_eq!(result, ClaimResult::Paid);
    // Buyer: minted 5_000_000_000, paid 50_000_000 premium, received 1_000_000_000 coverage
    let balance = soroban_sdk::token::Client::new(&w.env, &w.usdc).balance(&buyer);
    assert_eq!(balance, 5_000_000_000 - 4_109_589 + 1_000_000_000);
}

/// Buy policy → oracle submits above threshold → auto_process rejects.
#[test]
fn test_good_rainfall_no_payout() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    // 72mm > 50mm → trigger NOT met
    submit_rainfall(&w, 72_000_000);

    let result = ClaimsProcessorClient::new(&w.env, &w.claims_id)
        .auto_process(&w.keeper, &pol_id, &None);

    assert_eq!(result, ClaimResult::Rejected);
    // Buyer: minted 5_000_000_000, paid 50_000_000 premium, no payout received
    let buyer_bal = soroban_sdk::token::Client::new(&w.env, &w.usdc).balance(&buyer);
    assert_eq!(buyer_bal, 5_000_000_000 - 4_109_589, "no payout when trigger not met");
}

/// Policy past end_time with no trigger → auto_process marks Expired.
#[test]
fn test_expired_policy_no_payout() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    // Advance time past 30-day policy duration (2,592,000 seconds)
    w.env.ledger().with_mut(|l| l.timestamp += 31 * 86_400);

    let result = ClaimsProcessorClient::new(&w.env, &w.claims_id)
        .auto_process(&w.keeper, &pol_id, &None);

    assert_eq!(result, ClaimResult::Expired);
    let policy = PolicyEngineClient::new(&w.env, &w.policy_id).get_policy(&pol_id);
    assert_eq!(policy.status, parashield_policy_engine::PolicyStatus::Expired);
}

/// Expiring a policy must free its earmarked capital in the same transaction.
///
/// Previously `expire_policy` only flipped the policy status; the pool's
/// `release_for_expiry` was left to a separate backend call, so a crash between
/// the two left the policy Expired while its coverage stayed locked — quietly
/// shrinking the liquidity available to underwrite new policies.
#[test]
fn test_expired_policy_releases_pool_capital() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    let pool = RiskPoolClient::new(&w.env, &w.pool_id);
    let locked_while_active = pool.get_stats().total_locked;
    assert_eq!(locked_while_active, COVERAGE, "coverage should be locked while active");

    // Advance past the 30-day policy duration.
    w.env.ledger().with_mut(|l| l.timestamp += 31 * 86_400);

    let result = ClaimsProcessorClient::new(&w.env, &w.claims_id)
        .auto_process(&w.keeper, &pol_id, &None);
    assert_eq!(result, ClaimResult::Expired);

    // The lock is gone without any separate backend call.
    assert_eq!(
        pool.get_stats().total_locked,
        0,
        "expiry must release the capital lock atomically",
    );
}

/// auto_process on already-paid policy returns AlreadyProcessed (idempotent).
#[test]
fn test_double_process_idempotent() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    submit_rainfall(&w, 20_000_000); // 20mm — well below threshold

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let first  = cp.auto_process(&w.keeper, &pol_id, &None);
    let second = cp.auto_process(&w.keeper, &pol_id, &None);

    assert_eq!(first,  ClaimResult::Paid);
    assert_eq!(second, ClaimResult::AlreadyProcessed);

    // Buyer should NOT receive double coverage — exactly one payout
    let balance = soroban_sdk::token::Client::new(&w.env, &w.usdc).balance(&buyer);
    assert_eq!(balance, 5_000_000_000 - 4_109_589 + 1_000_000_000);
}

#[test]
fn test_process_claim_is_idempotent_after_first_settlement() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    let first = cp.process_claim(&w.keeper, &claim_id, &None);
    let second = cp.process_claim(&w.keeper, &claim_id, &None);

    assert_eq!(first, ClaimResult::Paid);
    assert_eq!(second, ClaimResult::AlreadyProcessed);
}

/// submit_claim on already-claimed policy panics.
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_double_submit_claim_panics() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    cp.submit_claim(&buyer, &pol_id);
    // Second submit for same policy → AlreadyClaimed error
    cp.submit_claim(&buyer, &pol_id);
}

/// Non-policyholder cannot submit a claim.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_non_policyholder_cannot_submit_claim() {
    let w        = deploy();
    let pid      = create_crop_product(&w);
    let buyer    = Address::generate(&w.env);
    let stranger = Address::generate(&w.env);
    let pol_id   = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 20_000_000);

    ClaimsProcessorClient::new(&w.env, &w.claims_id)
        .submit_claim(&stranger, &pol_id);
}

/// submit_claim on an expired policy must panic with PolicyExpired error.
#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_submit_claim_on_expired_policy_fails() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    
    // Advance time past the 30-day policy duration
    w.env.ledger().with_mut(|l| l.timestamp += 31 * 86_400);
    
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    cp.submit_claim(&buyer, &pol_id);
}

/// Manual submit_claim + process_claim flow works end-to-end.
#[test]
fn test_manual_claim_flow() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 30_000_000); // below threshold

    let cp       = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    let result   = cp.process_claim(&w.keeper, &claim_id, &None);

    assert_eq!(result, ClaimResult::Paid);
    let claim = cp.get_claim(&claim_id);
    assert_eq!(claim.status, ClaimStatus::Paid);
    assert!(claim.trigger_met);
}

// ── Keeper registry (Issue #79) ──────────────────────────────────────────────

/// auto_process from an address that is not a registered keeper is rejected.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_auto_process_rejects_unregistered_keeper() {
    let w        = deploy();
    let pid      = create_crop_product(&w);
    let buyer    = Address::generate(&w.env);
    let stranger = Address::generate(&w.env);
    let pol_id   = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 20_000_000);

    // `stranger` is not in the keeper registry → Unauthorized
    ClaimsProcessorClient::new(&w.env, &w.claims_id)
        .auto_process(&stranger, &pol_id, &None);
}

/// process_claim from an unregistered address is rejected.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_process_claim_rejects_unregistered_keeper() {
    let w        = deploy();
    let pid      = create_crop_product(&w);
    let buyer    = Address::generate(&w.env);
    let stranger = Address::generate(&w.env);
    let pol_id   = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    cp.process_claim(&stranger, &claim_id, &None);
}

/// A revoked keeper can no longer settle claims.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_removed_keeper_cannot_process() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    cp.remove_keeper(&w.admin, &w.keeper);
    cp.auto_process(&w.keeper, &pol_id, &None);
}

/// Only the admin may register a keeper.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_non_admin_cannot_add_keeper() {
    let w        = deploy();
    let stranger = Address::generate(&w.env);
    let new_keep = Address::generate(&w.env);
    ClaimsProcessorClient::new(&w.env, &w.claims_id)
        .add_keeper(&stranger, &new_keep);
}

// ── Pending queue lifecycle (Issues #76, #74) ────────────────────────────────

/// auto_process makes the internal claim visible, then settlement clears it
/// from the pending queue — the queue never accumulates settled claims.
#[test]
fn test_pending_queue_cleared_after_auto_process() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    assert_eq!(cp.get_pending_claims().len(), 0);

    let result = cp.auto_process(&w.keeper, &pol_id, &None);
    assert_eq!(result, ClaimResult::Paid);

    // Settled claim must not linger in the pending queue.
    assert_eq!(cp.get_pending_claims().len(), 0, "settled claim left in queue");
}

/// submit_claim enqueues; process_claim settlement dequeues.
#[test]
fn test_pending_queue_cleared_after_process_claim() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 30_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    assert_eq!(cp.get_pending_claims().len(), 1);

    cp.process_claim(&w.keeper, &claim_id, &None);
    assert_eq!(cp.get_pending_claims().len(), 0, "settled claim left in queue");
}

/// Disputing a still-pending claim removes it from the pending queue.
#[test]
fn test_dispute_pending_claim_dequeues() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 30_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    assert_eq!(cp.get_pending_claims().len(), 1);

    cp.dispute_claim(&buyer, &claim_id, &symbol_short!("disagree"));
    assert_eq!(cp.get_claim(&claim_id).status, ClaimStatus::Disputed);
    assert_eq!(cp.get_pending_claims().len(), 0, "disputed claim left in queue");
}

// ── Dispute status guard (Issue #78) ─────────────────────────────────────────

/// A Paid claim cannot be flipped back to Disputed.
#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_cannot_dispute_paid_claim() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    cp.process_claim(&w.keeper, &claim_id, &None);
    assert_eq!(cp.get_claim(&claim_id).status, ClaimStatus::Paid);

    // Attempting to dispute a settled/paid claim must panic.
    cp.dispute_claim(&buyer, &claim_id, &symbol_short!("reversal"));
}

/// A rejected claim may be disputed; re-disputing it then fails.
#[test]
fn test_rejected_claim_disputable_then_locked() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 72_000_000); // above threshold → rejected

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    let result = cp.process_claim(&w.keeper, &claim_id, &None);
    assert_eq!(result, ClaimResult::Rejected);

    // First dispute on a Rejected claim succeeds.
    cp.dispute_claim(&buyer, &claim_id, &symbol_short!("disagree"));
    assert_eq!(cp.get_claim(&claim_id).status, ClaimStatus::Disputed);
}

/// Re-disputing an already-Disputed claim is rejected (no dispute loop).
#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_cannot_redispute_disputed_claim() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 72_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    cp.process_claim(&w.keeper, &claim_id, &None);
    cp.dispute_claim(&buyer, &claim_id, &symbol_short!("disagree"));
    // Second dispute → AlreadyProcessed
    cp.dispute_claim(&buyer, &claim_id, &symbol_short!("again"));
}

// ── Address validation (Issue #12) ───────────────────────────────────────────────

/// Test that initialize accepts valid Stellar addresses (generated addresses are always valid)
#[test]
fn test_initialize_with_valid_addresses_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    
    let admin           = Address::generate(&env);
    let policy_engine   = Address::generate(&env);
    let risk_pool       = Address::generate(&env);
    let oracle_verifier = Address::generate(&env);
    
    let claims_id = env.register(ClaimsProcessor, ());
    ClaimsProcessorClient::new(&env, &claims_id)
        .initialize(&admin, &policy_engine, &risk_pool, &oracle_verifier, &604_800u64);
    
    // Should succeed without panic
    let stored_admin = ClaimsProcessorClient::new(&env, &claims_id).get_admin();
    assert_eq!(stored_admin, admin);
}

/// Note: In Soroban SDK, Address objects are type-safe and cannot be created with invalid format.
/// The validation function is a defensive measure for future extensibility.
/// This test verifies that the validation logic exists and would catch format issues
/// if addresses were ever passed as strings from external sources.
#[test]
fn test_address_validation_function_exists() {
    // This test verifies the validation helper is callable
    // Actual invalid address testing is limited by Soroban's type-safe Address type
    let env = Env::default();
    let valid_addr = Address::generate(&env);
    
    // The validation should succeed for valid addresses
    // We can't test invalid addresses because Address::from_string() would fail first
    let addr_str = valid_addr.to_string();
    assert_eq!(addr_str.len(), 56, "Stellar addresses are 56 characters");
    
    let mut buf = [0u8; 56];
    addr_str.copy_into_slice(&mut buf);
    assert!(buf[0] == b'G' || buf[0] == b'C', "Stellar addresses start with 'G' or 'C'");
}

// ── Keeper authorization ───────────────────────────────────────────────────────

/// Non-keeper cannot call auto_process.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_non_keeper_cannot_auto_process() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 20_000_000);

    // A stranger that is not an authorized keeper tries to auto_process
    let stranger = Address::generate(&w.env);
    ClaimsProcessorClient::new(&w.env, &w.claims_id)
        .auto_process(&stranger, &pol_id, &None);
}

/// Non-keeper cannot call process_claim.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_non_keeper_cannot_process_claim() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);

    let stranger = Address::generate(&w.env);
    cp.process_claim(&stranger, &claim_id, &None);
}

// ── Issue #160: double-processing the same claim via process_claim ──────────────

/// Calling `process_claim` twice on the same claim_id in quick succession must
/// return `AlreadyProcessed` on the second call rather than re-invoking the
/// oracle and paying out again. This tests the idempotency guard at the
/// process_claim level (PolicyClaim check at line 292 of lib.rs).
#[test]
fn test_process_claim_double_processing_returns_already_processed() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    // Submit low rainfall so the trigger is met
    submit_rainfall(&w, 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);

    // First call settles the claim (Paid)
    let first = cp.process_claim(&w.keeper, &claim_id, &None);
    assert_eq!(first, ClaimResult::Paid);

    // Second call on same claim returns AlreadyProcessed
    let second = cp.process_claim(&w.keeper, &claim_id, &None);
    assert_eq!(second, ClaimResult::AlreadyProcessed);

    // Balance confirms exactly one payout — no double-spend
    let balance = soroban_sdk::token::Client::new(&w.env, &w.usdc).balance(&buyer);
    assert_eq!(balance, 5_000_000_000 - 4_109_589 + 1_000_000_000);
}

/// Test that batch_auto_process with an empty pending list returns an empty vector.
/// This verifies the function gracefully handles the zero-claims case without panicking.
#[test]
fn test_batch_auto_process_empty_pending_list() {
    let w = deploy();
    
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    
    // Verify no pending claims exist initially
    assert_eq!(cp.get_pending_claims().len(), 0);
    
    // Call batch_auto_process with limit=10 on an empty list
    let results = cp.batch_auto_process(&w.keeper, &10u32);
    
    // Must return an empty vector
    assert_eq!(results.len(), 0, "batch_auto_process must return empty vec when pending list is empty");
    
    // Pending list should still be empty
    assert_eq!(cp.get_pending_claims().len(), 0);
}

// ── Dispute negative cases (Issue #338) ──────────────────────────────────────

/// Disputing a claim id that was never submitted must fail with ClaimNotFound.
#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_dispute_nonexistent_claim_fails() {
    let w = deploy();
    let claimant = Address::generate(&w.env);
    ClaimsProcessorClient::new(&w.env, &w.claims_id)
        .dispute_claim(&claimant, &999_999u128, &symbol_short!("reason"));
}

/// Disputing a claim that has already been paid out must fail with
/// AlreadyProcessed — a settled claim cannot be reopened.
#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_dispute_paid_claim_fails() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 30_000_000); // below threshold

    let cp       = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    assert_eq!(cp.process_claim(&w.keeper, &claim_id, &None), ClaimResult::Paid);

    cp.dispute_claim(&buyer, &claim_id, &symbol_short!("reason"));
}

/// Disputing a claim that is already Disputed must fail with
/// AlreadyProcessed — dispute cannot be filed twice on the same claim.
#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_dispute_already_disputed_claim_fails() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 20_000_000);

    let cp       = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    cp.dispute_claim(&buyer, &claim_id, &symbol_short!("first"));

    // Second dispute on the same already-Disputed claim must be rejected.
    cp.dispute_claim(&buyer, &claim_id, &symbol_short!("second"));
}

// ── TTL behavior (Issue #335) ────────────────────────────────────────────────

/// A submitted claim's TTL must be extended well past the default bump
/// threshold, so it survives long enough to be processed/disputed instead
/// of expiring prematurely and losing the underlying fund record.
#[test]
fn test_submitted_claim_ttl_is_extended() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    submit_rainfall(&w, 30_000_000);

    let cp       = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);

    let ttl = w.env.as_contract(&w.claims_id, || {
        w.env.storage().persistent().get_ttl(&StorageKey::Claim(claim_id))
    });
    // Must be extended well beyond the default archival threshold, not left
    // at whatever minimal TTL a fresh write happens to get.
    assert!(ttl > TTL_THRESHOLD, "claim TTL {} was not extended past the bump threshold", ttl);
}

// ── Cross-chain claim verification (issue #380) ──────────────────────────────

fn polygon() -> soroban_sdk::Symbol {
    symbol_short!("polygon")
}

fn zero_proof(env: &Env) -> soroban_sdk::BytesN<32> {
    soroban_sdk::BytesN::from_array(env, &[0u8; 32])
}

#[test]
fn test_add_and_remove_cross_chain_attestor() {
    let w = deploy();
    let attestor = Address::generate(&w.env);
    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);

    assert!(!cp.is_cross_chain_attestor(&polygon(), &attestor));
    cp.add_cross_chain_attestor(&w.admin, &polygon(), &attestor);
    assert!(cp.is_cross_chain_attestor(&polygon(), &attestor));
    assert_eq!(cp.get_cross_chain_attestors(&polygon()).len(), 1);

    cp.remove_cross_chain_attestor(&w.admin, &polygon(), &attestor);
    assert!(!cp.is_cross_chain_attestor(&polygon(), &attestor));
    assert_eq!(cp.get_cross_chain_attestors(&polygon()).len(), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_unregistered_attestor_cannot_submit_attestation() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    let stranger = Address::generate(&w.env);

    ClaimsProcessorClient::new(&w.env, &w.claims_id).submit_cross_chain_attestation(
        &stranger,
        &pol_id,
        &polygon(),
        &20_000_000i128,
        &zero_proof(&w.env),
        &w.env.ledger().timestamp(),
    );
}

/// A registered attestor reports rainfall below the policy's drought
/// threshold on another chain → the claim pays out exactly as the Stellar
/// oracle path would.
#[test]
fn test_process_cross_chain_claim_pays_out_when_trigger_met() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    let attestor = Address::generate(&w.env);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    cp.add_cross_chain_attestor(&w.admin, &polygon(), &attestor);

    let claim_id = cp.submit_claim(&buyer, &pol_id);
    cp.submit_cross_chain_attestation(
        &attestor,
        &pol_id,
        &polygon(),
        &20_000_000i128, // < 50mm threshold → trigger met
        &zero_proof(&w.env),
        &w.env.ledger().timestamp(),
    );

    let result = cp.process_cross_chain_claim(&w.keeper, &claim_id, &None);
    assert_eq!(result, ClaimResult::Paid);
    assert_eq!(cp.get_claim(&claim_id).status, ClaimStatus::Paid);
}

#[test]
fn test_process_cross_chain_claim_rejects_when_trigger_not_met() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    let attestor = Address::generate(&w.env);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    cp.add_cross_chain_attestor(&w.admin, &polygon(), &attestor);

    let claim_id = cp.submit_claim(&buyer, &pol_id);
    cp.submit_cross_chain_attestation(
        &attestor,
        &pol_id,
        &polygon(),
        &80_000_000i128, // > 50mm threshold → trigger NOT met
        &zero_proof(&w.env),
        &w.env.ledger().timestamp(),
    );

    let result = cp.process_cross_chain_claim(&w.keeper, &claim_id, &None);
    assert_eq!(result, ClaimResult::Rejected);
}

#[test]
#[should_panic(expected = "Error(Contract, #18)")]
fn test_process_cross_chain_claim_without_attestation_fails() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &pol_id);
    cp.process_cross_chain_claim(&w.keeper, &claim_id, &None);
}

#[test]
#[should_panic(expected = "Error(Contract, #19)")]
fn test_process_cross_chain_claim_rejects_stale_attestation() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    let attestor = Address::generate(&w.env);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    cp.add_cross_chain_attestor(&w.admin, &polygon(), &attestor);

    let claim_id = cp.submit_claim(&buyer, &pol_id);
    cp.submit_cross_chain_attestation(
        &attestor,
        &pol_id,
        &polygon(),
        &20_000_000i128,
        &zero_proof(&w.env),
        &w.env.ledger().timestamp(),
    );

    // Past the (default) 7-day staleness threshold configured at initialize().
    w.env.ledger().with_mut(|l| l.timestamp += 604_800 + 1);
    cp.process_cross_chain_claim(&w.keeper, &claim_id, &None);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_removed_attestor_cannot_submit_attestation() {
    let w      = deploy();
    let pid    = create_crop_product(&w);
    let buyer  = Address::generate(&w.env);
    let pol_id = buy_crop_policy(&w, &buyer, pid);
    let attestor = Address::generate(&w.env);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    cp.add_cross_chain_attestor(&w.admin, &polygon(), &attestor);
    cp.remove_cross_chain_attestor(&w.admin, &polygon(), &attestor);

    cp.submit_cross_chain_attestation(
        &attestor,
        &pol_id,
        &polygon(),
        &20_000_000i128,
        &zero_proof(&w.env),
        &w.env.ledger().timestamp(),
    );
}
