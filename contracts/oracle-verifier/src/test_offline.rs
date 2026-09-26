//! Offline-oracle detection and auto-deactivation (issue #513), plus the
//! per-data-type pause guard.
#![cfg(test)]

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    Env,
};

const START: u64 = 1_748_736_000;
const LIMIT: u64 = 300;

fn weather() -> Symbol {
    symbol_short!("weather")
}

fn key() -> Symbol {
    symbol_short!("kis2606")
}

fn setup() -> (Env, Address, OracleVerifierClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let admin = Address::generate(&env);
    let id = env.register(OracleVerifier, ());
    let client = OracleVerifierClient::new(&env, &id);
    client.initialize(&admin);
    client.set_max_data_age(&admin, &1_000_000u64);
    (env, admin, client)
}

fn advance(env: &Env, secs: u64) {
    let now = env.ledger().timestamp();
    env.ledger().set_timestamp(now + secs);
}

fn register(env: &Env, admin: &Address, client: &OracleVerifierClient) -> Address {
    let oracle = Address::generate(env);
    client.add_oracle(admin, &oracle, &weather(), &50u32);
    oracle
}

fn submit(env: &Env, client: &OracleVerifierClient, oracle: &Address, value: i128) {
    client.submit_data(
        oracle,
        &weather(),
        &key(),
        &value,
        &90u32,
        &env.ledger().timestamp(),
    );
}

#[test]
fn detection_is_disabled_by_default() {
    let (env, admin, client) = setup();
    let oracle = register(&env, &admin, &client);
    submit(&env, &client, &oracle, 10_000_000);
    advance(&env, 10_000_000);

    assert_eq!(client.get_oracle_inactivity_limit(), 0);
    assert!(!client.is_oracle_offline(&oracle, &weather()));
    assert_eq!(client.deactivate_inactive_oracles(&weather()).len(), 0);
}

#[test]
fn admin_sets_and_reads_the_limit() {
    let (_, admin, client) = setup();
    client.set_oracle_inactivity_limit(&admin, &LIMIT);
    assert_eq!(client.get_oracle_inactivity_limit(), LIMIT);
    client.set_oracle_inactivity_limit(&admin, &0u64);
    assert_eq!(client.get_oracle_inactivity_limit(), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn non_admin_cannot_set_the_limit() {
    let (env, _, client) = setup();
    client.set_oracle_inactivity_limit(&Address::generate(&env), &LIMIT);
}

#[test]
fn oracle_goes_offline_only_after_the_limit() {
    let (env, admin, client) = setup();
    client.set_oracle_inactivity_limit(&admin, &LIMIT);
    let oracle = register(&env, &admin, &client);
    submit(&env, &client, &oracle, 10_000_000);

    advance(&env, LIMIT);
    assert!(!client.is_oracle_offline(&oracle, &weather()));
    advance(&env, 1);
    assert!(client.is_oracle_offline(&oracle, &weather()));

    // A fresh submission brings it back.
    submit(&env, &client, &oracle, 11_000_000);
    assert!(!client.is_oracle_offline(&oracle, &weather()));
}

#[test]
fn oracle_that_never_submitted_is_not_offline() {
    let (env, admin, client) = setup();
    client.set_oracle_inactivity_limit(&admin, &LIMIT);
    let oracle = register(&env, &admin, &client);
    advance(&env, 10 * LIMIT);
    assert!(!client.is_oracle_offline(&oracle, &weather()));
}

#[test]
fn offline_oracle_is_ignored_by_aggregation() {
    let (env, admin, client) = setup();
    let stale = register(&env, &admin, &client);
    let live = register(&env, &admin, &client);
    submit(&env, &client, &stale, 10_000_000);
    advance(&env, LIMIT + 100);
    submit(&env, &client, &live, 50_000_000);

    // Detection off: both oracles count.
    assert_eq!(client.get_aggregated(&weather(), &key()).oracle_count, 2);

    client.set_oracle_inactivity_limit(&admin, &LIMIT);
    let agg = client.get_aggregated(&weather(), &key());
    assert_eq!(agg.oracle_count, 1);
    assert_eq!(agg.median_value, 50_000_000);
}

#[test]
fn offline_oracle_cannot_swing_a_trigger() {
    let (env, admin, client) = setup();
    client.set_oracle_inactivity_limit(&admin, &LIMIT);
    let stale = register(&env, &admin, &client);
    let live = register(&env, &admin, &client);
    submit(&env, &client, &stale, 1_000_000);
    advance(&env, LIMIT + 100);
    submit(&env, &client, &live, 90_000_000);

    let cond = TriggerCondition {
        data_type: weather(),
        key: key(),
        threshold: 50_000_000,
        comparison: TriggerComparison::GreaterThan,
        tolerance: 0,
    };
    assert!(client.verify_trigger(&weather(), &key(), &cond));
}

#[test]
fn deactivate_inactive_oracles_only_removes_the_silent_ones() {
    let (env, admin, client) = setup();
    client.set_oracle_inactivity_limit(&admin, &LIMIT);
    let stale = register(&env, &admin, &client);
    let live = register(&env, &admin, &client);
    let idle = register(&env, &admin, &client); // never submitted
    submit(&env, &client, &stale, 10_000_000);
    advance(&env, LIMIT + 100);
    submit(&env, &client, &live, 50_000_000);

    let removed = client.deactivate_inactive_oracles(&weather());
    assert_eq!(removed.len(), 1);
    assert_eq!(removed.get(0).unwrap(), stale);

    let remaining = client.get_oracles(&weather());
    assert_eq!(remaining.len(), 2);
    assert!(remaining.contains(&live));
    assert!(remaining.contains(&idle));
    assert!(!remaining.contains(&stale));

    // Idempotent.
    assert_eq!(client.deactivate_inactive_oracles(&weather()).len(), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn deactivated_oracle_cannot_submit_until_re_registered() {
    let (env, admin, client) = setup();
    client.set_oracle_inactivity_limit(&admin, &LIMIT);
    let stale = register(&env, &admin, &client);
    submit(&env, &client, &stale, 10_000_000);
    advance(&env, LIMIT + 100);
    client.deactivate_inactive_oracles(&weather());
    submit(&env, &client, &stale, 12_000_000);
}

// ── Data-type pause ──────────────────────────────────────────────────────────

#[test]
fn pause_is_scoped_to_one_data_type() {
    let (_, admin, client) = setup();
    let flight = symbol_short!("flight");
    assert!(!client.is_data_type_paused(&weather()));
    client.pause_data_type(&admin, &weather());
    assert!(client.is_data_type_paused(&weather()));
    assert!(!client.is_data_type_paused(&flight));
    client.resume_data_type(&admin, &weather());
    assert!(!client.is_data_type_paused(&weather()));
}

#[test]
#[should_panic(expected = "Error(Contract, #32)")]
fn paused_data_type_rejects_encrypted_and_batch_submissions() {
    let (env, admin, client) = setup();
    let oracle = register(&env, &admin, &client);
    client.pause_data_type(&admin, &weather());
    client.batch_submit_data(&oracle, &weather(), &soroban_sdk::Vec::new(&env));
}

#[test]
#[should_panic(expected = "Error(Contract, #32)")]
fn paused_data_type_rejects_aggregation_views_used_by_claims() {
    let (env, admin, client) = setup();
    let oracle = register(&env, &admin, &client);
    submit(&env, &client, &oracle, 10_000_000);
    client.pause_data_type(&admin, &weather());
    let cond = TriggerCondition {
        data_type: weather(),
        key: key(),
        threshold: 1,
        comparison: TriggerComparison::GreaterThan,
        tolerance: 0,
    };
    client.verify_trigger_fresh(&weather(), &key(), &cond, &1_000_000u64);
}
