#![allow(clippy::inconsistent_digit_grouping)]
#![allow(unused_variables)]
#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Bytes, Env, Symbol,
};

use crate::{DaoConfig, GovernanceDao, GovernanceDaoClient, VoteChoice};

// ── helpers ───────────────────────────────────────────────────────────────────

const VOTING_PERIOD: u64 = 7 * 24 * 3600;  // 7 days in seconds

fn setup() -> (Env, GovernanceDaoClient<'static>, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin    = Address::generate(&env);
    let voter1   = Address::generate(&env);
    let voter2   = Address::generate(&env);
    let target   = Address::generate(&env);

    let gov_token_id = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let gov_client   = token::StellarAssetClient::new(&env, &gov_token_id);

    // mint voting power
    gov_client.mint(&voter1, &1_000_000_0000000i128);
    gov_client.mint(&voter2, &  500_000_0000000i128);
    gov_client.mint(&admin,  &  100_000_0000000i128);

    let dao_id = env.register(GovernanceDao, ());
    let dao    = GovernanceDaoClient::new(&env, &dao_id);

    dao.initialize(
        &admin,
        &DaoConfig {
            gov_token:           gov_token_id,
            total_supply:        1_600_000_0000000i128,
            proposal_threshold:  10_000_0000000i128,  // 10k SHIELD
            quorum_bps:          1_000u32,             // 10%
            majority_bps:        5_100u32,             // 51%
            voting_period:       VOTING_PERIOD,
        },
    );

    (env, dao, admin, voter1, voter2, target)
}

// ── initialization ────────────────────────────────────────────────────────────

#[test]
fn initialize_stores_config() {
    let (_env, dao, admin, _, _, _target) = setup();
    let cfg = dao.get_config();
    assert_eq!(cfg.quorum_bps,    1_000);
    assert_eq!(cfg.majority_bps,  5_100);
    assert_eq!(dao.get_admin(),   admin);
    assert_eq!(dao.proposal_count(), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn cannot_initialize_twice() {
    let (env, dao, admin, _, _, _) = setup();
    let gov_token = dao.get_config().gov_token;
    let _target    = Address::generate(&env);
    dao.initialize(
        &admin,
        &DaoConfig {
            gov_token,
            total_supply:       0,
            proposal_threshold: 0,
            quorum_bps:         0,
            majority_bps:       0,
            voting_period:      0,
        },
    );
}

// ── proposal creation ─────────────────────────────────────────────────────────

#[test]
fn create_proposal_increments_counter() {
    let (env, dao, _, voter1, _, target) = setup();
    let id = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Lower quorum to 5%"),
        &target,
        &Symbol::new(&env, "update"),
    );
    assert_eq!(id, 0u64);
    assert_eq!(dao.proposal_count(), 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn create_proposal_below_threshold_fails() {
    let (env, dao, _, _, _, target) = setup();
    let nobody = Address::generate(&env);
    dao.create_proposal(
        &nobody,
        &Bytes::from_slice(&env, b"Sneaky proposal"),
        &target,
        &Symbol::new(&env, "update"),
    );
}

// ── voting ────────────────────────────────────────────────────────────────────

#[test]
fn vote_for_records_weight() {
    let (env, dao, _, voter1, _, target) = setup();
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Test proposal"),
        &target,
        &Symbol::new(&env, "update"),
    );
    dao.vote(&voter1, &pid, &VoteChoice::For);
    let rec = dao.get_vote(&pid, &voter1).unwrap();
    assert_eq!(rec.weight, 1_000_000_0000000i128);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn double_vote_fails() {
    let (env, dao, _, voter1, _, target) = setup();
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Test proposal"),
        &target,
        &Symbol::new(&env, "update"),
    );
    dao.vote(&voter1, &pid, &VoteChoice::For);
    dao.vote(&voter1, &pid, &VoteChoice::Against);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn vote_after_period_fails() {
    let (env, dao, _, voter1, _, target) = setup();
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Test proposal"),
        &target,
        &Symbol::new(&env, "update"),
    );
    env.ledger().with_mut(|l| l.timestamp += VOTING_PERIOD + 1);
    dao.vote(&voter1, &pid, &VoteChoice::For);
}

// ── finalize ──────────────────────────────────────────────────────────────────

#[test]
fn proposal_passes_with_quorum_and_majority() {
    let (env, dao, _, voter1, voter2, target) = setup();
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Pass me"),
        &target,
        &Symbol::new(&env, "update"),
    );
    dao.vote(&voter1, &pid, &VoteChoice::For);
    dao.vote(&voter2, &pid, &VoteChoice::For);
    env.ledger().with_mut(|l| l.timestamp += VOTING_PERIOD + 1);
    dao.finalize(&pid);
    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, crate::ProposalStatus::Passed);
}

#[test]
fn proposal_fails_without_quorum() {
    let (env, dao, _, voter1, _, target) = setup();
    // total supply ≈ 1.6M, quorum 10% = 160k; voter1 has 1M but we make them abstain
    // we test without enough total votes relative to supply
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Quorum miss"),
        &target,
        &Symbol::new(&env, "update"),
    );
    // Don't vote — zero total votes, fails quorum
    env.ledger().with_mut(|l| l.timestamp += VOTING_PERIOD + 1);
    dao.finalize(&pid);
    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, crate::ProposalStatus::Failed);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn finalize_while_voting_open_fails() {
    let (env, dao, _, voter1, _, target) = setup();
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Too early"),
        &target,
        &Symbol::new(&env, "update"),
    );
    dao.finalize(&pid);
}

// ── execute / cancel ──────────────────────────────────────────────────────────

#[test]
fn execute_passed_proposal() {
    let (env, dao, _, voter1, voter2, target) = setup();
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Execute me"),
        &target,
        &Symbol::new(&env, "update"),
    );
    dao.vote(&voter1, &pid, &VoteChoice::For);
    dao.vote(&voter2, &pid, &VoteChoice::For);
    env.ledger().with_mut(|l| l.timestamp += VOTING_PERIOD + 1);
    dao.finalize(&pid);
    dao.execute(&pid);
    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, crate::ProposalStatus::Executed);
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn execute_failed_proposal_panics() {
    let (env, dao, _, voter1, _, target) = setup();
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Doomed"),
        &target,
        &Symbol::new(&env, "update"),
    );
    env.ledger().with_mut(|l| l.timestamp += VOTING_PERIOD + 1);
    dao.finalize(&pid);  // fails (no votes)
    dao.execute(&pid);
}

#[test]
fn admin_can_cancel_active_proposal() {
    let (env, dao, admin, voter1, _, target) = setup();
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Will be cancelled"),
        &target,
        &Symbol::new(&env, "update"),
    );
    dao.cancel(&admin, &pid);
    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, crate::ProposalStatus::Cancelled);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn non_admin_cannot_cancel() {
    let (env, dao, _, voter1, voter2, target) = setup();
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"No cancel"),
        &target,
        &Symbol::new(&env, "update"),
    );
    dao.cancel(&voter2, &pid);
}

// ── ghost proposal guard ───────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn execute_non_existent_proposal_fails() {
    let (_env, dao, _admin, _voter1, _voter2, _target) = setup();
    // No proposals have been created, so any ID > 0 is non-existent.
    dao.execute(&9999u64);
}
