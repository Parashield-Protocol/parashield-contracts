//! Advanced oracle-verifier tests: multi-oracle median, confidence weighting,
//! oracle deactivation, and odd/even count median edge cases.
#![cfg(test)]

extern crate std;

use super::*;
use soroban_sdk::{symbol_short, testutils::{Address as _, Ledger}, Env};

fn setup() -> (Env, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 2_000_000_000);
    let admin       = Address::generate(&env);
    let contract_id = env.register(OracleVerifier, ());
    let client = OracleVerifierClient::new(&env, &contract_id);
    client.initialize(&admin);
    client.set_max_data_age(&admin, &3_000_000_000);
    (env, admin, contract_id)
}

fn wt() -> soroban_sdk::Symbol { symbol_short!("weather") }
fn kk() -> soroban_sdk::Symbol { symbol_short!("kis2606") }

#[test]
fn three_oracle_median_is_middle_value() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    let o3 = Address::generate(&env);
    c.add_oracle(&admin, &o1, &wt(), &90u32);
    c.add_oracle(&admin, &o2, &wt(), &80u32);
    c.add_oracle(&admin, &o3, &wt(), &70u32);

    c.submit_data(&o1, &wt(), &kk(), &10_000_000i128, &90u32, &1u64);
    c.submit_data(&o2, &wt(), &kk(), &30_000_000i128, &90u32, &1u64);
    c.submit_data(&o3, &wt(), &kk(), &20_000_000i128, &90u32, &1u64);

    let agg = c.get_aggregated(&wt(), &kk());
    // sorted: [10, 20, 30] → middle = 20
    assert_eq!(agg.median_value, 20_000_000i128);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn deactivated_oracle_cannot_submit() {
    let (env, admin, cid) = setup();
    let c      = OracleVerifierClient::new(&env, &cid);
    let oracle = Address::generate(&env);
    c.add_oracle(&admin, &oracle, &wt(), &90u32);
    c.remove_oracle(&admin, &oracle, &wt());

    c.submit_data(&oracle, &wt(), &kk(), &10_000_000i128, &90u32, &1u64);
}

#[test]
fn overwrite_submission_updates_value() {
    let (env, admin, cid) = setup();
    let c      = OracleVerifierClient::new(&env, &cid);
    let oracle = Address::generate(&env);
    c.add_oracle(&admin, &oracle, &wt(), &90u32);

    c.submit_data(&oracle, &wt(), &kk(), &10_000_000i128, &90u32, &100u64);
    c.submit_data(&oracle, &wt(), &kk(), &25_000_000i128, &95u32, &200u64);

    let dp = c.get_data(&wt(), &kk());
    assert_eq!(dp.value, 25_000_000i128);
    assert_eq!(dp.confidence, 95u32);

    // only one submission in the list (overwrite, not append)
    let agg = c.get_aggregated(&wt(), &kk());
    assert_eq!(agg.oracle_count, 1);
}

#[test]
fn even_count_median_is_average_of_middle_two() {
    let (env, admin, cid) = setup();
    let c  = OracleVerifierClient::new(&env, &cid);
    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    c.add_oracle(&admin, &o1, &wt(), &50u32);
    c.add_oracle(&admin, &o2, &wt(), &50u32);

    // sorted: [10, 30] → average = 20
    c.submit_data(&o1, &wt(), &kk(), &10_000_000i128, &90u32, &1u64);
    c.submit_data(&o2, &wt(), &kk(), &30_000_000i128, &80u32, &2u64);

    let agg = c.get_aggregated(&wt(), &kk());
    assert_eq!(agg.median_value, 20_000_000i128);
}

#[test]
fn test_weighted_median() {
    let (env, admin, cid) = setup();
    let c  = OracleVerifierClient::new(&env, &cid);
    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    c.add_oracle(&admin, &o1, &wt(), &90u32);
    c.add_oracle(&admin, &o2, &wt(), &10u32);

    // o1 submits 100, o2 submits 50
    c.submit_data(&o1, &wt(), &kk(), &100_000_000i128, &90u32, &1u64);
    c.submit_data(&o2, &wt(), &kk(), &50_000_000i128, &90u32, &2u64);

    // sorted: (50, 10), (100, 90) -> total 100, half 50.
    // i=0: 10 < 50
    // i=1: 100 > 50 -> return 100.
    let agg = c.get_aggregated(&wt(), &kk());
    assert_eq!(agg.median_value, 100_000_000i128);
}

#[test]
fn verify_trigger_greater_than() {
    let (env, admin, cid) = setup();
    let c      = OracleVerifierClient::new(&env, &cid);
    let oracle = Address::generate(&env);
    c.add_oracle(&admin, &oracle, &wt(), &90u32);
    c.submit_data(&oracle, &wt(), &kk(), &80_000_000i128, &90u32, &1u64);

    let condition = TriggerCondition {
        data_type:  wt(),
        key:        kk(),
        threshold:  50_000_000i128,
        comparison: TriggerComparison::GreaterThan,
    };
    assert!(c.verify_trigger(&wt(), &kk(), &condition));
}
