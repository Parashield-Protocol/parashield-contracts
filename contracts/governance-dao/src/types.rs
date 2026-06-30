use soroban_sdk::{contracttype, Address, Bytes, Symbol, Val, Vec};

/// Current lifecycle state of a governance proposal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProposalStatus {
    /// Accepting votes
    Active,
    /// Passed quorum + majority; ready to execute
    Passed,
    /// Failed to reach quorum or majority
    Failed,
    /// Execution was called and succeeded
    Executed,
    /// Cancelled by admin before vote close
    Cancelled,
}

/// Vote direction cast by a token holder.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VoteChoice {
    For,
    Against,
    Abstain,
}

/// A governance proposal for a protocol parameter change.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub id: u64,
    pub proposer: Address,
    /// Short human-readable title (max 256 bytes).
    pub title: Bytes,
    /// Target contract address that will be called on execution.
    pub target: Address,
    /// Function name to invoke on execution (max 9 chars — Soroban Symbol).
    pub function: Symbol,
    pub args: Vec<Val>,
    pub status: ProposalStatus,
    pub votes_for: i128,
    pub votes_against: i128,
    pub votes_abstain: i128,
    /// Ledger timestamp when voting opens.
    pub created_at: u64,
    /// Ledger timestamp when voting closes.
    pub vote_end: u64,
    /// Timelock expiration timestamp for execution.
    pub execution_time: u64,
}

/// A single vote record stored per (proposal_id, voter) key.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoteRecord {
    pub voter: Address,
    pub choice: VoteChoice,
    /// Token weight at the time of voting.
    pub weight: i128,
}

/// DAO configuration set at initialization.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaoConfig {
    /// Governance token address (balance = voting weight).
    pub gov_token: Address,
    /// Minimum tokens needed to create a proposal (7-decimal).
    pub proposal_threshold: i128,
    pub total_supply: i128,
    /// Minimum % of total supply that must vote (basis points, e.g. 1000 = 10%).
    pub quorum_bps: u32,
    /// Minimum % of cast votes that must be FOR (basis points, e.g. 5100 = 51%).
    pub majority_bps: u32,
    /// Voting period in seconds.
    pub voting_period: u64,
    /// Timelock period in seconds before an approved proposal can be executed.
    pub proposal_timelock: u64,
}

// ─── Events ──────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Initialized {
    pub admin: Address,
    pub gov_token: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalCreated {
    pub proposal_id: u64,
    pub proposer: Address,
    pub target: Address,
    pub function: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoteCast {
    pub proposal_id: u64,
    pub voter: Address,
    pub choice: VoteChoice,
    pub weight: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalFinalized {
    pub proposal_id: u64,
    pub status: ProposalStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalExecuted {
    pub proposal_id: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalCancelled {
    pub proposal_id: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaoConfigUpdated {
    pub gov_token: Address,
    pub proposal_threshold: i128,
    pub total_supply: i128,
    pub voting_period: u64,
    pub proposal_timelock: u64,
}
