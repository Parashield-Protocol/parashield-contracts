use soroban_sdk::{contracttype, Address, Symbol};

/// Reputation score for an oracle, tracking accuracy over time.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleReputation {
    pub oracle: Address,
    pub data_type: Symbol,
    /// Total number of submissions made by this oracle
    pub total_submissions: u64,
    /// Number of submissions that were accurate (within tolerance of median)
    pub accurate_submissions: u64,
    /// Reputation score 0-1000 (basis points of accuracy)
    pub score: u32,
    /// Timestamp of last reputation update
    pub last_updated: u64,
}

// ─── Events ──────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationUpdated {
    pub oracle: Address,
    pub data_type: Symbol,
    pub score: u32,
    pub accurate: u64,
    pub total: u64,
}

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
    /// Matches if |median - threshold| <= tolerance
    EqualWithTolerance,
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
    /// The maximum acceptable absolute variance.
    /// Set to 0 if utilizing standard LessThan/GreaterThan/Equal.
    pub tolerance: i128,
}

/// Input struct for a single reading inside a bulk `submit_data_batch` call.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleDataSubmission {
    /// Specific measurement key — e.g., `symbol_short!("kis2606")`
    pub key: Symbol,
    /// Observed value in 7-decimal fixed point
    pub value: i128,
    /// Reliability score 0-100
    pub confidence: u32,
    /// Unix timestamp of the real-world observation
    pub timestamp: u64,
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
    /// Number of data submissions currently stored for this (data_type, key) —
    /// NOT the number of currently-registered active oracles for the
    /// data_type (see `active_oracle_count`). Kept for backward
    /// compatibility with existing callers of this field.
    pub oracle_count: u32,
    /// Number of oracles currently registered and active for this
    /// data_type, independent of whether they have submitted data for
    /// this specific key. Use this (not `oracle_count`) as a proxy for
    /// oracle diversity/registration health — `oracle_count` conflates
    /// submission count with registration count (issue #136).
    pub active_oracle_count: u32,
    /// Aggregated confidence is the weighted average of valid oracle confidences,
    /// weighted by each oracle's configured registration weight and rounded down.
    pub confidence: u32,
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

/// Summary returned by get_oracle_health for a specific oracle.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleHealth {
    pub oracle: Address,
    pub data_type: Symbol,
    pub weight: u32,
    pub active: bool,
    /// Timestamp of their most recent submission, or 0 if never.
    pub last_submitted: u64,
}

// ─── Events ──────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Initialized {
    pub admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleAdded {
    pub oracle: Address,
    pub data_type: Symbol,
    pub weight: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleRemoved {
    pub oracle: Address,
    pub data_type: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinConfidenceUpdated {
    pub threshold: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaxDataAgeUpdated {
    pub max_age: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinOracleCountUpdated {
    pub min_count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinSubmitIntervalUpdated {
    pub seconds: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleDataSubmitted {
    pub oracle: Address,
    pub data_type: Symbol,
    pub key: Symbol,
    pub value: i128,
    pub confidence: u32,
    pub timestamp: u64,
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