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
    /// Installment payout configuration for large claims.
    pub installments: Option<InstallmentSchedule>,
    /// Whether the claimant's identity was verified (optional, for Sybil protection).
    pub identity_verified: bool,
    /// Type of identity verification performed (e.g., "kyc", "accreditation").
    pub verification_type: Option<Symbol>,
    /// Timestamp when identity verification occurred.
    pub verification_time: Option<u64>,
    /// Timestamp at which payout becomes available (issue #432).
    /// `None` means payout is immediate or not applicable.
    pub payout_ready_at: Option<u64>,
}

/// Configuration for installment-based claim payouts.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallmentSchedule {
    /// Total amount to be paid out in installments.
    pub total_amount: i128,
    /// Amount per installment.
    pub amount_per_installment: i128,
    /// Total number of installments.
    pub num_installments: u32,
    /// Interval in seconds between installments.
    pub interval_seconds: u64,
    /// Timestamp when first installment becomes claimable.
    pub first_installment_at: u64,
    /// Number of installments already paid out.
    pub paid_count: u32,
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
pub struct BatchClaimsSubmitted {
    pub claimant: Address,
    pub count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchClaimsProcessed {
    pub keeper: Address,
    pub count: u32,
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
pub struct ClaimResolved {
    pub claim_id: u128,
    pub resolver: Address,
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
pub struct ClaimDeadlineUpdated {
    pub deadline: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractUpgraded {
    pub old_version: u32,
    pub new_version: u32,
}
