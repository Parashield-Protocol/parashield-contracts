use soroban_sdk::{contracttype, Address, BytesN, Symbol, Vec};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TriggerType {
    /// Measured value crosses a threshold (rainfall, temperature, wind speed)
    Threshold,
    /// Binary event occurred / did not (flight delayed yes/no)
    Binary,
    /// Payout proportional to parameter deviation
    Parametric,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TriggerComparison {
    LessThan,
    GreaterThan,
    Equal,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductStatus {
    Active,
    Paused,
    Deprecated,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyStatus {
    Active,
    Claimed,
    Expired,
    Cancelled,
}

/// An insurance product template.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InsuranceProduct {
    pub id: u128,
    /// e.g. "Crop Insurance – Kisumu Rainfall"
    pub name: Symbol,
    /// "crop" | "flight" | "disaster" | "health" | "defi"
    pub category: Symbol,
    /// Specific oracle measurement key, e.g. symbol_short!("kis2606")
    pub oracle_key: Symbol,
    pub trigger_type: TriggerType,
    /// Oracle data category: "weather" | "flight" | "onchain"
    pub oracle_data_type: Symbol,
    /// 7-decimal fixed point (50_000_000 = 50.0000000 mm)
    pub trigger_threshold: i128,
    pub trigger_comparison: TriggerComparison,
    /// Minimum coverage in USDC stroops (7-decimal)
    pub coverage_min: i128,
    pub coverage_max: i128,
    /// Premium rate in basis points — 500 = 5.00%
    pub premium_rate_bps: u32,
    pub max_duration_days: u32,
    pub status: ProductStatus,
    pub created_at: u64,
}

/// One line item for `batch_buy_policy` — the same four arguments `buy_policy`
/// takes, bundled so several policies can be purchased in a single call.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchBuyItem {
    pub product_id: u128,
    pub coverage_amount: i128,
    pub duration_days: u32,
    pub oracle_key: Symbol,
}

/// Input struct for creating a new insurance product (avoids >10 param limit).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateProductParams {
    pub name: Symbol,
    pub category: Symbol,
    pub oracle_key: Symbol,
    pub trigger_type: TriggerType,
    pub oracle_data_type: Symbol,
    pub trigger_threshold: i128,
    pub trigger_comparison: TriggerComparison,
    pub coverage_min: i128,
    pub coverage_max: i128,
    pub premium_rate_bps: u32,
    pub max_duration_days: u32,
}

/// An individual insurance policy owned by a policyholder.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Policy {
    pub id: u128,
    pub product_id: u128,
    pub policyholder: Address,
    pub coverage_amount: i128,
    pub premium_paid: i128,
    /// Specific oracle measurement key, e.g. symbol_short!("kis2606")
    pub oracle_key: Symbol,
    pub oracle_data_type: Symbol,
    pub trigger_threshold: i128,
    pub trigger_comparison: TriggerComparison,
    pub start_time: u64,
    pub end_time: u64,
    pub status: PolicyStatus,
    pub created_at: u64,
}

/// Summary stats for a product — returned by get_product_stats.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductStats {
    pub product_id: u128,
    pub total_policies: u32,
    pub active_policies: u32,
    pub total_coverage: i128,
    pub total_premium_collected: i128,
}

// ─── Events ──────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Initialized {
    pub admin: Address,
    pub usdc_token: Address,
    pub oracle_address: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimsProcessorUpdated {
    pub claims_processor: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RiskPoolUpdated {
    pub risk_pool: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductCreated {
    pub product_id: u128,
    pub name: Symbol,
    pub category: Symbol,
    pub premium_rate_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductPaused {
    pub product_id: u128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductDeprecated {
    pub product_id: u128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyCreated {
    pub policy_id: u128,
    pub product_id: u128,
    pub policyholder: Address,
    pub coverage_amount: i128,
    pub premium_paid: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyCancelled {
    pub policy_id: u128,
    pub policyholder: Address,
    pub refund_amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyTransferred {
    pub policy_id: u128,
    pub from: Address,
    pub to: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyClaimed {
    pub policy_id: u128,
    pub policyholder: Address,
    pub coverage_amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyExpired {
    pub policy_id: u128,
}

/// Emitted when a still-Active policy enters its expiry warning window.
///
/// Coverage lapsing is not a state change the chain announces on its own —
/// `end_time` simply passes. Without this, the only on-chain signal is
/// `PolicyExpired`, which fires *after* cover has already gone. An indexer
/// watching for this topic can notify the holder while renewing still helps.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyExpiringSoon {
    pub policy_id: u128,
    pub policyholder: Address,
    pub product_id: u128,
    pub coverage_amount: i128,
    pub end_time: u64,
    /// Seconds remaining until `end_time` at the moment of emission.
    pub seconds_remaining: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpiryWarningWindowUpdated {
    pub window: u64,
}

/// Where a policy sits relative to its own expiry.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExpiryState {
    /// Not Active — claimed, cancelled, or already marked expired.
    NotActive,
    /// Active, and outside the warning window.
    Active,
    /// Active, inside the warning window, still covered.
    ExpiringSoon,
    /// `end_time` has passed but the policy has not been marked Expired yet.
    Lapsed,
}

/// Expiry status for one policy, returned without panicking.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyExpiryInfo {
    pub policy_id: u128,
    pub state: ExpiryState,
    pub end_time: u64,
    /// Seconds until `end_time`, or 0 once it has passed.
    pub seconds_remaining: u64,
    /// True when a warning event has already been emitted for this policy, so
    /// a keeper can skip it instead of paying to re-emit.
    pub warned: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminUpdated {
    pub new_admin: Address,
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

