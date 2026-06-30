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
use alloc::string::ToString;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, token, Address, Bytes,
    BytesN, Env, Symbol, Val, Vec,
};

pub mod types;
pub use types::*;

#[contracttype]
enum StorageKey {
    Initialized,
    Admin,
    Config,
    NextProposalId,
    Proposal(u64),
    VoteRecord(u64, Address),
    LockedBalance(u64, Address),
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
}

#[contract]
pub struct GovernanceDao;

#[contractimpl]
impl GovernanceDao {
    pub fn initialize(env: Env, admin: Address, config: DaoConfig) {
        if env.storage().instance().has(&StorageKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }

        // Address verification
        let admin_str = admin.to_string();
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

    pub fn create_proposal(
        env: Env,
        proposer: Address,
        title: Bytes,
        target: Address,
        function: Symbol,
        args: Vec<Val>, // <--- ADD THIS ARGUMENT
    ) -> u64 {
        proposer.require_auth();
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
        gov_token.transfer(
            &proposer,
            &env.current_contract_address(),
            &config.proposal_threshold,
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
            status: ProposalStatus::Active,
            votes_for: 0,
            votes_against: 0,
            votes_abstain: 0,
            created_at: now,
            vote_end: now + config.voting_period,
            execution_time: 0,
        };

        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);
        env.storage()
            .instance()
            .set(&StorageKey::NextProposalId, &(proposal_id + 1));

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
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

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
        let total_supply = config.total_supply;
        let total_votes = proposal.votes_for + proposal.votes_against + proposal.votes_abstain;
        let quorum_needed = total_supply * config.quorum_bps as i128 / 10_000;

        if total_votes < quorum_needed {
            proposal.status = ProposalStatus::Failed;
        } else {
            let for_bps = if total_votes > 0 {
                proposal.votes_for * 10_000 / total_votes
            } else {
                0
            };
            if for_bps as u32 >= config.majority_bps {
                proposal.status = ProposalStatus::Passed;
                proposal.execution_time = env.ledger().timestamp() + config.proposal_timelock;
            } else {
                proposal.status = ProposalStatus::Failed;
            }
        }

        let gov_token = token::Client::new(&env, &config.gov_token);
        gov_token.transfer(
            &env.current_contract_address(),
            &proposal.proposer,
            &config.proposal_threshold,
        );

        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

        env.events().publish(
            (Symbol::new(&env, "proposal_finalized"),),
            ProposalFinalized {
                proposal_id,
                status: proposal.status.clone(),
            },
        );
    }

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
            &config.proposal_threshold,
        );

        env.events().publish(
            (Symbol::new(&env, "proposal_cancelled"),),
            ProposalCancelled { proposal_id },
        );
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    pub fn get_proposal(env: Env, proposal_id: u64) -> Proposal {
        env.storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ProposalNotFound))
    }

    pub fn get_vote(env: Env, proposal_id: u64, voter: Address) -> Option<VoteRecord> {
        env.storage()
            .persistent()
            .get(&StorageKey::VoteRecord(proposal_id, voter))
    }

    pub fn get_config(env: Env) -> DaoConfig {
        env.storage()
            .instance()
            .get(&StorageKey::Config)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    pub fn proposal_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&StorageKey::NextProposalId)
            .unwrap_or(0)
    }

    // ── Admin ─────────────────────────────────────────────────────────────────

    pub fn update_config(env: Env, admin: Address, config: DaoConfig) {
        Self::require_admin(&env, &admin);
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
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) {
        Self::require_admin(&env, &admin);
        env.deployer().update_current_contract_wasm(new_wasm_hash);
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
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_advanced;
