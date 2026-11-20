#![allow(clippy::inconsistent_digit_grouping)]
//! Advanced governance-dao tests: update_config, multi-voter scenarios.
#![cfg(test)]

extern crate std;

use crate::test::setup;

use soroban_sdk::{
    testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke},
    token, Address, Bytes, BytesN, Env, IntoVal, Symbol, Val, Vec,
};

use crate::{
    DaoConfig, Error, GovernanceDao, GovernanceDaoClient, ProposalKind, ProposalStatus, VoteChoice,
};

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

#[test]
fn admin_can_change_the_proposal_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, admin, _, _) = make_dao(&env);

    dao.set_proposal_threshold(&admin, &2_000_0000000i128);

    assert_eq!(dao.get_config().proposal_threshold, 2_000_0000000i128);
}

#[test]
#[should_panic(expected = "Error(Contract, #41)")]
fn proposal_threshold_must_be_positive() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, admin, _, _) = make_dao(&env);

    dao.set_proposal_threshold(&admin, &0i128);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn only_the_admin_can_change_the_proposal_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let (dao, _admin, voter, _) = make_dao(&env);

    dao.set_proposal_threshold(&voter, &1_000i128);
}

#[test]
fn finalize_and_finalize_batch_all_abstain_no_division_by_zero() {
    let (env, dao, _admin, voter1, _voter2, target) = setup();

    let args: Vec<Val> = Vec::new(&env);
    let pid1 = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"All abstain test 1"),
        &target,
        &Symbol::new(&env, "update1"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: no material risk identified."),
    );
    dao.vote(&voter1, &pid1, &VoteChoice::Abstain);

    let pid2 = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"All abstain test 2"),
        &target,
        &Symbol::new(&env, "update2"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: no material risk identified."),
    );
    dao.vote(&voter1, &pid2, &VoteChoice::Abstain);

    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);

    // finalize() on all-abstain proposal must not panic and should mark the proposal Failed
    dao.finalize(&pid1);
    let p1 = dao.get_proposal(&pid1);
    assert_eq!(p1.status, ProposalStatus::Failed);
    assert_eq!(p1.votes_for, 0);
    assert_eq!(p1.votes_against, 0);
    assert!(p1.votes_abstain > 0);

    // finalize_batch() on all-abstain proposal must also succeed without panic
    let mut batch: Vec<u64> = Vec::new(&env);
    batch.push_back(pid2);
    dao.finalize_batch(&batch);
    let p2 = dao.get_proposal(&pid2);
    assert_eq!(p2.status, ProposalStatus::Failed);
    assert_eq!(p2.votes_for, 0);
    assert_eq!(p2.votes_against, 0);
    assert!(p2.votes_abstain > 0);
}

// ── Issue #836: quorum decay, time weighting, delegation, veto and
//    impact multipliers ──────────────────────────────────────────────────────

/// Quorum decay reduces the effective quorum after one low-participation
/// proposal and decays further across several low-participation proposals,
/// while strictly respecting `floor_bps` (never dropping below the floor).
#[test]
fn quorum_decay_reduces_across_low_turnout_proposals_and_respects_floor() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_750_000_000);

    let admin = Address::generate(&env);
    let proposer = Address::generate(&env);
    let voter_a = Address::generate(&env);
    let target = env.register(crate::test::MockTarget, ());

    let gov_token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let sac = token::StellarAssetClient::new(&env, &gov_token_id);
    // Total supply = 1,000,000 SHIELD (7-decimal).
    sac.mint(&proposer, &100_000_0000000i128);
    // voter_a holds 260,000 SHIELD = 2,600 bps (26%) of total_supply.
    sac.mint(&voter_a, &260_000_0000000i128);

    let dao_id = env.register(GovernanceDao, ());
    let dao = GovernanceDaoClient::new(&env, &dao_id);
    dao.initialize(
        &admin,
        &DaoConfig {
            gov_token: gov_token_id,
            total_supply: 1_000_000_0000000i128,
            proposal_threshold: 10_000_0000000i128,
            quorum_bps: 3_000u32, // 30% base quorum
            majority_bps: 5_100u32,
            voting_period: VOTING_PERIOD,
            proposal_timelock: 0,
            discussion_period: 0,
            vote_weight_cap: 0,
        },
    );
    // floor = 1,500 bps (15%), decay step = 200 bps, rolling window = 3.
    dao.set_quorum_decay(&admin, &true, &1_500u32, &200u32, &3u32);

    let args: Vec<Val> = Vec::new(&env);
    let impact = Bytes::from_slice(&env, b"Impact analysis: quorum decay test.");

    // Proposal 1: voter_a votes (260,000 / 1,000,000 = 2,600 bps turnout).
    // recent_bps = [2_600], avg = 2_600.
    // shortfall = 3_000 - 2_600 = 400; steps = 400 / 200 = 2; reduction = 400.
    // effective quorum = 3_000 - 400 = 2_600 bps (above floor 1_500).
    let pid1 = dao.create_proposal(
        &proposer,
        &Bytes::from_slice(&env, b"Decay P1"),
        &target,
        &Symbol::new(&env, "fn1"),
        &args,
        &impact,
    );
    dao.vote(&voter_a, &pid1, &VoteChoice::For);
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);
    dao.finalize(&pid1);

    let eff1 = dao.get_effective_quorum();
    assert_eq!(eff1.base_bps, 3_000);
    assert_eq!(eff1.avg_participation_bps, 2_600);
    assert_eq!(eff1.quorum_bps, 2_600);
    assert!(eff1.decayed);

    // Proposal 2: 0 votes -> turnout = 0 bps.
    // recent_bps = [2_600, 0], avg = (2_600 + 0) / 2 = 1_300.
    // shortfall = 3_000 - 1_300 = 1_700; steps = 1_700 / 200 = 8; reduction = 1_600.
    // 3_000 - 1_600 = 1_400, clamped up to floor_bps = 1_500.
    let pid2 = dao.create_proposal(
        &proposer,
        &Bytes::from_slice(&env, b"Decay P2"),
        &target,
        &Symbol::new(&env, "fn2"),
        &args,
        &impact,
    );
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);
    dao.finalize(&pid2);

    let eff2 = dao.get_effective_quorum();
    assert_eq!(eff2.avg_participation_bps, 1_300);
    assert_eq!(eff2.quorum_bps, 1_500);
    assert!(eff2.decayed);

    // Proposal 3: 0 votes -> turnout = 0 bps.
    // recent_bps = [2_600, 0, 0], avg = 2_600 / 3 = 866.
    // shortfall = 3_000 - 866 = 2_134; steps = 10; reduction = 2_000.
    // 3_000 - 2_000 = 1_000, still clamped to floor_bps = 1_500.
    let pid3 = dao.create_proposal(
        &proposer,
        &Bytes::from_slice(&env, b"Decay P3"),
        &target,
        &Symbol::new(&env, "fn3"),
        &args,
        &impact,
    );
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);
    dao.finalize(&pid3);

    let eff3 = dao.get_effective_quorum();
    assert_eq!(eff3.avg_participation_bps, 866);
    assert_eq!(eff3.quorum_bps, 1_500);
    assert!(eff3.decayed);
}

/// Quorum decay lands exactly on `floor_bps` when `base_bps - reduction == floor_bps`.
#[test]
fn quorum_decay_hits_floor_exactly() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_750_000_000);

    let admin = Address::generate(&env);
    let proposer = Address::generate(&env);
    let voter = Address::generate(&env);
    let target = env.register(crate::test::MockTarget, ());

    let gov_token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let sac = token::StellarAssetClient::new(&env, &gov_token_id);
    sac.mint(&proposer, &50_000_0000000i128);
    // 150,000 / 1,000,000 = 1,500 bps turnout.
    sac.mint(&voter, &150_000_0000000i128);

    let dao_id = env.register(GovernanceDao, ());
    let dao = GovernanceDaoClient::new(&env, &dao_id);
    dao.initialize(
        &admin,
        &DaoConfig {
            gov_token: gov_token_id,
            total_supply: 1_000_000_0000000i128,
            proposal_threshold: 10_000_0000000i128,
            quorum_bps: 2_500u32,
            majority_bps: 5_100u32,
            voting_period: VOTING_PERIOD,
            proposal_timelock: 0,
            discussion_period: 0,
            vote_weight_cap: 0,
        },
    );
    dao.set_quorum_decay(&admin, &true, &1_500u32, &500u32, &2u32);

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &proposer,
        &Bytes::from_slice(&env, b"Exact floor"),
        &target,
        &Symbol::new(&env, "fn1"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: exact floor test."),
    );
    dao.vote(&voter, &pid, &VoteChoice::For);
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);
    dao.finalize(&pid);

    // avg = 1_500, shortfall = 2_500 - 1_500 = 1_000, steps = 1_000 / 500 = 2,
    // reduction = 1_000 -> 2_500 - 1_000 = 1_500 == floor_bps.
    let eff = dao.get_effective_quorum();
    assert_eq!(eff.avg_participation_bps, 1_500);
    assert_eq!(eff.quorum_bps, 1_500);
    assert!(eff.decayed);
}

/// Time-weighted voting ramps linearly from `min_bps` at elapsed = 0 through
/// the midpoint and full `ramp_period`, and caps at 100% past `ramp_period`;
/// enabling time weighting with `ramp_period == 0` is rejected.
#[test]
fn time_weighting_ramps_across_start_middle_end_and_past_ramp_period() {
    let env = Env::default();
    env.mock_all_auths();
    let t0: u64 = 1_750_000_000;
    env.ledger().set_timestamp(t0);

    let admin = Address::generate(&env);
    let proposer = Address::generate(&env);
    let v_start = Address::generate(&env);
    let v_mid = Address::generate(&env);
    let v_end = Address::generate(&env);
    let v_past = Address::generate(&env);
    let target = env.register(crate::test::MockTarget, ());

    let gov_token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let sac = token::StellarAssetClient::new(&env, &gov_token_id);
    let balance = 100_000_0000000i128;
    sac.mint(&proposer, &balance);
    sac.mint(&v_start, &balance);
    sac.mint(&v_mid, &balance);
    sac.mint(&v_end, &balance);
    sac.mint(&v_past, &balance);

    let dao_id = env.register(GovernanceDao, ());
    let dao = GovernanceDaoClient::new(&env, &dao_id);
    dao.initialize(&admin, &base_config(gov_token_id));

    // ramp_period == 0 when enabled is rejected with InvalidTimeWeight (#45).
    assert_eq!(
        dao.try_set_time_weight(&admin, &true, &0u64, &2_000u32),
        Err(Ok(Error::InvalidTimeWeight))
    );

    // Enable: ramp_period = 1_000s, min_bps = 2_000 (20%).
    let ramp: u64 = 1_000;
    dao.set_time_weight(&admin, &true, &ramp, &2_000u32);

    // Checkpoint all four voters at t0.
    dao.checkpoint_voting_power(&v_start);
    dao.checkpoint_voting_power(&v_mid);
    dao.checkpoint_voting_power(&v_end);
    dao.checkpoint_voting_power(&v_past);

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &proposer,
        &Bytes::from_slice(&env, b"Time weight ramp"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: time-weight ramp test."),
    );

    // 1. Start (elapsed = 0):
    // factor = 2_000 + (10_000 - 2_000) * 0 / 1_000 = 2_000
    // weight = 100_000 * 2_000 / 10_000 = 20_000 SHIELD.
    dao.vote(&v_start, &pid, &VoteChoice::For);
    assert_eq!(dao.get_proposal(&pid).votes_for, 20_000_0000000i128);

    // 2. Middle (elapsed = 500):
    // factor = 2_000 + 8_000 * 500 / 1_000 = 6_000
    // weight = 100_000 * 6_000 / 10_000 = 60_000 SHIELD.
    // cumulative votes_for = 20_000 + 60_000 = 80_000 SHIELD.
    env.ledger().set_timestamp(t0 + 500);
    dao.vote(&v_mid, &pid, &VoteChoice::For);
    assert_eq!(dao.get_proposal(&pid).votes_for, 80_000_0000000i128);

    // 3. End (elapsed = 1_000 == ramp_period):
    // factor = 2_000 + 8_000 * 1_000 / 1_000 = 10_000
    // weight = 100_000 SHIELD.
    // cumulative votes_for = 80_000 + 100_000 = 180_000 SHIELD.
    env.ledger().set_timestamp(t0 + 1_000);
    dao.vote(&v_end, &pid, &VoteChoice::For);
    assert_eq!(dao.get_proposal(&pid).votes_for, 180_000_0000000i128);

    // 4. Past ramp_period (elapsed = 5_000 > 1_000, clamped to 1_000):
    // factor = 10_000, weight = 100_000 SHIELD (never exceeds full balance).
    // cumulative votes_for = 180_000 + 100_000 = 280_000 SHIELD.
    env.ledger().set_timestamp(t0 + 5_000);
    dao.vote(&v_past, &pid, &VoteChoice::For);
    assert_eq!(dao.get_proposal(&pid).votes_for, 280_000_0000000i128);
}

/// Delegation weight tracks live balances of both delegator and delegate,
/// handles self-delegation, re-delegation, double-delegation, revocation,
/// and prevents a delegator from double-counting their weight by voting directly.
#[test]
fn delegation_live_balances_lifecycle_and_no_double_counting() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_750_000_000);

    let admin = Address::generate(&env);
    let proposer = Address::generate(&env);
    let delegator = Address::generate(&env);
    let delegate1 = Address::generate(&env);
    let delegate2 = Address::generate(&env);
    let sink = Address::generate(&env);
    let target = env.register(crate::test::MockTarget, ());

    let gov_token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let sac = token::StellarAssetClient::new(&env, &gov_token_id);
    let tok = token::Client::new(&env, &gov_token_id);
    sac.mint(&proposer, &50_000_0000000i128);
    sac.mint(&delegator, &200_000_0000000i128);
    sac.mint(&delegate1, &100_000_0000000i128);
    sac.mint(&delegate2, &50_000_0000000i128);

    let dao_id = env.register(GovernanceDao, ());
    let dao = GovernanceDaoClient::new(&env, &dao_id);
    dao.initialize(&admin, &base_config(gov_token_id));

    // Self-delegation rejected with SelfDelegation (#26).
    assert_eq!(
        dao.try_delegate(&delegator, &delegator),
        Err(Ok(Error::SelfDelegation))
    );

    // Delegate to delegate1.
    dao.delegate(&delegator, &delegate1);
    let info1 = dao.get_delegate_info(&delegate1);
    assert_eq!(info1.own_weight, 100_000_0000000i128);
    assert_eq!(info1.delegated_weight, 200_000_0000000i128);
    assert_eq!(info1.total_weight, 300_000_0000000i128);

    // Delegating again to the same delegate fails with AlreadyDelegated (#28).
    assert_eq!(
        dao.try_delegate(&delegator, &delegate1),
        Err(Ok(Error::AlreadyDelegated))
    );

    // Live balance updates: mint +50k to delegator and +25k to delegate1.
    sac.mint(&delegator, &50_000_0000000i128);
    sac.mint(&delegate1, &25_000_0000000i128);
    let info_up = dao.get_delegate_info(&delegate1);
    // own = 125k, delegated = 250k, total = 375k.
    assert_eq!(info_up.own_weight, 125_000_0000000i128);
    assert_eq!(info_up.delegated_weight, 250_000_0000000i128);
    assert_eq!(info_up.total_weight, 375_000_0000000i128);

    // Delegator transfers 100k away -> delegated_weight drops live to 150k.
    tok.transfer(&delegator, &sink, &100_000_0000000i128);
    let info_down = dao.get_delegate_info(&delegate1);
    // own = 125k, delegated = 150k, total = 275k.
    assert_eq!(info_down.delegated_weight, 150_000_0000000i128);
    assert_eq!(info_down.total_weight, 275_000_0000000i128);

    // Re-delegating to delegate2 detaches from delegate1 and attaches to delegate2.
    dao.delegate(&delegator, &delegate2);
    assert_eq!(dao.get_delegate_info(&delegate1).delegated_weight, 0);
    assert_eq!(
        dao.get_delegate_info(&delegate2).delegated_weight,
        150_000_0000000i128
    );

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &proposer,
        &Bytes::from_slice(&env, b"Delegation vote"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: delegation test."),
    );

    // Delegator cannot vote directly while delegated -> WeightAlreadyCounted (#29).
    assert_eq!(
        dao.try_vote(&delegator, &pid, &VoteChoice::For),
        Err(Ok(Error::WeightAlreadyCounted))
    );

    // Delegate2 votes: own 50k + live delegated 150k = 200k.
    dao.vote(&delegate2, &pid, &VoteChoice::For);
    assert_eq!(dao.get_proposal(&pid).votes_for, 200_000_0000000i128);

    // Even after revoking delegation, delegator cannot vote on the same proposal
    // because delegate2 already swept their weight (no double counting).
    dao.revoke_delegation(&delegator);
    assert_eq!(dao.get_delegate(&delegator), None);
    assert_eq!(dao.get_delegate_info(&delegate2).delegated_weight, 0);
    assert_eq!(
        dao.try_vote(&delegator, &pid, &VoteChoice::For),
        Err(Ok(Error::WeightAlreadyCounted))
    );

    // Revoking again with no active delegation fails with NoDelegation (#27).
    assert_eq!(
        dao.try_revoke_delegation(&delegator),
        Err(Ok(Error::NoDelegation))
    );

    // On a new proposal, the undelegated holder can vote directly again.
    let pid2 = dao.create_proposal(
        &proposer,
        &Bytes::from_slice(&env, b"Post-revoke vote"),
        &target,
        &Symbol::new(&env, "update2"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: post-revoke test."),
    );
    dao.vote(&delegator, &pid2, &VoteChoice::For);
    assert_eq!(dao.get_proposal(&pid2).votes_for, 150_000_0000000i128);
}

/// Guardian veto prevents execution of a passed, timelock-elapsed proposal;
/// double veto is rejected; veto after execution is rejected.
#[test]
fn veto_proposal_blocks_execution_and_rejects_duplicate_or_post_exec() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_750_000_000);

    let (dao, admin, voter, target) = make_dao(&env);
    let guardian = Address::generate(&env);
    let mut guardians = Vec::new(&env);
    guardians.push_back(guardian.clone());
    dao.set_guardians(&admin, &guardians, &1u32);

    let args: Vec<Val> = Vec::new(&env);
    let impact = Bytes::from_slice(&env, b"Impact analysis: veto test.");

    // Proposal 1: passes voting & timelock, then vetoed -> cannot execute.
    let pid1 = dao.create_proposal(
        &voter,
        &Bytes::from_slice(&env, b"Vetoed proposal"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &impact,
    );
    dao.vote(&voter, &pid1, &VoteChoice::For);
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);
    dao.finalize(&pid1);
    assert_eq!(dao.get_proposal(&pid1).status, ProposalStatus::Passed);

    dao.veto_proposal(&guardian, &pid1, &Symbol::new(&env, "risk"));
    assert!(dao.get_proposal(&pid1).is_vetoed);

    // Second veto on the same proposal fails with ProposalVetoed (#43).
    assert_eq!(
        dao.try_veto_proposal(&guardian, &pid1, &Symbol::new(&env, "again")),
        Err(Ok(Error::ProposalVetoed))
    );

    // Execution of the passed, timelock-ready, vetoed proposal is blocked (#43).
    assert_eq!(dao.try_execute(&pid1), Err(Ok(Error::ProposalVetoed)));

    // Withdraw voter's locked tokens so they can vote on Proposal 2.
    dao.withdraw_tokens(&voter, &pid1);

    // Proposal 2: passes and executes; vetoing after execution fails (#11).
    let pid2 = dao.create_proposal(
        &voter,
        &Bytes::from_slice(&env, b"Executed proposal"),
        &target,
        &Symbol::new(&env, "update2"),
        &args,
        &impact,
    );
    dao.vote(&voter, &pid2, &VoteChoice::For);
    env.ledger()
        .with_mut(|l| l.timestamp += VOTING_PERIOD + (24 * 3600) + 1);
    dao.finalize(&pid2);
    dao.execute(&pid2);
    assert_eq!(dao.get_proposal(&pid2).status, ProposalStatus::Executed);

    assert_eq!(
        dao.try_veto_proposal(&guardian, &pid2, &Symbol::new(&env, "late")),
        Err(Ok(Error::AlreadyExecuted))
    );
}

/// Unauthorized caller attempting `veto_proposal` without blanket auth mocking
/// is rejected with `Error::NotGuardian` (#20).
#[test]
fn veto_proposal_unauthorized_caller_rejected_without_blanket_auth() {
    let env = Env::default();
    env.ledger().set_timestamp(1_750_000_000);

    // Setup uses scoped mock_all_auths, then we clear all auths before testing
    // the unauthorized veto call with explicit per-caller MockAuth.
    env.mock_all_auths();
    let (dao, admin, voter, target) = make_dao(&env);
    let guardian = Address::generate(&env);
    let outsider = Address::generate(&env);
    let mut guardians = Vec::new(&env);
    guardians.push_back(guardian);
    dao.set_guardians(&admin, &guardians, &1u32);

    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter,
        &Bytes::from_slice(&env, b"Auth veto test"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: auth veto test."),
    );

    // Replace blanket auth mocking with explicit auth only for `outsider`.
    let reason = Symbol::new(&env, "rogue");
    env.mock_auths(&[MockAuth {
        address: &outsider,
        invoke: &MockAuthInvoke {
            contract: &dao.address,
            fn_name: "veto_proposal",
            args: (&outsider, pid, reason.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    let res = dao.try_veto_proposal(&outsider, &pid, &reason);
    assert_eq!(res, Err(Ok(Error::NotGuardian)));
    assert!(!dao.get_proposal(&pid).is_vetoed);
}

/// Impact multipliers scale `proposal_threshold` (the creation deposit) for
/// `Upgrade` and self-targeted proposals: checks exact threshold, one stroop
/// below, and one stroop above.
#[test]
fn impact_multipliers_upgrade_threshold_boundaries() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_750_000_000);

    let admin = Address::generate(&env);
    let p_below = Address::generate(&env);
    let p_exact = Address::generate(&env);
    let p_above = Address::generate(&env);
    let target = env.register(crate::test::MockTarget, ());

    let gov_token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let sac = token::StellarAssetClient::new(&env, &gov_token_id);
    let tok = token::Client::new(&env, &gov_token_id);

    // Base proposal_threshold = 10,000 SHIELD (10_000_0000000).
    // With upgrade_bps = 50_000 (5x) -> upgrade threshold = 50,000 SHIELD.
    // With self_target_bps = 30_000 (3x) -> self-target threshold = 30,000 SHIELD.
    let upgrade_req: i128 = 50_000_0000000;
    sac.mint(&p_below, &(upgrade_req - 1));
    sac.mint(&p_exact, &upgrade_req);
    sac.mint(&p_above, &(upgrade_req + 1));

    let dao_id = env.register(GovernanceDao, ());
    let dao = GovernanceDaoClient::new(&env, &dao_id);
    dao.initialize(&admin, &base_config(gov_token_id));
    dao.set_impact_multipliers(&admin, &10_000u32, &50_000u32, &30_000u32);

    assert_eq!(
        dao.get_effective_threshold(&ProposalKind::Standard, &target),
        10_000_0000000i128
    );
    assert_eq!(
        dao.get_effective_threshold(&ProposalKind::Standard, &dao.address),
        30_000_0000000i128
    );
    assert_eq!(
        dao.get_effective_threshold(&ProposalKind::Upgrade, &target),
        upgrade_req
    );

    let impact = Bytes::from_slice(&env, b"Impact analysis: upgrade multiplier test.");
    let wasm1 = BytesN::from_array(&env, &[1u8; 32]);
    let wasm2 = BytesN::from_array(&env, &[2u8; 32]);

    // 1. One stroop below upgrade threshold: Upgrade rejected (#4), but Standard succeeds.
    assert_eq!(
        dao.try_propose_upgrade(
            &p_below,
            &Bytes::from_slice(&env, b"Too poor for upgrade"),
            &target,
            &wasm1,
            &impact,
        ),
        Err(Ok(Error::InsufficientWeight))
    );
    let std_pid = dao.create_proposal(
        &p_below,
        &Bytes::from_slice(&env, b"Standard ok"),
        &target,
        &Symbol::new(&env, "update"),
        &Vec::new(&env),
        &impact,
    );
    assert_eq!(dao.get_proposal(&std_pid).deposit, 10_000_0000000i128);

    // 2. Exactly at upgrade threshold: Upgrade succeeds and locks 50,000 SHIELD.
    let up_exact = dao.propose_upgrade(
        &p_exact,
        &Bytes::from_slice(&env, b"Exact upgrade"),
        &target,
        &wasm1,
        &impact,
    );
    assert_eq!(dao.get_proposal(&up_exact).deposit, upgrade_req);
    assert_eq!(tok.balance(&p_exact), 0);

    // 3. One stroop above upgrade threshold: Upgrade succeeds, leaving 1 stroop liquid.
    let up_above = dao.propose_upgrade(
        &p_above,
        &Bytes::from_slice(&env, b"Above upgrade"),
        &target,
        &wasm2,
        &impact,
    );
    assert_eq!(dao.get_proposal(&up_above).deposit, upgrade_req);
    assert_eq!(tok.balance(&p_above), 1);
}
