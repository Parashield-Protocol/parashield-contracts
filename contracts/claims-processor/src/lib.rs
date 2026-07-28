#![allow(dead_code)]
#![allow(unused_imports)]
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
extern crate alloc;
use alloc::string::ToString;

use soroban_sdk::{
    contract, contractimpl, contracttype, contracterror, panic_with_error,
    Address, BytesN, Env, Vec, Symbol,
};

pub mod types;
pub use types::*;

// ─── Cross-contract client interfaces ────────────────────────────────────────

#[soroban_sdk::contractclient(name = "RiskPoolClient")]
trait IRiskPool {
    fn release_for_claim(env: Env, caller: Address, policy_id: u128);
}

#[soroban_sdk::contractclient(name = "PolicyEngineClient")]
trait IPolicyEngine {
    fn get_policy(env: Env, policy_id: u128) -> parashield_policy_engine::Policy;
    fn pay_claim(env: Env, caller: Address, policy_id: u128);
    fn expire_policy(env: Env, caller: Address, policy_id: u128);
}

#[soroban_sdk::contractclient(name = "OracleVerifierClient")]
trait IOracleVerifier {
    fn verify_trigger_fresh(
        env: Env,
        data_type: soroban_sdk::Symbol,
        key: soroban_sdk::Symbol,
        condition: parashield_oracle_verifier::TriggerCondition,
        max_age_seconds: u64,
    ) -> bool;
}

// ─── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
enum StorageKey {
    Initialized,
    Admin,
    PolicyEngine,
    RiskPool,
    OracleVerifier,
    StalenessThreshold,  // u64 — max acceptable oracle data age in seconds
    Claim(u128),
    PolicyClaim(u128),   // policy_id → claim_id (one claim per policy)
    NextClaimId,
    PendingClaims,       // Vec<u128>
    Keeper(Address),     // keeper whitelist: address → bool
    Paused,              // bool — emergency pause state
    /// Contract version (u32) for storage migration tracking
    Version,
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
    InvalidAddress     = 8,
    Paused             = 9,
}

// ─── Contract ─────────────────────────────────────────────────────────────────

#[contract]
pub struct ClaimsProcessor;

#[contractimpl]
impl ClaimsProcessor {

    // ── Lifecycle ────────────────────────────────────────────────────────────

    /// One-time initialisation. Links the contract to `policy_engine`, `risk_pool`, and
    /// `oracle_verifier`. `staleness_threshold` is the maximum age in seconds for oracle
    /// data to be considered fresh. Panics with `AlreadyInitialized` on a second call.
    pub fn initialize(
        env: Env,
        admin: Address,
        policy_engine: Address,
        risk_pool: Address,
        oracle_verifier: Address,
        staleness_threshold: u64,
    ) {
        if env.storage().instance().has(&StorageKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        let admin_str = admin.to_string();
        
        if false {
            panic!("invalid address: admin must be an account address");
        }
        if admin_str.len() != 56 {
            panic!("invalid address: admin must be an account or contract address");
        }
        let mut admin_buf = [0u8; 56];
        admin_str.copy_into_slice(&mut admin_buf);
        if admin_buf[0] != b'G' && admin_buf[0] != b'C' {
            panic!("invalid address: admin must be an account or contract address");
        }

        let policy_engine_str = policy_engine.to_string();
        let oracle_verifier_str = oracle_verifier.to_string();
        
        
        if policy_engine_str.len() != 56 {
            panic!("invalid address: policy_engine must be a contract address");
        }
        let mut policy_engine_buf = [0u8; 56];
        policy_engine_str.copy_into_slice(&mut policy_engine_buf);
        if policy_engine_buf[0] != b'C' {
            panic!("invalid address: policy_engine must be a contract address");
        }

        let oracle_verifier_str = oracle_verifier.to_string();
        if oracle_verifier_str.len() != 56 {
            panic!("invalid address: oracle_verifier must be a contract address");
        }
        let mut oracle_verifier_buf = [0u8; 56];
        oracle_verifier_str.copy_into_slice(&mut oracle_verifier_buf);
        if oracle_verifier_buf[0] != b'C' {
            panic!("invalid address: oracle_verifier must be a contract address");
        }
        admin.require_auth();

        Self::validate_stellar_address(&env, &admin);
        Self::validate_stellar_address(&env, &policy_engine);
        Self::validate_stellar_address(&env, &risk_pool);
        Self::validate_stellar_address(&env, &oracle_verifier);
        
        env.storage().instance().set(&StorageKey::Initialized, &true);
        env.storage().instance().set(&StorageKey::Admin, &admin);
        env.storage().instance().set(&StorageKey::PolicyEngine, &policy_engine);
        env.storage().instance().set(&StorageKey::RiskPool, &risk_pool);
        env.storage().instance().set(&StorageKey::OracleVerifier, &oracle_verifier);
        env.storage().instance().set(&StorageKey::StalenessThreshold, &staleness_threshold);
        env.storage().instance().set(&StorageKey::NextClaimId, &1u128);
        env.storage().instance().set(&StorageKey::PendingClaims, &Vec::<u128>::new(&env));
        env.storage().instance().set(&StorageKey::Paused, &false);

        env.events().publish(
            (Symbol::new(&env, "initialized"),),
            Initialized {
                admin: admin.clone(),
                policy_engine: policy_engine.clone(),
                risk_pool: risk_pool.clone(),
                oracle_verifier: oracle_verifier.clone(),
                staleness_threshold,
            },
        );
    }

    // ── Keeper Registry ──────────────────────────────────────────────────────

    /// Admin-only: authorize `keeper` to call process_claim / auto_process /
    /// batch_auto_process. Without this, no address can settle claims.
    pub fn add_keeper(env: Env, admin: Address, keeper: Address) {
        Self::require_admin(&env, &admin);
        env.storage().persistent().set(&StorageKey::Keeper(keeper.clone()), &true);
        env.events().publish(
            (Symbol::new(&env, "keeper_added"),),
            keeper,
        );
    }

    /// Admin-only: revoke a keeper's settlement authority.
    pub fn remove_keeper(env: Env, admin: Address, keeper: Address) {
        Self::require_admin(&env, &admin);
        env.storage().persistent().remove(&StorageKey::Keeper(keeper.clone()));
        env.events().publish(
            (Symbol::new(&env, "keeper_removed"),),
            keeper,
        );
    }

    /// Whether `keeper` is currently authorized to settle claims.
    pub fn is_keeper(env: Env, keeper: Address) -> bool {
        env.storage().persistent()
            .get(&StorageKey::Keeper(keeper))
            .unwrap_or(false)
    }

    // ── Claim Submission ─────────────────────────────────────────────────────

    /// Manually submit a claim for a policy. Returns the new claim ID.
    /// Only the policyholder may submit; only one claim per policy.
    pub fn submit_claim(env: Env, claimant: Address, policy_id: u128) -> u128 {
        claimant.require_auth();
        Self::require_not_paused(&env);

        // Guard: one claim per policy
        if env.storage().persistent().has(&StorageKey::PolicyClaim(policy_id)) {
            panic_with_error!(&env, Error::AlreadyClaimed);
        }

        // Verify policy is Active via Policy Engine
        let policy_engine: Address = env.storage().instance()
            .get(&StorageKey::PolicyEngine)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
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
            claimant: claimant.clone(),
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

        env.events().publish(
            (Symbol::new(&env, "claim_submitted"),),
            ClaimSubmitted {
                claim_id,
                policy_id,
                claimant,
                coverage_amount: policy.coverage_amount,
            },
        );

        claim_id
    }

    /// Process an existing pending claim. Reads oracle data and pays out or rejects.
    pub fn process_claim(env: Env, keeper: Address, claim_id: u128) -> ClaimResult {
        Self::require_keeper(&env, &keeper);
        Self::require_not_paused(&env);
        let mut claim: Claim = env.storage().persistent()
            .get(&StorageKey::Claim(claim_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ClaimNotFound));

        if claim.status != ClaimStatus::Pending {
            return ClaimResult::AlreadyProcessed;
        }

        let policy_engine: Address = env.storage().instance()
            .get(&StorageKey::PolicyEngine)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let policy = PolicyEngineClient::new(&env, &policy_engine)
            .get_policy(&claim.policy_id);

        Self::evaluate_and_settle(&env, &mut claim, &policy)
    }

    /// Keeper-triggered automatic processing — no prior `submit_claim` needed.
    /// This is the primary flow for parametric insurance.
    /// Returns AlreadyClaimed / Expired idempotently if policy is already settled.
    pub fn auto_process(env: Env, keeper: Address, policy_id: u128) -> ClaimResult {
        Self::require_keeper(&env, &keeper);
        Self::require_not_paused(&env);

        // ─── IDEMPOTENCY GUARD ───
        // Check if an evaluation record already exists for this policy in our storage
        if env.storage().persistent().has(&StorageKey::PolicyClaim(policy_id)) {
            let existing_claim_id: u128 = env.storage().persistent()
                .get(&StorageKey::PolicyClaim(policy_id)).unwrap();
            
            if let Some(existing_claim) = env.storage().persistent().get::<StorageKey, Claim>(&StorageKey::Claim(existing_claim_id)) {
                if existing_claim.status != ClaimStatus::Pending {
                    return ClaimResult::AlreadyProcessed;
                }
            }
        }

        let policy_engine: Address = env.storage().instance()
            .get(&StorageKey::PolicyEngine)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let policy = PolicyEngineClient::new(&env, &policy_engine)
            .get_policy(&policy_id);

        // Idempotency: check current policy status from down-stream contract
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

        // Create or get the internal claim record
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

            // Make the new claim visible to batch processors and monitoring.
            let mut pending: Vec<u128> = env.storage().instance()
                .get(&StorageKey::PendingClaims).unwrap_or_else(|| Vec::new(&env));
            pending.push_back(cid);
            env.storage().instance().set(&StorageKey::PendingClaims, &pending);
            cid
        };

        let mut claim: Claim = env.storage().persistent()
            .get(&StorageKey::Claim(claim_id)).unwrap();
        if claim.status != ClaimStatus::Pending {
            return ClaimResult::AlreadyProcessed;
        }
        Self::evaluate_and_settle(&env, &mut claim, &policy)
    }

    /// Process up to `limit` pending claims parametrically in one call.
    /// Returns a Vec of (claim_id, result) pairs for the processed claims.
    /// Skips any claim that is not in Pending status (idempotent).
    pub fn batch_auto_process(env: Env, caller: Address, limit: u32) -> Vec<(u128, ClaimResult)> {
        Self::require_keeper(&env, &caller);
        Self::require_not_paused(&env);
        let pending: Vec<u128> = env.storage().instance()
            .get(&StorageKey::PendingClaims)
            .unwrap_or_else(|| Vec::new(&env));

        let mut results: Vec<(u128, ClaimResult)> = Vec::new(&env);
        let process_count = if pending.len() < limit { pending.len() } else { limit };

        let policy_engine: Address = env.storage().instance()
            .get(&StorageKey::PolicyEngine)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        for i in 0..process_count {
            let claim_id = pending.get_unchecked(i);
            let mut claim: Claim = match env.storage().persistent()
                .get(&StorageKey::Claim(claim_id)) {
                Some(c) => c,
                None    => continue,
            };
            if claim.status != ClaimStatus::Pending { continue; }
            if claim.processed_at.is_some() { continue; }

            let policy = PolicyEngineClient::new(&env, &policy_engine)
                .get_policy(&claim.policy_id);
            let result = Self::evaluate_and_settle(&env, &mut claim, &policy);
            results.push_back((claim_id, result));
        }
        results
    }

    // ── Dispute ───────────────────────────────────────────────────────────────

    /// Escalate a Pending or Rejected claim to Disputed status.
    /// Only the original claimant may dispute. Removes the claim from the pending queue
    /// so it is not auto-processed again until an admin resolves the dispute.
    pub fn dispute_claim(env: Env, claimant: Address, claim_id: u128, reason: soroban_sdk::Symbol) {
        claimant.require_auth();
        let mut claim: Claim = env.storage().persistent()
            .get(&StorageKey::Claim(claim_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ClaimNotFound));
        if claim.claimant != claimant { panic_with_error!(&env, Error::Unauthorized); }
        // Only open (Pending) or rejected claims are disputable. A Paid claim is
        // already settled (USDC transferred) and a Disputed claim is already open,
        // so neither may be overwritten.
        if claim.status != ClaimStatus::Pending && claim.status != ClaimStatus::Rejected {
            panic_with_error!(&env, Error::AlreadyProcessed);
        }
        claim.status = ClaimStatus::Disputed;
        claim.dispute_reason = Some(reason.clone());
        env.storage().persistent().set(&StorageKey::Claim(claim_id), &claim);

        // A disputed claim is no longer pending — drop it from the queue so it is
        // not re-evaluated and does not grow the queue unboundedly.
        Self::remove_from_pending(&env, claim_id);

        env.events().publish(
            (Symbol::new(&env, "claim_disputed"),),
            ClaimDisputed {
                claim_id,
                claimant,
                reason,
            },
        );
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    /// Return the `Claim` record for the given `claim_id`. Panics with `ClaimNotFound` if it does not exist.
    pub fn get_claim(env: Env, claim_id: u128) -> Claim {
        env.storage().persistent()
            .get(&StorageKey::Claim(claim_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ClaimNotFound))
    }

    /// Return the claim ID associated with `policy_id`, or `None` if no claim has been filed.
    pub fn get_claim_id_for_policy(env: Env, policy_id: u128) -> Option<u128> {
        env.storage().persistent()
            .get(&StorageKey::PolicyClaim(policy_id))
    }

    /// Return the list of claim IDs that are currently in `Pending` status.
    pub fn get_pending_claims(env: Env) -> Vec<u128> {
        env.storage().instance()
            .get(&StorageKey::PendingClaims)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Return the current admin address. Panics with `NotInitialized` if the contract has not been set up.
    pub fn get_admin(env: Env) -> Address {
        env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    /// Return the current storage schema version (defaults to 1 before any migration).
    pub fn get_version(env: Env) -> u32 {
        env.storage().instance().get(&StorageKey::Version).unwrap_or(1)
    }

    /// Admin-only: pause all claim submissions and processing.
    pub fn pause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&StorageKey::Paused, &true);
        env.events().publish(
            (Symbol::new(&env, "paused"),),
            admin,
        );
    }

    /// Admin-only: resume claim submissions and processing.
    pub fn resume(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&StorageKey::Paused, &false);
        env.events().publish(
            (Symbol::new(&env, "resumed"),),
            admin,
        );
    }

    /// Check whether the contract is currently paused.
    pub fn is_paused(env: Env) -> bool {
        env.storage().instance().get(&StorageKey::Paused).unwrap_or(false)
    }

    /// Upgrade the contract WASM in-place. Only the admin may call this.
    /// Storage is preserved across upgrades; only the execution code changes.
    /// Runs storage migrations if the new version requires them.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>, new_version: u32) {
        let stored_admin: Address = env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        if admin != stored_admin { panic_with_error!(&env, Error::Unauthorized); }
        admin.require_auth();
        
        let current_version: u32 = env.storage().instance().get(&StorageKey::Version).unwrap_or(1);
        if new_version <= current_version {
            panic!("new version must be greater than current version");
        }
        
        // Run migrations from current_version to new_version
        Self::run_migrations(&env, current_version, new_version);
        
        // Update the stored version
        env.storage().instance().set(&StorageKey::Version, &new_version);
        
        // Perform the actual WASM upgrade
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        
        env.events().publish(
            (Symbol::new(&env, "contract_upgraded"),),
            ContractUpgraded {
                old_version: current_version,
                new_version,
            },
        );
    }

    /// Run storage migrations from old_version to new_version.
    /// Each migration function handles a specific version transition.
    fn run_migrations(_env: &Env, _old_version: u32, _new_version: u32) {
        // Migration from v1 to v2: No storage changes needed yet
        // This is where you would add migration logic for specific version bumps
        // Example: if old_version < 2 && new_version >= 2 { Self::migrate_v1_to_v2(env); }
        
        // Future migrations follow the pattern:
        // if old_version < 3 && new_version >= 3 { Self::migrate_v2_to_v3(env); }
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Core evaluation: check oracle, update claim record, instruct Policy Engine.
    /// After successful claim payment, atomically releases the coverage lock on Risk Pool
    /// to prevent coverage from remaining locked indefinitely.
    fn evaluate_and_settle(env: &Env, claim: &mut Claim, policy: &parashield_policy_engine::Policy) -> ClaimResult {
        let oracle_verifier: Address = env.storage().instance()
            .get(&StorageKey::OracleVerifier)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        let policy_engine: Address = env.storage().instance()
            .get(&StorageKey::PolicyEngine)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        let risk_pool: Address = env.storage().instance()
            .get(&StorageKey::RiskPool)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        // Configurable staleness threshold (default 7 days = 604_800 s if not set)
        let staleness_threshold: u64 = env.storage().instance()
            .get(&StorageKey::StalenessThreshold)
            .unwrap_or(604_800u64);

        let condition = parashield_oracle_verifier::TriggerCondition {
            data_type:   policy.oracle_data_type.clone(),
            key:         policy.oracle_key.clone(),
            threshold:   policy.trigger_threshold,
            comparison:  map_comparison(&policy.trigger_comparison),
            tolerance:   0,  // Standard comparison without tolerance
        };

        // verify_trigger_fresh re-queries the oracle and rejects stale data
        // in the same atomic call, preventing stale-data and TOCTOU issues.
        let trigger_met = OracleVerifierClient::new(env, &oracle_verifier)
            .verify_trigger_fresh(
                &policy.oracle_data_type,
                &policy.oracle_key,
                &condition,
                &staleness_threshold,
            );

        claim.trigger_met  = trigger_met;
        claim.processed_at = Some(env.ledger().timestamp());

        let result = if trigger_met {
            claim.status = ClaimStatus::Paid;
            PolicyEngineClient::new(env, &policy_engine)
                .pay_claim(&env.current_contract_address(), &claim.policy_id);
            // Atomic lock release: release coverage lock after successful payment.
            // If release fails, the entire transaction reverts, leaving state unchanged.
            RiskPoolClient::new(env, &risk_pool)
                .release_for_claim(&env.current_contract_address(), &claim.policy_id);
            ClaimResult::Paid
        } else {
            claim.status = ClaimStatus::Rejected;
            ClaimResult::Rejected
        };

        env.storage().persistent().set(&StorageKey::Claim(claim.id), claim);

        // The claim is now settled (Paid/Rejected) — drop it from the pending
        // queue so it is neither re-evaluated nor allowed to grow the queue
        // without bound.
        Self::remove_from_pending(env, claim.id);

        // Emit specific event for rejected claims to enable off-chain monitoring
        if !trigger_met {
            env.events().publish(
                (Symbol::new(env, "claim_rejected"),),
                (claim.id, claim.policy_id, Symbol::new(env, "trigger_not_met")),
            );
        }

        env.events().publish(
            (Symbol::new(env, "claim_settled"), claim.id),
            (claim.trigger_met, claim.coverage_amount),
        );

        result
    }

    /// Remove a claim id from the pending queue, if present.
    fn remove_from_pending(env: &Env, claim_id: u128) {
        let pending: Vec<u128> = env.storage().instance()
            .get(&StorageKey::PendingClaims)
            .unwrap_or_else(|| Vec::new(env));
        let mut updated: Vec<u128> = Vec::new(env);
        for id in pending.iter() {
            if id != claim_id {
                updated.push_back(id);
            }
        }
        if updated.len() != pending.len() {
            env.storage().instance().set(&StorageKey::PendingClaims, &updated);
        }
    }


    /// Panic unless `caller` is a registered keeper and authorizes the call.
    fn require_keeper(env: &Env, caller: &Address) {
        let authorized: bool = env.storage().persistent()
            .get(&StorageKey::Keeper(caller.clone()))
            .unwrap_or(false);
        if !authorized {
            panic_with_error!(env, Error::Unauthorized);
        }
        caller.require_auth();
    }

    /// Panic unless `caller` is the admin and authorizes the call.
    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env.storage().instance()
            .get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        if *caller != admin {
            panic_with_error!(env, Error::Unauthorized);
        }
        caller.require_auth();
    }

    /// Panic if the contract is currently paused.
    fn require_not_paused(env: &Env) {
        let paused: bool = env.storage().instance()
            .get(&StorageKey::Paused)
            .unwrap_or(false);
        if paused {
            panic_with_error!(env, Error::Paused);
        }
    }

    fn next_claim_id(env: &Env) -> u128 {
        let id: u128 = env.storage().instance()
            .get(&StorageKey::NextClaimId).unwrap_or(1);
        env.storage().instance().set(&StorageKey::NextClaimId, &(id + 1));
        id
    }

    fn validate_stellar_address(env: &Env, address: &Address) {
        let addr_str = address.to_string();
        
        // Check length: Stellar public keys are exactly 56 characters
        if addr_str.len() != 56 {
            panic_with_error!(env, Error::InvalidAddress);
        }
        
        let mut buf = [0u8; 56];
        addr_str.copy_into_slice(&mut buf);
        
        // Check prefix: G (Stellar account) or C (Stellar contract)
        if buf[0] != b'G' && buf[0] != b'C' {
            panic_with_error!(env, Error::InvalidAddress);
        }
    }

    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env.storage().instance()
            .get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        if *caller != admin {
            panic_with_error!(env, Error::Unauthorized);
        }
        caller.require_auth();
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
#[cfg(test)]
mod test_advanced;
