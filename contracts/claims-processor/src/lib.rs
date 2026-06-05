//! Parashield Claims Processor
//!
//! Evaluates whether a policy's trigger condition has been met by querying the
//! Oracle Verifier, then instructs the Policy Engine to pay out or expire.
//!
//! Two processing paths
//! ─────────────────────
//! 1. `submit_claim` + `process_claim` — user or keeper manually triggers evaluation.
//! 2. `auto_process` — keeper-triggered; evaluates without a prior user submission.
//!    This is the primary path for parametric insurance (no claim form needed).
//!
//! Idempotency
//! ────────────
//! Once a policy is Claimed or Expired, further process/auto_process calls
//! return the appropriate ClaimResult without writing again.
#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, contracterror, panic_with_error,
    Address, Env, Vec,
};

pub mod types;
pub use types::*;

// ─── Cross-contract client interfaces ────────────────────────────────────────

#[soroban_sdk::contractclient(name = "PolicyEngineClient")]
trait IPolicyEngine {
    fn get_policy(env: Env, policy_id: u128) -> parashield_policy_engine::Policy;
    fn pay_claim(env: Env, caller: Address, policy_id: u128);
    fn expire_policy(env: Env, caller: Address, policy_id: u128);
}

#[soroban_sdk::contractclient(name = "OracleVerifierClient")]
trait IOracleVerifier {
    fn verify_trigger(
        env: Env,
        data_type: soroban_sdk::Symbol,
        key: soroban_sdk::Symbol,
        condition: parashield_oracle_verifier::TriggerCondition,
    ) -> bool;
}

// ─── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
enum StorageKey {
    Initialized,
    Admin,
    PolicyEngine,
    OracleVerifier,
    Claim(u128),
    PolicyClaim(u128),   // policy_id → claim_id (one claim per policy)
    NextClaimId,
    PendingClaims,       // Vec<u128>
}

// ─── Errors ───────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized     = 2,
    Unauthorized       = 3,
    ClaimNotFound      = 4,
    PolicyNotActive    = 5,
    AlreadyClaimed     = 6,
    AlreadyProcessed   = 7,
}

// ─── Contract ─────────────────────────────────────────────────────────────────

#[contract]
pub struct ClaimsProcessor;

#[contractimpl]
impl ClaimsProcessor {

    // ── Lifecycle ────────────────────────────────────────────────────────────

    pub fn initialize(
        env: Env,
        admin: Address,
        policy_engine: Address,
        oracle_verifier: Address,
    ) {
        if env.storage().instance().has(&StorageKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&StorageKey::Initialized, &true);
        env.storage().instance().set(&StorageKey::Admin, &admin);
        env.storage().instance().set(&StorageKey::PolicyEngine, &policy_engine);
        env.storage().instance().set(&StorageKey::OracleVerifier, &oracle_verifier);
        env.storage().instance().set(&StorageKey::NextClaimId, &1u128);
        env.storage().instance().set(&StorageKey::PendingClaims, &Vec::<u128>::new(&env));
    }

    // ── Claim Submission ─────────────────────────────────────────────────────

    /// Manually submit a claim for a policy. Returns the new claim ID.
    /// Only the policyholder may submit; only one claim per policy.
    pub fn submit_claim(env: Env, claimant: Address, policy_id: u128) -> u128 {
        claimant.require_auth();

        // Guard: one claim per policy
        if env.storage().persistent().has(&StorageKey::PolicyClaim(policy_id)) {
            panic_with_error!(&env, Error::AlreadyClaimed);
        }

        // Verify policy is Active via Policy Engine
        let policy_engine: Address = env.storage().instance()
            .get(&StorageKey::PolicyEngine).unwrap();
        let policy = PolicyEngineClient::new(&env, &policy_engine)
            .get_policy(&policy_id);

        if policy.policyholder != claimant {
            panic_with_error!(&env, Error::Unauthorized);
        }
        if policy.status != parashield_policy_engine::PolicyStatus::Active {
            panic_with_error!(&env, Error::PolicyNotActive);
        }

        let claim_id   = Self::next_claim_id(&env);
        let now        = env.ledger().timestamp();
        let claim = Claim {
            id: claim_id,
            policy_id,
            claimant,
            coverage_amount: policy.coverage_amount,
            observed_value: None,
            trigger_met: false,
            status: ClaimStatus::Pending,
            submitted_at: now,
            processed_at: None,
            dispute_reason: None,
        };
        env.storage().persistent().set(&StorageKey::Claim(claim_id), &claim);
        env.storage().persistent().set(&StorageKey::PolicyClaim(policy_id), &claim_id);

        let mut pending: Vec<u128> = env.storage().instance()
            .get(&StorageKey::PendingClaims).unwrap_or_else(|| Vec::new(&env));
        pending.push_back(claim_id);
        env.storage().instance().set(&StorageKey::PendingClaims, &pending);

        claim_id
    }

    /// Process an existing pending claim. Reads oracle data and pays out or rejects.
    pub fn process_claim(env: Env, keeper: Address, claim_id: u128) -> ClaimResult {
        keeper.require_auth();
        let mut claim: Claim = env.storage().persistent()
            .get(&StorageKey::Claim(claim_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ClaimNotFound));

        if claim.status != ClaimStatus::Pending {
            panic_with_error!(&env, Error::AlreadyProcessed);
        }
        Self::evaluate_and_settle(&env, &mut claim)
    }

    /// Keeper-triggered automatic processing — no prior `submit_claim` needed.
    /// This is the primary flow for parametric insurance.
    /// Returns AlreadyClaimed / Expired idempotently if policy is already settled.
    pub fn auto_process(env: Env, keeper: Address, policy_id: u128) -> ClaimResult {
        keeper.require_auth();

        let policy_engine: Address = env.storage().instance()
            .get(&StorageKey::PolicyEngine).unwrap();
        let policy = PolicyEngineClient::new(&env, &policy_engine)
            .get_policy(&policy_id);

        // Idempotency: check current policy status
        match policy.status {
            parashield_policy_engine::PolicyStatus::Claimed    => return ClaimResult::AlreadyClaimed,
            parashield_policy_engine::PolicyStatus::Expired    => return ClaimResult::Expired,
            parashield_policy_engine::PolicyStatus::Cancelled  => return ClaimResult::PolicyNotActive,
            parashield_policy_engine::PolicyStatus::Active     => {}
        }

        // Check if policy has expired with no trigger
        let now = env.ledger().timestamp();
        if now > policy.end_time {
            PolicyEngineClient::new(&env, &policy_engine)
                .expire_policy(&env.current_contract_address(), &policy_id);
            return ClaimResult::Expired;
        }

        // Create an internal claim record for this auto-processing
        let claim_id = if env.storage().persistent().has(&StorageKey::PolicyClaim(policy_id)) {
            env.storage().persistent()
                .get(&StorageKey::PolicyClaim(policy_id)).unwrap()
        } else {
            let cid = Self::next_claim_id(&env);
            let claim = Claim {
                id: cid,
                policy_id,
                claimant: policy.policyholder.clone(),
                coverage_amount: policy.coverage_amount,
                observed_value: None,
                trigger_met: false,
                status: ClaimStatus::Pending,
                submitted_at: now,
                processed_at: None,
                dispute_reason: None,
            };
            env.storage().persistent().set(&StorageKey::Claim(cid), &claim);
            env.storage().persistent().set(&StorageKey::PolicyClaim(policy_id), &cid);
            cid
        };

        let mut claim: Claim = env.storage().persistent()
            .get(&StorageKey::Claim(claim_id)).unwrap();
        if claim.status != ClaimStatus::Pending {
            return ClaimResult::AlreadyClaimed;
        }
        Self::evaluate_and_settle(&env, &mut claim)
    }

    /// Process up to `limit` pending claims parametrically in one call.
    /// Returns a Vec of (claim_id, result) pairs for the processed claims.
    /// Skips any claim that is not in Pending status (idempotent).
    pub fn batch_auto_process(env: Env, caller: Address, limit: u32) -> Vec<(u128, ClaimResult)> {
        caller.require_auth();
        let pending: Vec<u128> = env.storage().instance()
            .get(&StorageKey::PendingClaims)
            .unwrap_or_else(|| Vec::new(&env));

        let mut results: Vec<(u128, ClaimResult)> = Vec::new(&env);
        let process_count = if pending.len() < limit { pending.len() } else { limit };

        for i in 0..process_count {
            let claim_id = pending.get_unchecked(i);
            let mut claim: Claim = match env.storage().persistent()
                .get(&StorageKey::Claim(claim_id)) {
                Some(c) => c,
                None    => continue,
            };
            if claim.status != ClaimStatus::Pending { continue; }
            if claim.processed_at.is_some() { continue; }

            let result = Self::evaluate_and_settle(&env, &mut claim);
            results.push_back((claim_id, result));
        }
        results
    }

    // ── Dispute ───────────────────────────────────────────────────────────────

    pub fn dispute_claim(env: Env, claimant: Address, claim_id: u128, reason: soroban_sdk::Symbol) {
        claimant.require_auth();
        let mut claim: Claim = env.storage().persistent()
            .get(&StorageKey::Claim(claim_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ClaimNotFound));
        if claim.claimant != claimant { panic_with_error!(&env, Error::Unauthorized); }
        claim.status = ClaimStatus::Disputed;
        claim.dispute_reason = Some(reason);
        env.storage().persistent().set(&StorageKey::Claim(claim_id), &claim);
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    pub fn get_claim(env: Env, claim_id: u128) -> Claim {
        env.storage().persistent()
            .get(&StorageKey::Claim(claim_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ClaimNotFound))
    }

    pub fn get_pending_claims(env: Env) -> Vec<u128> {
        env.storage().instance()
            .get(&StorageKey::PendingClaims)
            .unwrap_or_else(|| Vec::new(&env))
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Core evaluation: check oracle, update claim record, instruct Policy Engine.
    fn evaluate_and_settle(env: &Env, claim: &mut Claim) -> ClaimResult {
        let oracle_verifier: Address = env.storage().instance()
            .get(&StorageKey::OracleVerifier).unwrap();
        let policy_engine: Address = env.storage().instance()
            .get(&StorageKey::PolicyEngine).unwrap();

        // Reload policy to get trigger params
        let policy = PolicyEngineClient::new(env, &policy_engine)
            .get_policy(&claim.policy_id);

        let condition = parashield_oracle_verifier::TriggerCondition {
            data_type:   policy.oracle_data_type.clone(),
            key:         policy.oracle_key.clone(),
            threshold:   policy.trigger_threshold,
            comparison:  map_comparison(&policy.trigger_comparison),
        };

        let trigger_met = OracleVerifierClient::new(env, &oracle_verifier)
            .verify_trigger(&policy.oracle_data_type, &policy.oracle_key, &condition);

        claim.trigger_met  = trigger_met;
        claim.processed_at = Some(env.ledger().timestamp());

        let result = if trigger_met {
            claim.status = ClaimStatus::Paid;
            PolicyEngineClient::new(env, &policy_engine)
                .pay_claim(&env.current_contract_address(), &claim.policy_id);
            ClaimResult::Paid
        } else {
            claim.status = ClaimStatus::Rejected;
            ClaimResult::Rejected
        };

        env.storage().persistent().set(&StorageKey::Claim(claim.id), claim);
        result
    }

    fn next_claim_id(env: &Env) -> u128 {
        let id: u128 = env.storage().instance()
            .get(&StorageKey::NextClaimId).unwrap_or(1);
        env.storage().instance().set(&StorageKey::NextClaimId, &(id + 1));
        id
    }
}

/// Map policy-engine TriggerComparison to oracle-verifier TriggerComparison.
fn map_comparison(
    c: &parashield_policy_engine::TriggerComparison,
) -> parashield_oracle_verifier::TriggerComparison {
    match c {
        parashield_policy_engine::TriggerComparison::LessThan    =>
            parashield_oracle_verifier::TriggerComparison::LessThan,
        parashield_policy_engine::TriggerComparison::GreaterThan =>
            parashield_oracle_verifier::TriggerComparison::GreaterThan,
        parashield_policy_engine::TriggerComparison::Equal       =>
            parashield_oracle_verifier::TriggerComparison::Equal,
    }
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_integration;
