use soroban_sdk::{contracttype, Address, Bytes, Symbol};

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
    pub id:           u64,
    pub proposer:     Address,
    /// Short human-readable title (max 256 bytes).
    pub title:        Bytes,
    /// Target contract address that will be called on execution.
    pub target:       Address,
    /// Function name to invoke on execution (max 9 chars — Soroban Symbol).
    pub function:     Symbol,
    pub status:       ProposalStatus,
    pub votes_for:    i128,
    pub votes_against: i128,
    pub votes_abstain: i128,
    /// Ledger timestamp when voting opens.
    pub created_at:   u64,
    /// Ledger timestamp when voting closes.
    pub vote_end:     u64,
}

/// A single vote record stored per (proposal_id, voter) key.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoteRecord {
    pub voter:  Address,
    pub choice: VoteChoice,
    /// Token weight at the time of voting.
    pub weight: i128,
}

/// DAO configuration set at initialization.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaoConfig {
    /// Governance token address (balance = voting weight).
    pub gov_token:        Address,
    /// Minimum tokens needed to create a proposal (7-decimal).
    pub proposal_threshold: i128,
    pub total_supply:     i128,
    /// Minimum % of total supply that must vote (basis points, e.g. 1000 = 10%).
    pub quorum_bps:       u32,
    /// Minimum % of cast votes that must be FOR (basis points, e.g. 5100 = 51%).
    pub majority_bps:     u32,
    /// Voting period in seconds.
    pub voting_period:    u64,
}
