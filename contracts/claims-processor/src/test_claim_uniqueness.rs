//! One claim per policy (issue #569).
//!
//! A policy is the unit of risk: it carries exactly one `coverage_amount`
//! earmarked in the risk pool, and `pay_claim` settles it once. If more than
//! one claim record could exist for the same policy, a second record would be
//! a second handle on the same coverage — so the invariant is enforced by a
//! `policy_id → claim_id` index (`StorageKey::PolicyClaim`) that every claim
//! creation path must consult.
//!
//! The guard already existed on `submit_claim`; what was not pinned down is
//! that *every* creation path shares it, and that a rejected duplicate leaves
//! nothing behind. That is what these tests fix in place.
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
    /// The risk pool refuses an admin LP position (issue #568), so the pool's
    /// capital has to come from a third party.
    lp: Address,
    /// Product created on first use. Cached because product *names* are unique
    /// (issue #570), so a test that buys more than one batch cannot create a
    /// second product under the same name.
    product_id: Option<u128>,
}

fn deploy() -> World {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

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
        product_id: None,
    }
}

fn create_crop_product(w: &World) -> u128 {
    PolicyEngineClient::new(&w.env, &w.policy_id).create_product(
        &w.admin,
        &CreateProductParams {
            name: symbol_short!("uniq_cism"),
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
    )
}

/// The world's single product, created on first use.
fn product(w: &mut World) -> u128 {
    if let Some(id) = w.product_id {
        return id;
    }
    let id = create_crop_product(w);
    w.product_id = Some(id);
    id
}

/// Buy `n` distinct policies for `buyer`, funding the pool once. The pool is
/// deliberately over-funded relative to the coverage it will have to lock, so
/// the tests exercise the claims index rather than the pool's solvency checks.
fn buy_policies(w: &mut World, buyer: &Address, n: usize) -> std::vec::Vec<u128> {
    let product_id = product(w);
    let pool_funding = 100_000_000_000i128;
    StellarAssetClient::new(&w.env, &w.usdc).mint(buyer, &(COVERAGE * 10));
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.lp, &pool_funding);
    RiskPoolClient::new(&w.env, &w.pool_id).deposit(&w.lp, &pool_funding, &0i128, &false);
    StellarAssetClient::new(&w.env, &w.usdc).mint(&w.policy_id, &100_000_000_000i128);

    let mut out = std::vec::Vec::new();
    for i in 0..n {
        let oracle_key = match i {
            0 => symbol_short!("kis2606"),
            1 => symbol_short!("kis2607"),
            2 => symbol_short!("kis2608"),
            _ => symbol_short!("kis2609"),
        };
        let policy_id = PolicyEngineClient::new(&w.env, &w.policy_id).buy_policy(
            buyer,
            &product_id,
            &COVERAGE,
            &30u32,
            &oracle_key,
        );
        RiskPoolClient::new(&w.env, &w.pool_id).lock_for_policy(&w.admin, &policy_id, &COVERAGE);
        out.push(policy_id);
    }
    out
}

fn submit_rainfall(w: &World, key: soroban_sdk::Symbol, mm_7dec: i128) {
    OracleVerifierClient::new(&w.env, &w.oracle_id).submit_data(
        &w.oracle_w,
        &symbol_short!("weather"),
        &key,
        &mm_7dec,
        &95u32,
        &w.env.ledger().timestamp(),
    );
}

/// The contract error a `try_*` client call raised, or `None` if it did not
/// fail with one.
///
/// Returning `Option` rather than unwrapping keeps the failure path in the
/// caller's `assert_eq!`, which reports the actual error code on mismatch and
/// needs no `panic!` of its own.
fn contract_error<T, E>(
    res: Result<Result<T, E>, Result<soroban_sdk::Error, soroban_sdk::InvokeError>>,
) -> Option<soroban_sdk::Error> {
    res.err().and_then(|inner| inner.ok())
}

/// `Error::AlreadyClaimed` — the code the one-claim-per-policy guard raises.
fn already_claimed() -> soroban_sdk::Error {
    soroban_sdk::Error::from_contract_error(6)
}

// ── The one-claim-per-policy index is the single source of truth ───────────

/// A second `submit_claim` on the same policy is refused, and the index still
/// points at the first claim.
#[test]
fn second_submit_claim_on_same_policy_is_refused() {
    let mut w = deploy();
    let buyer = Address::generate(&w.env);
    let policies = buy_policies(&mut w, &buyer, 1);
    let policy_id = policies[0];

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let first = cp.submit_claim(&buyer, &policy_id);
    assert_eq!(
        contract_error(cp.try_submit_claim(&buyer, &policy_id)),
        Some(already_claimed()),
        "a duplicate claim must be refused with AlreadyClaimed"
    );
    assert_eq!(cp.get_claim_id_for_policy(&policy_id), Some(first));
}

/// The rejection must be total: no second claim record, no extra queue entry,
/// and no claim id burned by the failed attempt.
#[test]
fn refused_duplicate_leaves_no_trace() {
    let mut w = deploy();
    let buyer = Address::generate(&w.env);
    let policies = buy_policies(&mut w, &buyer, 1);
    let policy_id = policies[0];

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let first = cp.submit_claim(&buyer, &policy_id);
    let pending_after_first = cp.get_pending_claims().len();

    assert!(cp.try_submit_claim(&buyer, &policy_id).is_err());

    assert_eq!(
        cp.get_pending_claims().len(),
        pending_after_first,
        "the rejected duplicate must not enqueue anything"
    );
    assert_eq!(cp.get_claim_id_for_policy(&policy_id), Some(first));

    // The id counter is untouched, so the next genuine claim on another policy
    // still gets the very next id.
    let other = buy_policies(&mut w, &buyer, 2)[1];
    let second = cp.submit_claim(&buyer, &other);
    assert_eq!(
        second,
        first + 1,
        "a refused duplicate must not consume an id"
    );
}

// ── Every creation path shares the guard ───────────────────────────────────

/// `batch_submit_claims` goes through the same guarded body, so two entries
/// for the same policy inside one batch cannot both be admitted. The batch is
/// a single transaction, so the whole call reverts — the first claim is not
/// left dangling either.
#[test]
fn batch_with_repeated_policy_id_reverts() {
    let mut w = deploy();
    let buyer = Address::generate(&w.env);
    let policy_id = buy_policies(&mut w, &buyer, 1)[0];

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let dupes = soroban_sdk::vec![&w.env, policy_id, policy_id];
    assert_eq!(
        contract_error(cp.try_batch_submit_claims(&buyer, &dupes)),
        Some(already_claimed()),
        "a batch must not admit the same policy twice"
    );
    assert_eq!(
        cp.get_claim_id_for_policy(&policy_id),
        None,
        "the reverted batch must not leave a claim behind"
    );
    assert_eq!(cp.get_pending_claims().len(), 0);
}

/// A batch is still allowed to cover several *distinct* policies, and each one
/// gets its own claim.
#[test]
fn batch_over_distinct_policies_admits_one_claim_each() {
    let mut w = deploy();
    let buyer = Address::generate(&w.env);
    let policies = buy_policies(&mut w, &buyer, 2);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let ids = soroban_sdk::vec![&w.env, policies[0], policies[1]];
    let claims = cp.batch_submit_claims(&buyer, &ids);

    assert_eq!(claims.len(), 2);
    assert_eq!(
        cp.get_claim_id_for_policy(&policies[0]),
        Some(claims.get_unchecked(0))
    );
    assert_eq!(
        cp.get_claim_id_for_policy(&policies[1]),
        Some(claims.get_unchecked(1))
    );
}

/// A claim filed through `batch_submit_claims` blocks a later single
/// submission on the same policy.
#[test]
fn batch_claim_blocks_later_single_submission() {
    let mut w = deploy();
    let buyer = Address::generate(&w.env);
    let policy_id = buy_policies(&mut w, &buyer, 1)[0];

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let ids = soroban_sdk::vec![&w.env, policy_id];
    let claims = cp.batch_submit_claims(&buyer, &ids);

    assert_eq!(
        contract_error(cp.try_submit_claim(&buyer, &policy_id)),
        Some(already_claimed())
    );
    assert_eq!(
        cp.get_claim_id_for_policy(&policy_id),
        Some(claims.get_unchecked(0))
    );
}

/// The critical cross-path case: the keeper-driven `auto_process` path creates
/// a claim too, so a claimant cannot slip a second record in behind it. This
/// is the ordering that actually matters in production — parametric policies
/// are normally settled by `auto_process` first, and the claimant's own
/// `submit_claim` arriving afterwards must be refused, not treated as a new
/// claim.
#[test]
fn submit_claim_after_auto_process_is_refused() {
    let mut w = deploy();
    let buyer = Address::generate(&w.env);
    let policy_id = buy_policies(&mut w, &buyer, 1)[0];
    // Trigger met → auto_process settles the policy.
    submit_rainfall(&w, symbol_short!("kis2606"), 20_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    cp.auto_process(&w.keeper, &policy_id, &None);

    assert_eq!(
        contract_error(cp.try_submit_claim(&buyer, &policy_id)),
        Some(already_claimed()),
        "auto_process already owns this policy's single claim"
    );
}

/// `auto_process` settles the *existing* claim instead of minting a second
/// record for the same policy, so the `PolicyClaim` index keeps pointing at
/// the original id and no additional claim id is ever consumed.
#[test]
fn auto_process_settles_the_existing_claim_in_place() {
    let mut w = deploy();
    let buyer = Address::generate(&w.env);
    let policy_id = buy_policies(&mut w, &buyer, 1)[0];
    // Trigger not met, so the claim is rejected rather than paid and the test
    // does not depend on the pool's payout path succeeding.
    submit_rainfall(&w, symbol_short!("kis2606"), 90_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let filed = cp.submit_claim(&buyer, &policy_id);
    assert_eq!(cp.get_claim_id_for_policy(&policy_id), Some(filed));
    assert_eq!(
        cp.get_pending_claims().len(),
        1,
        "the filed claim must be queued exactly once"
    );

    assert_eq!(
        cp.auto_process(&w.keeper, &policy_id, &None),
        ClaimResult::Rejected
    );

    assert_eq!(
        cp.get_claim_id_for_policy(&policy_id),
        Some(filed),
        "auto_process must settle the existing claim, not create another"
    );
    assert_eq!(cp.get_claim(&filed).status, ClaimStatus::Rejected);
    assert_eq!(
        cp.get_pending_claims().len(),
        0,
        "the settled claim must leave the queue"
    );

    // A fresh claim on another policy gets the very next id, proving the
    // auto_process call consumed none.
    let other = buy_policies(&mut w, &buyer, 2)[1];
    assert_eq!(cp.submit_claim(&buyer, &other), filed + 1);
}

/// A settled policy keeps its single-claim reservation: the payout path must
/// not open the door to a follow-up claim once the money has moved.
#[test]
fn settled_policy_cannot_be_claimed_again() {
    let mut w = deploy();
    let buyer = Address::generate(&w.env);
    let policy_id = buy_policies(&mut w, &buyer, 1)[0];
    // Trigger not met → the claim is rejected rather than paid, so the test
    // does not depend on pool settlement succeeding.
    submit_rainfall(&w, symbol_short!("kis2606"), 90_000_000);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let claim_id = cp.submit_claim(&buyer, &policy_id);
    cp.process_claim(&w.keeper, &claim_id, &None);
    assert_eq!(cp.get_claim(&claim_id).status, ClaimStatus::Rejected);

    assert_eq!(
        contract_error(cp.try_submit_claim(&buyer, &policy_id)),
        Some(already_claimed())
    );
    assert_eq!(cp.get_claim_id_for_policy(&policy_id), Some(claim_id));
}

/// Claim ids and the `PolicyClaim` index stay in lockstep across a mix of
/// paths: every admitted claim is reachable from its policy, and the pending
/// queue holds no duplicates.
#[test]
fn index_stays_consistent_across_mixed_paths() {
    let mut w = deploy();
    let buyer = Address::generate(&w.env);
    let policies = buy_policies(&mut w, &buyer, 3);

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let via_batch = cp.batch_submit_claims(&buyer, &soroban_sdk::vec![&w.env, policies[0]]);
    let single = cp.submit_claim(&buyer, &policies[1]);
    let other = cp.submit_claim(&buyer, &policies[2]);

    let expected: [u128; 3] = [via_batch.get_unchecked(0), single, other];
    for (i, policy_id) in policies.iter().enumerate() {
        let claim_id = expected[i];
        assert_eq!(cp.get_claim_id_for_policy(policy_id), Some(claim_id));
        assert_eq!(cp.get_claim(&claim_id).policy_id, *policy_id);
        assert_eq!(cp.get_claim(&claim_id).claimant, buyer);
    }

    let pending = cp.get_pending_claims();
    assert_eq!(pending.len(), 3);
    for claim_id in expected {
        let occurrences = (0..pending.len())
            .filter(|i| pending.get_unchecked(*i) == claim_id)
            .count();
        assert_eq!(occurrences, 1, "each claim must be queued exactly once");
    }
}

/// Time passing does not release the reservation: the guard is on the index,
/// not on a timer, so a stale-but-present claim still blocks a new one.
#[test]
fn reservation_survives_ledger_time_advances() {
    let mut w = deploy();
    let buyer = Address::generate(&w.env);
    let policy_id = buy_policies(&mut w, &buyer, 1)[0];

    let cp = ClaimsProcessorClient::new(&w.env, &w.claims_id);
    let first = cp.submit_claim(&buyer, &policy_id);

    w.env.ledger().with_mut(|l| l.timestamp += 86_400);

    assert_eq!(
        contract_error(cp.try_submit_claim(&buyer, &policy_id)),
        Some(already_claimed())
    );
    assert_eq!(cp.get_claim_id_for_policy(&policy_id), Some(first));
}
