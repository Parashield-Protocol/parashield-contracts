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

/// Default ceiling on `total_locked / total_deposited`, in basis points.
///
/// 10_000 (100%) preserves the historical behaviour exactly: before this was
/// configurable the pool would commit every unit of capital it held. Lowering
/// it is how an admin expresses "keep a buffer against correlated risk", and
/// that has to be an explicit choice rather than a silent change to how much
/// coverage an existing pool can write.
const DEFAULT_MAX_UTILIZATION_BPS: u32 = 10_000;

/// Longest withdrawal delay an admin may configure (30 days).
///
/// An unbounded delay is indistinguishable from freezing LP funds, so the
/// ceiling is part of what makes the queue safe to hand to an admin at all.
const MAX_EXIT_DELAY: u64 = 30 * 24 * 60 * 60;

/// Timelock duration for admin withdrawals: 7 days in seconds.
const TIMELOCK_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Timelock duration for parameter changes: 2 days in seconds.
/// Shorter than withdrawal timelock since parameter changes are less risky.
const PARAMETER_TIMELOCK_SECONDS: u64 = 2 * 24 * 60 * 60;

/// Grace period between an admin transfer being fully proposed/approved and the
/// proposed admin being able to `accept_admin` (issue #356). Hand-synced across
/// the 4 contracts that expose admin rotation (policy-engine, risk-pool,
/// oracle-verifier, claims-processor).
const ADMIN_TRANSFER_TIMELOCK: u64 = 48 * 60 * 60;

/// Extend a persistent entry's TTL once it has fewer than ~30 days of life left
/// (at ~5s/ledger).
// Issue #342: kept in sync by hand across all 5 contracts (governance-dao,
// risk-pool, policy-engine, oracle-verifier, claims-processor) — extracting
// to a shared crate is a real follow-up, not done here to avoid touching
// every contract's Cargo.toml in one pass.
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
    /// Ledger timestamp (u64) at which the current `PendingAdmin` was set,
    /// used to enforce `ADMIN_TRANSFER_TIMELOCK` before `accept_admin`.
    PendingAdminSince,
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
    /// A time-locked premium-split change awaiting execution.
    PendingParameterChange,
    /// Next LP NFT token ID (u64), sequential minting counter.
    NextNftId,
    /// LP NFT record by token ID — u64 → LpNft.
    LpNft(u64),
    /// Maps provider address to their LP NFT token ID — Address → u64.
    ProviderNft(Address),
    /// Admin-configured capacity limits (`PoolCapacity`). Falls back to the
    /// compile-time defaults when unset.
    Capacity,
    /// Withdrawal delay in seconds (u64). 0 = instant withdrawals, the
    /// historical behaviour and the default.
    ExitDelay,
    /// A provider's outstanding queued exit — Address → ExitRequest.
    ExitReq(Address),
    /// Total shares currently reserved by queued exits (i128), so the pool can
    /// see committed outflow before it happens.
    QueuedExitShares,
    /// Dynamic fee adjustment configuration (DynamicFeeConfig).
    /// Allows pool fees to automatically adjust based on market conditions and utilization.
    DynamicFeeConfig,
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
    ParameterChangePending    = 27,
    NoPendingParameterChange  = 28,
    ParameterChangeNotReady   = 29,
    AdminTimelockNotExpired   = 30,
    UtilizationCapExceeded    = 31,
    InvalidCapacity           = 32,
    ExitAlreadyQueued         = 33,
    NoExitRequest             = 34,
    ExitDelayNotElapsed       = 35,
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
    /// `compound_enabled` — if true, accrued yield is automatically reinvested (compounded)
    /// instead of being paid out on each deposit. LPs can toggle this later via `toggle_compound`.
    pub fn deposit(
        env: Env,
        provider: Address,
        amount: i128,
        min_shares: i128,
        compound_enabled: bool,
    ) -> i128 {
        provider.require_auth();
        if amount <= 0 { panic_with_error!(&env, Error::ZeroAmount); }
        if amount < MIN_DEPOSIT { panic_with_error!(&env, Error::DepositTooSmall); }
        Self::assert_active(&env);

        let total_deposited: i128 = env.storage().instance()
            .get(&StorageKey::TotalDeposited).unwrap_or(0);
        let total_shares: i128 = env.storage().instance()
            .get(&StorageKey::TotalShares).unwrap_or(0);

        // Enforce the configured pool size cap before accepting new liquidity.
        let capacity = Self::capacity(&env);
        if total_deposited + amount > capacity.max_total_deposited {
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
        let mut is_new_lp = false;
        let mut position: LpPosition = match env.storage().persistent().get::<_, LpPosition>(&lp_key) {
            Some(mut pos) => {
                pending_yield = Self::settle_yield(&env, &mut pos);
                pos.deposited += amount;
                pos.shares    += new_shares;
                pos.yield_debt = (env.storage().instance().get(&StorageKey::AccumulatedPerShare).unwrap_or(0) * pos.shares) / 1_000_000_000_000;
                pos.compound_enabled = compound_enabled;
                pos
            }
            None => {
                is_new_lp = true;
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
                    compound_enabled,
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

        // Mint LP NFT on first deposit, or update existing NFT.
        let category: Symbol = env.storage().instance().get(&StorageKey::Category).unwrap();
        if is_new_lp {
            Self::mint_lp_nft(&env, &provider, &category, now, &position);
        } else {
            Self::update_lp_nft(&env, &provider, &position);
        }

        // Compound yield: if enabled, reinvest yield as additional deposit instead of paying out.
        if compound_enabled && pending_yield > 0 {
            // Yield is already reflected in position.shares via settle_yield;
            // skip external transfer — the yield stays in the pool backing the LP's shares.
        } else {
            // All deposit state is now persisted; safe to move the yield owed to
            // the provider, if any, as the last step (checks-effects-interactions).
            Self::pay_out_yield(&env, &provider, pending_yield);
        }

        new_shares
    }

    /// Burn `shares` and return the proportional USDC to `provider`. Returns the USDC amount
    /// transferred. Panics with `Undercollateralized` if the available (unlocked) liquidity
    /// is insufficient to cover the redemption.
    pub fn withdraw(env: Env, provider: Address, shares: i128) -> i128 {
        provider.require_auth();
        Self::withdraw_inner(env, provider, shares)
    }

    /// Settlement body shared by `withdraw` and `claim_exit`.
    ///
    /// Takes no authorization of its own: the caller has already established
    /// it. Calling `withdraw` from `claim_exit` instead would re-authorize a
    /// frame that is already authorized, which the host rejects outright.
    fn withdraw_inner(env: Env, provider: Address, shares: i128) -> i128 {
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

        // Update LP NFT after withdrawal
        Self::update_lp_nft(&env, &provider, &position);

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

    /// Transfer `shares` from `from` address to `to` address.
    /// Returns the proportional USDC deposit amount transferred.
    pub fn transfer_position(env: Env, from: Address, to: Address, shares: i128) -> i128 {
        from.require_auth();
        if shares <= 0 { panic_with_error!(&env, Error::ZeroAmount); }
        if from == to { panic_with_error!(&env, Error::InvalidAddress); }
        Self::assert_active(&env);

        let from_key = StorageKey::LpPosition(from.clone());
        let mut from_pos: LpPosition = env.storage().persistent()
            .get(&from_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoShares));

        if from_pos.shares < shares {
            panic_with_error!(&env, Error::InsufficientShares);
        }

        let amount = shares.checked_mul(from_pos.deposited)
            .and_then(|v| v.checked_div(from_pos.shares))
            .unwrap_or_else(|| panic_with_error!(&env, Error::Overflow));

        let pending_yield_from = Self::settle_yield(&env, &mut from_pos);
        from_pos.deposited = from_pos.deposited.saturating_sub(amount);
        from_pos.shares -= shares;
        let acc_per_share: i128 = env.storage().instance().get(&StorageKey::AccumulatedPerShare).unwrap_or(0);
        from_pos.yield_debt = (acc_per_share * from_pos.shares) / 1_000_000_000_000;
        env.storage().persistent().set(&from_key, &from_pos);
        Self::extend_to_max(&env, &from_key);

        Self::update_lp_nft(&env, &from, &from_pos);
        Self::pay_out_yield(&env, &from, pending_yield_from);

        let now = env.ledger().timestamp();
        let to_key = StorageKey::LpPosition(to.clone());
        let mut is_new_lp = false;
        let mut to_pos: LpPosition = match env.storage().persistent().get::<_, LpPosition>(&to_key) {
            Some(mut pos) => {
                let pending_yield_to = Self::settle_yield(&env, &mut pos);
                pos.deposited += amount;
                pos.shares += shares;
                pos.yield_debt = (acc_per_share * pos.shares) / 1_000_000_000_000;
                env.storage().persistent().set(&to_key, &pos);
                Self::extend_to_max(&env, &to_key);
                Self::pay_out_yield(&env, &to, pending_yield_to);
                pos
            }
            None => {
                is_new_lp = true;
                let count: u32 = env.storage().instance()
                    .get(&StorageKey::LpCount).unwrap_or(0);
                let lp_address_key = StorageKey::LpAddress(count);
                env.storage().persistent().set(&lp_address_key, &to);
                Self::extend_to_max(&env, &lp_address_key);
                env.storage().instance().set(&StorageKey::LpCount, &(count + 1));
                let pos = LpPosition {
                    provider: to.clone(),
                    deposited: amount,
                    shares,
                    yield_claimed: 0,
                    yield_debt: (acc_per_share * shares) / 1_000_000_000_000,
                    deposited_at: now,
                    last_yield_claim: now,
                    compound_enabled: false,
                };
                env.storage().persistent().set(&to_key, &pos);
                Self::extend_to_max(&env, &to_key);
                pos
            }
        };

        let category: Symbol = env.storage().instance().get(&StorageKey::Category).unwrap();
        if is_new_lp {
            Self::mint_lp_nft(&env, &to, &category, now, &to_pos);
        } else {
            Self::update_lp_nft(&env, &to, &to_pos);
        }

        env.events().publish(
            (Symbol::new(&env, "lp_position_transferred"),),
            LpPositionTransferred {
                from,
                to,
                shares,
                amount,
            },
        );

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

    // ── Capacity limits (issue #373) ──────────────────────────────────────────

    /// Set the pool's capacity limits.
    ///
    /// Two ceilings, because two different things can go wrong:
    ///
    /// - `max_total_deposited` bounds the capital held, so share value cannot
    ///   become infinitesimal and `total_shares` cannot overflow.
    /// - `max_utilization_bps` bounds how much of that capital may be
    ///   committed to active coverage at once. This is the correlated-risk
    ///   limit: a pool fully committed to one category in one region is a
    ///   single event away from insolvency however much capital it holds, and
    ///   nothing in the deposit ceiling prevents that.
    ///
    /// Lowering a limit below current usage is allowed and does not claw
    /// anything back — existing deposits and locks stand, and the pool simply
    /// accepts nothing further until it is back under the line. Refusing the
    /// change would leave an admin unable to stop an overexposed pool from
    /// growing, which is the opposite of what this exists for.
    pub fn set_capacity(
        env: Env,
        admin: Address,
        max_total_deposited: i128,
        max_utilization_bps: u32,
    ) {
        Self::require_admin(&env, &admin);

        if max_total_deposited <= 0 || max_utilization_bps == 0 || max_utilization_bps > 10_000 {
            panic_with_error!(&env, Error::InvalidCapacity);
        }

        env.storage().instance().set(
            &StorageKey::Capacity,
            &PoolCapacity {
                max_total_deposited,
                max_utilization_bps,
            },
        );

        env.events().publish(
            (Symbol::new(&env, "capacity_updated"),),
            PoolCapacityUpdated {
                max_total_deposited,
                max_utilization_bps,
            },
        );
    }

    /// The configured capacity limits, or the built-in defaults.
    pub fn get_capacity(env: Env) -> PoolCapacity {
        Self::capacity(&env)
    }

    /// How much of the pool's capacity is currently consumed, and how much
    /// room is left on each limit.
    ///
    /// Exposed so a front-end can tell an LP "this pool is full" and a policy
    /// engine can tell a buyer "this coverage cannot be written right now"
    /// before either submits a transaction that would revert.
    pub fn get_capacity_status(env: Env) -> CapacityStatus {
        let total_deposited: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::TotalDeposited)
            .unwrap_or(0);
        let total_locked: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::TotalLocked)
            .unwrap_or(0);
        let cap = Self::capacity(&env);

        let utilization_bps = Self::utilization_bps(total_locked, total_deposited);

        // What may still be underwritten before the utilization cap binds.
        let max_lockable = total_deposited
            .saturating_mul(cap.max_utilization_bps as i128)
            / 10_000;

        CapacityStatus {
            total_deposited,
            total_locked,
            max_total_deposited: cap.max_total_deposited,
            utilization_bps,
            max_utilization_bps: cap.max_utilization_bps,
            remaining_deposit_capacity: cap
                .max_total_deposited
                .saturating_sub(total_deposited)
                .max(0),
            remaining_coverage_capacity: max_lockable.saturating_sub(total_locked).max(0),
        }
    }

    // ── Exit queue (issue #377) ───────────────────────────────────────────────

    /// Set the withdrawal delay in seconds. 0 restores instant withdrawals.
    ///
    /// The delay exists because instant exit is a bank run waiting to happen:
    /// when a pool looks stressed, the LPs who move first are made whole out
    /// of the liquidity the remaining LPs were relying on. A queue makes every
    /// LP wait the same interval, which removes the advantage of panicking and
    /// gives the pool a window to see committed outflow coming.
    ///
    /// Capped at `MAX_EXIT_DELAY` (30 days). An unbounded delay would be
    /// indistinguishable from freezing LP funds.
    pub fn set_exit_delay(env: Env, admin: Address, delay_seconds: u64) {
        Self::require_admin(&env, &admin);

        if delay_seconds > MAX_EXIT_DELAY {
            panic_with_error!(&env, Error::InvalidCapacity);
        }

        env.storage()
            .instance()
            .set(&StorageKey::ExitDelay, &delay_seconds);

        env.events().publish(
            (Symbol::new(&env, "exit_delay_updated"),),
            ExitDelayUpdated { delay_seconds },
        );
    }

    /// The configured withdrawal delay in seconds (default 0 — instant).
    pub fn get_exit_delay(env: Env) -> u64 {
        Self::exit_delay(&env)
    }

    /// Queue a withdrawal of `shares`, claimable once the delay has elapsed.
    ///
    /// The shares stay in the LP's position and keep earning yield until the
    /// exit is claimed — queuing is a commitment to leave, not a forfeiture.
    /// What it does prevent is queuing twice: one outstanding request per
    /// provider, so the reserved total cannot be inflated past what the LP
    /// actually holds.
    ///
    /// Panics with `ExitAlreadyQueued` if a request is already outstanding;
    /// cancel it first to change the amount.
    pub fn request_exit(env: Env, provider: Address, shares: i128) {
        provider.require_auth();
        if shares <= 0 {
            panic_with_error!(&env, Error::ZeroAmount);
        }
        Self::assert_withdrawable(&env);

        let position: LpPosition = env
            .storage()
            .persistent()
            .get(&StorageKey::LpPosition(provider.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoShares));
        if position.shares < shares {
            panic_with_error!(&env, Error::InsufficientFunds);
        }

        let req_key = StorageKey::ExitReq(provider.clone());
        if env.storage().persistent().has(&req_key) {
            panic_with_error!(&env, Error::ExitAlreadyQueued);
        }

        let now = env.ledger().timestamp();
        let claimable_at = now.saturating_add(Self::exit_delay(&env));

        env.storage().persistent().set(
            &req_key,
            &ExitRequest {
                provider: provider.clone(),
                shares,
                requested_at: now,
                claimable_at,
            },
        );
        Self::extend_to_max(&env, &req_key);

        let queued: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::QueuedExitShares)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&StorageKey::QueuedExitShares, &queued.saturating_add(shares));

        env.events().publish(
            (Symbol::new(&env, "exit_requested"),),
            ExitRequested {
                provider,
                shares,
                claimable_at,
            },
        );
    }

    /// Withdraw a queued request whose delay has elapsed.
    ///
    /// Settlement runs through the same path as a direct `withdraw`, so the
    /// amount is priced at the share value *at claim time* rather than at
    /// request time. An LP cannot lock in a favourable price and wait to see
    /// whether the pool takes a loss in the meantime — that would hand queued
    /// LPs an option paid for by everyone else.
    pub fn claim_exit(env: Env, provider: Address) -> i128 {
        provider.require_auth();

        let req_key = StorageKey::ExitReq(provider.clone());
        let request: ExitRequest = env
            .storage()
            .persistent()
            .get(&req_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoExitRequest));

        let now = env.ledger().timestamp();
        if now < request.claimable_at {
            panic_with_error!(&env, Error::ExitDelayNotElapsed);
        }

        // Release the reservation before settling, so a failed withdrawal
        // cannot leave the request stuck and double-counted.
        Self::clear_exit_reservation(&env, &provider, request.shares);

        let amount = Self::withdraw_inner(env.clone(), provider.clone(), request.shares);

        env.events().publish(
            (Symbol::new(&env, "exit_claimed"),),
            ExitClaimed {
                provider,
                shares_burned: request.shares,
                amount_returned: amount,
                waited: now.saturating_sub(request.requested_at),
            },
        );

        amount
    }

    /// Cancel an outstanding exit request and release its reservation.
    ///
    /// Always available, including before the delay elapses: an LP who changes
    /// their mind should not be forced out, and a queue nobody can leave is a
    /// worse trap than the instant withdrawals it replaced.
    pub fn cancel_exit(env: Env, provider: Address) {
        provider.require_auth();

        let req_key = StorageKey::ExitReq(provider.clone());
        let request: ExitRequest = env
            .storage()
            .persistent()
            .get(&req_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoExitRequest));

        Self::clear_exit_reservation(&env, &provider, request.shares);

        env.events().publish(
            (Symbol::new(&env, "exit_cancelled"),),
            ExitCancelled {
                provider,
                shares: request.shares,
            },
        );
    }

    /// Where a provider's exit stands. Safe to call for any address — an
    /// address with no request reports `ExitStatus::None` rather than panicking.
    pub fn get_exit_info(env: Env, provider: Address) -> ExitInfo {
        let request: Option<ExitRequest> = env
            .storage()
            .persistent()
            .get(&StorageKey::ExitReq(provider.clone()));

        match request {
            None => ExitInfo {
                provider,
                status: ExitStatus::None,
                shares: 0,
                requested_at: 0,
                claimable_at: 0,
                seconds_remaining: 0,
            },
            Some(req) => {
                let now = env.ledger().timestamp();
                let claimable = now >= req.claimable_at;
                ExitInfo {
                    provider,
                    status: if claimable {
                        ExitStatus::Claimable
                    } else {
                        ExitStatus::Pending
                    },
                    shares: req.shares,
                    requested_at: req.requested_at,
                    claimable_at: req.claimable_at,
                    seconds_remaining: req.claimable_at.saturating_sub(now),
                }
            }
        }
    }

    /// Total shares reserved by all outstanding exit requests.
    ///
    /// This is the pool's forward view of committed outflow — the number that
    /// makes a queue useful for anything beyond delay.
    pub fn get_queued_exit_shares(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StorageKey::QueuedExitShares)
            .unwrap_or(0)
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

        // Enforce the utilization ceiling. `available` alone only asks whether
        // the pool *can* commit the capital; this asks whether it *should* —
        // a pool committed to the last unit has no buffer left for correlated
        // losses across the policies it already wrote.
        let capacity = Self::capacity(&env);
        if capacity.max_utilization_bps < 10_000 {
            let max_lockable = total_deposited
                .saturating_mul(capacity.max_utilization_bps as i128)
                / 10_000;
            if total_locked.saturating_add(amount) > max_lockable {
                panic_with_error!(&env, Error::UtilizationCapExceeded);
            }
        }

        if env.storage().persistent().has(&StorageKey::Lock(policy_id)) { panic_with_error!(&env, Error::AlreadyLocked); }

        let lock_key = StorageKey::Lock(policy_id);
        env.storage().persistent().set(&lock_key, &CapitalLock {
            policy_id,
            amount,
            locked_at: env.ledger().timestamp(),
            released:  false,
        });
        env.storage().persistent().extend_ttl(&StorageKey::Lock(policy_id), TTL_THRESHOLD, TTL_EXTEND_TO);
        env.storage().instance().set(&StorageKey::TotalLocked, &total_locked.checked_add(amount).unwrap_or_else(|| panic_with_error!(&env, Error::Overflow)));

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

    /// Return the dynamic premium rate (basis points) for a given base rate,
    /// adjusted for the pool's current risk as measured by utilization.
    ///
    /// Premiums are static by default (`InsuranceProduct.premium_rate_bps`),
    /// which means the protocol charges the same rate whether the pool is
    /// nearly empty or almost fully committed. A pool running hot has a thinner
    /// buffer to absorb a correlated payout, so its coverage is objectively
    /// riskier — and should cost more (issue #386).
    ///
    /// The adjustment scales the base rate linearly with utilization: at 0%
    /// utilization the rate is unchanged, and at 100% utilization it doubles
    /// (`rate * (10_000 + utilization_bps) / 10_000`). Utilization is capped at
    /// 10_000 bps so the multiplier never exceeds 2x. The policy engine calls
    /// this on every purchase and uses the result in place of the static rate.
    pub fn get_dynamic_premium_rate(env: Env, base_rate_bps: u32) -> u32 {
        let status = Self::get_capacity_status(env);
        let util = status.utilization_bps.min(10_000);
        let scaled = (base_rate_bps as u128)
            .saturating_mul((10_000u128).saturating_add(util as u128))
            / 10_000u128;
        u32::try_from(scaled).unwrap_or(u32::MAX)
    }

    /// Set dynamic fee adjustment configuration based on market conditions.
    /// Allows pool fees to automatically adjust based on pool utilization.
    ///
    /// Parameters:
    /// - `base_fee_bps`: Base fee in basis points
    /// - `max_fee_bps`: Maximum fee cap in basis points
    /// - `min_fee_bps`: Minimum fee floor in basis points
    /// - `utilization_threshold_bps`: Utilization threshold (in bps) at which fees start increasing
    /// - `fee_adjustment_per_1pct_bps`: Fee increase per 1% utilization above threshold
    /// - `enabled`: Whether dynamic fee adjustment is active
    pub fn set_dynamic_fee_config(
        env: Env,
        admin: Address,
        base_fee_bps: u32,
        max_fee_bps: u32,
        min_fee_bps: u32,
        utilization_threshold_bps: u32,
        fee_adjustment_per_1pct_bps: u32,
        enabled: bool,
    ) {
        Self::require_admin(&env, &admin);

        // Validate thresholds
        if base_fee_bps > 10000 || max_fee_bps > 10000 || min_fee_bps > 10000 {
            panic_with_error!(&env, Error::InvalidParameter);
        }
        if min_fee_bps > base_fee_bps || base_fee_bps > max_fee_bps {
            panic_with_error!(&env, Error::InvalidParameter);
        }
        if utilization_threshold_bps > 10000 {
            panic_with_error!(&env, Error::InvalidParameter);
        }

        let config = DynamicFeeConfig {
            base_fee_bps,
            max_fee_bps,
            min_fee_bps,
            utilization_threshold_bps,
            fee_adjustment_per_1pct_bps,
            enabled,
            last_updated: env.ledger().timestamp(),
        };

        env.storage()
            .instance()
            .set(&StorageKey::DynamicFeeConfig, &config);

        env.events().publish(
            (Symbol::new(&env, "dynamic_fee_config_updated"),),
            DynamicFeeConfigUpdated {
                base_fee_bps,
                max_fee_bps,
                min_fee_bps,
                utilization_threshold_bps,
                fee_adjustment_per_1pct_bps,
                enabled,
            },
        );
    }

    /// Get current dynamic fee configuration.
    pub fn get_dynamic_fee_config(env: Env) -> DynamicFeeConfig {
        env.storage()
            .instance()
            .get(&StorageKey::DynamicFeeConfig)
            .unwrap_or_else(|| DynamicFeeConfig {
                base_fee_bps: 0,
                max_fee_bps: 1000,
                min_fee_bps: 0,
                utilization_threshold_bps: 7000,
                fee_adjustment_per_1pct_bps: 10,
                enabled: false,
                last_updated: 0,
            })
    }

    /// Calculate the current dynamic fee based on pool utilization.
    /// Returns adjusted fee in basis points within the configured min/max bounds.
    pub fn calculate_dynamic_fee(env: Env) -> u32 {
        let config = Self::get_dynamic_fee_config(&env);
        if !config.enabled {
            return config.base_fee_bps;
        }

        let status = Self::get_capacity_status(&env);
        let util_bps = status.utilization_bps;

        // If below threshold, use base fee
        if util_bps <= config.utilization_threshold_bps {
            return config.base_fee_bps;
        }

        // Calculate fee increase based on utilization above threshold
        let util_above_threshold = util_bps.saturating_sub(config.utilization_threshold_bps);
        // Convert basis points (1/100th of 1%) to 1% increments
        let pct_above_threshold = util_above_threshold / 100;
        let fee_increase = (pct_above_threshold as u32).saturating_mul(config.fee_adjustment_per_1pct_bps);

        let adjusted_fee = config.base_fee_bps.saturating_add(fee_increase);
        adjusted_fee.min(config.max_fee_bps).max(config.min_fee_bps)
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
    ///
    /// Issue #341: this and the other admin-only setters below are callable
    /// only by the plain `admin` address today — governance-dao has no path
    /// to call them on the DAO's behalf. Wiring that requires either
    /// pointing `admin` at the DAO contract address (needs the DAO to gain
    /// an "execute arbitrary cross-contract call" ability first) or adding
    /// a parallel `update_premium_split_via_dao` entrypoint gated on a
    /// passed/executed proposal — a real design decision, not made here.
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

    /// Admin-only: propose a time-locked change to premium split ratios.
    /// LPs have `PARAMETER_TIMELOCK_SECONDS` (2 days) to exit before the change takes effect.
    pub fn propose_parameter_change(
        env: Env,
        admin: Address,
        lp_bps: i128,
        treas_bps: i128,
        backstop_bps: i128,
    ) {
        Self::require_admin(&env, &admin);
        if lp_bps < 0 || treas_bps < 0 || backstop_bps < 0 {
            panic_with_error!(&env, Error::InvalidSplit);
        }
        if lp_bps + treas_bps + backstop_bps != 10_000 {
            panic_with_error!(&env, Error::InvalidSplit);
        }
        if env.storage().persistent().has(&StorageKey::PendingParameterChange) {
            panic_with_error!(&env, Error::ParameterChangePending);
        }

        let now = env.ledger().timestamp();
        let executable_after = now + PARAMETER_TIMELOCK_SECONDS;

        let pending = PendingParameterChange {
            new_lp_bps: lp_bps,
            new_treas_bps: treas_bps,
            new_backstop_bps: backstop_bps,
            proposed_at: now,
            executable_after,
            executed: false,
        };
        env.storage().persistent().set(&StorageKey::PendingParameterChange, &pending);
        Self::extend_to_max(&env, &StorageKey::PendingParameterChange);

        env.events().publish(
            (Symbol::new(&env, "parameter_change_scheduled"),),
            ParameterChangeScheduled {
                admin: admin.clone(),
                new_lp_bps: lp_bps,
                new_treas_bps: treas_bps,
                new_backstop_bps: backstop_bps,
                executable_after,
            },
        );
    }

    /// Execute a previously proposed parameter change after the timelock expires.
    pub fn execute_parameter_change(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        let mut pending: PendingParameterChange = env.storage().persistent()
            .get(&StorageKey::PendingParameterChange)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoPendingParameterChange));

        if pending.executed {
            panic_with_error!(&env, Error::AlreadyReleased);
        }

        let now = env.ledger().timestamp();
        if now < pending.executable_after {
            panic_with_error!(&env, Error::ParameterChangeNotReady);
        }

        // Apply the parameter change
        let split = PremiumSplit {
            lp_bps: pending.new_lp_bps,
            treas_bps: pending.new_treas_bps,
            backstop_bps: pending.new_backstop_bps,
        };
        env.storage().instance().set(&StorageKey::PremiumSplit, &split);

        // Mark as executed
        pending.executed = true;
        env.storage().persistent().set(&StorageKey::PendingParameterChange, &pending);

        env.events().publish(
            (Symbol::new(&env, "parameter_change_executed"),),
            ParameterChangeExecuted {
                admin: admin.clone(),
                lp_bps: pending.new_lp_bps,
                treas_bps: pending.new_treas_bps,
                backstop_bps: pending.new_backstop_bps,
            },
        );
    }

    /// Cancel a pending parameter change.
    pub fn cancel_parameter_change(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        if !env.storage().persistent().has(&StorageKey::PendingParameterChange) {
            panic_with_error!(&env, Error::NoPendingParameterChange);
        }
        env.storage().persistent().remove(&StorageKey::PendingParameterChange);

        env.events().publish(
            (Symbol::new(&env, "parameter_change_cancelled"),),
            ParameterChangeCancelled { admin: admin.clone() },
        );
    }

    /// Get the pending parameter change, if any.
    pub fn get_pending_parameter_change(env: Env) -> Option<PendingParameterChange> {
        env.storage().persistent().get(&StorageKey::PendingParameterChange)
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

    /// Ledger timestamp at which the current pending admin transfer was
    /// registered, or `0` if none. `accept_admin` succeeds only once
    /// `now >= this + ADMIN_TRANSFER_TIMELOCK` (issue #356).
    pub fn get_pending_admin_since(env: Env) -> u64 {
        env.storage().instance().get(&StorageKey::PendingAdminSince).unwrap_or(0)
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

    /// Accept the proposed admin. Only the proposed admin can call this, and
    /// only once `ADMIN_TRANSFER_TIMELOCK` has elapsed since the transfer was
    /// registered (issue #356).
    pub fn accept_admin(env: Env, admin: Address) {
        let pending_admin: Address = env.storage().instance()
            .get(&StorageKey::PendingAdmin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::Unauthorized));
        // Only the pending admin can accept
        if admin != pending_admin {
            panic_with_error!(&env, Error::Unauthorized);
        }
        admin.require_auth();
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
    /// Configured capacity limits, or the built-in defaults.
    ///
    /// The default `max_total_deposited` is the historical `MAX_TOTAL_DEPOSITED`
    /// constant and the default utilization ceiling is 100%, so an existing
    /// pool behaves exactly as before until an admin sets a limit.
    fn capacity(env: &Env) -> PoolCapacity {
        env.storage()
            .instance()
            .get(&StorageKey::Capacity)
            .unwrap_or(PoolCapacity {
                max_total_deposited: MAX_TOTAL_DEPOSITED,
                max_utilization_bps: DEFAULT_MAX_UTILIZATION_BPS,
            })
    }

    /// Configured withdrawal delay in seconds (default 0 — instant).
    fn exit_delay(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&StorageKey::ExitDelay)
            .unwrap_or(0)
    }

    /// `total_locked / total_deposited` in basis points. An empty pool is 0%
    /// utilized rather than a division by zero.
    fn utilization_bps(total_locked: i128, total_deposited: i128) -> u32 {
        if total_deposited <= 0 {
            return 0;
        }
        let bps = total_locked.saturating_mul(10_000) / total_deposited;
        bps.clamp(0, 10_000) as u32
    }

    /// Remove a provider's exit request and release its share reservation.
    fn clear_exit_reservation(env: &Env, provider: &Address, shares: i128) {
        env.storage()
            .persistent()
            .remove(&StorageKey::ExitReq(provider.clone()));

        let queued: i128 = env
            .storage()
            .instance()
            .get(&StorageKey::QueuedExitShares)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&StorageKey::QueuedExitShares, &queued.saturating_sub(shares).max(0));
    }

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

    // ── LP NFT (Soulbound Token) ─────────────────────────────────────────────

    /// Toggle compound yield on/off for the caller's LP position.
    /// When enabled, accrued yield is reinvested into additional shares instead
    /// of being paid out as USDC on deposit/claim.
    pub fn toggle_compound(env: Env, provider: Address, enabled: bool) {
        provider.require_auth();
        let lp_key = StorageKey::LpPosition(provider.clone());
        let mut position: LpPosition = env.storage().persistent()
            .get(&lp_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoShares));
        position.compound_enabled = enabled;
        env.storage().persistent().set(&lp_key, &position);
        Self::extend_to_max(&env, &lp_key);

        env.events().publish(
            (Symbol::new(&env, "compound_yield_toggled"),),
            CompoundYieldToggled {
                provider,
                enabled,
            },
        );
    }

    /// Return the LP NFT for a given token ID, if it exists.
    pub fn get_lp_nft(env: Env, token_id: u64) -> Option<LpNft> {
        env.storage().persistent().get(&StorageKey::LpNft(token_id))
    }

    /// Return the LP NFT token ID for a given provider address, if they have one.
    pub fn get_provider_nft(env: Env, provider: Address) -> Option<u64> {
        env.storage().persistent().get(&StorageKey::ProviderNft(provider))
    }

    /// Return the next LP NFT token ID that will be minted.
    pub fn get_next_nft_id(env: Env) -> u64 {
        env.storage().instance().get(&StorageKey::NextNftId).unwrap_or(1)
    }

    /// Mint a soulbound LP NFT for a new provider. Called internally on first deposit.
    fn mint_lp_nft(
        env: &Env,
        provider: &Address,
        category: &Symbol,
        minted_at: u64,
        position: &LpPosition,
    ) {
        let token_id: u64 = env.storage().instance()
            .get(&StorageKey::NextNftId).unwrap_or(1);
        env.storage().instance().set(&StorageKey::NextNftId, &(token_id + 1));

        let nft = LpNft {
            token_id,
            provider: provider.clone(),
            category: category.clone(),
            minted_at,
            shares: position.shares,
            deposited: position.deposited,
            active: true,
        };
        env.storage().persistent().set(&StorageKey::LpNft(token_id), &nft);
        Self::extend_to_max(env, &StorageKey::LpNft(token_id));
        env.storage().persistent().set(&StorageKey::ProviderNft(provider.clone()), &token_id);
        Self::extend_to_max(env, &StorageKey::ProviderNft(provider.clone()));

        env.events().publish(
            (Symbol::new(env, "lp_nft_minted"),),
            LpNftMinted {
                token_id,
                provider: provider.clone(),
                category: category.clone(),
                minted_at,
            },
        );
    }

    /// Update an existing LP NFT after deposit/withdraw. Called internally.
    fn update_lp_nft(env: &Env, provider: &Address, position: &LpPosition) {
        let token_id: u64 = match env.storage().persistent().get::<_, u64>(&StorageKey::ProviderNft(provider.clone())) {
            Some(id) => id,
            None => return, // No NFT yet (shouldn't happen after mint)
        };
        let mut nft: LpNft = match env.storage().persistent().get(&StorageKey::LpNft(token_id)) {
            Some(n) => n,
            None => return,
        };
        nft.shares = position.shares;
        nft.deposited = position.deposited;
        nft.active = position.shares > 0;
        env.storage().persistent().set(&StorageKey::LpNft(token_id), &nft);

        env.events().publish(
            (Symbol::new(env, "lp_nft_updated"),),
            LpNftUpdated {
                token_id,
                provider: provider.clone(),
                shares: position.shares,
                deposited: position.deposited,
                active: nft.active,
            },
        );
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
