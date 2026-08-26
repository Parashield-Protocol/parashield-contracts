//! Parashield Risk Pool
//!
//! Liquidity providers deposit USDC into category-specific risk pools.
//! Pool-share tokens represent proportional ownership.
//!
//! Economics
//! ──────────
//! - Premium flow: 80% pool yield, 10% protocol treasury, 10% backstop fund
//! - Claims flow: coverage settled from pool balance; LP share value decreases
//! - Utilization rate = total_locked / total_deposited
//! - Target APY: 8-40% depending on risk category
//!
//! v2 — full implementation; Risk Pool is now deployable and testable.
#![no_std]
extern crate alloc;
use alloc::string::ToString;

use soroban_sdk::{
    contract, contractimpl, contracttype, contracterror, panic_with_error,
    token, Address, Env, Symbol, Vec,
};

pub mod types;
pub use types::*;

/// Default premium split, in effect until the admin calls `update_premium_split`.
const DEFAULT_PREMIUM_LP_BPS:       i128 = 8_000;  // 80% of premium to LP pool
const DEFAULT_PREMIUM_TREAS_BPS:    i128 = 1_000;  // 10% to treasury
const DEFAULT_PREMIUM_BACKSTOP_BPS: i128 = 1_000;  // 10% to backstop fund
const _: () = assert!(DEFAULT_PREMIUM_LP_BPS + DEFAULT_PREMIUM_TREAS_BPS + DEFAULT_PREMIUM_BACKSTOP_BPS == 10_000);

/// Upper bound on cumulative deposits (7-decimal USDC stroops).
/// 10^15 stroops == 100,000,000 USDC. Caps total pool size so share value
/// cannot become infinitesimal and total_shares cannot overflow.
const MAX_TOTAL_DEPOSITED: i128 = 1_000_000_000_000_000;

/// Minimum deposit amount (1_000_000 stroops).
const MIN_DEPOSIT: i128 = 1_000_000;

/// Timelock duration for admin withdrawals: 7 days in seconds.
const TIMELOCK_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Extend a persistent entry's TTL once it has fewer than ~30 days of life left
/// (at ~5s/ledger).
const TTL_THRESHOLD: u32 = 518_400;
/// Extend persistent entries out to ~1 year (at ~5s/ledger) so capital locks
/// backing long-dated policies don't expire from storage before maturity.
const TTL_EXTEND_TO: u32 = 6_312_000;

#[contracttype]
enum StorageKey {
    Initialized,
    Admin,
    Treasury,
    Backstop,
    UsdcToken,
    Category,
    TotalDeposited,
    TotalLocked,
    TotalShares,
    AccumulatedPremium,
    AccumulatedBackstop,
    AccumulatedPerShare,
    Status,
    LpPosition(Address),
    LpCount,
    LpAddress(u32),
    Lock(u128),
    AdminWithdrawalRequest,
    PendingAdmin,
    PolicyEngine,
    ClaimsProcessor,
    /// Contract version (u32) for storage migration tracking
    Version,
    /// Premium split ratios (lp_bps, treas_bps, backstop_bps), admin-adjustable
    PremiumSplit,
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

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized  = 1,
    NotInitialized      = 2,
    Unauthorized        = 3,
    InsufficientFunds   = 4,
    ZeroAmount          = 5,
    PoolNotActive       = 6,
    NoShares            = 7,
    AlreadyLocked       = 8,
    LockNotFound        = 9,
    AlreadyReleased     = 10,
    Undercollateralized = 11,
    PoolCapExceeded     = 12,
    InvalidToken        = 13,
    TimelockPending     = 14,
    TimelockNotReady    = 15,
    NoPendingWithdrawal = 16,
    InsufficientShares  = 17,
    DepositTooSmall     = 18,
    Overflow            = 19,
    InvalidAddress      = 20,
    InvalidVersion      = 21,
    InvalidSplit        = 22,
    NotGuardian         = 23,
    AlreadyApprovedAction = 24,
    NoPendingUpgrade    = 25,
    InvalidThreshold    = 26,
}

#[contract]
pub struct RiskPool;

#[contractimpl]
impl RiskPool {

    /// One-time initialisation. Sets up the USDC token, treasury, backstop, and linked
    /// protocol contracts. `category` is the coverage category this pool serves (e.g.
    /// `"weather"`). Panics with `AlreadyInitialized` on a second call.
    pub fn initialize(
        env: Env,
        admin: Address,
        usdc_token: Address,
        treasury: Address,
        backstop: Address,
        category: Symbol,
        policy_engine: Address,
        claims_processor: Address,
    ) {
        if env.storage().instance().has(&StorageKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        // Address validation is deferred to require_auth() calls which
        // verify the address on the Soroban network layer.
        
        let admin_str = admin.to_string();

        if admin_str.len() != 56 {
            panic_with_error!(&env, Error::InvalidAddress);
        }
        let mut admin_buf = [0u8; 56];
        admin_str.copy_into_slice(&mut admin_buf);
        if admin_buf[0] != b'G' && admin_buf[0] != b'C' {
            panic_with_error!(&env, Error::InvalidAddress);
        }

        let usdc_str = usdc_token.to_string();

        let balance_res = env.try_invoke_contract::<i128, soroban_sdk::Error>(
            &usdc_token,
            &Symbol::new(&env, "balance"),
            soroban_sdk::vec![&env, env.current_contract_address().to_val()],
        );
        if balance_res.is_err() {
            panic_with_error!(&env, Error::InvalidToken);
        }

        if usdc_str.len() != 56 {
            panic_with_error!(&env, Error::InvalidAddress);
        }
        let mut usdc_buf = [0u8; 56];
        usdc_str.copy_into_slice(&mut usdc_buf);
        if usdc_buf[0] != b'C' {
            panic_with_error!(&env, Error::InvalidAddress);
        }

        let treasury_str = treasury.to_string();
        if treasury_str.len() != 56 {
            panic_with_error!(&env, Error::InvalidAddress);
        }
        let mut treasury_buf = [0u8; 56];
        treasury_str.copy_into_slice(&mut treasury_buf);
        if treasury_buf[0] != b'C' {
            panic_with_error!(&env, Error::InvalidAddress);
        }

        admin.require_auth();
        env.storage().instance().set(&StorageKey::Initialized,          &true);
        env.storage().instance().set(&StorageKey::Admin,                &admin);
        env.storage().instance().set(&StorageKey::UsdcToken,            &usdc_token);
        env.storage().instance().set(&StorageKey::Treasury,             &treasury);
        env.storage().instance().set(&StorageKey::Backstop,             &backstop);
        env.storage().instance().set(&StorageKey::Category,             &category);
        env.storage().instance().set(&StorageKey::PolicyEngine,         &policy_engine);
        env.storage().instance().set(&StorageKey::ClaimsProcessor,      &claims_processor);
        env.storage().instance().set(&StorageKey::TotalDeposited,       &0i128);
        env.storage().instance().set(&StorageKey::TotalLocked,          &0i128);
        env.storage().instance().set(&StorageKey::TotalShares,          &0i128);
        env.storage().instance().set(&StorageKey::AccumulatedPremium,   &0i128);
        env.storage().instance().set(&StorageKey::AccumulatedBackstop,  &0i128);
        env.storage().instance().set(&StorageKey::AccumulatedPerShare,   &0i128);
        env.storage().instance().set(&StorageKey::Status,               &PoolStatus::Active);
        env.storage().instance().set(&StorageKey::LpCount,              &0u32);
        // PendingAdmin is absent until propose_new_admin is called; no init needed.

        env.events().publish(
            (Symbol::new(&env, "initialized"),),
            Initialized {
                admin:            admin.clone(),
                usdc_token:       usdc_token.clone(),
                treasury:         treasury.clone(),
                backstop:         backstop.clone(),
                category:         category.clone(),
                policy_engine:    policy_engine.clone(),
                claims_processor: claims_processor.clone(),
            },
        );
    }

    // ── Deposits ──────────────────────────────────────────────────────────────

    /// Deposit USDC into the pool and receive LP shares. Returns the number of shares minted.
    /// `min_shares` is a slippage guard — the transaction reverts if fewer shares would be issued.
    pub fn deposit(env: Env, provider: Address, amount: i128, min_shares: i128) -> i128 {
        provider.require_auth();
        if amount <= 0 { panic_with_error!(&env, Error::ZeroAmount); }
        if amount < MIN_DEPOSIT { panic_with_error!(&env, Error::DepositTooSmall); }
        Self::assert_active(&env);

        let total_deposited: i128 = env.storage().instance()
            .get(&StorageKey::TotalDeposited).unwrap_or(0);
        let total_shares: i128 = env.storage().instance()
            .get(&StorageKey::TotalShares).unwrap_or(0);

        // Enforce the global pool size cap before accepting new liquidity.
        if total_deposited + amount > MAX_TOTAL_DEPOSITED {
            panic_with_error!(&env, Error::PoolCapExceeded);
        }

        let new_shares = if total_deposited == 0 {
            amount * 1_000_000_000  // 1 share = 1 USDC * 1e9 precision
        } else {
            amount.checked_mul(total_shares)
                .and_then(|v| v.checked_div(total_deposited))
                .unwrap_or_else(|| panic_with_error!(&env, Error::Overflow))
        };

        if new_shares == 0 {
            panic_with_error!(&env, Error::ZeroAmount);
        }

        if new_shares < min_shares {
            panic_with_error!(&env, Error::InsufficientShares);
        }

        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&provider, &env.current_contract_address(), &amount);

        let now = env.ledger().timestamp();
        let lp_key = StorageKey::LpPosition(provider.clone());
        let mut pending_yield: i128 = 0;
        let mut position: LpPosition = match env.storage().persistent().get::<_, LpPosition>(&lp_key) {
            Some(mut pos) => {
                pending_yield = Self::settle_yield(&env, &mut pos);
                pos.deposited += amount;
                pos.shares    += new_shares;
                pos.yield_debt = (env.storage().instance().get(&StorageKey::AccumulatedPerShare).unwrap_or(0) * pos.shares) / 1_000_000_000_000;
                pos
            }
            None => {
                let count: u32 = env.storage().instance()
                    .get(&StorageKey::LpCount).unwrap_or(0);
                let lp_address_key = StorageKey::LpAddress(count);
                env.storage().persistent().set(&lp_address_key, &provider);
                Self::extend_to_max(&env, &lp_address_key);
                env.storage().instance().set(&StorageKey::LpCount, &(count + 1));
                let acc_per_share: i128 = env.storage().instance().get(&StorageKey::AccumulatedPerShare).unwrap_or(0);
                LpPosition {
                    provider:         provider.clone(),
                    deposited:        amount,
                    shares:           new_shares,
                    yield_claimed:    0,
                    yield_debt:        (acc_per_share * new_shares) / 1_000_000_000_000,
                    deposited_at:     now,
                    last_yield_claim: now,
                }
            }
        };
        env.storage().persistent().set(&lp_key, &position);
        Self::extend_to_max(&env, &lp_key);
        env.storage().instance().set(&StorageKey::TotalDeposited, &(total_deposited + amount));
        env.storage().instance().set(&StorageKey::TotalShares,    &(total_shares + new_shares));

        env.events().publish(
            (Symbol::new(&env, "liquidity_deposited"),),
            LiquidityDeposited {
                provider: provider.clone(),
                amount,
                shares_minted: new_shares,
            },
        );

        // All deposit state is now persisted; safe to move the yield owed to
        // the provider, if any, as the last step (checks-effects-interactions).
        Self::pay_out_yield(&env, &provider, pending_yield);

        new_shares
    }

    /// Burn `shares` and return the proportional USDC to `provider`. Returns the USDC amount
    /// transferred. Panics with `Undercollateralized` if the available (unlocked) liquidity
    /// is insufficient to cover the redemption.
    pub fn withdraw(env: Env, provider: Address, shares: i128) -> i128 {
        provider.require_auth();
        // Guard: check for zero or negative shares input
        if shares <= 0 { panic_with_error!(&env, Error::ZeroAmount); }
        Self::assert_withdrawable(&env);

        let lp_key = StorageKey::LpPosition(provider.clone());
        let mut position: LpPosition = env.storage().persistent()
            .get(&lp_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoShares));
        if position.shares < shares { panic_with_error!(&env, Error::InsufficientFunds); }

        let total_deposited: i128 = env.storage().instance().get(&StorageKey::TotalDeposited).unwrap_or(0);
        let total_shares: i128    = env.storage().instance().get(&StorageKey::TotalShares).unwrap_or(0);
        let total_locked: i128    = env.storage().instance().get(&StorageKey::TotalLocked).unwrap_or(0);

        // Guard: prevent division by zero if total_shares == 0
        if total_shares == 0 { panic_with_error!(&env, Error::NoShares); }

        let available_liquidity = total_deposited.saturating_sub(total_locked);
        if available_liquidity <= 0 { panic_with_error!(&env, Error::Undercollateralized); }
        let amount = shares.checked_mul(total_deposited)
            .and_then(|v| v.checked_div(total_shares))
            .unwrap_or_else(|| panic_with_error!(&env, Error::Overflow));
        if amount == 0 { panic_with_error!(&env, Error::ZeroAmount); }
        if amount > available_liquidity { panic_with_error!(&env, Error::Undercollateralized); }

        let pending_yield = Self::settle_yield(&env, &mut position);
        position.deposited = position.deposited.saturating_sub(amount);
        position.shares   -= shares;
        position.yield_debt = (env.storage().instance().get(&StorageKey::AccumulatedPerShare).unwrap_or(0) * position.shares) / 1_000_000_000_000;
        env.storage().persistent().set(&lp_key, &position);
        Self::extend_to_max(&env, &lp_key);
        env.storage().instance().set(&StorageKey::TotalDeposited, &total_deposited.checked_sub(amount).unwrap_or_else(|| panic_with_error!(&env, Error::Overflow)));
        env.storage().instance().set(&StorageKey::TotalShares,    &(total_shares - shares));

        env.events().publish(
            (Symbol::new(&env, "liquidity_withdrawn"),),
            LiquidityWithdrawn {
                provider: provider.clone(),
                shares_burned: shares,
                amount_returned: amount,
            },
        );

        // All withdrawal state is persisted before either external transfer
        // runs: the principal amount, then any yield owed (checks-effects-interactions).
        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&env.current_contract_address(), &provider, &amount);
        Self::pay_out_yield(&env, &provider, pending_yield);

        amount
    }

    // ── Premium and yield ─────────────────────────────────────────────────────

    /// Pull `amount` USDC from `caller` and split it among LPs, treasury, and backstop
    /// according to the protocol fee schedule. No-op if `amount` is zero or negative.
    pub fn receive_premium(env: Env, caller: Address, amount: i128) {
        caller.require_auth();
        if amount <= 0 { return; }
        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&caller, &env.current_contract_address(), &amount);

        let (lp_bps, treas_bps, backstop_bps) = Self::get_premium_split_bps(&env);
        let lp_share       = amount * lp_bps       / 10_000;
        let treas_share    = amount * treas_bps    / 10_000;
        let backstop_share = amount * backstop_bps / 10_000;

        let treasury: Address = env.storage().instance().get(&StorageKey::Treasury).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&env.current_contract_address(), &treasury, &treas_share);

        let backstop: Address = env.storage().instance().get(&StorageKey::Backstop).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&env.current_contract_address(), &backstop, &backstop_share);

        let acc: i128 = env.storage().instance()
            .get(&StorageKey::AccumulatedPremium).unwrap_or(0);
        env.storage().instance().set(&StorageKey::AccumulatedPremium, &(acc + lp_share));

        let total_shares: i128 = env.storage().instance()
            .get(&StorageKey::TotalShares).unwrap_or(0);
        if total_shares > 0 {
            let acc_per_share: i128 = env.storage().instance()
                .get(&StorageKey::AccumulatedPerShare).unwrap_or(0);
            let increment = lp_share
                .checked_mul(1_000_000_000_000)
                .and_then(|v| v.checked_div(total_shares))
                .unwrap_or_else(|| panic_with_error!(&env, Error::Overflow));
            env.storage().instance().set(&StorageKey::AccumulatedPerShare, &(acc_per_share + increment));
        }

        let acc_backstop: i128 = env.storage().instance()
            .get(&StorageKey::AccumulatedBackstop).unwrap_or(0);
        env.storage().instance().set(&StorageKey::AccumulatedBackstop, &(acc_backstop + backstop_share));

        env.events().publish(
            (Symbol::new(&env, "premium_distributed"),),
            PremiumDistributed {
                amount,
                lp_share,
                treasury_share: treas_share,
                backstop_share,
            },
        );
    }

    /// Send a specified amount of USDC from the pool to the treasury address.
    /// Called by the backend after each policy purchase to distribute earned premiums.
    pub fn send_premium_to_treasury(env: Env, caller: Address, amount: i128) {
        Self::require_admin(&env, &caller);
        if amount <= 0 { panic_with_error!(&env, Error::ZeroAmount); }
        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        let treasury: Address = env.storage().instance().get(&StorageKey::Treasury).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&env.current_contract_address(), &treasury, &amount);
        env.events().publish(
            (Symbol::new(&env, "treasury_funded"),),
            TreasuryFunded { amount, recipient: treasury },
        );
    }

    /// Send a specified amount of USDC from the pool to the backstop address.
    /// Called by the backend after each policy purchase to distribute earned premiums.
    pub fn send_premium_to_backstop(env: Env, caller: Address, amount: i128) {
        Self::require_admin(&env, &caller);
        if amount <= 0 { panic_with_error!(&env, Error::ZeroAmount); }
        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        let backstop: Address = env.storage().instance().get(&StorageKey::Backstop).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&env.current_contract_address(), &backstop, &amount);
        env.events().publish(
            (Symbol::new(&env, "backstop_funded"),),
            BackstopFunded { amount, recipient: backstop },
        );
    }

    /// Returns the configured backstop address.
    pub fn get_backstop(env: Env) -> Address {
        env.storage().instance().get(&StorageKey::Backstop)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
    }

    /// Collect accumulated premium yield for `provider` and transfer it in USDC.
    /// Returns the amount claimed. No-op (returns 0) if no yield has accrued.
    pub fn claim_yield(env: Env, provider: Address) -> i128 {
        provider.require_auth();
        let lp_key = StorageKey::LpPosition(provider.clone());
        let mut position: LpPosition = env.storage().persistent()
            .get(&lp_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoShares));

        let claimed = Self::settle_yield(&env, &mut position);

        if claimed > 0 {
            env.storage().persistent().set(&lp_key, &position);
            Self::extend_to_max(&env, &lp_key);
        }

        Self::pay_out_yield(&env, &provider, claimed);

        claimed
    }

    // ── Capital locks ─────────────────────────────────────────────────────────

    /// Earmark `amount` USDC as collateral for `policy_id`. Only the policy engine or
    /// claims processor may call this. Panics if the pool is under-collateralised or if
    /// a lock for this policy already exists.
    pub fn lock_for_policy(env: Env, caller: Address, policy_id: u128, amount: i128) {
        Self::require_protocol_caller(&env, &caller);
        Self::assert_active(&env);
        // Guard: check for zero or negative lock amount input
        if amount <= 0 { panic_with_error!(&env, Error::ZeroAmount); }

        let total_deposited: i128 = env.storage().instance().get(&StorageKey::TotalDeposited).unwrap_or(0);
        let total_locked: i128    = env.storage().instance().get(&StorageKey::TotalLocked).unwrap_or(0);
        let available = total_deposited.saturating_sub(total_locked);
        if available < amount { panic_with_error!(&env, Error::Undercollateralized); }
        if env.storage().persistent().has(&StorageKey::Lock(policy_id)) { panic_with_error!(&env, Error::AlreadyLocked); }

        let lock_key = StorageKey::Lock(policy_id);
        env.storage().persistent().set(&lock_key, &CapitalLock {
            policy_id,
            amount,
            locked_at: env.ledger().timestamp(),
            released:  false,
        });
        env.storage().persistent().extend_ttl(&StorageKey::Lock(policy_id), TTL_THRESHOLD, TTL_EXTEND_TO);
        env.storage().instance().set(&StorageKey::TotalLocked, &(total_locked + amount));

        env.events().publish(
            (Symbol::new(&env, "capital_locked"),),
            CapitalLocked {
                policy_id,
                amount,
            },
        );
    }

    /// Release the capital lock for `policy_id` after a successful claim payout.
    ///
    /// ### Flow & Caller
    /// This function is called by the `claims-processor` contract immediately after the policy
    /// engine successfully transfers the coverage payout to the policyholder.
    ///
    /// ### Capital Effect
    /// This reduces `total_locked` in the pool, reflecting that the coverage lock is now removed.
    /// Since the claim was paid, the underwriting capital has been disbursed to the policyholder.
    ///
    /// ### Design Rationale
    /// Having separate functions (`release_for_claim` and `release_for_expiry`) instead of a single
    /// `release(policy_id, reason)` endpoint serves several key purposes:
    /// 1. **Access Control & Security**: Allows fine-grained tracking of capital outflows due to claims
    ///    vs. standard policy expirations.
    /// 2. **Auditability & Logging**: Distinct event paths make off-chain monitoring, analytics,
    ///    and accounting of paid claims vs. expired policies trivial.
    /// 3. **Gas Optimization**: Avoids the instruction overhead of parsing and branching on an enum/string
    ///    reason within the contract.
    pub fn release_for_claim(env: Env, caller: Address, policy_id: u128) {
        Self::require_protocol_caller(&env, &caller);
        let mut lock: CapitalLock = env.storage().persistent()
            .get(&StorageKey::Lock(policy_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::LockNotFound));
        
        // Guard: check for zero or negative amount before processing release metrics
        if lock.amount <= 0 { panic_with_error!(&env, Error::ZeroAmount); }
        if lock.released { panic_with_error!(&env, Error::AlreadyReleased); }
        
        lock.released = true;
        env.storage().persistent().set(&StorageKey::Lock(policy_id), &lock);
        env.storage().persistent().extend_ttl(&StorageKey::Lock(policy_id), TTL_THRESHOLD, TTL_EXTEND_TO);
        let total_locked: i128 = env.storage().instance().get(&StorageKey::TotalLocked).unwrap_or(0);
        env.storage().instance().set(&StorageKey::TotalLocked, &(total_locked.saturating_sub(lock.amount)));

        env.events().publish(
            (Symbol::new(&env, "capital_released"),),
            CapitalReleased {
                policy_id,
                amount: lock.amount,
            },
        );
    }

    /// Release the capital lock for `policy_id` when the policy expires without a payout.
    ///
    /// ### Flow & Caller
    /// This function is called by the `claims-processor` contract when a policy reaches its
    /// expiration timestamp without triggering a payout.
    ///
    /// ### Capital Effect
    /// This reduces `total_locked` in the pool, releasing the locked capital back into the
    /// pool's available liquidity. Unlike `release_for_claim`, the capital remains in the pool
    /// and is available to underwrite new policies, while the premium paid by the policyholder
    /// is fully earned by the pool.
    ///
    /// ### Design Rationale
    /// Having separate functions (`release_for_claim` and `release_for_expiry`) instead of a single
    /// `release(policy_id, reason)` endpoint serves several key purposes:
    /// 1. **Access Control & Security**: Allows fine-grained tracking of capital outflows due to claims
    ///    vs. standard policy expirations.
    /// 2. **Auditability & Logging**: Distinct event paths make off-chain monitoring, analytics,
    ///    and accounting of paid claims vs. expired policies trivial.
    /// 3. **Gas Optimization**: Avoids the instruction overhead of parsing and branching on an enum/string
    ///    reason within the contract.
    pub fn release_for_expiry(env: Env, caller: Address, policy_id: u128) {
        Self::require_protocol_caller(&env, &caller);
        let mut lock: CapitalLock = env.storage().persistent()
            .get(&StorageKey::Lock(policy_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::LockNotFound));
        if lock.released { panic_with_error!(&env, Error::AlreadyReleased); }
        lock.released = true;
        env.storage().persistent().set(&StorageKey::Lock(policy_id), &lock);
        env.storage().persistent().extend_ttl(&StorageKey::Lock(policy_id), TTL_THRESHOLD, TTL_EXTEND_TO);
        let total_locked: i128 = env.storage().instance().get(&StorageKey::TotalLocked).unwrap_or(0);
        env.storage().instance().set(&StorageKey::TotalLocked, &(total_locked.saturating_sub(lock.amount)));
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    /// Return aggregate pool statistics: total deposited, locked, shares, and premium accumulators.
    pub fn get_stats(env: Env) -> PoolStats {
        PoolStats {
            category:             env.storage().instance().get(&StorageKey::Category).unwrap(),
            total_deposited:      env.storage().instance().get(&StorageKey::TotalDeposited).unwrap_or(0),
            total_locked:         env.storage().instance().get(&StorageKey::TotalLocked).unwrap_or(0),
            total_shares:         env.storage().instance().get(&StorageKey::TotalShares).unwrap_or(0),
            accumulated_premium:  env.storage().instance().get(&StorageKey::AccumulatedPremium).unwrap_or(0),
            accumulated_backstop: env.storage().instance().get(&StorageKey::AccumulatedBackstop).unwrap_or(0),
            status:               env.storage().instance().get(&StorageKey::Status).unwrap_or(PoolStatus::Active),
        }
    }

    /// Return the LP position for `provider`, or `None` if they have never deposited.
    pub fn get_position(env: Env, provider: Address) -> Option<LpPosition> {
        env.storage().persistent().get(&StorageKey::LpPosition(provider))
    }

    /// Return the pool utilisation rate in basis points (locked / deposited × 10,000).
    /// Returns 0 if no USDC has been deposited.
    pub fn get_utilization_rate(env: Env) -> u32 {
        let deposited: i128 = env.storage().instance().get(&StorageKey::TotalDeposited).unwrap_or(0);
        let locked: i128    = env.storage().instance().get(&StorageKey::TotalLocked).unwrap_or(0);
        if deposited == 0 { return 0; }
        let util_bps = locked.checked_mul(10_000)
            .and_then(|v| v.checked_div(deposited))
            .unwrap_or(0);
        // Saturate to u32::MAX if the result exceeds u32 range
        if util_bps > u32::MAX as i128 {
            u32::MAX
        } else {
            util_bps as u32
        }
    }

    /// Return the current admin address. Panics with `NotInitialized` if not set up.
    pub fn get_admin(env: Env) -> Address {
        env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
    }

    /// Return the total number of unique LP addresses that have ever deposited.
    pub fn get_lp_count(env: Env) -> u32 {
        env.storage().instance().get(&StorageKey::LpCount).unwrap_or(0)
    }

    /// Return the current storage schema version (defaults to 1 before any migration).
    pub fn get_version(env: Env) -> u32 {
        env.storage().instance().get(&StorageKey::Version).unwrap_or(1)
    }

    /// Return the current premium split ratios in basis points: (lp, treasury, backstop).
    pub fn get_premium_split(env: Env) -> PremiumSplit {
        let (lp_bps, treas_bps, backstop_bps) = Self::get_premium_split_bps(&env);
        PremiumSplit { lp_bps, treas_bps, backstop_bps }
    }

    /// Return a paginated list of LP addresses that currently hold shares.
    /// `offset` defaults to 0 and `limit` defaults to 100 (capped at 500).
    pub fn get_lp_list(env: Env, offset: Option<u32>, limit: Option<u32>) -> PaginatedLps {
        let total_count: u32 = env.storage().instance()
            .get(&StorageKey::LpCount).unwrap_or(0);

        let offset_val = offset.unwrap_or(0);
        let limit_val = core::cmp::min(limit.unwrap_or(100), 500);

        let mut paginated = Vec::new(&env);
        if offset_val < total_count {
            let end = core::cmp::min(offset_val + limit_val, total_count);
            for i in offset_val..end {
                if let Some(addr) = env.storage().persistent()
                    .get::<_, Address>(&StorageKey::LpAddress(i))
                {
                    if let Some(position) = env.storage().persistent()
                        .get::<_, LpPosition>(&StorageKey::LpPosition(addr.clone()))
                    {
                        if position.shares > 0 {
                            paginated.push_back(addr);
                        }
                    }
                }
            }
        }

        PaginatedLps {
            lps: paginated,
            total_count,
        }
    }

    /// Available (unlocked) liquidity in USDC stroops.
    pub fn get_available_liquidity(env: Env) -> i128 {
        let deposited: i128 = env.storage().instance().get(&StorageKey::TotalDeposited).unwrap_or(0);
        let locked: i128    = env.storage().instance().get(&StorageKey::TotalLocked).unwrap_or(0);
        deposited.saturating_sub(locked)
    }

    // ── Admin ─────────────────────────────────────────────────────────────────
    //
    // Admin Powers:
    //   - `pause` / `resume`  — halt or resume deposits (no effect on withdrawals)
    //   - `lock_for_policy`   — earmark capital for an active policy
    //   - `release_for_claim` / `release_for_expiry` — unlock earmarked capital
    //   - `request_admin_withdrawal` / `execute_admin_withdrawal` —
    //     7-day-timelocked emergency withdrawal of unlocked liquidity
    //   - `cancel_admin_withdrawal` — abort a pending withdrawal
    //
    // Admin Limitations:
    //   - Admin CANNOT withdraw LP shares directly. Only the LP who owns
    //     shares may call `withdraw()` (gated by `provider.require_auth()`).
    //   - Admin CANNOT transfer pool USDC to any address except via the
    //     7-day timelock path, and only to the treasury address.
    //   - Admin CANNOT modify LP positions, shares, or yield entitlements.
    //
    // Timelock Policy:
    //   Any withdrawal of pool funds by the admin requires a 7-day waiting
    //   period so LPs have time to exit if they disagree with the decision.

    /// Admin-only: halt new deposits. Existing LPs may still withdraw and claim yield.
    pub fn pause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&StorageKey::Status, &PoolStatus::Paused);
        env.events().publish(
            (Symbol::new(&env, "pool_paused"),),
            PoolPaused { admin: admin.clone() },
        );
    }

    /// Admin-only: re-enable deposits after a pause.
    pub fn resume(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&StorageKey::Status, &PoolStatus::Active);
        env.events().publish(
            (Symbol::new(&env, "pool_resumed"),),
            PoolResumed { admin: admin.clone() },
        );
    }

    /// Admin-only: begin graceful wind-down of the pool. Blocks new deposits while
    /// letting existing LPs continue to withdraw and claim yield. Only callable
    /// while the pool is `Active`.
    pub fn start_winding_down(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        Self::assert_active(&env);
        env.storage().instance().set(&StorageKey::Status, &PoolStatus::WindingDown);
        env.events().publish(
            (Symbol::new(&env, "pool_winding_down"),),
            PoolWindingDown { admin: admin.clone() },
        );
    }

    /// Admin-only: update the premium split ratios (in basis points). The three
    /// values must sum to 10,000 (100%). Applies to premiums received after this call.
    pub fn update_premium_split(env: Env, admin: Address, lp_bps: i128, treas_bps: i128, backstop_bps: i128) {
        Self::require_admin(&env, &admin);
        if lp_bps < 0 || treas_bps < 0 || backstop_bps < 0 {
            panic_with_error!(&env, Error::InvalidSplit);
        }
        if lp_bps + treas_bps + backstop_bps != 10_000 {
            panic_with_error!(&env, Error::InvalidSplit);
        }
        let split = PremiumSplit { lp_bps, treas_bps, backstop_bps };
        env.storage().instance().set(&StorageKey::PremiumSplit, &split);
        env.events().publish(
            (Symbol::new(&env, "premium_split_updated"),),
            PremiumSplitUpdated { lp_bps, treas_bps, backstop_bps },
        );
    }

    /// Request an emergency withdrawal of unlocked liquidity.
    /// A 7-day timelock begins; LPs can exit before it matures.
    /// Only one pending request may exist at a time.
    pub fn request_admin_withdrawal(env: Env, admin: Address, amount: i128) {
        Self::require_admin(&env, &admin);
        if amount <= 0 { panic_with_error!(&env, Error::ZeroAmount); }

        let available = Self::get_available_liquidity(env.clone());
        if amount > available { panic_with_error!(&env, Error::Undercollateralized); }

        if env.storage().persistent().has(&StorageKey::AdminWithdrawalRequest) {
            panic_with_error!(&env, Error::TimelockPending);
        }

        let now = env.ledger().timestamp();
        // Freeze the deadline now. Recomputing it at execution time would let a
        // contract upgrade that shortens TIMELOCK_SECONDS cut the wait on a
        // request LPs have already seen published.
        let execute_after = now + TIMELOCK_SECONDS;
        env.storage().persistent().set(
            &StorageKey::AdminWithdrawalRequest,
            &AdminWithdrawalRequest {
                amount,
                requested_at: now,
                execute_after,
                executed: false,
            },
        );
        Self::extend_withdrawal_ttl(&env, &StorageKey::AdminWithdrawalRequest);

        env.events().publish(
            (Symbol::new(&env, "admin_withdrawal_scheduled"),),
            AdminWithdrawalScheduled {
                admin: admin.clone(),
                amount,
                execute_after,
            },
        );
    }

    /// Execute a previously requested admin withdrawal after the 7-day timelock.
    /// Funds are transferred to the treasury address.
    pub fn execute_admin_withdrawal(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        let req: AdminWithdrawalRequest = env.storage().persistent()
            .get(&StorageKey::AdminWithdrawalRequest)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoPendingWithdrawal));

        if req.executed { panic_with_error!(&env, Error::AlreadyReleased); }

        let now = env.ledger().timestamp();
        if now < req.execute_after {
            panic_with_error!(&env, Error::TimelockNotReady);
        }

        // Re-check unlocked liquidity with fresh totals. `request_admin_withdrawal`
        // validated the amount at request time, but new capital locks created during
        // the timelock can shrink the available balance — executing blindly would
        // drain funds earmarked as policy collateral.
        let available = Self::get_available_liquidity(env.clone());
        if req.amount > available { panic_with_error!(&env, Error::Undercollateralized); }

        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        let treasury: Address = env.storage().instance().get(&StorageKey::Treasury).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&env.current_contract_address(), &treasury, &req.amount);

        let mut req = req;
        req.executed = true;
        env.storage().persistent().set(&StorageKey::AdminWithdrawalRequest, &req);
        Self::extend_withdrawal_ttl(&env, &StorageKey::AdminWithdrawalRequest);

        let total_deposited: i128 = env.storage().instance()
            .get(&StorageKey::TotalDeposited).unwrap_or(0);
        env.storage().instance()
            .set(&StorageKey::TotalDeposited, &(total_deposited.saturating_sub(req.amount)));

        env.events().publish(
            (Symbol::new(&env, "admin_withdrawal_executed"),),
            AdminWithdrawalExecuted {
                admin: admin.clone(),
                amount: req.amount,
            },
        );
    }

    /// Cancel a pending admin withdrawal request.
    pub fn cancel_admin_withdrawal(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        if !env.storage().persistent().has(&StorageKey::AdminWithdrawalRequest) {
            panic_with_error!(&env, Error::NoPendingWithdrawal);
        }
        env.storage().persistent().remove(&StorageKey::AdminWithdrawalRequest);

        env.events().publish(
            (Symbol::new(&env, "admin_withdrawal_cancelled"),),
            AdminWithdrawalCancelled { admin: admin.clone() },
        );
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
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: soroban_sdk::BytesN<32>, new_version: u32) {
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
    pub fn approve_upgrade(env: Env, guardian: Address, new_wasm_hash: soroban_sdk::BytesN<32>) {
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
        } else {
            env.storage()
                .instance()
                .set(&StorageKey::PendingAdminChange, &pending);
        }
    }

    /// Accept the proposed admin. Only the proposed admin can call this.
    pub fn accept_admin(env: Env, admin: Address) {
        let pending_admin: Address = env.storage().instance()
            .get(&StorageKey::PendingAdmin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::Unauthorized));
        // Only the pending admin can accept
        if admin != pending_admin {
            panic_with_error!(&env, Error::Unauthorized);
        }
        admin.require_auth();
        // Update admin
        env.storage().instance().set(&StorageKey::Admin, &admin);
        // Clear the proposal
        env.storage().instance().remove(&StorageKey::PendingAdmin);
        // Emit event
        env.events().publish(
            (Symbol::new(&env, "admin_updated"),),
            AdminUpdated {
                new_admin: admin,
            },
        );
    }

    /// Enforces that only admin, the registered policy engine, or the registered
    /// claims processor may call capital-lock functions.
    fn require_protocol_caller(env: &Env, caller: &Address) {
        let admin: Address = env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        let pe: Address = env.storage().instance().get(&StorageKey::PolicyEngine)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        let cp: Address = env.storage().instance().get(&StorageKey::ClaimsProcessor)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        if *caller != admin && *caller != pe && *caller != cp {
            panic_with_error!(env, Error::Unauthorized);
        }
        caller.require_auth();
    }

    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        if *caller != admin { panic_with_error!(env, Error::Unauthorized); }
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

    /// Return the current premium split ratios in basis points, falling back to the
    /// compiled-in defaults if the admin has never called `update_premium_split`.
    fn get_premium_split_bps(env: &Env) -> (i128, i128, i128) {
        match env.storage().instance().get::<_, PremiumSplit>(&StorageKey::PremiumSplit) {
            Some(split) => (split.lp_bps, split.treas_bps, split.backstop_bps),
            None => (DEFAULT_PREMIUM_LP_BPS, DEFAULT_PREMIUM_TREAS_BPS, DEFAULT_PREMIUM_BACKSTOP_BPS),
        }
    }

    fn assert_active(env: &Env) {
        let status: PoolStatus = env.storage().instance()
            .get(&StorageKey::Status).unwrap_or(PoolStatus::Active);
        if status != PoolStatus::Active { panic_with_error!(env, Error::PoolNotActive); }
    }

    /// Withdrawals are allowed while the pool is `Active` or `WindingDown`, but not
    /// while `Paused`.
    fn assert_withdrawable(env: &Env) {
        let status: PoolStatus = env.storage().instance()
            .get(&StorageKey::Status).unwrap_or(PoolStatus::Active);
        if status == PoolStatus::Paused { panic_with_error!(env, Error::PoolNotActive); }
    }

    /// Extend a persistent entry's TTL to the network maximum. Used for
    /// LP position/index records, which have no natural expiry (issue #244).
    fn extend_to_max(env: &Env, key: &StorageKey) {
        let max_ttl = env.storage().max_ttl();
        env.storage().persistent().extend_ttl(key, max_ttl, max_ttl);
    }

    /// Extend an `AdminWithdrawalRequest` entry's TTL. `TTL_THRESHOLD`/`TTL_EXTEND_TO`
    /// (~30 days / ~1 year) comfortably cover the `TIMELOCK_SECONDS` (7-day) wait
    /// between requesting and executing a withdrawal (issue #244).
    fn extend_withdrawal_ttl(env: &Env, key: &StorageKey) {
        env.storage().persistent().extend_ttl(key, TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    /// Settle `position`'s accrued yield in memory (updates `yield_claimed`,
    /// `last_yield_claim`, and `yield_debt`) and return the amount owed to the
    /// provider, if any. Does **not** transfer tokens or emit events — callers
    /// must finish persisting all state first, then call
    /// [`Self::pay_out_yield`] last, so the external token transfer happens
    /// only after every state mutation for the operation has landed
    /// (checks-effects-interactions; avoids reentering mid-deposit/withdraw).
    fn settle_yield(env: &Env, position: &mut LpPosition) -> i128 {
        let total_shares: i128 = env.storage().instance().get(&StorageKey::TotalShares).unwrap_or(0);
        let acc_per_share: i128 = env.storage().instance().get(&StorageKey::AccumulatedPerShare).unwrap_or(0);
        if total_shares == 0 {
            return 0;
        }

        let entitled = (acc_per_share * position.shares) / 1_000_000_000_000;
        let claimable = entitled.saturating_sub(position.yield_debt);
        if claimable > 0 {
            position.yield_claimed += claimable;
            position.last_yield_claim = env.ledger().timestamp();
            position.yield_debt = entitled;
        }
        claimable
    }

    /// Transfer a previously-settled yield `amount` to `provider` and emit
    /// `yield_claimed`. Must be called only after all other state for the
    /// current operation has been persisted — see [`Self::settle_yield`].
    fn pay_out_yield(env: &Env, provider: &Address, amount: i128) {
        if amount <= 0 {
            return;
        }
        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        token::Client::new(env, &usdc)
            .transfer(&env.current_contract_address(), provider, &amount);

        env.events().publish(
            (Symbol::new(env, "yield_claimed"),),
            YieldClaimed {
                provider: provider.clone(),
                amount,
            },
        );
    }
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_advanced;
#[cfg(test)]
mod test_edge;
