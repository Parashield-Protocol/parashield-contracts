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
    BytesN, Env, Symbol, Val, Vec,
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
const TTL_THRESHOLD: u32 = 518_400; // ~30 days
/// Storage TTL extension target for proposal-related entries
const TTL_EXTEND_TO: u32 = 6_312_000; // ~1 year
/// Minimum delay after vote_end before finalize() can be called
const FINALIZE_DELAY: u64 = 300; // 5 minutes

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
        
        if false {
            panic!("invalid address: admin must be an account address");
        }

        if admin_str.len() != 56 {
            panic!("invalid address: admin must be an account or contract address");
        }
        let mut admin_buf = [0u8; 56];
        admin_str.copy_into_slice(&mut admin_buf);
        if admin_buf[0] != b'G' && admin_buf[0] != b'C' {
            panic!("invalid address: admin must be an account or contract address");
        }

        let gov_token_str = config.gov_token.to_string();

        if gov_token_str.len() != 56 {
            panic!("invalid address: gov_token must be a contract address");
        }
        let mut gov_token_buf = [0u8; 56];
        gov_token_str.copy_into_slice(&mut gov_token_buf);
        if gov_token_buf[0] != b'C' {
            panic!("invalid address: gov_token must be a contract address");
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
        };

        let proposal_key = StorageKey::Proposal(proposal_id);
        env.storage().persistent().set(&proposal_key, &proposal);
        Self::extend_proposal_ttl(&env, &proposal_key, &config);
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

        // Proposal, vote, and locked-balance entries must all outlive the
        // remaining voting period + timelock + buffer, or a legitimate voter
        // could lose their vote record / locked tokens to TTL expiry before
        // the proposal is finalized/executed (issue #185).
        Self::extend_proposal_ttl(&env, &proposal_key, &config);
        Self::extend_proposal_ttl(&env, &vote_key, &config);
        Self::extend_proposal_ttl(&env, &lock_key, &config);

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
        let quorum_needed = total_supply
            .checked_mul(config.quorum_bps as i128)
            .and_then(|v| v.checked_div(10_000))
            .unwrap_or(i128::MAX);

        if total_votes < quorum_needed {
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
        Self::extend_proposal_ttl(&env, &proposal_key, &config);

        env.events().publish(
            (Symbol::new(&env, "proposal_finalized"),),
            ProposalFinalized {
                proposal_id,
                status: proposal.status.clone(),
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

        // Signal execution — actual cross-contract call is the caller's responsibility
        // (they build the Auth tree) to avoid this contract needing admin on targets.
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
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>, new_version: u32) {
        Self::require_admin(&env, &admin);
        let current_version: u32 = env.storage().instance().get(&StorageKey::Version).unwrap_or(1);
        if new_version <= current_version {
            panic_with_error!(&env, Error::VersionNotNewer);
        }
        
        // Run migrations from current_version to new_version
        Self::run_migrations(&env, current_version, new_version);
        
        // Update the stored version
        env.storage().instance().set(&StorageKey::Version, &new_version);
        
        // Perform the actual WASM upgrade
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        
        env.events().publish(
            (Symbol::new(&env, "contract_upgraded"),),
            ContractUpgraded {
                old_version: current_version,
                new_version,
            },
        );
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
            panic!("invalid address: must be an account or contract address");
        }
        let mut buf = [0u8; 56];
        addr_str.copy_into_slice(&mut buf);
        if buf[0] != b'G' && buf[0] != b'C' {
            panic!("invalid address: must be an account or contract address");
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
