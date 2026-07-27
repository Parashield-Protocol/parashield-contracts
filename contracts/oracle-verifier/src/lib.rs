//! Parashield Oracle Verifier
//!
//! Authorized oracle addresses submit real-world data (rainfall, flight status,
//! on-chain metrics). The contract stores submissions and exposes a
//! `verify_trigger` function used by the Claims Processor to determine whether
//! a policy's payout condition has been met.
//!
//! Design notes
//! ─────────────
//! - Multiple oracles can submit the same key; the contract computes a
//!   weight-based median for aggregation (oracle weight, not submission confidence).
//! - Only the admin can register/remove oracle addresses.
//! - Any oracle already registered for a (data_type) may submit data.
//! - Duplicate submissions from the same oracle overwrite the previous value.
#![no_std]
extern crate alloc;
use alloc::string::ToString;

#[cfg_attr(feature = "library", allow(unused_imports))]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, Address, BytesN, Env,
    Symbol, Vec,
};

pub mod types;
pub use types::*;

/// Maximum number of registered oracles. Bounds the median aggregation loop and
/// the worst-case weighted sum (MAX_ORACLES * max_weight * max_value) so it
/// cannot overflow i128.
#[allow(dead_code)]
const MAX_ORACLES: u32 = 100;

// ─── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
enum StorageKey {
    Initialized,
    Admin,
    /// OracleEntry for (data_type, oracle_address)
    Oracle(Symbol, Address),
    /// Vec<OracleDataPoint> — all submissions for (data_type, key)
    DataPoints(Symbol, Symbol),
    /// Vec<Address> — every registered oracle address
    OracleList,
    /// Global minimum confidence threshold (u32)
    MinConfidence,
    MaxDataAge,
    PendingAdmin,
    MinOracleCount,
}

// ─── Errors ───────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    OracleNotRegistered = 4,
    OracleAlreadyExists = 5,
    NoDataAvailable = 6,
    InvalidConfidence = 7,
    InvalidWeight = 8,
    StaleData = 9,
    TooManyOracles = 10,
    InvalidTimestamp = 11,
}

// ─── Contract ─────────────────────────────────────────────────────────────────

#[cfg(any(test, feature = "testutils", not(feature = "library")))]
#[contract]
pub struct OracleVerifier;

#[cfg(any(test, feature = "testutils", not(feature = "library")))]
#[contractimpl]
impl OracleVerifier {
    // ── Lifecycle ────────────────────────────────────────────────────────────

    /// One-time initialisation. Caller becomes admin.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&StorageKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }

        // require_auth() validates the address at the protocol level, so we do
        // not need manual address format string validation here.
        admin.require_auth();

        env.storage()
            .instance()
            .set(&StorageKey::Initialized, &true);
        env.storage().instance().set(&StorageKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&StorageKey::OracleList, &Vec::<Address>::new(&env));

        // No pending admin initially
        let pending_admin = env
            .storage()
            .instance()
            .get::<_, Option<Address>>(&StorageKey::PendingAdmin) // Explicitly type the lookup
            .unwrap_or(None);

        env.events().publish(
            (Symbol::new(&env, "initialized"),),
            Initialized {
                admin: admin.clone(),
            },
        );
    }

    // ── Oracle Management (admin only) ───────────────────────────────────────

    /// Register an oracle address for a given data type with a relative weight.
    /// `weight` is 1-100; higher-weight oracles contribute more to the median.
    pub fn add_oracle(env: Env, admin: Address, oracle: Address, data_type: Symbol, weight: u32) {
        Self::require_admin(&env, &admin);
        if weight == 0 || weight > 100 {
            panic_with_error!(&env, Error::InvalidWeight);
        }
        let key = StorageKey::Oracle(data_type.clone(), oracle.clone());
        if env.storage().persistent().has(&key) {
            panic_with_error!(&env, Error::OracleAlreadyExists);
        }
        let entry = OracleEntry {
            oracle: oracle.clone(),
            data_type: data_type.clone(),
            weight,
            active: true,
        };
        env.storage().persistent().set(&key, &entry);

        let mut list: Vec<Address> = env
            .storage()
            .instance()
            .get(&StorageKey::OracleList)
            .unwrap_or_else(|| Vec::new(&env));
        if list.len() >= MAX_ORACLES {
            panic_with_error!(&env, Error::TooManyOracles);
        }
        list.push_back(oracle.clone());
        env.storage().instance().set(&StorageKey::OracleList, &list);
        let mut already_present = false;
        for i in 0..list.len() {
            if list.get_unchecked(i) == oracle {
                already_present = true;
                break;
            }
        }
        if !already_present {
            list.push_back(oracle.clone());
            env.storage().instance().set(&StorageKey::OracleList, &list);
        }

        env.events().publish(
            (Symbol::new(&env, "oracle_added"),),
            OracleAdded {
                oracle,
                data_type,
                weight,
            },
        );
    }

    /// Update the relative weight of an existing oracle registration.
    /// `weight` is 1-100; use `add_oracle` only for new registrations.
    pub fn update_oracle_weight(
        env: Env,
        admin: Address,
        oracle: Address,
        data_type: Symbol,
        weight: u32,
    ) {
        Self::require_admin(&env, &admin);
        if weight == 0 || weight > 100 {
            panic_with_error!(&env, Error::InvalidWeight);
        }

        let key = StorageKey::Oracle(data_type, oracle);
        let mut entry: OracleEntry = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::OracleNotRegistered));
        entry.weight = weight;
        env.storage().persistent().set(&key, &entry);
    }

    /// Deactivate an oracle (soft delete — historical data is retained).
    pub fn remove_oracle(env: Env, admin: Address, oracle: Address, data_type: Symbol) {
        Self::require_admin(&env, &admin);
        let key = StorageKey::Oracle(data_type.clone(), oracle.clone());
        let mut entry: OracleEntry = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::OracleNotRegistered));
        entry.active = false;
        env.storage().persistent().set(&key, &entry);

        env.events().publish(
            (Symbol::new(&env, "oracle_removed"),),
            OracleRemoved { oracle, data_type },
        );
    }

    // ── Contract Settings (admin only) ───────────────────────────────────────

    /// Set the global minimum confidence threshold for oracle data.
    pub fn set_min_confidence(env: Env, admin: Address, threshold: u32) {
        Self::require_admin(&env, &admin);
        if threshold > 100 {
            panic_with_error!(&env, Error::InvalidConfidence);
        }
        env.storage()
            .instance()
            .set(&StorageKey::MinConfidence, &threshold);
        env.events().publish(
            (Symbol::new(&env, "min_confidence_updated"),),
            MinConfidenceUpdated { threshold },
        );
    }

    /// Set the maximum data age in seconds.
    pub fn set_max_data_age(env: Env, admin: Address, max_age: u64) {
        Self::require_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&StorageKey::MaxDataAge, &max_age);
        env.events().publish(
            (Symbol::new(&env, "max_data_age_updated"),),
            MaxDataAgeUpdated { max_age },
        );
    }

    /// Propose a new admin. Only the current admin can call this.
    pub fn propose_new_admin(env: Env, admin: Address, new_admin: Address) {
        Self::require_admin(&env, &admin);
        // Store the proposed admin (zero address means no proposal)
        env.storage()
            .instance()
            .set(&StorageKey::PendingAdmin, &new_admin);
    }

    /// Accept the proposed admin. Only the proposed admin can call this.
    pub fn accept_admin(env: Env, admin: Address) {
        // Correct way to read an Optional field from Soroban instance storage
        let pending_admin_opt: Option<Address> = env
            .storage()
            .instance()
            .get(&StorageKey::PendingAdmin)
            .unwrap_or(None); // If key doesn't exist, evaluates to None

        let pending_admin = match pending_admin_opt {
            Some(addr) => addr,
            None => panic_with_error!(&env, Error::Unauthorized),
        };

        if admin != pending_admin {
            panic_with_error!(&env, Error::Unauthorized);
        }

        admin.require_auth();

        if !env.storage().instance().has(&StorageKey::Initialized) {
            panic_with_error!(&env, Error::NotInitialized);
        }

        env.storage().instance().set(&StorageKey::Admin, &admin);

        // Clear the slot explicitly using a clean None type hint
        let no_pending: Option<Address> = None;
        env.storage()
            .instance()
            .set(&StorageKey::PendingAdmin, &no_pending);

        env.events().publish(
            (Symbol::new(&env, "admin_updated"),),
            AdminUpdated { new_admin: admin },
        );
    }

    /// Get the maximum data age in seconds (defaults to 7 days = 604,800 seconds).
    pub fn get_max_data_age(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&StorageKey::MaxDataAge)
            .unwrap_or(604_800)
    }

    pub fn set_min_oracle_count(env: Env, admin: Address, min_count: u32) {
        Self::require_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&StorageKey::MinOracleCount, &min_count);
        env.events().publish(
            (Symbol::new(&env, "min_oracle_count_updated"),),
            MinOracleCountUpdated { min_count },
        );
    }

    pub fn get_min_oracle_count(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::MinOracleCount)
            .unwrap_or(1)
    }

    // ── Data Submission ───────────────────────────────────────────────────────

    /// Submit a data point for a (data_type, key) pair.
    ///
    /// - `data_type`: category — "weather", "flight", "onchain", "disaster"
    /// - `key`: specific measurement — "rainfall:kisumu:2026-06", "flight:KQ100:2026-06-15"
    /// - `value`: 7-decimal fixed point (same precision as Stellar assets)
    /// - `confidence`: 0-100 reliability score
    /// - `timestamp`: Unix timestamp of the real-world observation
    pub fn submit_data(
        env: Env,
        oracle: Address,
        data_type: Symbol,
        key: Symbol,
        value: i128,
        confidence: u32,
        timestamp: u64,
    ) {
        oracle.require_auth();
        if confidence == 0 || confidence > 100 {
            panic_with_error!(&env, Error::InvalidConfidence);
        }

        let now = env.ledger().timestamp();
        if timestamp > now {
            panic_with_error!(&env, Error::InvalidTimestamp);
        }
        let ninety_days = 90 * 24 * 60 * 60;
        if timestamp < now.saturating_sub(ninety_days) {
            panic_with_error!(&env, Error::InvalidTimestamp);
        }

        // Verify oracle is registered and active for this data_type
        let oracle_key = StorageKey::Oracle(data_type.clone(), oracle.clone());
        let entry: OracleEntry = env
            .storage()
            .persistent()
            .get(&oracle_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::OracleNotRegistered));
        if !entry.active {
            panic_with_error!(&env, Error::Unauthorized);
        }

        // Load existing submissions for this (data_type, key)
        let dp_key = StorageKey::DataPoints(data_type.clone(), key.clone());
        let mut points: Vec<OracleDataPoint> = env
            .storage()
            .persistent()
            .get(&dp_key)
            .unwrap_or_else(|| Vec::new(&env));

        // Overwrite existing submission from this oracle; append if new
        let new_point = OracleDataPoint {
            oracle: oracle.clone(),
            value,
            confidence,
            timestamp,
        };
        let mut found = false;
        for i in 0..points.len() {
            if points.get_unchecked(i).oracle == oracle {
                points.set(i, new_point.clone());
                found = true;
                break;
            }
        }
        if !found {
            points.push_back(new_point);
        }

        env.storage().persistent().set(&dp_key, &points);

        env.events().publish(
            (Symbol::new(&env, "oracle_data_submitted"),),
            OracleDataSubmitted {
                oracle,
                data_type,
                key,
                value,
                confidence,
                timestamp,
            },
        );
    }

    // ── Verification ─────────────────────────────────────────────────────────

    /// Returns true if the aggregated oracle value satisfies `condition`.
    /// This is the single function the Claims Processor calls to decide payout.
    pub fn verify_trigger(
        env: Env,
        data_type: Symbol,
        key: Symbol,
        condition: TriggerCondition,
    ) -> bool {
        let median = Self::get_median_value(&env, &data_type, &key);
        match condition.comparison {
            TriggerComparison::LessThan => median < condition.threshold,
            TriggerComparison::GreaterThan => median > condition.threshold,
            TriggerComparison::Equal => median == condition.threshold,
            TriggerComparison::EqualWithTolerance => {
                let diff = median.saturating_sub(condition.threshold);
                diff.abs() <= condition.tolerance
            }
        }
    }

    /// Return the most recent submission from any oracle for (data_type, key).
    /// Panics with NoDataAvailable if no submissions exist.
    pub fn get_data(env: Env, data_type: Symbol, key: Symbol) -> OracleDataPoint {
        let points: Vec<OracleDataPoint> = env
            .storage()
            .persistent()
            .get(&StorageKey::DataPoints(data_type, key))
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoDataAvailable));
        if points.is_empty() {
            panic_with_error!(&env, Error::NoDataAvailable);
        }
        // Return the most recently timestamped submission
        let mut latest = points.get_unchecked(0);
        for i in 1..points.len() {
            let p = points.get_unchecked(i);
            if p.timestamp > latest.timestamp {
                latest = p;
            }
        }
        let max_data_age: u64 = env
            .storage()
            .instance()
            .get(&StorageKey::MaxDataAge)
            .unwrap_or(604_800);
        let now = env.ledger().timestamp();
        if now.saturating_sub(latest.timestamp) > max_data_age {
            panic_with_error!(&env, Error::NoDataAvailable);
        }
        latest
    }

    /// Return aggregated statistics across all oracle submissions for (data_type, key).
    pub fn get_aggregated(env: Env, data_type: Symbol, key: Symbol) -> AggregatedData {
        let points: Vec<OracleDataPoint> = env
            .storage()
            .persistent()
            .get(&StorageKey::DataPoints(data_type.clone(), key.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoDataAvailable));
        if points.is_empty() {
            panic_with_error!(&env, Error::NoDataAvailable);
        }
        let median_value = Self::get_median_value(&env, &data_type, &key);
        let oracle_count = points.len();
        let mut min_confidence = 100u32;
        let mut weighted_confidence_sum: u128 = 0;
        let mut total_weight: u128 = 0;
        let mut last_updated = 0u64;
        for i in 0..oracle_count {
            let p = points.get_unchecked(i);
            if p.confidence < min_confidence {
                min_confidence = p.confidence;
            }
            if p.timestamp > last_updated {
                last_updated = p.timestamp;
            }

            let oracle_key = StorageKey::Oracle(data_type.clone(), p.oracle.clone());
            if let Some(entry) = env
                .storage()
                .persistent()
                .get::<_, OracleEntry>(&oracle_key)
            {
                weighted_confidence_sum += (p.confidence as u128) * (entry.weight as u128);
                total_weight += entry.weight as u128;
            }
        }
        let confidence = match weighted_confidence_sum.checked_div(total_weight) {
            Some(c) => c as u32,
            None => 0u32,
        };
        AggregatedData {
            median_value,
            oracle_count,
            confidence,
            min_confidence,
            last_updated,
        }
    }

    /// Like `verify_trigger` but panics with `StaleData` if the newest submission
    /// is older than `max_age_seconds`. Use this in parametric claim paths that
    /// require fresh oracle data.
    pub fn verify_trigger_fresh(
        env: Env,
        data_type: Symbol,
        key: Symbol,
        condition: TriggerCondition,
        max_age_seconds: u64,
    ) -> bool {
        let dp_key = StorageKey::DataPoints(data_type.clone(), key.clone());
        let points: Vec<OracleDataPoint> = env
            .storage()
            .persistent()
            .get(&dp_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoDataAvailable));
        if points.is_empty() {
            panic_with_error!(&env, Error::NoDataAvailable);
        }

        let now = env.ledger().timestamp();
        let mut latest_ts = 0u64;
        for i in 0..points.len() {
            let ts = points.get_unchecked(i).timestamp;
            if ts > latest_ts {
                latest_ts = ts;
            }
        }
        if now.saturating_sub(latest_ts) > max_age_seconds {
            panic_with_error!(&env, Error::StaleData);
        }

        let median = Self::get_median_value(&env, &data_type, &key);
        match condition.comparison {
            TriggerComparison::LessThan => median < condition.threshold,
            TriggerComparison::GreaterThan => median > condition.threshold,
            TriggerComparison::Equal => median == condition.threshold,
            TriggerComparison::EqualWithTolerance => {
                let diff = median.saturating_sub(condition.threshold);
                diff.abs() <= condition.tolerance
            }
        }
    }

    /// Submit data for multiple keys in one call.
    /// Each tuple: (key, value, confidence, timestamp).
    pub fn batch_submit_data(
        env: Env,
        oracle: Address,
        data_type: Symbol,
        submissions: Vec<(Symbol, i128, u32, u64)>,
    ) {
        oracle.require_auth();
        let oracle_key = StorageKey::Oracle(data_type.clone(), oracle.clone());
        let entry: OracleEntry = env
            .storage()
            .persistent()
            .get(&oracle_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::OracleNotRegistered));
        if !entry.active {
            panic_with_error!(&env, Error::Unauthorized);
        }

        for i in 0..submissions.len() {
            let (key, value, confidence, timestamp) = submissions.get_unchecked(i);
            if confidence == 0 || confidence > 100 {
                panic_with_error!(&env, Error::InvalidConfidence);
            }

            let now = env.ledger().timestamp();
            if timestamp > now {
                panic_with_error!(&env, Error::InvalidTimestamp);
            }
            let ninety_days = 90 * 24 * 60 * 60;
            if timestamp < now.saturating_sub(ninety_days) {
                panic_with_error!(&env, Error::InvalidTimestamp);
            }

            let now = env.ledger().timestamp();
            if timestamp > now {
                panic_with_error!(&env, Error::InvalidTimestamp);
            }
            let dp_key = StorageKey::DataPoints(data_type.clone(), key.clone());
            let mut points: Vec<OracleDataPoint> = env
                .storage()
                .persistent()
                .get(&dp_key)
                .unwrap_or_else(|| Vec::new(&env));
            let new_point = OracleDataPoint {
                oracle: oracle.clone(),
                value,
                confidence,
                timestamp,
            };
            let mut found = false;
            for j in 0..points.len() {
                if points.get_unchecked(j).oracle == oracle {
                    points.set(j, new_point.clone());
                    found = true;
                    break;
                }
            }
            if !found {
                points.push_back(new_point);
            }
            env.storage().persistent().set(&dp_key, &points);

            env.events().publish(
                (Symbol::new(&env, "oracle_data_submitted"),),
                OracleDataSubmitted {
                    oracle: oracle.clone(),
                    data_type: data_type.clone(),
                    key,
                    value,
                    confidence,
                    timestamp,
                },
            );
        }
    }

    /// Submit multiple data readings in a single invocation — avoids the cost
    /// and latency of calling `submit_data` once per key.  All readings share
    /// the same `oracle` and `data_type`; each carries its own key, value,
    /// confidence, and timestamp.  Every reading is validated and persisted
    /// atomically within the single transaction.
    pub fn submit_data_batch(
        env: Env,
        oracle: Address,
        data_type: Symbol,
        submissions: Vec<OracleDataSubmission>,
    ) {
        oracle.require_auth();

        let oracle_key = StorageKey::Oracle(data_type.clone(), oracle.clone());
        let entry: OracleEntry = env
            .storage()
            .persistent()
            .get(&oracle_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::OracleNotRegistered));
        if !entry.active {
            panic_with_error!(&env, Error::Unauthorized);
        }

        for i in 0..submissions.len() {
            let sub = submissions.get_unchecked(i);
            if sub.confidence == 0 || sub.confidence > 100 {
                panic_with_error!(&env, Error::InvalidConfidence);
            }
            let now = env.ledger().timestamp();
            if sub.timestamp > now {
                panic_with_error!(&env, Error::InvalidTimestamp);
            }
            let dp_key = StorageKey::DataPoints(data_type.clone(), sub.key.clone());
            let mut points: Vec<OracleDataPoint> = env
                .storage()
                .persistent()
                .get(&dp_key)
                .unwrap_or_else(|| Vec::new(&env));
            let new_point = OracleDataPoint {
                oracle: oracle.clone(),
                value: sub.value,
                confidence: sub.confidence,
                timestamp: sub.timestamp,
            };
            let mut found = false;
            for j in 0..points.len() {
                if points.get_unchecked(j).oracle == oracle {
                    points.set(j, new_point.clone());
                    found = true;
                    break;
                }
            }
            if !found {
                points.push_back(new_point);
            }
            env.storage().persistent().set(&dp_key, &points);

            env.events().publish(
                (Symbol::new(&env, "oracle_data_submitted"),),
                OracleDataSubmitted {
                    oracle: oracle.clone(),
                    data_type: data_type.clone(),
                    key: sub.key,
                    value: sub.value,
                    confidence: sub.confidence,
                    timestamp: sub.timestamp,
                },
            );
        }
    }

    /// Upgrade the contract WASM in-place. Only the admin may call this.
    /// Storage is preserved across upgrades; only the execution code changes.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) {
        Self::require_admin(&env, &admin);
        env.deployer().update_current_contract_wasm(new_wasm_hash);
    }

    /// List all registered oracle addresses.
    pub fn get_oracles(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&StorageKey::OracleList)
            .unwrap_or_else(|| Vec::new(&env))
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        if *caller != admin {
            panic_with_error!(env, Error::Unauthorized);
        }
        caller.require_auth();
    }

    /// Compute the simple median of all submitted values for (data_type, key).
    fn get_median_value(env: &Env, data_type: &Symbol, key: &Symbol) -> i128 {
        let points: Vec<OracleDataPoint> = env
            .storage()
            .persistent()
            .get(&StorageKey::DataPoints(data_type.clone(), key.clone()))
            .unwrap_or_else(|| panic_with_error!(env, Error::NoDataAvailable));
        if points.is_empty() {
            panic_with_error!(env, Error::NoDataAvailable);
        }

        let max_data_age: u64 = env
            .storage()
            .instance()
            .get(&StorageKey::MaxDataAge)
            .unwrap_or(604_800);
        let now = env.ledger().timestamp();
        let min_confidence: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::MinConfidence)
            .unwrap_or(0);

        // Collect values and weights on the stack
        let mut values = [(0i128, 0u32); 100];
        let mut total_weight: u32 = 0;
        let mut n = 0;

        for i in 0..points.len() {
            let p = points.get_unchecked(i);
            if now.saturating_sub(p.timestamp) <= max_data_age && p.confidence >= min_confidence {
                let oracle_key = StorageKey::Oracle(data_type.clone(), p.oracle.clone());
                if let Some(entry) = env
                    .storage()
                    .persistent()
                    .get::<_, OracleEntry>(&oracle_key)
                {
                    if n < 100 {
                        values[n] = (p.value, entry.weight);
                        n += 1;
                        total_weight += entry.weight;
                    }
                }
            }
        }

        let min_oracle_count: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::MinOracleCount)
            .unwrap_or(1);
        if (n as u32) < min_oracle_count || n == 0 || total_weight == 0 {
            panic_with_error!(env, Error::NoDataAvailable);
        }

        // Native sort on the stack slice: O(N log N)
        let active_values = &mut values[0..n];
        active_values.sort_unstable_by_key(|&(val, _)| val);

        let half = total_weight / 2;
        let mut cumulative = 0;
        for i in 0..n {
            let (val, wt) = active_values[i];
            cumulative += wt;
            if cumulative > half {
                return val;
            } else if cumulative == half && total_weight.is_multiple_of(2) {
                if i + 1 < n {
                    return (val + active_values[i + 1].0) / 2;
                } else {
                    return val;
                }
            }
        }

        active_values[n - 1].0
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_advanced;
