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

use soroban_sdk::{
    contract, contractimpl, contracttype, contracterror, panic_with_error,
    token, Address, Bytes, Env, Symbol,
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
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized   = 1,
    NotInitialized       = 2,
    Unauthorized         = 3,
    InsufficientWeight   = 4,
    ProposalNotFound     = 5,
    ProposalNotActive    = 6,
    AlreadyVoted         = 7,
    VotingClosed         = 8,
    VotingStillOpen      = 9,
    ProposalNotPassed    = 10,
    AlreadyExecuted      = 11,
    AlreadyCancelled     = 12,
}

#[contract]
pub struct GovernanceDao;

#[contractimpl]
impl GovernanceDao {

    pub fn initialize(
        env: Env,
        admin: Address,
        config: DaoConfig,
    ) {
        if env.storage().instance().has(&StorageKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        // Address validation is deferred to require_auth() calls which
        // verify the address on the Soroban network layer.
        admin.require_auth();
        env.storage().instance().set(&StorageKey::Initialized,    &true);
        env.storage().instance().set(&StorageKey::Admin,          &admin);
        env.storage().instance().set(&StorageKey::Config,         &config);
        env.storage().instance().set(&StorageKey::NextProposalId, &0u64);

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
    ) -> u64 {
        proposer.require_auth();
        let config: DaoConfig = env.storage().instance().get(&StorageKey::Config)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        let gov_token = token::Client::new(&env, &config.gov_token);
        let weight = gov_token.balance(&proposer);
        if weight < config.proposal_threshold {
            panic_with_error!(&env, Error::InsufficientWeight);
        }

        let proposal_id: u64 = env.storage().instance()
            .get(&StorageKey::NextProposalId).unwrap_or(0);
        let now = env.ledger().timestamp();

        let proposal = Proposal {
            id:            proposal_id,
            proposer:      proposer.clone(),
            title,
            target:        target.clone(),
            function:      function.clone(),
            status:        ProposalStatus::Active,
            votes_for:     0,
            votes_against: 0,
            votes_abstain: 0,
            created_at:    now,
            vote_end:      now + config.voting_period,
        };

        env.storage().persistent().set(&StorageKey::Proposal(proposal_id), &proposal);
        env.storage().instance().set(&StorageKey::NextProposalId, &(proposal_id + 1));

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

    pub fn vote(
        env: Env,
        voter: Address,
        proposal_id: u64,
        choice: VoteChoice,
    ) {
        voter.require_auth();

        let mut proposal: Proposal = env.storage().persistent()
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
        let weight = token::Client::new(&env, &config.gov_token).balance(&voter);
        if weight <= 0 { panic_with_error!(&env, Error::InsufficientWeight); }

        match choice {
            VoteChoice::For     => proposal.votes_for     += weight,
            VoteChoice::Against => proposal.votes_against += weight,
            VoteChoice::Abstain => proposal.votes_abstain += weight,
        }

        env.storage().persistent().set(&vote_key, &VoteRecord {
            voter: voter.clone(),
            choice: choice.clone(),
            weight,
        });
        env.storage().persistent().set(&StorageKey::Proposal(proposal_id), &proposal);

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

    pub fn finalize(env: Env, proposal_id: u64) {
        let mut proposal: Proposal = env.storage().persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ProposalNotFound));

        if proposal.status != ProposalStatus::Active {
            panic_with_error!(&env, Error::ProposalNotActive);
        }
        if env.ledger().timestamp() <= proposal.vote_end {
            panic_with_error!(&env, Error::VotingStillOpen);
        }

        let config: DaoConfig = env.storage().instance().get(&StorageKey::Config).unwrap();
        let total_supply = config.total_supply;
        let total_votes  = proposal.votes_for + proposal.votes_against + proposal.votes_abstain;
        let quorum_needed = total_supply * config.quorum_bps as i128 / 10_000;

        if total_votes < quorum_needed {
            proposal.status = ProposalStatus::Failed;
        } else {
            let for_bps = if total_votes > 0 {
                proposal.votes_for * 10_000 / total_votes
            } else {
                0
            };
            proposal.status = if for_bps as u32 >= config.majority_bps {
                ProposalStatus::Passed
            } else {
                ProposalStatus::Failed
            };
        }

        env.storage().persistent().set(&StorageKey::Proposal(proposal_id), &proposal);

        env.events().publish(
            (Symbol::new(&env, "proposal_finalized"),),
            ProposalFinalized {
                proposal_id,
                status: proposal.status.clone(),
            },
        );
    }

    pub fn execute(env: Env, proposal_id: u64) {
        let mut proposal: Proposal = env.storage().persistent()
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

        // Signal execution — actual cross-contract call is the caller's responsibility
        // (they build the Auth tree) to avoid this contract needing admin on targets.
        proposal.status = ProposalStatus::Executed;
        env.storage().persistent().set(&StorageKey::Proposal(proposal_id), &proposal);

        env.events().publish(
            (Symbol::new(&env, "proposal_executed"),),
            ProposalExecuted { proposal_id },
        );
    }

    pub fn cancel(env: Env, admin: Address, proposal_id: u64) {
        Self::require_admin(&env, &admin);

        let mut proposal: Proposal = env.storage().persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ProposalNotFound));

        if proposal.status != ProposalStatus::Active {
            panic_with_error!(&env, Error::ProposalNotActive);
        }

        proposal.status = ProposalStatus::Cancelled;
        env.storage().persistent().set(&StorageKey::Proposal(proposal_id), &proposal);

        env.events().publish(
            (Symbol::new(&env, "proposal_cancelled"),),
            ProposalCancelled { proposal_id },
        );
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    pub fn get_proposal(env: Env, proposal_id: u64) -> Proposal {
        env.storage().persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::ProposalNotFound))
    }

    pub fn get_vote(env: Env, proposal_id: u64, voter: Address) -> Option<VoteRecord> {
        env.storage().persistent()
            .get(&StorageKey::VoteRecord(proposal_id, voter))
    }

    pub fn get_config(env: Env) -> DaoConfig {
        env.storage().instance().get(&StorageKey::Config)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    pub fn proposal_count(env: Env) -> u64 {
        env.storage().instance().get(&StorageKey::NextProposalId).unwrap_or(0)
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
            },
        );
    }

    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env.storage().instance().get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        if *caller != admin { panic_with_error!(env, Error::Unauthorized); }
        caller.require_auth();
    }
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_advanced;
