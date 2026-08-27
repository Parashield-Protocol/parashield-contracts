use soroban_sdk::{contracttype, Address, Bytes, BytesN, Symbol, Vec};

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

/// How multiple oracle submissions are combined into a single consensus value.
///
/// The right choice depends on the threat model for a data type. Median
/// resists a minority of outliers completely — one oracle reporting an absurd
/// value cannot move it — which is what you want for adversarial or
/// error-prone feeds. A weighted average uses every submission in proportion
/// to its registered weight, which tracks small genuine variations more
/// smoothly but lets a single extreme value drag the result. A time-weighted
/// average additionally accounts for how long each submission held before
/// being superseded, so a feed with irregular submission cadence isn't
/// skewed by a burst of readings clustered in a short window.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AggregationMethod {
    /// Weight-aware median: the value at the middle of cumulative weight.
    /// A minority of outliers cannot move it. This is the default.
    WeightedMedian,
    /// Weighted arithmetic mean: `sum(value * weight) / sum(weight)`.
    /// Smooth, but one extreme submission moves the result.
    WeightedAverage,
    /// Unweighted arithmetic mean: every valid submission counts equally.
    /// Use when registered weights are not meaningful for this data type.
    Mean,
    /// Oracle-weight- and time-weighted average: each eligible submission
    /// is weighted by its registered oracle weight *and* by how many
    /// seconds it was the most recent reading for its oracle — from its
    /// own timestamp until the next later submission's timestamp (or
    /// "now" for the newest one). Only point-in-time values are ever
    /// submitted; this reconstructs a time-weighted average (TWAP-style)
    /// over the observation window from those snapshots, rather than
    /// treating every snapshot as equally significant regardless of how
    /// long it held.
    TimeWeightedAverage,
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
pub struct DataTypeMaxAgeUpdated {
    pub data_type: Symbol,
    /// `None` is encoded as 0, meaning the override was cleared.
    pub max_age: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AggregationMethodUpdated {
    pub data_type: Symbol,
    pub method: AggregationMethod,
}

/// Freshness report for a `(data_type, key)` pair.
///
/// Answers "can this data be used right now?" without panicking, so a caller
/// can check before committing to a claim evaluation that would otherwise
/// abort the whole transaction.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FreshnessReport {
    pub data_type: Symbol,
    pub key: Symbol,
    /// True when at least `min_oracle_count` submissions are within the
    /// effective max age for this data type.
    pub is_fresh: bool,
    /// Age in seconds of the newest submission. `u64::MAX` when there are no
    /// submissions at all.
    pub newest_age: u64,
    /// Number of submissions still inside the freshness window.
    pub fresh_count: u32,
    /// Number of submissions held for this key, fresh or not.
    pub total_count: u32,
    /// The max age applied — the per-data-type override when set, else global.
    pub max_age: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinOracleCountUpdated {
    pub min_count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataTypeMinOracleCountUpdated {
    pub data_type: Symbol,
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

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StakeDeposited {
    pub oracle: Address,
    pub data_type: Symbol,
    pub amount: i128,
    pub total_stake: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StakeWithdrawn {
    pub oracle: Address,
    pub data_type: Symbol,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleSlashed {
    pub oracle: Address,
    pub data_type: Symbol,
    pub amount: i128,
    pub remaining_stake: i128,
    pub reason: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinStakeUpdated {
    pub min_stake: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StakeTokenUpdated {
    pub token: Address,
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
    pub approvals: Vec<Address>,
}

/// A pending admin-transfer proposal awaiting guardian approvals.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingAdminChange {
    pub new_admin: Address,
    pub approvals: Vec<Address>,
}

/// An encrypted data submission from one oracle.
///
/// `ciphertext`/`nonce` are opaque to the contract — decryption happens
/// entirely off-chain by whichever parties hold the key for this data_type.
/// Kept in a separate storage path from `OracleDataPoint` so a consumer can
/// never accidentally treat ciphertext as a usable plaintext value.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncryptedOracleDataPoint {
    pub oracle: Address,
    pub ciphertext: Bytes,
    /// Nonce/IV used for this submission's encryption. 12 bytes fits the
    /// common AEAD schemes (e.g. AES-GCM, ChaCha20-Poly1305) off-chain
    /// consumers are expected to use.
    pub nonce: BytesN<12>,
    /// Reliability score 0-100, same meaning as on a plaintext submission.
    pub confidence: u32,
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncryptionRequiredUpdated {
    pub data_type: Symbol,
    pub required: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaxTimestampAgeUpdated {
    pub max_timestamp_age: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleEncryptedDataSubmitted {
    pub oracle: Address,
    pub data_type: Symbol,
    pub key: Symbol,
    pub confidence: u32,
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimestampFutureBufferUpdated {
    pub seconds: u64,
}