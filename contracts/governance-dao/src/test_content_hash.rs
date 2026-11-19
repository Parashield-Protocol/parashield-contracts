//! Type-tagged `proposal_content_hash` and duplicate-proposal tests (issue #820).
#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::Address as _,
    token::StellarAssetClient,
    Bytes, Env, IntoVal, Symbol, Val, Vec,
};

struct HashFixture<'a> {
    admin: Address,
    proposer: Address,
    target: Address,
    dao_id: Address,
    dao: GovernanceDaoClient<'a>,
}

fn setup(env: &Env) -> HashFixture<'_> {
    env.mock_all_auths();

    let admin = Address::generate(env);
    let proposer = Address::generate(env);
    let target = Address::generate(env);

    let gov_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    StellarAssetClient::new(env, &gov_token).mint(&proposer, &100_000_000_000i128);

    let dao_id = env.register(GovernanceDao, ());
    let dao = GovernanceDaoClient::new(env, &dao_id);
    dao.initialize(
        &admin,
        &DaoConfig {
            gov_token,
            proposal_threshold: 1_000_000_000,
            total_supply: 100_000_000_000,
            quorum_bps: 1_000,
            majority_bps: 5_100,
            voting_period: 86_400,
            proposal_timelock: 3_600,
            discussion_period: 0,
            vote_weight_cap: 0,
        },
    );

    HashFixture {
        admin,
        proposer,
        target,
        dao_id,
        dao,
    }
}

fn contract_error<T, E>(
    res: Result<Result<T, E>, Result<soroban_sdk::Error, soroban_sdk::InvokeError>>,
) -> Option<soroban_sdk::Error> {
    res.err().and_then(|inner| inner.ok())
}

fn single_arg_vec(env: &Env, v: Val) -> Vec<Val> {
    let mut args = Vec::new(env);
    args.push_back(v);
    args
}

#[test]
fn different_types_with_same_numeric_body_hash_differently() {
    let env = Env::default();
    let target = Address::generate(&env);
    let func = Symbol::new(&env, "set_val");

    let v_u32: Val = 1u32.into_val(&env);
    let v_i32: Val = 1i32.into_val(&env);
    let v_u64: Val = 1u64.into_val(&env);
    let v_bool: Val = true.into_val(&env);

    // Note: in soroban-sdk 22, Val::get_payload() packs the 8-bit tag in the
    // low byte, so these four values already carry distinct tags and payloads;
    // including `v.get_tag() as u8` explicitly makes type separation part of
    // the preimage format.
    assert_ne!(v_u32.get_tag() as u8, v_i32.get_tag() as u8);
    assert_ne!(v_u32.get_tag() as u8, v_u64.get_tag() as u8);
    assert_ne!(v_u32.get_tag() as u8, v_bool.get_tag() as u8);

    let h_u32 = GovernanceDao::proposal_content_hash(&env, &target, &func, &single_arg_vec(&env, v_u32));
    let h_i32 = GovernanceDao::proposal_content_hash(&env, &target, &func, &single_arg_vec(&env, v_i32));
    let h_u64 = GovernanceDao::proposal_content_hash(&env, &target, &func, &single_arg_vec(&env, v_u64));
    let h_bool = GovernanceDao::proposal_content_hash(&env, &target, &func, &single_arg_vec(&env, v_bool));

    let hashes = [h_u32, h_i32, h_u64, h_bool];
    for i in 0..hashes.len() {
        for j in (i + 1)..hashes.len() {
            assert_ne!(hashes[i], hashes[j], "hashes at {} and {} must differ", i, j);
        }
    }
}

#[test]
fn identical_arguments_hash_identically_and_order_matters() {
    let env = Env::default();
    let target = Address::generate(&env);
    let func = Symbol::new(&env, "update");

    let mut args_ab = Vec::new(&env);
    args_ab.push_back(10u32.into_val(&env));
    args_ab.push_back(20u32.into_val(&env));

    let mut args_ab_copy = Vec::new(&env);
    args_ab_copy.push_back(10u32.into_val(&env));
    args_ab_copy.push_back(20u32.into_val(&env));

    let mut args_ba = Vec::new(&env);
    args_ba.push_back(20u32.into_val(&env));
    args_ba.push_back(10u32.into_val(&env));

    let h1 = GovernanceDao::proposal_content_hash(&env, &target, &func, &args_ab);
    let h2 = GovernanceDao::proposal_content_hash(&env, &target, &func, &args_ab_copy);
    let h_rev = GovernanceDao::proposal_content_hash(&env, &target, &func, &args_ba);

    assert_eq!(h1, h2);
    assert_ne!(h1, h_rev);
}

#[test]
fn empty_args_and_boundary_values_produce_distinct_hashes() {
    let env = Env::default();
    let target = Address::generate(&env);
    let func = Symbol::new(&env, "bounds");

    let empty = Vec::new(&env);
    let h_empty = GovernanceDao::proposal_content_hash(&env, &target, &func, &empty);
    let h_u32_zero = GovernanceDao::proposal_content_hash(
        &env,
        &target,
        &func,
        &single_arg_vec(&env, 0u32.into_val(&env)),
    );
    let h_u32_max = GovernanceDao::proposal_content_hash(
        &env,
        &target,
        &func,
        &single_arg_vec(&env, u32::MAX.into_val(&env)),
    );
    let h_i32_min = GovernanceDao::proposal_content_hash(
        &env,
        &target,
        &func,
        &single_arg_vec(&env, i32::MIN.into_val(&env)),
    );
    let h_i32_max = GovernanceDao::proposal_content_hash(
        &env,
        &target,
        &func,
        &single_arg_vec(&env, i32::MAX.into_val(&env)),
    );
    let h_false = GovernanceDao::proposal_content_hash(
        &env,
        &target,
        &func,
        &single_arg_vec(&env, false.into_val(&env)),
    );
    let h_true = GovernanceDao::proposal_content_hash(
        &env,
        &target,
        &func,
        &single_arg_vec(&env, true.into_val(&env)),
    );

    let hashes = [
        h_empty,
        h_u32_zero,
        h_u32_max,
        h_i32_min,
        h_i32_max,
        h_false,
        h_true,
    ];
    for i in 0..hashes.len() {
        for j in (i + 1)..hashes.len() {
            assert_ne!(hashes[i], hashes[j]);
        }
    }
}

#[test]
fn preimage_layout_includes_explicit_type_tag_and_differs_from_legacy() {
    let env = Env::default();
    let target = Address::generate(&env);
    let func = Symbol::new(&env, "set_rate");
    let arg_val: Val = 42u32.into_val(&env);
    let args = single_arg_vec(&env, arg_val);

    // Construct the exact expected preimage:
    // target_56_bytes || fn_payload_be_8_bytes || arg_tag_1_byte || arg_payload_be_8_bytes
    let mut expected_preimage = Bytes::new(&env);
    let mut target_buf = [0u8; 56];
    target.to_string().copy_into_slice(&mut target_buf);
    expected_preimage.extend_from_array(&target_buf);
    expected_preimage.extend_from_array(&func.to_val().get_payload().to_be_bytes());
    expected_preimage.extend_from_array(&[arg_val.get_tag() as u8]);
    expected_preimage.extend_from_array(&arg_val.get_payload().to_be_bytes());
    let expected_hash: BytesN<32> = env.crypto().sha256(&expected_preimage).into();

    let actual_hash = GovernanceDao::proposal_content_hash(&env, &target, &func, &args);
    let legacy_hash = GovernanceDao::legacy_proposal_content_hash(&env, &target, &func, &args);

    assert_eq!(actual_hash, expected_hash);
    assert_ne!(actual_hash, legacy_hash);
}

#[test]
fn proposals_differing_only_by_arg_type_are_not_duplicates_while_true_duplicates_fail() {
    let env = Env::default();
    let f = setup(&env);

    let func = Symbol::new(&env, "set_flag");
    let impact = Bytes::from_slice(&env, b"Parameter update impact analysis");

    let args_u32 = single_arg_vec(&env, 1u32.into_val(&env));
    let args_i32 = single_arg_vec(&env, 1i32.into_val(&env));
    let args_bool = single_arg_vec(&env, true.into_val(&env));

    let id1 = f.dao.create_proposal(
        &f.proposer,
        &Bytes::from_slice(&env, b"Proposal with u32(1)"),
        &f.target,
        &func,
        &args_u32,
        &impact,
    );

    // Different argument types with body 1 are NOT duplicates.
    let id2 = f.dao.create_proposal(
        &f.proposer,
        &Bytes::from_slice(&env, b"Proposal with i32(1)"),
        &f.target,
        &func,
        &args_i32,
        &impact,
    );
    let id3 = f.dao.create_proposal(
        &f.proposer,
        &Bytes::from_slice(&env, b"Proposal with bool(true)"),
        &f.target,
        &func,
        &args_bool,
        &impact,
    );
    assert_ne!(id1, id2);
    assert_ne!(id2, id3);

    // True duplicate (same target, function, and typed arg u32(1)) must be rejected.
    let dup = f.dao.try_create_proposal(
        &f.proposer,
        &Bytes::from_slice(&env, b"Duplicate u32(1)"),
        &f.target,
        &func,
        &args_u32,
        &impact,
    );
    assert_eq!(
        contract_error(dup),
        Some(soroban_sdk::Error::from_contract_error(
            Error::DuplicateProposal as u32
        ))
    );
}

#[test]
fn legacy_active_proposal_hash_blocks_duplicate_and_clears_on_cancel() {
    let env = Env::default();
    let f = setup(&env);

    let func = Symbol::new(&env, "legacy_fn");
    let impact = Bytes::from_slice(&env, b"Legacy proposal impact analysis");
    let args = single_arg_vec(&env, 7u32.into_val(&env));

    // Create a proposal, then move its ActiveProposalHash entry to the legacy
    // pre-#820 key to simulate an open proposal created before this upgrade.
    let pid = f.dao.create_proposal(
        &f.proposer,
        &Bytes::from_slice(&env, b"Legacy open proposal"),
        &f.target,
        &func,
        &args,
        &impact,
    );
    env.as_contract(&f.dao_id, || {
        let new_hash = GovernanceDao::proposal_content_hash(&env, &f.target, &func, &args);
        let legacy_hash = GovernanceDao::legacy_proposal_content_hash(&env, &f.target, &func, &args);
        env.storage()
            .instance()
            .remove(&StorageKey::ActiveProposalHash(new_hash));
        env.storage()
            .instance()
            .set(&StorageKey::ActiveProposalHash(legacy_hash), &pid);
    });

    // Submitting a duplicate of the legacy open proposal must still be rejected.
    let dup = f.dao.try_create_proposal(
        &f.proposer,
        &Bytes::from_slice(&env, b"Duplicate of legacy proposal"),
        &f.target,
        &func,
        &args,
        &impact,
    );
    assert_eq!(
        contract_error(dup),
        Some(soroban_sdk::Error::from_contract_error(
            Error::DuplicateProposal as u32
        ))
    );

    // Cancelling the legacy proposal clears the legacy hash entry so the action
    // can be re-proposed cleanly.
    f.dao.cancel(&f.admin, &pid);
    let pid2 = f.dao.create_proposal(
        &f.proposer,
        &Bytes::from_slice(&env, b"Re-proposed after cancel"),
        &f.target,
        &func,
        &args,
        &impact,
    );
    assert_ne!(pid, pid2);
}
