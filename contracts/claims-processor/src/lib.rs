// Address/state validation must fail with a typed contract error so callers
// can match on it programmatically, never with a raw panic! and a string
// message.
#![deny(clippy::panic)]
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
    Address, BytesN, Env, Vec, Symbol, IntoVal,
};

pub mod types;
pub use types::*;

// ─── Cross-contract client interfaces ────────────────────────────────────────

#[soroban_sdk::contractclient(name = "RiskPoolClient")]
trait IRiskPool {
    fn release_for_claim(env: Env, caller: Address, policy_id: u128);
    fn release_for_expiry(env: Env, caller: Address, policy_id: u128);
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

// ─── Storage TTL ──────────────────────────────────────────────────────────────

// Shared protocol constants — single source of truth in parashield-common (issue #342).
use parashield_common::{TTL_THRESHOLD, TTL_EXTEND_TO, ADMIN_TRANSFER_TIMELOCK};

// ─── Batch processing ─────────────────────────────────────────────────────────

/// Hard ceiling on how many claims a single `batch_auto_process` call may
/// settle.
///
/// Each claim in the batch costs an oracle read plus a cross-contract call into
/// the policy-engine, so an unbounded batch would exhaust Soroban's per
/// transaction instruction budget and fail with an opaque gas error — taking
/// the whole batch down with it. Capping keeps every call within budget;
/// callers with a longer queue simply invoke the function again.
pub const MAX_BATCH_SIZE: u32 = 50;

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
    /// Proposed next admin awaiting `accept_admin` (issue #356).
    PendingAdmin,
    /// Ledger timestamp (u64) at which `PendingAdmin` was set, used to enforce
    /// `ADMIN_TRANSFER_TIMELOCK` before `accept_admin` succeeds.
    PendingAdminSince,
    /// A pending admin-transfer proposal awaiting guardian approvals.
    PendingAdminChange,
    /// Contract version (u32) for storage migration tracking
    Version,
    /// Guardian addresses authorized to approve critical actions (Vec<Address>).
    Guardians,
    /// Number of guardian approvals required to execute a critical action
    /// (u32). 0 means guardian multisig is disabled (admin acts alone).
    GuardianThreshold,
    /// A pending, not-yet-executed contract upgrade awaiting guardian approvals.
    PendingUpgrade,
    /// Seconds a claim may sit Pending before it can be escalated (u64).
    EscalationThreshold,
    /// Maximum seconds after a policy's `end_time` during which a claim may
    /// still be submitted for the triggering event (u64). `0` means a claim
    /// can only be filed while the policy is Active (behaves as before).
    ClaimDeadline,
    /// Configurable delay in seconds between claim approval and payout (u64).
    /// 0 = immediate payout (default behavior).
    PayoutDelay,
    /// Identity attestation requirement: product_id → Symbol (id_type required).
    IdentityRequirement(u128),
    /// Fraud detector configuration (issue #437). Absent = detector disabled
    /// entirely; every claim proceeds unchecked.
    FraudConfig,
    /// Compact aggregate of a claimant's recent submission behaviour
    /// (issue #437). Keyed by claimant so the fraud detector never has to
    /// scan the full claim table on a new submission.
    ClaimantHistory(Address),
    /// A frozen snapshot of the fraud detector's judgement on one claim
    /// (issue #437). Written only when the detector flagged something.
    FraudRecord(u128),
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
    PolicyExpired      = 10,
    InvalidVersion     = 11,
    NotGuardian          = 12,
    AlreadyApprovedAction = 13,
    NoPendingUpgrade     = 14,
    InvalidThreshold     = 15,
    AdminTimelockNotExpired = 16,
    NotEscalatable       = 17,
    InvalidThresholdValue = 18,
    /// A `submit_claim` arrived after `end_time + claim_deadline` had elapsed,
    /// so the window to file a claim for the triggering event has closed.
    ClaimDeadlinePassed  = 19,
    /// Payout delay has not yet elapsed — the claim cannot be settled now.
    PayoutDelayNotElapsed = 20,
    /// Identity verification required for this claim category but not verified.
    IdentityVerificationRequired = 21,
    InvalidInput = 22,
    /// Fraud detector flagged this claim's submission and the current
    /// `FraudMode` is `Block` (issue #437). No state was written for the
    /// rejected claim.
    FraudSuspected = 23,
    /// An admin transfer was proposed while another one is still pending,
    /// which would reset the transfer timelock (issue #457).
    AdminTransferPending = 24,
}

/// Approximate Stellar ledger close time in seconds, used to convert
/// wall-clock TTL windows into ledger counts for `extend_ttl`.
const LEDGER_SECONDS: u64 = 5;

/// Claim and PolicyClaim entries must survive from submission until the
/// claim is finally settled (Paid/Rejected) — including disputes, which have
/// no automatic timeout. 365 days comfortably covers policy-engine's longest
/// policy durations plus dispute-resolution time; capped to the network's
/// max TTL at call time so `extend_ttl` never panics.
const CLAIM_RETENTION_SECONDS: u64 = 365 * 24 * 60 * 60;

/// How long a claim may sit Pending before anyone can escalate it, when the
/// admin has not configured a threshold.
///
/// Seven days is long enough that ordinary keeper latency, oracle data still
/// arriving, or a quiet weekend do not trip it, and short enough that a
/// claimant is not left indefinitely without recourse. It is the point past
/// which "still processing" stops being a plausible explanation.
const DEFAULT_ESCALATION_THRESHOLD: u64 = 7 * 24 * 60 * 60;

/// Shortest escalation threshold an admin may configure (1 hour). A near-zero
/// threshold would let every claim be escalated on submission, which turns the
/// signal into noise and defeats the purpose of having one.
const MIN_ESCALATION_THRESHOLD: u64 = 60 * 60;

/// Default window after a policy's `end_time` during which a claim may still be
/// submitted. 30 days is long enough to cover keeper latency, oracle data
/// still arriving, or a claimant simply not noticing the trigger immediately,
/// while still putting a firm upper bound on how long a claim can be filed
/// after the event it relates to (issue #386).
const DEFAULT_CLAIM_DEADLINE: u64 = 30 * 24 * 60 * 60;

// ─── Fraud detection (issue #437) ────────────────────────────────────────────

/// Ceiling for `FraudConfig.fraud_threshold_score`. Scores above 100 could
/// never be reached because the three rules sum to at most 90, and even a
/// future fourth rule should not push a single claim over 100.
const MAX_FRAUD_SCORE: u32 = 100;

/// Bitmask constants matching the `FraudRecord.flags` documentation. Kept
/// as `u32` because the field is `u32`.
const FRAUD_FLAG_RATE: u32 = 1 << 0;
const FRAUD_FLAG_BURST: u32 = 1 << 1;
const FRAUD_FLAG_COVERAGE: u32 = 1 << 2;

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
        env.storage().instance().set(&StorageKey::ClaimDeadline, &DEFAULT_CLAIM_DEADLINE);
        env.storage().instance().set(&StorageKey::EscalationThreshold, &DEFAULT_ESCALATION_THRESHOLD);

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
        env.storage().persistent().extend_ttl(&StorageKey::Keeper(keeper.clone()), TTL_THRESHOLD, TTL_EXTEND_TO);
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

        // Guard: reject expired policies even if status hasn't been updated yet.
        // A direct contract caller could bypass the backend's status check, so
        // we verify end_time at the contract level. A claim may still be filed
        // for a bounded window after the policy ends (`claim_deadline`); once
        // that window closes the triggering event is too old to act on and the
        // submission is rejected (issue #386).
        let now = env.ledger().timestamp();
        if policy.end_time > 0 {
            let cutoff = policy.end_time.saturating_add(Self::claim_deadline(&env));
            if now > cutoff {
                panic_with_error!(&env, Error::ClaimDeadlinePassed);
            }
        }

        let claim_id   = Self::next_claim_id(&env);

        // Issue #437: score this submission against the configured fraud
        // rules. In `Block` mode a high enough score panics here, before any
        // claim state is written — the caller gets FraudSuspected and the
        // world looks exactly as it did before the call. In `FlagOnly` mode
        // a FraudRecord is persisted and the claim proceeds. When no config
        // is set, this is a no-op.
        Self::evaluate_fraud(&env, claim_id, &claimant, policy.coverage_amount);

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
            paid_amount: None,
            partial_payout_bps: None,
            installments: Vec::new(&env),
            payout_ready_at: None,
            identity_verified: false,
            verification_type: None,
            verification_time: None,
        };
        env.storage().persistent().set(&StorageKey::Claim(claim_id), &claim);
        env.storage().persistent().extend_ttl(&StorageKey::Claim(claim_id), TTL_THRESHOLD, TTL_EXTEND_TO);
        env.storage().persistent().set(&StorageKey::PolicyClaim(policy_id), &claim_id);
        env.storage().persistent().extend_ttl(&StorageKey::PolicyClaim(policy_id), TTL_THRESHOLD, TTL_EXTEND_TO);

        let mut pending: Vec<u128> = env.storage().instance()
            .get(&StorageKey::PendingClaims).unwrap_or_else(|| Vec::new(&env));
        pending.push_back(claim_id);
        env.storage().instance().set(&StorageKey::PendingClaims, &pending);

        env.events().publish(
            (Symbol::new(&env, "claim_submitted"),),
            ClaimSubmitted {
                claim_id,
                policy_id,
                claimant: claimant.clone(),
                coverage_amount: policy.coverage_amount,
            },
        );

        // Issue #437: fold this submission into the claimant's aggregate
        // history for the next call's fraud check. Only meaningful when the
        // detector is on, but cheap enough to always maintain; keeping the
        // aggregate up to date lets an admin turn the detector on later
        // without every claimant looking like a first-time submitter.
        let burst_window = Self::fraud_config(&env)
            .map(|c| c.burst_window_secs)
            .unwrap_or(0);
        Self::update_claimant_history(
            &env,
            &claimant,
            now,
            policy.coverage_amount,
            burst_window,
        );

        claim_id
    }

    /// Submit multiple claims in a single transaction up to MAX_BATCH_SIZE.
    pub fn batch_submit_claims(env: Env, claimant: Address, policy_ids: Vec<u128>) -> Vec<u128> {
        claimant.require_auth();
        Self::require_not_paused(&env);

        let mut claim_ids = Vec::new(&env);
        let count = if policy_ids.len() > MAX_BATCH_SIZE {
            MAX_BATCH_SIZE
        } else {
            policy_ids.len()
        };

        for i in 0..count {
            let pid = policy_ids.get_unchecked(i);
            let cid = Self::submit_claim(env.clone(), claimant.clone(), pid);
            claim_ids.push_back(cid);
        }

        env.events().publish(
            (Symbol::new(&env, "batch_claims_submitted"),),
            BatchClaimsSubmitted {
                claimant,
                count,
            },
        );

        claim_ids
    }

    /// Process an existing pending claim. Reads oracle data and pays out or rejects.
    ///
    /// ## Parametric Payout Design Note
    /// In parametric insurance, claim payouts are strictly binary or determined by
    /// objective oracle trigger measurements (e.g., rainfall, wind speed, flight delay)
    /// rather than traditional indemnity models that require loss adjustments or proof
    /// of actual financial loss. When an oracle trigger condition is verified, the
    /// contract pays out the pre-agreed coverage amount (or pre-configured partial
    /// payout percentage) in full, regardless of the policyholder's actual incurred loss.
    ///
    /// `partial_payout_bps` is an optional payout ratio in basis points (0-10000).
    /// - `None` or `Some(10000)` → full coverage payment (default behavior).
    /// - `Some(bps)` where bps < 10000 → proportional partial payment, e.g. `Some(5000)` pays 50%.
    pub fn process_claim(
        env: Env,
        keeper: Address,
        claim_id: u128,
        partial_payout_bps: Option<u32>,
    ) -> ClaimResult {
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

        Self::evaluate_and_settle(&env, &mut claim, &policy, partial_payout_bps)
    }

    /// Process multiple existing claims in a single transaction up to MAX_BATCH_SIZE.
    pub fn batch_process_claims(
        env: Env,
        keeper: Address,
        claim_ids: Vec<u128>,
        partial_payout_bps: Option<u32>,
    ) -> Vec<(u128, ClaimResult)> {
        Self::require_keeper(&env, &keeper);
        Self::require_not_paused(&env);

        let mut results = Vec::new(&env);
        let count = if claim_ids.len() > MAX_BATCH_SIZE {
            MAX_BATCH_SIZE
        } else {
            claim_ids.len()
        };

        for i in 0..count {
            let cid = claim_ids.get_unchecked(i);
            let res = Self::process_claim(env.clone(), keeper.clone(), cid, partial_payout_bps);
            results.push_back((cid, res));
        }

        env.events().publish(
            (Symbol::new(&env, "batch_claims_processed"),),
            BatchClaimsProcessed {
                keeper,
                count,
            },
        );

        results
    }


    /// Keeper-triggered automatic processing — no prior `submit_claim` needed.
    /// This is the primary flow for parametric insurance.
    /// Returns AlreadyClaimed / Expired idempotently if policy is already settled.
    ///
    /// `partial_payout_bps` — see `process_claim` for details.
    pub fn auto_process(
        env: Env,
        keeper: Address,
        policy_id: u128,
        partial_payout_bps: Option<u32>,
    ) -> ClaimResult {
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
            let risk_pool: Address = env.storage().instance()
                .get(&StorageKey::RiskPool)
                .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
            PolicyEngineClient::new(&env, &policy_engine)
                .expire_policy(&env.current_contract_address(), &policy_id);
            // Atomic lock release, mirroring the payout path: expiring a policy
            // and freeing its earmarked capital happen in one transaction, so a
            // crash between the two cannot strand liquidity in the pool. If the
            // release fails the whole call reverts and the policy stays Active.
            RiskPoolClient::new(&env, &risk_pool)
                .release_for_expiry(&env.current_contract_address(), &policy_id);
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
                paid_amount: None,
                partial_payout_bps: None,
                installments: Vec::new(&env),
                payout_ready_at: None,
                identity_verified: false,
                verification_type: None,
                verification_time: None,
            };
            env.storage().persistent().set(&StorageKey::Claim(cid), &claim);
            env.storage().persistent().extend_ttl(&StorageKey::Claim(cid), TTL_THRESHOLD, TTL_EXTEND_TO);
            env.storage().persistent().set(&StorageKey::PolicyClaim(policy_id), &cid);
            env.storage().persistent().extend_ttl(&StorageKey::PolicyClaim(policy_id), TTL_THRESHOLD, TTL_EXTEND_TO);

            // Make the new claim visible to batch processors and monitoring.
            let mut pending: Vec<u128> = env.storage().instance()
                .get(&StorageKey::PendingClaims).unwrap_or_else(|| Vec::new(&env));
            pending.push_back(cid);
            env.storage().instance().set(&StorageKey::PendingClaims, &pending);

            // Emit claim_submitted event for off-chain indexing
            env.events().publish(
                (Symbol::new(&env, "claim_submitted"),),
                ClaimSubmitted {
                    claim_id: cid,
                    policy_id,
                    claimant: policy.policyholder.clone(),
                    coverage_amount: policy.coverage_amount,
                },
            );

            cid
        };

        let mut claim: Claim = env.storage().persistent()
            .get(&StorageKey::Claim(claim_id)).unwrap();
        if claim.status != ClaimStatus::Pending {
            return ClaimResult::AlreadyProcessed;
        }
        Self::evaluate_and_settle(&env, &mut claim, &policy, partial_payout_bps)
    }

    /// Process up to `limit` pending claims parametrically in one call.
    /// Returns a Vec of (claim_id, result) pairs for the processed claims.
    /// Skips any claim that is not in Pending status (idempotent).
    ///
    /// `limit` is clamped to [`MAX_BATCH_SIZE`]. Passing a larger value (or
    /// `u32::MAX`) is not an error — it simply settles the first
    /// `MAX_BATCH_SIZE` pending claims, keeping the transaction inside
    /// Soroban's instruction budget. Call again to drain the rest of the queue.
    ///
    /// Failure isolation: each claim is evaluated via a sub-invocation of
    /// `auto_process` on this contract using `env.try_invoke_contract`. If
    /// any individual claim's cross-contract calls panic (e.g. stale oracle
    /// data, policy engine rejection, risk-pool error), that sub-invocation
    /// returns `Err` and the claim is skipped — the batch itself never
    /// aborts, so all other claims in the queue are still processed.
    pub fn batch_auto_process(env: Env, caller: Address, limit: u32) -> Vec<(u128, ClaimResult)> {
        Self::require_keeper(&env, &caller);
        Self::require_not_paused(&env);
        let pending: Vec<u128> = env.storage().instance()
            .get(&StorageKey::PendingClaims)
            .unwrap_or_else(|| Vec::new(&env));

        let mut results: Vec<(u128, ClaimResult)> = Vec::new(&env);
        let effective_limit = if limit > MAX_BATCH_SIZE { MAX_BATCH_SIZE } else { limit };
        let process_count = if pending.len() < effective_limit {
            pending.len()
        } else {
            effective_limit
        };

        let this = env.current_contract_address();
        let fn_name = Symbol::new(&env, "auto_process");

        for i in 0..process_count {
            let claim_id = pending.get_unchecked(i);

            // Pre-check: skip claims that are no longer Pending without
            // spending a sub-invocation on them (fast path, no isolation cost).
            let claim_opt: Option<Claim> = env.storage().persistent()
                .get(&StorageKey::Claim(claim_id));
            let policy_id = match &claim_opt {
                None => continue,
                Some(c) if c.status != ClaimStatus::Pending => continue,
                Some(c) if c.processed_at.is_some() => continue,
                Some(c) => c.policy_id,
            };

            // Each claim is evaluated in its own sub-invocation so that a
            // panic inside (stale oracle, cross-contract failure, etc.) only
            // rolls back that single claim, not the entire batch.
            //
            // auto_process(caller, policy_id, partial_payout_bps: None)
            // None is encoded as the void Val in the args vector.
            let none_val: soroban_sdk::Val = Option::<u32>::None.into_val(&env);
            let args = soroban_sdk::vec![
                &env,
                caller.to_val(),
                policy_id.into_val(&env),
                none_val,
            ];
            match env.try_invoke_contract::<ClaimResult, soroban_sdk::Error>(
                &this,
                &fn_name,
                args,
            ) {
                Ok(Ok(result)) => {
                    results.push_back((claim_id, result));
                }
                // Sub-invocation failed or returned a contract error — skip
                // this claim and continue processing the rest of the batch.
                Ok(Err(_)) | Err(_) => continue,
            }
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
        // Only open (Pending), rejected, or partially-paid claims are disputable.
        // Fully Paid claims are already settled (USDC transferred), Disputed
        // claims are already open, and Expired claims should not be reopened.
        if claim.status != ClaimStatus::Pending
            && claim.status != ClaimStatus::Rejected
            && claim.status != ClaimStatus::PartiallyPaid
        {
            panic_with_error!(&env, Error::AlreadyProcessed);
        }
        claim.status = ClaimStatus::Disputed;
        claim.dispute_reason = Some(reason.clone());
        let claim_key = StorageKey::Claim(claim_id);
        env.storage().persistent().set(&claim_key, &claim);
        Self::extend_claim_ttl(&env, &claim_key);

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

    /// Admin-only: resolve a disputed claim and re-queue it for processing.
    ///
    /// When a claim is disputed, it is removed from the pending queue and sits in
    /// Disputed status indefinitely. This function allows the admin to review the
    /// dispute and either:
    /// - Clear the dispute and return the claim to Pending for re-evaluation, or
    /// - Perform an off-chain investigation and then call this to re-queue the claim.
    ///
    /// The claim transitions from Disputed → Pending and is added back to the pending
    /// claims queue for the next keeper to process.
    pub fn resolve_dispute(env: Env, admin: Address, claim_id: u128) {
        Self::require_admin(&env, &admin);
        
        let mut claim: Claim = env.storage().persistent()
            .get(&StorageKey::Claim(claim_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ClaimNotFound));
        
        // Only Disputed claims can be resolved
        if claim.status != ClaimStatus::Disputed {
            panic_with_error!(&env, Error::AlreadyProcessed);
        }
        
        // Clear dispute and return to Pending status
        claim.status = ClaimStatus::Pending;
        claim.dispute_reason = None;
        
        let claim_key = StorageKey::Claim(claim_id);
        env.storage().persistent().set(&claim_key, &claim);
        Self::extend_claim_ttl(&env, &claim_key);
        
        // Re-add the claim to the pending queue for re-processing
        let mut pending: Vec<u128> = env.storage().instance()
            .get(&StorageKey::PendingClaims)
            .unwrap_or_else(|| Vec::new(&env));
        pending.push_back(claim_id);
        env.storage().instance().set(&StorageKey::PendingClaims, &pending);
        
        env.events().publish(
            (Symbol::new(&env, "claim_resolved"),),
            ClaimResolved {
                claim_id,
                resolver: admin,
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

    /// Schedule installment payouts for a large claim.
    /// This allows claims to be paid out over time rather than as a single lump sum.
    ///
    /// Parameters:
    /// - `claim_id`: The claim to schedule installments for
    /// - `amount_per_installment`: Amount to pay per installment
    /// - `num_installments`: Total number of installments
    /// - `interval_seconds`: Seconds between each installment
    pub fn schedule_installments(
        env: Env,
        caller: Address,
        claim_id: u128,
        amount_per_installment: i128,
        num_installments: u32,
        interval_seconds: u64,
    ) {
        Self::require_keeper(&env, &caller);
        Self::require_not_paused(&env);

        let mut claim = Self::get_claim(env.clone(), claim_id);
        
        // Only schedule installments for approved claims
        if claim.status != ClaimStatus::Paid && claim.status != ClaimStatus::PartiallyPaid {
            panic_with_error!(&env, Error::InvalidInput);
        }

        // Total installment amount should not exceed coverage
        let total_amount = amount_per_installment.saturating_mul(num_installments as i128);
        if total_amount > claim.coverage_amount {
            panic_with_error!(&env, Error::InvalidInput);
        }

        let now = env.ledger().timestamp();
        let schedule = InstallmentSchedule {
            total_amount,
            amount_per_installment,
            num_installments,
            interval_seconds,
            first_installment_at: now.saturating_add(interval_seconds),
            paid_count: 0,
        };

        claim.installments = Vec::from_array(&env, [schedule.clone()]);
        env.storage().persistent().set(&StorageKey::Claim(claim_id), &claim);
        env.storage().persistent().extend_ttl(&StorageKey::Claim(claim_id), TTL_THRESHOLD, TTL_EXTEND_TO);

        env.events().publish(
            (Symbol::new(&env, "installment_payout_scheduled"),),
            InstallmentPayoutScheduled {
                claim_id,
                policy_id: claim.policy_id,
                claimant: claim.claimant.clone(),
                total_amount,
                num_installments,
                interval_seconds,
                first_installment_at: schedule.first_installment_at,
            },
        );
    }

    /// Claim the next installment for a scheduled claim.
    /// Can be called by the claimant to collect available installments.
    pub fn claim_installment(env: Env, claimant: Address, claim_id: u128) -> i128 {
        claimant.require_auth();
        Self::require_not_paused(&env);

        let mut claim = Self::get_claim(env.clone(), claim_id);
        
        if claim.claimant != claimant {
            panic_with_error!(&env, Error::Unauthorized);
        }

        let schedule = claim.installments.first()
            .unwrap_or_else(|| panic_with_error!(&env, Error::InvalidInput));

        // Check if there are remaining installments
        if schedule.paid_count >= schedule.num_installments {
            panic_with_error!(&env, Error::InvalidInput);
        }

        let now = env.ledger().timestamp();
        
        // Calculate which installments are now available
        let installments_available = if now >= schedule.first_installment_at {
            ((now - schedule.first_installment_at) / schedule.interval_seconds).saturating_add(1)
                .min(schedule.num_installments as u64) as u32
        } else {
            0
        };

        if installments_available <= schedule.paid_count {
            panic_with_error!(&env, Error::InvalidInput);
        }

        // Pay out all available installments
        let amount_to_pay = schedule.amount_per_installment
            .saturating_mul((installments_available - schedule.paid_count) as i128);

        let total_installments = schedule.num_installments;

        // Transfer funds from risk pool
        let risk_pool: Address = env.storage().instance()
            .get(&StorageKey::RiskPool)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        let pool_client = RiskPoolClient::new(&env, &risk_pool);
        pool_client.release_for_claim(&env.current_contract_address(), &claim.policy_id);

        // Update installment schedule
        if let Some(mut sched) = claim.installments.first() {
            sched.paid_count = installments_available;
            claim.installments.set(0, sched);
        }

        env.storage().persistent().set(&StorageKey::Claim(claim_id), &claim);
        env.storage().persistent().extend_ttl(&StorageKey::Claim(claim_id), TTL_THRESHOLD, TTL_EXTEND_TO);

        env.events().publish(
            (Symbol::new(&env, "installment_paid"),),
            InstallmentPaid {
                claim_id,
                claimant,
                amount: amount_to_pay,
                paid_count: installments_available,
                total_installments,
            },
        );

        amount_to_pay
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
    ///
    /// If a guardian threshold > 0 is configured (`set_guardians`), this call
    /// does not upgrade immediately — it registers the upgrade as pending and
    /// requires `threshold` guardians to call `approve_upgrade` before the
    /// WASM is actually replaced, guarding this irreversible operation
    /// against a single compromised admin key.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>, new_version: u32) {
        let stored_admin: Address = env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        if admin != stored_admin { panic_with_error!(&env, Error::Unauthorized); }
        admin.require_auth();

        let current_version: u32 = env.storage().instance().get(&StorageKey::Version).unwrap_or(1);
        if new_version <= current_version {
            panic_with_error!(&env, Error::InvalidVersion);
        }

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::GuardianThreshold)
            .unwrap_or(0);
        if threshold == 0 {
            Self::run_migrations(&env, current_version, new_version);
            env.storage().instance().set(&StorageKey::Version, &new_version);
            env.deployer().update_current_contract_wasm(new_wasm_hash);

            env.events().publish(
                (Symbol::new(&env, "contract_upgraded"),),
                ContractUpgraded {
                    old_version: current_version,
                    new_version,
                },
            );
            return;
        }

        let pending = PendingUpgrade {
            new_wasm_hash,
            new_version,
            approvals: Vec::new(&env),
        };
        env.storage().instance().set(&StorageKey::PendingUpgrade, &pending);
    }

    /// Configure the guardian set and approval threshold required for
    /// critical actions (currently: contract upgrades). Admin-only.
    /// `threshold == 0` disables the guardian requirement (default), so the
    /// admin alone can act — preserves existing single-admin behavior until
    /// guardians are explicitly configured.
    pub fn set_guardians(env: Env, admin: Address, guardians: Vec<Address>, threshold: u32) {
        let stored_admin: Address = env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        if admin != stored_admin { panic_with_error!(&env, Error::Unauthorized); }
        admin.require_auth();
        if threshold > guardians.len() {
            panic_with_error!(&env, Error::InvalidThreshold);
        }
        env.storage().instance().set(&StorageKey::Guardians, &guardians);
        env.storage()
            .instance()
            .set(&StorageKey::GuardianThreshold, &threshold);
        env.events().publish(
            (Symbol::new(&env, "guardians_updated"),),
            GuardiansUpdated { guardians, threshold },
        );
    }

    /// Return the current guardian set.
    pub fn get_guardians(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&StorageKey::Guardians)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Return the current guardian approval threshold (0 = disabled).
    pub fn get_guardian_threshold(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::GuardianThreshold)
            .unwrap_or(0)
    }

    /// Return the pending upgrade awaiting guardian approvals, if any.
    pub fn get_pending_upgrade(env: Env) -> Option<PendingUpgrade> {
        env.storage().instance().get(&StorageKey::PendingUpgrade)
    }

    /// Guardian approval for the pending upgrade. Once enough guardians have
    /// approved (>= threshold), the upgrade executes immediately.
    pub fn approve_upgrade(env: Env, guardian: Address, new_wasm_hash: BytesN<32>) {
        guardian.require_auth();

        let guardians: Vec<Address> = env
            .storage()
            .instance()
            .get(&StorageKey::Guardians)
            .unwrap_or_else(|| Vec::new(&env));
        let mut is_guardian = false;
        for g in guardians.iter() {
            if g == guardian {
                is_guardian = true;
                break;
            }
        }
        if !is_guardian {
            panic_with_error!(&env, Error::NotGuardian);
        }

        let mut pending: PendingUpgrade = env
            .storage()
            .instance()
            .get(&StorageKey::PendingUpgrade)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoPendingUpgrade));
        if pending.new_wasm_hash != new_wasm_hash {
            panic_with_error!(&env, Error::NoPendingUpgrade);
        }
        for a in pending.approvals.iter() {
            if a == guardian {
                panic_with_error!(&env, Error::AlreadyApprovedAction);
            }
        }
        pending.approvals.push_back(guardian.clone());

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::GuardianThreshold)
            .unwrap_or(0);

        env.events().publish(
            (Symbol::new(&env, "upgrade_approved"),),
            UpgradeApproved {
                new_wasm_hash: new_wasm_hash.clone(),
                approver: guardian,
                approvals: pending.approvals.len(),
                threshold,
            },
        );

        if pending.approvals.len() >= threshold {
            let current_version: u32 =
                env.storage().instance().get(&StorageKey::Version).unwrap_or(1);
            env.storage().instance().remove(&StorageKey::PendingUpgrade);
            Self::run_migrations(&env, current_version, pending.new_version);
            env.storage()
                .instance()
                .set(&StorageKey::Version, &pending.new_version);
            env.deployer().update_current_contract_wasm(new_wasm_hash);

            env.events().publish(
                (Symbol::new(&env, "contract_upgraded"),),
                ContractUpgraded {
                    old_version: current_version,
                    new_version: pending.new_version,
                },
            );
        } else {
            env.storage().instance().set(&StorageKey::PendingUpgrade, &pending);
        }
    }

    /// Admin-only: cancel a pending upgrade before it collects enough
    /// guardian approvals.
    pub fn cancel_pending_upgrade(env: Env, admin: Address) {
        let stored_admin: Address = env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        if admin != stored_admin { panic_with_error!(&env, Error::Unauthorized); }
        admin.require_auth();
        if !env.storage().instance().has(&StorageKey::PendingUpgrade) {
            panic_with_error!(&env, Error::NoPendingUpgrade);
        }
        env.storage().instance().remove(&StorageKey::PendingUpgrade);
    }

    // ── Admin rotation (issue #356) ─────────────────────────────────────────

    /// Propose a new admin. Only the current admin can call this.
    ///
    /// If a guardian threshold > 0 is configured (`set_guardians`), the
    /// proposal is not armed until `threshold` guardians call
    /// `approve_admin_change`. Once armed, `new_admin` must call `accept_admin`
    /// — and that only succeeds after `ADMIN_TRANSFER_TIMELOCK` has elapsed, so
    /// a hostile or mistaken rotation has a 48h window to be noticed.
    ///
    /// Panics with `AdminTransferPending` while a transfer is already armed:
    /// re-proposing over an armed transfer would rewrite `PendingAdminSince`
    /// and reset the `ADMIN_TRANSFER_TIMELOCK`, letting the current admin
    /// keep a transfer perpetually un-acceptable (issue #457).
    pub fn propose_new_admin(env: Env, admin: Address, new_admin: Address) {
        Self::require_admin(&env, &admin);
        Self::validate_stellar_address(&env, &new_admin);

        // Issue #457: reject a fresh proposal while one is already pending —
        // the armed transfer must complete (be accepted) before another can
        // be made, so the timelock clock can never be reset by re-proposing.
        let pending: Option<Address> = env.storage().instance()
            .get(&StorageKey::PendingAdmin)
            .unwrap_or(None);
        if pending.is_some() {
            panic_with_error!(&env, Error::AdminTransferPending);
        }

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::GuardianThreshold)
            .unwrap_or(0);
        if threshold == 0 {
            env.storage().instance().set(&StorageKey::PendingAdmin, &new_admin);
            env.storage()
                .instance()
                .set(&StorageKey::PendingAdminSince, &env.ledger().timestamp());
            return;
        }

        let pending = PendingAdminChange {
            new_admin,
            approvals: Vec::new(&env),
        };
        env.storage()
            .instance()
            .set(&StorageKey::PendingAdminChange, &pending);
    }

    /// Guardian approval for a pending admin-change proposal. Once enough
    /// guardians have approved (>= threshold) the transfer is armed and the
    /// `ADMIN_TRANSFER_TIMELOCK` clock starts; `new_admin` must still call
    /// `accept_admin`.
    pub fn approve_admin_change(env: Env, guardian: Address, new_admin: Address) {
        guardian.require_auth();

        let guardians: Vec<Address> = env
            .storage()
            .instance()
            .get(&StorageKey::Guardians)
            .unwrap_or_else(|| Vec::new(&env));
        let mut is_guardian = false;
        for g in guardians.iter() {
            if g == guardian {
                is_guardian = true;
                break;
            }
        }
        if !is_guardian {
            panic_with_error!(&env, Error::NotGuardian);
        }

        let mut pending: PendingAdminChange = env
            .storage()
            .instance()
            .get(&StorageKey::PendingAdminChange)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoPendingUpgrade));
        if pending.new_admin != new_admin {
            panic_with_error!(&env, Error::NoPendingUpgrade);
        }
        for a in pending.approvals.iter() {
            if a == guardian {
                panic_with_error!(&env, Error::AlreadyApprovedAction);
            }
        }
        pending.approvals.push_back(guardian);

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::GuardianThreshold)
            .unwrap_or(0);

        if pending.approvals.len() >= threshold {
            env.storage().instance().remove(&StorageKey::PendingAdminChange);
            env.storage().instance().set(&StorageKey::PendingAdmin, &new_admin);
            // Issue #457: arm the timelock at most once per transfer — never
            // overwrite an already-armed `PendingAdminSince`, or the
            // `ADMIN_TRANSFER_TIMELOCK` would restart from this later moment.
            if !env.storage().instance().has(&StorageKey::PendingAdminSince) {
                env.storage()
                    .instance()
                    .set(&StorageKey::PendingAdminSince, &env.ledger().timestamp());
            }
        } else {
            env.storage()
                .instance()
                .set(&StorageKey::PendingAdminChange, &pending);
        }
    }

    /// Accept the proposed admin. Only the proposed admin can call this, and
    /// only once `ADMIN_TRANSFER_TIMELOCK` has elapsed since the transfer was
    /// armed (issue #356).
    pub fn accept_admin(env: Env, admin: Address) {
        let pending_admin: Address = env.storage().instance()
            .get(&StorageKey::PendingAdmin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::Unauthorized));
        if admin != pending_admin {
            panic_with_error!(&env, Error::Unauthorized);
        }
        admin.require_auth();

        let since: u64 = env.storage().instance()
            .get(&StorageKey::PendingAdminSince).unwrap_or(0);
        if env.ledger().timestamp() < since.saturating_add(ADMIN_TRANSFER_TIMELOCK) {
            panic_with_error!(&env, Error::AdminTimelockNotExpired);
        }

        env.storage().instance().set(&StorageKey::Admin, &admin);
        env.storage().instance().remove(&StorageKey::PendingAdmin);
        env.storage().instance().remove(&StorageKey::PendingAdminSince);

        env.events().publish(
            (Symbol::new(&env, "admin_updated"),),
            AdminUpdated { new_admin: admin },
        );
    }

    /// Return the proposed next admin, if a transfer is pending.
    pub fn get_pending_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&StorageKey::PendingAdmin)
    }

    /// Ledger timestamp at which the current pending admin transfer was armed,
    /// or `0` if none. `accept_admin` succeeds only once
    /// `now >= this + ADMIN_TRANSFER_TIMELOCK` (issue #356).
    pub fn get_pending_admin_since(env: Env) -> u64 {
        env.storage().instance().get(&StorageKey::PendingAdminSince).unwrap_or(0)
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

    // ── Escalation (issue #376) ───────────────────────────────────────────────

    /// Set how long a claim may sit Pending before it can be escalated.
    ///
    /// Floored at `MIN_ESCALATION_THRESHOLD` (1 hour): a near-zero threshold
    /// would make every claim escalatable the moment it is submitted, which
    /// turns the signal into noise and leaves genuinely stuck claims no easier
    /// to find than they are today.
    pub fn set_escalation_threshold(env: Env, admin: Address, threshold: u64) {
        Self::require_admin(&env, &admin);

        if threshold < MIN_ESCALATION_THRESHOLD {
            panic_with_error!(&env, Error::InvalidThresholdValue);
        }

        env.storage()
            .instance()
            .set(&StorageKey::EscalationThreshold, &threshold);

        env.events().publish(
            (Symbol::new(&env, "escalation_threshold_set"),),
            EscalationThresholdUpdated { threshold },
        );
    }

    /// The escalation threshold in seconds (default: 7 days).
    pub fn get_escalation_threshold(env: Env) -> u64 {
        Self::escalation_threshold(&env)
    }

    /// How long a claim has been waiting, and whether it can be escalated yet.
    ///
    /// Returns a value rather than panicking, so a claimant's UI can show
    /// "escalatable in 2 days" instead of offering a button that reverts.
    pub fn get_claim_age(env: Env, claim_id: u128) -> ClaimAgeInfo {
        let claim: Claim = env
            .storage()
            .persistent()
            .get(&StorageKey::Claim(claim_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ClaimNotFound));

        let threshold = Self::escalation_threshold(&env);
        let now = env.ledger().timestamp();
        let is_pending = claim.status == ClaimStatus::Pending;
        let pending_for = if is_pending {
            now.saturating_sub(claim.submitted_at)
        } else {
            0
        };

        ClaimAgeInfo {
            claim_id,
            status: claim.status,
            submitted_at: claim.submitted_at,
            pending_for,
            escalation_threshold: threshold,
            escalatable: is_pending && pending_for >= threshold,
            seconds_until_escalatable: if !is_pending {
                0
            } else {
                threshold.saturating_sub(pending_for)
            },
        }
    }

    /// Set how long after a policy's `end_time` a claim may still be submitted.
    ///
    /// The deadline bounds how stale a triggering event may be when a claim is
    /// filed. Without it, a policyholder (or anyone) could open a claim
    /// indefinitely far after the event, so a disputed or rejected claim could
    /// resurface years later against data nobody can still verify. A larger
    /// value is more forgiving to claimants; a smaller one tightens the link
    /// between the claim and the oracle data that backs it. `0` restores the
    /// historical behaviour: claims only while the policy is Active.
    ///
    /// Floored at `MIN_ESCALATION_THRESHOLD` (1 hour) so the deadline is never
    /// so short that a single missed ledger makes a valid claim impossible.
    pub fn set_claim_deadline(env: Env, admin: Address, deadline: u64) {
        Self::require_admin(&env, &admin);

        if deadline != 0 && deadline < MIN_ESCALATION_THRESHOLD {
            panic_with_error!(&env, Error::InvalidThresholdValue);
        }

        env.storage().instance().set(&StorageKey::ClaimDeadline, &deadline);

        env.events().publish(
            (Symbol::new(&env, "claim_deadline_set"),),
            ClaimDeadlineUpdated { deadline },
        );
    }

    /// The claim submission deadline in seconds (default: 30 days).
    pub fn get_claim_deadline(env: Env) -> u64 {
        Self::claim_deadline(&env)
    }

    // ── Payout Delay (issue #432) ─────────────────────────────────────────────

    /// Set the delay in seconds between claim approval and actual payout.
    ///
    /// When non-zero, a claim that passes evaluation enters `PaidPendingDelay`
    /// status and the payout is held for `delay_seconds` before becoming
    /// claimable. This gives the protocol a window to catch fraud or errors
    /// before funds leave the pool. `0` restores immediate payout behavior.
    pub fn set_payout_delay(env: Env, admin: Address, delay_seconds: u64) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&StorageKey::PayoutDelay, &delay_seconds);
        env.events().publish(
            (Symbol::new(&env, "payout_delay_set"),),
            PayoutDelayUpdated { delay_seconds },
        );
    }

    /// The configured payout delay in seconds (default: 0 — immediate).
    pub fn get_payout_delay(env: Env) -> u64 {
        Self::payout_delay(&env)
    }

    /// Configure the fraud detector (issue #437). Admin-only. Absent
    /// configuration means the detector is fully disabled; calling this
    /// once with any `FraudConfig` turns it on for every subsequent
    /// `submit_claim`.
    ///
    /// Bounds enforced:
    /// - `fraud_threshold_score <= MAX_FRAUD_SCORE`
    ///
    /// Rule score fields are bounded implicitly by their `u32` type; a
    /// pathological admin can still make a single rule fire the threshold
    /// alone. That is intentional, so `Rule 3 alone = suspicious enough`
    /// remains configurable.
    pub fn set_fraud_config(env: Env, admin: Address, config: FraudConfig) {
        Self::require_admin(&env, &admin);
        if config.fraud_threshold_score > MAX_FRAUD_SCORE {
            panic_with_error!(&env, Error::InvalidInput);
        }
        env.storage()
            .instance()
            .set(&StorageKey::FraudConfig, &config);
        env.events().publish(
            (Symbol::new(&env, "fraud_config_updated"),),
            FraudConfigUpdated {
                rate_window_secs: config.rate_window_secs,
                rate_score: config.rate_score,
                burst_window_secs: config.burst_window_secs,
                burst_count: config.burst_count,
                burst_score: config.burst_score,
                coverage_anomaly_multiplier: config.coverage_anomaly_multiplier,
                coverage_score: config.coverage_score,
                fraud_threshold_score: config.fraud_threshold_score,
                mode: config.mode,
            },
        );
    }

    /// The fraud detector configuration currently in force, or `None` when
    /// the detector is disabled (issue #437).
    pub fn get_fraud_config(env: Env) -> Option<FraudConfig> {
        Self::fraud_config(&env)
    }

    /// The frozen fraud snapshot for a claim (issue #437). `None` when the
    /// claim was never flagged (either the detector is off, the score was
    /// below threshold, or the mode was Block and no state was written).
    pub fn get_fraud_record(env: Env, claim_id: u128) -> Option<FraudRecord> {
        env.storage()
            .persistent()
            .get(&StorageKey::FraudRecord(claim_id))
    }

    /// The claimant's aggregate history the detector uses. Returns the
    /// empty aggregate when the claimant has never submitted (issue #437).
    pub fn get_claimant_history(env: Env, claimant: Address) -> ClaimantHistory {
        Self::claimant_history(&env, &claimant)
    }

    /// The configured payout delay, or the default.
    fn payout_delay(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&StorageKey::PayoutDelay)
            .unwrap_or(0)
    }

    /// Claim the payout for an approved claim after the payout delay has elapsed.
    ///
    /// When a payout delay is configured, approved claims enter Paid/PartiallyPaid
    /// status but funds are not transferred until the delay passes. This function
    /// completes the transfer. Callable by anyone — the claim is already approved.
    pub fn claim_payout(env: Env, claim_id: u128) -> i128 {
        Self::require_not_paused(&env);

        let mut claim: Claim = env.storage().persistent()
            .get(&StorageKey::Claim(claim_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ClaimNotFound));

        let payout_ready_at = claim.payout_ready_at
            .unwrap_or_else(|| panic_with_error!(&env, Error::PayoutDelayNotElapsed));

        let now = env.ledger().timestamp();
        if now < payout_ready_at {
            panic_with_error!(&env, Error::PayoutDelayNotElapsed);
        }

        let paid_amount = claim.paid_amount
            .unwrap_or_else(|| panic_with_error!(&env, Error::ClaimNotFound));

        // Clear payout_ready_at so this cannot be called again
        claim.payout_ready_at = None;
        env.storage().persistent().set(&StorageKey::Claim(claim_id), &claim);

        // Execute the actual payout
        let policy_engine: Address = env.storage().instance()
            .get(&StorageKey::PolicyEngine)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let risk_pool: Address = env.storage().instance()
            .get(&StorageKey::RiskPool)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        PolicyEngineClient::new(&env, &policy_engine)
            .pay_claim(&env.current_contract_address(), &claim.policy_id);
        RiskPoolClient::new(&env, &risk_pool)
            .release_for_claim(&env.current_contract_address(), &claim.policy_id);

        env.events().publish(
            (Symbol::new(&env, "payout_released"),),
            (claim_id, claim.policy_id, paid_amount),
        );

        paid_amount
    }

    /// Escalate a claim that has been Pending past the threshold.
    ///
    /// A claim that nothing processes is worse than a rejected one: a
    /// rejection can be disputed, but a claim stuck in Pending gives the
    /// claimant nothing to act on and no signal that anything is wrong. This
    /// moves it to `Escalated`, emits `ClaimEscalated`, and takes it out of the
    /// automated pending queue so manual review can pick it up.
    ///
    /// Permissionless by design. The person with the strongest interest in
    /// escalating is the claimant who is waiting, and requiring a keeper or the
    /// admin to do it would mean the party responsible for the delay is also
    /// the only party able to flag it.
    ///
    /// Escalation does not decide the claim. It records that processing is
    /// overdue and hands it to review — the outcome still comes from a normal
    /// resolution path.
    pub fn escalate_claim(env: Env, caller: Address, claim_id: u128) {
        caller.require_auth();
        Self::require_not_paused(&env);

        let mut claim: Claim = env
            .storage()
            .persistent()
            .get(&StorageKey::Claim(claim_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ClaimNotFound));

        if claim.status != ClaimStatus::Pending {
            panic_with_error!(&env, Error::NotEscalatable);
        }

        let now = env.ledger().timestamp();
        let pending_for = now.saturating_sub(claim.submitted_at);
        if pending_for < Self::escalation_threshold(&env) {
            panic_with_error!(&env, Error::NotEscalatable);
        }

        claim.status = ClaimStatus::Escalated;
        let policy_id = claim.policy_id;
        let claimant = claim.claimant.clone();

        env.storage()
            .persistent()
            .set(&StorageKey::Claim(claim_id), &claim);
        Self::extend_claim_ttl(&env, &StorageKey::Claim(claim_id));

        // Drop it from the automated queue — a keeper sweeping pending claims
        // should not keep retrying one that has been handed to review.
        Self::remove_from_pending(&env, claim_id);

        env.events().publish(
            (Symbol::new(&env, "claim_escalated"),),
            ClaimEscalated {
                claim_id,
                policy_id,
                claimant,
                pending_for,
                escalated_by: caller,
            },
        );
    }

    /// Claim IDs that are Pending and past the escalation threshold.
    ///
    /// Lets a monitoring job find stuck claims in one call rather than probing
    /// each one, which is what makes the threshold actionable in practice.
    pub fn get_escalatable_claims(env: Env) -> Vec<u128> {
        let pending: Vec<u128> = env
            .storage()
            .instance()
            .get(&StorageKey::PendingClaims)
            .unwrap_or_else(|| Vec::new(&env));

        let threshold = Self::escalation_threshold(&env);
        let now = env.ledger().timestamp();
        let mut overdue: Vec<u128> = Vec::new(&env);

        for i in 0..pending.len() {
            let cid = pending.get_unchecked(i);
            if let Some(claim) = env
                .storage()
                .persistent()
                .get::<_, Claim>(&StorageKey::Claim(cid))
            {
                if claim.status == ClaimStatus::Pending
                    && now.saturating_sub(claim.submitted_at) >= threshold
                {
                    overdue.push_back(cid);
                }
            }
        }

        overdue
    }

    // ── Identity Verification ───────────────────────────────────────────────

    /// Admin-only: mark a product/category as requiring identity verification.
    /// Claimants must have verified identity (via oracle-verifier attestation)
    /// to claim payouts for this product.
    pub fn require_identity_for_category(
        env: Env,
        admin: Address,
        product_id: u128,
        id_type: Symbol,
    ) {
        Self::require_admin(&env, &admin);

        let key = StorageKey::IdentityRequirement(product_id);
        env.storage().instance().set(&key, &id_type);

        env.events().publish(
            (Symbol::new(&env, "identity_requirement_set"),),
            (product_id, id_type),
        );
    }

    /// Admin-only: remove identity verification requirement for a product.
    pub fn remove_identity_requirement(env: Env, admin: Address, product_id: u128) {
        Self::require_admin(&env, &admin);

        let key = StorageKey::IdentityRequirement(product_id);
        env.storage().instance().remove(&key);

        env.events().publish(
            (Symbol::new(&env, "identity_requirement_removed"),),
            product_id,
        );
    }

    /// Admin-only: manually verify a claimant's identity for a claim.
    /// Used when off-chain identity verification is completed or approved by DAO.
    pub fn verify_claimant_identity(
        env: Env,
        admin: Address,
        claim_id: u128,
        id_type: Symbol,
    ) {
        Self::require_admin(&env, &admin);

        let mut claim: Claim = env
            .storage()
            .persistent()
            .get(&StorageKey::Claim(claim_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ClaimNotFound));

        let now = env.ledger().timestamp();
        claim.identity_verified = true;
        claim.verification_type = Some(id_type.clone());
        claim.verification_time = Some(now);

        let key = StorageKey::Claim(claim_id);
        env.storage().persistent().set(&key, &claim);
        Self::extend_claim_ttl(&env, &key);

        env.events().publish(
            (Symbol::new(&env, "identity_verified"),),
            (claim_id, id_type, now),
        );
    }

    /// Get the identity requirement for a product (if set).
    pub fn get_identity_requirement(env: Env, product_id: u128) -> Option<Symbol> {
        env.storage()
            .instance()
            .get(&StorageKey::IdentityRequirement(product_id))
    }

    /// Check if a claimant's identity is verified for a particular ID type.
    /// This is a utility for off-chain systems to verify attestation status.
    pub fn is_identity_verified(env: Env, claim_id: u128) -> bool {
        env.storage()
            .persistent()
            .get(&StorageKey::Claim(claim_id))
            .map(|claim: Claim| claim.identity_verified)
            .unwrap_or(false)
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Evaluate a pending claim and settle it against the configured oracle trigger.
    ///
    /// This internal helper is the core claim lifecycle step. It first validates
    /// that the claim is still pending, then resolves the oracle verifier,
    /// policy engine, and risk pool contract addresses from storage. It builds a
    /// trigger condition from the policy's oracle data type, key, threshold, and
    /// comparison mode, and asks the oracle verifier whether the trigger is
    /// currently met using a fresh-data check.
    ///
    /// If the trigger is met, the claim is marked as paid, the policy engine is
    /// instructed to pay the claim, and the risk-pool coverage lock is released
    /// in the same transaction. If the trigger is not met, the claim is marked
    /// as rejected and no payout is issued. In either case, the claim record is
    /// persisted, its pending-queue entry is removed, and a settlement event is
    /// emitted so the outcome is observable off-chain.
    fn evaluate_and_settle(
        env: &Env,
        claim: &mut Claim,
        policy: &parashield_policy_engine::Policy,
        partial_payout_bps: Option<u32>,
    ) -> ClaimResult {
        // Validate claim is in Pending state before transitioning (atomic state guard)  
        if claim.status != ClaimStatus::Pending {
            panic_with_error!(env, Error::AlreadyProcessed);
        }
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

        let payout_delay = Self::payout_delay(env);

        let result = if trigger_met {
            // Determine payout: full or partial based on partial_payout_bps.
            // Parametric model design: pays the pre-agreed coverage amount (or pre-set partial bps)
            // automatically when the oracle condition is verified, without assessing post-hoc actual loss.
            let bps = partial_payout_bps.unwrap_or(10_000);
            let effective_bps = if bps > 10_000 { 10_000 } else { bps };
            let now = env.ledger().timestamp();

            if effective_bps >= 10_000 {
                // Full payment
                claim.status = ClaimStatus::Paid;
                claim.paid_amount = Some(claim.coverage_amount);
                claim.partial_payout_bps = Some(10_000);

                if payout_delay > 0 {
                    // Delay payout: record when payout becomes available
                    claim.payout_ready_at = Some(now.saturating_add(payout_delay));
                } else {
                    // Immediate payout
                    PolicyEngineClient::new(env, &policy_engine)
                        .pay_claim(&env.current_contract_address(), &claim.policy_id);
                    RiskPoolClient::new(env, &risk_pool)
                        .release_for_claim(&env.current_contract_address(), &claim.policy_id);
                }
                ClaimResult::Paid
            } else {
                // Partial payment: calculate proportional payout
                let paid = claim.coverage_amount * (effective_bps as i128) / 10_000;
                claim.status = ClaimStatus::PartiallyPaid;
                claim.paid_amount = Some(paid);
                claim.partial_payout_bps = Some(effective_bps);

                if payout_delay > 0 {
                    claim.payout_ready_at = Some(now.saturating_add(payout_delay));
                } else {
                    PolicyEngineClient::new(env, &policy_engine)
                        .pay_claim(&env.current_contract_address(), &claim.policy_id);
                    RiskPoolClient::new(env, &risk_pool)
                        .release_for_claim(&env.current_contract_address(), &claim.policy_id);
                }
                ClaimResult::PartiallyPaid
            }
        } else {
            claim.status = ClaimStatus::Rejected;
            ClaimResult::Rejected
        };

        env.storage().persistent().set(&StorageKey::Claim(claim.id), claim);
        env.storage().persistent().extend_ttl(&StorageKey::Claim(claim.id), TTL_THRESHOLD, TTL_EXTEND_TO);

        // The claim is now settled (Paid/Rejected) — drop it from the pending
        // queue so it is neither re-evaluated nor allowed to grow the queue
        // without bound.
        Self::remove_from_pending(env, claim.id);

        // Emit specific event for rejected/paid claims to enable off-chain monitoring
        if trigger_met {
            env.events().publish(
                (Symbol::new(env, "claim_paid"),),
                (claim.id, claim.policy_id, claim.coverage_amount),
            );
        } else {
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

    /// The configured escalation threshold, or the default.
    fn escalation_threshold(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&StorageKey::EscalationThreshold)
            .unwrap_or(DEFAULT_ESCALATION_THRESHOLD)
    }

    /// The configured claim submission deadline, or the default.
    fn claim_deadline(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&StorageKey::ClaimDeadline)
            .unwrap_or(DEFAULT_CLAIM_DEADLINE)
    }

    fn remove_from_pending(env: &Env, claim_id: u128) {
        let mut pending: Vec<u128> = env.storage().instance()
            .get(&StorageKey::PendingClaims)
            .unwrap_or_else(|| Vec::new(env));
        if let Some(idx) = pending.first_index_of(claim_id) {
            pending.remove(idx);
            env.storage().instance().set(&StorageKey::PendingClaims, &pending);
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

    /// Extend a `Claim`/`PolicyClaim` entry's TTL to cover
    /// `CLAIM_RETENTION_SECONDS` from now (clamped to the network's max TTL),
    /// so a claim record survives from submission through settlement or an
    /// open-ended dispute even if it's only ever written once (issue #246).
    fn extend_claim_ttl(env: &Env, key: &StorageKey) {
        let desired_ledgers = (CLAIM_RETENTION_SECONDS / LEDGER_SECONDS) as u32;
        let extend_to = desired_ledgers.min(env.storage().max_ttl());
        env.storage().persistent().extend_ttl(key, extend_to, extend_to);
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

    // ── Fraud detection helpers (issue #437) ──────────────────────────────

    /// Fraud detector configuration if the admin has set one, else `None`.
    /// `None` means the detector is disabled and every submission proceeds
    /// unchecked (pre-issue-437 behaviour).
    fn fraud_config(env: &Env) -> Option<FraudConfig> {
        env.storage().instance().get(&StorageKey::FraudConfig)
    }

    /// A claimant's aggregate history, or an empty one for a first-time
    /// submitter. `unwrap_or_else` ensures fresh submitters do not pay a
    /// storage read that could fail.
    fn claimant_history(env: &Env, claimant: &Address) -> ClaimantHistory {
        env.storage()
            .persistent()
            .get(&StorageKey::ClaimantHistory(claimant.clone()))
            .unwrap_or(ClaimantHistory {
                last_submission_at: 0,
                burst_bucket_start: 0,
                burst_count: 0,
                max_coverage_ever: 0,
                total_submissions: 0,
            })
    }

    /// Score this submission against the configured rules. Returns
    /// `(score, flags)`, both zero when the detector is disabled or when
    /// no rule fires.
    ///
    /// Pure function over the passed-in `now` / `history` / `coverage` so it
    /// can be tested without spinning up the full cross-contract world.
    fn score_submission(
        cfg: &FraudConfig,
        now: u64,
        history: &ClaimantHistory,
        coverage_amount: i128,
    ) -> (u32, u32) {
        let mut score: u32 = 0;
        let mut flags: u32 = 0;

        // Rule 1: rapid resubmission. Only fires once a claimant has any
        // prior submission on record; a first-ever claim never trips it.
        if cfg.rate_window_secs > 0
            && cfg.rate_score > 0
            && history.last_submission_at > 0
            && now.saturating_sub(history.last_submission_at) < cfg.rate_window_secs
        {
            score = score.saturating_add(cfg.rate_score);
            flags |= FRAUD_FLAG_RATE;
        }

        // Rule 2: multi-policy burst. Fires when *this* submission would
        // be the `burst_count`-th inside the active window bucket. The
        // caller updates the bucket after admission; here we compute the
        // hypothetical post-increment count using the stored bucket.
        if cfg.burst_window_secs > 0 && cfg.burst_count > 0 && cfg.burst_score > 0 {
            let bucket_alive = history.burst_bucket_start > 0
                && now.saturating_sub(history.burst_bucket_start) < cfg.burst_window_secs;
            let projected = if bucket_alive {
                history.burst_count.saturating_add(1)
            } else {
                1
            };
            if projected >= cfg.burst_count {
                score = score.saturating_add(cfg.burst_score);
                flags |= FRAUD_FLAG_BURST;
            }
        }

        // Rule 3: coverage anomaly. Requires prior coverage on record —
        // a first-ever claim cannot be an anomaly relative to nothing.
        if cfg.coverage_anomaly_multiplier > 0
            && cfg.coverage_score > 0
            && history.max_coverage_ever > 0
        {
            let threshold = history
                .max_coverage_ever
                .saturating_mul(cfg.coverage_anomaly_multiplier as i128);
            if coverage_amount > threshold {
                score = score.saturating_add(cfg.coverage_score);
                flags |= FRAUD_FLAG_COVERAGE;
            }
        }

        (score, flags)
    }

    /// Fold a just-admitted submission into the claimant's history. Bumps
    /// `total_submissions` and `max_coverage_ever` on every call; resets or
    /// extends the burst bucket based on how much time has passed since the
    /// previous submission. Called by `submit_claim` after the fraud gate.
    fn update_claimant_history(
        env: &Env,
        claimant: &Address,
        now: u64,
        coverage_amount: i128,
        burst_window_secs: u64,
    ) {
        let prev = Self::claimant_history(env, claimant);
        let (bucket_start, bucket_count) = if burst_window_secs == 0 {
            // Feature effectively off — do not carry a stale bucket forward.
            (0, 0)
        } else if prev.burst_bucket_start == 0
            || now.saturating_sub(prev.burst_bucket_start) >= burst_window_secs
        {
            (now, 1)
        } else {
            (prev.burst_bucket_start, prev.burst_count.saturating_add(1))
        };

        let next = ClaimantHistory {
            last_submission_at: now,
            burst_bucket_start: bucket_start,
            burst_count: bucket_count,
            max_coverage_ever: prev.max_coverage_ever.max(coverage_amount),
            total_submissions: prev.total_submissions.saturating_add(1),
        };
        let key = StorageKey::ClaimantHistory(claimant.clone());
        env.storage().persistent().set(&key, &next);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    /// Run the configured fraud detector against a hypothetical submission.
    /// If the detector is off (no `FraudConfig`) this returns immediately
    /// with `(0, 0)` and does nothing else.
    ///
    /// If the score meets or exceeds the configured threshold and mode is
    /// `Block`, the function panics with `FraudSuspected` after emitting
    /// `FraudFlagged`. If mode is `FlagOnly`, it emits the event, persists a
    /// `FraudRecord`, and returns so the caller can continue writing the
    /// claim.
    ///
    /// Called from `submit_claim` after all existing gates so a rejection at
    /// this point does not leak partial state.
    fn evaluate_fraud(
        env: &Env,
        claim_id: u128,
        claimant: &Address,
        coverage_amount: i128,
    ) -> (u32, u32) {
        let Some(cfg) = Self::fraud_config(env) else {
            return (0, 0);
        };
        let now = env.ledger().timestamp();
        let history = Self::claimant_history(env, claimant);
        let (score, flags) = Self::score_submission(&cfg, now, &history, coverage_amount);
        if score >= cfg.fraud_threshold_score && cfg.fraud_threshold_score > 0 {
            env.events().publish(
                (Symbol::new(env, "fraud_flagged"),),
                FraudFlagged {
                    claim_id,
                    claimant: claimant.clone(),
                    score,
                    flags,
                    mode: cfg.mode.clone(),
                },
            );
            match cfg.mode {
                FraudMode::Block => panic_with_error!(env, Error::FraudSuspected),
                FraudMode::FlagOnly => {
                    let record = FraudRecord {
                        claim_id,
                        score,
                        flags,
                        checked_at: now,
                    };
                    let key = StorageKey::FraudRecord(claim_id);
                    env.storage().persistent().set(&key, &record);
                    env.storage()
                        .persistent()
                        .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
                }
            }
        }
        (score, flags)
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
