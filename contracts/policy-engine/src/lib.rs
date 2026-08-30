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

#[cfg_attr(feature = "library", allow(unused_imports))]
use crate::alloc::string::ToString;
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, token, Address, BytesN,
    Env, Symbol, Vec,
};

pub mod types;
pub use types::*;

// ─── Storage TTL ──────────────────────────────────────────────────────────────

/// Extend a persistent entry's TTL once it has fewer than ~30 days of life left
/// (at ~5s/ledger).
#[cfg(any(test, feature = "testutils", not(feature = "library")))]
const TTL_THRESHOLD: u32 = 518_400;
/// Extend persistent entries out to ~1 year (at ~5s/ledger) so long-lived
/// products and policies don't get evicted from storage before they mature.
#[cfg(any(test, feature = "testutils", not(feature = "library")))]
const TTL_EXTEND_TO: u32 = 6_312_000;
const DEFAULT_MAX_PRODUCTS_PER_POOL: u32 = 100;
const CURRENT_STORAGE_VERSION: u32 = 3;

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
    MaxProductsPerPool,
    PoolProductCount(Symbol),
    /// Contract version (u32) for storage migration tracking
    Version,
}

// ─── Errors ───────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    ProductNotFound = 4,
    ProductNotActive = 5,
    PolicyNotFound = 6,
    PolicyNotActive = 7,
    CoverageOutOfRange = 8,
    DurationTooLong = 9,
    InsufficientPool = 10,
    AlreadyClaimed = 11,
    AlreadyExpired = 12,
    InvalidPremiumRate = 13,
    InvalidTriggerThreshold = 14,
    DuplicateProductKey = 15,
    InvalidCoverageRange = 16,
    InvalidToken = 17,
    ClaimsProcessorNotSet = 18,
    InvalidDurationRange = 19,
    InvalidOracleKey = 20,
    Overflow = 21,
    TooManyProducts = 22,
    InvalidMaxProducts = 23,
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
    pub fn initialize(env: Env, admin: Address, usdc_token: Address, oracle_address: Address) {
        if env.storage().instance().has(&StorageKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        // require_auth() validates all addresses at the protocol level, so we
        // do not need manual address format validation here.
        let admin_str = admin.to_string();
        //
        if false {
            panic!("invalid address: admin must be an account address");
        }
        if admin_str.len() != 56 {
            panic!("invalid address: admin must be an account or contract address");
        }
        let mut admin_buf = [0u8; 56];
        admin_str.copy_into_slice(&mut admin_buf);
        if admin_buf[0] != b'G' && admin_buf[0] != b'C' {
            panic!("invalid address: admin must be an account or contract address");
        }

        let usdc_str = usdc_token.to_string();
        let oracle_str = oracle_address.to_string();
        //
        //
        if false {
            panic!("invalid address: usdc_token must be a contract address");
        }
        if usdc_str.len() != 56 {
            panic!("invalid address: usdc_token must be a contract address");
        }
        let mut usdc_buf = [0u8; 56];
        usdc_str.copy_into_slice(&mut usdc_buf);
        if usdc_buf[0] != b'C' {
            panic!("invalid address: usdc_token must be a contract address");
        }

        let oracle_str = oracle_address.to_string();
        if oracle_str.len() != 56 {
            panic!("invalid address: oracle_address must be a contract address");
        }
        let mut oracle_buf = [0u8; 56];
        oracle_str.copy_into_slice(&mut oracle_buf);
        if oracle_buf[0] != b'C' {
            panic!("invalid address: oracle_address must be a contract address");
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
        env.storage()
            .instance()
            .set(&StorageKey::Initialized, &true);
        env.storage().instance().set(&StorageKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&StorageKey::UsdcToken, &usdc_token);
        env.storage()
            .instance()
            .set(&StorageKey::OracleAddress, &oracle_address);
        env.storage()
            .instance()
            .set(&StorageKey::NextProductId, &1u128);
        env.storage()
            .instance()
            .set(&StorageKey::NextPolicyId, &1u128);
        env.storage()
            .instance()
            .set(&StorageKey::ActiveProducts, &Vec::<u128>::new(&env));
        env.storage().instance().set(
            &StorageKey::MaxProductsPerPool,
            &DEFAULT_MAX_PRODUCTS_PER_POOL,
        );
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
        env.storage()
            .instance()
            .set(&StorageKey::ClaimsProcessor, &claims_processor);
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
        if env
            .storage()
            .persistent()
            .has(&StorageKey::ProductKey(key.clone()))
        {
            panic_with_error!(&env, Error::DuplicateProductKey);
        }

        let pool_count_key = StorageKey::PoolProductCount(params.category.clone());
        let pool_count: u32 = env.storage().persistent().get(&pool_count_key).unwrap_or(0);
        let max_products: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::MaxProductsPerPool)
            .unwrap_or(DEFAULT_MAX_PRODUCTS_PER_POOL);
        if pool_count >= max_products {
            panic_with_error!(&env, Error::TooManyProducts);
        }

        let id = Self::next_product_id(&env);
        let product = InsuranceProduct {
            id,
            name: params.name,
            category: params.category,
            oracle_key: params.oracle_key,
            trigger_type: params.trigger_type,
            oracle_data_type: params.oracle_data_type,
            trigger_threshold: params.trigger_threshold,
            trigger_comparison: params.trigger_comparison,
            coverage_min: params.coverage_min,
            coverage_max: params.coverage_max,
            premium_rate_bps: params.premium_rate_bps,
            max_duration_days: params.max_duration_days,
            status: ProductStatus::Active,
            created_at: env.ledger().timestamp(),
        };
        env.storage()
            .persistent()
            .set(&StorageKey::Product(id), &product);
        env.storage().persistent().extend_ttl(
            &StorageKey::Product(id),
            TTL_THRESHOLD,
            TTL_EXTEND_TO,
        );

        // Store the (category, oracle_key) -> product_id mapping for uniqueness
        env.storage()
            .persistent()
            .set(&StorageKey::ProductKey(key.clone()), &id);
        env.storage().persistent().extend_ttl(
            &StorageKey::ProductKey(key),
            TTL_THRESHOLD,
            TTL_EXTEND_TO,
        );

        let mut products: Vec<u128> = env
            .storage()
            .instance()
            .get(&StorageKey::ActiveProducts)
            .unwrap_or_else(|| Vec::new(&env));
        products.push_back(id);
        env.storage()
            .instance()
            .set(&StorageKey::ActiveProducts, &products);
        env.storage()
            .persistent()
            .set(&pool_count_key, &(pool_count + 1));
        env.storage()
            .persistent()
            .extend_ttl(&pool_count_key, TTL_THRESHOLD, TTL_EXTEND_TO);

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
        env.storage()
            .persistent()
            .set(&StorageKey::Product(product_id), &product);

        let mut products: Vec<u128> = env
            .storage()
            .instance()
            .get(&StorageKey::ActiveProducts)
            .unwrap_or_else(|| Vec::new(&env));
        let mut idx: Option<u32> = None;
        for i in 0..products.len() {
            if products.get_unchecked(i) == product_id {
                idx = Some(i);
                break;
            }
        }
        if let Some(i) = idx {
            products.remove(i);
            env.storage()
                .instance()
                .set(&StorageKey::ActiveProducts, &products);
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
        env.storage()
            .persistent()
            .set(&StorageKey::Product(product_id), &product);

        // Remove from the ActiveProducts list on deprecation
        let mut products: Vec<u128> = env
            .storage()
            .instance()
            .get(&StorageKey::ActiveProducts)
            .unwrap_or_else(|| Vec::new(&env));
        let mut idx: Option<u32> = None;
        for i in 0..products.len() {
            if products.get_unchecked(i) == product_id {
                idx = Some(i);
                break;
            }
        }
        if let Some(i) = idx {
            products.remove(i);
            env.storage()
                .instance()
                .set(&StorageKey::ActiveProducts, &products);
        }

        // Remove the (category, oracle_key) mapping to allow reuse of the key
        let pool_count_key = StorageKey::PoolProductCount(product.category.clone());
        let pool_count: u32 = env.storage().persistent().get(&pool_count_key).unwrap_or(0);
        if pool_count > 0 {
            env.storage()
                .persistent()
                .set(&pool_count_key, &(pool_count - 1));
            env.storage()
                .persistent()
                .extend_ttl(&pool_count_key, TTL_THRESHOLD, TTL_EXTEND_TO);
        }

        let key = (product.category, product.oracle_key);
        env.storage()
            .persistent()
            .remove(&StorageKey::ProductKey(key));
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
        if env
            .storage()
            .instance()
            .get::<_, bool>(&StorageKey::Paused)
            .unwrap_or(false)
        {
            panic_with_error!(&env, Error::Unauthorized);
        }
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
        let usdc: Address = env
            .storage()
            .instance()
            .get(&StorageKey::UsdcToken)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        // Pull premium from buyer into this contract
        token::Client::new(&env, &usdc).transfer(&buyer, &env.current_contract_address(), &premium);

        let now = env.ledger().timestamp();
        let duration_secs = (duration_days as u64)
            .checked_mul(86_400)
            .unwrap_or_else(|| panic_with_error!(&env, Error::CoverageOutOfRange));
        let end_time = now
            .checked_add(duration_secs)
            .unwrap_or_else(|| panic_with_error!(&env, Error::CoverageOutOfRange));
        let policy_id = Self::next_policy_id(&env);

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
        env.storage()
            .persistent()
            .set(&StorageKey::Policy(policy_id), &policy);
        env.storage().persistent().extend_ttl(
            &StorageKey::Policy(policy_id),
            TTL_THRESHOLD,
            TTL_EXTEND_TO,
        );

        // Append to user's policy list
        let user_key = StorageKey::UserPolicies(buyer.clone());
        let mut user_policies: Vec<u128> = env
            .storage()
            .persistent()
            .get(&user_key)
            .unwrap_or_else(|| Vec::new(&env));
        user_policies.push_back(policy_id);
        env.storage().persistent().set(&user_key, &user_policies);
        env.storage()
            .persistent()
            .extend_ttl(&user_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        env.events().publish(
            (Symbol::new(&env, "buy_policy"), buyer),
            (policy_id, product_id, coverage_amount, premium),
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
        env.storage()
            .persistent()
            .set(&StorageKey::Policy(policy_id), &policy);
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
            let earned = policy
                .premium_paid
                .checked_mul(elapsed_capped as i128)
                .and_then(|v| v.checked_div(total_duration as i128))
                .unwrap_or_else(|| panic_with_error!(&env, Error::Overflow));
            policy.premium_paid.saturating_sub(earned)
        };

        let usdc: Address = env
            .storage()
            .instance()
            .get(&StorageKey::UsdcToken)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        if refund > 0 {
            token::Client::new(&env, &usdc).transfer(
                &env.current_contract_address(),
                &policyholder,
                &refund,
            );
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

    // ── Status updates (called by Claims Processor) ──────────────────────────

    /// Mark a policy as Claimed and pay coverage to the policyholder.
    /// Only callable by the registered Claims Processor.
    pub fn pay_claim(env: Env, caller: Address, policy_id: u128) {
        Self::require_claims_processor(&env, &caller);
        let mut policy: Policy = Self::load_policy(&env, policy_id);
        match policy.status {
            PolicyStatus::Claimed => panic_with_error!(&env, Error::AlreadyClaimed),
            PolicyStatus::Expired => panic_with_error!(&env, Error::AlreadyExpired),
            PolicyStatus::Cancelled => panic_with_error!(&env, Error::PolicyNotActive),
            PolicyStatus::Active => {}
        }
        let usdc: Address = env
            .storage()
            .instance()
            .get(&StorageKey::UsdcToken)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let token_client = token::Client::new(&env, &usdc);
        match token_client.try_transfer(
            &env.current_contract_address(),
            &policy.policyholder,
            &policy.coverage_amount,
        ) {
            Ok(Ok(())) => {}
            _ => {
                panic_with_error!(&env, Error::InsufficientPool);
            }
        }

        policy.status = PolicyStatus::Claimed;
        env.storage()
            .persistent()
            .set(&StorageKey::Policy(policy_id), &policy);
        Self::remove_policy_from_user(&env, &policy.policyholder, policy_id);

        env.events().publish(
            (Symbol::new(&env, "claim_paid"), policy_id),
            (policy.policyholder.clone(), policy.coverage_amount),
        );
    }

    /// Mark a policy as Expired (trigger not met before end_time).
    /// Premium remains in the contract as earned pool revenue.
    pub fn expire_policy(env: Env, caller: Address, policy_id: u128) {
        Self::require_claims_processor(&env, &caller);
        let mut policy: Policy = Self::load_policy(&env, policy_id);
        match policy.status {
            PolicyStatus::Claimed => panic_with_error!(&env, Error::AlreadyClaimed),
            PolicyStatus::Expired => panic_with_error!(&env, Error::AlreadyExpired),
            PolicyStatus::Cancelled => panic_with_error!(&env, Error::PolicyNotActive),
            PolicyStatus::Active => {}
        }
        policy.status = PolicyStatus::Expired;
        env.storage()
            .persistent()
            .set(&StorageKey::Policy(policy_id), &policy);
        Self::remove_policy_from_user(&env, &policy.policyholder, policy_id);
        env.events().publish(
            (Symbol::new(&env, "policy_expired"),),
            PolicyExpired { policy_id },
        );
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
    /// start index; `limit` caps the number of IDs returned.
    pub fn get_user_policies(env: Env, user: Address, offset: u32, limit: u32) -> Vec<u128> {
        let all: Vec<u128> = env
            .storage()
            .persistent()
            .get(&StorageKey::UserPolicies(user))
            .unwrap_or_else(|| Vec::new(&env));

        let mut paginated = Vec::new(&env);
        let len = all.len();
        if offset >= len {
            return paginated;
        }
        let end = (offset + limit).min(len);
        for i in offset..end {
            paginated.push_back(all.get_unchecked(i));
        }
        paginated
    }

    /// Return the IDs of all products whose status is `Active`.
    pub fn get_active_products(env: Env) -> Vec<u128> {
        env.storage()
            .instance()
            .get(&StorageKey::ActiveProducts)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Return the USDC balance held by this contract (7-decimal stroops).
    pub fn get_contract_balance(env: Env) -> i128 {
        let usdc: Address = env
            .storage()
            .instance()
            .get(&StorageKey::UsdcToken)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        token::Client::new(&env, &usdc).balance(&env.current_contract_address())
    }

    /// Return the current admin address. Panics with `NotInitialized` if not set up.
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    /// Return the configured oracle verifier contract address.
    pub fn get_oracle(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&StorageKey::OracleAddress)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    /// Return `true` if the contract is currently in emergency-pause mode.
    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&StorageKey::Paused)
            .unwrap_or(false)
    }

    /// Return the current storage schema version (defaults to 1 before any migration).
    pub fn get_version(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::Version)
            .unwrap_or(1)
    }

    /// Return the configured maximum number of non-deprecated products per risk pool/category.
    pub fn get_max_products_per_pool(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::MaxProductsPerPool)
            .unwrap_or(DEFAULT_MAX_PRODUCTS_PER_POOL)
    }

    /// Return the current non-deprecated product count for a pool/category.
    pub fn get_pool_product_count(env: Env, category: Symbol) -> u32 {
        env.storage()
            .persistent()
            .get(&StorageKey::PoolProductCount(category))
            .unwrap_or(0)
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
    pub fn propose_new_admin(env: Env, admin: Address, new_admin: Address) {
        Self::require_admin(&env, &admin);
        // Store the proposed admin
        env.storage()
            .instance()
            .set(&StorageKey::PendingAdmin, &new_admin);
    }

    /// Accept the proposed admin. Only the proposed admin can call this.
    pub fn accept_admin(env: Env, admin: Address) {
        let pending_admin: Address = env
            .storage()
            .instance()
            .get(&StorageKey::PendingAdmin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::Unauthorized));
        // Only the pending admin can accept
        if pending_admin != admin {
            panic_with_error!(&env, Error::Unauthorized);
        }
        admin.require_auth();
        let _current_admin: Address = env
            .storage()
            .instance()
            .get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        // Update admin
        env.storage().instance().set(&StorageKey::Admin, &admin);
        // Clear the proposal
        env.storage().instance().remove(&StorageKey::PendingAdmin);
        // Emit event
        env.events().publish(
            (Symbol::new(&env, "admin_updated"),),
            AdminUpdated { new_admin: admin },
        );
    }

    /// Admin-only: configure the maximum non-deprecated products per pool/category.
    pub fn set_max_products_per_pool(env: Env, admin: Address, max_products: u32) {
        Self::require_admin(&env, &admin);
        if max_products == 0 {
            panic_with_error!(&env, Error::InvalidMaxProducts);
        }
        env.storage()
            .instance()
            .set(&StorageKey::MaxProductsPerPool, &max_products);
        env.events()
            .publish((Symbol::new(&env, "max_products_updated"),), max_products);
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

    fn require_claims_processor(env: &Env, caller: &Address) {
        let cp: Address = env
            .storage()
            .instance()
            .get(&StorageKey::ClaimsProcessor)
            .unwrap_or_else(|| panic_with_error!(env, Error::ClaimsProcessorNotSet));
        if *caller != cp {
            panic_with_error!(env, Error::Unauthorized);
        }
        caller.require_auth();
    }

    fn remove_policy_from_user(env: &Env, user: &Address, policy_id: u128) {
        let key = StorageKey::UserPolicies(user.clone());
        let mut user_policies: Vec<u128> = env
            .storage()
            .persistent()
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
        }
    }

    fn load_product(env: &Env, id: u128) -> InsuranceProduct {
        env.storage()
            .persistent()
            .get(&StorageKey::Product(id))
            .unwrap_or_else(|| panic_with_error!(env, Error::ProductNotFound))
    }

    fn load_policy(env: &Env, id: u128) -> Policy {
        env.storage()
            .persistent()
            .get(&StorageKey::Policy(id))
            .unwrap_or_else(|| panic_with_error!(env, Error::PolicyNotFound))
    }

    /// Atomically fetch-and-increment the product ID counter.
    /// Uses storage().update() to guarantee a single read-modify-write operation,
    /// preventing two concurrent ledger entries from reading the same value.
    fn next_product_id(env: &Env) -> u128 {
        let mut id = 0u128;
        env.storage()
            .instance()
            .update(&StorageKey::NextProductId, |v: Option<u128>| {
                id = v.unwrap_or(1);
                id + 1
            });
        id
    }

    fn next_policy_id(env: &Env) -> u128 {
        let mut id = 0u128;
        env.storage()
            .instance()
            .update(&StorageKey::NextPolicyId, |v: Option<u128>| {
                id = v.unwrap_or(1);
                id + 1
            });
        id
    }

    /// Upgrade the contract WASM in-place. Only the admin may call this.
    /// Storage is preserved across upgrades; only the execution code changes.
    /// Runs storage migrations if the new version requires them.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>, new_version: u32) {
        Self::require_admin(&env, &admin);
        let current_version: u32 = env
            .storage()
            .instance()
            .get(&StorageKey::Version)
            .unwrap_or(1);
        if new_version <= current_version {
            panic!("new version must be greater than current version");
        }

        // Run migrations from current_version to new_version
        Self::run_migrations(&env, current_version, new_version);

        // Update the stored version
        env.storage()
            .instance()
            .set(&StorageKey::Version, &new_version);

        // Perform the actual WASM upgrade
        env.deployer().update_current_contract_wasm(new_wasm_hash);

        env.events().publish(
            (Symbol::new(&env, "contract_upgraded"),),
            ContractUpgraded {
                old_version: current_version,
                new_version,
            },
        );
    }

    /// Run storage migrations from old_version to new_version.
    /// Each migration function handles a specific version transition.
    fn run_migrations(env: &Env, old_version: u32, new_version: u32) {
        if old_version == 0 || new_version <= old_version || new_version > CURRENT_STORAGE_VERSION {
            panic!("invalid migration version");
        }

        let mut version = old_version;
        while version < new_version {
            match version {
                1 => {
                    Self::migrate_v1_to_v2(env);
                    version = 2;
                }
                2 => {
                    Self::migrate_v2_to_v3(env);
                    version = 3;
                }
                _ => panic!("unsupported migration path"),
            }
        }
    }

    fn migrate_v1_to_v2(env: &Env) {
        if !env
            .storage()
            .instance()
            .has(&StorageKey::MaxProductsPerPool)
        {
            env.storage().instance().set(
                &StorageKey::MaxProductsPerPool,
                &DEFAULT_MAX_PRODUCTS_PER_POOL,
            );
        }
    }

    fn migrate_v2_to_v3(_env: &Env) {}
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_advanced;
