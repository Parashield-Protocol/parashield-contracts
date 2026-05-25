use soroban_sdk::{contracttype, Address, Symbol};

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
