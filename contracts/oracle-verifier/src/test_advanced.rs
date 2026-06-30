//! Advanced oracle-verifier tests: multi-oracle median, confidence weighting,
//! oracle deactivation, and odd/even count median edge cases.

extern crate std;

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    Env,
};

fn setup() -> (Env, Address, Address) {
    let env = Env::default();

    // Fast-forward mock clock to keep validation checks happy
    env.ledger().set_timestamp(1748736000);

    env.mock_all_auths();

    let admin = Address::generate(&env);
    let contract_id = env.register(OracleVerifier, ());

    OracleVerifierClient::new(&env, &contract_id).initialize(&admin);

    (env, admin, contract_id)
}

fn wt() -> soroban_sdk::Symbol {
    symbol_short!("weather")
}
fn kk() -> soroban_sdk::Symbol {
    symbol_short!("kis2606")
}

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

    // Pin environment clock to t=1 so payload timestamps of 1 are valid
    env.ledger().set_timestamp(1);
    c.submit_data(&o1, &wt(), &kk(), &10_000_000i128, &90u32, &1u64);
    c.submit_data(&o2, &wt(), &kk(), &30_000_000i128, &90u32, &1u64);
    c.submit_data(&o3, &wt(), &kk(), &20_000_000i128, &90u32, &1u64);

    let agg = c.get_aggregated(&wt(), &kk());
    // sorted: [10, 20, 30] → middle = 20
    assert_eq!(agg.median_value, 20_000_000i128);
}

#[test]
#[should_panic(expected = "HostError: Error(Contract, #3)")] // Match Soroban's native error panic style
fn deactivated_oracle_cannot_submit() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let oracle = Address::generate(&env);
    c.add_oracle(&admin, &oracle, &wt(), &90u32);
    c.remove_oracle(&admin, &oracle, &wt());

    // Pin environment clock to t=1 so payload timestamp of 1 bypasses time-check
    env.ledger().set_timestamp(1);
    c.submit_data(&oracle, &wt(), &kk(), &10_000_000i128, &90u32, &1u64);
}

#[test]
fn overwrite_submission_updates_value() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let oracle = Address::generate(&env);
    c.add_oracle(&admin, &oracle, &wt(), &90u32);

    // Sync environment clock to match sequential incoming historical entries
    env.ledger().set_timestamp(100);
    c.submit_data(&oracle, &wt(), &kk(), &10_000_000i128, &90u32, &100u64);

    env.ledger().set_timestamp(200);
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
    let c = OracleVerifierClient::new(&env, &cid);
    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    c.add_oracle(&admin, &o1, &wt(), &50u32);
    c.add_oracle(&admin, &o2, &wt(), &50u32);

    // Track the ledger clock with sequential entry frames
    env.ledger().set_timestamp(1);
    c.submit_data(&o1, &wt(), &kk(), &10_000_000i128, &90u32, &1u64);

    env.ledger().set_timestamp(2);
    c.submit_data(&o2, &wt(), &kk(), &30_000_000i128, &80u32, &2u64);

    let agg = c.get_aggregated(&wt(), &kk());
    assert_eq!(agg.median_value, 20_000_000i128);
}

#[test]
fn test_weighted_median() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    c.add_oracle(&admin, &o1, &wt(), &90u32);
    c.add_oracle(&admin, &o2, &wt(), &10u32);

    // Align clocks sequentially
    env.ledger().set_timestamp(1);
    c.submit_data(&o1, &wt(), &kk(), &100_000_000i128, &90u32, &1u64);

    env.ledger().set_timestamp(2);
    c.submit_data(&o2, &wt(), &kk(), &50_000_000i128, &90u32, &2u64);

    let agg = c.get_aggregated(&wt(), &kk());
    assert_eq!(agg.median_value, 100_000_000i128);
}

#[test]
fn verify_trigger_greater_than() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let oracle = Address::generate(&env);
    c.add_oracle(&admin, &oracle, &wt(), &90u32);

    env.ledger().set_timestamp(1);
    c.submit_data(&oracle, &wt(), &kk(), &80_000_000i128, &90u32, &1u64);

    let condition = TriggerCondition {
        data_type: wt(),
        key: kk(),
        threshold: 50_000_000i128,
        comparison: TriggerComparison::GreaterThan,
        tolerance: 0i128,
    };
    assert!(c.verify_trigger(&wt(), &kk(), &condition));
}

#[test]
fn verify_trigger_equal_with_tolerance_success() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    let o3 = Address::generate(&env);

    c.add_oracle(&admin, &o1, &wt(), &50u32);
    c.add_oracle(&admin, &o2, &wt(), &50u32);
    c.add_oracle(&admin, &o3, &wt(), &50u32);

    env.ledger().set_timestamp(1);
    c.submit_data(&o1, &wt(), &kk(), &499_000_000i128, &90u32, &1u64);
    c.submit_data(&o2, &wt(), &kk(), &501_000_000i128, &90u32, &1u64);
    c.submit_data(&o3, &wt(), &kk(), &500_500_000i128, &90u32, &1u64);

    let condition = TriggerCondition {
        data_type: wt(),
        key: kk(),
        comparison: TriggerComparison::EqualWithTolerance,
        threshold: 500_000_000i128,
        tolerance: 1_000_000i128,
    };

    assert!(c.verify_trigger(&wt(), &kk(), &condition));
}

#[test]
fn verify_trigger_equal_with_tolerance_failure_outside_range() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let o1 = Address::generate(&env);
    c.add_oracle(&admin, &o1, &wt(), &50u32);

    env.ledger().set_timestamp(1);
    c.submit_data(&o1, &wt(), &kk(), &502_000_000i128, &90u32, &1u64);

    let condition = TriggerCondition {
        data_type: wt(),
        key: kk(),
        comparison: TriggerComparison::EqualWithTolerance,
        threshold: 500_000_000i128,
        tolerance: 1_000_000i128,
    };

    assert!(!c.verify_trigger(&wt(), &kk(), &condition));
}
