//! Admin transfer timelock tests (issue #856).
//!
//! Tests verify that admin transfers enforce the ADMIN_TRANSFER_TIMELOCK
//! and that cancel_admin_transfer works during the timelock window.
#![cfg(test)]

use super::*;
use soroban_sdk::{symbol_short, testutils::{Address as _, Ledger}, Env};
use parashield_common::ADMIN_TRANSFER_TIMELOCK;

fn setup() -> (Env, Address, PolicyEngineClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let usdc = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let id = env.register(PolicyEngine, ());
    let client = PolicyEngineClient::new(&env, &id);
    client.initialize(&admin, &usdc, &Address::generate(&env));
    (env, admin, client)
}

#[test]
fn admin_transfer_timelock_blocks_premature_acceptance() {
    let (env, admin, client) = setup();
    let new_admin = Address::generate(&env);
    
    client.propose_new_admin(&admin, &new_admin);
    
    // Try to accept immediately - should fail
    let result = client.try_accept_admin(&new_admin);
    assert!(result.is_err(), "accept_admin should fail before timelock expires");
}

#[test]
fn admin_transfer_succeeds_after_timelock() {
    let (env, admin, client) = setup();
    let new_admin = Address::generate(&env);
    
    client.propose_new_admin(&admin, &new_admin);
    
    // Advance time past the timelock
    env.ledger().with_mut(|l| l.timestamp += ADMIN_TRANSFER_TIMELOCK);
    
    // Accept should succeed now
    client.accept_admin(&new_admin);
    
    // Verify the new admin is now in control
    let current_admin = client.get_admin();
    assert_eq!(current_admin, new_admin);
}

#[test]
fn cancel_admin_transfer_works_during_timelock() {
    let (env, admin, client) = setup();
    let new_admin = Address::generate(&env);
    
    client.propose_new_admin(&admin, &new_admin);
    
    // Cancel the transfer
    client.cancel_admin_transfer(&admin);
    
    // Advance time past the timelock
    env.ledger().with_mut(|l| l.timestamp += ADMIN_TRANSFER_TIMELOCK);
    
    // Accept should fail because transfer was cancelled
    let result = client.try_accept_admin(&new_admin);
    assert!(result.is_err(), "accept_admin should fail after cancellation");
}

#[test]
fn cancel_admin_transfer_allows_new_proposal() {
    let (env, admin, client) = setup();
    let first_new_admin = Address::generate(&env);
    let second_new_admin = Address::generate(&env);
    
    // Propose first transfer
    client.propose_new_admin(&admin, &first_new_admin);
    
    // Cancel it
    client.cancel_admin_transfer(&admin);
    
    // Should be able to propose a different admin now
    client.propose_new_admin(&admin, &second_new_admin);
    
    // Advance time and accept
    env.ledger().with_mut(|l| l.timestamp += ADMIN_TRANSFER_TIMELOCK);
    client.accept_admin(&second_new_admin);
    
    assert_eq!(client.get_admin(), second_new_admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #40)")]
fn propose_new_admin_rejects_when_transfer_pending() {
    let (_env, admin, client) = setup();
    let first_new_admin = Address::generate(&_env);
    let second_new_admin = Address::generate(&_env);
    
    // Propose first transfer
    client.propose_new_admin(&admin, &first_new_admin);
    
    // Try to propose another without canceling - should fail
    client.propose_new_admin(&admin, &second_new_admin);
}

#[test]
fn only_current_admin_can_cancel_transfer() {
    let (env, admin, client) = setup();
    let new_admin = Address::generate(&env);
    let unauthorized = Address::generate(&env);
    
    client.propose_new_admin(&admin, &new_admin);
    
    // Try to cancel with unauthorized address - should fail
    let result = client.try_cancel_admin_transfer(&unauthorized);
    assert!(result.is_err(), "only current admin can cancel transfer");
}
