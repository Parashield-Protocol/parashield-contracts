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
    contract, contracterror, contractimpl, contracttype, panic_with_error, token, Address,
    BytesN, Env, Symbol, Vec,
};

pub mod types;
pub use types::*;

// ─── Storage TTL ──────────────────────────────────────────────────────────────
/// Extend a persistent entry's TTL once it has fewer than ~30 days of life left
/// (at ~5s/ledger).
const TTL_THRESHOLD: u32 = 518_400; // ~30 days
/// Extend persistent entries out to ~1 year (at ~5s/ledger) so an oracle
/// registration doesn't silently expire from storage during a quiet period
/// with no submissions.
const TTL_EXTEND_TO: u32 = 6_312_000; // ~1 year

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
        let max_data_age: u64 = env
            .storage()
            .instance()
            .get(&StorageKey::MaxDataAge)
            .unwrap_or(604_800);
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

        let mut values = [(0i128, 0u32); 100];
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

        let active_values = &mut values[0..n];
        active_values.sort_unstable_by_key(|&(val, _)| val);

        let half = total_weight / 2;
        let mut cumulative = 0;
        let mut median_value = 0i128;
        for i in 0..n {
            let (val, wt) = active_values[i];
            cumulative += wt;
            if cumulative > half {
                median_value = val;
                break;
            } else if cumulative == half && total_weight.is_multiple_of(2) {
                if i + 1 < n {
                    median_value = (val + active_values[i + 1].0) / 2;
                } else {
                    median_value = val;
                }
                break;
            }
        }
        if median_value == 0 && cumulative <= half {
            median_value = active_values[n - 1].0;
        }

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
            (data_type, key, result, median, condition.threshold),
        );
        
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
            if timestamp > now {
                panic_with_error!(&env, Error::InvalidTimestamp);
            }
            let ninety_days = 90 * 24 * 60 * 60;
            if timestamp < now.saturating_sub(ninety_days) {
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

        // Collect values and weights on the stack (capped by MAX_DATA_POINTS)
        let mut values = [(0i128, 0u32); 100];
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
