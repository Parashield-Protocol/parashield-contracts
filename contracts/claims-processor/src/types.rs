use soroban_sdk::{contracttype, Address, Symbol};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaimStatus {
    Pending,
    Paid,
    Rejected,
    Disputed,
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

