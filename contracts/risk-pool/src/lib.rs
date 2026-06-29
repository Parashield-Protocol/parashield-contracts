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

use soroban_sdk::{
    contract, contractimpl, contracttype, contracterror, panic_with_error,
    token, Address, Env, Symbol, Vec,
};

pub mod types;
pub use types::*;

const PREMIUM_LP_BPS:    i128 = 8_000;  // 80% of premium to LP pool
const PREMIUM_TREAS_BPS: i128 = 1_000;  // 10% to treasury

/// Upper bound on cumulative deposits (7-decimal USDC stroops).
/// 10^15 stroops == 100,000,000 USDC. Caps total pool size so share value
/// cannot become infinitesimal and total_shares cannot overflow.
const MAX_TOTAL_DEPOSITED: i128 = 1_000_000_000_000_000;

/// Timelock duration for admin withdrawals: 7 days in seconds.
const TIMELOCK_SECONDS: u64 = 7 * 24 * 60 * 60;

#[contracttype]
enum StorageKey {
    Initialized,
    Admin,
    Treasury,
    UsdcToken,
    Category,
    TotalDeposited,
    TotalLocked,
    TotalShares,
    AccumulatedPremium,
    Status,
    LpPosition(Address),
    LpList,
    Lock(u128),
    AdminWithdrawalRequest,
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
    TimelockPending     = 13,
    TimelockNotReady    = 14,
    NoPendingWithdrawal = 15,
}

#[contract]
pub struct RiskPool;

#[contractimpl]
impl RiskPool {

    pub fn initialize(
        env: Env,
        admin: Address,
        usdc_token: Address,
        treasury: Address,
        category: Symbol,
    ) {
        if env.storage().instance().has(&StorageKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        // Address validation is deferred to require_auth() calls which
        // verify the address on the Soroban network layer.
        admin.require_auth();
        env.storage().instance().set(&StorageKey::Initialized,        &true);
        env.storage().instance().set(&StorageKey::Admin,              &admin);
        env.storage().instance().set(&StorageKey::UsdcToken,          &usdc_token);
        env.storage().instance().set(&StorageKey::Treasury,           &treasury);
        env.storage().instance().set(&StorageKey::Category,           &category);
        env.storage().instance().set(&StorageKey::TotalDeposited,     &0i128);
        env.storage().instance().set(&StorageKey::TotalLocked,        &0i128);
        env.storage().instance().set(&StorageKey::TotalShares,        &0i128);
        env.storage().instance().set(&StorageKey::AccumulatedPremium, &0i128);
        env.storage().instance().set(&StorageKey::Status,             &PoolStatus::Active);
        env.storage().instance().set(&StorageKey::LpList,             &Vec::<Address>::new(&env));

        env.events().publish(
            (Symbol::new(&env, "initialized"),),
            Initialized {
                admin: admin.clone(),
                usdc_token: usdc_token.clone(),
                treasury: treasury.clone(),
                category: category.clone(),
            },
        );
    }

    // ── Deposits ──────────────────────────────────────────────────────────────

    pub fn deposit(env: Env, provider: Address, amount: i128) -> i128 {
        provider.require_auth();
        if amount <= 0 { panic_with_error!(&env, Error::ZeroAmount); }
        Self::assert_active(&env);

        let total_deposited: i128 = env.storage().instance()
            .get(&StorageKey::TotalDeposited).unwrap_or(0);
        let total_shares: i128 = env.storage().instance()
            .get(&StorageKey::TotalShares).unwrap_or(0);

        // Enforce the global pool size cap before accepting new liquidity.
        if total_deposited + amount > MAX_TOTAL_DEPOSITED {
            panic_with_error!(&env, Error::PoolCapExceeded);
        }

        let new_shares = if total_deposited == 0 || total_shares == 0 {
            amount * 1_000_000_000  // 1 share = 1 USDC * 1e9 precision
        } else {
            amount * total_shares / total_deposited
        };

        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&provider, &env.current_contract_address(), &amount);

        let now = env.ledger().timestamp();
        let lp_key = StorageKey::LpPosition(provider.clone());
        let position: LpPosition = match env.storage().persistent().get::<_, LpPosition>(&lp_key) {
            Some(mut pos) => {
                pos.deposited += amount;
                pos.shares    += new_shares;
                pos
            }
            None => {
                let mut lp_list: Vec<Address> = env.storage().instance()
                    .get(&StorageKey::LpList).unwrap_or_else(|| Vec::new(&env));
                lp_list.push_back(provider.clone());
                env.storage().instance().set(&StorageKey::LpList, &lp_list);
                LpPosition {
                    provider:         provider.clone(),
                    deposited:        amount,
                    shares:           new_shares,
                    yield_claimed:    0,
                    deposited_at:     now,
                    last_yield_claim: now,
                }
            }
        };
        env.storage().persistent().set(&lp_key, &position);
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

        new_shares
    }

    pub fn withdraw(env: Env, provider: Address, shares: i128) -> i128 {
        provider.require_auth();
        // Guard: check for zero or negative shares input
        if shares <= 0 { panic_with_error!(&env, Error::ZeroAmount); }

        let lp_key = StorageKey::LpPosition(provider.clone());
        let mut position: LpPosition = env.storage().persistent()
            .get(&lp_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoShares));
        if position.shares < shares { panic_with_error!(&env, Error::InsufficientFunds); }

        let total_deposited: i128 = env.storage().instance().get(&StorageKey::TotalDeposited).unwrap_or(0);
        let total_shares: i128    = env.storage().instance().get(&StorageKey::TotalShares).unwrap_or(0);
        let total_locked: i128    = env.storage().instance().get(&StorageKey::TotalLocked).unwrap_or(0);

        let available_liquidity = total_deposited.saturating_sub(total_locked);
        if available_liquidity <= 0 { panic_with_error!(&env, Error::Undercollateralized); }
        let amount = shares * available_liquidity / total_shares;
        if amount > available_liquidity { panic_with_error!(&env, Error::Undercollateralized); }

        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&env.current_contract_address(), &provider, &amount);

        position.deposited = position.deposited.saturating_sub(amount);
        position.shares   -= shares;
        env.storage().persistent().set(&lp_key, &position);
        env.storage().instance().set(&StorageKey::TotalDeposited, &(total_deposited - amount));
        env.storage().instance().set(&StorageKey::TotalShares,    &(total_shares - shares));

        env.events().publish(
            (Symbol::new(&env, "liquidity_withdrawn"),),
            LiquidityWithdrawn {
                provider: provider.clone(),
                shares_burned: shares,
                amount_returned: amount,
            },
        );

        amount
    }

    // ── Premium and yield ─────────────────────────────────────────────────────

    pub fn receive_premium(env: Env, caller: Address, amount: i128) {
        caller.require_auth();
        if amount <= 0 { return; }
        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&caller, &env.current_contract_address(), &amount);

        let lp_share    = amount * PREMIUM_LP_BPS    / 10_000;
        let treas_share = amount * PREMIUM_TREAS_BPS / 10_000;

        let treasury: Address = env.storage().instance().get(&StorageKey::Treasury).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&env.current_contract_address(), &treasury, &treas_share);

        let acc: i128 = env.storage().instance()
            .get(&StorageKey::AccumulatedPremium).unwrap_or(0);
        env.storage().instance().set(&StorageKey::AccumulatedPremium, &(acc + lp_share));

        env.events().publish(
            (Symbol::new(&env, "premium_distributed"),),
            PremiumDistributed {
                amount,
                lp_share,
                treasury_share: treas_share,
            },
        );
    }

    pub fn claim_yield(env: Env, provider: Address) -> i128 {
        provider.require_auth();
        let lp_key = StorageKey::LpPosition(provider.clone());
        let mut position: LpPosition = env.storage().persistent()
            .get(&lp_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoShares));

        let total_shares: i128   = env.storage().instance().get(&StorageKey::TotalShares).unwrap_or(0);
        let accumulated: i128    = env.storage().instance().get(&StorageKey::AccumulatedPremium).unwrap_or(0);
        if total_shares == 0 { return 0; }

        let entitled  = accumulated * position.shares / total_shares;
        let claimable = entitled.saturating_sub(position.yield_claimed);
        if claimable == 0 { return 0; }

        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&env.current_contract_address(), &provider, &claimable);

        position.yield_claimed   += claimable;
        position.last_yield_claim = env.ledger().timestamp();
        env.storage().persistent().set(&lp_key, &position);

        env.events().publish(
            (Symbol::new(&env, "yield_claimed"),),
            YieldClaimed {
                provider: provider.clone(),
                amount: claimable,
            },
        );

        claimable
    }

    // ── Capital locks ─────────────────────────────────────────────────────────

    pub fn lock_for_policy(env: Env, caller: Address, policy_id: u128, amount: i128) {
        Self::require_admin(&env, &caller);
        // Guard: check for zero or negative lock amount input
        if amount <= 0 { panic_with_error!(&env, Error::ZeroAmount); }

        let total_deposited: i128 = env.storage().instance().get(&StorageKey::TotalDeposited).unwrap_or(0);
        let total_locked: i128    = env.storage().instance().get(&StorageKey::TotalLocked).unwrap_or(0);
        if total_deposited - total_locked < amount { panic_with_error!(&env, Error::Undercollateralized); }
        if env.storage().persistent().has(&StorageKey::Lock(policy_id)) { panic_with_error!(&env, Error::AlreadyLocked); }

        env.storage().persistent().set(&StorageKey::Lock(policy_id), &CapitalLock {
            policy_id,
            amount,
            locked_at: env.ledger().timestamp(),
            released:  false,
        });
        env.storage().instance().set(&StorageKey::TotalLocked, &(total_locked + amount));

        env.events().publish(
            (Symbol::new(&env, "capital_locked"),),
            CapitalLocked {
                policy_id,
                amount,
            },
        );
    }

    pub fn release_for_claim(env: Env, caller: Address, policy_id: u128) {
        Self::require_admin(&env, &caller);
        let mut lock: CapitalLock = env.storage().persistent()
            .get(&StorageKey::Lock(policy_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::LockNotFound));
        
        // Guard: check for zero or negative amount before processing release metrics
        if lock.amount <= 0 { panic_with_error!(&env, Error::ZeroAmount); }
        if lock.released { panic_with_error!(&env, Error::AlreadyReleased); }
        
        lock.released = true;
        env.storage().persistent().set(&StorageKey::Lock(policy_id), &lock);
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

    pub fn release_for_expiry(env: Env, caller: Address, policy_id: u128) {
        Self::require_admin(&env, &caller);
        let mut lock: CapitalLock = env.storage().persistent()
            .get(&StorageKey::Lock(policy_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::LockNotFound));
        if lock.released { panic_with_error!(&env, Error::AlreadyReleased); }
        lock.released = true;
        env.storage().persistent().set(&StorageKey::Lock(policy_id), &lock);
        let total_locked: i128 = env.storage().instance().get(&StorageKey::TotalLocked).unwrap_or(0);
        env.storage().instance().set(&StorageKey::TotalLocked, &(total_locked.saturating_sub(lock.amount)));
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    pub fn get_stats(env: Env) -> PoolStats {
        PoolStats {
            category:            env.storage().instance().get(&StorageKey::Category).unwrap(),
            total_deposited:     env.storage().instance().get(&StorageKey::TotalDeposited).unwrap_or(0),
            total_locked:        env.storage().instance().get(&StorageKey::TotalLocked).unwrap_or(0),
            total_shares:        env.storage().instance().get(&StorageKey::TotalShares).unwrap_or(0),
            accumulated_premium: env.storage().instance().get(&StorageKey::AccumulatedPremium).unwrap_or(0),
            status:              env.storage().instance().get(&StorageKey::Status).unwrap_or(PoolStatus::Active),
        }
    }

    pub fn get_position(env: Env, provider: Address) -> Option<LpPosition> {
        env.storage().persistent().get(&StorageKey::LpPosition(provider))
    }

    pub fn get_utilization_rate(env: Env) -> u32 {
        let deposited: i128 = env.storage().instance().get(&StorageKey::TotalDeposited).unwrap_or(0);
        let locked: i128    = env.storage().instance().get(&StorageKey::TotalLocked).unwrap_or(0);
        if deposited == 0 { return 0; }
        (locked * 10_000 / deposited) as u32
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
    }

    pub fn get_lp_count(env: Env) -> u32 {
        let list: Vec<Address> = env.storage().instance()
            .get(&StorageKey::LpList)
            .unwrap_or_else(|| Vec::new(&env));
        list.len()
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

    pub fn pause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&StorageKey::Status, &PoolStatus::Paused);
        env.events().publish(
            (Symbol::new(&env, "pool_paused"),),
            PoolPaused { admin: admin.clone() },
        );
    }

    pub fn resume(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&StorageKey::Status, &PoolStatus::Active);
        env.events().publish(
            (Symbol::new(&env, "pool_resumed"),),
            PoolResumed { admin: admin.clone() },
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
        env.storage().persistent().set(
            &StorageKey::AdminWithdrawalRequest,
            &AdminWithdrawalRequest {
                amount,
                requested_at: now,
                executed: false,
            },
        );

        env.events().publish(
            (Symbol::new(&env, "admin_withdrawal_scheduled"),),
            AdminWithdrawalScheduled {
                admin: admin.clone(),
                amount,
                execute_after: now + TIMELOCK_SECONDS,
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
        if now < req.requested_at + TIMELOCK_SECONDS {
            panic_with_error!(&env, Error::TimelockNotReady);
        }

        let usdc: Address = env.storage().instance().get(&StorageKey::UsdcToken).unwrap();
        let treasury: Address = env.storage().instance().get(&StorageKey::Treasury).unwrap();
        token::Client::new(&env, &usdc)
            .transfer(&env.current_contract_address(), &treasury, &req.amount);

        let mut req = req;
        req.executed = true;
        env.storage().persistent().set(&StorageKey::AdminWithdrawalRequest, &req);

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

    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        if *caller != admin { panic_with_error!(env, Error::Unauthorized); }
        caller.require_auth();
    }

    fn assert_active(env: &Env) {
        let status: PoolStatus = env.storage().instance()
            .get(&StorageKey::Status).unwrap_or(PoolStatus::Active);
        if status != PoolStatus::Active { panic_with_error!(env, Error::PoolNotActive); }
    }
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_advanced;
#[cfg(test)]
mod test_edge;