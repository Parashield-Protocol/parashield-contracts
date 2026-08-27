use soroban_sdk::{contracttype, Address, BytesN, Symbol, Vec};

/// Paginated result from `get_lp_list`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaginatedLps {
    pub lps:         Vec<Address>,
    pub total_count: u32,
}

/// Status of a risk pool.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PoolStatus {
    Active,
    Paused,
    /// No new deposits; existing LPs can withdraw
    WindingDown,
}

/// Admin-adjustable premium split ratios, in basis points, summing to 10,000.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PremiumSplit {
    pub lp_bps:       i128,
    pub treas_bps:    i128,
    pub backstop_bps: i128,
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
    pub yield_debt:        i128,
    pub deposited_at:      u64,
    pub last_yield_claim:  u64,
    /// Whether this LP has opted into compound yield (reinvest instead of claim).
    pub compound_enabled:  bool,
}

/// A soulbound NFT representing an LP's position in the pool.
/// Non-transferable by design — only the provider can interact with it.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LpNft {
    /// Unique token ID (sequential, minted on first deposit).
    pub token_id:    u64,
    /// The LP provider address.
    pub provider:    Address,
    /// Pool category this NFT represents.
    pub category:    Symbol,
    /// Timestamp when the NFT was minted (first deposit).
    pub minted_at:   u64,
    /// Current share count held by this position.
    pub shares:      i128,
    /// Total deposited amount.
    pub deposited:   i128,
    /// Whether the position is still active (shares > 0).
    pub active:      bool,
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
    pub category:             Symbol,
    pub total_deposited:      i128,
    pub total_locked:         i128,
    pub total_shares:         i128,
    pub accumulated_premium:  i128,
    pub accumulated_backstop: i128,
    pub status:               PoolStatus,
}

// ─── Events ──────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Initialized {
    pub admin:             Address,
    pub usdc_token:        Address,
    pub treasury:          Address,
    pub backstop:          Address,
    pub category:          Symbol,
    pub policy_engine:     Address,
    pub claims_processor:  Address,
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
    pub backstop_share: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreasuryFunded {
    pub amount: i128,
    pub recipient: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackstopFunded {
    pub amount: i128,
    pub recipient: Address,
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

/// A timelocked admin withdrawal request.
/// Once created, the admin must wait until `execute_after` before executing.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminWithdrawalRequest {
    pub amount: i128,
    pub requested_at: u64,
    /// Absolute timestamp the request becomes executable, fixed when the
    /// request is made.
    ///
    /// The deadline is stored rather than recomputed from `requested_at +
    /// TIMELOCK_SECONDS` at execution time, so that upgrading the contract
    /// with a shorter `TIMELOCK_SECONDS` cannot retroactively shorten the
    /// wait on a request that is already in flight — the guarantee LPs relied
    /// on when the request was published.
    pub execute_after: u64,
    pub executed: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminWithdrawalScheduled {
    pub admin: Address,
    pub amount: i128,
    pub execute_after: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminWithdrawalExecuted {
    pub admin: Address,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminWithdrawalCancelled {
    pub admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminUpdated {
    pub new_admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractUpgraded {
    pub old_version: u32,
    pub new_version: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PremiumSplitUpdated {
    pub lp_bps:       i128,
    pub treas_bps:    i128,
    pub backstop_bps: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolWindingDown {
    pub admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardiansUpdated {
    pub guardians: Vec<Address>,
    pub threshold: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeApproved {
    pub new_wasm_hash: BytesN<32>,
    pub approver: Address,
    pub approvals: u32,
    pub threshold: u32,
}

/// A pending contract-upgrade action awaiting guardian approvals.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingUpgrade {
    pub new_wasm_hash: BytesN<32>,
    pub new_version: u32,
    pub approvals: Vec<Address>,
}

/// A pending admin-transfer proposal awaiting guardian approvals.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingAdminChange {
    pub new_admin: Address,
    pub approvals: Vec<Address>,
}

/// A time-locked change to the premium-split ratios, awaiting execution once
/// `executable_after` has passed. LPs can exit during the timelock window.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingParameterChange {
    pub new_lp_bps: i128,
    pub new_treas_bps: i128,
    pub new_backstop_bps: i128,
    pub proposed_at: u64,
    pub executable_after: u64,
    pub executed: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParameterChangeScheduled {
    pub admin: Address,
    pub new_lp_bps: i128,
    pub new_treas_bps: i128,
    pub new_backstop_bps: i128,
    pub executable_after: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParameterChangeExecuted {
    pub admin: Address,
    pub lp_bps: i128,
    pub treas_bps: i128,
    pub backstop_bps: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParameterChangeCancelled {
    pub admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LpNftMinted {
    pub token_id:  u64,
    pub provider:  Address,
    pub category:  Symbol,
    pub minted_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LpNftUpdated {
    pub token_id:  u64,
    pub provider:  Address,
    pub shares:    i128,
    pub deposited: i128,
    pub active:    bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompoundYieldToggled {
    pub provider:  Address,
    pub enabled:   bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinReserveUpdated {
    pub min_reserve: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReserveFundUpdated {
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VotesDelegated {
    pub provider: Address,
    pub delegate: Address,
}
