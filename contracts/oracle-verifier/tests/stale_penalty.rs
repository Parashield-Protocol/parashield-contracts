#![cfg(test)]

//! Issue #459: stale-submission penalty coverage.
//!
//! Placed as an integration test rather than a `mod` under `src/` because
//! the crate's lib-test binary has pre-existing compile errors on `main`
//! (references to `pause_data_type` / `resume_data_type` / `is_data_type_paused`
//! that no longer exist), so it cannot build a single-file addition today.
//! Integration tests compile per-file and are unaffected by that.

use parashield_oracle_verifier::{OracleVerifier, OracleVerifierClient};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    Address, Env, Symbol,
};

/// Chosen far enough into the epoch that timestamp arithmetic never underflows
/// in the "5 minutes ago" stale calculations below.
const NOW_TS: u64 = 1_748_736_000;

fn weather() -> Symbol {
    symbol_short!("weather")
}

fn kisumu_key() -> Symbol {
    symbol_short!("kis2606")
}

/// Register the oracle and set a tight 60 s per-feed max data age so stale-
/// window tests do not have to sit around waiting for the 90-day global
/// cutoff.
fn setup() -> (Env, Address, Address, OracleVerifierClient<'static>) {
    let env = Env::default();
    env.ledger().set_timestamp(NOW_TS);
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let contract_id = env.register(OracleVerifier, ());
    let client = OracleVerifierClient::new(&env, &contract_id);
    client.initialize(&admin);

    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &50u32);
    client.set_data_type_max_age(&admin, &weather(), &60u64);
    // Loosen the rate limiter so back-to-back stale submissions in one test
    // do not trip `RateLimited` before the stale-penalty pipeline fires.
    client.set_min_submit_interval(&admin, &0u64);

    (env, admin, oracle, client)
}

#[test]
fn stale_submission_below_threshold_records_count_no_slash() {
    let (env, admin, oracle, client) = setup();
    client.set_stale_threshold(&admin, &3u32);
    client.set_stale_reputation_penalty(&admin, &100u32);

    let now = env.ledger().timestamp();
    let stale_ts = now - 300;
    client.submit_data(&oracle, &weather(), &kisumu_key(), &20_000_000i128, &90u32, &stale_ts);
    client.submit_data(&oracle, &weather(), &kisumu_key(), &21_000_000i128, &90u32, &stale_ts);

    assert_eq!(client.get_stale_count(&weather(), &oracle), 2);
    let rep = client.get_reputation(&oracle, &weather());
    assert_eq!(rep.score, 500);
}

#[test]
fn stale_submission_at_threshold_applies_reputation_penalty_and_resets_count() {
    let (env, admin, oracle, client) = setup();
    client.set_stale_threshold(&admin, &3u32);
    client.set_stale_reputation_penalty(&admin, &100u32);

    let now = env.ledger().timestamp();
    let stale_ts = now - 300;
    for _ in 0..3 {
        client.submit_data(&oracle, &weather(), &kisumu_key(), &20_000_000i128, &90u32, &stale_ts);
    }

    let rep = client.get_reputation(&oracle, &weather());
    assert_eq!(rep.score, 400);
    assert_eq!(client.get_stale_count(&weather(), &oracle), 0);
}

#[test]
fn fresh_submission_resets_stale_count() {
    let (env, admin, oracle, client) = setup();
    client.set_stale_threshold(&admin, &10u32);
    client.set_stale_reputation_penalty(&admin, &100u32);

    let now = env.ledger().timestamp();
    let stale_ts = now - 300;
    client.submit_data(&oracle, &weather(), &kisumu_key(), &20_000_000i128, &90u32, &stale_ts);
    client.submit_data(&oracle, &weather(), &kisumu_key(), &20_000_000i128, &90u32, &stale_ts);
    assert_eq!(client.get_stale_count(&weather(), &oracle), 2);

    client.submit_data(&oracle, &weather(), &kisumu_key(), &21_000_000i128, &90u32, &now);
    assert_eq!(client.get_stale_count(&weather(), &oracle), 0);
}

#[test]
fn stale_gate_is_disabled_when_threshold_zero_default() {
    let (env, _admin, oracle, client) = setup();
    let now = env.ledger().timestamp();
    let stale_ts = now - 300;
    for _ in 0..10 {
        client.submit_data(&oracle, &weather(), &kisumu_key(), &20_000_000i128, &90u32, &stale_ts);
    }
    assert_eq!(client.get_stale_count(&weather(), &oracle), 10);
    let rep = client.get_reputation(&oracle, &weather());
    assert_eq!(rep.score, 500);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn non_admin_cannot_set_stale_threshold() {
    let (env, _admin, _oracle, client) = setup();
    let impostor = Address::generate(&env);
    client.set_stale_threshold(&impostor, &3u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn non_admin_cannot_set_stale_slash_amount() {
    let (env, _admin, _oracle, client) = setup();
    let impostor = Address::generate(&env);
    client.set_stale_slash_amount(&impostor, &10i128);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn non_admin_cannot_set_stale_reputation_penalty() {
    let (env, _admin, _oracle, client) = setup();
    let impostor = Address::generate(&env);
    client.set_stale_reputation_penalty(&impostor, &100u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn negative_stale_slash_amount_rejected() {
    let (_env, admin, _oracle, client) = setup();
    client.set_stale_slash_amount(&admin, &-1i128);
}
