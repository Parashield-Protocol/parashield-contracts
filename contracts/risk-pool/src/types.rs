use soroban_sdk::{contracttype, Address, Symbol, Vec};

/// Status of a risk pool.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PoolStatus {
    Active,
    Paused,
    /// No new deposits; existing LPs can withdraw
    WindingDown,
}

/// A liquidity provider's position in the pool.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LpPosition {
    pub provider:          Address,
    /// Amount of USDC deposited (7-decimal stroops)
    pub deposited:         i128,
    /// Pool-share tokens held (7-decimal, proportional to ownership)
    pub shares:            i128,
    /// Total accumulated premium yield already claimed by this LP
    pub yield_claimed:     i128,
    pub deposited_at:      u64,
    pub last_yield_claim:  u64,
}

/// A capital lock placed on the pool when a policy is active.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapitalLock {
    pub policy_id:   u128,
    pub amount:      i128,
    pub locked_at:   u64,
    pub released:    bool,
}

/// Aggregate pool stats exposed via queries.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolStats {
    /// Category: "crop" | "flight" | "disaster" | "defi"
    pub category:        Symbol,
    pub total_deposited: i128,
    pub total_locked:    i128,
    pub total_shares:    i128,
    pub accumulated_premium: i128,
    pub status:          PoolStatus,
}

// ─── Events ──────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Initialized {
    pub admin: Address,
    pub usdc_token: Address,
    pub treasury: Address,
    pub category: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidityDeposited {
    pub provider: Address,
    pub amount: i128,
    pub shares_minted: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidityWithdrawn {
    pub provider: Address,
    pub shares_burned: i128,
    pub amount_returned: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PremiumDistributed {
    pub amount: i128,
    pub lp_share: i128,
    pub treasury_share: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldClaimed {
    pub provider: Address,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapitalLocked {
    pub policy_id: u128,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapitalReleased {
    pub policy_id: u128,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolPaused {
    pub admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolResumed {
    pub admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaginatedLps {
    pub lps: Vec<Address>,
    pub total_count: u32,
}


