//! Parashield Policy Engine
//!
//! Manages insurance products and policies.
//!
//! Flow
//! ─────
//! 1. Admin calls `create_product` to define a new insurance product.
//! 2. User calls `buy_policy` — transfers premium to this contract,
//!    and the contract locks coverage USDC from its pool balance.
//! 3. The Claims Processor calls `pay_claim` / `expire_policy` to update
//!    policy status after processing. It also performs the USDC transfer.
//!
//! Architecture note on Claimable Balances
//! ─────────────────────────────────────────
//! Stellar's ClaimPredicate cannot encode data-driven conditions
//! (e.g., "rainfall < 50mm"). Therefore this contract acts as the
//! escrow: it holds USDC and the Claims Processor calls `token.transfer`
//! to pay the policyholder when the oracle confirms a trigger.
#![no_std]
extern crate alloc;
use alloc::string::ToString;


use soroban_sdk::{
    contract, contractimpl, contracttype, contracterror, panic_with_error,
    token, Address, BytesN, Env, Symbol, Vec,
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
const TTL_THRESHOLD: u32 = 518_400;
/// Extend persistent entries out to ~1 year (at ~5s/ledger) so long-lived
/// products and policies don't get evicted from storage before they mature.
const TTL_EXTEND_TO: u32 = 6_312_000;

// ─── Admin rotation ───────────────────────────────────────────────────────────

/// Grace period between an admin transfer being fully proposed/approved and
/// the proposed admin being able to `accept_admin` (issue #356). Gives the
/// wider system time to react to a hostile or mistaken rotation. Hand-synced
/// across the 4 contracts that expose admin rotation (policy-engine,
/// risk-pool, oracle-verifier, claims-processor).
const ADMIN_TRANSFER_TIMELOCK: u64 = 48 * 60 * 60;

// ─── Batch limits ─────────────────────────────────────────────────────────────

/// Upper bound on the number of policies `batch_buy_policy` will create in one
/// call, so a single transaction cannot blow Soroban's instruction budget.
#[cfg(any(test, feature = "testutils", not(feature = "library")))]
const MAX_BATCH_BUY: u32 = 20;

// ─── Pagination ───────────────────────────────────────────────────────────────

/// Upper bound on the number of entries a paginated query will return in one
/// call. Without a cap, a caller could pass `limit = u32::MAX` and force the
/// contract to build one huge `Vec`, blowing Soroban's instruction budget.
#[cfg(any(test, feature = "testutils", not(feature = "library")))]
const MAX_PAGE_SIZE: u32 = 100;

// ─── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
enum StorageKey {
    Initialized,
    Admin,
    UsdcToken,
    OracleAddress,
    ClaimsProcessor,
    /// InsuranceProduct
    Product(u128),
    /// Policy
    Policy(u128),
    /// Vec<u128> — product IDs for a user
    UserPolicies(Address),
    /// Vec<u128> — all active product IDs
    ActiveProducts,
    NextProductId,
    NextPolicyId,
    Paused,
    /// Maps (category, oracle_key) -> product_id for uniqueness constraint
    ProductKey((Symbol, Symbol)),
    PendingAdmin,
    /// Ledger timestamp (u64) at which the current `PendingAdmin` was set, used
    /// to enforce `ADMIN_TRANSFER_TIMELOCK` before `accept_admin` succeeds.
    PendingAdminSince,
    /// Contract version (u32) for storage migration tracking
    Version,
    /// Guardian addresses authorized to approve critical actions (Vec<Address>).
    Guardians,
    /// Number of guardian approvals required to execute a critical action
    /// (u32). 0 means guardian multisig is disabled (admin acts alone).
    GuardianThreshold,
    /// A pending, not-yet-executed contract upgrade awaiting guardian approvals.
    PendingUpgrade,
    /// A pending admin-transfer proposal awaiting guardian approvals.
    PendingAdminChange,
    /// How long before `end_time` a policy counts as expiring soon (u64
    /// seconds). Defaults to `DEFAULT_EXPIRY_WARNING_WINDOW`.
    ExpiryWarningWindow,
    /// Marks that a `PolicyExpiringSoon` event has already been emitted for a
    /// policy, so repeat calls cannot spam the event log.
    ExpiryWarned(u128),
}

/// Approximate Stellar ledger close time in seconds, used to convert
/// wall-clock TTL windows into ledger counts for `extend_ttl`.
#[allow(dead_code)]
const LEDGER_SECONDS: u64 = 5;

/// How long before `end_time` a policy is considered to be expiring soon,
/// when the admin has not configured a window.
///
/// Seven days is chosen to be long enough that a holder who checks in weekly
/// still sees the warning while cover is live, and short enough that the
/// warning means something — a 90-day window on a 90-day policy would fire
/// immediately and be ignored.
const DEFAULT_EXPIRY_WARNING_WINDOW: u64 = 7 * 24 * 60 * 60;

/// Maximum policies a single `notify_expiring_policies` call will scan.
/// Bounds the instruction budget of a permissionless entry point.
const MAX_EXPIRY_SCAN: u32 = 50;

/// Extra time added on top of a policy's own duration when extending the TTL
/// of its `Policy` entry, so the Claims Processor still has time to evaluate
/// and settle a claim after the policy's `end_time` (issue #186).
#[allow(dead_code)]
const POLICY_CLAIMS_BUFFER_SECONDS: u64 = 90 * 24 * 60 * 60;

// ─── Errors ───────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized      = 1,
    NotInitialized          = 2,
    Unauthorized            = 3,
    ProductNotFound         = 4,
    ProductNotActive        = 5,
    PolicyNotFound          = 6,
    PolicyNotActive         = 7,
    CoverageOutOfRange      = 8,
    DurationTooLong         = 9,
    InsufficientPool        = 10,
    AlreadyClaimed          = 11,
    AlreadyExpired          = 12,
    InvalidPremiumRate      = 13,
    InvalidTriggerThreshold = 14,
    DuplicateProductKey    = 15,
    InvalidCoverageRange    = 16,
    InvalidToken            = 17,
    ClaimsProcessorNotSet   = 18,
    InvalidDurationRange    = 19,
    InvalidOracleKey        = 20,
    Overflow               = 21,
    InvalidAddress          = 22,
    InvalidVersion          = 23,
    NotGuardian             = 24,
    AlreadyApprovedAction   = 25,
    NoPendingUpgrade        = 26,
    InvalidThreshold        = 27,
    AdminTimelockNotExpired = 28,
    EmptyBatch              = 29,
    BatchTooLarge           = 30,
    NotExpiringSoon         = 31,
    InvalidWarningWindow    = 32,
}

// ─── Contract ─────────────────────────────────────────────────────────────────

#[cfg(any(test, feature = "testutils", not(feature = "library")))]
#[contract]
pub struct PolicyEngine;

#[cfg(any(test, feature = "testutils", not(feature = "library")))]
#[contractimpl]
impl PolicyEngine {

    // ── Lifecycle ────────────────────────────────────────────────────────────

    /// One-time initialisation. Wires up the USDC token and oracle contracts.
    /// Panics with `AlreadyInitialized` on a second call, or `InvalidToken` if
    /// `usdc_token` does not expose a `balance` entry-point.
    pub fn initialize(
        env: Env,
        admin: Address,
        usdc_token: Address,
        oracle_address: Address,
    ) {
        if env.storage().instance().has(&StorageKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        // require_auth() validates all addresses at the protocol level, so we
        // do not need manual address format validation here.
        let admin_str = admin.to_string();
        //
        if admin_str.len() != 56 {
            panic_with_error!(&env, Error::InvalidAddress);
        }
        let mut admin_buf = [0u8; 56];
        admin_str.copy_into_slice(&mut admin_buf);
        if admin_buf[0] != b'G' && admin_buf[0] != b'C' {
            panic_with_error!(&env, Error::InvalidAddress);
        }

        let usdc_str = usdc_token.to_string();
        let oracle_str = oracle_address.to_string();
        //
        //
        if usdc_str.len() != 56 {
            panic_with_error!(&env, Error::InvalidAddress);
        }
        let mut usdc_buf = [0u8; 56];
        usdc_str.copy_into_slice(&mut usdc_buf);
        if usdc_buf[0] != b'C' {
            panic_with_error!(&env, Error::InvalidAddress);
        }

        if oracle_str.len() != 56 {
            panic_with_error!(&env, Error::InvalidAddress);
        }
        let mut oracle_buf = [0u8; 56];
        oracle_str.copy_into_slice(&mut oracle_buf);
        if oracle_buf[0] != b'C' {
            panic_with_error!(&env, Error::InvalidAddress);
        }
        
        let balance_res = env.try_invoke_contract::<i128, soroban_sdk::Error>(
            &usdc_token,
            &Symbol::new(&env, "balance"),
            soroban_sdk::vec![&env, env.current_contract_address().to_val()],
        );
        if balance_res.is_err() {
            panic_with_error!(&env, Error::InvalidToken);
        }

        admin.require_auth();
        env.storage().instance().set(&StorageKey::Initialized, &true);
        env.storage().instance().set(&StorageKey::Admin, &admin);
        env.storage().instance().set(&StorageKey::UsdcToken, &usdc_token);
        env.storage().instance().set(&StorageKey::OracleAddress, &oracle_address);
        env.storage().instance().set(&StorageKey::NextProductId, &1u128);
        env.storage().instance().set(&StorageKey::NextPolicyId,  &1u128);
        env.storage().instance().set(&StorageKey::ActiveProducts, &Vec::<u128>::new(&env));
        // No pending admin initially
        env.storage().instance().remove(&StorageKey::PendingAdmin);

        env.events().publish(
            (Symbol::new(&env, "initialized"),),
            Initialized {
                admin: admin.clone(),
                usdc_token: usdc_token.clone(),
                oracle_address: oracle_address.clone(),
            },
        );
    }

    /// Set the Claims Processor address. Called once after deploying claims contract.
    pub fn set_claims_processor(env: Env, admin: Address, claims_processor: Address) {
        Self::require_admin(&env, &admin);
        Self::validate_stellar_address(&env, &claims_processor);
        env.storage().instance().set(&StorageKey::ClaimsProcessor, &claims_processor);
        env.events().publish(
            (Symbol::new(&env, "claims_processor_updated"),),
            ClaimsProcessorUpdated {
                claims_processor: claims_processor.clone(),
            },
        );
    }

    // ── Product Management (admin only) ──────────────────────────────────────

    /// Admin-only: create a new insurance product and return its ID.
    /// `params.premium_rate_bps` must be 1-10000; `params.coverage_amount` must be positive.
    pub fn create_product(env: Env, admin: Address, params: CreateProductParams) -> u128 {
        Self::require_admin(&env, &admin);
        if params.premium_rate_bps == 0 || params.premium_rate_bps > 10_000 {
            panic_with_error!(&env, Error::InvalidPremiumRate);
        }
        // trigger_threshold must be positive and within the 7-decimal fixed-point
        // range used across the protocol (1 unit = 0.0000001; max ≈ 1 quadrillion).
        if params.trigger_threshold <= 0
            || params.trigger_threshold > 1_000_000_000_000_000_000_000i128
        {
            panic_with_error!(&env, Error::InvalidTriggerThreshold);
        }
        // Coverage bounds must form a valid, positive range: 0 < min < max.
        // Rejects free coverage (min == 0) and inverted ranges (min >= max).
        if params.coverage_min <= 0 || params.coverage_min >= params.coverage_max {
            panic_with_error!(&env, Error::InvalidCoverageRange);
        }
        // max_duration_days must be positive and less than 3650 (up to 10 years)
        // Rejects 0-day policies and unrealistically long durations
        if params.max_duration_days == 0 || params.max_duration_days > 3650 {
            panic_with_error!(&env, Error::InvalidDurationRange);
        }
        // oracle_key must be at least 3 characters — defense-in-depth against
        // trivially unresolvable keys that the oracle-verifier can never match.
        // Soroban Symbol only accepts [a-zA-Z0-9_], so character-set is already
        // enforced by the type; this adds a minimum-length semantic guard.
        {
            const MIN_LEN: usize = 3;
            let key_repr = params.oracle_key.to_string();
            if key_repr.len() < MIN_LEN {
                panic_with_error!(&env, Error::InvalidOracleKey);
            }
        }

        // Check for duplicate (category, oracle_key) pair
        let key = (params.category.clone(), params.oracle_key.clone());
        if env.storage().persistent().has(&StorageKey::ProductKey(key.clone())) {
            panic_with_error!(&env, Error::DuplicateProductKey);
        }

        let id = Self::next_product_id(&env);
        let product = InsuranceProduct {
            id,
            name:               params.name,
            category:           params.category,
            oracle_key:         params.oracle_key,
            trigger_type:       params.trigger_type,
            oracle_data_type:   params.oracle_data_type,
            trigger_threshold:  params.trigger_threshold,
            trigger_comparison: params.trigger_comparison,
            coverage_min:       params.coverage_min,
            coverage_max:       params.coverage_max,
            premium_rate_bps:   params.premium_rate_bps,
            max_duration_days:  params.max_duration_days,
            status:             ProductStatus::Active,
            created_at:         env.ledger().timestamp(),
        };
        env.storage().persistent().set(&StorageKey::Product(id), &product);
        env.storage().persistent().extend_ttl(&StorageKey::Product(id), TTL_THRESHOLD, TTL_EXTEND_TO);

        // Store the (category, oracle_key) -> product_id mapping for uniqueness
        env.storage().persistent().set(&StorageKey::ProductKey(key.clone()), &id);
        env.storage().persistent().extend_ttl(&StorageKey::ProductKey(key), TTL_THRESHOLD, TTL_EXTEND_TO);

        let mut products: Vec<u128> = env.storage().instance()
            .get(&StorageKey::ActiveProducts).unwrap_or_else(|| Vec::new(&env));
        products.push_back(id);
        env.storage().instance().set(&StorageKey::ActiveProducts, &products);

        env.events().publish(
            (Symbol::new(&env, "product_created"),),
            ProductCreated {
                product_id: id,
                name: product.name.clone(),
                category: product.category.clone(),
                premium_rate_bps: product.premium_rate_bps,
            },
        );
        id
    }

    /// Admin-only: suspend sales for a product without deleting it.
    /// The product is removed from the active-products list; existing policies are unaffected.
    pub fn pause_product(env: Env, admin: Address, product_id: u128) {
        Self::require_admin(&env, &admin);
        let mut product: InsuranceProduct = Self::load_product(&env, product_id);
        product.status = ProductStatus::Paused;
        env.storage().persistent().set(&StorageKey::Product(product_id), &product);
        Self::extend_to_max(&env, &StorageKey::Product(product_id));

        let mut products: Vec<u128> = env.storage().instance()
            .get(&StorageKey::ActiveProducts).unwrap_or_else(|| Vec::new(&env));
        let mut idx: Option<u32> = None;
        for i in 0..products.len() {
            if products.get_unchecked(i) == product_id {
                idx = Some(i);
                break;
            }
        }
        if let Some(i) = idx {
            products.remove(i);
            env.storage().instance().set(&StorageKey::ActiveProducts, &products);
        }

        env.events().publish(
            (Symbol::new(&env, "product_paused"),),
            ProductPaused { product_id },
        );
    }

    /// Admin-only: permanently retire a product. It is removed from the active list and its
    /// `(category, oracle_key)` slot is freed so another product may reuse it.
    pub fn deprecate_product(env: Env, admin: Address, product_id: u128) {
        Self::require_admin(&env, &admin);
        let mut product: InsuranceProduct = Self::load_product(&env, product_id);
        product.status = ProductStatus::Deprecated;
        env.storage().persistent().set(&StorageKey::Product(product_id), &product);
        Self::extend_to_max(&env, &StorageKey::Product(product_id));

        // Remove from the ActiveProducts list on deprecation
        let mut products: Vec<u128> = env.storage().instance()
            .get(&StorageKey::ActiveProducts).unwrap_or_else(|| Vec::new(&env));
        let mut idx: Option<u32> = None;
        for i in 0..products.len() {
            if products.get_unchecked(i) == product_id {
                idx = Some(i);
                break;
            }
        }
        if let Some(i) = idx {
            products.remove(i);
            env.storage().instance().set(&StorageKey::ActiveProducts, &products);
        }

        // Remove the (category, oracle_key) mapping to allow reuse of the key
        let key = (product.category, product.oracle_key);
        env.storage().persistent().remove(&StorageKey::ProductKey(key));
    }

    // ── Policy Lifecycle ──────────────────────────────────────────────────────

    /// Buy an insurance policy.
    ///
    /// The buyer must have approved this contract to spend `premium` USDC
    /// (or the transaction must include the token transfer auth).
    /// Premium = coverage_amount * premium_rate_bps / 10_000.
    ///
    /// `oracle_key` is the specific measurement key to watch, e.g.
    /// `symbol_short!("kis2606")` for Kisumu June 2026 rainfall.
    pub fn buy_policy(
        env: Env,
        buyer: Address,
        product_id: u128,
        coverage_amount: i128,
        duration_days: u32,
        oracle_key: Symbol,
    ) -> u128 {
        buyer.require_auth();
        Self::ensure_not_paused(&env);
        Self::buy_policy_inner(&env, &buyer, product_id, coverage_amount, duration_days, oracle_key)
    }

    /// Buy several policies in a single transaction (issue #357). The buyer
    /// authorizes once and every line item is created atomically — if any item
    /// is invalid the whole call reverts and no premium is transferred.
    /// Returns the new policy IDs in the same order as `items`.
    pub fn batch_buy_policy(env: Env, buyer: Address, items: Vec<BatchBuyItem>) -> Vec<u128> {
        buyer.require_auth();
        Self::ensure_not_paused(&env);

        let n = items.len();
        if n == 0 {
            panic_with_error!(&env, Error::EmptyBatch);
        }
        if n > MAX_BATCH_BUY {
            panic_with_error!(&env, Error::BatchTooLarge);
        }

        let mut ids = Vec::new(&env);
        for item in items.iter() {
            ids.push_back(Self::buy_policy_inner(
                &env,
                &buyer,
                item.product_id,
                item.coverage_amount,
                item.duration_days,
                item.oracle_key,
            ));
        }
        ids
    }

    /// Shared body of `buy_policy` / `batch_buy_policy`. Assumes the caller has
    /// already run `buyer.require_auth()` and the not-paused check.
    fn buy_policy_inner(
        env: &Env,
        buyer: &Address,
        product_id: u128,
        coverage_amount: i128,
        duration_days: u32,
        oracle_key: Symbol,
    ) -> u128 {
        let env = env.clone();
        let buyer = buyer.clone();
        let product = Self::load_product(&env, product_id);
        if product.status != ProductStatus::Active {
            panic_with_error!(&env, Error::ProductNotActive);
        }
        if coverage_amount < product.coverage_min || coverage_amount > product.coverage_max {
            panic_with_error!(&env, Error::CoverageOutOfRange);
        }
        if duration_days == 0 || duration_days > product.max_duration_days {
            panic_with_error!(&env, Error::DurationTooLong);
        }

        // Premium calculation: premium = coverage * rate * duration_days / 365 / 10_000
        // where coverage and premium are in USDC stroops (7 decimal places),
        // premium_rate_bps is in basis points (e.g., 500 = 5%).
        // Use checked operations to prevent overflow on large coverage amounts
        if coverage_amount > 1_000_000_000_000 {
            panic_with_error!(&env, Error::CoverageOutOfRange);
        }
        let premium = coverage_amount
            .checked_mul(product.premium_rate_bps as i128)
            .and_then(|v| v.checked_mul(duration_days as i128))
            .and_then(|v| v.checked_div(365))
            .and_then(|v| v.checked_div(10_000))
            .unwrap_or_else(|| panic_with_error!(&env, Error::CoverageOutOfRange));
        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        // Pull premium from buyer into this contract
        token::Client::new(&env, &usdc)
            .transfer(&buyer, &env.current_contract_address(), &premium);

        let now        = env.ledger().timestamp();
        let duration_secs = (duration_days as u64)
            .checked_mul(86_400)
            .unwrap_or_else(|| panic_with_error!(&env, Error::CoverageOutOfRange));
        let end_time   = now.checked_add(duration_secs)
            .unwrap_or_else(|| panic_with_error!(&env, Error::CoverageOutOfRange));
        let policy_id  = Self::next_policy_id(&env);

        let policy = Policy {
            id: policy_id,
            product_id,
            policyholder: buyer.clone(),
            coverage_amount,
            premium_paid: premium,
            oracle_key,
            oracle_data_type: product.oracle_data_type,
            trigger_threshold: product.trigger_threshold,
            trigger_comparison: product.trigger_comparison,
            start_time: now,
            end_time,
            status: PolicyStatus::Active,
            created_at: now,
        };
        env.storage().persistent().set(&StorageKey::Policy(policy_id), &policy);
        env.storage().persistent().extend_ttl(&StorageKey::Policy(policy_id), TTL_THRESHOLD, TTL_EXTEND_TO);

        // Append to user's policy list
        let user_key = StorageKey::UserPolicies(buyer.clone());
        let mut user_policies: Vec<u128> = env.storage().persistent()
            .get(&user_key).unwrap_or_else(|| Vec::new(&env));
        user_policies.push_back(policy_id);
        env.storage().persistent().set(&user_key, &user_policies);
        env.storage().persistent().extend_ttl(&user_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        env.events().publish(
            (Symbol::new(&env, "policy_created"),),
            PolicyCreated {
                policy_id,
                product_id,
                policyholder: buyer.clone(),
                coverage_amount,
                premium_paid: premium,
            },
        );

        policy_id
    }

    /// Cancel an active policy and refund the premium to the policyholder.
    /// Only the policyholder may cancel, and only while the policy is Active.
    pub fn cancel_policy(env: Env, policyholder: Address, policy_id: u128) -> i128 {
        policyholder.require_auth();
        let mut policy: Policy = Self::load_policy(&env, policy_id);
        if policy.policyholder != policyholder {
            panic_with_error!(&env, Error::Unauthorized);
        }
        if policy.status != PolicyStatus::Active {
            panic_with_error!(&env, Error::PolicyNotActive);
        }
        policy.status = PolicyStatus::Cancelled;
        env.storage().persistent().set(&StorageKey::Policy(policy_id), &policy);
        Self::extend_to_max(&env, &StorageKey::Policy(policy_id));
        Self::remove_policy_from_user(&env, &policyholder, policy_id);

        // Pro-rate the refund: only return the unearned portion of the premium.
        // Earned = premium_paid * elapsed / total_duration; refund = premium_paid - earned.
        let now = env.ledger().timestamp();
        let elapsed = now.saturating_sub(policy.start_time);
        let total_duration = policy.end_time.saturating_sub(policy.start_time);
        let refund = if total_duration == 0 {
            policy.premium_paid
        } else {
            let elapsed_capped = elapsed.min(total_duration);
            let earned = policy.premium_paid.checked_mul(elapsed_capped as i128)
                .and_then(|v| v.checked_div(total_duration as i128))
                .unwrap_or_else(|| panic_with_error!(&env, Error::Overflow));
            policy.premium_paid.saturating_sub(earned)
        };

        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        if refund > 0 {
            token::Client::new(&env, &usdc)
                .transfer(&env.current_contract_address(), &policyholder, &refund);
        }

        env.events().publish(
            (Symbol::new(&env, "policy_cancelled"),),
            PolicyCancelled {
                policy_id,
                policyholder: policyholder.clone(),
                refund_amount: refund,
            },
        );
        refund
    }

    /// Transfer ownership of an Active policy from `from` to `to` (issue #358).
    ///
    /// Both parties must authorize: the current holder consents to giving the
    /// policy up and the recipient consents to taking it on (so a policy can
    /// never be pushed onto an unwilling address). Coverage terms, premium
    /// paid and timing are unchanged — only the payout recipient moves.
    pub fn transfer_policy(env: Env, from: Address, to: Address, policy_id: u128) {
        from.require_auth();
        to.require_auth();
        Self::ensure_not_paused(&env);
        Self::validate_stellar_address(&env, &to);

        let mut policy: Policy = Self::load_policy(&env, policy_id);
        if policy.policyholder != from {
            panic_with_error!(&env, Error::Unauthorized);
        }
        if policy.status != PolicyStatus::Active {
            panic_with_error!(&env, Error::PolicyNotActive);
        }
        if from == to {
            // No-op transfer — nothing to do, and skipping keeps the user
            // index free of duplicate entries.
            return;
        }

        policy.policyholder = to.clone();
        env.storage().persistent().set(&StorageKey::Policy(policy_id), &policy);
        Self::extend_to_max(&env, &StorageKey::Policy(policy_id));

        // Move the id from the sender's index to the recipient's.
        Self::remove_policy_from_user(&env, &from, policy_id);
        let to_key = StorageKey::UserPolicies(to.clone());
        let mut to_policies: Vec<u128> = env.storage().persistent()
            .get(&to_key).unwrap_or_else(|| Vec::new(&env));
        to_policies.push_back(policy_id);
        env.storage().persistent().set(&to_key, &to_policies);
        env.storage().persistent().extend_ttl(&to_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        env.events().publish(
            (Symbol::new(&env, "policy_transferred"),),
            PolicyTransferred { policy_id, from, to },
        );
    }

    // ── Status updates (called by Claims Processor) ──────────────────────────

    /// Mark a policy as Claimed and pay coverage to the policyholder.
    /// Only callable by the registered Claims Processor.
    pub fn pay_claim(env: Env, caller: Address, policy_id: u128) {
        Self::require_claims_processor(&env, &caller);
        let mut policy: Policy = Self::load_policy(&env, policy_id);
        match policy.status {
            PolicyStatus::Claimed   => panic_with_error!(&env, Error::AlreadyClaimed),
            PolicyStatus::Expired   => panic_with_error!(&env, Error::AlreadyExpired),
            PolicyStatus::Cancelled => panic_with_error!(&env, Error::PolicyNotActive),
            PolicyStatus::Active    => {}
        }
        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let token_client = token::Client::new(&env, &usdc);
        match token_client.try_transfer(&env.current_contract_address(), &policy.policyholder, &policy.coverage_amount) {
            Ok(Ok(())) => {}
            _ => {
                panic_with_error!(&env, Error::InsufficientPool);
            }
        }

        policy.status = PolicyStatus::Claimed;
        env.storage().persistent().set(&StorageKey::Policy(policy_id), &policy);
        Self::extend_to_max(&env, &StorageKey::Policy(policy_id));
        Self::remove_policy_from_user(&env, &policy.policyholder, policy_id);

        env.events().publish(
            (Symbol::new(&env, "claim_paid"),),
            PolicyClaimed {
                policy_id,
                policyholder: policy.policyholder.clone(),
                coverage_amount: policy.coverage_amount,
            },
        );
    }

    /// Mark a policy as Expired (trigger not met before end_time).
    /// Premium remains in the contract as earned pool revenue.
    pub fn expire_policy(env: Env, caller: Address, policy_id: u128) {
        Self::require_claims_processor(&env, &caller);
        let mut policy: Policy = Self::load_policy(&env, policy_id);
        match policy.status {
            PolicyStatus::Claimed   => panic_with_error!(&env, Error::AlreadyClaimed),
            PolicyStatus::Expired   => panic_with_error!(&env, Error::AlreadyExpired),
            PolicyStatus::Cancelled => panic_with_error!(&env, Error::PolicyNotActive),
            PolicyStatus::Active    => {}
        }
        policy.status = PolicyStatus::Expired;
        env.storage().persistent().set(&StorageKey::Policy(policy_id), &policy);
        Self::extend_to_max(&env, &StorageKey::Policy(policy_id));
        Self::remove_policy_from_user(&env, &policy.policyholder, policy_id);
        env.events().publish(
            (Symbol::new(&env, "policy_expired"),),
            PolicyExpired { policy_id },
        );
    }


    // ── Expiry notification ───────────────────────────────────────────────────

    /// Set how long before `end_time` a policy counts as expiring soon.
    ///
    /// The window must be non-zero: a zero window would make the warning fire
    /// at the same instant cover lapses, which is the situation this mechanism
    /// exists to avoid.
    pub fn set_expiry_warning_window(env: Env, admin: Address, window: u64) {
        Self::require_admin(&env, &admin);
        if window == 0 {
            panic_with_error!(&env, Error::InvalidWarningWindow);
        }
        env.storage()
            .instance()
            .set(&StorageKey::ExpiryWarningWindow, &window);
        env.events().publish(
            (Symbol::new(&env, "expiry_window_updated"),),
            ExpiryWarningWindowUpdated { window },
        );
    }

    /// The configured expiry warning window in seconds (default: 7 days).
    pub fn get_expiry_warning_window(env: Env) -> u64 {
        Self::expiry_warning_window(&env)
    }

    /// Report where a policy sits relative to its own expiry, without panicking
    /// on state and without emitting anything.
    ///
    /// A caller deciding whether to renew, or a keeper deciding whether a
    /// notification is worth paying for, needs this as a value rather than as
    /// a transaction that might abort.
    pub fn get_policy_expiry_info(env: Env, policy_id: u128) -> PolicyExpiryInfo {
        let policy: Policy = Self::load_policy(&env, policy_id);
        let now = env.ledger().timestamp();
        let window = Self::expiry_warning_window(&env);

        let seconds_remaining = policy.end_time.saturating_sub(now);
        let state = if policy.status != PolicyStatus::Active {
            ExpiryState::NotActive
        } else if now >= policy.end_time {
            ExpiryState::Lapsed
        } else if seconds_remaining <= window {
            ExpiryState::ExpiringSoon
        } else {
            ExpiryState::Active
        };

        PolicyExpiryInfo {
            policy_id,
            state,
            end_time: policy.end_time,
            seconds_remaining,
            warned: env
                .storage()
                .persistent()
                .has(&StorageKey::ExpiryWarned(policy_id)),
        }
    }

    /// Emit `PolicyExpiringSoon` for a policy that has entered its warning
    /// window, so off-chain infrastructure can notify the holder.
    ///
    /// Permissionless on purpose. The party who most needs the reminder is the
    /// holder, and requiring the admin to trigger it would make coverage
    /// continuity depend on the admin running a keeper — exactly the kind of
    /// silent dependency that leaves users uncovered.
    ///
    /// Emits at most once per policy: the first successful call records a flag
    /// and later calls panic with `NotExpiringSoon`, so a permissionless entry
    /// point cannot be used to flood the event log.
    ///
    /// Panics with `NotExpiringSoon` when the policy is not Active, has not
    /// yet entered the window, has already lapsed, or has already been warned.
    pub fn notify_policy_expiring(env: Env, policy_id: u128) {
        let policy: Policy = Self::load_policy(&env, policy_id);

        if policy.status != PolicyStatus::Active {
            panic_with_error!(&env, Error::PolicyNotActive);
        }

        let now = env.ledger().timestamp();
        let window = Self::expiry_warning_window(&env);

        // Already lapsed is `expire_policy`'s job, not a warning.
        if now >= policy.end_time {
            panic_with_error!(&env, Error::NotExpiringSoon);
        }

        let seconds_remaining = policy.end_time - now;
        if seconds_remaining > window {
            panic_with_error!(&env, Error::NotExpiringSoon);
        }

        let warned_key = StorageKey::ExpiryWarned(policy_id);
        if env.storage().persistent().has(&warned_key) {
            panic_with_error!(&env, Error::NotExpiringSoon);
        }

        env.storage().persistent().set(&warned_key, &true);
        Self::extend_to_max(&env, &warned_key);

        env.events().publish(
            (Symbol::new(&env, "policy_expiring_soon"),),
            PolicyExpiringSoon {
                policy_id,
                policyholder: policy.policyholder,
                product_id: policy.product_id,
                coverage_amount: policy.coverage_amount,
                end_time: policy.end_time,
                seconds_remaining,
            },
        );
    }

    /// Scan one user's policies and emit `PolicyExpiringSoon` for each that has
    /// entered its warning window and has not been warned yet.
    ///
    /// Returns the number of events emitted. Policies that are ineligible are
    /// skipped rather than aborting the call — a keeper sweeping a user's book
    /// should not lose the whole batch because one policy was already warned.
    ///
    /// Scans at most `MAX_EXPIRY_SCAN` policies per call to bound the
    /// instruction budget of a permissionless entry point.
    pub fn notify_expiring_policies(env: Env, user: Address, offset: u32) -> u32 {
        let ids: Vec<u128> = env
            .storage()
            .persistent()
            .get(&StorageKey::UserPolicies(user))
            .unwrap_or_else(|| Vec::new(&env));

        let now = env.ledger().timestamp();
        let window = Self::expiry_warning_window(&env);
        let mut emitted = 0u32;
        let mut scanned = 0u32;

        let mut i = offset;
        while i < ids.len() && scanned < MAX_EXPIRY_SCAN {
            scanned += 1;
            let policy_id = ids.get_unchecked(i);
            i += 1;

            let policy: Policy = match env
                .storage()
                .persistent()
                .get(&StorageKey::Policy(policy_id))
            {
                Some(p) => p,
                None => continue,
            };

            if policy.status != PolicyStatus::Active || now >= policy.end_time {
                continue;
            }

            let seconds_remaining = policy.end_time - now;
            if seconds_remaining > window {
                continue;
            }

            let warned_key = StorageKey::ExpiryWarned(policy_id);
            if env.storage().persistent().has(&warned_key) {
                continue;
            }

            env.storage().persistent().set(&warned_key, &true);
            Self::extend_to_max(&env, &warned_key);

            env.events().publish(
                (Symbol::new(&env, "policy_expiring_soon"),),
                PolicyExpiringSoon {
                    policy_id,
                    policyholder: policy.policyholder,
                    product_id: policy.product_id,
                    coverage_amount: policy.coverage_amount,
                    end_time: policy.end_time,
                    seconds_remaining,
                },
            );
            emitted += 1;
        }

        emitted
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    /// Return the `InsuranceProduct` for the given ID. Panics if the product does not exist.
    pub fn get_product(env: Env, product_id: u128) -> InsuranceProduct {
        Self::load_product(&env, product_id)
    }

    /// Return the `Policy` for the given ID. Panics if the policy does not exist.
    pub fn get_policy(env: Env, policy_id: u128) -> Policy {
        Self::load_policy(&env, policy_id)
    }

    /// Return a paginated slice of policy IDs owned by `user`. `offset` is the zero-based
    /// start index; `limit` caps the number of IDs returned and is itself clamped to
    /// `MAX_PAGE_SIZE`.
    pub fn get_user_policies(env: Env, user: Address, offset: u32, limit: u32) -> Vec<u128> {
        let all: Vec<u128> = env.storage().persistent()
            .get(&StorageKey::UserPolicies(user))
            .unwrap_or_else(|| Vec::new(&env));

        let limit = limit.min(MAX_PAGE_SIZE);
        let mut paginated = Vec::new(&env);
        let len = all.len();
        if offset >= len {
            return paginated;
        }
        let end = offset.saturating_add(limit).min(len);
        for i in offset..end {
            paginated.push_back(all.get_unchecked(i));
        }
        paginated
    }

    /// Return the IDs of all products whose status is `Active`.
    pub fn get_active_products(env: Env) -> Vec<u128> {
        env.storage().instance()
            .get(&StorageKey::ActiveProducts)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Return the USDC balance held by this contract (7-decimal stroops).
    pub fn get_contract_balance(env: Env) -> i128 {
        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        token::Client::new(&env, &usdc).balance(&env.current_contract_address())
    }

    /// Return the current admin address. Panics with `NotInitialized` if not set up.
    pub fn get_admin(env: Env) -> Address {
        env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    /// Return the ledger timestamp at which the current pending admin transfer
    /// was registered, or `0` if none is pending. `accept_admin` succeeds only
    /// once `now >= this + ADMIN_TRANSFER_TIMELOCK` (issue #356).
    pub fn get_pending_admin_since(env: Env) -> u64 {
        env.storage().instance().get(&StorageKey::PendingAdminSince).unwrap_or(0)
    }

    /// Return the configured oracle verifier contract address.
    pub fn get_oracle(env: Env) -> Address {
        env.storage().instance().get(&StorageKey::OracleAddress)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    /// Return `true` if the contract is currently in emergency-pause mode.
    pub fn is_paused(env: Env) -> bool {
        env.storage().instance().get(&StorageKey::Paused).unwrap_or(false)
    }

    /// Return the current storage schema version (defaults to 1 before any migration).
    pub fn get_version(env: Env) -> u32 {
        env.storage().instance().get(&StorageKey::Version).unwrap_or(1)
    }

    // ── Admin: emergency controls ─────────────────────────────────────────────

    /// Emergency pause — halts buy_policy for all products.
    pub fn emergency_pause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&StorageKey::Paused, &true);
    }

pub fn emergency_resume(env: Env, admin: Address) {
         Self::require_admin(&env, &admin);
         env.storage().instance().set(&StorageKey::Paused, &false);
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
             env.storage().instance().set(&StorageKey::PendingAdmin, &new_admin);
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
             env.storage().instance().set(&StorageKey::PendingAdmin, &new_admin);
             env.storage()
                 .instance()
                 .set(&StorageKey::PendingAdminSince, &env.ledger().timestamp());
         } else {
             env.storage()
                 .instance()
                 .set(&StorageKey::PendingAdminChange, &pending);
         }
     }

     /// Configure the guardian set and approval threshold required for
     /// critical actions (upgrades, admin transfer). Admin-only.
     /// `threshold == 0` disables the guardian requirement (default), so the
     /// admin alone can act — preserves existing single-admin behavior until
     /// guardians are explicitly configured.
     pub fn set_guardians(env: Env, admin: Address, guardians: Vec<Address>, threshold: u32) {
         Self::require_admin(&env, &admin);
         if threshold > guardians.len() {
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
             let current_version: u32 =
                 env.storage().instance().get(&StorageKey::Version).unwrap_or(1);
             env.storage().instance().remove(&StorageKey::PendingUpgrade);
             Self::run_migrations(&env, current_version, pending.new_version);
             env.storage()
                 .instance()
                 .set(&StorageKey::Version, &pending.new_version);
             env.deployer().update_current_contract_wasm(new_wasm_hash);

             env.events().publish(
                 (Symbol::new(&env, "contract_upgraded"),),
                 ContractUpgraded {
                     old_version: current_version,
                     new_version: pending.new_version,
                 },
             );
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

     /// Accept the proposed admin. Only the proposed admin can call this.
     pub fn accept_admin(env: Env, admin: Address) {
         let pending_admin: Address = env.storage().instance()
             .get(&StorageKey::PendingAdmin)
             .unwrap_or_else(|| panic_with_error!(&env, Error::Unauthorized));
         // Only the pending admin can accept
         if pending_admin != admin {
             panic_with_error!(&env, Error::Unauthorized);
         }
         admin.require_auth();
         let _current_admin: Address = env.storage().instance()
             .get(&StorageKey::Admin)
             .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
         // Enforce the admin-rotation timelock (issue #356).
         let since: u64 = env.storage().instance()
             .get(&StorageKey::PendingAdminSince).unwrap_or(0);
         if env.ledger().timestamp() < since.saturating_add(ADMIN_TRANSFER_TIMELOCK) {
             panic_with_error!(&env, Error::AdminTimelockNotExpired);
         }
         // Update admin
         env.storage().instance().set(&StorageKey::Admin, &admin);
         // Clear the proposal
         env.storage().instance().remove(&StorageKey::PendingAdmin);
         env.storage().instance().remove(&StorageKey::PendingAdminSince);
         // Emit event
         env.events().publish(
             (Symbol::new(&env, "admin_updated"),),
             AdminUpdated {
                 new_admin: admin,
             },
         );
     }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Panic with `Unauthorized` if the contract is in emergency-pause mode.
    fn ensure_not_paused(env: &Env) {
        if env.storage().instance().get::<_, bool>(&StorageKey::Paused).unwrap_or(false) {
            panic_with_error!(env, Error::Unauthorized);
        }
    }

    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        if *caller != admin { panic_with_error!(env, Error::Unauthorized); }
        caller.require_auth();
    }

    fn require_claims_processor(env: &Env, caller: &Address) {
        let cp: Address = env.storage().instance().get(&StorageKey::ClaimsProcessor)
            .unwrap_or_else(|| panic_with_error!(env, Error::ClaimsProcessorNotSet));
        if *caller != cp { panic_with_error!(env, Error::Unauthorized); }
        caller.require_auth();
    }

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

    fn remove_policy_from_user(env: &Env, user: &Address, policy_id: u128) {
        let key = StorageKey::UserPolicies(user.clone());
        let mut user_policies: Vec<u128> = env.storage().persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));
        let mut pos: Option<u32> = None;
        for i in 0..user_policies.len() {
            if user_policies.get_unchecked(i) == policy_id {
                pos = Some(i);
                break;
            }
        }
        if let Some(i) = pos {
            user_policies.remove(i);
            env.storage().persistent().set(&key, &user_policies);
            Self::extend_to_max(env, &key);
        }
    }

    /// Extend a persistent entry's TTL to the network maximum. Used for
    /// Product/ProductKey/UserPolicies records, which are admin- or
    /// user-index data with no natural expiry.
    /// The configured expiry warning window, or the default when unset.
    fn expiry_warning_window(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&StorageKey::ExpiryWarningWindow)
            .unwrap_or(DEFAULT_EXPIRY_WARNING_WINDOW)
    }

    fn extend_to_max(env: &Env, key: &StorageKey) {
        let max_ttl = env.storage().max_ttl();
        env.storage().persistent().extend_ttl(key, max_ttl, max_ttl);
    }

    /// Extend a `Policy` entry's TTL to cover its own coverage duration plus
    /// `POLICY_CLAIMS_BUFFER_SECONDS` (clamped to the network's max TTL), so
    /// `get_policy`/`pay_claim`/`expire_policy` can still find it even if it
    /// is only ever written once, at purchase time (issue #186).
    fn extend_policy_ttl(env: &Env, key: &StorageKey, duration_secs: u64) {
        let ttl_seconds = duration_secs.saturating_add(POLICY_CLAIMS_BUFFER_SECONDS);
        let desired_ledgers = (ttl_seconds / LEDGER_SECONDS) as u32;
        let extend_to = desired_ledgers.min(env.storage().max_ttl());
        env.storage().persistent().extend_ttl(key, extend_to, extend_to);
    }

    fn load_product(env: &Env, id: u128) -> InsuranceProduct {
        env.storage().persistent().get(&StorageKey::Product(id))
            .unwrap_or_else(|| panic_with_error!(env, Error::ProductNotFound))
    }

    fn load_policy(env: &Env, id: u128) -> Policy {
        env.storage().persistent().get(&StorageKey::Policy(id))
            .unwrap_or_else(|| panic_with_error!(env, Error::PolicyNotFound))
    }

    /// Atomically fetch-and-increment the product ID counter.
    /// Uses storage().update() to guarantee a single read-modify-write operation,
    /// preventing two concurrent ledger entries from reading the same value.
    fn next_product_id(env: &Env) -> u128 {
        let mut id = 0u128;
        env.storage().instance().update(
            &StorageKey::NextProductId,
            |v: Option<u128>| {
                id = v.unwrap_or(1);
                id + 1
            },
        );
        id
    }

    fn next_policy_id(env: &Env) -> u128 {
        let mut id = 0u128;
        env.storage().instance().update(
            &StorageKey::NextPolicyId,
            |v: Option<u128>| {
                id = v.unwrap_or(1);
                id + 1
            },
        );
        id
    }

    /// Upgrade the contract WASM in-place. Only the admin may call this.
    /// Storage is preserved across upgrades; only the execution code changes.
    /// Runs storage migrations if the new version requires them.
    ///
    /// If a guardian threshold > 0 is configured (`set_guardians`), this call
    /// does not upgrade immediately — it registers the upgrade as pending and
    /// requires `threshold` guardians to call `approve_upgrade` before the
    /// WASM is actually replaced, guarding this irreversible operation
    /// against a single compromised admin key.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>, new_version: u32) {
        Self::require_admin(&env, &admin);
        let current_version: u32 = env.storage().instance().get(&StorageKey::Version).unwrap_or(1);
        if new_version <= current_version {
            panic_with_error!(&env, Error::InvalidVersion);
        }

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::GuardianThreshold)
            .unwrap_or(0);
        if threshold == 0 {
            Self::run_migrations(&env, current_version, new_version);
            env.storage().instance().set(&StorageKey::Version, &new_version);
            env.deployer().update_current_contract_wasm(new_wasm_hash);

            env.events().publish(
                (Symbol::new(&env, "contract_upgraded"),),
                ContractUpgraded {
                    old_version: current_version,
                    new_version,
                },
            );
            return;
        }

        let pending = PendingUpgrade {
            new_wasm_hash,
            new_version,
            approvals: Vec::new(&env),
        };
        env.storage().instance().set(&StorageKey::PendingUpgrade, &pending);
    }

    /// Run storage migrations from old_version to new_version.
    /// Each migration function handles a specific version transition.
    fn run_migrations(_env: &Env, _old_version: u32, _new_version: u32) {
        // Migration from v1 to v2: No storage changes needed yet
        // This is where you would add migration logic for specific version bumps
        // Example: if old_version < 2 && new_version >= 2 { Self::migrate_v1_to_v2(env); }
        
        // Future migrations follow the pattern:
        // if old_version < 3 && new_version >= 3 { Self::migrate_v2_to_v3(env); }
    }
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_advanced;
