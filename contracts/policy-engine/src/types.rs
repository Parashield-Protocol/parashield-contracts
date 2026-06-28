use soroban_sdk::{contracttype, Address, Symbol};

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
