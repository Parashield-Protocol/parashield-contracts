use soroban_sdk::{contracttype, Address, Symbol};

/// How the trigger threshold is compared against the observed value.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TriggerComparison {
    /// Trigger fires when observed < threshold (drought: rainfall < 50mm)
    LessThan,
    /// Trigger fires when observed > threshold (storm: wind_speed > 120 km/h)
    GreaterThan,
    /// Trigger fires when observed == threshold (binary events)
    Equal,
}

/// A trigger condition evaluated by the Claims Processor.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TriggerCondition {
    /// Oracle data category: "weather", "flight", "disaster", "onchain"
    pub data_type: Symbol,
    /// Specific key: "rainfall:kisumu:2026-06", "flight:KQ100:2026-06-15"
    pub key: Symbol,
    /// Threshold in 7-decimal fixed point
    pub threshold: i128,
    pub comparison: TriggerComparison,
}

/// A single data submission from one oracle.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleDataPoint {
    pub oracle: Address,
    /// Observed value in 7-decimal fixed point (e.g., 32_000_000 = 32.0000000 mm)
    pub value: i128,
    /// Reliability score 0-100
    pub confidence: u32,
    /// Unix timestamp of the real-world observation
    pub timestamp: u64,
}

/// Aggregated statistics across all oracles for a key.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AggregatedData {
    pub median_value: i128,
    pub oracle_count: u32,
    pub min_confidence: u32,
    pub last_updated: u64,
}

/// Registered oracle record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleEntry {
    pub oracle: Address,
    pub data_type: Symbol,
    /// Relative weight for future weighted-median (1-100)
    pub weight: u32,
    pub active: bool,
}
