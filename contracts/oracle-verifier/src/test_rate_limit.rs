#![cfg(test)]

//! Regression and edge-case coverage for issue #834: `MinSubmitInterval` rate
//! limiting is scoped per `(data_type, key, oracle)` rather than per
//! `(data_type, oracle)`.

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    Env,
};

const START_TS: u64 = 1_750_000_000;

fn weather() -> Symbol {
    symbol_short!("weather")
}

fn flight() -> Symbol {
    symbol_short!("flight")
}

fn key_a() -> Symbol {
    symbol_short!("kis001")
}

fn key_b() -> Symbol {
    symbol_short!("nbo002")
}

fn setup() -> (Env, Address, OracleVerifierClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START_TS);

    let admin = Address::generate(&env);
    let contract_id = env.register(OracleVerifier, ());
    let client = OracleVerifierClient::new(&env, &contract_id);
    client.initialize(&admin);

    (env, admin, client)
}

/// Core bug scenario (#834): the same oracle submitting for the same data_type
/// under two different keys within a single MinSubmitInterval must succeed for
/// both keys.
#[test]
fn same_oracle_same_data_type_different_keys_within_interval_succeeds() {
    let (env, admin, client) = setup();
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &100u32);
    client.set_min_submit_interval(&admin, &60u64);

    // First key at START_TS.
    client.submit_data(&oracle, &weather(), &key_a(), &25_000_000i128, &95u32, &START_TS);

    // Second key 1 second later (well inside the 60s interval) must succeed.
    let t1 = START_TS + 1;
    env.ledger().set_timestamp(t1);
    client.submit_data(&oracle, &weather(), &key_b(), &40_000_000i128, &95u32, &t1);

    assert_eq!(client.get_data(&weather(), &key_a()).value, 25_000_000i128);
    assert_eq!(client.get_data(&weather(), &key_b()).value, 40_000_000i128);
}

/// Same oracle and same (data_type, key) inside the interval is rejected with
/// Error::RateLimited (#13).
#[test]
fn same_oracle_same_key_inside_interval_is_rate_limited() {
    let (env, admin, client) = setup();
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &100u32);
    client.set_min_submit_interval(&admin, &60u64);

    client.submit_data(&oracle, &weather(), &key_a(), &25_000_000i128, &95u32, &START_TS);

    let t1 = START_TS + 10;
    env.ledger().set_timestamp(t1);
    let res = client.try_submit_data(&oracle, &weather(), &key_a(), &30_000_000i128, &95u32, &t1);
    assert_eq!(res, Err(Ok(Error::RateLimited)));
}

/// Same oracle and same key after the interval has elapsed is accepted.
#[test]
fn same_oracle_same_key_after_interval_is_accepted() {
    let (env, admin, client) = setup();
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &100u32);
    client.set_min_submit_interval(&admin, &60u64);

    client.submit_data(&oracle, &weather(), &key_a(), &25_000_000i128, &95u32, &START_TS);

    let t_after = START_TS + 61;
    env.ledger().set_timestamp(t_after);
    client.submit_data(&oracle, &weather(), &key_a(), &35_000_000i128, &95u32, &t_after);

    assert_eq!(client.get_data(&weather(), &key_a()).value, 35_000_000i128);
}

/// Pins the exact interval boundary: `elapsed < min_interval` (`min_interval - 1`)
/// is rejected with `RateLimited`, while `elapsed == min_interval` is accepted.
#[test]
fn exact_interval_boundary_rejects_one_second_early_and_accepts_at_interval() {
    let (env, admin, client) = setup();
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &100u32);
    client.set_min_submit_interval(&admin, &60u64);

    client.submit_data(&oracle, &weather(), &key_a(), &20_000_000i128, &95u32, &START_TS);

    // 59 seconds elapsed: 59 < 60 -> rejected.
    let t_early = START_TS + 59;
    env.ledger().set_timestamp(t_early);
    let res =
        client.try_submit_data(&oracle, &weather(), &key_a(), &21_000_000i128, &95u32, &t_early);
    assert_eq!(res, Err(Ok(Error::RateLimited)));

    // 60 seconds elapsed: 60 < 60 is false -> accepted.
    let t_exact = START_TS + 60;
    env.ledger().set_timestamp(t_exact);
    client.submit_data(&oracle, &weather(), &key_a(), &22_000_000i128, &95u32, &t_exact);
    assert_eq!(client.get_data(&weather(), &key_a()).value, 22_000_000i128);
}

/// Different oracles submitting for the same (data_type, key) have independent
/// rate limits.
#[test]
fn different_oracles_same_key_are_independent() {
    let (env, admin, client) = setup();
    let oracle1 = Address::generate(&env);
    let oracle2 = Address::generate(&env);
    client.add_oracle(&admin, &oracle1, &weather(), &100u32);
    client.add_oracle(&admin, &oracle2, &weather(), &100u32);
    client.set_min_submit_interval(&admin, &60u64);

    client.submit_data(&oracle1, &weather(), &key_a(), &20_000_000i128, &95u32, &START_TS);
    // Same ledger timestamp, same data_type, same key, different oracle -> succeeds.
    client.submit_data(&oracle2, &weather(), &key_a(), &24_000_000i128, &95u32, &START_TS);

    let agg = client.get_aggregated(&weather(), &key_a());
    assert_eq!(agg.oracle_count, 2);
}

/// Different data types on the same key have independent rate limits for the
/// same oracle (de-registered from the first type and registered on the second,
/// respecting the GlobalOracle single-active-type invariant).
#[test]
fn different_data_types_same_key_are_independent() {
    let (env, admin, client) = setup();
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &100u32);
    client.set_min_submit_interval(&admin, &60u64);

    client.submit_data(&oracle, &weather(), &key_a(), &20_000_000i128, &95u32, &START_TS);

    // Move the same oracle to another data_type within the 60s interval and
    // submit under the exact same key at the exact same ledger timestamp.
    client.remove_oracle(&admin, &oracle, &weather());
    client.add_oracle(&admin, &oracle, &flight(), &100u32);
    client.submit_data(&oracle, &flight(), &key_a(), &50_000_000i128, &95u32, &START_TS);

    assert_eq!(client.get_data(&flight(), &key_a()).value, 50_000_000i128);
}

/// A rejected submission (rate-limited or invalid) must NOT reset the
/// last-submission timer for that (data_type, key, oracle).
#[test]
fn rejected_submission_does_not_reset_rate_limit_timer() {
    let (env, admin, client) = setup();
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &100u32);
    client.set_min_submit_interval(&admin, &60u64);

    // Accepted at t = START_TS.
    client.submit_data(&oracle, &weather(), &key_a(), &20_000_000i128, &95u32, &START_TS);

    // Rejected with RateLimited at t = START_TS + 50.
    let t_retry = START_TS + 50;
    env.ledger().set_timestamp(t_retry);
    assert_eq!(
        client.try_submit_data(&oracle, &weather(), &key_a(), &21_000_000i128, &95u32, &t_retry),
        Err(Ok(Error::RateLimited))
    );

    // At t = START_TS + 60, try a submission that fails with InvalidConfidence (0).
    let t_boundary = START_TS + 60;
    env.ledger().set_timestamp(t_boundary);
    assert_eq!(
        client.try_submit_data(&oracle, &weather(), &key_a(), &21_000_000i128, &0u32, &t_boundary),
        Err(Ok(Error::InvalidConfidence))
    );

    // Still at t = START_TS + 60 (only 10s after the rate-limited attempt and 0s
    // after the invalid-confidence attempt): a valid submission must succeed
    // because neither failed call updated LastKeySubmission.
    client.submit_data(&oracle, &weather(), &key_a(), &25_000_000i128, &95u32, &t_boundary);
    assert_eq!(client.get_data(&weather(), &key_a()).value, 25_000_000i128);
}

/// When MinSubmitInterval is unset, the default interval (30s) still applies
/// per (data_type, key, oracle).
#[test]
fn unset_min_submit_interval_uses_default_per_key() {
    let (env, admin, client) = setup();
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &100u32);
    assert_eq!(client.get_min_submit_interval(), 30u64);

    client.submit_data(&oracle, &weather(), &key_a(), &10_000_000i128, &95u32, &START_TS);

    // Different key at the same timestamp succeeds.
    client.submit_data(&oracle, &weather(), &key_b(), &15_000_000i128, &95u32, &START_TS);

    // Same key 29s later is rate-limited under the 30s default.
    let t29 = START_TS + 29;
    env.ledger().set_timestamp(t29);
    assert_eq!(
        client.try_submit_data(&oracle, &weather(), &key_a(), &12_000_000i128, &95u32, &t29),
        Err(Ok(Error::RateLimited))
    );

    // Same key at 30s succeeds.
    let t30 = START_TS + 30;
    env.ledger().set_timestamp(t30);
    client.submit_data(&oracle, &weather(), &key_a(), &12_000_000i128, &95u32, &t30);
    assert_eq!(client.get_data(&weather(), &key_a()).value, 12_000_000i128);
}

/// When MinSubmitInterval is set to 0, rate limiting is disabled even on the
/// same key at the same ledger timestamp (provided reading timestamps advance
/// strictly forward for the monotonic guard).
#[test]
fn zero_min_submit_interval_allows_immediate_resubmission_same_key() {
    let (env, admin, client) = setup();
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &100u32);
    client.set_min_submit_interval(&admin, &0u64);

    client.submit_data(&oracle, &weather(), &key_a(), &10_000_000i128, &95u32, &(START_TS - 2));
    client.submit_data(&oracle, &weather(), &key_a(), &15_000_000i128, &95u32, &(START_TS - 1));

    assert_eq!(client.get_data(&weather(), &key_a()).value, 15_000_000i128);
}
