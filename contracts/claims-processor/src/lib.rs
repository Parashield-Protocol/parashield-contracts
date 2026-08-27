#![allow(dead_code)]
#![allow(unused_imports)]
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
    Address, BytesN, Env, Vec, Symbol,
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

#[soroban_sdk::contractclient(name = "IdentityVerifierClient")]
trait IIdentityVerifier {
    fn is_verified(env: Env, address: Address) -> bool;
}

// ─── Storage TTL ──────────────────────────────────────────────────────────────

/// Extend a persistent entry's TTL once it has fewer than ~30 days of life left
/// (at ~5s/ledger).
// Issue #342: kept in sync by hand across all 5 contracts (governance-dao,
// risk-pool, policy-engine, oracle-verifier, claims-processor) — extracting
// to a shared crate is a real follow-up, not done here to avoid touching
// every contract's Cargo.toml in one pass.
const TTL_THRESHOLD: u32 = 518_400;
/// Extend persistent entries out to ~1 year (at ~5s/ledger) so pending claims
/// survive long enough to be processed.
const TTL_EXTEND_TO: u32 = 6_312_000;

/// Grace period between an admin transfer being fully proposed/approved and the
/// proposed admin being able to `accept_admin` (issue #356). Hand-synced across
/// the 4 contracts that expose admin rotation (policy-engine, risk-pool,
/// oracle-verifier, claims-processor).
const ADMIN_TRANSFER_TIMELOCK: u64 = 48 * 60 * 60;

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
    /// Whether `attestor` is authorized to submit cross-chain attestations
    /// for `chain_id` — (chain_id, attestor) → bool.
    CrossChainAttestor(Symbol, Address),
    /// Registered attestor addresses for a chain_id (`Vec<Address>`).
    CrossChainAttestorList(Symbol),
    /// The most recent cross-chain attestation submitted for a policy_id.
    CrossChainAttestation(u128),
    /// Optional identity verifier contract address (Address).
    IdentityVerifier,
    /// Whether identity verification is required for claimants (bool).
    IdentityVerificationRequired,
    /// Optional stability fund contract address for payout smoothing (Address).
    StabilityFund,
    /// Payout smoothing factor in basis points (0-10000). When set, the
    /// actual payout is blended toward the historical average to reduce
    /// volatility. 0 = no smoothing (default); 10000 = full smoothing
    /// (payout always equals the historical average).
    PayoutSmoothingBps,
    /// Running historical average payout in USDC stroops (i128), updated
    /// with exponential moving average on each settlement.
    HistoricalAveragePayout,
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
    AttestorNotRegistered = 17,
    NoAttestation        = 18,
    AttestationStale     = 19,
    IdentityNotVerified  = 20,
    InvalidSmoothingBps  = 21,
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

    // ── Cross-Chain Verification (issue #380) ────────────────────────────────
    //
    // Stellar remains the canonical verification path: `evaluate_and_settle`
    // (used by `process_claim`/`auto_process`) is untouched and always reads
    // the Stellar oracle-verifier. This adds a second, explicit path for
    // policies whose trigger condition can only be observed on another
    // chain — a registered attestor (a relayer or bridge the admin trusts
    // for that specific chain_id) submits a signed observation, which
    // `process_cross_chain_claim` compares against the policy's trigger the
    // same way the Stellar oracle path does. The trust boundary is explicit
    // and per-chain: adding a chain means registering an attestor for it,
    // never touches the Stellar oracle path, and each attestation is only
    // ever used for the one policy it was submitted for.

    /// Admin-only: authorize `attestor` to submit cross-chain attestations
    /// for `chain_id`.
    pub fn add_cross_chain_attestor(env: Env, admin: Address, chain_id: Symbol, attestor: Address) {
        Self::require_admin(&env, &admin);
        let key = StorageKey::CrossChainAttestor(chain_id.clone(), attestor.clone());
        env.storage().persistent().set(&key, &true);
        env.storage().persistent().extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);

        let list_key = StorageKey::CrossChainAttestorList(chain_id.clone());
        let mut list: Vec<Address> = env.storage().instance()
            .get(&list_key).unwrap_or_else(|| Vec::new(&env));
        if list.first_index_of(attestor.clone()).is_none() {
            list.push_back(attestor.clone());
            env.storage().instance().set(&list_key, &list);
        }

        env.events().publish(
            (Symbol::new(&env, "cross_chain_attestor_added"),),
            CrossChainAttestorAdded { chain_id, attestor },
        );
    }

    /// Admin-only: revoke `attestor`'s authorization for `chain_id`.
    pub fn remove_cross_chain_attestor(env: Env, admin: Address, chain_id: Symbol, attestor: Address) {
        Self::require_admin(&env, &admin);
        env.storage().persistent()
            .remove(&StorageKey::CrossChainAttestor(chain_id.clone(), attestor.clone()));

        let list_key = StorageKey::CrossChainAttestorList(chain_id.clone());
        let list: Vec<Address> = env.storage().instance()
            .get(&list_key).unwrap_or_else(|| Vec::new(&env));
        if let Some(idx) = list.first_index_of(attestor.clone()) {
            let mut pruned = list;
            pruned.remove(idx);
            env.storage().instance().set(&list_key, &pruned);
        }

        env.events().publish(
            (Symbol::new(&env, "cross_chain_attestor_removed"),),
            CrossChainAttestorRemoved { chain_id, attestor },
        );
    }

    /// Whether `attestor` is currently authorized for `chain_id`.
    pub fn is_cross_chain_attestor(env: Env, chain_id: Symbol, attestor: Address) -> bool {
        env.storage().persistent()
            .get(&StorageKey::CrossChainAttestor(chain_id, attestor))
            .unwrap_or(false)
    }

    /// Return the registered attestor addresses for `chain_id`.
    pub fn get_cross_chain_attestors(env: Env, chain_id: Symbol) -> Vec<Address> {
        env.storage().instance()
            .get(&StorageKey::CrossChainAttestorList(chain_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Submit a cross-chain attestation for `policy_id`'s trigger condition.
    ///
    /// Only an address registered via `add_cross_chain_attestor` for
    /// `chain_id` may call this. `proof_hash` is opaque to the contract — a
    /// hash of whatever off-chain proof (light-client proof, relayer
    /// message, oracle report) backs `observed_value`, kept for audit/
    /// dispute purposes but not verified on-chain. Overwrites any prior
    /// attestation for the same policy; only the latest is used to settle.
    pub fn submit_cross_chain_attestation(
        env: Env,
        attestor: Address,
        policy_id: u128,
        chain_id: Symbol,
        observed_value: i128,
        proof_hash: BytesN<32>,
        timestamp: u64,
    ) {
        attestor.require_auth();

        let authorized: bool = env.storage().persistent()
            .get(&StorageKey::CrossChainAttestor(chain_id.clone(), attestor.clone()))
            .unwrap_or(false);
        if !authorized {
            panic_with_error!(&env, Error::AttestorNotRegistered);
        }

        let now = env.ledger().timestamp();
        if timestamp > now {
            panic_with_error!(&env, Error::AttestationStale);
        }

        let attestation = CrossChainAttestation {
            chain_id: chain_id.clone(),
            attestor: attestor.clone(),
            observed_value,
            proof_hash,
            timestamp,
        };
        let key = StorageKey::CrossChainAttestation(policy_id);
        env.storage().persistent().set(&key, &attestation);
        env.storage().persistent().extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);

        env.events().publish(
            (Symbol::new(&env, "cc_attestation_submitted"),),
            CrossChainAttestationSubmitted {
                policy_id,
                chain_id,
                attestor,
                observed_value,
                timestamp,
            },
        );
    }

    /// Return the most recent cross-chain attestation submitted for
    /// `policy_id`, if any.
    pub fn get_cross_chain_attestation(env: Env, policy_id: u128) -> Option<CrossChainAttestation> {
        env.storage().persistent().get(&StorageKey::CrossChainAttestation(policy_id))
    }

    // ── Identity Verification (issue #384) ──────────────────────────────────

    /// Admin-only: set the identity verifier contract address. The contract
    /// must implement `is_verified(Address) -> bool`.
    pub fn set_identity_verifier(env: Env, admin: Address, verifier: Address) {
        Self::require_admin(&env, &admin);
        Self::validate_stellar_address(&env, &verifier);
        env.storage().instance().set(&StorageKey::IdentityVerifier, &verifier);
        env.events().publish(
            (Symbol::new(&env, "identity_verifier_set"),),
            verifier,
        );
    }

    /// Admin-only: enable or disable mandatory identity verification for
    /// claimants. When enabled, `submit_claim` and `auto_process` check
    /// that the policyholder has passed KYC via the configured identity
    /// verifier before accepting or processing a claim.
    pub fn set_kyc_required(env: Env, admin: Address, required: bool) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&StorageKey::IdentityVerificationRequired, &required);
        env.events().publish(
            (Symbol::new(&env, "kyc_required_toggled"),),
            required,
        );
    }

    /// Whether identity verification is currently required.
    pub fn is_kyc_required(env: Env) -> bool {
        env.storage().instance()
            .get(&StorageKey::IdentityVerificationRequired)
            .unwrap_or(false)
    }

    /// Return the configured identity verifier contract address, if any.
    pub fn get_identity_verifier(env: Env) -> Option<Address> {
        env.storage().instance().get(&StorageKey::IdentityVerifier)
    }

    /// Panic with `IdentityNotVerified` if identity verification is enabled
    /// and the claimant has not passed KYC via the identity verifier.
    fn require_identity_verification(env: &Env, claimant: &Address) {
        let required: bool = env.storage().instance()
            .get(&StorageKey::IdentityVerificationRequired)
            .unwrap_or(false);
        if !required {
            return;
        }
        let verifier_addr: Address = env.storage().instance()
            .get(&StorageKey::IdentityVerifier)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        let verified = IdentityVerifierClient::new(env, &verifier_addr)
            .is_verified(claimant);
        if !verified {
            panic_with_error!(env, Error::IdentityNotVerified);
        }
    }

    // ── Payout Stability (issue #398) ──────────────────────────────────────

    /// Admin-only: set the stability fund address. When payout smoothing is
    /// enabled, excess payouts (above the historical average) draw from this
    /// fund, and below-average payouts deposit the surplus here.
    pub fn set_stability_fund(env: Env, admin: Address, fund: Address) {
        Self::require_admin(&env, &admin);
        Self::validate_stellar_address(&env, &fund);
        env.storage().instance().set(&StorageKey::StabilityFund, &fund);
        env.events().publish(
            (Symbol::new(&env, "stability_fund_set"),),
            fund,
        );
    }

    /// Admin-only: configure payout smoothing. `smoothing_bps` controls how
    /// aggressively payouts are blended toward the historical average:
    ///   - 0 = no smoothing (default, payout = coverage_amount)
    ///   - 5000 = 50% smoothing (payout = 50% coverage + 50% avg)
    ///   - 10000 = full smoothing (payout always = historical average)
    ///
    /// Requires a stability fund to be set first.
    pub fn set_payout_smoothing_bps(env: Env, admin: Address, smoothing_bps: u32) {
        Self::require_admin(&env, &admin);
        if smoothing_bps > 10_000 {
            panic_with_error!(&env, Error::InvalidSmoothingBps);
        }
        if smoothing_bps > 0 {
            // Ensure stability fund is configured when smoothing is enabled
            if !env.storage().instance().has(&StorageKey::StabilityFund) {
                panic_with_error!(&env, Error::NotInitialized);
            }
        }
        env.storage().instance().set(&StorageKey::PayoutSmoothingBps, &smoothing_bps);
        env.events().publish(
            (Symbol::new(&env, "payout_smoothing_updated"),),
            smoothing_bps,
        );
    }

    /// Return the current payout smoothing configuration.
    pub fn get_payout_smoothing_bps(env: Env) -> u32 {
        env.storage().instance()
            .get(&StorageKey::PayoutSmoothingBps)
            .unwrap_or(0)
    }

    /// Return the running historical average payout.
    pub fn get_historical_average_payout(env: Env) -> i128 {
        env.storage().instance()
            .get(&StorageKey::HistoricalAveragePayout)
            .unwrap_or(0)
    }

    /// Apply payout smoothing: blend the raw payout toward the historical
    /// average and emit an event for the backend to handle the stability
    /// fund transfer. Returns the smoothed payout amount.
    fn apply_payout_smoothing(env: &Env, raw_payout: i128) -> i128 {
        let smoothing_bps: u32 = env.storage().instance()
            .get(&StorageKey::PayoutSmoothingBps)
            .unwrap_or(0);
        if smoothing_bps == 0 {
            return raw_payout;
        }

        let avg: i128 = env.storage().instance()
            .get(&StorageKey::HistoricalAveragePayout)
            .unwrap_or(raw_payout);

        // smoothed = (raw * (10000 - smoothing_bps) + avg * smoothing_bps) / 10000
        let inv_bps = (10_000u32).saturating_sub(smoothing_bps) as i128;
        let smooth_bps = smoothing_bps as i128;
        let smoothed = (raw_payout * inv_bps + avg * smooth_bps) / 10_000;

        // Update historical average with exponential moving average:
        // new_avg = (old_avg * 9 + smoothed) / 10  (EMA with alpha=0.1)
        let new_avg = (avg * 9 + smoothed) / 10;
        env.storage().instance().set(&StorageKey::HistoricalAveragePayout, &new_avg);

        // Emit event for backend to handle stability fund transfer
        let diff = raw_payout - smoothed;
        if diff != 0 {
            if let Some(fund_addr) = env.storage().instance().get::<_, Address>(&StorageKey::StabilityFund) {
                env.events().publish(
                    (Symbol::new(env, "payout_stabilized"),),
                    (raw_payout, smoothed, diff, fund_addr),
                );
            }
        }

        smoothed
    }

    /// Settle a claim using a previously submitted cross-chain attestation
    /// instead of the Stellar oracle-verifier.
    ///
    /// Requires an attestation on file for the claim's policy, no older than
    /// `staleness_threshold` seconds (the same freshness bar the Stellar
    /// path enforces). The attestation's `observed_value` is compared
    /// against the policy's trigger threshold/comparison exactly as the
    /// Stellar oracle path does; payout/rejection then follows the same
    /// settlement logic as `process_claim`.
    pub fn process_cross_chain_claim(
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

        let attestation: CrossChainAttestation = env.storage().persistent()
            .get(&StorageKey::CrossChainAttestation(claim.policy_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoAttestation));

        let staleness_threshold: u64 = env.storage().instance()
            .get(&StorageKey::StalenessThreshold)
            .unwrap_or(604_800u64);
        let now = env.ledger().timestamp();
        if now.saturating_sub(attestation.timestamp) > staleness_threshold {
            panic_with_error!(&env, Error::AttestationStale);
        }

        let trigger_met = evaluate_comparison(
            attestation.observed_value,
            policy.trigger_threshold,
            &policy.trigger_comparison,
        );

        Self::settle_claim(&env, &mut claim, partial_payout_bps, trigger_met)
    }

    // ── Claim Submission ─────────────────────────────────────────────────────

    /// Manually submit a claim for a policy. Returns the new claim ID.
    /// Only the policyholder may submit; only one claim per policy.
    pub fn submit_claim(env: Env, claimant: Address, policy_id: u128) -> u128 {
        claimant.require_auth();
        Self::require_not_paused(&env);
        Self::require_identity_verification(&env, &claimant);

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
        // we verify end_time at the contract level.
        let now = env.ledger().timestamp();
        if policy.end_time > 0 && now > policy.end_time {
            panic_with_error!(&env, Error::PolicyExpired);
        }

        let claim_id   = Self::next_claim_id(&env);
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
                claimant,
                coverage_amount: policy.coverage_amount,
            },
        );

        claim_id
    }

    /// Process an existing pending claim. Reads oracle data and pays out or rejects.
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

        Self::require_identity_verification(&env, &policy.policyholder);

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
            let result = Self::evaluate_and_settle(&env, &mut claim, &policy, None);
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
    /// a hostile or mistaken rotation has a 48h window to be noticed and
    /// countered with a fresh `propose_new_admin`.
    pub fn propose_new_admin(env: Env, admin: Address, new_admin: Address) {
        Self::require_admin(&env, &admin);
        Self::validate_stellar_address(&env, &new_admin);

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
            env.storage()
                .instance()
                .set(&StorageKey::PendingAdminSince, &env.ledger().timestamp());
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

        Self::settle_claim(env, claim, partial_payout_bps, trigger_met)
    }

    /// Shared settlement path used by both the Stellar-oracle evaluation
    /// (`evaluate_and_settle`) and the cross-chain path
    /// (`process_cross_chain_claim`) — the only difference between the two
    /// verification routes is how `trigger_met` was decided; once it's
    /// known, paying out, rejecting, and bookkeeping are identical.
    fn settle_claim(
        env: &Env,
        claim: &mut Claim,
        partial_payout_bps: Option<u32>,
        trigger_met: bool,
    ) -> ClaimResult {
        let policy_engine: Address = env.storage().instance()
            .get(&StorageKey::PolicyEngine)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        let risk_pool: Address = env.storage().instance()
            .get(&StorageKey::RiskPool)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));

        claim.trigger_met  = trigger_met;
        claim.processed_at = Some(env.ledger().timestamp());

        let result = if trigger_met {
            // Determine payout: full or partial based on partial_payout_bps.
            let bps = partial_payout_bps.unwrap_or(10_000);
            let effective_bps = if bps > 10_000 { 10_000 } else { bps };

            if effective_bps >= 10_000 {
                // Full payment — apply payout smoothing if configured
                let raw_payout = claim.coverage_amount;
                let smoothed_payout = Self::apply_payout_smoothing(env, raw_payout);
                claim.status = ClaimStatus::Paid;
                claim.paid_amount = Some(smoothed_payout);
                claim.partial_payout_bps = Some(10_000);
                PolicyEngineClient::new(env, &policy_engine)
                    .pay_claim(&env.current_contract_address(), &claim.policy_id);
                // Atomic lock release
                RiskPoolClient::new(env, &risk_pool)
                    .release_for_claim(&env.current_contract_address(), &claim.policy_id);
                ClaimResult::Paid
            } else {
                // Partial payment: calculate proportional payout, then smooth
                let raw_paid = claim.coverage_amount * (effective_bps as i128) / 10_000;
                let smoothed_paid = Self::apply_payout_smoothing(env, raw_paid);
                claim.status = ClaimStatus::PartiallyPaid;
                claim.paid_amount = Some(smoothed_paid);
                claim.partial_payout_bps = Some(effective_bps);
                PolicyEngineClient::new(env, &policy_engine)
                    .pay_claim(&env.current_contract_address(), &claim.policy_id);
                // Atomic lock release
                RiskPoolClient::new(env, &risk_pool)
                    .release_for_claim(&env.current_contract_address(), &claim.policy_id);
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
}

/// Evaluate a policy's trigger comparison directly against an observed
/// value, without going through the oracle-verifier contract. Used by the
/// cross-chain attestation path, which has no oracle data of its own to
/// aggregate — only whatever single value the attestor reported.
fn evaluate_comparison(
    observed: i128,
    threshold: i128,
    comparison: &parashield_policy_engine::TriggerComparison,
) -> bool {
    match comparison {
        parashield_policy_engine::TriggerComparison::LessThan => observed < threshold,
        parashield_policy_engine::TriggerComparison::GreaterThan => observed > threshold,
        parashield_policy_engine::TriggerComparison::Equal => observed == threshold,
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
