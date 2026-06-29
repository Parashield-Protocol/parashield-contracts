//! Parashield Policy Engine
//!
//! Manages insurance products and policies.
//!
//! Flow
//! ─────
//! 1. Admin calls `create_product` to define a new insurance product.
//! 2. User calls `buy_policy` — transfers premium to this contract,
//!    and the contract locks coverage USDC from its pool balance.
//! 3. The Claims Processor calls `mark_claimed` / `mark_expired` to update
//!    policy status after processing. It also performs the USDC transfer.
//!
//! Architecture note on Claimable Balances
//! ─────────────────────────────────────────
//! Stellar's ClaimPredicate cannot encode data-driven conditions
//! (e.g., "rainfall < 50mm"). Therefore this contract acts as the
//! escrow: it holds USDC and the Claims Processor calls `token.transfer`
//! to pay the policyholder when the oracle confirms a trigger.
#![no_std]

#[cfg_attr(feature = "library", allow(unused_imports))]
use soroban_sdk::{
    contract, contractimpl, contracttype, contracterror, panic_with_error,
    token, Address, Env, Symbol, Vec,
};

pub mod types;
pub use types::*;

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
}

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
}

// ─── Contract ─────────────────────────────────────────────────────────────────

#[cfg(any(test, feature = "testutils", not(feature = "library")))]
#[contract]
pub struct PolicyEngine;

#[cfg(any(test, feature = "testutils", not(feature = "library")))]
#[contractimpl]
impl PolicyEngine {

    // ── Lifecycle ────────────────────────────────────────────────────────────

    pub fn initialize(
        env: Env,
        admin: Address,
        usdc_token: Address,
        oracle_address: Address,
    ) {
        if env.storage().instance().has(&StorageKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        let admin_str = admin.to_string();
        let admin_prefix = admin_str.to_string();
        if !admin_prefix.starts_with('G') {
            panic!("invalid address: admin must be an account address");
        }
        let usdc_str = usdc_token.to_string();
        let oracle_str = oracle_address.to_string();
        let usdc_prefix = usdc_str.to_string();
        let oracle_prefix = oracle_str.to_string();
        if !usdc_prefix.starts_with('C') {
            panic!("invalid address: usdc_token must be a contract address");
        }
        if !oracle_prefix.starts_with('C') {
            panic!("invalid address: oracle_address must be a contract address");
        }
        admin.require_auth();
        env.storage().instance().set(&StorageKey::Initialized, &true);
        env.storage().instance().set(&StorageKey::Admin, &admin);
        env.storage().instance().set(&StorageKey::UsdcToken, &usdc_token);
        env.storage().instance().set(&StorageKey::OracleAddress, &oracle_address);
        env.storage().instance().set(&StorageKey::NextProductId, &1u128);
        env.storage().instance().set(&StorageKey::NextPolicyId,  &1u128);
        env.storage().instance().set(&StorageKey::ActiveProducts, &Vec::<u128>::new(&env));

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
        env.storage().instance().set(&StorageKey::ClaimsProcessor, &claims_processor);
        env.events().publish(
            (Symbol::new(&env, "claims_processor_updated"),),
            ClaimsProcessorUpdated {
                claims_processor: claims_processor.clone(),
            },
        );
    }

    // ── Product Management (admin only) ──────────────────────────────────────

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

        // Store the (category, oracle_key) -> product_id mapping for uniqueness
        env.storage().persistent().set(&StorageKey::ProductKey(key), &id);

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

    pub fn pause_product(env: Env, admin: Address, product_id: u128) {
        Self::require_admin(&env, &admin);
        let mut product: InsuranceProduct = Self::load_product(&env, product_id);
        product.status = ProductStatus::Paused;
        env.storage().persistent().set(&StorageKey::Product(product_id), &product);
        env.events().publish(
            (Symbol::new(&env, "product_paused"),),
            ProductPaused { product_id },
        );
    }

    pub fn deprecate_product(env: Env, admin: Address, product_id: u128) {
        Self::require_admin(&env, &admin);
        let mut product: InsuranceProduct = Self::load_product(&env, product_id);
        product.status = ProductStatus::Deprecated;
        env.storage().persistent().set(&StorageKey::Product(product_id), &product);

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
        if env.storage().instance().get::<_, bool>(&StorageKey::Paused).unwrap_or(false) {
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

        let premium = coverage_amount * product.premium_rate_bps as i128 / 10_000;
        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();

        // Pull premium from buyer into this contract
        token::Client::new(&env, &usdc)
            .transfer(&buyer, &env.current_contract_address(), &premium);

        let now        = env.ledger().timestamp();
        let end_time   = now + (duration_days as u64) * 86_400;
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

        // Append to user's policy list
        let user_key = StorageKey::UserPolicies(buyer.clone());
        let mut user_policies: Vec<u128> = env.storage().persistent()
            .get(&user_key).unwrap_or_else(|| Vec::new(&env));
        user_policies.push_back(policy_id);
        env.storage().persistent().set(&user_key, &user_policies);

        env.events().publish(
            (Symbol::new(&env, "policy_created"),),
            PolicyCreated {
                policy_id,
                product_id,
                policyholder: buyer,
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

        // Pro-rate the refund: only return the unearned portion of the premium.
        // Earned = premium_paid * elapsed / total_duration; refund = premium_paid - earned.
        let now = env.ledger().timestamp();
        let elapsed = now.saturating_sub(policy.start_time);
        let total_duration = policy.end_time.saturating_sub(policy.start_time);
        let refund = if total_duration == 0 {
            policy.premium_paid
        } else {
            let elapsed_capped = elapsed.min(total_duration);
            let earned = policy.premium_paid * elapsed_capped as i128 / total_duration as i128;
            policy.premium_paid.saturating_sub(earned)
        };

        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
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
        policy.status = PolicyStatus::Claimed;
        env.storage().persistent().set(&StorageKey::Policy(policy_id), &policy);

        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&env.current_contract_address(), &policy.policyholder, &policy.coverage_amount);

        env.events().publish(
            (Symbol::new(&env, "policy_claimed"),),
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
        env.events().publish(
            (Symbol::new(&env, "policy_expired"),),
            PolicyExpired { policy_id },
        );
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    pub fn get_product(env: Env, product_id: u128) -> InsuranceProduct {
        Self::load_product(&env, product_id)
    }

    pub fn get_policy(env: Env, policy_id: u128) -> Policy {
        Self::load_policy(&env, policy_id)
    }

    pub fn get_user_policies(env: Env, user: Address, offset: u32, limit: u32) -> Vec<u128> {
        let all: Vec<u128> = env.storage().persistent()
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

    pub fn get_active_products(env: Env) -> Vec<u128> {
        env.storage().instance()
            .get(&StorageKey::ActiveProducts)
            .unwrap_or_else(|| Vec::new(&env))
    }

    pub fn get_contract_balance(env: Env) -> i128 {
        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        token::Client::new(&env, &usdc).balance(&env.current_contract_address())
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    pub fn get_oracle(env: Env) -> Address {
        env.storage().instance().get(&StorageKey::OracleAddress)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage().instance().get(&StorageKey::Paused).unwrap_or(false)
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

    // ── Internal helpers ─────────────────────────────────────────────────────

    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        if *caller != admin { panic_with_error!(env, Error::Unauthorized); }
        caller.require_auth();
    }

    fn require_claims_processor(env: &Env, caller: &Address) {
        let cp: Address = env.storage().instance().get(&StorageKey::ClaimsProcessor)
            .unwrap_or_else(|| panic_with_error!(env, Error::Unauthorized));
        if *caller != cp { panic_with_error!(env, Error::Unauthorized); }
        caller.require_auth();
    }

    fn load_product(env: &Env, id: u128) -> InsuranceProduct {
        env.storage().persistent().get(&StorageKey::Product(id))
            .unwrap_or_else(|| panic_with_error!(env, Error::ProductNotFound))
    }

    fn load_policy(env: &Env, id: u128) -> Policy {
        env.storage().persistent().get(&StorageKey::Policy(id))
            .unwrap_or_else(|| panic_with_error!(env, Error::PolicyNotFound))
    }

    fn next_product_id(env: &Env) -> u128 {
        let id: u128 = env.storage().instance()
            .get(&StorageKey::NextProductId).unwrap_or(1);
        env.storage().instance().set(&StorageKey::NextProductId, &(id + 1));
        id
    }

    fn next_policy_id(env: &Env) -> u128 {
        let id: u128 = env.storage().instance()
            .get(&StorageKey::NextPolicyId).unwrap_or(1);
        env.storage().instance().set(&StorageKey::NextPolicyId, &(id + 1));
        id
    }
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_advanced;
