#![allow(clippy::inconsistent_digit_grouping)]
//! Advanced governance-dao tests: update_config, multi-voter scenarios.
#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Bytes, Env, Symbol,
};

use crate::{DaoConfig, GovernanceDao, GovernanceDaoClient, ProposalStatus, VoteChoice};

const VOTING_PERIOD: u64 = 7 * 24 * 3600;

fn base_config(gov_token: Address) -> DaoConfig {
    DaoConfig {
        gov_token,
        total_supply:       1_100_000_0000000i128,
        proposal_threshold: 10_000_0000000i128,
        quorum_bps:         1_000u32,
        majority_bps:       5_100u32,
        voting_period:      VOTING_PERIOD,
        proposal_timelock:  0,
    }
}

fn make_dao(env: &Env) -> (GovernanceDaoClient<'static>, Address, Address, Address) {
    let admin  = Address::generate(env);
    let voter  = Address::generate(env);
    let target = Address::generate(env);

    let gov_token_id = env.register_stellar_asset_contract_v2(admin.clone()).address();
    token::StellarAssetClient::new(env, &gov_token_id)
        .mint(&voter, &1_000_000_0000000i128);
    token::StellarAssetClient::new(env, &gov_token_id)
        .mint(&admin, &100_000_0000000i128);

    let dao_id = env.register(GovernanceDao, ());
    let dao    = GovernanceDaoClient::new(env, &dao_id);
    dao.initialize(&admin, &base_config(gov_token_id));

    (dao, admin, voter, target)
}

// ── config update ─────────────────────────────────────────────────────────────

#[test]
fn admin_can_update_config() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, admin, _, _) = make_dao(&env);

    let cfg_before = dao.get_config();
    let new_cfg = DaoConfig {
        quorum_bps:    2_000u32,
        majority_bps:  6_000u32,
        ..cfg_before.clone()
    };
    dao.update_config(&admin, &new_cfg);
    let cfg_after = dao.get_config();
    assert_eq!(cfg_after.quorum_bps,  2_000);
    assert_eq!(cfg_after.majority_bps, 6_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn non_admin_cannot_update_config() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, _, voter, _) = make_dao(&env);
    let cfg = dao.get_config();
    dao.update_config(&voter, &cfg);
}

// ── abstain vote ──────────────────────────────────────────────────────────────

#[test]
fn abstain_contributes_to_quorum_but_not_majority() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, _, voter, target) = make_dao(&env);

    let pid = dao.create_proposal(
        &voter,
        &Bytes::from_slice(&env, b"Abstain test"),
        &target,
        &Symbol::new(&env, "update"),
    );
    dao.vote(&voter, &pid, &VoteChoice::Abstain);
    env.ledger().with_mut(|l| l.timestamp += VOTING_PERIOD + 1);
    dao.finalize(&pid);

    let p = dao.get_proposal(&pid);
    // Abstain satisfies quorum but no FOR votes → fails majority check
    assert_eq!(p.status, ProposalStatus::Failed);
    assert_eq!(p.votes_abstain, 1_000_000_0000000i128);
    assert_eq!(p.votes_for, 0);
}

// ── proposal_count ────────────────────────────────────────────────────────────

#[test]
fn proposal_count_increments_per_proposal() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, _, voter, target) = make_dao(&env);

    assert_eq!(dao.proposal_count(), 0);
    dao.create_proposal(&voter, &Bytes::from_slice(&env, b"P1"), &target, &Symbol::new(&env, "fn1"));
    assert_eq!(dao.proposal_count(), 1);
    dao.create_proposal(&voter, &Bytes::from_slice(&env, b"P2"), &target, &Symbol::new(&env, "fn2"));
    assert_eq!(dao.proposal_count(), 2);
}

// ── double-execute guard ──────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn execute_twice_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, _, voter, target) = make_dao(&env);

    let pid = dao.create_proposal(
        &voter,
        &Bytes::from_slice(&env, b"Execute twice"),
        &target,
        &Symbol::new(&env, "update"),
    );
    dao.vote(&voter, &pid, &VoteChoice::For);
    env.ledger().with_mut(|l| l.timestamp += VOTING_PERIOD + 1);
    dao.finalize(&pid);
    dao.execute(&pid);
    dao.execute(&pid);  // second execute should panic
}
