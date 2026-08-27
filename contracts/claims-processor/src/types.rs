use soroban_sdk::{contracttype, Address, BytesN, Symbol, Vec};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaimStatus {
    Pending,
    Paid,
    Rejected,
    Disputed,
    /// Claim was resolved as expired before it could be processed.
    Expired,
    /// Claim was partially paid (proportional payout based on trigger severity).
    PartiallyPaid,
    /// Claim sat Pending past the escalation threshold and was escalated for
    /// manual review. Still unresolved — this records that it is overdue, not
    /// that it was decided.
    Escalated,
}

/// Result returned by `process_claim` and `auto_process`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaimResult {
    /// Trigger was met — coverage paid to policyholder.
    Paid,
    /// Trigger not met — no payout.
    Rejected,
    /// Policy expired before trigger was confirmed.
    Expired,
    /// Policy was already claimed (idempotent response).
    AlreadyClaimed,
    /// Policy is not in Active state (cancelled etc.).
    PolicyNotActive,
    AlreadyProcessed,
    /// Trigger was met but payout was proportional (partial payment).
    PartiallyPaid,
}

/// A claim record stored on-chain.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Claim {
    pub id: u128,
    pub policy_id: u128,
    pub claimant: Address,
    pub coverage_amount: i128,
    /// Oracle value read at processing time (None if not yet evaluated)
    pub observed_value: Option<i128>,
    pub trigger_met: bool,
    pub status: ClaimStatus,
    pub submitted_at: u64,
    pub processed_at: Option<u64>,
    pub dispute_reason: Option<Symbol>,
    /// For PartiallyPaid claims: the actual USDC amount paid out.
    pub paid_amount: Option<i128>,
    /// For PartiallyPaid claims: payout ratio in basis points (0-10000).
    /// 10000 = full coverage; lower = proportional partial payment.
    pub partial_payout_bps: Option<u32>,
}

/// How overdue a pending claim is.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimAgeInfo {
    pub claim_id: u128,
    pub status: ClaimStatus,
    pub submitted_at: u64,
    /// Seconds the claim has been waiting. 0 once it is resolved.
    pub pending_for: u64,
    /// Threshold in force for escalation.
    pub escalation_threshold: u64,
    /// True when the claim is Pending and past the threshold.
    pub escalatable: bool,
    /// Seconds until it becomes escalatable, or 0 if it already is.
    pub seconds_until_escalatable: u64,
}

// ─── Events ──────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Initialized {
    pub admin: Address,
    pub policy_engine: Address,
    pub risk_pool: Address,
    pub oracle_verifier: Address,
    pub staleness_threshold: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimSubmitted {
    pub claim_id: u128,
    pub policy_id: u128,
    pub claimant: Address,
    pub coverage_amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimProcessed {
    pub claim_id: u128,
    pub policy_id: u128,
    pub trigger_met: bool,
    pub status: ClaimStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimDisputed {
    pub claim_id: u128,
    pub claimant: Address,
    pub reason: Symbol,
}

/// Emitted when an overdue claim is escalated for manual review.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimEscalated {
    pub claim_id: u128,
    pub policy_id: u128,
    pub claimant: Address,
    /// Seconds the claim had been Pending when it was escalated.
    pub pending_for: u64,
    /// Who triggered the escalation.
    pub escalated_by: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscalationThresholdUpdated {
    pub threshold: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractUpgraded {
    pub old_version: u32,
    pub new_version: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardiansUpdated {
    pub guardians: Vec<Address>,
    pub threshold: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeApproved {
    pub new_wasm_hash: BytesN<32>,
    pub approver: Address,
    pub approvals: u32,
    pub threshold: u32,
}

/// A pending contract-upgrade action awaiting guardian approvals.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingUpgrade {
    pub new_wasm_hash: BytesN<32>,
    pub new_version: u32,
    pub approvals: Vec<Address>,
}

/// A pending admin-transfer proposal awaiting guardian approvals.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingAdminChange {
    pub new_admin: Address,
    pub approvals: Vec<Address>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminUpdated {
    pub new_admin: Address,
}

use soroban_sdk::{contracttype, Address, BytesN, Symbol, Vec};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaimStatus {
    Pending,
    Paid,
    Rejected,
    Disputed,
    /// Claim was resolved as expired before it could be processed.
    Expired,
    /// Claim was partially paid (proportional payout based on trigger severity).
    PartiallyPaid,
}

/// Result returned by `process_claim` and `auto_process`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaimResult {
    /// Trigger was met — coverage paid to policyholder.
    Paid,
    /// Trigger not met — no payout.
    Rejected,
    /// Policy expired before trigger was confirmed.
    Expired,
    /// Policy was already claimed (idempotent response).
    AlreadyClaimed,
    /// Policy is not in Active state (cancelled etc.).
    PolicyNotActive,
    AlreadyProcessed,
    /// Trigger was met but payout was proportional (partial payment).
    PartiallyPaid,
}

/// A claim record stored on-chain.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Claim {
    pub id: u128,
    pub policy_id: u128,
    pub claimant: Address,
    pub coverage_amount: i128,
    /// Oracle value read at processing time (None if not yet evaluated)
    pub observed_value: Option<i128>,
    pub trigger_met: bool,
    pub status: ClaimStatus,
    pub submitted_at: u64,
    pub processed_at: Option<u64>,
    pub dispute_reason: Option<Symbol>,
    /// For PartiallyPaid claims: the actual USDC amount paid out.
    pub paid_amount: Option<i128>,
    /// For PartiallyPaid claims: payout ratio in basis points (0-10000).
    /// 10000 = full coverage; lower = proportional partial payment.
    pub partial_payout_bps: Option<u32>,
}

// ─── Events ──────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Initialized {
    pub admin: Address,
    pub policy_engine: Address,
    pub risk_pool: Address,
    pub oracle_verifier: Address,
    pub staleness_threshold: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimSubmitted {
    pub claim_id: u128,
    pub policy_id: u128,
    pub claimant: Address,
    pub coverage_amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimProcessed {
    pub claim_id: u128,
    pub policy_id: u128,
    pub trigger_met: bool,
    pub status: ClaimStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimDisputed {
    pub claim_id: u128,
    pub claimant: Address,
    pub reason: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractUpgraded {
    pub old_version: u32,
    pub new_version: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardiansUpdated {
    pub guardians: Vec<Address>,
    pub threshold: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeApproved {
    pub new_wasm_hash: BytesN<32>,
    pub approver: Address,
    pub approvals: u32,
    pub threshold: u32,
}

/// A pending contract-upgrade action awaiting guardian approvals.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingUpgrade {
    pub new_wasm_hash: BytesN<32>,
    pub new_version: u32,
    pub approvals: Vec<Address>,
}

/// A pending admin-transfer proposal awaiting guardian approvals.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingAdminChange {
    pub new_admin: Address,
    pub approvals: Vec<Address>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminUpdated {
    pub new_admin: Address,
}

/// A cross-chain trigger observation submitted by a registered attestor for
/// one policy, used by `process_cross_chain_claim` in place of the Stellar
/// oracle-verifier.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrossChainAttestation {
    /// Which chain this observation came from.
    pub chain_id: Symbol,
    /// The registered attestor that submitted it.
    pub attestor: Address,
    /// The observed value, in the same fixed-point units as
    /// `Policy.trigger_threshold`.
    pub observed_value: i128,
    /// Hash of the off-chain proof (light-client proof, relayer message,
    /// oracle report) backing `observed_value`. Opaque to the contract —
    /// kept for audit/dispute purposes, not verified on-chain.
    pub proof_hash: BytesN<32>,
    /// Unix timestamp of the observation.
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrossChainAttestorAdded {
    pub chain_id: Symbol,
    pub attestor: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrossChainAttestorRemoved {
    pub chain_id: Symbol,
    pub attestor: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrossChainAttestationSubmitted {
    pub policy_id: u128,
    pub chain_id: Symbol,
    pub attestor: Address,
    pub observed_value: i128,
    pub timestamp: u64,
}

