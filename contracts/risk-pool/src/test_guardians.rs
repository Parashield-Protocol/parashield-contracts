#![allow(clippy::inconsistent_digit_grouping)]
#![cfg(test)]

//! Regression coverage for the guardian-set hardening in issue #774:
//! guardians can no longer be dismantled via `set_guardians`, and
//! `propose_remove_guardian` + `approve_remove_guardian` is the only path
//! that actually shrinks the committee.

extern crate std;

use soroban_sdk::{testutils::Address as _, token, vec, Address, Env, Symbol};

use crate::{RiskPool, RiskPoolClient};

fn setup() -> (Env, RiskPoolClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let policy_engine = Address::generate(&env);
    let claims_processor = Address::generate(&env);

    let usdc_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let backstop_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let pool_id = env.register(RiskPool, ());
    let pool = RiskPoolClient::new(&env, &pool_id);
    let _ = token::StellarAssetClient::new(&env, &usdc_id);

    pool.initialize(
        &admin,
        &usdc_id,
        &treasury,
        &backstop_id,
        &Symbol::new(&env, "crop"),
        &policy_engine,
        &claims_processor,
    );
    (env, pool, admin)
}

#[test]
fn initial_set_guardians_accepts_any_set() {
    let (env, pool, admin) = setup();
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    let g3 = Address::generate(&env);
    pool.set_guardians(&admin, &vec![&env, g1.clone(), g2.clone(), g3.clone()], &2);
    assert_eq!(pool.get_guardians().len(), 3);
    assert_eq!(pool.get_guardian_threshold(), 2);
}

#[test]
fn set_guardians_can_expand_the_committee() {
    let (env, pool, admin) = setup();
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    let g3 = Address::generate(&env);
    pool.set_guardians(&admin, &vec![&env, g1.clone(), g2.clone()], &2);

    pool.set_guardians(&admin, &vec![&env, g1.clone(), g2.clone(), g3.clone()], &2);
    assert_eq!(pool.get_guardians().len(), 3);
}

/// Error code 49 = `GuardianRemoveBlocked`: admin cannot shrink the set via
/// `set_guardians`; the propose/approve flow is the only way to remove.
#[test]
#[should_panic(expected = "Error(Contract, #49)")]
fn set_guardians_cannot_shrink_the_committee() {
    let (env, pool, admin) = setup();
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    let g3 = Address::generate(&env);
    pool.set_guardians(&admin, &vec![&env, g1.clone(), g2.clone(), g3.clone()], &2);

    // Dropping g3 requires approval from the remaining guardians.
    pool.set_guardians(&admin, &vec![&env, g1.clone(), g2.clone()], &2);
}

/// A single approval is not enough when the threshold is 2.
#[test]
fn propose_remove_guardian_requires_threshold_approvals() {
    let (env, pool, admin) = setup();
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    let g3 = Address::generate(&env);
    pool.set_guardians(&admin, &vec![&env, g1.clone(), g2.clone(), g3.clone()], &2);

    pool.propose_remove_guardian(&admin, &g3);
    pool.approve_remove_guardian(&g1);

    assert!(pool.get_pending_guardian_removal().is_some());
    assert_eq!(pool.get_guardians().len(), 3);
}

#[test]
fn removal_applies_once_threshold_is_met() {
    let (env, pool, admin) = setup();
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    let g3 = Address::generate(&env);
    pool.set_guardians(&admin, &vec![&env, g1.clone(), g2.clone(), g3.clone()], &2);

    pool.propose_remove_guardian(&admin, &g3);
    pool.approve_remove_guardian(&g1);
    pool.approve_remove_guardian(&g2);

    assert!(pool.get_pending_guardian_removal().is_none());
    let guardians = pool.get_guardians();
    assert_eq!(guardians.len(), 2);
    for g in guardians.iter() {
        assert_ne!(g, g3);
    }
}

/// Error 50 = `MinGuardiansUnmet`: can't propose below the floor of 2.
#[test]
#[should_panic(expected = "Error(Contract, #50)")]
fn cannot_remove_below_minimum_guardians() {
    let (env, pool, admin) = setup();
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    pool.set_guardians(&admin, &vec![&env, g1.clone(), g2.clone()], &2);

    // Removing either would drop the committee to one; propose must refuse.
    pool.propose_remove_guardian(&admin, &g1);
}

/// Error 23 = `NotGuardian`: a non-guardian cannot approve a guardian removal.
#[test]
#[should_panic(expected = "Error(Contract, #23)")]
fn non_guardian_cannot_approve_removal() {
    let (env, pool, admin) = setup();
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    let g3 = Address::generate(&env);
    pool.set_guardians(&admin, &vec![&env, g1.clone(), g2.clone(), g3.clone()], &2);

    // We proposed to remove g3 — but not_a_guardian is not a guardian at all,
    // so approve_remove_guardian from them must fail with NotGuardian.
    let not_a_guardian = Address::generate(&env);
    pool.propose_remove_guardian(&admin, &g3);
    pool.approve_remove_guardian(&not_a_guardian);
}

/// Error 3 = `Unauthorized`: the guardian scheduled for removal cannot
/// vote on their own exit.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn target_of_removal_cannot_approve_their_own_removal() {
    let (env, pool, admin) = setup();
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    let g3 = Address::generate(&env);
    pool.set_guardians(&admin, &vec![&env, g1.clone(), g2.clone(), g3.clone()], &2);

    // We proposed to remove g3 — g3 cannot approve their own removal.
    pool.propose_remove_guardian(&admin, &g3);
    pool.approve_remove_guardian(&g3);
}

#[test]
fn admin_can_cancel_a_pending_removal() {
    let (env, pool, admin) = setup();
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    let g3 = Address::generate(&env);
    pool.set_guardians(&admin, &vec![&env, g1.clone(), g2.clone(), g3.clone()], &2);
    pool.propose_remove_guardian(&admin, &g3);
    assert!(pool.get_pending_guardian_removal().is_some());

    pool.cancel_pending_guardian_removal(&admin);
    assert!(pool.get_pending_guardian_removal().is_none());
}
