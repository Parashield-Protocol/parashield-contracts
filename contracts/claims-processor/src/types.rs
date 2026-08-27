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

