use soroban_sdk::{contracttype, Address, Bytes, BytesN, Symbol, Val, Vec};

// Issue #339: this is a compile-time constant with no runtime override.
// Making it configurable means adding a `finalize_delay: u64` field to
// `DaoConfig` below, which is a required field on every construction site
// (lib.rs `initialize`/`update_config`, plus test.rs and test_advanced.rs) —
// left as a follow-up so that migration can be done deliberately in one
// pass rather than partially here.
pub const FINALIZE_DELAY: u64 = 24 * 3600;

/// Approximate Stellar ledger close time in seconds, used to convert
/// wall-clock TTL windows into ledger counts for `extend_ttl`.
pub const LEDGER_SECONDS: u64 = 5;

/// Extra buffer (beyond voting period + finalize delay + timelock) added to
/// proposal/vote/locked-balance TTLs so `withdraw_tokens`/`execute` still
/// have time to run after the timelock expires (issue #185).
pub const GOVERNANCE_TTL_BUFFER_SECONDS: u64 = 30 * 24 * 3600;

/// Current lifecycle state of a governance proposal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProposalStatus {
    /// In mandatory discussion period before voting opens.
    Discussion,
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
    /// Passed but execution deadline expired without execution
    Expired,
}

/// On-chain comment on a proposal for discussion and feedback.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalComment {
    pub id: u128,
    pub proposal_id: u64,
    pub author: Address,
    pub text: Bytes,
    pub created_at: u64,
    /// Optional: ID of the comment this replies to, for threaded discussion
    pub reply_to: Option<u128>,
}

/// Vote direction cast by a token holder.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VoteChoice {
    For,
    Against,
    Abstain,
}

/// What kind of action a proposal performs on execution.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProposalKind {
    /// Arbitrary `target::function(args)` call, as authored by the proposer.
    Standard,
    /// Upgrades `target`'s contract WASM. `target` must have this DAO
    /// configured as its admin for execution to succeed.
    Upgrade,
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
    /// gov_token amount actually locked from the proposer at creation.
    /// Refunded verbatim at finalize() — never re-read from the live
    /// DaoConfig, since config.proposal_threshold can change between
    /// proposal creation and finalization.
    pub deposit: i128,
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
    /// Timestamp after which a passed proposal can no longer be executed.
    /// Defaults to vote_end + finalize_delay + 7 days. Prevents stale proposals from executing.
    pub execution_deadline: u64,
    /// Total supply captured at proposal creation time for quorum calculation.
    /// This prevents admin manipulation of total_supply during active votes.
    pub total_supply: i128,
    /// Whether this is a generic call or a contract-upgrade proposal.
    pub kind: ProposalKind,
    /// Mandatory impact analysis describing potential consequences of this proposal.
    /// Max 4096 bytes to provide comprehensive risk assessment.
    pub impact_analysis: Bytes,
    /// Optional verification callback function on the target contract to confirm
    /// execution produced the intended state change. Called as `target::verify_proposal_execution(proposal_id)`.
    /// If specified and fails, execution is marked as failed with audit trail.
    /// Signature: fn verify_proposal_execution(env: Env, proposal_id: u64) -> Result<bool, Symbol>
    pub verification_callback: Option<Symbol>,
    /// Whether execution has been verified (callback succeeded or not required).
    pub execution_verified: bool,
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
    /// Mandatory discussion period in seconds before voting opens.
    /// Set to 0 to disable (proposals go straight to Active).
    pub discussion_period: u64,
    /// Maximum voting weight any single address may cast per proposal (7-decimal).
    /// 0 = no cap (unlimited whale voting). Capping prevents a single large
    /// holder from dominating governance outcomes.
    pub vote_weight_cap: i128,
}

/// Settings controlling adaptive (decaying) quorum.
///
/// A static quorum assumes participation is stable. It is not: token holders
/// drift away, delegates go quiet, and a DAO that was healthy at 20% turnout
/// can spend months unable to pass anything — including the proposals that
/// would fix its own participation problem. Governance deadlock is itself a
/// failure mode.
///
/// Decay lowers the bar as participation falls, but only down to `floor_bps`,
/// which can never go below the contract's hard `MIN_QUORUM_BPS`. The
/// intent is to keep a quiet DAO governable, not to let a handful of holders
/// quietly take it over.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuorumDecayConfig {
    /// Whether adaptive quorum is applied at all. Off by default, so an
    /// existing DAO's behaviour does not change until it opts in.
    pub enabled: bool,
    /// Absolute lower bound the decayed quorum can reach, in basis points.
    /// Clamped up to the contract's `MIN_QUORUM_BPS` if set below it.
    pub floor_bps: u32,
    /// Basis points of decay applied per consecutive low-participation
    /// proposal, subtracted from the configured `quorum_bps`.
    pub decay_per_period_bps: u32,
    /// Number of recent proposals whose turnout is averaged to decide whether
    /// participation counts as low.
    pub window: u32,
}

/// Rolling record of recent participation, used to compute adaptive quorum.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParticipationHistory {
    /// Turnout in basis points for each of the last `window` finalized
    /// proposals, oldest first.
    pub recent_bps: Vec<u32>,
    /// Number of finalized proposals recorded so far (saturating).
    pub total_recorded: u64,
}

/// The quorum actually applied to a proposal, and why.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveQuorum {
    /// The quorum requirement in effect, in basis points.
    pub quorum_bps: u32,
    /// The configured, undecayed requirement.
    pub base_bps: u32,
    /// Average turnout across the participation window, in basis points.
    pub avg_participation_bps: u32,
    /// True when decay actually lowered the requirement below `base_bps`.
    pub decayed: bool,
}

/// A standing delegation of voting power from one holder to another.
///
/// Delegation transfers *authority*, never tokens. The delegator keeps custody
/// throughout — the DAO simply counts their balance toward the delegate's vote
/// at the moment that vote is cast.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delegation {
    pub delegator: Address,
    pub delegate: Address,
    pub delegated_at: u64,
}

/// A delegate's standing, for display and for pre-vote checks.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegateInfo {
    pub delegate: Address,
    /// Addresses currently delegating to this account.
    pub delegators: Vec<Address>,
    /// Sum of those delegators' live token balances.
    pub delegated_weight: i128,
    /// The delegate's own balance.
    pub own_weight: i128,
    /// `own_weight + delegated_weight` — what a vote right now would carry.
    pub total_weight: i128,
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
pub struct ExecutionAuditRecord {
    pub proposal_id: u64,
    pub executor: Address,
    pub target: Address,
    pub function: Symbol,
    pub executed_at: u64,
    pub votes_for: i128,
    pub votes_against: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalExecuted {
    pub proposal_id: u64,
    pub executor: Address,
    pub target: Address,
    pub function: Symbol,
    pub executed_at: u64,
}


#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalCancelled {
    pub proposal_id: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalVetoed {
    pub proposal_id: u64,
    pub guardian: Address,
    pub reason: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscussionPeriodEnded {
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

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VotingPowerDelegated {
    pub delegator: Address,
    pub delegate: Address,
    pub weight: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationRevoked {
    pub delegator: Address,
    pub delegate: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuorumDecayConfigUpdated {
    pub enabled: bool,
    pub floor_bps: u32,
    pub decay_per_period_bps: u32,
    pub window: u32,
}

/// Emitted at finalize when the applied quorum differed from the configured one.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuorumDecayApplied {
    pub proposal_id: u64,
    pub base_bps: u32,
    pub effective_bps: u32,
    pub avg_participation_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractUpgraded {
    pub old_version: u32,
    pub new_version: u32,
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

/// Emitted when a proposer (or anyone on their behalf) reclaims a deposit
/// via `reclaim_deposit` on a proposal nobody ever finalized.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepositReclaimed {
    pub proposal_id: u64,
    pub proposer: Address,
    pub amount: i128,
}

/// A registered structural template for a common proposal type.
///
/// Standardizes the minimum shape a proposal of this kind must have —
/// title length and argument count — so proposals created against it are
/// consistent and reviewers spend less time parsing intent out of
/// free-form submissions.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalTemplate {
    pub name: Symbol,
    /// Human-readable description of what this template is for.
    pub description: Bytes,
    /// Minimum number of bytes required in a proposal's `title`.
    pub min_title_len: u32,
    /// Exact number of entries required in a proposal's `args`.
    pub required_arg_count: u32,
    /// Whether new proposals may still be created from this template.
    pub active: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateRegistered {
    pub name: Symbol,
    pub min_title_len: u32,
    pub required_arg_count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalCreatedFromTemplate {
    pub proposal_id: u64,
    pub template_name: Symbol,
}

/// Emitted when the vote weight cap is updated via DAO config.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoteWeightCapUpdated {
    pub vote_weight_cap: i128,
}

/// Emitted when a delegation's weight is used in a vote.
///
/// Tracks the moment delegated voting power is actually exercised,
/// making it possible to see which delegations contributed to which
/// proposals and how much weight they carried.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationUsed {
    pub proposal_id: u64,
    pub delegate: Address,
    pub delegator: Address,
    pub weight: i128,
}

/// Emitted when a delegation is created or revoked, recording the
/// full delegation graph at a point in time for off-chain indexing.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationRecorded {
    pub delegator: Address,
    pub delegate: Address,
    pub action: Symbol,
    pub recorded_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionVerified {
    pub proposal_id: u64,
    pub executor: Address,
    pub target: Address,
    pub verification_callback: Symbol,
    pub verified_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionVerificationFailed {
    pub proposal_id: u64,
    pub executor: Address,
    pub target: Address,
    pub callback: Symbol,
    pub error: Symbol,
}

/// Emitted when a comment is posted on a proposal for discussion.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalCommentAdded {
    pub proposal_id: u64,
    pub comment_id: u128,
    pub author: Address,
    pub reply_to: Option<u128>,
    pub created_at: u64,
}
