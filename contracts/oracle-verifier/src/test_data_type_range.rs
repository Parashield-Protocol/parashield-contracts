#![cfg(test)]

//! Regression coverage for issue #831: `set_data_type_range` used to treat
//! `[0, 0]` as a "clear the override" sentinel, so an admin who wanted a
//! real range of exactly `[0, 0]` (valid for binary and disaster data types)
//! silently removed the override instead. Setting now always stores the
//! given range, and clearing is an explicit `clear_data_type_range` call.

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events as _, Ledger},
    Address, Env, Symbol, TryFromVal,
};

const NOW: u64 = 1_750_000_000;
const OBSERVED_AT: u64 = 1_749_900_000;

fn setup() -> (Env, Address, Address, Address) {
    let env = Env::default();
    env.ledger().set_timestamp(NOW);
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let oracle = Address::generate(&env);
    let contract_id = env.register(OracleVerifier, ());
    let client = OracleVerifierClient::new(&env, &contract_id);
    client.initialize(&admin);
    client.add_oracle(&admin, &oracle, &disaster(), &100u32);

    (env, admin, oracle, contract_id)
}

fn disaster() -> Symbol {
    symbol_short!("disaster")
}

fn key() -> Symbol {
    symbol_short!("quake01")
}

/// `Error::InvalidInput` — raised for out-of-range readings and for an
/// inverted (`min > max`) range.
fn invalid_input() -> soroban_sdk::Error {
    soroban_sdk::Error::from_contract_error(28)
}

/// The contract error a `try_*` client call raised, or `None` if it did not
/// fail with one.
fn contract_error<T, E>(
    res: Result<Result<T, E>, Result<soroban_sdk::Error, soroban_sdk::InvokeError>>,
) -> Option<soroban_sdk::Error> {
    res.err().and_then(|inner| inner.ok())
}

/// Number of events published by `contract_id` with `topic` among the
/// events the test host currently reports.
fn count_events(env: &Env, contract_id: &Address, topic: &str) -> usize {
    let topic_sym = Symbol::new(env, topic);
    env.events()
        .all()
        .iter()
        .filter(|e| {
            e.0 == *contract_id
                && e.1
                    .iter()
                    .any(|t| Symbol::try_from_val(env, &t) == Ok(topic_sym.clone()))
        })
        .count()
}

// ── set_data_type_range ────────────────────────────────────────────────────

#[test]
fn set_zero_zero_stores_a_real_range() {
    let (env, admin, _oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    client.set_data_type_range(&admin, &disaster(), &0i128, &0i128);

    assert_eq!(client.get_data_type_range(&disaster()), Some((0i128, 0i128)));
}

#[test]
fn set_zero_zero_overwrites_an_existing_range_instead_of_clearing_it() {
    let (env, admin, _oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    client.set_data_type_range(&admin, &disaster(), &0i128, &50i128);
    client.set_data_type_range(&admin, &disaster(), &0i128, &0i128);

    assert_eq!(client.get_data_type_range(&disaster()), Some((0i128, 0i128)));
}

#[test]
fn zero_zero_range_accepts_zero_and_rejects_every_other_reading() {
    let (env, admin, oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    client.set_data_type_range(&admin, &disaster(), &0i128, &0i128);

    // Rejected attempts first: a failed call records nothing, so it cannot
    // trip the rate limit for the accepted reading below.
    for bad in [1i128, -1i128, 10_000_000i128, -10_000_000i128] {
        assert_eq!(
            contract_error(client.try_submit_data(
                &oracle,
                &disaster(),
                &key(),
                &bad,
                &100u32,
                &OBSERVED_AT,
            )),
            Some(invalid_input()),
            "a non-zero reading must be rejected under a [0, 0] range",
        );
    }
    assert!(client.get_data_points(&disaster(), &key()).is_empty());

    client.submit_data(&oracle, &disaster(), &key(), &0i128, &100u32, &OBSERVED_AT);
    assert_eq!(client.get_data(&disaster(), &key()).value, 0i128);
}

#[test]
fn set_data_type_range_emits_updated_event_for_zero_zero() {
    let (env, admin, _oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    client.set_data_type_range(&admin, &disaster(), &0i128, &0i128);

    assert_eq!(count_events(&env, &contract_id, "data_type_range_updated"), 1);
    assert_eq!(count_events(&env, &contract_id, "data_type_range_cleared"), 0);
}

#[test]
fn set_data_type_range_rejects_min_greater_than_max() {
    let (env, admin, _oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    assert_eq!(
        contract_error(client.try_set_data_type_range(&admin, &disaster(), &10i128, &5i128)),
        Some(invalid_input()),
    );
    assert_eq!(client.get_data_type_range(&disaster()), None);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn set_data_type_range_requires_admin() {
    let (env, _admin, _oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let impostor = Address::generate(&env);
    client.set_data_type_range(&impostor, &disaster(), &0i128, &0i128);
}

// ── clear_data_type_range ──────────────────────────────────────────────────

#[test]
fn clear_removes_the_override_and_restores_the_default_bound() {
    let (env, admin, oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    client.set_data_type_range(&admin, &disaster(), &0i128, &0i128);

    client.clear_data_type_range(&admin, &disaster());
    assert_eq!(client.get_data_type_range(&disaster()), None);
    assert_eq!(count_events(&env, &contract_id, "data_type_range_cleared"), 1);

    // The compiled-in disaster default (DEFAULT_DISASTER_MAX) applies again:
    // just above it is rejected, a non-zero value below it is accepted.
    assert_eq!(
        contract_error(client.try_submit_data(
            &oracle,
            &disaster(),
            &key(),
            &(DEFAULT_DISASTER_MAX + 1),
            &100u32,
            &OBSERVED_AT,
        )),
        Some(invalid_input()),
    );
    client.submit_data(&oracle, &disaster(), &key(), &50_000_000i128, &100u32, &OBSERVED_AT);
    assert_eq!(client.get_data(&disaster(), &key()).value, 50_000_000i128);
}

#[test]
fn clearing_a_missing_override_is_a_silent_no_op() {
    let (env, admin, _oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    // No override was ever set: no error, no state change, no event.
    client.clear_data_type_range(&admin, &disaster());

    assert_eq!(client.get_data_type_range(&disaster()), None);
    assert_eq!(count_events(&env, &contract_id, "data_type_range_cleared"), 0);
}

#[test]
fn clearing_twice_is_idempotent() {
    let (env, admin, _oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    client.set_data_type_range(&admin, &disaster(), &0i128, &10i128);

    client.clear_data_type_range(&admin, &disaster());
    assert_eq!(count_events(&env, &contract_id, "data_type_range_cleared"), 1);

    // The second clear finds nothing to remove and must not fail.
    client.clear_data_type_range(&admin, &disaster());
    assert_eq!(client.get_data_type_range(&disaster()), None);
    // At most the single event from the first clear is visible; the second
    // call publishes nothing (see `clearing_a_missing_override_is_a_silent_no_op`).
    assert!(count_events(&env, &contract_id, "data_type_range_cleared") <= 1);
}

#[test]
fn clear_then_set_zero_zero_stores_the_range_again() {
    let (env, admin, _oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    client.set_data_type_range(&admin, &disaster(), &0i128, &0i128);
    client.clear_data_type_range(&admin, &disaster());
    client.set_data_type_range(&admin, &disaster(), &0i128, &0i128);

    assert_eq!(client.get_data_type_range(&disaster()), Some((0i128, 0i128)));
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn clear_data_type_range_requires_admin() {
    let (env, admin, _oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    client.set_data_type_range(&admin, &disaster(), &0i128, &0i128);

    let impostor = Address::generate(&env);
    client.clear_data_type_range(&impostor, &disaster());
}
