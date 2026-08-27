//! Parashield Governance DAO
//!
//! Token-weighted governance over protocol parameters:
//!   - Add/remove insurance products
//!   - Adjust premium rates and trigger thresholds
//!   - Add/remove oracle sources
//!   - Spend protocol treasury
//!   - Emergency pause / upgrade contracts
//!
//! Governance token: SHIELD (Stellar asset, tradeable on built-in DEX)
//! Proposal lifecycle: Draft → Active → Passed/Rejected → Executed
//! Quorum: configurable % of total supply; configurable majority to pass
//!
//! v2 — full implementation; DAO is now deployable and testable.
#![no_std]
extern crate alloc;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, token, Address, Bytes,
    BytesN, Env, IntoVal, Symbol, Val, Vec,
};

pub mod types;
pub use types::*;

/// Minimum voting period: 1 hour in seconds.
const MIN_VOTING_PERIOD: u64 = 3_600;
/// Maximum voting period: 30 days in seconds. Prevents an admin from setting
/// an unreachably large period that would cause vote_end + FINALIZE_DELAY to
/// overflow or make proposals permanently unresolvable.
const MAX_VOTING_PERIOD: u64 = 30 * 24 * 3_600;
/// Storage TTL threshold for proposal-related entries
// Issue #342: kept in sync by hand across all 5 contracts (governance-dao,
// risk-pool, policy-engine, oracle-verifier, claims-processor) — extracting
// to a shared crate is a real follow-up, not done here to avoid touching
// every contract's Cargo.toml in one pass.
const TTL_THRESHOLD: u32 = 518_400; // ~30 days
/// Storage TTL extension target for proposal-related entries
const TTL_EXTEND_TO: u32 = 6_312_000; // ~1 year
/// Minimum delay after vote_end before finalize() can be called
const FINALIZE_DELAY: u64 = 300; // 5 minutes
/// How long a proposal must sit unfinalized past `vote_end` before its
/// proposer (or anyone, on the proposer's behalf) can reclaim the deposit
/// directly via `reclaim_deposit`, bypassing the quorum/majority computation
/// `finalize()` performs. `finalize()` is already permissionless and already
/// refunds the deposit on every outcome, but nothing obliges anyone to ever
/// call it — a proposal that clearly failed quorum has no one economically
/// incentivized to pay for finalizing it. This is the backstop so a deposit
/// can never depend on that goodwill (issue #378). Set well beyond
/// `FINALIZE_DELAY` so the normal finalize path always gets first chance.
const DEPOSIT_RECLAIM_TIMEOUT: u64 = 14 * 24 * 3_600; // 14 days
/// Lower bound on `DaoConfig.quorum_bps` (issue #355). Without a floor the
/// admin could configure `quorum_bps = 0`, letting a proposal pass on
/// negligible turnout. 1000 = 10% of total supply must participate.
const MIN_QUORUM_BPS: u32 = 1_000;
/// Number of recent proposals averaged for participation when adaptive quorum
/// is enabled without an explicit window.
const DEFAULT_PARTICIPATION_WINDOW: u32 = 5;
/// Upper bound on the participation window. Bounds both the stored history and
/// the averaging loop.
const MAX_PARTICIPATION_WINDOW: u32 = 20;

#[contracttype]
enum StorageKey {
    Initialized,
    Admin,
    Config,
    NextProposalId,
    Proposal(u64),
    VoteRecord(u64, Address),
    LockedBalance(u64, Address),
    /// Contract version (u32) for storage migration tracking
    Version,
    /// Guardian addresses authorized to approve critical actions (Vec<Address>).
    Guardians,
    /// Number of guardian approvals required to execute a critical action
    /// (u32). 0 means guardian multisig is disabled (admin acts alone).
    GuardianThreshold,
    /// A pending, not-yet-executed contract upgrade awaiting guardian approvals.
    PendingUpgrade,
    /// Adaptive-quorum settings (`QuorumDecayConfig`).
    QuorumDecay,
    /// Rolling turnout history used to compute adaptive quorum.
    Participation,
    /// A registered proposal template (`ProposalTemplate`) by name.
    Template(Symbol),
    /// Names of all registered templates (`Vec<Symbol>`).
    TemplateList,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InsufficientWeight = 4,
    ProposalNotFound = 5,
    ProposalNotActive = 6,
    AlreadyVoted = 7,
    VotingClosed = 8,
    VotingStillOpen = 9,
    ProposalNotPassed = 10,
    AlreadyExecuted = 11,
    AlreadyCancelled = 12,
    TimelockNotExpired = 13,
    FinalizeDelayNotMet = 14,
    VersionNotNewer = 15,
    LimitReached = 16,
    VotingPeriodTooShort = 17,
    VotingPeriodTooLong = 18,
    InvalidAddress = 19,
    NotGuardian = 20,
    AlreadyApprovedAction = 21,
    NoPendingUpgrade = 22,
    InvalidThreshold = 23,
    QuorumTooLow = 24,
    InvalidQuorumDecay = 25,
    ProposalNotExpired = 26,
    TemplateNotFound = 27,
    TemplateInactive = 28,
    TitleTooShort = 29,
    ArgCountMismatch = 30,
}

#[contract]
pub struct GovernanceDao;

#[contractimpl]
impl GovernanceDao {
    /// Initialize the DAO. Can only be called once.
    ///
    /// Stores `admin` and `config` (gov token, quorum/majority thresholds,
    /// voting period, timelock) and sets the proposal counter to 0.
    pub fn initialize(env: Env, admin: Address, config: DaoConfig) {
        if env.storage().instance().has(&StorageKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }

        // Address verification
        let admin_str = admin.to_string();
        
        if admin_str.len() != 56 {
            panic_with_error!(&env, Error::InvalidAddress);
        }
        let mut admin_buf = [0u8; 56];
        admin_str.copy_into_slice(&mut admin_buf);
        if admin_buf[0] != b'G' && admin_buf[0] != b'C' {
            panic_with_error!(&env, Error::InvalidAddress);
        }

        let gov_token_str = config.gov_token.to_string();

        if gov_token_str.len() != 56 {
            panic_with_error!(&env, Error::InvalidAddress);
        }
        let mut gov_token_buf = [0u8; 56];
        gov_token_str.copy_into_slice(&mut gov_token_buf);
        if gov_token_buf[0] != b'C' {
            panic_with_error!(&env, Error::InvalidAddress);
        }

        env.storage()
            .instance()
            .set(&StorageKey::Initialized, &true);
        if config.voting_period < MIN_VOTING_PERIOD {
            panic_with_error!(&env, Error::VotingPeriodTooShort);
        }
        if config.voting_period > MAX_VOTING_PERIOD {
            panic_with_error!(&env, Error::VotingPeriodTooLong);
        }
        if config.quorum_bps < MIN_QUORUM_BPS {
            panic_with_error!(&env, Error::QuorumTooLow);
        }
        env.storage().instance().set(&StorageKey::Admin, &admin);
        env.storage().instance().set(&StorageKey::Config, &config);
        env.storage()
            .instance()
            .set(&StorageKey::NextProposalId, &0u64);

        env.events().publish(
            (Symbol::new(&env, "initialized"),),
            Initialized {
                admin: admin.clone(),
                gov_token: config.gov_token.clone(),
            },
        );
    }

    // ── Proposals ─────────────────────────────────────────────────────────────

    /// Create a new governance proposal targeting `target::function(args)`.
    ///
    /// The proposer must hold at least `config.proposal_threshold` gov
    /// tokens; that exact amount is locked (transferred into the DAO) as a
    /// deposit and is refunded verbatim by `finalize()`, regardless of any
    /// later change to `config.proposal_threshold`.
    pub fn create_proposal(
        env: Env,
        proposer: Address,
        title: Bytes,
        target: Address,
        function: Symbol,
        args: Vec<Val>, // <--- ADD THIS ARGUMENT
    ) -> u64 {
        proposer.require_auth();
        Self::validate_stellar_address(&env, &target);
        let config: DaoConfig = env
            .storage()
            .instance()
            .get(&StorageKey::Config)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        let gov_token = token::Client::new(&env, &config.gov_token);
        let weight = gov_token.balance(&proposer);
        if weight < config.proposal_threshold {
            panic_with_error!(&env, Error::InsufficientWeight);
        }
        // Lock the threshold as it stands right now; this exact amount
        // (not whatever config.proposal_threshold reads as later) is what
        // finalize() must refund, so it's captured on the Proposal below.
        let deposit = config.proposal_threshold;
        gov_token.transfer(
            &proposer,
            &env.current_contract_address(),
            &deposit,
        );

        let proposal_id: u64 = env
            .storage()
            .instance()
            .get(&StorageKey::NextProposalId)
            .unwrap_or(0);
        let now = env.ledger().timestamp();

        let proposal = Proposal {
            id: proposal_id,
            proposer: proposer.clone(),
            title,
            target: target.clone(),
            function: function.clone(),
            args, // <--- BIND TO STRUCT
            deposit,
            status: ProposalStatus::Active,
            votes_for: 0,
            votes_against: 0,
            votes_abstain: 0,
            created_at: now,
            vote_end: now.saturating_add(config.voting_period),
            execution_time: 0,
            total_supply: config.total_supply,
            kind: ProposalKind::Standard,
        };

        let proposal_key = StorageKey::Proposal(proposal_id);
        env.storage().persistent().set(&proposal_key, &proposal);
        env.storage()
            .instance()
            .set(&StorageKey::NextProposalId, &(proposal_id.checked_add(1).unwrap_or_else(|| panic_with_error!(&env, Error::LimitReached))));

        // Note: You can append `args` to your event payload if necessary
        env.events().publish(
            (Symbol::new(&env, "proposal_created"),),
            ProposalCreated {
                proposal_id,
                proposer,
                target,
                function,
            },
        );

        proposal_id
    }

    /// Create a proposal that, on execution, upgrades `target`'s contract
    /// WASM to `new_wasm_hash`. `target` must have this DAO's own address
    /// configured as its admin — `execute()` invokes
    /// `target::upgrade(dao_address, new_wasm_hash)` on the DAO's behalf, so
    /// contract-upgrade authorization becomes a first-class governance
    /// action instead of living solely with a single admin key.
    ///
    /// Shares the same threshold/deposit/vote/finalize/timelock lifecycle as
    /// `create_proposal`; only the proposal `kind` and pre-built
    /// target/function/args differ.
    pub fn propose_upgrade(
        env: Env,
        proposer: Address,
        title: Bytes,
        target: Address,
        new_wasm_hash: BytesN<32>,
    ) -> u64 {
        proposer.require_auth();
        Self::validate_stellar_address(&env, &target);
        let config: DaoConfig = env
            .storage()
            .instance()
            .get(&StorageKey::Config)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        let gov_token = token::Client::new(&env, &config.gov_token);
        let weight = gov_token.balance(&proposer);
        if weight < config.proposal_threshold {
            panic_with_error!(&env, Error::InsufficientWeight);
        }
        let deposit = config.proposal_threshold;
        gov_token.transfer(&proposer, &env.current_contract_address(), &deposit);

        let proposal_id: u64 = env
            .storage()
            .instance()
            .get(&StorageKey::NextProposalId)
            .unwrap_or(0);
        let now = env.ledger().timestamp();

        let mut args: Vec<Val> = Vec::new(&env);
        args.push_back(env.current_contract_address().into_val(&env));
        args.push_back(new_wasm_hash.into_val(&env));
        let function = Symbol::new(&env, "upgrade");

        let proposal = Proposal {
            id: proposal_id,
            proposer: proposer.clone(),
            title,
            target: target.clone(),
            function: function.clone(),
            args,
            deposit,
            status: ProposalStatus::Active,
            votes_for: 0,
            votes_against: 0,
            votes_abstain: 0,
            created_at: now,
            vote_end: now.saturating_add(config.voting_period),
            execution_time: 0,
            total_supply: config.total_supply,
            kind: ProposalKind::Upgrade,
        };

        let proposal_key = StorageKey::Proposal(proposal_id);
        env.storage().persistent().set(&proposal_key, &proposal);
        env.storage().instance().set(
            &StorageKey::NextProposalId,
            &(proposal_id
                .checked_add(1)
                .unwrap_or_else(|| panic_with_error!(&env, Error::LimitReached))),
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_created"),),
            ProposalCreated {
                proposal_id,
                proposer,
                target,
                function,
            },
        );

        proposal_id
    }

    /// Cast a vote (For/Against/Abstain) on an Active proposal.
    ///
    /// The voter's entire current gov-token balance is used as their vote
    /// weight and is transferred into (locked in) the DAO contract for the
    /// duration of the vote — this prevents transferring the same tokens to
    /// another address to vote again ("token cycling"). The locked amount
    /// is released later via `withdraw_tokens`, once the proposal is no
    /// longer Active. Each address may vote at most once per proposal.
    pub fn vote(env: Env, voter: Address, proposal_id: u64, choice: VoteChoice) {
        voter.require_auth();

        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ProposalNotFound));

        if proposal.status != ProposalStatus::Active {
            panic_with_error!(&env, Error::ProposalNotActive);
        }
        if env.ledger().timestamp() > proposal.vote_end {
            panic_with_error!(&env, Error::VotingClosed);
        }
        let vote_key = StorageKey::VoteRecord(proposal_id, voter.clone());
        if env.storage().persistent().has(&vote_key) {
            panic_with_error!(&env, Error::AlreadyVoted);
        }

        let config: DaoConfig = env.storage().instance().get(&StorageKey::Config).unwrap();
        let gov_token = token::Client::new(&env, &config.gov_token);

        // 1. Capture voting balance weight
        let weight = gov_token.balance(&voter);
        if weight <= 0 {
            panic_with_error!(&env, Error::InsufficientWeight);
        }

        // 2. Lock tokens in the DAO contract to prevent token cycling / double-voting
        gov_token.transfer(&voter, &env.current_contract_address(), &weight);

        // Save the tracked locked balance for later retrieval
        let lock_key = StorageKey::LockedBalance(proposal_id, voter.clone());
        env.storage().persistent().set(&lock_key, &weight);

        match choice {
            VoteChoice::For => proposal.votes_for += weight,
            VoteChoice::Against => proposal.votes_against += weight,
            VoteChoice::Abstain => proposal.votes_abstain += weight,
        }

        env.storage().persistent().set(
            &vote_key,
            &VoteRecord {
                voter: voter.clone(),
                choice: choice.clone(),
                weight,
            },
        );
        let proposal_key = StorageKey::Proposal(proposal_id);
        env.storage().persistent().set(&proposal_key, &proposal);

        env.events().publish(
            (Symbol::new(&env, "vote_cast"),),
            VoteCast {
                proposal_id,
                voter,
                choice,
                weight,
            },
        );
    }

    /// Withdraw gov tokens that were locked by `vote()` on `proposal_id`.
    ///
    /// Only available once the proposal is no longer Active (i.e. after
    /// `finalize()` or `cancel()`). Returns the voter's full locked balance
    /// and clears the lock record so it cannot be withdrawn twice.
    pub fn withdraw_tokens(env: Env, voter: Address, proposal_id: u64) {
        voter.require_auth();

        let proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ProposalNotFound));

        // Prevent withdrawing while voting is still active
        if proposal.status == ProposalStatus::Active {
            panic_with_error!(&env, Error::ProposalNotActive);
        }

        let lock_key = StorageKey::LockedBalance(proposal_id, voter.clone());
        let locked_amount: i128 = env.storage().persistent().get(&lock_key).unwrap_or(0);

        if locked_amount <= 0 {
            panic_with_error!(&env, Error::InsufficientWeight);
        }

        // Clear tracking storage entry to prevent double-withdrawals
        env.storage().persistent().remove(&lock_key);

        // Refund the tokens back to the voter
        let config: DaoConfig = env.storage().instance().get(&StorageKey::Config).unwrap();
        let gov_token = token::Client::new(&env, &config.gov_token);
        gov_token.transfer(&env.current_contract_address(), &voter, &locked_amount);

        env.events().publish(
            (Symbol::new(&env, "tokens_withdrawn"),),
            (proposal_id, voter, locked_amount),
        );
    }

    /// Close voting on an Active proposal and settle it to Passed or Failed.
    ///
    /// Callable by anyone once `vote_end + FINALIZE_DELAY` has passed (the
    /// delay buffer prevents finalize being raced at the exact close of
    /// voting). Quorum is `total_votes >= total_supply * quorum_bps /
    /// 10_000`, using the `total_supply` snapshotted at proposal creation.
    /// If quorum is met, the proposal Passes only when `votes_for * 10_000 /
    /// total_votes >= majority_bps`; otherwise (including the exact-tie
    /// case, which lands at 50%) it Fails. Passing also starts the
    /// execution timelock (`execution_time = now + proposal_timelock`).
    /// The proposer's original deposit is refunded in both outcomes.
    pub fn finalize(env: Env, proposal_id: u64) {
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ProposalNotFound));

        if proposal.status != ProposalStatus::Active {
            panic_with_error!(&env, Error::ProposalNotActive);
        }

        // Enforce the validation buffer delay to prevent immediate edge execution races
        if env.ledger().timestamp() <= proposal.vote_end + FINALIZE_DELAY {
            panic_with_error!(&env, Error::FinalizeDelayNotMet);
        }

        let config: DaoConfig = env.storage().instance().get(&StorageKey::Config).unwrap();
        let total_supply = proposal.total_supply;
        let total_votes = proposal.votes_for + proposal.votes_against + proposal.votes_abstain;

        // Adaptive quorum: after a run of low-turnout votes the requirement
        // relaxes toward the configured floor, so a quiet DAO stays governable
        // instead of deadlocking on proposals nobody can pass. Disabled by
        // default, and never able to fall below MIN_QUORUM_BPS.
        let effective = Self::effective_quorum(&env, &config);
        if effective.decayed {
            env.events().publish(
                (Symbol::new(&env, "quorum_decay_applied"),),
                QuorumDecayApplied {
                    proposal_id,
                    base_bps: effective.base_bps,
                    effective_bps: effective.quorum_bps,
                    avg_participation_bps: effective.avg_participation_bps,
                },
            );
        }

        let quorum_needed = total_supply
            .checked_mul(effective.quorum_bps as i128)
            .and_then(|v| v.checked_div(10_000))
            .unwrap_or(i128::MAX);

        if total_votes == 0 {
            // No participation at all — always fails, even for a legacy
            // proposal that snapshotted total_supply == 0 (quorum_needed == 0).
            proposal.status = ProposalStatus::Failed;
        } else if total_votes < quorum_needed {
            proposal.status = ProposalStatus::Failed;
        } else {
            // Guard: prevent division by zero if total_votes == 0
            let for_bps = if total_votes > 0 {
                proposal.votes_for.checked_mul(10_000).map(|v| v / total_votes).unwrap_or(0)
            } else {
                0
            };
            if for_bps >= config.majority_bps as i128 {
                proposal.status = ProposalStatus::Passed;
                proposal.execution_time = env.ledger().timestamp() + config.proposal_timelock;
            } else {
                proposal.status = ProposalStatus::Failed;
            }
        }

        // Record this vote's turnout so it feeds the next proposal's adaptive
        // quorum. Recorded for every finalized proposal, pass or fail, so the
        // history reflects actual participation rather than only successes.
        Self::record_participation(&env, total_votes, total_supply);

        let gov_token = token::Client::new(&env, &config.gov_token);
        // Refund exactly what was locked at creation — never re-read the
        // live config, which the admin can change after the proposer
        // deposited (issue #137).
        gov_token.transfer(
            &env.current_contract_address(),
            &proposal.proposer,
            &proposal.deposit,
        );

        let proposal_key = StorageKey::Proposal(proposal_id);
        env.storage().persistent().set(&proposal_key, &proposal);

        env.events().publish(
            (Symbol::new(&env, "proposal_finalized"),),
            ProposalFinalized {
                proposal_id,
                status: proposal.status.clone(),
            },
        );
    }

    /// Reclaim a proposer's deposit on a proposal that was never finalized.
    ///
    /// `finalize()` is already permissionless and already refunds the
    /// deposit regardless of whether quorum/majority was reached — but it
    /// still requires *someone* to call it, and nobody is economically
    /// incentivized to spend a transaction finalizing a proposal that
    /// plainly failed quorum. Without this, such a deposit can sit locked
    /// indefinitely waiting on goodwill (issue #378).
    ///
    /// Callable by anyone once `now > vote_end + DEPOSIT_RECLAIM_TIMEOUT` —
    /// far past the point `finalize()` itself becomes callable — on a
    /// proposal still `Active`. Settles the proposal as `Failed` (mirroring
    /// what `finalize()` would compute for zero additional turnout) and
    /// refunds the deposit to the original proposer.
    pub fn reclaim_deposit(env: Env, proposal_id: u64) {
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ProposalNotFound));

        if proposal.status != ProposalStatus::Active {
            panic_with_error!(&env, Error::ProposalNotActive);
        }
        if env.ledger().timestamp() <= proposal.vote_end.saturating_add(DEPOSIT_RECLAIM_TIMEOUT) {
            panic_with_error!(&env, Error::ProposalNotExpired);
        }

        proposal.status = ProposalStatus::Failed;

        let config: DaoConfig = env.storage().instance().get(&StorageKey::Config).unwrap();
        let total_votes = proposal.votes_for + proposal.votes_against + proposal.votes_abstain;
        Self::record_participation(&env, total_votes, proposal.total_supply);

        let gov_token = token::Client::new(&env, &config.gov_token);
        gov_token.transfer(
            &env.current_contract_address(),
            &proposal.proposer,
            &proposal.deposit,
        );

        let proposal_key = StorageKey::Proposal(proposal_id);
        env.storage().persistent().set(&proposal_key, &proposal);

        env.events().publish(
            (Symbol::new(&env, "deposit_reclaimed"),),
            DepositReclaimed {
                proposal_id,
                proposer: proposal.proposer,
                amount: proposal.deposit,
            },
        );
    }

    /// Mark a Passed proposal as Executed once its timelock has expired.
    ///
    /// Requires `status == Passed` and `now >= execution_time`. This
    /// contract only flips the status flag — it does not itself perform the
    /// cross-contract call to `target::function(args)`; the caller is
    /// responsible for building the actual invocation and its auth tree, so
    /// this contract never needs admin rights on the proposal's target.
    pub fn execute(env: Env, proposal_id: u64) {
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ProposalNotFound));

        if proposal.status == ProposalStatus::Executed {
            panic_with_error!(&env, Error::AlreadyExecuted);
        }
        if proposal.status == ProposalStatus::Cancelled {
            panic_with_error!(&env, Error::AlreadyCancelled);
        }
        if proposal.status != ProposalStatus::Passed {
            panic_with_error!(&env, Error::ProposalNotPassed);
        }
        if env.ledger().timestamp() < proposal.execution_time {
            panic_with_error!(&env, Error::TimelockNotExpired);
        }

        // Validate target address is a valid Stellar address before execution
        // This is a defense-in-depth check to prevent targeting invalid contracts
        Self::validate_stellar_address(&env, &proposal.target);

        // Perform actual cross-contract call to target::function(args)
        // If the target contract doesn't exist or the function is invalid,
        // this call will fail and the proposal won't be marked as executed
        let _: Val = env.invoke_contract(&proposal.target, &proposal.function, proposal.args.clone());

        proposal.status = ProposalStatus::Executed;
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

        env.events().publish(
            (Symbol::new(&env, "proposal_executed"),),
            ProposalExecuted { proposal_id },
        );
    }

    /// Admin-only: cancel an Active proposal before voting closes.
    ///
    /// Refunds the proposer's deposit (the exact amount locked at
    /// proposal creation) and marks the proposal Cancelled. Voters who
    /// already locked tokens can reclaim them via `withdraw_tokens`
    /// once cancelled.
    pub fn cancel(env: Env, admin: Address, proposal_id: u64) {
        Self::require_admin(&env, &admin);

        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ProposalNotFound));

        if proposal.status != ProposalStatus::Active {
            panic_with_error!(&env, Error::ProposalNotActive);
        }

        proposal.status = ProposalStatus::Cancelled;
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

        let config: DaoConfig = env.storage().instance().get(&StorageKey::Config).unwrap();
        let gov_token = token::Client::new(&env, &config.gov_token);
        gov_token.transfer(
            &env.current_contract_address(),
            &proposal.proposer,
            &proposal.deposit,
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_cancelled"),),
            ProposalCancelled { proposal_id },
        );
    }


    // ── Adaptive quorum ───────────────────────────────────────────────────────

    /// Configure adaptive quorum decay.
    ///
    /// Disabled by default: an existing DAO keeps its static quorum until it
    /// deliberately opts in. `floor_bps` is clamped up to `MIN_QUORUM_BPS` —
    /// decay can relax the requirement toward the floor but can never take it
    /// below the level the contract considers safe at all.
    pub fn set_quorum_decay(
        env: Env,
        admin: Address,
        enabled: bool,
        floor_bps: u32,
        decay_per_period_bps: u32,
        window: u32,
    ) {
        Self::require_admin(&env, &admin);

        let config: DaoConfig = env
            .storage()
            .instance()
            .get(&StorageKey::Config)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        // A floor at or above the configured quorum would make decay a no-op,
        // which is a configuration mistake worth surfacing rather than
        // silently accepting.
        if enabled && floor_bps >= config.quorum_bps {
            panic_with_error!(&env, Error::InvalidQuorumDecay);
        }
        if enabled && (window == 0 || window > MAX_PARTICIPATION_WINDOW) {
            panic_with_error!(&env, Error::InvalidQuorumDecay);
        }
        if enabled && decay_per_period_bps == 0 {
            panic_with_error!(&env, Error::InvalidQuorumDecay);
        }

        let effective_floor = floor_bps.max(MIN_QUORUM_BPS);

        let decay = QuorumDecayConfig {
            enabled,
            floor_bps: effective_floor,
            decay_per_period_bps,
            window,
        };
        env.storage()
            .instance()
            .set(&StorageKey::QuorumDecay, &decay);

        env.events().publish(
            (Symbol::new(&env, "quorum_decay_updated"),),
            QuorumDecayConfigUpdated {
                enabled,
                floor_bps: effective_floor,
                decay_per_period_bps,
                window,
            },
        );
    }

    /// The current adaptive-quorum settings.
    pub fn get_quorum_decay(env: Env) -> QuorumDecayConfig {
        Self::quorum_decay_config(&env)
    }

    /// Rolling participation history used to compute adaptive quorum.
    pub fn get_participation_history(env: Env) -> ParticipationHistory {
        Self::participation_history(&env)
    }

    /// The quorum that would be applied to a proposal finalized right now,
    /// and the participation figure behind it.
    ///
    /// Exposed so voters can see the bar before they vote rather than
    /// discovering it at finalize.
    pub fn get_effective_quorum(env: Env) -> EffectiveQuorum {
        let config: DaoConfig = env
            .storage()
            .instance()
            .get(&StorageKey::Config)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        Self::effective_quorum(&env, &config)
    }

    // ── Proposal templates ────────────────────────────────────────────────────

    /// Admin-only: register (or update) a named proposal template.
    ///
    /// A template is a minimal structural contract for a common proposal
    /// type: a minimum title length and an exact expected argument count.
    /// `create_proposal_from_template` rejects proposals that don't match,
    /// so reviewers no longer have to parse intent out of free-form,
    /// inconsistent proposals for the categories a DAO cares to standardize
    /// (issue #381).
    pub fn register_template(
        env: Env,
        admin: Address,
        name: Symbol,
        description: Bytes,
        min_title_len: u32,
        required_arg_count: u32,
    ) {
        Self::require_admin(&env, &admin);

        let template = ProposalTemplate {
            name: name.clone(),
            description,
            min_title_len,
            required_arg_count,
            active: true,
        };
        env.storage()
            .instance()
            .set(&StorageKey::Template(name.clone()), &template);

        let mut names: Vec<Symbol> = env
            .storage()
            .instance()
            .get(&StorageKey::TemplateList)
            .unwrap_or_else(|| Vec::new(&env));
        let mut already_present = false;
        for n in names.iter() {
            if n == name {
                already_present = true;
                break;
            }
        }
        if !already_present {
            names.push_back(name.clone());
            env.storage().instance().set(&StorageKey::TemplateList, &names);
        }

        env.events().publish(
            (Symbol::new(&env, "template_registered"),),
            TemplateRegistered {
                name,
                min_title_len,
                required_arg_count,
            },
        );
    }

    /// Admin-only: deactivate a template so it can no longer be used for new
    /// proposals. Proposals already created from it are unaffected.
    pub fn deactivate_template(env: Env, admin: Address, name: Symbol) {
        Self::require_admin(&env, &admin);
        let key = StorageKey::Template(name.clone());
        let mut template: ProposalTemplate = env
            .storage()
            .instance()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::TemplateNotFound));
        template.active = false;
        env.storage().instance().set(&key, &template);
    }

    /// Fetch a registered template by name.
    pub fn get_template(env: Env, name: Symbol) -> ProposalTemplate {
        env.storage()
            .instance()
            .get(&StorageKey::Template(name))
            .unwrap_or_else(|| panic_with_error!(&env, Error::TemplateNotFound))
    }

    /// List the names of all registered templates (active or not).
    pub fn list_templates(env: Env) -> Vec<Symbol> {
        env.storage()
            .instance()
            .get(&StorageKey::TemplateList)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Create a proposal, validated against a registered template's
    /// structure before creation: `title` must be at least
    /// `template.min_title_len` bytes and `args` must have exactly
    /// `template.required_arg_count` entries. Otherwise behaves exactly
    /// like `create_proposal` (same deposit/threshold/lifecycle).
    pub fn create_proposal_from_template(
        env: Env,
        proposer: Address,
        template_name: Symbol,
        title: Bytes,
        target: Address,
        function: Symbol,
        args: Vec<Val>,
    ) -> u64 {
        let template: ProposalTemplate = env
            .storage()
            .instance()
            .get(&StorageKey::Template(template_name.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, Error::TemplateNotFound));
        if !template.active {
            panic_with_error!(&env, Error::TemplateInactive);
        }
        if title.len() < template.min_title_len {
            panic_with_error!(&env, Error::TitleTooShort);
        }
        if args.len() != template.required_arg_count {
            panic_with_error!(&env, Error::ArgCountMismatch);
        }

        let proposal_id = Self::create_proposal(env.clone(), proposer, title, target, function, args);

        env.events().publish(
            (Symbol::new(&env, "proposal_created_from_template"),),
            ProposalCreatedFromTemplate {
                proposal_id,
                template_name,
            },
        );

        proposal_id
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    /// Fetch a proposal by id. Panics with `ProposalNotFound` if it doesn't exist.
    pub fn get_proposal(env: Env, proposal_id: u64) -> Proposal {
        env.storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ProposalNotFound))
    }

    /// Look up a voter's vote record (choice + weight) for a proposal, if
    /// they voted. Returns `None` if the address has not voted.
    pub fn get_vote(env: Env, proposal_id: u64, voter: Address) -> Option<VoteRecord> {
        env.storage()
            .persistent()
            .get(&StorageKey::VoteRecord(proposal_id, voter))
    }

    /// Return the current DAO configuration (gov token, thresholds, periods).
    pub fn get_config(env: Env) -> DaoConfig {
        env.storage()
            .instance()
            .get(&StorageKey::Config)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    /// Return the current admin address.
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    /// Return the total number of proposals ever created (next proposal id).
    pub fn proposal_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&StorageKey::NextProposalId)
            .unwrap_or(0)
    }

    /// Return the contract's current storage/version number (defaults to 1).
    pub fn get_version(env: Env) -> u32 {
        env.storage().instance().get(&StorageKey::Version).unwrap_or(1)
    }

    // ── Admin ─────────────────────────────────────────────────────────────────

    /// Admin-only: replace the DAO configuration wholesale.
    ///
    /// Only affects proposals created after this call — `finalize()` always
    /// uses the `deposit` and `total_supply` snapshotted on each `Proposal`
    /// at creation time, so changing `proposal_threshold` or `total_supply`
    /// here cannot retroactively affect proposals already in flight.
    pub fn update_config(env: Env, admin: Address, config: DaoConfig) {
        Self::require_admin(&env, &admin);
        if config.voting_period < MIN_VOTING_PERIOD {
            panic_with_error!(&env, Error::VotingPeriodTooShort);
        }
        if config.voting_period > MAX_VOTING_PERIOD {
            panic_with_error!(&env, Error::VotingPeriodTooLong);
        }
        if config.quorum_bps < MIN_QUORUM_BPS {
            panic_with_error!(&env, Error::QuorumTooLow);
        }
        env.storage().instance().set(&StorageKey::Config, &config);

        env.events().publish(
            (Symbol::new(&env, "dao_config_updated"),),
            DaoConfigUpdated {
                gov_token: config.gov_token.clone(),
                proposal_threshold: config.proposal_threshold,
                total_supply: config.total_supply,
                voting_period: config.voting_period,
                proposal_timelock: config.proposal_timelock,
            },
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
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>, new_version: u32) {
        Self::require_admin(&env, &admin);
        let current_version: u32 = env.storage().instance().get(&StorageKey::Version).unwrap_or(1);
        if new_version <= current_version {
            panic_with_error!(&env, Error::VersionNotNewer);
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

    /// Configure the guardian set and approval threshold required for
    /// critical actions (currently: contract upgrades). Admin-only.
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
    pub fn approve_upgrade(env: Env, guardian: Address, new_wasm_hash: BytesN<32>) {
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

    /// Run storage migrations from old_version to new_version.
    /// Each migration function handles a specific version transition.
    fn run_migrations(_env: &Env, _old_version: u32, _new_version: u32) {
        // Migration from v1 to v2: No storage changes needed yet
        // This is where you would add migration logic for specific version bumps
        // Example: if old_version < 2 && new_version >= 2 { Self::migrate_v1_to_v2(env); }
        
        // Future migrations follow the pattern:
        // if old_version < 3 && new_version >= 3 { Self::migrate_v2_to_v3(env); }
    }


    /// Adaptive-quorum settings, or the disabled default.
    fn quorum_decay_config(env: &Env) -> QuorumDecayConfig {
        env.storage()
            .instance()
            .get(&StorageKey::QuorumDecay)
            .unwrap_or(QuorumDecayConfig {
                enabled: false,
                floor_bps: MIN_QUORUM_BPS,
                decay_per_period_bps: 0,
                window: 0,
            })
    }

    /// Rolling participation history, or an empty one.
    fn participation_history(env: &Env) -> ParticipationHistory {
        env.storage()
            .instance()
            .get(&StorageKey::Participation)
            .unwrap_or(ParticipationHistory {
                recent_bps: Vec::new(env),
                total_recorded: 0,
            })
    }

    /// Average turnout across the recorded window, in basis points.
    /// Returns `None` when there is no history to average.
    fn average_participation_bps(history: &ParticipationHistory) -> Option<u32> {
        let len = history.recent_bps.len();
        if len == 0 {
            return None;
        }

        let mut sum: u64 = 0;
        for i in 0..len {
            sum += history.recent_bps.get_unchecked(i) as u64;
        }
        Some((sum / len as u64) as u32)
    }

    /// The quorum requirement in force, after any decay.
    ///
    /// Decay is proportional to the shortfall: the further average turnout has
    /// fallen below the configured requirement, the more the requirement
    /// relaxes — bounded by `floor_bps`, which itself can never sit below
    /// `MIN_QUORUM_BPS`.
    ///
    /// With no history at all, the base requirement stands. A DAO's very first
    /// proposals should not get a discount for the absence of evidence.
    fn effective_quorum(env: &Env, config: &DaoConfig) -> EffectiveQuorum {
        let base_bps = config.quorum_bps;
        let decay = Self::quorum_decay_config(env);
        let history = Self::participation_history(env);
        let avg = Self::average_participation_bps(&history);

        let avg_bps = avg.unwrap_or(0);

        if !decay.enabled || avg.is_none() {
            return EffectiveQuorum {
                quorum_bps: base_bps,
                base_bps,
                avg_participation_bps: avg_bps,
                decayed: false,
            };
        }

        // Participation at or above the bar is not a low-participation period,
        // so nothing decays.
        if avg_bps >= base_bps {
            return EffectiveQuorum {
                quorum_bps: base_bps,
                base_bps,
                avg_participation_bps: avg_bps,
                decayed: false,
            };
        }

        // How many whole decay steps the shortfall represents. Each
        // `decay_per_period_bps` of shortfall relaxes the bar by the same
        // amount, so the requirement tracks observed turnout rather than
        // collapsing to the floor at the first quiet vote.
        let shortfall = base_bps - avg_bps;
        let steps = shortfall / decay.decay_per_period_bps.max(1);
        let reduction = steps.saturating_mul(decay.decay_per_period_bps);

        let decayed_bps = base_bps.saturating_sub(reduction).max(decay.floor_bps);

        EffectiveQuorum {
            quorum_bps: decayed_bps,
            base_bps,
            avg_participation_bps: avg_bps,
            decayed: decayed_bps < base_bps,
        }
    }

    /// Record a finalized proposal's turnout into the rolling window.
    ///
    /// Turnout is measured against the supply snapshotted at proposal
    /// creation, matching how quorum itself is measured, so the two figures
    /// are always comparable.
    fn record_participation(env: &Env, total_votes: i128, total_supply: i128) {
        let decay = Self::quorum_decay_config(env);
        let window = if decay.window == 0 {
            DEFAULT_PARTICIPATION_WINDOW
        } else {
            decay.window
        };

        let turnout_bps: u32 = if total_supply <= 0 {
            0
        } else {
            total_votes
                .checked_mul(10_000)
                .map(|v| v / total_supply)
                .unwrap_or(10_000)
                .clamp(0, 10_000) as u32
        };

        let mut history = Self::participation_history(env);
        history.recent_bps.push_back(turnout_bps);
        while history.recent_bps.len() > window {
            history.recent_bps.pop_front();
        }
        history.total_recorded = history.total_recorded.saturating_add(1);

        env.storage()
            .instance()
            .set(&StorageKey::Participation, &history);
    }

    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        if *caller != admin {
            panic_with_error!(env, Error::Unauthorized);
        }
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

    /// Extend the TTL for proposal-related storage entries to prevent expiry
    /// during voting, timelock, and execution periods.
    fn extend_proposal_ttl(env: &Env, key: &StorageKey, _config: &DaoConfig) {
        env.storage().persistent().extend_ttl(key, TTL_THRESHOLD, TTL_EXTEND_TO);
    }
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_advanced;
