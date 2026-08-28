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

#[cfg_attr(feature = "library", allow(unused_imports))]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, token, Address, Bytes,
    BytesN, Env, Symbol, Vec,
};

pub mod types;
pub use types::*;

// ─── Storage TTL ──────────────────────────────────────────────────────────────
/// Extend a persistent entry's TTL once it has fewer than ~30 days of life left
/// (at ~5s/ledger).
// Issue #342: kept in sync by hand across all 5 contracts (governance-dao,
// risk-pool, policy-engine, oracle-verifier, claims-processor) — extracting
// to a shared crate is a real follow-up, not done here to avoid touching
// every contract's Cargo.toml in one pass.
const TTL_THRESHOLD: u32 = 518_400; // ~30 days
/// Extend persistent entries out to ~1 year (at ~5s/ledger) so an oracle
/// registration doesn't silently expire from storage during a quiet period
/// with no submissions.
const TTL_EXTEND_TO: u32 = 6_312_000; // ~1 year

/// Grace period between an admin transfer being fully proposed/approved and the
/// proposed admin being able to `accept_admin` (issue #356). Hand-synced across
/// the 4 contracts that expose admin rotation (policy-engine, risk-pool,
/// oracle-verifier, claims-processor).
const ADMIN_TRANSFER_TIMELOCK: u64 = 48 * 60 * 60;

/// Maximum number of registered oracles. Bounds the median aggregation loop and
/// the worst-case weighted sum (MAX_ORACLES * max_weight * max_value) so it
/// cannot overflow i128.
#[allow(dead_code)]
const MAX_ORACLES: u32 = 100;
/// Maximum number of data points stored per (data_type, key).
const MAX_DATA_POINTS: u32 = 100;

/// Default minimum number of seconds a single oracle must wait between
/// submissions for the same data_type, used when no admin override has been
/// set via `set_min_submit_interval`. Chosen well below any realistic
/// real-world observation cadence (rainfall, flight status, etc.) while still
/// bounding how much storage/instruction budget a single misbehaving oracle
/// can consume by flooding submissions.
const DEFAULT_MIN_SUBMIT_INTERVAL: u64 = 30;

// ─── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
enum StorageKey {
    Initialized,
    Admin,
    /// OracleEntry for (data_type, oracle_address)
    Oracle(Symbol, Address),
    /// Vec<OracleDataPoint> — all submissions for (data_type, key)
    DataPoints(Symbol, Symbol),
    /// Vec<Address> — registered oracle addresses for a specific data_type
    OracleList(Symbol),
    /// Global minimum confidence threshold (u32)
    MinConfidence,
    MaxDataAge,
    PendingAdmin,
    /// Ledger timestamp (u64) at which the current `PendingAdmin` was set,
    /// used to enforce `ADMIN_TRANSFER_TIMELOCK` before `accept_admin`.
    PendingAdminSince,
    MinOracleCount,
    /// Minimum seconds between submissions from the same oracle for a given
    /// data_type (u64)
    MinSubmitInterval,
    /// Timestamp (u64) of an oracle's last accepted submission for a
    /// data_type — (data_type, oracle) → last submission time
    LastSubmission(Symbol, Address),
    /// Token used for oracle stake deposits (Address). Unset = staking disabled.
    StakeToken,
    /// Minimum stake (i128) an oracle must hold for a data_type before
    /// `add_oracle` will register it. Defaults to 0 (disabled).
    MinStake,
    /// Amount of stake token (i128) currently deposited by an oracle for a
    /// given data_type — (data_type, oracle) → amount.
    OracleStakeAmt(Symbol, Address),
    /// Address slashed stake is transferred to. Unset = slashed stake is
    /// retained by the contract (effectively burned).
    SlashTreasury,
    /// Guardian addresses authorized to approve critical actions (Vec<Address>).
    Guardians,
    /// Number of guardian approvals required to execute a critical action
    /// (u32). 0 means guardian multisig is disabled (admin acts alone).
    GuardianThreshold,
    /// A pending, not-yet-executed contract upgrade awaiting guardian approvals.
    PendingUpgrade,
    /// A pending admin-transfer proposal awaiting guardian approvals.
    PendingAdminChange,
    /// OracleReputation for (data_type, oracle_address).
    Reputation(Symbol, Address),
    /// Per-data-type max data age override (u64 seconds). Falls back to the
    /// global `MaxDataAge` when unset. A flight-status feed goes stale in
    /// minutes while a monthly rainfall total is valid for weeks — one global
    /// number cannot serve both.
    DataTypeMaxAge(Symbol),
    /// Per-data-type aggregation method. Falls back to `WeightedMedian`.
    AggregationMethod(Symbol),
    /// Whether plaintext `submit_data`/`batch_submit_data` is refused for a
    /// data_type (bool). Off by default — an existing deployment keeps
    /// submitting plaintext until it opts a sensitive data_type in.
    EncryptionRequired(Symbol),
    /// Vec<EncryptedOracleDataPoint> — ciphertext submissions for
    /// (data_type, key), stored separately from plaintext `DataPoints` since
    /// they are never aggregated or compared on-chain.
    EncryptedDataPoints(Symbol, Symbol),
    /// Maximum age (in seconds) for oracle submission timestamps to be accepted.
    /// Submissions older than `now - MaxTimestampAge` are rejected. Defaults to
    /// 90 days. Admin-configurable via `set_max_timestamp_age`.
    MaxTimestampAge,
    /// Maximum number of seconds into the future a submission timestamp may be.
    /// Protects against submissions with clocks far ahead of the ledger.
    /// Defaults to 60 seconds. Admin-configurable via `set_timestamp_future_buffer`.
    TimestampFutureBuffer,
    /// Per-data-type minimum oracle participation count for valid aggregation
    /// (u32). Falls back to the global `MinOracleCount` when unset. A data
    /// type with high-value triggers may require more independent submissions
    /// before the aggregation is considered valid.
    DataTypeMinOracleCount(Symbol),
    /// Per-product consensus threshold configuration (ConsensusThreshold).
    /// Specifies different oracle agreement levels for different data types/products.
    ConsensusThreshold(Symbol),
    /// Cross-validation rule between two data types — (source, target) → CrossValidationRule.
    CrossValidationRule(Symbol, Symbol),
    /// List of target data types that a source data type has cross-validation rules for.
    CrossValidationTargets(Symbol),
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
    InvalidAddress = 12,
    RateLimited = 13,
    InsufficientStake = 14,
    StakeTokenNotSet = 15,
    NoStake = 16,
    InvalidStakeAmount = 17,
    OracleStillActive = 18,
    NotGuardian = 19,
    AlreadyApprovedAction = 20,
    NoPendingUpgrade = 21,
    InvalidThreshold = 22,
    AdminTimelockNotExpired = 23,
    InvalidMaxAge = 24,
    EncryptionRequiredForType = 25,
    TimestampOutOfRange = 26,
    CrossValidationFailed = 27,
    InvalidInput = 28,
}

// ─── Contract ─────────────────────────────────────────────────────────────────

#[contract]
pub struct OracleVerifier;

#[contractimpl]
impl OracleVerifier {
    // ── Lifecycle ────────────────────────────────────────────────────────────

    /// One-time initialisation. Caller becomes admin.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&StorageKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }

        Self::validate_stellar_address(&env, &admin);
        admin.require_auth();

        env.storage()
            .instance()
            .set(&StorageKey::Initialized, &true);
        env.storage().instance().set(&StorageKey::Admin, &admin);


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
        Self::validate_stellar_address(&env, &oracle);
        if weight == 0 || weight > 100 {
            panic_with_error!(&env, Error::InvalidWeight);
        }
        let key = StorageKey::Oracle(data_type.clone(), oracle.clone());
        if env.storage().persistent().has(&key) {
            panic_with_error!(&env, Error::OracleAlreadyExists);
        }
        // Enforce a minimum economic stake so oracles have skin in the game.
        // Disabled by default (min_stake == 0) for backward compatibility with
        // deployments/tests that don't use the staking feature.
        let min_stake: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::MinStake)
            .unwrap_or(0);
        if min_stake > 0 {
            let staked: i128 = env
                .storage()
                .persistent()
                .get(&StorageKey::OracleStakeAmt(data_type.clone(), oracle.clone()))
                .unwrap_or(0);
            if staked < min_stake {
                panic_with_error!(&env, Error::InsufficientStake);
            }
        }
        let entry = OracleEntry {
            oracle: oracle.clone(),
            data_type: data_type.clone(),
            weight,
            active: true,
        };
        env.storage().persistent().set(&key, &entry);
        env.storage().persistent().extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);

        let mut list: Vec<Address> = env
            .storage()
            .instance()
            .get(&StorageKey::OracleList(data_type.clone()))
            .unwrap_or_else(|| Vec::new(&env));
        let mut already_present = false;
        for i in 0..list.len() {
            if list.get_unchecked(i) == oracle {
                already_present = true;
                break;
            }
        }
        if !already_present {
            if list.len() >= MAX_ORACLES {
                panic_with_error!(&env, Error::TooManyOracles);
            }
            list.push_back(oracle.clone());
            env.storage().instance().set(&StorageKey::OracleList(data_type.clone()), &list);
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
        env.storage().persistent().extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
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
        env.storage().persistent().extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);

        let list: Vec<Address> = env
            .storage()
            .instance()
            .get(&StorageKey::OracleList(data_type.clone()))
            .unwrap_or_else(|| Vec::new(&env));
        let mut pruned: Vec<Address> = Vec::new(&env);
        for addr in list.iter() {
            if addr != oracle {
                pruned.push_back(addr);
            }
        }
        env.storage().instance().set(&StorageKey::OracleList(data_type.clone()), &pruned);

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

    /// Set the maximum acceptable age for oracle submission timestamps (in seconds).
    /// Submissions with timestamps older than `now - max_timestamp_age` are rejected.
    /// Defaults to 90 days (7,776,000 seconds). Admin-only.
    pub fn set_max_timestamp_age(env: Env, admin: Address, max_timestamp_age: u64) {
        Self::require_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&StorageKey::MaxTimestampAge, &max_timestamp_age);
        env.events().publish(
            (Symbol::new(&env, "max_timestamp_age_updated"),),
            MaxTimestampAgeUpdated { max_timestamp_age },
        );
    }

    /// Return the configured max timestamp age (defaults to 90 days).
    pub fn get_max_timestamp_age(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&StorageKey::MaxTimestampAge)
            .unwrap_or(90 * 24 * 60 * 60)
    }

    /// Set the maximum number of seconds a submission timestamp may be ahead of
    /// the ledger. Protects against oracles with clocks far ahead of reality.
    /// Defaults to 60 seconds. Admin-only.
    pub fn set_timestamp_future_buffer(env: Env, admin: Address, seconds: u64) {
        Self::require_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&StorageKey::TimestampFutureBuffer, &seconds);
        env.events().publish(
            (Symbol::new(&env, "timestamp_future_buffer_updated"),),
            TimestampFutureBufferUpdated { seconds },
        );
    }

    /// Return the configured future buffer (defaults to 60 seconds).
    pub fn get_timestamp_future_buffer(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&StorageKey::TimestampFutureBuffer)
            .unwrap_or(60)
    }

    /// Propose a new admin. Only the current admin can call this.
    ///
    /// If a guardian threshold > 0 is configured (`set_guardians`), this does
    /// not activate the proposal immediately — it requires `threshold`
    /// guardians to call `approve_admin_change` first, guarding this
    /// takeover-capable operation against a single compromised admin key.
    /// With no guardians configured (default), behavior is unchanged.
    pub fn propose_new_admin(env: Env, admin: Address, new_admin: Address) {
        Self::require_admin(&env, &admin);
        Self::validate_stellar_address(&env, &new_admin);

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::GuardianThreshold)
            .unwrap_or(0);
        if threshold == 0 {
            env.storage()
                .instance()
                .set(&StorageKey::PendingAdmin, &new_admin);
            env.storage()
                .instance()
                .set(&StorageKey::PendingAdminSince, &env.ledger().timestamp());
            return;
        }

        let pending = PendingAdminChange {
            new_admin,
            approvals: Vec::new(&env),
        };
        env.storage()
            .instance()
            .set(&StorageKey::PendingAdminChange, &pending);
    }

    /// Guardian approval for a pending admin-change proposal. Once enough
    /// guardians have approved (>= threshold), the change is activated —
    /// `new_admin` must then still call `accept_admin` to take effect.
    pub fn approve_admin_change(env: Env, guardian: Address, new_admin: Address) {
        guardian.require_auth();

        let guardians: Vec<Address> = env
            .storage()
            .instance()
            .get(&StorageKey::Guardians)
            .unwrap_or_else(|| Vec::new(&env));
        let mut is_guardian = false;
        for g in guardians.iter() {
            if g == guardian {
                is_guardian = true;
                break;
            }
        }
        if !is_guardian {
            panic_with_error!(&env, Error::NotGuardian);
        }

        let mut pending: PendingAdminChange = env
            .storage()
            .instance()
            .get(&StorageKey::PendingAdminChange)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoPendingUpgrade));
        if pending.new_admin != new_admin {
            panic_with_error!(&env, Error::NoPendingUpgrade);
        }
        for a in pending.approvals.iter() {
            if a == guardian {
                panic_with_error!(&env, Error::AlreadyApprovedAction);
            }
        }
        pending.approvals.push_back(guardian);

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::GuardianThreshold)
            .unwrap_or(0);

        if pending.approvals.len() >= threshold {
            env.storage().instance().remove(&StorageKey::PendingAdminChange);
            env.storage()
                .instance()
                .set(&StorageKey::PendingAdmin, &new_admin);
            env.storage()
                .instance()
                .set(&StorageKey::PendingAdminSince, &env.ledger().timestamp());
        } else {
            env.storage()
                .instance()
                .set(&StorageKey::PendingAdminChange, &pending);
        }
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

        // Enforce the admin-rotation timelock (issue #356).
        let since: u64 = env.storage().instance()
            .get(&StorageKey::PendingAdminSince).unwrap_or(0);
        if env.ledger().timestamp() < since.saturating_add(ADMIN_TRANSFER_TIMELOCK) {
            panic_with_error!(&env, Error::AdminTimelockNotExpired);
        }

        env.storage().instance().set(&StorageKey::Admin, &admin);

        // Clear the slot explicitly using a clean None type hint
        let no_pending: Option<Address> = None;
        env.storage()
            .instance()
            .set(&StorageKey::PendingAdmin, &no_pending);
        env.storage().instance().remove(&StorageKey::PendingAdminSince);

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

    /// Set a max-data-age override for a single data type, in seconds.
    ///
    /// Freshness is not one number. A flight-status feed is worthless minutes
    /// after the gate closes; a monthly rainfall total stays valid for weeks.
    /// A single global `max_data_age` has to be loose enough for the slowest
    /// feed, which leaves the fastest one accepting data long past the point
    /// it describes reality.
    ///
    /// Pass `max_age = 0` to clear the override and fall back to the global
    /// value.
    pub fn set_data_type_max_age(env: Env, admin: Address, data_type: Symbol, max_age: u64) {
        Self::require_admin(&env, &admin);

        if max_age == 0 {
            env.storage()
                .instance()
                .remove(&StorageKey::DataTypeMaxAge(data_type.clone()));
        } else {
            env.storage()
                .instance()
                .set(&StorageKey::DataTypeMaxAge(data_type.clone()), &max_age);
        }

        env.events().publish(
            (Symbol::new(&env, "dt_max_age_updated"),),
            DataTypeMaxAgeUpdated { data_type, max_age },
        );
    }

    /// The max data age actually applied to a data type: the per-type override
    /// when one is set, otherwise the global value.
    pub fn get_data_type_max_age(env: Env, data_type: Symbol) -> u64 {
        Self::effective_max_age(&env, &data_type)
    }

    /// Choose how submissions for a data type are combined into one value.
    ///
    /// Defaults to [`AggregationMethod::WeightedMedian`], which is the safe
    /// choice against a minority of bad or adversarial oracles. Switch to an
    /// average only for feeds where every reporter is trusted and small
    /// genuine variations should be reflected rather than discarded.
    pub fn set_aggregation_method(
        env: Env,
        admin: Address,
        data_type: Symbol,
        method: AggregationMethod,
    ) {
        Self::require_admin(&env, &admin);

        env.storage()
            .instance()
            .set(&StorageKey::AggregationMethod(data_type.clone()), &method);

        env.events().publish(
            (Symbol::new(&env, "agg_method_updated"),),
            AggregationMethodUpdated { data_type, method },
        );
    }

    /// The aggregation method applied to a data type (default: weighted median).
    pub fn get_aggregation_method(env: Env, data_type: Symbol) -> AggregationMethod {
        Self::effective_aggregation_method(&env, &data_type)
    }

    /// Report whether the data for `(data_type, key)` is fresh enough to use,
    /// without panicking.
    ///
    /// Every other read path — `get_data`, `get_aggregated`, `verify_trigger` —
    /// aborts the transaction when data is missing or stale. That is correct
    /// for a claim evaluation but useless for a caller that wants to *decide*
    /// whether to evaluate. This returns the same facts as a value, so a
    /// front-end or a keeper can check first and a batch job can skip one
    /// policy instead of losing the whole batch.
    ///
    /// A key with no submissions at all reports `is_fresh: false` and
    /// `newest_age: u64::MAX` rather than failing.
    pub fn check_freshness(env: Env, data_type: Symbol, key: Symbol) -> FreshnessReport {
        let points: Vec<OracleDataPoint> = env
            .storage()
            .persistent()
            .get(&StorageKey::DataPoints(data_type.clone(), key.clone()))
            .unwrap_or_else(|| Vec::new(&env));

        let max_age = Self::effective_max_age(&env, &data_type);
        let now = env.ledger().timestamp();
        let min_oracle_count: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::MinOracleCount)
            .unwrap_or(1);

        let mut newest_age = u64::MAX;
        let mut fresh_count = 0u32;

        for i in 0..points.len() {
            let p = points.get_unchecked(i);
            // A future-dated point has age 0 — it is not stale, and
            // `submit_data` already rejects timestamps ahead of the ledger.
            let age = now.saturating_sub(p.timestamp);
            if age < newest_age {
                newest_age = age;
            }
            if age <= max_age {
                fresh_count += 1;
            }
        }

        FreshnessReport {
            data_type,
            key,
            is_fresh: fresh_count >= min_oracle_count.max(1),
            newest_age,
            fresh_count,
            total_count: points.len(),
            max_age,
        }
    }

    /// Check for stale oracle data across all submissions for a key.
    ///
    /// Unlike `check_freshness` which counts individual fresh submissions,
    /// this provides a holistic staleness report that includes the ratio
    /// of fresh to stale data and the age range of submissions. Useful for
    /// monitoring dashboards and automated alerts.
    ///
    /// Data is considered stale when its age exceeds the effective max_age
    /// for the data type. If more than half the submissions are stale, the
    /// entire data set is flagged as stale.
    pub fn check_staleness(env: Env, data_type: Symbol, key: Symbol) -> StalenessReport {
        let points: Vec<OracleDataPoint> = env
            .storage()
            .persistent()
            .get(&StorageKey::DataPoints(data_type.clone(), key.clone()))
            .unwrap_or_else(|| Vec::new(&env));

        let max_age = Self::effective_max_age(&env, &data_type);
        let now = env.ledger().timestamp();

        let mut stale_count = 0u32;
        let mut newest_age = u64::MAX;
        let mut oldest_fresh_age = 0u64;
        let total_count = points.len();

        for i in 0..points.len() {
            let p = points.get_unchecked(i);
            let age = now.saturating_sub(p.timestamp);

            if age < newest_age {
                newest_age = age;
            }

            if age > max_age {
                stale_count += 1;
            } else {
                if age > oldest_fresh_age {
                    oldest_fresh_age = age;
                }
            }
        }

        // If no submissions, report as stale with max ages
        if total_count == 0 {
            return StalenessReport {
                data_type,
                key,
                is_stale: true,
                oldest_fresh_age: 0,
                newest_age: u64::MAX,
                stale_count: 0,
                total_count: 0,
                max_age,
                freshness_ratio_bps: 0,
            };
        }

        let fresh_count = total_count.saturating_sub(stale_count);
        let freshness_ratio_bps = if total_count > 0 {
            (fresh_count as u64 * 10_000 / total_count as u64) as u32
        } else {
            0
        };

        // Data is stale if more than half of submissions are stale
        let is_stale = stale_count > total_count / 2;

        if is_stale {
            env.events().publish(
                (Symbol::new(&env, "stale_data_detected"),),
                StaleDataDetected {
                    data_type: data_type.clone(),
                    key: key.clone(),
                    stale_count,
                    total_count,
                    oldest_age: now.saturating_sub(newest_age),
                    max_age,
                },
            );
        }

        StalenessReport {
            data_type,
            key,
            is_stale,
            oldest_fresh_age,
            newest_age,
            stale_count,
            total_count,
            max_age,
            freshness_ratio_bps,
        }
    }

    /// Set the minimum number of oracle submissions required to form a consensus value.
    /// `min_count` must be at least 1; the default is 1.
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

    /// Return the minimum number of oracle submissions required for consensus (defaults to 1).
    pub fn get_min_oracle_count(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::MinOracleCount)
            .unwrap_or(1)
    }

    /// Set a per-data-type minimum oracle participation count override.
    ///
    /// Different data types have different risk profiles: a high-value
    /// crop-insurance trigger may require 3+ independent oracle submissions
    /// before the aggregation is trusted, while a low-stakes weather feed
    /// may be fine with the global default.
    ///
    /// Pass `min_count = 0` to clear the override and fall back to the
    /// global `MinOracleCount` value.
    pub fn set_data_type_min_oracle_count(
        env: Env,
        admin: Address,
        data_type: Symbol,
        min_count: u32,
    ) {
        Self::require_admin(&env, &admin);
        if min_count == 0 {
            env.storage()
                .instance()
                .remove(&StorageKey::DataTypeMinOracleCount(data_type.clone()));
        } else {
            env.storage()
                .instance()
                .set(&StorageKey::DataTypeMinOracleCount(data_type.clone()), &min_count);
        }
        env.events().publish(
            (Symbol::new(&env, "dt_min_oracle_count_updated"),),
            DataTypeMinOracleCountUpdated { data_type, min_count },
        );
    }

    /// The effective minimum oracle count for a data type: the per-type
    /// override when set, otherwise the global value.
    pub fn get_data_type_min_oracle_count(env: Env, data_type: Symbol) -> u32 {
        Self::effective_min_oracle_count(&env, &data_type)
    }

    /// Set per-product configurable consensus threshold for oracle agreement.
    /// Allows specifying different consensus requirements for different data types/products.
    ///
    /// The threshold is in basis points: 10000 = unanimous, 5000 = majority, etc.
    /// This replaces fixed consensus thresholds with flexible, per-product configuration.
    pub fn set_consensus_threshold(
        env: Env,
        admin: Address,
        data_type: Symbol,
        agreement_threshold_bps: u32,
    ) {
        Self::require_admin(&env, &admin);
        
        // Validate threshold is between 0 and 10000 basis points
        if agreement_threshold_bps > 10000 {
            panic_with_error!(&env, Error::InvalidInput);
        }
        
        let threshold = ConsensusThreshold {
            data_type: data_type.clone(),
            agreement_threshold_bps,
        };
        
        env.storage()
            .instance()
            .set(&StorageKey::ConsensusThreshold(data_type.clone()), &threshold);
        
        env.events().publish(
            (Symbol::new(&env, "consensus_threshold_updated"),),
            ConsensusThresholdUpdated { 
                data_type, 
                agreement_threshold_bps 
            },
        );
    }

    /// Get the consensus threshold for a specific data type/product.
    /// Returns the configured threshold, or a default of 5000 (50%, simple majority) if not configured.
    pub fn get_consensus_threshold(env: Env, data_type: Symbol) -> ConsensusThreshold {
        match env
            .storage()
            .instance()
            .get::<_, ConsensusThreshold>(&StorageKey::ConsensusThreshold(data_type.clone()))
        {
            Some(threshold) => threshold,
            None => ConsensusThreshold {
                data_type,
                agreement_threshold_bps: 5000, // Default to simple majority
            },
        }
    }

    // ── Cross-Validation (issue #430) ────────────────────────────────────────

    /// Add or update a cross-validation rule between two data types.
    ///
    /// When data is submitted for `source_type`, the aggregated values for
    /// `source_type` and `target_type` on the same key must not differ by
    /// more than `max_variance`. This catches inconsistent oracle data
    /// across correlated feeds (e.g. rainfall vs. temperature).
    pub fn set_cross_validation_rule(
        env: Env,
        admin: Address,
        source_type: Symbol,
        target_type: Symbol,
        max_variance: i128,
        description: Bytes,
    ) {
        Self::require_admin(&env, &admin);
        if max_variance < 0 {
            panic_with_error!(&env, Error::InvalidInput);
        }
        if source_type == target_type {
            panic_with_error!(&env, Error::InvalidInput);
        }

        let rule = CrossValidationRule {
            source_type: source_type.clone(),
            target_type: target_type.clone(),
            max_variance,
            description,
        };
        env.storage().instance().set(
            &StorageKey::CrossValidationRule(source_type.clone(), target_type.clone()),
            &rule,
        );

        // Track which targets this source has rules for
        let mut targets: Vec<Symbol> = env
            .storage()
            .instance()
            .get(&StorageKey::CrossValidationTargets(source_type.clone()))
            .unwrap_or_else(|| Vec::new(&env));
        let mut found = false;
        for i in 0..targets.len() {
            if targets.get_unchecked(i) == target_type {
                found = true;
                break;
            }
        }
        if !found {
            targets.push_back(target_type.clone());
            env.storage().instance().set(
                &StorageKey::CrossValidationTargets(source_type.clone()),
                &targets,
            );
        }

        env.events().publish(
            (Symbol::new(&env, "cross_validation_rule_added"),),
            CrossValidationRuleAdded {
                source_type,
                target_type,
                max_variance,
            },
        );
    }

    /// Remove a cross-validation rule between two data types.
    pub fn remove_cross_validation_rule(
        env: Env,
        admin: Address,
        source_type: Symbol,
        target_type: Symbol,
    ) {
        Self::require_admin(&env, &admin);
        let key = StorageKey::CrossValidationRule(source_type.clone(), target_type.clone());
        if !env.storage().instance().has(&key) {
            panic_with_error!(&env, Error::InvalidInput);
        }
        env.storage().instance().remove(&key);

        // Remove from targets list
        let mut targets: Vec<Symbol> = env
            .storage()
            .instance()
            .get(&StorageKey::CrossValidationTargets(source_type.clone()))
            .unwrap_or_else(|| Vec::new(&env));
        let mut pruned: Vec<Symbol> = Vec::new(&env);
        for i in 0..targets.len() {
            if targets.get_unchecked(i) != target_type {
                pruned.push_back(targets.get_unchecked(i));
            }
        }
        env.storage().instance().set(
            &StorageKey::CrossValidationTargets(source_type.clone()),
            &pruned,
        );

        env.events().publish(
            (Symbol::new(&env, "cross_validation_rule_removed"),),
            CrossValidationRuleRemoved {
                source_type,
                target_type,
            },
        );
    }

    /// Get the cross-validation rule between two data types, if one exists.
    pub fn get_cross_validation_rule(
        env: Env,
        source_type: Symbol,
        target_type: Symbol,
    ) -> Option<CrossValidationRule> {
        env.storage().instance().get(
            &StorageKey::CrossValidationRule(source_type, target_type),
        )
    }

    /// Get all cross-validation targets for a given source data type.
    pub fn get_cross_validation_targets(env: Env, source_type: Symbol) -> Vec<Symbol> {
        env.storage().instance().get(
            &StorageKey::CrossValidationTargets(source_type),
        ).unwrap_or_else(|| Vec::new(&env))
    }

    /// Check cross-validation rules between a source data type and all its
    /// configured targets for a given key. Returns Ok(()) if all rules pass,
    /// or the first failing rule's details.
    ///
    /// Called internally after aggregation to detect inconsistent oracle data
    /// across correlated feeds. Panics with `CrossValidationFailed` if any
    /// rule is violated.
    fn check_cross_validation(env: &Env, source_type: &Symbol, key: &Symbol) {
        let targets: Vec<Symbol> = env
            .storage()
            .instance()
            .get(&StorageKey::CrossValidationTargets(source_type.clone()))
            .unwrap_or_else(|| Vec::new(env));

        if targets.is_empty() {
            return;
        }

        // Get source aggregated value
        let source_value = Self::get_median_value(env, source_type, key);

        for i in 0..targets.len() {
            let target_type = targets.get_unchecked(i);
            let rule: CrossValidationRule = match env.storage().instance().get(
                &StorageKey::CrossValidationRule(source_type.clone(), target_type.clone()),
            ) {
                Some(r) => r,
                None => continue,
            };

            // Try to get target aggregated value — skip if no data available
            let target_points: Vec<OracleDataPoint> = match env.storage().persistent().get(
                &StorageKey::DataPoints(target_type.clone(), key.clone()),
            ) {
                Some(pts) => pts,
                None => continue,
            };
            if target_points.is_empty() {
                continue;
            }

            let target_value = Self::get_median_value(env, &target_type, key);
            let variance = source_value.saturating_sub(target_value).abs();

            if variance > rule.max_variance {
                panic_with_error!(env, Error::CrossValidationFailed);
            }
        }
    }

    /// Set the minimum number of seconds a single oracle must wait between
    /// submissions for the same data_type. Guards against a malicious or
    /// malfunctioning oracle flooding the contract with submissions to
    /// consume storage/instruction budget (griefing).
    pub fn set_min_submit_interval(env: Env, admin: Address, seconds: u64) {
        Self::require_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&StorageKey::MinSubmitInterval, &seconds);
        env.events().publish(
            (Symbol::new(&env, "min_submit_interval_updated"),),
            MinSubmitIntervalUpdated { seconds },
        );
    }

    /// Return the minimum number of seconds required between submissions
    /// from the same oracle for a data_type (defaults to 30).
    pub fn get_min_submit_interval(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&StorageKey::MinSubmitInterval)
            .unwrap_or(DEFAULT_MIN_SUBMIT_INTERVAL)
    }

    // ── Oracle Staking (economic security) ───────────────────────────────────

    /// Set the token used for oracle stake deposits. Admin-only.
    pub fn set_stake_token(env: Env, admin: Address, token: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&StorageKey::StakeToken, &token);
        env.events().publish(
            (Symbol::new(&env, "stake_token_updated"),),
            StakeTokenUpdated { token },
        );
    }

    /// Return the configured stake token, if any.
    pub fn get_stake_token(env: Env) -> Option<Address> {
        env.storage().instance().get(&StorageKey::StakeToken)
    }

    /// Set the minimum stake an oracle must hold for a data_type before
    /// `add_oracle` will register it. 0 disables the requirement (default).
    pub fn set_min_stake(env: Env, admin: Address, min_stake: i128) {
        Self::require_admin(&env, &admin);
        if min_stake < 0 {
            panic_with_error!(&env, Error::InvalidStakeAmount);
        }
        env.storage().instance().set(&StorageKey::MinStake, &min_stake);
        env.events().publish(
            (Symbol::new(&env, "min_stake_updated"),),
            MinStakeUpdated { min_stake },
        );
    }

    /// Return the currently configured minimum oracle stake (defaults to 0).
    pub fn get_min_stake(env: Env) -> i128 {
        env.storage().instance().get(&StorageKey::MinStake).unwrap_or(0)
    }

    /// Set the address slashed stake is transferred to. Admin-only. If never
    /// set, slashed stake stays locked in the contract.
    pub fn set_slash_treasury(env: Env, admin: Address, treasury: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&StorageKey::SlashTreasury, &treasury);
    }

    /// Deposit `amount` of the configured stake token toward `oracle`'s stake
    /// for `data_type`. Callable by anyone but requires the oracle's own
    /// authorization, so only the oracle (or someone it has delegated to)
    /// can grow its own stake.
    pub fn stake(env: Env, oracle: Address, data_type: Symbol, amount: i128) {
        oracle.require_auth();
        if amount <= 0 {
            panic_with_error!(&env, Error::InvalidStakeAmount);
        }
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&StorageKey::StakeToken)
            .unwrap_or_else(|| panic_with_error!(&env, Error::StakeTokenNotSet));

        token::Client::new(&env, &token_addr).transfer(
            &oracle,
            &env.current_contract_address(),
            &amount,
        );

        let stake_key = StorageKey::OracleStakeAmt(data_type.clone(), oracle.clone());
        let current: i128 = env.storage().persistent().get(&stake_key).unwrap_or(0);
        let total_stake = current + amount;
        env.storage().persistent().set(&stake_key, &total_stake);
        env.storage()
            .persistent()
            .extend_ttl(&stake_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        env.events().publish(
            (Symbol::new(&env, "stake_deposited"),),
            StakeDeposited {
                oracle,
                data_type,
                amount,
                total_stake,
            },
        );
    }

    /// Return the amount currently staked by `oracle` for `data_type`.
    pub fn get_oracle_stake(env: Env, data_type: Symbol, oracle: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&StorageKey::OracleStakeAmt(data_type, oracle))
            .unwrap_or(0)
    }

    // ── Geographic Weighting (Issue #426) ───────────────────────────────────

    /// Admin-only: Set geographic weighting multiplier for an oracle in basis points (10000 = 1.0x).
    pub fn set_oracle_geo_weight(
        env: Env,
        admin: Address,
        oracle: Address,
        data_type: Symbol,
        region: Symbol,
        geo_weight_bps: u32,
    ) {
        Self::require_admin(&env, &admin);
        if geo_weight_bps == 0 {
            panic_with_error!(&env, Error::InvalidConfidence);
        }
        let key = StorageKey::GeoWeight(data_type.clone(), oracle.clone(), region.clone());
        env.storage().persistent().set(&key, &geo_weight_bps);
        env.storage().persistent().extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);

        env.events().publish(
            (Symbol::new(&env, "geo_weight_updated"),),
            GeoWeightUpdated {
                oracle,
                data_type,
                region,
                geo_weight_bps,
            },
        );
    }

    /// Return the geographic weighting multiplier in basis points for (data_type, oracle, region).
    /// Defaults to 10,000 (1.0x baseline weight) if unconfigured.
    pub fn get_oracle_geo_weight(
        env: Env,
        oracle: Address,
        data_type: Symbol,
        region: Symbol,
    ) -> u32 {
        env.storage()
            .persistent()
            .get(&StorageKey::GeoWeight(data_type, oracle, region))
            .unwrap_or(10_000)
    }

    /// Return aggregated data for a specific geographic region, applying geographic weighting to active oracles.
    pub fn get_aggregated_for_region(
        env: Env,
        data_type: Symbol,
        key: Symbol,
        max_age_seconds: u64,
        target_region: Symbol,
    ) -> AggregatedData {
        let points: Vec<OracleDataPoint> = env
            .storage()
            .persistent()
            .get(&StorageKey::DataPoints(data_type.clone(), key.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoDataAvailable));
        if points.is_empty() {
            panic_with_error!(&env, Error::NoDataAvailable);
        }

        let now = env.ledger().timestamp();
        let min_confidence: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::MinConfidence)
            .unwrap_or(0);

        let mut values = [(0i128, 0u32); 100];
        let mut count: usize = 0;
        let mut total_effective_weight: u32 = 0;
        let mut weighted_confidence_sum: u64 = 0;
        let mut min_conf: u32 = 100;
        let mut newest_timestamp: u64 = 0;

        for i in 0..points.len() {
            let p = points.get_unchecked(i);
            let age = now.saturating_sub(p.timestamp);
            if age <= max_age_seconds && p.confidence >= min_confidence {
                let oracle_key = StorageKey::Oracle(data_type.clone(), p.oracle.clone());
                let base_weight = match env.storage().persistent().get::<_, OracleEntry>(&oracle_key) {
                    Some(entry) if entry.active => entry.weight,
                    _ => continue,
                };

                let geo_multiplier = Self::get_oracle_geo_weight(env.clone(), p.oracle.clone(), data_type.clone(), target_region.clone());
                let effective_weight = ((base_weight as u64 * geo_multiplier as u64) / 10_000) as u32;
                let effective_weight = if effective_weight == 0 { 1 } else { effective_weight };

                if count < 100 {
                    values[count] = (p.value, effective_weight);
                    count += 1;
                }

                total_effective_weight += effective_weight;
                weighted_confidence_sum += p.confidence as u64 * effective_weight as u64;
                if p.confidence < min_conf {
                    min_conf = p.confidence;
                }
                if p.timestamp > newest_timestamp {
                    newest_timestamp = p.timestamp;
                }
            }
        }

        if count == 0 {
            panic_with_error!(&env, Error::NoDataAvailable);
        }

        let slice = &mut values[..count];
        slice.sort_by(|a, b| a.0.cmp(&b.0));

        let half_weight = total_effective_weight / 2;
        let mut accum: u32 = 0;
        let mut median: i128 = slice[0].0;
        for &(val, weight) in slice.iter() {
            accum += weight;
            if accum >= half_weight {
                median = val;
                break;
            }
        }

        let avg_confidence = if total_effective_weight > 0 {
            (weighted_confidence_sum / total_effective_weight as u64) as u32
        } else {
            0
        };

        AggregatedData {
            median_value: median,
            oracle_count: count as u32,
            active_oracle_count: count as u32,
            confidence: avg_confidence,
            min_confidence: min_conf,
            last_updated: newest_timestamp,
        }
    }

    /// Withdraw the caller's full stake for `data_type`. Only permitted once
    /// the oracle is not an active registration for that data_type (never
    /// registered, or previously removed via `remove_oracle`) — an active
    /// oracle cannot pull its economic backing out from under a live
    /// registration.
    pub fn withdraw_stake(env: Env, oracle: Address, data_type: Symbol) {
        oracle.require_auth();

        let oracle_key = StorageKey::Oracle(data_type.clone(), oracle.clone());
        if let Some(entry) = env.storage().persistent().get::<_, OracleEntry>(&oracle_key) {
            if entry.active {
                panic_with_error!(&env, Error::OracleStillActive);
            }
        }

        let stake_key = StorageKey::OracleStakeAmt(data_type.clone(), oracle.clone());
        let amount: i128 = env.storage().persistent().get(&stake_key).unwrap_or(0);
        if amount <= 0 {
            panic_with_error!(&env, Error::NoStake);
        }
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&StorageKey::StakeToken)
            .unwrap_or_else(|| panic_with_error!(&env, Error::StakeTokenNotSet));

        env.storage().persistent().remove(&stake_key);
        token::Client::new(&env, &token_addr).transfer(
            &env.current_contract_address(),
            &oracle,
            &amount,
        );

        env.events().publish(
            (Symbol::new(&env, "stake_withdrawn"),),
            StakeWithdrawn {
                oracle,
                data_type,
                amount,
            },
        );
    }

    /// Slash `amount` from `oracle`'s stake for `data_type` as a penalty for
    /// provably incorrect data (verified off-chain / via governance and
    /// submitted by the admin). Caps at the oracle's current stake. If the
    /// slash brings the oracle below the configured `min_stake`, the oracle
    /// is deactivated so it can no longer submit until it re-stakes and is
    /// re-registered. Slashed funds move to the configured slash treasury,
    /// if any, otherwise remain locked in the contract.
    pub fn slash_oracle(
        env: Env,
        admin: Address,
        oracle: Address,
        data_type: Symbol,
        amount: i128,
        reason: Symbol,
    ) {
        Self::require_admin(&env, &admin);
        if amount <= 0 {
            panic_with_error!(&env, Error::InvalidStakeAmount);
        }

        let stake_key = StorageKey::OracleStakeAmt(data_type.clone(), oracle.clone());
        let current: i128 = env.storage().persistent().get(&stake_key).unwrap_or(0);
        if current <= 0 {
            panic_with_error!(&env, Error::NoStake);
        }
        let slashed = amount.min(current);
        let remaining = current - slashed;
        env.storage().persistent().set(&stake_key, &remaining);

        if let Some(token_addr) = env.storage().instance().get::<_, Address>(&StorageKey::StakeToken) {
            if let Some(treasury) = env.storage().instance().get::<_, Address>(&StorageKey::SlashTreasury) {
                token::Client::new(&env, &token_addr).transfer(
                    &env.current_contract_address(),
                    &treasury,
                    &slashed,
                );
            }
        }

        let min_stake: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::MinStake)
            .unwrap_or(0);
        if min_stake > 0 && remaining < min_stake {
            let oracle_key = StorageKey::Oracle(data_type.clone(), oracle.clone());
            if let Some(mut entry) = env.storage().persistent().get::<_, OracleEntry>(&oracle_key) {
                if entry.active {
                    entry.active = false;
                    env.storage().persistent().set(&oracle_key, &entry);

                    let list: Vec<Address> = env
                        .storage()
                        .instance()
                        .get(&StorageKey::OracleList(data_type.clone()))
                        .unwrap_or_else(|| Vec::new(&env));
                    let mut pruned: Vec<Address> = Vec::new(&env);
                    for addr in list.iter() {
                        if addr != oracle {
                            pruned.push_back(addr);
                        }
                    }
                    env.storage()
                        .instance()
                        .set(&StorageKey::OracleList(data_type.clone()), &pruned);
                }
            }
        }

        env.events().publish(
            (Symbol::new(&env, "oracle_slashed"),),
            OracleSlashed {
                oracle,
                data_type,
                amount: slashed,
                remaining_stake: remaining,
                reason,
            },
        );
    }

    // ── Guardian Multisig (critical actions) ─────────────────────────────────

    /// Configure the guardian set and approval threshold required for
    /// critical actions (currently: contract upgrades). Admin-only.
    /// `threshold == 0` disables the guardian requirement (default), so the
    /// admin alone can act — preserves existing single-admin behavior until
    /// guardians are explicitly configured.
    pub fn set_guardians(env: Env, admin: Address, guardians: Vec<Address>, threshold: u32) {
        Self::require_admin(&env, &admin);
        if threshold as u32 > guardians.len() {
            panic_with_error!(&env, Error::InvalidThreshold);
        }
        env.storage().instance().set(&StorageKey::Guardians, &guardians);
        env.storage()
            .instance()
            .set(&StorageKey::GuardianThreshold, &threshold);
        env.events().publish(
            (Symbol::new(&env, "guardians_updated"),),
            GuardiansUpdated { guardians, threshold },
        );
    }

    /// Return the current guardian set.
    pub fn get_guardians(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&StorageKey::Guardians)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Return the current guardian approval threshold (0 = disabled).
    pub fn get_guardian_threshold(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::GuardianThreshold)
            .unwrap_or(0)
    }

    /// Return the pending upgrade awaiting guardian approvals, if any.
    pub fn get_pending_upgrade(env: Env) -> Option<PendingUpgrade> {
        env.storage().instance().get(&StorageKey::PendingUpgrade)
    }

    /// Ledger timestamp at which the current pending admin transfer was
    /// registered, or `0` if none. `accept_admin` succeeds only once
    /// `now >= this + ADMIN_TRANSFER_TIMELOCK` (issue #356).
    pub fn get_pending_admin_since(env: Env) -> u64 {
        env.storage().instance().get(&StorageKey::PendingAdminSince).unwrap_or(0)
    }

    /// Guardian approval for the pending upgrade. Once enough guardians have
    /// approved (>= threshold), the upgrade executes immediately.
    pub fn approve_upgrade(env: Env, guardian: Address, new_wasm_hash: BytesN<32>) {
        guardian.require_auth();

        let guardians: Vec<Address> = env
            .storage()
            .instance()
            .get(&StorageKey::Guardians)
            .unwrap_or_else(|| Vec::new(&env));
        let mut is_guardian = false;
        for g in guardians.iter() {
            if g == guardian {
                is_guardian = true;
                break;
            }
        }
        if !is_guardian {
            panic_with_error!(&env, Error::NotGuardian);
        }

        let mut pending: PendingUpgrade = env
            .storage()
            .instance()
            .get(&StorageKey::PendingUpgrade)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoPendingUpgrade));
        if pending.new_wasm_hash != new_wasm_hash {
            panic_with_error!(&env, Error::NoPendingUpgrade);
        }

        for a in pending.approvals.iter() {
            if a == guardian {
                panic_with_error!(&env, Error::AlreadyApprovedAction);
            }
        }
        pending.approvals.push_back(guardian.clone());

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::GuardianThreshold)
            .unwrap_or(0);

        env.events().publish(
            (Symbol::new(&env, "upgrade_approved"),),
            UpgradeApproved {
                new_wasm_hash: new_wasm_hash.clone(),
                approver: guardian,
                approvals: pending.approvals.len(),
                threshold,
            },
        );

        if pending.approvals.len() >= threshold {
            env.storage().instance().remove(&StorageKey::PendingUpgrade);
            env.deployer().update_current_contract_wasm(new_wasm_hash);
        } else {
            env.storage().instance().set(&StorageKey::PendingUpgrade, &pending);
        }
    }

    /// Admin-only: cancel a pending upgrade before it collects enough
    /// guardian approvals.
    pub fn cancel_pending_upgrade(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        if !env.storage().instance().has(&StorageKey::PendingUpgrade) {
            panic_with_error!(&env, Error::NoPendingUpgrade);
        }
        env.storage().instance().remove(&StorageKey::PendingUpgrade);
    }

    // ── Data Submission ───────────────────────────────────────────────────────

    /// Submit a data point for a (data_type, key) pair.
    ///
    /// This function stores one reading from a registered oracle for a specific
    /// observation key. Multiple oracles may submit values for the same key;
    /// later aggregation does not discard or "average out" conflicting values
    /// by default. Instead, the contract computes a weighted median over all
    /// eligible submissions for that key, so a small number of contradictory
    /// readings do not dominate the result unless their assigned oracle weights
    /// do.
    ///
    /// For a reading to be considered during aggregation, the submission must
    /// be fresh enough for the configured max-data-age window, from an active
    /// oracle, and at or above the configured minimum confidence threshold. The
    /// aggregation logic also requires at least `min_oracle_count` eligible
    /// submissions before it will return a consensus value; if fewer are
    /// available, the call fails with `NoDataAvailable`.
    ///
    /// The final value is the weighted median of the eligible submissions, where
    /// each oracle's registered weight influences its position in the ordered
    /// set. If the total weight is even, the midpoint between the two middle
    /// values is returned.
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
        if Self::encryption_required(&env, &data_type) {
            panic_with_error!(&env, Error::EncryptionRequiredForType);
        }
        if confidence == 0 || confidence > 100 {
            panic_with_error!(&env, Error::InvalidConfidence);
        }

        let now = env.ledger().timestamp();
        let future_buffer: u64 = env
            .storage()
            .instance()
            .get(&StorageKey::TimestampFutureBuffer)
            .unwrap_or(60);
        if timestamp > now.saturating_add(future_buffer) {
            panic_with_error!(&env, Error::InvalidTimestamp);
        }
        let max_timestamp_age: u64 = env
            .storage()
            .instance()
            .get(&StorageKey::MaxTimestampAge)
            .unwrap_or(90 * 24 * 60 * 60);
        if timestamp < now.saturating_sub(max_timestamp_age) {
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
        // Keep the registration alive alongside the reading it authorized.
        env.storage()
            .persistent()
            .extend_ttl(&oracle_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        Self::enforce_rate_limit(&env, &data_type, &oracle, now);

        // Load existing submissions for this (data_type, key)
        let dp_key = StorageKey::DataPoints(data_type.clone(), key.clone());
        let points: Vec<OracleDataPoint> = env
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
        // Prune stale or unregistered/inactive oracle entries first
        let max_data_age = Self::effective_max_age(&env, &data_type);
        let mut pruned_points: Vec<OracleDataPoint> = Vec::new(&env);
        for i in 0..points.len() {
            let p = points.get_unchecked(i);
            if p.oracle == oracle {
                continue; // will be replaced
            }
            if now.saturating_sub(p.timestamp) <= max_data_age {
                let oracle_k = StorageKey::Oracle(data_type.clone(), p.oracle.clone());
                if let Some(e) = env.storage().persistent().get::<_, OracleEntry>(&oracle_k) {
                    if e.active {
                        pruned_points.push_back(p);
                    }
                }
            }
        }
        if pruned_points.len() >= MAX_DATA_POINTS {
            pruned_points.pop_front();
        }
        pruned_points.push_back(new_point);

        env.storage().persistent().set(&dp_key, &pruned_points);
        env.storage().persistent().extend_ttl(&dp_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        // Check cross-validation rules after storing new data
        Self::check_cross_validation(&env, &data_type, &key);

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

    // ── Data Encryption (issue #379) ──────────────────────────────────────────

    /// Require (or stop requiring) encrypted submissions for a data_type.
    ///
    /// Off by default — plaintext `submit_data`/`batch_submit_data` behave
    /// exactly as before for any data_type that never opts in. Once enabled,
    /// plaintext submissions for that data_type are refused and only
    /// `submit_encrypted_data` is accepted, for data types whose values are
    /// sensitive (private valuations, confidential off-chain metrics, etc.)
    /// and should not sit in plaintext on a public ledger.
    pub fn set_encryption_required(env: Env, admin: Address, data_type: Symbol, required: bool) {
        Self::require_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&StorageKey::EncryptionRequired(data_type.clone()), &required);
        env.events().publish(
            (Symbol::new(&env, "encryption_required_updated"),),
            EncryptionRequiredUpdated { data_type, required },
        );
    }

    /// Whether `data_type` currently requires encrypted submissions.
    pub fn get_encryption_required(env: Env, data_type: Symbol) -> bool {
        Self::encryption_required(&env, &data_type)
    }

    /// Admin-only: Invalidate all oracle data points for a given (data_type, key) pair.
    /// This removes bad or stale data from storage, preventing it from being used
    /// in future trigger verifications or aggregations.
    pub fn invalidate_data(env: Env, admin: Address, data_type: Symbol, key: Symbol) {
        Self::require_admin(&env, &admin);
        
        let storage_key = StorageKey::DataPoints(data_type.clone(), key.clone());
        env.storage().persistent().remove(&storage_key);
        
        env.events().publish(
            (Symbol::new(&env, "oracle_data_invalidated"),),
            (data_type, key),
        );
    }

    /// Submit an encrypted data point for a (data_type, key) pair.
    ///
    /// The contract never sees the plaintext value: `ciphertext` and `nonce`
    /// are opaque to it and stored as-is. Encryption/decryption happens
    /// entirely off-chain, between whichever parties hold the key for this
    /// data_type — the contract's only job is tamper-evident storage and
    /// making explicit, at the type level, that a submission is encrypted so
    /// a consumer never mistakes ciphertext for a usable value. Encrypted
    /// submissions are not fed into `verify_trigger`/`get_aggregated`, since
    /// aggregation and threshold comparison require the plaintext value.
    ///
    /// Confidence and timestamp validation mirror `submit_data`.
    pub fn submit_encrypted_data(
        env: Env,
        oracle: Address,
        data_type: Symbol,
        key: Symbol,
        ciphertext: Bytes,
        nonce: BytesN<12>,
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

        let oracle_key = StorageKey::Oracle(data_type.clone(), oracle.clone());
        let entry: OracleEntry = env
            .storage()
            .persistent()
            .get(&oracle_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::OracleNotRegistered));
        if !entry.active {
            panic_with_error!(&env, Error::Unauthorized);
        }
        env.storage()
            .persistent()
            .extend_ttl(&oracle_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        Self::enforce_rate_limit(&env, &data_type, &oracle, now);

        let dp_key = StorageKey::EncryptedDataPoints(data_type.clone(), key.clone());
        let mut points: Vec<EncryptedOracleDataPoint> = env
            .storage()
            .persistent()
            .get(&dp_key)
            .unwrap_or_else(|| Vec::new(&env));

        let new_point = EncryptedOracleDataPoint {
            oracle: oracle.clone(),
            ciphertext,
            nonce,
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
            if points.len() >= MAX_DATA_POINTS {
                points.pop_front();
            }
            points.push_back(new_point);
        }

        env.storage().persistent().set(&dp_key, &points);
        env.storage().persistent().extend_ttl(&dp_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        env.events().publish(
            (Symbol::new(&env, "oracle_encrypted_data_submitted"),),
            OracleEncryptedDataSubmitted {
                oracle,
                data_type,
                key,
                confidence,
                timestamp,
            },
        );
    }

    /// Return all encrypted submissions stored for (data_type, key), for an
    /// off-chain consumer holding the decryption key to decrypt and verify.
    pub fn get_encrypted_data(env: Env, data_type: Symbol, key: Symbol) -> Vec<EncryptedOracleDataPoint> {
        env.storage()
            .persistent()
            .get(&StorageKey::EncryptedDataPoints(data_type, key))
            .unwrap_or_else(|| Vec::new(&env))
    }

    // ── Verification ─────────────────────────────────────────────────────────

    /// Evaluate whether the aggregated oracle value satisfies a trigger condition.
    ///
    /// This is the single entry point the Claims Processor uses to decide
    /// whether a policy should payout. The function reads the aggregated value
    /// for the requested `(data_type, key)` pair and compares it to the
    /// configured threshold using the requested comparison operator.
    ///
    /// The comparison is performed on the same fixed-point numeric units used
    /// for oracle submissions, so the threshold and observed value should be
    /// expressed in the same precision. `LessThan`, `GreaterThan`, and `Equal`
    /// apply the standard relational comparison directly, while
    /// `EqualWithTolerance` treats the condition as satisfied when the
    /// absolute difference between the aggregated value and the threshold is
    /// less than or equal to the supplied tolerance.
    ///
    /// The return value is `true` when the condition is met and `false`
    /// otherwise.
    pub fn verify_trigger(
        env: Env,
        data_type: Symbol,
        key: Symbol,
        condition: TriggerCondition,
    ) -> bool {
        // Enforce minimum oracle participation before aggregation so a single
        // oracle cannot unilaterally determine the outcome.
        let min_count = Self::effective_min_oracle_count(&env, &data_type);
        let points: Vec<OracleDataPoint> = env
            .storage()
            .persistent()
            .get(&StorageKey::DataPoints(data_type.clone(), key.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoDataAvailable));
        let max_data_age = Self::effective_max_age(&env, &data_type);
        let now = env.ledger().timestamp();
        let min_confidence: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::MinConfidence)
            .unwrap_or(0);
        let mut eligible_count = 0u32;
        for i in 0..points.len() {
            let p = points.get_unchecked(i);
            if now.saturating_sub(p.timestamp) <= max_data_age && p.confidence >= min_confidence {
                let oracle_key = StorageKey::Oracle(data_type.clone(), p.oracle.clone());
                if let Some(entry) = env.storage().persistent().get::<_, OracleEntry>(&oracle_key) {
                    if entry.active {
                        eligible_count += 1;
                    }
                }
            }
        }
        if eligible_count < min_count {
            panic_with_error!(&env, Error::NoDataAvailable);
        }

        let median = Self::get_median_value(&env, &data_type, &key);
        let result = match condition.comparison {
            TriggerComparison::LessThan => median < condition.threshold,
            TriggerComparison::GreaterThan => median > condition.threshold,
            TriggerComparison::Equal => median == condition.threshold,
            TriggerComparison::EqualWithTolerance => {
                let diff = median.saturating_sub(condition.threshold);
                diff.abs() <= condition.tolerance
            }
        };

        // Update reputation for all contributing oracles
        let points: Vec<OracleDataPoint> = env
            .storage()
            .persistent()
            .get(&StorageKey::DataPoints(data_type.clone(), key.clone()))
            .unwrap_or_else(|| Vec::new(&env));

        let max_data_age = Self::effective_max_age(&env, &data_type);
        let now = env.ledger().timestamp();
        let min_confidence: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::MinConfidence)
            .unwrap_or(0);

        let tolerance = condition.tolerance;
        for i in 0..points.len() {
            let p = points.get_unchecked(i);
            if now.saturating_sub(p.timestamp) <= max_data_age && p.confidence >= min_confidence {
                let oracle_key = StorageKey::Oracle(data_type.clone(), p.oracle.clone());
                if let Some(entry) = env.storage().persistent().get::<_, OracleEntry>(&oracle_key) {
                    if entry.active {
                        // Oracle is accurate if its value is within tolerance of the median
                        let diff = (p.value - median).abs();
                        let accurate = diff <= tolerance;
                        Self::update_reputation(&env, &p.oracle, &data_type, accurate);
                    }
                }
            }
        }

        result
    }

    /// Like `verify_trigger` but also returns the aggregated oracle data —
    /// including the aggregated `confidence` score — alongside the boolean
    /// verdict. A caller can thus act on the trigger result and reason about how
    /// trustworthy that result is in a single call, instead of having to call
    /// `get_aggregated` separately to obtain the confidence (issue #388).
    pub fn verify_trigger_with_confidence(
        env: Env,
        data_type: Symbol,
        key: Symbol,
        condition: TriggerCondition,
    ) -> (bool, AggregatedData) {
        let agg = Self::get_aggregated(env.clone(), data_type.clone(), key.clone());
        let median = agg.median_value;
        let result = match condition.comparison {
            TriggerComparison::LessThan => median < condition.threshold,
            TriggerComparison::GreaterThan => median > condition.threshold,
            TriggerComparison::Equal => median == condition.threshold,
            TriggerComparison::EqualWithTolerance => {
                let diff = median.saturating_sub(condition.threshold);
                diff.abs() <= condition.tolerance
            }
        };
        (result, agg)
    }

    /// Return the most recent submission from any oracle for (data_type, key).
    /// Panics with NoDataAvailable if no submissions exist.
    pub fn get_data(env: Env, data_type: Symbol, key: Symbol) -> OracleDataPoint {
        let points: Vec<OracleDataPoint> = env
            .storage()
            .persistent()
            .get(&StorageKey::DataPoints(data_type.clone(), key))
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
        let max_data_age = Self::effective_max_age(&env, &data_type);
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

        let max_data_age = Self::effective_max_age(&env, &data_type);
        let now = env.ledger().timestamp();
        let min_confidence: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::MinConfidence)
            .unwrap_or(0);

        let mut values = [(0i128, 0u32); 100];
        let mut timestamps = [0u64; 100];
        let mut total_weight: u32 = 0;
        let mut n = 0;

        let mut oracle_count = 0u32;
        let mut min_confidence_val = 100u32;
        let mut weighted_confidence_sum: u128 = 0;
        let mut total_weight_sum: u128 = 0;
        let mut last_updated = 0u64;

        let points_cap = points.len().min(MAX_DATA_POINTS);
        for i in 0..points_cap {
            let p = points.get_unchecked(i);
            let oracle_key = StorageKey::Oracle(data_type.clone(), p.oracle.clone());
            if let Some(entry) = env
                .storage()
                .persistent()
                .get::<_, OracleEntry>(&oracle_key)
            {
                if entry.active {
                    oracle_count += 1;
                    min_confidence_val = min_confidence_val.min(p.confidence);
                    last_updated = last_updated.max(p.timestamp);
                    weighted_confidence_sum += (p.confidence as u128) * (entry.weight as u128);
                    total_weight_sum += entry.weight as u128;

                    if now.saturating_sub(p.timestamp) <= max_data_age && p.confidence >= min_confidence {
                        if n >= 100 {
                            panic_with_error!(&env, Error::TooManyOracles);
                        }
                        values[n] = (p.value, entry.weight);
                        timestamps[n] = p.timestamp;
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
            panic_with_error!(&env, Error::NoDataAvailable);
        }

        // Aggregate through the same dispatcher `verify_trigger` uses, so the
        // value reported here can never disagree with the value a claim is
        // actually evaluated against. This previously computed its own median
        // inline, which silently ignored the configured aggregation method.
        let active_values = &mut values[0..n];
        let active_timestamps = &timestamps[0..n];
        let median_value = match Self::effective_aggregation_method(&env, &data_type) {
            AggregationMethod::WeightedMedian => Self::weighted_median(active_values, total_weight),
            AggregationMethod::WeightedAverage => {
                Self::weighted_average(active_values, total_weight)
            }
            AggregationMethod::Mean => Self::mean(active_values),
            AggregationMethod::TimeWeightedAverage => {
                Self::time_weighted_average(&env, active_values, active_timestamps)
            }
        };

        let confidence = match weighted_confidence_sum.checked_div(total_weight_sum) {
            Some(c) => u32::try_from(c).unwrap_or(u32::MAX),
            None => 0u32,
        };

        let oracle_list: Vec<Address> = env
            .storage()
            .instance()
            .get(&StorageKey::OracleList(data_type))
            .unwrap_or_else(|| Vec::new(&env));
        let active_oracle_count: u32 = oracle_list.len();

        AggregatedData {
            median_value,
            oracle_count,
            active_oracle_count,
            confidence,
            min_confidence: min_confidence_val,
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

        // Enforce minimum oracle participation before aggregation so a single
        // oracle cannot unilaterally determine the outcome.
        let min_count = Self::effective_min_oracle_count(&env, &data_type);
        let now = env.ledger().timestamp();
        let max_data_age = Self::effective_max_age(&env, &data_type);
        let min_confidence_val: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::MinConfidence)
            .unwrap_or(0);
        let mut eligible_count = 0u32;
        for i in 0..points.len() {
            let p = points.get_unchecked(i);
            if now.saturating_sub(p.timestamp) <= max_data_age && p.confidence >= min_confidence_val {
                let oracle_key = StorageKey::Oracle(data_type.clone(), p.oracle.clone());
                if let Some(entry) = env.storage().persistent().get::<_, OracleEntry>(&oracle_key) {
                    if entry.active {
                        eligible_count += 1;
                    }
                }
            }
        }
        if eligible_count < min_count {
            panic_with_error!(&env, Error::NoDataAvailable);
        }

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
        let result = match condition.comparison {
            TriggerComparison::LessThan => median < condition.threshold,
            TriggerComparison::GreaterThan => median > condition.threshold,
            TriggerComparison::Equal => median == condition.threshold,
            TriggerComparison::EqualWithTolerance => {
                let diff = median.saturating_sub(condition.threshold);
                diff.abs() <= condition.tolerance
            }
        };
        
        // Emit event for verification result to enable monitoring and auditing
        env.events().publish(
            (Symbol::new(&env, "verification_result"),),
            (data_type.clone(), key, result, median, condition.threshold),
        );

        // Update reputation for all contributing oracles
        let max_data_age = Self::effective_max_age(&env, &data_type);
        let min_confidence: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::MinConfidence)
            .unwrap_or(0);

        let tolerance = condition.tolerance;
        for i in 0..points.len() {
            let p = points.get_unchecked(i);
            if now.saturating_sub(p.timestamp) <= max_data_age && p.confidence >= min_confidence {
                let oracle_key = StorageKey::Oracle(data_type.clone(), p.oracle.clone());
                if let Some(entry) = env.storage().persistent().get::<_, OracleEntry>(&oracle_key) {
                    if entry.active {
                        let diff = (p.value - median).abs();
                        let accurate = diff <= tolerance;
                        Self::update_reputation(&env, &p.oracle, &data_type, accurate);
                    }
                }
            }
        }
        
        result
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
        if Self::encryption_required(&env, &data_type) {
            panic_with_error!(&env, Error::EncryptionRequiredForType);
        }
        let oracle_key = StorageKey::Oracle(data_type.clone(), oracle.clone());
        let entry: OracleEntry = env
            .storage()
            .persistent()
            .get(&oracle_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::OracleNotRegistered));
        if !entry.active {
            panic_with_error!(&env, Error::Unauthorized);
        }
        // Keep the registration alive alongside the readings it authorized.
        env.storage()
            .persistent()
            .extend_ttl(&oracle_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        Self::enforce_rate_limit(&env, &data_type, &oracle, env.ledger().timestamp());

        for i in 0..submissions.len() {
            let (key, value, confidence, timestamp) = submissions.get_unchecked(i);
            if confidence == 0 || confidence > 100 {
                panic_with_error!(&env, Error::InvalidConfidence);
            }

            let now = env.ledger().timestamp();
            let future_buffer: u64 = env
                .storage()
                .instance()
                .get(&StorageKey::TimestampFutureBuffer)
                .unwrap_or(60);
            if timestamp > now.saturating_add(future_buffer) {
                panic_with_error!(&env, Error::InvalidTimestamp);
            }
            let max_timestamp_age: u64 = env
                .storage()
                .instance()
                .get(&StorageKey::MaxTimestampAge)
                .unwrap_or(90 * 24 * 60 * 60);
            if timestamp < now.saturating_sub(max_timestamp_age) {
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
            env.storage().persistent().extend_ttl(&dp_key, TTL_THRESHOLD, TTL_EXTEND_TO);

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
        if Self::encryption_required(&env, &data_type) {
            panic_with_error!(&env, Error::EncryptionRequiredForType);
        }

        let oracle_key = StorageKey::Oracle(data_type.clone(), oracle.clone());
        let entry: OracleEntry = env
            .storage()
            .persistent()
            .get(&oracle_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::OracleNotRegistered));
        if !entry.active {
            panic_with_error!(&env, Error::Unauthorized);
        }
        // Keep the registration alive alongside the readings it authorized.
        env.storage()
            .persistent()
            .extend_ttl(&oracle_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        Self::enforce_rate_limit(&env, &data_type, &oracle, env.ledger().timestamp());

        for i in 0..submissions.len() {
            let sub = submissions.get_unchecked(i);
            if sub.confidence == 0 || sub.confidence > 100 {
                panic_with_error!(&env, Error::InvalidConfidence);
            }
            let now = env.ledger().timestamp();
            let future_buffer: u64 = env
                .storage()
                .instance()
                .get(&StorageKey::TimestampFutureBuffer)
                .unwrap_or(60);
            if sub.timestamp > now.saturating_add(future_buffer) {
                panic_with_error!(&env, Error::InvalidTimestamp);
            }
            let max_timestamp_age: u64 = env
                .storage()
                .instance()
                .get(&StorageKey::MaxTimestampAge)
                .unwrap_or(90 * 24 * 60 * 60);
            if sub.timestamp < now.saturating_sub(max_timestamp_age) {
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
            env.storage().persistent().extend_ttl(&dp_key, TTL_THRESHOLD, TTL_EXTEND_TO);

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
    ///
    /// If a guardian threshold > 0 is configured (`set_guardians`), this call
    /// does not upgrade immediately — it registers the upgrade as pending and
    /// requires `threshold` guardians to call `approve_upgrade` before the
    /// WASM is actually replaced, guarding this irreversible operation
    /// against a single compromised admin key.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) {
        Self::require_admin(&env, &admin);

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::GuardianThreshold)
            .unwrap_or(0);
        if threshold == 0 {
            env.deployer().update_current_contract_wasm(new_wasm_hash);
            return;
        }

        let pending = PendingUpgrade {
            new_wasm_hash,
            approvals: Vec::new(&env),
        };
        env.storage().instance().set(&StorageKey::PendingUpgrade, &pending);
    }

    /// List all registered oracle addresses for a specific data_type.
    pub fn get_oracles(env: Env, data_type: Symbol) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&StorageKey::OracleList(data_type))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Return the current admin address. Panics with `NotInitialized` if the contract has not been set up.
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    // ── Oracle Reputation ────────────────────────────────────────────────────

    /// Get the reputation score for an oracle.
    pub fn get_reputation(env: Env, oracle: Address, data_type: Symbol) -> OracleReputation {
        let key = StorageKey::Reputation(data_type.clone(), oracle.clone());
        env.storage().persistent().get(&key).unwrap_or(OracleReputation {
            oracle,
            data_type,
            total_submissions: 0,
            accurate_submissions: 0,
            score: 500, // Default 50% score for new oracles
            last_updated: 0,
        })
    }

    /// Update oracle reputation based on whether submission was accurate.
    /// Called internally after aggregation to track oracle performance.
    fn update_reputation(env: &Env, oracle: &Address, data_type: &Symbol, accurate: bool) {
        let key = StorageKey::Reputation(data_type.clone(), oracle.clone());
        let mut rep: OracleReputation = env.storage().persistent().get(&key).unwrap_or(OracleReputation {
            oracle: oracle.clone(),
            data_type: data_type.clone(),
            total_submissions: 0,
            accurate_submissions: 0,
            score: 500,
            last_updated: 0,
        });

        rep.total_submissions += 1;
        if accurate {
            rep.accurate_submissions += 1;
        }
        // Calculate score as basis points (0-1000)
        if rep.total_submissions > 0 {
            rep.score = ((rep.accurate_submissions * 1000) / rep.total_submissions) as u32;
        }
        rep.last_updated = env.ledger().timestamp();

        env.storage().persistent().set(&key, &rep);
        env.storage().persistent().extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);

        env.events().publish(
            (Symbol::new(env, "reputation_updated"),),
            ReputationUpdated {
                oracle: oracle.clone(),
                data_type: data_type.clone(),
                score: rep.score,
                accurate: rep.accurate_submissions,
                total: rep.total_submissions,
            },
        );
    }

    /// Get the effective weight of an oracle, adjusted by reputation.
    /// Oracles with higher reputation get higher effective weights.
    pub fn get_effective_weight(env: Env, oracle: Address, data_type: Symbol) -> u32 {
        let oracle_key = StorageKey::Oracle(data_type.clone(), oracle.clone());
        let entry: OracleEntry = env
            .storage()
            .persistent()
            .get(&oracle_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::OracleNotRegistered));

        let rep = Self::get_reputation(env.clone(), oracle.clone(), data_type);
        // Effective weight = base weight * (score / 1000), minimum 1
        let effective = (entry.weight as u64 * rep.score as u64) / 1000;
        core::cmp::max(effective as u32, 1)
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Validate that an address has a valid Stellar format (56-char, starts with G or C).
    fn validate_stellar_address(env: &Env, address: &Address) {
        let addr_str = address.to_string();

        if addr_str.len() != 56 {
            panic_with_error!(env, Error::InvalidAddress);
        }

        let mut buf = [0u8; 56];
        addr_str.copy_into_slice(&mut buf);

        if buf[0] != b'G' && buf[0] != b'C' {
            panic_with_error!(env, Error::InvalidAddress);
        }
    }

    /// Enforce the per-oracle submission cooldown for `data_type`: panics with
    /// `RateLimited` if `oracle` submitted for this data_type more recently
    /// than `get_min_submit_interval()` seconds ago, otherwise records `now`
    /// as the new last-submission time. Call once per submission entry point
    /// (submit_data / submit_data_batch / batch_submit_data), not per reading,
    /// so a single call with many keys pays the check once.
    fn enforce_rate_limit(env: &Env, data_type: &Symbol, oracle: &Address, now: u64) {
        let min_interval = Self::get_min_submit_interval(env.clone());
        let key = StorageKey::LastSubmission(data_type.clone(), oracle.clone());
        if let Some(last) = env.storage().persistent().get::<_, u64>(&key) {
            if now.saturating_sub(last) < min_interval {
                panic_with_error!(env, Error::RateLimited);
            }
        }
        env.storage().persistent().set(&key, &now);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
    }

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

    /// Whether `data_type` currently requires `submit_encrypted_data` instead
    /// of plaintext submission. Off by default.
    fn encryption_required(env: &Env, data_type: &Symbol) -> bool {
        env.storage()
            .instance()
            .get(&StorageKey::EncryptionRequired(data_type.clone()))
            .unwrap_or(false)
    }

    /// The max data age that applies to a data type: the per-type override
    /// when set, otherwise the global value (default 7 days).
    fn effective_max_age(env: &Env, data_type: &Symbol) -> u64 {
        env.storage()
            .instance()
            .get(&StorageKey::DataTypeMaxAge(data_type.clone()))
            .unwrap_or_else(|| {
                env.storage()
                    .instance()
                    .get(&StorageKey::MaxDataAge)
                    .unwrap_or(604_800)
            })
    }

    /// The aggregation method configured for a data type. Weighted median is
    /// the default because it is the only one a minority of bad oracles
    /// cannot move.
    fn effective_aggregation_method(env: &Env, data_type: &Symbol) -> AggregationMethod {
        env.storage()
            .instance()
            .get(&StorageKey::AggregationMethod(data_type.clone()))
            .unwrap_or(AggregationMethod::WeightedMedian)
    }

    /// The effective minimum oracle count for a data type: the per-type
    /// override when set, otherwise the global value (default 1).
    fn effective_min_oracle_count(env: &Env, data_type: &Symbol) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::DataTypeMinOracleCount(data_type.clone()))
            .unwrap_or_else(|| {
                env.storage()
                    .instance()
                    .get(&StorageKey::MinOracleCount)
                    .unwrap_or(1)
            })
    }

    /// Compute the weighted median of active, sufficiently fresh submissions.
    fn get_median_value(env: &Env, data_type: &Symbol, key: &Symbol) -> i128 {
        let points: Vec<OracleDataPoint> = env
            .storage()
            .persistent()
            .get(&StorageKey::DataPoints(data_type.clone(), key.clone()))
            .unwrap_or_else(|| panic_with_error!(env, Error::NoDataAvailable));
        if points.is_empty() {
            panic_with_error!(env, Error::NoDataAvailable);
        }

        let max_data_age = Self::effective_max_age(env, data_type);
        let now = env.ledger().timestamp();
        let min_confidence: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::MinConfidence)
            .unwrap_or(0);

        // Collect values and weights on the stack (capped by MAX_DATA_POINTS)
        let mut values = [(0i128, 0u32); 100];
        let mut timestamps = [0u64; 100];
        let mut total_weight: u32 = 0;
        let mut n = 0;

        let points_cap = points.len().min(MAX_DATA_POINTS);
        for i in 0..points_cap {
            let p = points.get_unchecked(i);
            if now.saturating_sub(p.timestamp) <= max_data_age && p.confidence >= min_confidence {
                let oracle_key = StorageKey::Oracle(data_type.clone(), p.oracle.clone());
                if let Some(entry) = env
                    .storage()
                    .persistent()
                    .get::<_, OracleEntry>(&oracle_key)
                {
                    if entry.active {
                        if n >= 100 {
                            panic_with_error!(env, Error::TooManyOracles);
                        }
                        values[n] = (p.value, entry.weight);
                        timestamps[n] = p.timestamp;
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

        let active_values = &mut values[0..n];
        let active_timestamps = &timestamps[0..n];

        match Self::effective_aggregation_method(env, data_type) {
            AggregationMethod::WeightedMedian => Self::weighted_median(active_values, total_weight),
            AggregationMethod::WeightedAverage => Self::weighted_average(active_values, total_weight),
            AggregationMethod::Mean => Self::mean(active_values),
            AggregationMethod::TimeWeightedAverage => {
                Self::time_weighted_average(env, active_values, active_timestamps)
            }
        }
    }

    /// The value at the middle of cumulative weight.
    ///
    /// Because it picks an actual reported value rather than blending them, a
    /// minority of oracles cannot move the result no matter how extreme their
    /// submissions are. That property is why this is the default.
    fn weighted_median(values: &mut [(i128, u32)], total_weight: u32) -> i128 {
        // Native sort on the stack slice: O(N log N)
        values.sort_unstable_by_key(|&(val, _)| val);
        let n = values.len();

        let half = total_weight / 2;
        let mut cumulative = 0;
        for i in 0..n {
            let (val, wt) = values[i];
            cumulative += wt;
            if cumulative > half {
                return val;
            } else if cumulative == half && total_weight.is_multiple_of(2) {
                if i + 1 < n {
                    return (val + values[i + 1].0) / 2;
                } else {
                    return val;
                }
            }
        }

        values[n - 1].0
    }

    /// `sum(value * weight) / sum(weight)`, rounded toward zero.
    ///
    /// Every submission contributes in proportion to its registered weight, so
    /// small genuine variations show up instead of being discarded. The
    /// trade-off is the one the median avoids: a single extreme value drags
    /// the result, in proportion to that oracle's weight.
    ///
    /// The accumulator is i128 and the caller has already capped the input at
    /// `MAX_DATA_POINTS` (100) with weights bounded to 100, so the running sum
    /// cannot overflow for any value an oracle can submit. `saturating_*` is
    /// used regardless rather than trusting that reasoning to hold if those
    /// bounds ever change.
    fn weighted_average(values: &[(i128, u32)], total_weight: u32) -> i128 {
        let mut weighted_sum: i128 = 0;
        for &(val, wt) in values.iter() {
            weighted_sum = weighted_sum.saturating_add(val.saturating_mul(wt as i128));
        }
        weighted_sum / (total_weight as i128)
    }

    /// Unweighted arithmetic mean — every valid submission counts equally.
    ///
    /// Use when registered weights carry no meaning for a data type and
    /// treating them as significance would be misleading.
    fn mean(values: &[(i128, u32)]) -> i128 {
        let mut sum: i128 = 0;
        for &(val, _) in values.iter() {
            sum = sum.saturating_add(val);
        }
        sum / (values.len() as i128)
    }

    /// Oracle-weight- and time-weighted average.
    ///
    /// Oracles only ever submit point-in-time observations — there is no
    /// interval or duration attached to a single reading. TWA reconstructs
    /// a time-weighted average from those snapshots: sorted by timestamp,
    /// each value is treated as holding from its own timestamp until the
    /// next (later) submission's timestamp, or until "now" for the newest
    /// one, and contributes to the average in proportion to that duration
    /// times its oracle's registered weight. A plain average (weighted or
    /// not) would let a burst of submissions clustered in a short window
    /// dominate the result as heavily as one that held for hours; TWA
    /// corrects for that by weighting on how long each value actually
    /// prevailed.
    ///
    /// `values` and `timestamps` are parallel slices (same length, same
    /// index refers to the same submission) — callers build them from the
    /// same loop that already collects `(value, weight)` pairs for the
    /// other aggregation methods.
    fn time_weighted_average(env: &Env, values: &[(i128, u32)], timestamps: &[u64]) -> i128 {
        let n = values.len();
        if n == 1 {
            return values[0].0;
        }

        // Pair each (value, weight) with its timestamp and sort by time —
        // `values`/`timestamps` are capped at MAX_DATA_POINTS (100), so
        // this stays on the stack like the other aggregation methods.
        let mut by_time = [(0i128, 0u32, 0u64); 100];
        for i in 0..n {
            let (val, wt) = values[i];
            by_time[i] = (val, wt, timestamps[i]);
        }
        let active = &mut by_time[0..n];
        active.sort_unstable_by_key(|&(_, _, ts)| ts);

        let now = env.ledger().timestamp();
        let mut weighted_sum: i128 = 0;
        let mut total_weighted_duration: i128 = 0;
        for i in 0..n {
            let (val, wt, ts) = active[i];
            let end = if i + 1 < n { active[i + 1].2 } else { now.max(ts) };
            // A submission sharing its timestamp with the next (or with
            // "now") held for zero observed seconds; floor at 1 second so
            // it still contributes rather than vanishing from the average.
            let duration = end.saturating_sub(ts).max(1) as i128;
            let weighted_duration = duration.saturating_mul(wt as i128);
            weighted_sum = weighted_sum.saturating_add(val.saturating_mul(weighted_duration));
            total_weighted_duration = total_weighted_duration.saturating_add(weighted_duration);
        }

        if total_weighted_duration == 0 {
            return active[n - 1].0;
        }
        weighted_sum / total_weighted_duration
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_advanced;
