#![allow(clippy::inconsistent_digit_grouping)]
#![allow(unused_variables)]
#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Bytes, Env, Symbol, Val, Vec,
};

use crate::{DaoConfig, GovernanceDao, GovernanceDaoClient, ProposalStatus, VoteChoice};

// ── helpers ───────────────────────────────────────────────────────────────────

const VOTING_PERIOD: u64 = 7 * 24 * 3600; // 7 days in seconds

#[soroban_sdk::contract]
pub struct MockTarget;

#[soroban_sdk::contractimpl]
impl MockTarget {
    pub fn update(_env: Env) {}
    pub fn upgrade(_env: Env) {}
}

pub fn setup() -> (
    Env,
    GovernanceDaoClient<'static>,
    Address,
    Address,
    Address,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let voter1 = Address::generate(&env);
    let voter2 = Address::generate(&env);
    let target = env.register(MockTarget, ());

    let gov_token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let gov_client = token::StellarAssetClient::new(&env, &gov_token_id);

    // mint voting power
    gov_client.mint(&voter1, &1_000_000_0000000i128);
    gov_client.mint(&voter2, &500_000_0000000i128);
    gov_client.mint(&admin, &100_000_0000000i128);

    let dao_id = env.register(GovernanceDao, ());
    let dao = GovernanceDaoClient::new(&env, &dao_id);

    dao.initialize(
        &admin,
        &DaoConfig {
            gov_token: gov_token_id,
            total_supply: 1_600_000_0000000i128,
            proposal_threshold: 10_000_0000000i128, // 10k SHIELD
            quorum_bps: 1_000u32,                   // 10%
            majority_bps: 5_100u32,                 // 51%
            voting_period: VOTING_PERIOD,
            proposal_timelock: 0,
        },
    );

    (env, dao, admin, voter1, voter2, target)
}

// ── initialization ────────────────────────────────────────────────────────────

#[test]
fn initialize_stores_config() {
    let (_env, dao, admin, _, _, _target) = setup();
    let cfg = dao.get_config();
    assert_eq!(cfg.quorum_bps, 1_000);
    assert_eq!(cfg.majority_bps, 5_100);
    assert_eq!(dao.get_admin(), admin);
    assert_eq!(dao.proposal_count(), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn cannot_initialize_twice() {
    let (env, dao, admin, _, _, _) = setup();
    let gov_token = dao.get_config().gov_token;
    let _target = Address::generate(&env);
    dao.initialize(
        &admin,
        &DaoConfig {
            gov_token,
            total_supply: 0,
            proposal_threshold: 0,
            quorum_bps: 0,
            majority_bps: 0,
            voting_period: 0,
            proposal_timelock: 0,
        },
    );
}

// ── proposal creation ─────────────────────────────────────────────────────────

#[test]
fn create_proposal_increments_counter() {
    let (env, dao, _, voter1, _, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let id = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Lower quorum to 5%"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );
    assert_eq!(id, 0u64);
    assert_eq!(dao.proposal_count(), 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn create_proposal_below_threshold_fails() {
    let (env, dao, _, _, _, target) = setup();
    let nobody = Address::generate(&env);
    let args: Vec<Val> = Vec::new(&env);
    dao.create_proposal(
        &nobody,
        &Bytes::from_slice(&env, b"Sneaky proposal"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );
}

// ── voting ────────────────────────────────────────────────────────────────────

#[test]
fn vote_for_records_weight() {
    let (env, dao, _, voter1, _, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Test proposal"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );
    dao.vote(&voter1, &pid, &VoteChoice::For);
    let rec = dao.get_vote(&pid, &voter1).unwrap();

    assert_eq!(rec.weight, 990_000_0000000i128);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn double_vote_fails() {
    let (env, dao, _, voter1, _, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Test proposal"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );
    dao.vote(&voter1, &pid, &VoteChoice::For);
    dao.vote(&voter1, &pid, &VoteChoice::Against);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn vote_after_period_fails() {
    let (env, dao, _, voter1, _, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Test proposal"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );
    env.ledger().with_mut(|l| l.timestamp += VOTING_PERIOD + 1);
    dao.vote(&voter1, &pid, &VoteChoice::For);
}

// ── finalize ──────────────────────────────────────────────────────────────────

#[test]
fn proposal_passes_with_quorum_and_majority() {
    let (env, dao, _, voter1, voter2, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Pass me"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );
    dao.vote(&voter1, &pid, &VoteChoice::For);
    dao.vote(&voter2, &pid, &VoteChoice::For);

    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);

    dao.finalize(&pid);
    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, crate::ProposalStatus::Passed);
}

#[test]
fn proposal_fails_without_quorum() {
    let (env, dao, _, voter1, _, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Quorum miss"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );

    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);

    dao.finalize(&pid);
    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, crate::ProposalStatus::Failed);
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn finalize_while_voting_open_fails() {
    let (env, dao, _, voter1, _, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Too early"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );
    dao.finalize(&pid);
}

// ── execute / cancel ──────────────────────────────────────────────────────────

#[test]
fn execute_passed_proposal() {
    let (env, dao, _, voter1, voter2, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Execute me"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );
    dao.vote(&voter1, &pid, &VoteChoice::For);
    dao.vote(&voter2, &pid, &VoteChoice::For);

    // Fast-forward past both voting period AND the 24-hour finalize delay buffer
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);

    dao.finalize(&pid);
    dao.execute(&pid);
    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, crate::ProposalStatus::Executed);
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn execute_failed_proposal_panics() {
    let (env, dao, _, voter1, _, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Doomed"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );

    // Fast-forward past both voting period AND the 24-hour finalize delay buffer
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);

    dao.finalize(&pid); // fails (no votes)
    dao.execute(&pid);
}

#[test]
fn admin_can_cancel_active_proposal() {
    let (env, dao, admin, voter1, _, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Will be cancelled"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );
    dao.cancel(&admin, &pid);
    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, crate::ProposalStatus::Cancelled);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn non_admin_cannot_cancel() {
    let (env, dao, _, voter1, voter2, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"No cancel"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );
    dao.cancel(&voter2, &pid);
}

// ── ghost proposal guard ───────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn execute_non_existent_proposal_fails() {
    let (_env, dao, _admin, _voter1, _voter2, _target) = setup();
    dao.execute(&9999u64);
}

#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_proposal_timelock_execution() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let voter1 = Address::generate(&env);
    let target = env.register(MockTarget, ());
    let gov_token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let gov_client = token::StellarAssetClient::new(&env, &gov_token_id);
    gov_client.mint(&voter1, &1_000_000_0000000i128);

    let dao_id = env.register(GovernanceDao, ());
    let dao = GovernanceDaoClient::new(&env, &dao_id);

    dao.initialize(
        &admin,
        &DaoConfig {
            gov_token: gov_token_id,
            total_supply: 1_000_000_0000000i128,
            proposal_threshold: 10_000_0000000i128,
            quorum_bps: 1_000u32,
            majority_bps: 5_100u32,
            voting_period: 604800,
            proposal_timelock: 604800,
        },
    );

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Upgrade Admin Address"),
        &target,
        &Symbol::new(&env, "upgrade"),
        &args,
    );

    dao.vote(&voter1, &pid, &VoteChoice::For);

    // Fast forward past voting_period (604800) AND the 24-hour buffer delay (86400) + 1
    env.ledger().with_mut(|l| l.timestamp = 604800 + 86400 + 1);
    dao.finalize(&pid);

    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, ProposalStatus::Passed);

    // Attempt to execute immediately (timelock not expired)
    dao.execute(&pid);
}

#[test]
fn test_finalize_cooldown_delay_enforced() {
    let (env, dao, _admin, voter1, _voter2, target) = setup();

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Race Condition Test"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );

    dao.vote(&voter1, &pid, &VoteChoice::For);

    // 1. Fast forward to exactly 1 second after voting period ends (cooldown active)
    env.ledger().with_mut(|l| l.timestamp += VOTING_PERIOD + 1);

    // 2. Attempting to finalize right away must fail with FinalizeDelayNotMet (Error #14)
    let res_early = dao.try_finalize(&pid);
    assert!(
        res_early.is_err(),
        "Vulnerability exists: finalized could be raced immediately!"
    );

    // 3. Fast forward past the 24-hour grace period buffer
    env.ledger().with_mut(|l| l.timestamp += 24 * 3600);

    // 4. Settle proposal after buffer window expires — should succeed cleanly
    let res_delayed = dao.try_finalize(&pid);
    assert!(
        res_delayed.is_ok(),
        "Failed to finalize after cooldown expired"
    );
}

// ── Issue #45: execute_proposal approval verification ─────────────────────────

/// Test that an Active proposal (not yet voted on) cannot be executed.
/// This prevents malicious executors from executing proposals that were never approved.
#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_execute_active_proposal_without_voting_panics() {
    let (env, dao, _, voter1, _, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Unapproved proposal"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );

    // Attempt to execute without any voting - should panic with ProposalNotPassed
    dao.execute(&pid);
}

/// Test that a proposal with votes_against > votes_for cannot be executed
/// even after the voting period ends (it will be Failed, not Passed).
#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_execute_rejected_proposal_panics() {
    let (env, dao, _, voter1, voter2, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Rejected proposal"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );

    // Vote against the proposal
    dao.vote(&voter1, &pid, &VoteChoice::Against);
    dao.vote(&voter2, &pid, &VoteChoice::Against);

    // Fast-forward past voting period and finalize delay
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);

    dao.finalize(&pid);
    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, crate::ProposalStatus::Failed);

    // Attempt to execute a failed proposal - should panic
    dao.execute(&pid);
}

// ── Issue #165: tied vote boundary ────────────────────────────────────────────

/// When votes_for == votes_against exactly, `for_bps` lands at 50% which is
/// below `majority_bps` (51%), so the proposal must be marked Failed — not
/// Passed. This pins down the exact-tie boundary behavior of finalize().
#[test]
fn finalize_with_exactly_tied_votes_fails() {
    let (env, dao, _, voter1, _, target) = setup();
    let gov_token_id = dao.get_config().gov_token;

    // Two fresh voters with identical weight, voting on opposite sides.
    let voter_a = Address::generate(&env);
    let voter_b = Address::generate(&env);
    let tied_weight = 300_000_0000000i128;
    let gov_asset_client = token::StellarAssetClient::new(&env, &gov_token_id);
    gov_asset_client.mint(&voter_a, &tied_weight);
    gov_asset_client.mint(&voter_b, &tied_weight);

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Tied vote test"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );

    dao.vote(&voter_a, &pid, &VoteChoice::For);
    dao.vote(&voter_b, &pid, &VoteChoice::Against);

    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);

    dao.finalize(&pid);
    let p = dao.get_proposal(&pid);
    assert_eq!(p.votes_for, p.votes_against);
    assert_eq!(p.status, ProposalStatus::Failed);
}

// ── Issue #137: finalize() must refund the deposit locked at creation ─────────

/// Admin lowers proposal_threshold after a proposal is created but before
/// finalize() runs. The proposer must still get back exactly what they
/// locked at creation time, not the new (lower) live threshold.
#[test]
fn test_finalize_refunds_deposit_locked_at_creation_not_live_config() {
    let (env, dao, admin, voter1, voter2, target) = setup();

    let deposit_before: i128 = 10_000_0000000i128; // matches setup()'s proposal_threshold
    let balance_before = token::Client::new(&env, &dao.get_config().gov_token).balance(&voter1);

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Deposit refund test"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );

    // Deposit was actually taken.
    let gov_token = token::Client::new(&env, &dao.get_config().gov_token);
    assert_eq!(gov_token.balance(&voter1), balance_before - deposit_before);

    // Admin lowers the live threshold to 1 stroop before finalize.
    let mut new_config = dao.get_config();
    new_config.proposal_threshold = 1;
    dao.update_config(&admin, &new_config);

    dao.vote(&voter1, &pid, &VoteChoice::For);
    dao.vote(&voter2, &pid, &VoteChoice::For);
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);

    dao.finalize(&pid);

    // Must have gotten back the full original deposit (10k SHIELD), not
    // the live (lowered) threshold of 1 stroop.
    assert_eq!(gov_token.balance(&voter1), balance_before);

    let p = dao.get_proposal(&pid);
    assert_eq!(p.deposit, deposit_before);
}

/// Admin raises proposal_threshold after creation. finalize() must still
/// only move the amount actually locked at creation, not attempt to pull
/// the new higher threshold out of the contract's balance.
#[test]
fn test_finalize_does_not_pull_raised_live_threshold() {
    let (env, dao, admin, voter1, voter2, target) = setup();

    let deposit_before: i128 = 10_000_0000000i128;

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Deposit overpull test"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );

    // Admin raises the live threshold well above what the contract holds.
    let mut new_config = dao.get_config();
    new_config.proposal_threshold = 10_000_000_0000000i128;
    dao.update_config(&admin, &new_config);

    dao.vote(&voter1, &pid, &VoteChoice::For);
    dao.vote(&voter2, &pid, &VoteChoice::For);
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);

    // Must succeed — refunds the original deposit, not the raised threshold.
    dao.finalize(&pid);

    let p = dao.get_proposal(&pid);
    assert_eq!(p.deposit, deposit_before);
    assert_eq!(p.status, ProposalStatus::Passed);
}

#[test]
fn test_proposal_expires_after_voting_period() {
    let (env, dao, _, voter1, _, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Expire proposal"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );
    // Move time past the voting period AND finalize delay buffer (24h)
    env.ledger().with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);
    
    // Finalize it without votes, it should fail
    dao.finalize(&pid);
    
    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, crate::ProposalStatus::Failed);
}

#[test]
fn test_cancel_refunds_deposit_to_proposer() {
    let (env, dao, admin, voter1, _, target) = setup();
    
    let config = dao.get_config();
    let gov_token = token::Client::new(&env, &config.gov_token);
    let balance_before = gov_token.balance(&voter1);
    
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Refund proposal"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
    );
    
    assert_eq!(gov_token.balance(&voter1), balance_before - config.proposal_threshold);
    
    dao.cancel(&admin, &pid);
    
    assert_eq!(gov_token.balance(&voter1), balance_before);
}
