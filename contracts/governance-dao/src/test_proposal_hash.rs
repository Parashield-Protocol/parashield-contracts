use super::*;
use soroban_sdk::{
    contract, contractimpl, testutils::{Address as _, Ledger}, Address, Bytes, Env, Symbol, Val, Vec,
};

#[contract]
struct MockTarget;

#[contractimpl]
impl MockTarget {
    pub fn update(_env: Env) -> u32 {
        10_000
    }
    pub fn drain(_env: Env) -> u32 {
        10_000
    }
}

fn setup() -> (Env, GovernanceDaoClient<'static>, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let voter1 = Address::generate(&env);
    let voter2 = Address::generate(&env);
    let target = env.register(MockTarget, ());

    let gov_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let token_admin = soroban_sdk::token::StellarAssetClient::new(&env, &gov_token);
    token_admin.mint(&voter1, &10_000_0000000i128);
    token_admin.mint(&voter2, &10_000_0000000i128);

    let dao_id = env.register(GovernanceDao, ());
    let dao = GovernanceDaoClient::new(&env, &dao_id);

    let config = DaoConfig {
        gov_token,
        total_supply: 20_000_0000000i128,
        proposal_threshold: 1_000_0000000i128,
        quorum_bps: 2_000,
        majority_bps: 5_001,
        voting_period: 3_600 * 24 * 7,
        proposal_timelock: 0,
        discussion_period: 0,
        vote_weight_cap: 0,
    };
    dao.initialize(&admin, &config);

    (env, dao, admin, voter1, voter2, target)
}

#[test]
fn test_proposal_hash_verified_on_execution_success() {
    let (env, dao, _, voter1, voter2, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Legitimate proposal"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: verified safe."),
    );

    // Stored hash must be queryable and non-empty
    let stored_hash = dao.get_proposal_hash(&pid);
    assert_ne!(stored_hash, soroban_sdk::BytesN::from_array(&env, &[0u8; 32]));

    // Pass and finalize
    dao.vote(&voter1, &pid, &VoteChoice::For);
    dao.vote(&voter2, &pid, &VoteChoice::For);
    env.ledger().with_mut(|l| l.timestamp += 7 * 86_400 + 301);
    dao.finalize(&pid);

    // Execution verifies hash matches payload and succeeds
    dao.execute(&pid);
    let proposal = dao.get_proposal(&pid);
    assert_eq!(proposal.status, ProposalStatus::Executed);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_tampered_proposal_target_rejected_on_execution() {
    let (env, dao, _, voter1, voter2, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Tamper target"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: verified safe."),
    );

    dao.vote(&voter1, &pid, &VoteChoice::For);
    dao.vote(&voter2, &pid, &VoteChoice::For);
    env.ledger().with_mut(|l| l.timestamp += 7 * 86_400 + 301);
    dao.finalize(&pid);

    // Tamper with target contract address in storage
    let rogue_target = Address::generate(&env);
    env.as_contract(&dao.address, || {
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(pid))
            .unwrap();
        proposal.target = rogue_target;
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(pid), &proposal);
    });

    // Execution MUST reject mismatched hash (#771)
    dao.execute(&pid);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_tampered_proposal_function_rejected_on_execution() {
    let (env, dao, _, voter1, voter2, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Tamper function"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: verified safe."),
    );

    dao.vote(&voter1, &pid, &VoteChoice::For);
    dao.vote(&voter2, &pid, &VoteChoice::For);
    env.ledger().with_mut(|l| l.timestamp += 7 * 86_400 + 301);
    dao.finalize(&pid);

    // Tamper with function name in storage
    env.as_contract(&dao.address, || {
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(pid))
            .unwrap();
        proposal.function = Symbol::new(&env, "drain");
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(pid), &proposal);
    });

    // Execution MUST reject mismatched hash (#771)
    dao.execute(&pid);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_tampered_proposal_args_rejected_on_execution() {
    let (env, dao, _, voter1, voter2, target) = setup();
    let args: Vec<Val> = Vec::new(&env);
    let pid = dao.create_proposal(
        &voter1,
        &Bytes::from_slice(&env, b"Tamper args"),
        &target,
        &Symbol::new(&env, "update"),
        &args,
        &Bytes::from_slice(&env, b"Impact analysis: verified safe."),
    );

    dao.vote(&voter1, &pid, &VoteChoice::For);
    dao.vote(&voter2, &pid, &VoteChoice::For);
    env.ledger().with_mut(|l| l.timestamp += 7 * 86_400 + 301);
    dao.finalize(&pid);

    // Tamper with args in storage
    env.as_contract(&dao.address, || {
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(pid))
            .unwrap();
        let mut tampered_args = Vec::new(&env);
        tampered_args.push_back(100u32.into_val(&env));
        proposal.args = tampered_args;
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(pid), &proposal);
    });

    // Execution MUST reject mismatched hash (#771)
    dao.execute(&pid);
}
