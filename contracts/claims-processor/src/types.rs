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
    /// Installment payout configuration for large claims. `Vec` instead of
    /// `Option<InstallmentSchedule>` because the soroban-sdk XDR (`ScVal`)
    /// conversion generated for a contracttype struct does not support
    /// `Option<CustomStruct>` fields — only `Vec<T>` round-trips a custom
    /// struct through both the WASM `Val` path and the host-side `ScVal`
    /// path used by test tooling. Empty = no installment schedule.
    pub installments: Vec<InstallmentSchedule>,
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

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallmentPayoutScheduled {
    pub claim_id: u128,
    pub policy_id: u128,
    pub claimant: Address,
    pub total_amount: i128,
    pub num_installments: u32,
    pub interval_seconds: u64,
    pub first_installment_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallmentPaid {
    pub claim_id: u128,
    pub claimant: Address,
    pub amount: i128,
    pub paid_count: u32,
    pub total_installments: u32,
}

/// Emitted when the payout delay configuration is updated.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayoutDelayUpdated {
    pub delay_seconds: u64,
}

/// Supported payout currencies for claim settlements.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PayoutCurrency {
    /// Default USDC (7-decimal)
    USDC,
    /// Alternative stablecoin (address stored separately)
    Custom(Address),
}

/// Multi-currency payout configuration for a claim.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayoutOption {
    /// Currency to pay out in.
    pub currency: PayoutCurrency,
    /// Exchange rate (basis points) relative to USDC. 10000 = 1:1 parity.
    /// Used to convert USDC coverage amounts to equivalent other-currency amounts.
    pub exchange_rate_bps: u32,
    /// Whether this payout option is currently enabled.
    pub enabled: bool,
}

/// Record of available payout currencies and their exchange rates.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayoutCurrencyRegistry {
    /// Address of the USDC token contract (default payout currency).
    pub usdc_token: Address,
    /// Optional alternative payout currencies with their exchange rates.
    pub alt_currencies: Vec<Address>,
    /// Exchange rates for alt currencies (index matches alt_currencies).
    pub exchange_rates_bps: Vec<u32>,
}

/// Emitted when a new payout currency is registered.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayoutCurrencyAdded {
    pub token: Address,
    pub exchange_rate_bps: u32,
}

/// Emitted when a payout currency's exchange rate is updated.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayoutCurrencyRateUpdated {
    pub token: Address,
    pub old_rate_bps: u32,
    pub new_rate_bps: u32,
}

/// Emitted when a claim is paid out in an alternate currency.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimPaidInAlternativeCurrency {
    pub claim_id: u128,
    pub claimant: Address,
    pub usdc_equivalent: i128,
    pub token: Address,
    pub actual_amount: i128,
    pub exchange_rate_bps: u32,
}

// ─── Fraud detection (issue #437) ────────────────────────────────────────────

/// What the contract does when a claim's fraud score meets or exceeds
/// `FraudConfig.fraud_threshold_score`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FraudMode {
    /// Reject the submission with `Error::FraudSuspected`. Default and safest:
    /// no state is written for the flagged claim.
    Block,
    /// Persist a `FraudRecord` and emit `FraudFlagged`, then let the claim
    /// enter the normal pending queue. Useful when the admin wants human
    /// review to gate the claim later without blocking legitimate claimants.
    FlagOnly,
}

/// Configuration for the additive rule-based fraud detector applied inside
/// `submit_claim` and `batch_submit_claims`. Absent from storage means the
/// detector is fully disabled and every claim proceeds unchecked, matching
/// pre-issue-437 behaviour.
///
/// Rule scores default to values whose sum (90) is above the default
/// `fraud_threshold_score` (50), so any single rule alone will not flag a
/// claim but any two rules together will. Admins can rebalance freely.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FraudConfig {
    /// Two claim submissions from the same claimant less than
    /// `rate_window_secs` apart add `rate_score` to the second claim.
    /// `0` disables the rule.
    pub rate_window_secs: u64,
    pub rate_score: u32,
    /// The claimant's `burst_count`-th claim within `burst_window_secs` adds
    /// `burst_score`. `0` in either window field disables the rule.
    pub burst_window_secs: u64,
    pub burst_count: u32,
    pub burst_score: u32,
    /// A claim whose `coverage_amount` exceeds
    /// `coverage_anomaly_multiplier` * (claimant's rolling max prior
    /// coverage_amount) adds `coverage_score`. `0` disables the rule.
    pub coverage_anomaly_multiplier: u32,
    pub coverage_score: u32,
    /// Threshold at (or above) which the claim is treated as fraudulent
    /// per `mode`. Capped at 100 by `set_fraud_config`.
    pub fraud_threshold_score: u32,
    pub mode: FraudMode,
}

/// Compact aggregate of a claimant's recent submission behaviour, kept as
/// one storage entry per claimant instead of iterating every past claim on
/// each new submission.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimantHistory {
    /// Timestamp of the claimant's most recent successful submission.
    /// `0` when the claimant has never submitted.
    pub last_submission_at: u64,
    /// Timestamp anchoring the current `burst_window` bucket. When
    /// `now - burst_bucket_start > burst_window_secs` the bucket resets.
    pub burst_bucket_start: u64,
    /// Claims submitted inside the current burst bucket.
    pub burst_count: u32,
    /// Highest `coverage_amount` this claimant has ever submitted (across
    /// all statuses; a rejected claim still shows the coverage they asked
    /// for). Used by the coverage-anomaly rule.
    pub max_coverage_ever: i128,
    /// Total claims this claimant has ever submitted. Kept for observability
    /// so admins can distinguish "new" from "veteran" claimants in dashboards.
    pub total_submissions: u64,
}

/// A frozen snapshot of the fraud detector's judgement for one claim.
/// Written only when the detector is configured and the score is nonzero
/// so admins can post-hoc review flagged submissions.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FraudRecord {
    pub claim_id: u128,
    pub score: u32,
    /// Bitfield of rule contributions:
    ///   bit 0 = rate rule
    ///   bit 1 = burst rule
    ///   bit 2 = coverage anomaly rule
    pub flags: u32,
    pub checked_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FraudConfigUpdated {
    pub rate_window_secs: u64,
    pub rate_score: u32,
    pub burst_window_secs: u64,
    pub burst_count: u32,
    pub burst_score: u32,
    pub coverage_anomaly_multiplier: u32,
    pub coverage_score: u32,
    pub fraud_threshold_score: u32,
    pub mode: FraudMode,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FraudFlagged {
    pub claim_id: u128,
    pub claimant: Address,
    pub score: u32,
    pub flags: u32,
    pub mode: FraudMode,
}
