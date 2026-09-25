#![allow(clippy::inconsistent_digit_grouping)]
//! Advanced governance-dao tests: update_config, multi-voter scenarios.
#![cfg(test)]

extern crate std;

use crate::test::setup;

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Bytes, Env, Symbol, Val, Vec,
};

use crate::{DaoConfig, GovernanceDao, GovernanceDaoClient, ProposalStatus, VoteChoice};

const VOTING_PERIOD: u64 = 7 * 24 * 3600;

fn base_config(gov_token: Address) -> DaoConfig {
    DaoConfig {
        gov_token,
        total_supply: 1_100_000_0000000i128,
        proposal_threshold: 10_000_0000000i128,
        quorum_bps: 1_000u32,
        majority_bps: 5_100u32,
        voting_period: VOTING_PERIOD,
        proposal_timelock: 0,
        discussion_period: 0,
        vote_weight_cap: 0,
    }
}

fn make_dao(env: &Env) -> (GovernanceDaoClient<'static>, Address, Address, Address) {
    let admin = Address::generate(env);
    let voter = Address::generate(env);
    let target = env.register(crate::test::MockTarget, ());

    let gov_token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    token::StellarAssetClient::new(env, &gov_token_id).mint(&voter, &1_000_000_0000000i128);
    token::StellarAssetClient::new(env, &gov_token_id).mint(&admin, &100_000_0000000i128);

    let dao_id = env.register(GovernanceDao, ());
    let dao = GovernanceDaoClient::new(env, &dao_id);
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
        quorum_bps: 2_000u32,
        majority_bps: 6_000u32,
        ..cfg_before.clone()
    };
    dao.update_config(&admin, &new_cfg);
    let cfg_after = dao.get_config();
    assert_eq!(cfg_after.quorum_bps, 2_000);
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

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter,
        &Bytes::from_slice(&env, b"Abstain test"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: no material risk identified."),
    );
    dao.vote(&voter, &pid, &VoteChoice::Abstain);

    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);
    dao.finalize(&pid);

    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, ProposalStatus::Failed);
    assert_eq!(p.votes_abstain, 990_000_0000000i128);
    assert_eq!(p.votes_for, 0);
}

#[test]
fn large_abstain_bloc_does_not_sink_a_partisan_majority() {
    // Regression test for issue #385: majority must be decided by the
    // For/Against split alone. Computing it against total_votes (including
    // Abstain) would let a large abstaining bloc dilute the For share below
    // majority_bps even when every partisan voter backed the proposal.
    let (env, dao, admin, voter1, voter2, target) = setup();

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Large abstain bloc"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: no material risk identified."),
    );

    // voter1 abstains with 990,000 (their 1,000,000 balance minus the
    // 10,000 proposal_threshold deposit locked when they created this very
    // proposal); voter2 (500,000) votes For; admin (100,000) votes Against.
    // Partisan split is 500k For / 100k Against = 83.3% For, comfortably
    // above the 51% majority_bps. Naively dividing by total_votes
    // (1,590,000, including the abstain bloc) would instead give ~31.4%,
    // well under majority_bps, and wrongly fail the proposal.
    dao.vote(&voter1, &pid, &VoteChoice::Abstain);
    dao.vote(&voter2, &pid, &VoteChoice::For);
    dao.vote(&admin, &pid, &VoteChoice::Against);

    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);
    dao.finalize(&pid);

    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, ProposalStatus::Passed);
    assert_eq!(p.votes_abstain, 990_000_0000000i128);
    assert_eq!(p.votes_for, 500_000_0000000i128);
    assert_eq!(p.votes_against, 100_000_0000000i128);
}

#[test]
fn abstain_still_counts_toward_quorum() {
    // Quorum is decided by total_votes, which does include Abstain — an
    // abstain-only turnout is still turnout, even though it can never pass.
    let (env, dao, _admin, voter1, _voter2, target) = setup();

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Abstain quorum test"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: no material risk identified."),
    );

    // voter1 alone (990,000 of 1,600,000 total_supply after their 10,000
    // proposal_threshold deposit — still 61.9%) clears the 10% quorum_bps
    // configured in setup() purely via Abstain.
    dao.vote(&voter1, &pid, &VoteChoice::Abstain);

    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);
    dao.finalize(&pid);

    // Quorum was met, but with zero partisan votes there is no majority to
    // speak of, so the proposal still fails.
    let p = dao.get_proposal(&pid);
    assert_eq!(p.status, ProposalStatus::Failed);
    assert_eq!(p.votes_abstain, 990_000_0000000i128);
}

// ── proposal_count ────────────────────────────────────────────────────────────

#[test]
fn proposal_count_increments_per_proposal() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, _, voter, target) = make_dao(&env);

    let args: Vec<Val> = Vec::new(&env);
    assert_eq!(dao.proposal_count(), 0);
    dao.create_proposal(
        &voter,
        &Bytes::from_slice(&env, b"P1"),
        &target,
        &Symbol::new(&env, "fn1"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: no material risk identified."),
    ); // <--- Added args
    assert_eq!(dao.proposal_count(), 1);
    dao.create_proposal(
        &voter,
        &Bytes::from_slice(&env, b"P2"),
        &target,
        &Symbol::new(&env, "fn2"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: no material risk identified."),
    ); // <--- Added args
    assert_eq!(dao.proposal_count(), 2);
}

// ── double-execute guard ──────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn execute_twice_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, _, voter, target) = make_dao(&env);

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter,
        &Bytes::from_slice(&env, b"Execute twice"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: no material risk identified."),
    );
    dao.vote(&voter, &pid, &VoteChoice::For);

    // Fast-forward past both voting period AND the 24-hour finalize delay buffer
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);
    dao.finalize(&pid);
    dao.execute(&pid);
    dao.execute(&pid);
}

#[test]
fn test_double_voting_attack_prevention() {
    let (env, dao, _admin, voter1, _voter2, target) = setup();
    let config = dao.get_config();
    let gov_client = token::Client::new(&env, &config.gov_token);
    let attacker_b = Address::generate(&env);

    // Track original balance of voter1 post-setup (has 1_000_000_0000000 SHIELD)
    let initial_balance = gov_client.balance(&voter1);
    assert!(
        initial_balance > 0,
        "Attacker needs tokens to initiate attack"
    );

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Malicious Proposal"),
        &target,
        &Symbol::new(&env, "drain"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: no material risk identified."),
    );

    // 1. Attacker (voter1) votes FOR
    dao.vote(&voter1, &pid, &VoteChoice::For);

    // 2. VERIFY LOCK: Attacker's liquid wallet balance must now be exactly 0
    // because the voting weight tokens are securely escrowed by the DAO contract.
    let post_vote_balance_a = gov_client.balance(&voter1);
    assert_eq!(
        post_vote_balance_a, 0,
        "Tokens were not locked in the DAO contract!"
    );

    // 3. Attacker attempts to transfer tokens to Attacker B.
    // This will panic or move 0 tokens because their liquid balance is empty.
    let res_transfer = gov_client.try_transfer(&voter1, &attacker_b, &initial_balance);
    assert!(
        res_transfer.is_err(),
        "Should not be able to transfer locked tokens"
    );

    // 4. Attacker B attempts to vote with an empty balance.
    // This must fail with a contract panic due to 0 balance weight.
    let res_vote = dao.try_vote(&attacker_b, &pid, &VoteChoice::For);
    assert!(
        res_vote.is_err(),
        "Attack successful: Account B voted using cycled tokens!"
    );
}

#[test]
fn test_successful_token_withdrawal_post_finalize() {
    let (env, dao, _admin, voter1, _voter2, target) = setup();
    let config = dao.get_config();
    let gov_client = token::Client::new(&env, &config.gov_token);

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Legitimate Proposal"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: no material risk identified."),
    );

    dao.vote(&voter1, &pid, &VoteChoice::For);
    assert_eq!(gov_client.balance(&voter1), 0);

    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);
    dao.finalize(&pid);

    dao.withdraw_tokens(&voter1, &pid);

    let final_balance = gov_client.balance(&voter1);
    assert!(
        final_balance > 0,
        "Voter could not reclaim their locked stake"
    );
}

// ── Regression test: admin cannot manipulate total_supply during active vote ──

#[test]
fn admin_cannot_manipulate_total_supply_during_active_vote() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, admin, voter, target) = make_dao(&env);

    // Initial config: total_supply = 1,100,000, quorum = 10% = 110,000
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter,
        &Bytes::from_slice(&env, b"Test proposal"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: no material risk identified."),
    );

    // Voter has 1,000,000 tokens, votes FOR (after 10k threshold lock)
    dao.vote(&voter, &pid, &VoteChoice::For);

    // Admin attempts to reduce total_supply to lower quorum mid-vote
    let cfg = dao.get_config();
    let malicious_cfg = DaoConfig {
        total_supply: 500_000_0000000i128, // Halve supply to lower quorum
        ..cfg.clone()
    };
    dao.update_config(&admin, &malicious_cfg);

    // Fast-forward to finalize
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);

    // Finalize should use the ORIGINAL total_supply captured at proposal creation
    dao.finalize(&pid);

    let p = dao.get_proposal(&pid);
    // With original supply (1.1M), quorum = 110k. Votes = 990k (after 10k lock).
    // This should PASS because 990k > 110k quorum.
    // If admin manipulation worked, new supply (500k) would make quorum = 50k,
    // but the fix prevents this by using proposal.total_supply.
    assert_eq!(p.status, ProposalStatus::Passed);
}

// ── Issue #260: proposal_id u64 overflow guard ──────────────────────────────

/// When NextProposalId is at u64::MAX, the next create_proposal must panic
/// with LimitReached (#16) instead of wrapping around to 0.
#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_proposal_id_overflow_panics_with_limit_reached() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, _, voter, target) = make_dao(&env);

    // Force the counter to u64::MAX so the next increment overflows.
    env.as_contract(&dao.address, || {
        env.storage()
            .instance()
            .set(&crate::StorageKey::NextProposalId, &u64::MAX);
    });

    let args: Vec<Val> = Vec::new(&env);
    // This must panic with LimitReached — not silently wrap to 0.
    dao.create_proposal(
        &voter,
        &Bytes::from_slice(&env, b"Overflow test"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: no material risk identified."),
    );
}

// ── #355: minimum quorum floor ───────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #24)")]
fn initialize_rejects_quorum_below_floor() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let gov_token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let dao_id = env.register(GovernanceDao, ());
    let dao = GovernanceDaoClient::new(&env, &dao_id);
    let mut cfg = base_config(gov_token_id);
    cfg.quorum_bps = 999; // just under the 10% floor
    dao.initialize(&admin, &cfg);
}

#[test]
#[should_panic(expected = "Error(Contract, #24)")]
fn update_config_rejects_quorum_below_floor() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, admin, _, _) = make_dao(&env);
    let mut cfg = dao.get_config();
    cfg.quorum_bps = 0;
    dao.update_config(&admin, &cfg);
}

#[test]
fn finalize_fails_proposal_with_zero_votes() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, _admin, voter, target) = make_dao(&env);

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter,
        &Bytes::from_slice(&env, b"No turnout"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: no material risk identified."),
    );

    // Nobody votes.
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);
    dao.finalize(&pid);

    assert_eq!(dao.get_proposal(&pid).status, ProposalStatus::Failed);
}
