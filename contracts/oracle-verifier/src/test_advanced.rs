//! Advanced oracle-verifier tests: multi-oracle median, confidence weighting,
//! oracle deactivation, and odd/even count median edge cases.

extern crate std;

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    Env, Symbol,
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

// ── Issue #261: zero-division guard when total_votes / total_weight == 0 ────

/// When no oracles have submitted data, calling verify_trigger must panic with
/// NoDataAvailable (#6) instead of dividing by zero in the median calculation.
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_verify_trigger_panics_when_no_submissions() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let oracle = Address::generate(&env);
    c.add_oracle(&admin, &oracle, &wt(), &90u32);

    // Oracle is registered but has NOT submitted any data.
    // total_weight == 0, n == 0 → must not divide by zero.
    let condition = TriggerCondition {
        data_type: wt(),
        key: kk(),
        threshold: 50_000_000i128,
        comparison: TriggerComparison::LessThan,
        tolerance: 0i128,
    };
    c.verify_trigger(&wt(), &kk(), &condition);
}

/// get_aggregated must also panic with NoDataAvailable when there are zero
/// submissions, rather than producing a division-by-zero in the weighted
/// confidence calculation.
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_get_aggregated_panics_when_no_submissions() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let oracle = Address::generate(&env);
    c.add_oracle(&admin, &oracle, &wt(), &80u32);

    // No data submitted → total_weight == 0.
    c.get_aggregated(&wt(), &kk());
}

/// When all registered oracles are deactivated after submitting data, the
/// median calculation must reject with NoDataAvailable rather than dividing
/// by total_weight == 0.
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_verify_trigger_panics_when_all_oracles_deactivated() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let oracle = Address::generate(&env);
    c.add_oracle(&admin, &oracle, &wt(), &90u32);

    env.ledger().set_timestamp(1);
    c.submit_data(&oracle, &wt(), &kk(), &30_000_000i128, &90u32, &1u64);

    // Deactivate the only oracle — its submission remains, but total_weight
    // drops to 0 because the oracle is no longer active.
    c.remove_oracle(&admin, &oracle, &wt());

    let condition = TriggerCondition {
        data_type: wt(),
        key: kk(),
        threshold: 50_000_000i128,
        comparison: TriggerComparison::LessThan,
        tolerance: 0i128,
    };
    c.verify_trigger(&wt(), &kk(), &condition);
}

// ── #356: admin transfer timelock ────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #23)")]
fn accept_admin_rejected_before_timelock() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let new_admin = Address::generate(&env);
    c.propose_new_admin(&admin, &new_admin);
    c.accept_admin(&new_admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #23)")]
fn accept_admin_rejected_one_second_before_timelock() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let new_admin = Address::generate(&env);
    c.propose_new_admin(&admin, &new_admin);
    env.ledger().with_mut(|l| l.timestamp += parashield_common::ADMIN_TRANSFER_TIMELOCK - 1);
    c.accept_admin(&new_admin);
}

#[test]
fn accept_admin_succeeds_exactly_at_and_after_timelock() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let new_admin = Address::generate(&env);
    c.propose_new_admin(&admin, &new_admin);
    env.ledger().with_mut(|l| l.timestamp += parashield_common::ADMIN_TRANSFER_TIMELOCK);
    c.accept_admin(&new_admin);
    assert_eq!(c.get_admin(), new_admin);
    assert_eq!(c.get_pending_admin_since(), 0);
}

#[test]
fn accept_admin_succeeds_after_timelock() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let new_admin = Address::generate(&env);
    c.propose_new_admin(&admin, &new_admin);
    env.ledger().with_mut(|l| l.timestamp += parashield_common::ADMIN_TRANSFER_TIMELOCK + 1);
    c.accept_admin(&new_admin);
    assert_eq!(c.get_admin(), new_admin);
    assert_eq!(c.get_pending_admin_since(), 0);
}

// ── Per-data-type freshness (issue #371) ─────────────────────────────────────

/// Everything `submit_from_many` needs to register oracles and post one
/// reading each. Grouped into a struct so the helper stays within clippy's
/// argument limit.
struct BulkSubmission<'a> {
    data_type: Symbol,
    key: Symbol,
    /// One `(value, weight)` pair per oracle to register.
    readings: &'a [(i128, u32)],
    timestamp: u64,
}

/// Register one oracle per reading and have each submit exactly once, so the
/// per-oracle rate limit is never tripped.
fn submit_from_many(
    env: &Env,
    client: &OracleVerifierClient,
    admin: &Address,
    submission: BulkSubmission,
) {
    for &(value, weight) in submission.readings.iter() {
        let oracle = Address::generate(env);
        client.add_oracle(admin, &oracle, &submission.data_type, &weight);
        client.submit_data(
            &oracle,
            &submission.data_type,
            &submission.key,
            &value,
            &90u32,
            &submission.timestamp,
        );
    }
}

#[test]
fn test_data_type_max_age_defaults_to_global() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    client.set_max_data_age(&admin, &3_600u64);

    // No override set for this data type — the global value applies.
    assert_eq!(client.get_data_type_max_age(&wt()), 3_600);
}

#[test]
fn test_data_type_max_age_override_wins() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    client.set_max_data_age(&admin, &604_800u64);
    let flight = Symbol::new(&env, "flight");
    client.set_data_type_max_age(&admin, &flight, &600u64);

    // A flight feed goes stale in minutes; rainfall is fine for a week.
    assert_eq!(client.get_data_type_max_age(&flight), 600);
    assert_eq!(client.get_data_type_max_age(&wt()), 604_800);
}

#[test]
fn test_clearing_data_type_max_age_restores_global() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    client.set_max_data_age(&admin, &604_800u64);
    client.set_data_type_max_age(&admin, &wt(), &600u64);
    assert_eq!(client.get_data_type_max_age(&wt()), 600);

    client.set_data_type_max_age(&admin, &wt(), &0u64);
    assert_eq!(client.get_data_type_max_age(&wt()), 604_800);
}

#[test]
fn test_per_type_age_makes_data_stale_before_global_would() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let now = env.ledger().timestamp();
    client.set_max_data_age(&admin, &604_800u64);
    submit_from_many(
        &env,
        &client,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[(30_000_000, 100)],
            timestamp: now,
        },
    );

    // Still fresh under the global week-long window.
    env.ledger().set_timestamp(now + 3_600);
    assert!(client.check_freshness(&wt(), &kk()).is_fresh);

    // Tighten just this data type to 10 minutes — the same point is now stale.
    client.set_data_type_max_age(&admin, &wt(), &600u64);
    let report = client.check_freshness(&wt(), &kk());
    assert!(!report.is_fresh);
    assert_eq!(report.fresh_count, 0);
    assert_eq!(report.total_count, 1);
    assert_eq!(report.max_age, 600);
}

#[test]
fn test_check_freshness_reports_ages_without_panicking() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let now = env.ledger().timestamp();
    client.set_max_data_age(&admin, &3_600u64);
    submit_from_many(
        &env,
        &client,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[(30_000_000, 100)],
            timestamp: now,
        },
    );

    env.ledger().set_timestamp(now + 100);
    let report = client.check_freshness(&wt(), &kk());

    assert!(report.is_fresh);
    assert_eq!(report.newest_age, 100);
    assert_eq!(report.fresh_count, 1);
    assert_eq!(report.max_age, 3_600);
}

#[test]
fn test_check_freshness_on_unknown_key_returns_not_fresh() {
    let (env, _admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    // Every other read path panics here. This one answers, so a caller can
    // decide whether to evaluate rather than losing the transaction.
    let report = client.check_freshness(&wt(), &kk());

    assert!(!report.is_fresh);
    assert_eq!(report.total_count, 0);
    assert_eq!(report.fresh_count, 0);
    assert_eq!(report.newest_age, u64::MAX);
}

#[test]
fn test_stale_data_is_excluded_from_verification() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let now = env.ledger().timestamp();
    client.set_data_type_max_age(&admin, &wt(), &600u64);
    submit_from_many(
        &env,
        &client,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[(30_000_000, 100)],
            timestamp: now,
        },
    );

    let condition = TriggerCondition {
        data_type: wt(),
        key: kk(),
        threshold: 50_000_000,
        comparison: TriggerComparison::LessThan,
        tolerance: 0,
    };

    // Fresh: the trigger evaluates.
    assert!(client.verify_trigger(&wt(), &kk(), &condition));

    // Past the per-type window, the only submission no longer counts.
    env.ledger().set_timestamp(now + 601);
    assert!(!client.check_freshness(&wt(), &kk()).is_fresh);
}

// ── Aggregation methods (issue #375) ─────────────────────────────────────────

#[test]
fn test_default_aggregation_is_weighted_median() {
    let (env, _admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    assert_eq!(
        client.get_aggregation_method(&wt()),
        AggregationMethod::WeightedMedian
    );
}

#[test]
fn test_weighted_average_differs_from_median() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let now = env.ledger().timestamp();
    // Equal weights, one far outlier: mean is dragged, median is not.
    submit_from_many(
        &env,
        &client,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[(10_000_000, 100), (20_000_000, 100), (300_000_000, 100)],
            timestamp: now,
        },
    );

    let median = client.get_aggregated(&wt(), &kk()).median_value;
    assert_eq!(median, 20_000_000, "median ignores the outlier");

    client.set_aggregation_method(&admin, &wt(), &AggregationMethod::WeightedAverage);
    let avg = client.get_aggregated(&wt(), &kk()).median_value;

    // (10 + 20 + 300) / 3 = 110
    assert_eq!(avg, 110_000_000, "the average is dragged by the outlier");
}

#[test]
fn test_weighted_average_respects_oracle_weight() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let now = env.ledger().timestamp();
    // 30 at weight 300, 60 at weight 100 → (30*300 + 60*100) / 400 = 37.5
    submit_from_many(
        &env,
        &client,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[(30_000_000, 75), (60_000_000, 25)],
            timestamp: now,
        },
    );

    client.set_aggregation_method(&admin, &wt(), &AggregationMethod::WeightedAverage);
    let avg = client.get_aggregated(&wt(), &kk()).median_value;

    // (30 * 75 + 60 * 25) / 100 = 37.5
    assert_eq!(avg, 37_500_000);
}

#[test]
fn test_mean_ignores_weights() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let now = env.ledger().timestamp();
    submit_from_many(
        &env,
        &client,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[(30_000_000, 99), (60_000_000, 1)],
            timestamp: now,
        },
    );

    client.set_aggregation_method(&admin, &wt(), &AggregationMethod::Mean);
    let mean = client.get_aggregated(&wt(), &kk()).median_value;

    // Weights are lopsided but ignored: (30 + 60) / 2 = 45
    assert_eq!(mean, 45_000_000);
}

#[test]
fn test_aggregation_method_is_per_data_type() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let flight = Symbol::new(&env, "flight");
    client.set_aggregation_method(&admin, &flight, &AggregationMethod::Mean);

    assert_eq!(client.get_aggregation_method(&flight), AggregationMethod::Mean);
    assert_eq!(
        client.get_aggregation_method(&wt()),
        AggregationMethod::WeightedMedian,
        "other data types keep the safe default"
    );
}

#[test]
fn test_aggregation_method_changes_trigger_outcome() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let now = env.ledger().timestamp();
    submit_from_many(
        &env,
        &client,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[(10_000_000, 100), (20_000_000, 100), (300_000_000, 100)],
            timestamp: now,
        },
    );

    // Threshold sits between the median (20) and the mean (110).
    let condition = TriggerCondition {
        data_type: wt(),
        key: kk(),
        threshold: 50_000_000,
        comparison: TriggerComparison::LessThan,
        tolerance: 0,
    };

    assert!(
        client.verify_trigger(&wt(), &kk(), &condition),
        "median 20 < 50"
    );

    client.set_aggregation_method(&admin, &wt(), &AggregationMethod::WeightedAverage);
    assert!(
        !client.verify_trigger(&wt(), &kk(), &condition),
        "average 110 is not < 50"
    );
}

// ── Time-weighted average aggregation ────────────────────────────────────────

#[test]
fn test_time_weighted_average_weights_longer_held_values_more() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    client.add_oracle(&admin, &o1, &wt(), &100u32);
    client.add_oracle(&admin, &o2, &wt(), &100u32);

    // o1's reading holds for 800s before o2 supersedes it; o2's reading
    // then holds for only 200s before "now".
    let t0 = env.ledger().timestamp();
    client.submit_data(&o1, &wt(), &kk(), &10_000_000, &90u32, &t0);

    env.ledger().set_timestamp(t0 + 800);
    client.submit_data(&o2, &wt(), &kk(), &110_000_000, &90u32, &(t0 + 800));

    env.ledger().set_timestamp(t0 + 1000);

    client.set_aggregation_method(&admin, &wt(), &AggregationMethod::TimeWeightedAverage);
    let twa = client.get_aggregated(&wt(), &kk()).median_value;

    // (10 * 800 + 110 * 200) / 1000 = 30
    assert_eq!(twa, 30_000_000);

    // A plain mean treats both snapshots as equally significant regardless
    // of how long each one held, so it lands far higher.
    client.set_aggregation_method(&admin, &wt(), &AggregationMethod::Mean);
    let mean = client.get_aggregated(&wt(), &kk()).median_value;
    assert_eq!(mean, 60_000_000, "plain mean ignores how long each value held");
}

#[test]
fn test_time_weighted_average_respects_oracle_weight() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    client.add_oracle(&admin, &o1, &wt(), &75u32);
    client.add_oracle(&admin, &o2, &wt(), &25u32);

    let t0 = env.ledger().timestamp();
    client.submit_data(&o1, &wt(), &kk(), &10_000_000, &90u32, &t0);

    env.ledger().set_timestamp(t0 + 100);
    client.submit_data(&o2, &wt(), &kk(), &110_000_000, &90u32, &(t0 + 100));

    env.ledger().set_timestamp(t0 + 200);

    client.set_aggregation_method(&admin, &wt(), &AggregationMethod::TimeWeightedAverage);
    let twa = client.get_aggregated(&wt(), &kk()).median_value;

    // Both readings hold for 100s, but o1 carries 3x the weight:
    // (10*100*75 + 110*100*25) / (100*75 + 100*25) = 35
    assert_eq!(twa, 35_000_000);
}

#[test]
fn test_time_weighted_average_single_submission_returns_value() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let o1 = Address::generate(&env);
    client.add_oracle(&admin, &o1, &wt(), &100u32);

    let t0 = env.ledger().timestamp();
    client.submit_data(&o1, &wt(), &kk(), &42_000_000, &90u32, &t0);

    client.set_aggregation_method(&admin, &wt(), &AggregationMethod::TimeWeightedAverage);
    assert_eq!(client.get_aggregated(&wt(), &kk()).median_value, 42_000_000);
}

#[test]
fn test_time_weighted_average_used_by_verify_trigger() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    client.add_oracle(&admin, &o1, &wt(), &100u32);
    client.add_oracle(&admin, &o2, &wt(), &100u32);

    let t0 = env.ledger().timestamp();
    client.submit_data(&o1, &wt(), &kk(), &10_000_000, &90u32, &t0);

    env.ledger().set_timestamp(t0 + 800);
    client.submit_data(&o2, &wt(), &kk(), &110_000_000, &90u32, &(t0 + 800));

    env.ledger().set_timestamp(t0 + 1000);

    client.set_aggregation_method(&admin, &wt(), &AggregationMethod::TimeWeightedAverage);

    // Same scenario as test_time_weighted_average_weights_longer_held_values_more:
    // TWA = 30, which is < 50. verify_trigger has its own aggregation code
    // path (`get_median_value`), separate from `get_aggregated` — exercise
    // it here so both stay in sync.
    let condition = TriggerCondition {
        data_type: wt(),
        key: kk(),
        threshold: 50_000_000,
        comparison: TriggerComparison::LessThan,
        tolerance: 0,
    };
    assert!(client.verify_trigger(&wt(), &kk(), &condition));
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_set_aggregation_method_requires_admin() {
    let (env, _admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let impostor = Address::generate(&env);
    client.set_aggregation_method(&impostor, &wt(), &AggregationMethod::Mean);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_set_data_type_max_age_requires_admin() {
    let (env, _admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let impostor = Address::generate(&env);
    client.set_data_type_max_age(&impostor, &wt(), &600u64);
}

// ── outlier detection (issue #383) ──────────────────────────────────────────

#[test]
fn outlier_config_disabled_by_default() {
    let (env, _admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let cfg = c.get_outlier_config(&wt());
    assert!(!cfg.enabled);
    assert_eq!(cfg.threshold_bps, DEFAULT_OUTLIER_THRESHOLD_MULTIPLIER_BPS);
    assert_eq!(cfg.min_sample_size, DEFAULT_OUTLIER_MIN_SAMPLE_SIZE);
}

#[test]
#[should_panic(expected = "Error(Contract, #27)")]
fn set_outlier_config_rejects_zero_threshold() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    c.set_outlier_config(&admin, &wt(), &true, &0u32, &4u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #27)")]
fn set_outlier_config_rejects_tiny_sample_size() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    c.set_outlier_config(&admin, &wt(), &true, &50_000u32, &2u32);
}

/// Five oracles, four in tight agreement and one reporting a value 1000x
/// larger. With outlier detection off, `Mean` (which has no built-in
/// resistance to outliers the way `WeightedMedian` does) is dragged far
/// above what every well-behaved oracle actually reported.
#[test]
fn mean_without_outlier_filtering_is_skewed_by_one_bad_oracle() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    c.set_aggregation_method(&admin, &wt(), &AggregationMethod::Mean);

    let oracles: std::vec::Vec<Address> = (0..5).map(|_| Address::generate(&env)).collect();
    for o in oracles.iter() {
        c.add_oracle(&admin, o, &wt(), &50u32);
    }

    env.ledger().set_timestamp(1);
    let good = 100_0000000i128;
    let bad = 100_000_0000000i128; // 1000x the honest value
    for o in oracles.iter().take(4) {
        c.submit_data(o, &wt(), &kk(), &good, &90u32, &1u64);
    }
    c.submit_data(&oracles[4], &wt(), &kk(), &bad, &90u32, &1u64);

    let agg = c.get_aggregated(&wt(), &kk());
    // (4*100 + 100_000) / 5 = 20_080 — nowhere near the honest value of 100.
    assert!(agg.median_value > good * 100);
}

/// Same five submissions as above, but with outlier detection enabled for
/// the data type. The lone extreme submission is dropped before
/// aggregation, so `Mean` reflects only the four honest oracles.
#[test]
fn outlier_filtering_protects_mean_from_one_bad_oracle() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    c.set_aggregation_method(&admin, &wt(), &AggregationMethod::Mean);
    c.set_outlier_config(&admin, &wt(), &true, &DEFAULT_OUTLIER_THRESHOLD_MULTIPLIER_BPS, &DEFAULT_OUTLIER_MIN_SAMPLE_SIZE);

    let oracles: std::vec::Vec<Address> = (0..5).map(|_| Address::generate(&env)).collect();
    for o in oracles.iter() {
        c.add_oracle(&admin, o, &wt(), &50u32);
    }

    env.ledger().set_timestamp(1);
    let good = 100_0000000i128;
    let bad = 100_000_0000000i128;
    for o in oracles.iter().take(4) {
        c.submit_data(o, &wt(), &kk(), &good, &90u32, &1u64);
    }
    c.submit_data(&oracles[4], &wt(), &kk(), &bad, &90u32, &1u64);

    let agg = c.get_aggregated(&wt(), &kk());
    assert_eq!(agg.median_value, good);
}

/// Outlier detection never removes enough entries to drop below
/// `min_sample_size` — with exactly four submissions and a `min_sample_size`
/// of 4, the arrangement has no slack to trim, so even a wild outlier
/// survives into the aggregate untouched.
#[test]
fn outlier_filtering_never_drops_below_min_sample_size() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    c.set_aggregation_method(&admin, &wt(), &AggregationMethod::Mean);
    c.set_outlier_config(&admin, &wt(), &true, &DEFAULT_OUTLIER_THRESHOLD_MULTIPLIER_BPS, &4u32);

    let oracles: std::vec::Vec<Address> = (0..4).map(|_| Address::generate(&env)).collect();
    for o in oracles.iter() {
        c.add_oracle(&admin, o, &wt(), &50u32);
    }

    env.ledger().set_timestamp(1);
    let good = 100_0000000i128;
    let bad = 100_000_0000000i128;
    for o in oracles.iter().take(3) {
        c.submit_data(o, &wt(), &kk(), &good, &90u32, &1u64);
    }
    c.submit_data(&oracles[3], &wt(), &kk(), &bad, &90u32, &1u64);

    let agg = c.get_aggregated(&wt(), &kk());
    // All 4 submissions survive (below the slack needed to trim), so the
    // outlier still drags the mean up.
    assert!(agg.median_value > good * 100);
}

// ── default outlier threshold pinned (issue #829) ───────────────────────────

/// The default is a multiplier of the MAD in basis points: 50_000 = 5.0x.
/// Pinned so a change to the value is a deliberate, visible decision.
#[test]
fn default_outlier_threshold_is_five_times_the_mad() {
    let (env, _admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);

    assert_eq!(DEFAULT_OUTLIER_THRESHOLD_MULTIPLIER_BPS, 50_000u32);
    // 10_000 bps = 1.0x the MAD, so the default is exactly 5.0x.
    assert_eq!(DEFAULT_OUTLIER_THRESHOLD_MULTIPLIER_BPS / 10_000, 5u32);
    assert_eq!(DEFAULT_OUTLIER_THRESHOLD_MULTIPLIER_BPS % 10_000, 0u32);
    assert_eq!(
        c.get_outlier_config(&wt()).threshold_bps,
        DEFAULT_OUTLIER_THRESHOLD_MULTIPLIER_BPS
    );
}

/// Submit `outlier` alongside four readings whose median is 100 and whose
/// MAD is 10 (all in 7-decimal fixed point), with outlier filtering enabled
/// at the default threshold, and return the `Mean` aggregate.
///
/// Sorted: [90, 100, 100, 110, outlier] → median 100.
/// Deviations: [10, 0, 0, 10, outlier - 100] → sorted middle (MAD) = 10.
/// At 5.0x the MAD the cutoff deviation is 50, i.e. an outlier of 150.
fn mean_with_outlier(outlier: i128) -> i128 {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    c.set_aggregation_method(&admin, &wt(), &AggregationMethod::Mean);
    c.set_outlier_config(
        &admin,
        &wt(),
        &true,
        &DEFAULT_OUTLIER_THRESHOLD_MULTIPLIER_BPS,
        &DEFAULT_OUTLIER_MIN_SAMPLE_SIZE,
    );

    let oracles: std::vec::Vec<Address> = (0..5).map(|_| Address::generate(&env)).collect();
    for o in oracles.iter() {
        c.add_oracle(&admin, o, &wt(), &50u32);
    }

    env.ledger().set_timestamp(1);
    let values = [90_0000000i128, 100_0000000, 100_0000000, 110_0000000, outlier];
    for (o, v) in oracles.iter().zip(values.iter()) {
        c.submit_data(o, &wt(), &kk(), v, &90u32, &1u64);
    }

    c.get_aggregated(&wt(), &kk()).median_value
}

/// A reading exactly 5.0x the MAD from the median sits on the boundary and
/// is kept: the comparison is strict (`>`), so the mean includes it.
#[test]
fn reading_at_exactly_five_times_the_mad_is_kept() {
    // (90 + 100 + 100 + 110 + 150) / 5 = 110
    assert_eq!(mean_with_outlier(150_0000000), 110_0000000);
}

/// One unit past the 5.0x boundary is flagged and dropped, so the mean
/// reflects only the four in-range readings.
#[test]
fn reading_just_past_five_times_the_mad_is_dropped() {
    // (90 + 100 + 100 + 110) / 4 = 100
    assert_eq!(mean_with_outlier(150_0000001), 100_0000000);
}

// ── Issue #835: weighted median, mean, outlier rejection, cross-validation,
//    freshness and staleness edge cases ──────────────────────────────────────

/// Odd submission count with unequal weights: a single heavy oracle whose
/// weight exceeds half the total weight decides the weighted median.
#[test]
fn weighted_median_odd_count_heavy_oracle_decides() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let now = env.ledger().timestamp();

    // Sorted by (value, weight): [(10, 10), (20, 10), (30, 80)]
    // total_weight = 10 + 10 + 80 = 100, half = 50.
    // Cumulative weights: 10 (<=50), 20 (<=50), 100 (>50) -> 30_000_000.
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[(30_000_000, 80), (10_000_000, 10), (20_000_000, 10)],
            timestamp: now,
        },
    );

    assert_eq!(c.get_aggregated(&wt(), &kk()).median_value, 30_000_000);
}

/// Even submission count with equal weights: cumulative weight lands exactly on
/// `total_weight / 2` (with even total_weight), returning the midpoint of the
/// two middle values.
#[test]
fn weighted_median_even_count_equal_weights_averages_middle_two() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let now = env.ledger().timestamp();

    // Sorted: [(10, 25), (20, 25), (40, 25), (80, 25)]
    // total_weight = 100, half = 50.
    // Cumulative after 20 is 50 == half (and 100 is even) -> (20 + 40) / 2 = 30.
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[
                (80_000_000, 25),
                (10_000_000, 25),
                (40_000_000, 25),
                (20_000_000, 25),
            ],
            timestamp: now,
        },
    );

    assert_eq!(c.get_aggregated(&wt(), &kk()).median_value, 30_000_000);
}

/// Even submission count with unequal weights hitting exact half vs crossing
/// half on a heavy oracle.
#[test]
fn weighted_median_even_count_unequal_weights_exact_half_and_heavy_cross() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let now = env.ledger().timestamp();

    // Case 1 (exact half): sorted [(10, 20), (20, 30), (50, 10), (90, 40)]
    // total_weight = 100, half = 50.
    // Cumulative after 20 = 20 + 30 = 50 == half -> (20 + 50) / 2 = 35_000_000.
    let key1 = symbol_short!("even1");
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: key1.clone(),
            readings: &[
                (10_000_000, 20),
                (20_000_000, 30),
                (50_000_000, 10),
                (90_000_000, 40),
            ],
            timestamp: now,
        },
    );
    assert_eq!(c.get_aggregated(&wt(), &key1).median_value, 35_000_000);

    // Case 2 (heavy oracle crosses half): sorted [(10, 10), (20, 10), (50, 60), (90, 20)]
    // total_weight = 100, half = 50.
    // Cumulative: 10, 20, 80 (>50) -> 50_000_000.
    let key2 = symbol_short!("even2");
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: key2.clone(),
            readings: &[
                (10_000_000, 10),
                (20_000_000, 10),
                (50_000_000, 60),
                (90_000_000, 20),
            ],
            timestamp: now,
        },
    );
    assert_eq!(c.get_aggregated(&wt(), &key2).median_value, 50_000_000);
}

/// Single submission returns that exact value under WeightedMedian.
#[test]
fn weighted_median_single_submission_returns_value() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let now = env.ledger().timestamp();

    // total_weight = 50, half = 25; cumulative = 50 > 25 -> 42_000_000.
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[(42_000_000, 50)],
            timestamp: now,
        },
    );

    assert_eq!(c.get_aggregated(&wt(), &kk()).median_value, 42_000_000);
}

/// Signed (negative) values are supported and aggregated accurately for both
/// odd and even counts.
#[test]
fn weighted_median_handles_negative_values() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let now = env.ledger().timestamp();

    // Odd count: sorted [-30, -10, 20], equal weights 50 -> total 150, half 75.
    // Cumulative after -30 is 50, after -10 is 100 > 75 -> -10_000_000.
    let key_odd = symbol_short!("negodd");
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: key_odd.clone(),
            readings: &[(-30_000_000, 50), (20_000_000, 50), (-10_000_000, 50)],
            timestamp: now,
        },
    );
    assert_eq!(c.get_aggregated(&wt(), &key_odd).median_value, -10_000_000);

    // Even count: sorted [-40, -10], equal weights 50 -> total 100, half 50.
    // (-40_000_000 + -10_000_000) / 2 = -25_000_000.
    let key_even = symbol_short!("negevn");
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: key_even.clone(),
            readings: &[(-40_000_000, 50), (-10_000_000, 50)],
            timestamp: now,
        },
    );
    assert_eq!(c.get_aggregated(&wt(), &key_even).median_value, -25_000_000);
}

/// Very large values up to MAX_DATA_VALUE (10^27) on a custom data type do not
/// overflow WeightedMedian or Mean.
#[test]
fn large_values_near_max_data_value_do_not_overflow() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let now = env.ledger().timestamp();
    let custom = symbol_short!("custom");

    let v1: i128 = 900_000_000_000_000_000_000_000_000; // 9 * 10^26
    let v2: i128 = 1_000_000_000_000_000_000_000_000_000; // 10^27 == MAX_DATA_VALUE
    let expected_mid: i128 = 950_000_000_000_000_000_000_000_000; // (9e26 + 1e27) / 2

    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: custom.clone(),
            key: kk(),
            readings: &[(v1, 50), (v2, 50)],
            timestamp: now,
        },
    );

    assert_eq!(c.get_aggregated(&custom, &kk()).median_value, expected_mid);

    c.set_aggregation_method(&admin, &custom, &AggregationMethod::Mean);
    assert_eq!(c.get_aggregated(&custom, &kk()).median_value, expected_mid);
}

/// All identical submissions produce MAD = 0; since every deviation is 0 and
/// `0 * 10_000 > threshold * 0` is false, all submissions are kept.
#[test]
fn outlier_rejection_all_identical_values_mad_zero_keeps_all() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let now = env.ledger().timestamp();
    c.set_aggregation_method(&admin, &wt(), &AggregationMethod::Mean);
    c.set_outlier_config(&admin, &wt(), &true, &50_000u32, &4u32);

    // median = 50_000_000, deviations = [0, 0, 0, 0, 0], mad = 0.
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[
                (50_000_000, 50),
                (50_000_000, 50),
                (50_000_000, 50),
                (50_000_000, 50),
                (50_000_000, 50),
            ],
            timestamp: now,
        },
    );

    let agg = c.get_aggregated(&wt(), &kk());
    assert_eq!(agg.median_value, 50_000_000);
}

/// When a unanimous majority yields MAD = 0, any single differing value has
/// `deviation * 10_000 > 0` and is rejected.
#[test]
fn outlier_rejection_mad_zero_rejects_single_differing_value() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let now = env.ledger().timestamp();
    c.set_aggregation_method(&admin, &wt(), &AggregationMethod::Mean);
    c.set_outlier_config(&admin, &wt(), &true, &50_000u32, &4u32);

    // Sorted: [50_000_000, 50_000_000, 50_000_000, 50_000_000, 50_000_001]
    // median = 50_000_000, deviations = [0, 0, 0, 0, 1], mad = 0.
    // For 50_000_001: 1 * 10_000 = 10_000 > 50_000 * 0 = 0 -> dropped.
    // Mean of remaining 4 = 50_000_000.
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[
                (50_000_000, 50),
                (50_000_000, 50),
                (50_000_000, 50),
                (50_000_000, 50),
                (50_000_001, 50),
            ],
            timestamp: now,
        },
    );

    assert_eq!(c.get_aggregated(&wt(), &kk()).median_value, 50_000_000);
}

/// `min_sample_size` boundaries: one below (filtering skipped), exact (no
/// trimming slack), one above (single outlier dropped), and multiple outliers
/// where trimming stops at `min_sample_size` after removing the worst offender.
#[test]
fn outlier_rejection_min_sample_size_boundaries() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let now = env.ledger().timestamp();
    c.set_aggregation_method(&admin, &wt(), &AggregationMethod::Mean);
    c.set_outlier_config(&admin, &wt(), &true, &20_000u32, &4u32);

    // Register 5 oracles once (staying within MAX_ORACLES_PER_KEY = 10) and
    // reuse them across distinct keys — enabled by per-key rate limiting (#834).
    let oracles: std::vec::Vec<Address> = (0..5).map(|_| Address::generate(&env)).collect();
    for o in oracles.iter() {
        c.add_oracle(&admin, o, &wt(), &50u32);
    }

    // 1. Below min_sample_size (n = 3 < 4): filtering skipped.
    // Mean = (100 + 100 + 1000) / 3 = 400.
    let k_below = symbol_short!("msbelow");
    let vals_below = [100_000_000i128, 100_000_000, 1_000_000_000];
    for (o, v) in oracles.iter().zip(vals_below.iter()) {
        c.submit_data(o, &wt(), &k_below, v, &90u32, &now);
    }
    assert_eq!(c.get_aggregated(&wt(), &k_below).median_value, 400_000_000);

    // 2. Exactly at min_sample_size (n = 4 == 4): no slack to drop any point.
    // Mean = (100 + 100 + 100 + 1000) / 4 = 325.
    let k_exact = symbol_short!("msexact");
    let vals_exact = [100_000_000i128, 100_000_000, 100_000_000, 1_000_000_000];
    for (o, v) in oracles.iter().zip(vals_exact.iter()) {
        c.submit_data(o, &wt(), &k_exact, v, &90u32, &now);
    }
    assert_eq!(c.get_aggregated(&wt(), &k_exact).median_value, 325_000_000);

    // 3. One above min_sample_size (n = 5 > 4) with 1 outlier: outlier dropped.
    // Mean of remaining 4 = 100.
    let k_above = symbol_short!("msabove");
    let vals_above = [
        100_000_000i128,
        100_000_000,
        100_000_000,
        100_000_000,
        1_000_000_000,
    ];
    for (o, v) in oracles.iter().zip(vals_above.iter()) {
        c.submit_data(o, &wt(), &k_above, v, &90u32, &now);
    }
    assert_eq!(c.get_aggregated(&wt(), &k_above).median_value, 100_000_000);

    // 4. n = 5, min_sample_size = 4, two outliers (500 and 1000):
    // median = 100, mad = 0; worst offender (1000) dropped first, leaving 4 == min_keep.
    // Mean of survivors [100, 100, 100, 500] = 800 / 4 = 200.
    let k_two = symbol_short!("mstwo");
    let vals_two = [
        100_000_000i128,
        100_000_000,
        100_000_000,
        500_000_000,
        1_000_000_000,
    ];
    for (o, v) in oracles.iter().zip(vals_two.iter()) {
        c.submit_data(o, &wt(), &k_two, v, &90u32, &now);
    }
    assert_eq!(c.get_aggregated(&wt(), &k_two).median_value, 200_000_000);
}

/// Custom MAD threshold (2.0x = 20_000 bps): borderline value at exactly 2.0x
/// MAD is kept, while one unit beyond 2.0x MAD is rejected.
#[test]
fn outlier_rejection_custom_mad_threshold_borderline_vs_outlier() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let now = env.ledger().timestamp();
    c.set_aggregation_method(&admin, &wt(), &AggregationMethod::Mean);
    c.set_outlier_config(&admin, &wt(), &true, &20_000u32, &4u32);

    // Sorted [80, 100, 100, 120, 140] -> median = 100, devs = [0, 0, 20, 20, 40], mad = 20.
    // Borderline 140: dev = 40; 40 * 10_000 = 400_000 > 20_000 * 20 = 400_000 is false -> kept.
    // Mean = (80 + 100 + 100 + 120 + 140) / 5 = 540 / 5 = 108.
    let k_border = symbol_short!("madbord");
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: k_border.clone(),
            readings: &[
                (80_000_000, 50),
                (100_000_000, 50),
                (100_000_000, 50),
                (120_000_000, 50),
                (140_000_000, 50),
            ],
            timestamp: now,
        },
    );
    assert_eq!(c.get_aggregated(&wt(), &k_border).median_value, 108_000_000);

    // Outlier 140_000_001: dev = 40_000_001 > 40_000_000 -> dropped.
    // Mean = (80 + 100 + 100 + 120) / 4 = 400 / 4 = 100.
    let k_out = symbol_short!("madout");
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: k_out.clone(),
            readings: &[
                (80_000_000, 50),
                (100_000_000, 50),
                (100_000_000, 50),
                (120_000_000, 50),
                (140_000_001, 50),
            ],
            timestamp: now,
        },
    );
    assert_eq!(c.get_aggregated(&wt(), &k_out).median_value, 100_000_000);
}

/// `AggregationMethod::Mean` vs `WeightedMedian` on the same dataset produce
/// distinct, hand-verified results differing by a known amount.
#[test]
fn mean_versus_weighted_median_known_difference() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let now = env.ledger().timestamp();

    // Readings: (10, weight 60), (20, weight 20), (90, weight 20).
    // WeightedMedian: total_weight = 100, half = 50; cum after 10 = 60 > 50 -> 10_000_000.
    // Mean: (10_000_000 + 20_000_000 + 90_000_000) / 3 = 120_000_000 / 3 = 40_000_000.
    // Difference: 40_000_000 - 10_000_000 = 30_000_000.
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[(10_000_000, 60), (20_000_000, 20), (90_000_000, 20)],
            timestamp: now,
        },
    );

    let wmed = c.get_aggregated(&wt(), &kk()).median_value;
    assert_eq!(wmed, 10_000_000);

    c.set_aggregation_method(&admin, &wt(), &AggregationMethod::Mean);
    let mean = c.get_aggregated(&wt(), &kk()).median_value;
    assert_eq!(mean, 40_000_000);
    assert_eq!(mean - wmed, 30_000_000);
}

/// Cross-validation between data types: skips when target has no data, accepts
/// variance <= max_variance (including exact boundary), rejects variance >
/// max_variance with Error::CrossValidationFailed (#29), and succeeds after
/// rule removal.
#[test]
fn cross_validation_between_data_types_boundary_and_removal() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let now = env.ledger().timestamp();

    let temp = symbol_short!("temp");
    let o_weather = Address::generate(&env);
    let o_temp = Address::generate(&env);
    c.add_oracle(&admin, &o_weather, &wt(), &100u32);
    c.add_oracle(&admin, &o_temp, &temp, &100u32);
    c.set_min_submit_interval(&admin, &0u64);

    // Configure rule: |weather - temp| <= 10_000_000.
    c.set_cross_validation_rule(
        &admin,
        &wt(),
        &temp,
        &10_000_000i128,
        &soroban_sdk::Bytes::from_slice(&env, b"weather vs temp"),
    );

    // Target (`temp`) has no data yet -> cross-validation is skipped gracefully.
    c.submit_data(&o_weather, &wt(), &kk(), &100_000_000, &95u32, &(now - 10));

    // Populate target `temp` = 25_000_000.
    c.submit_data(&o_temp, &temp, &kk(), &25_000_000, &95u32, &(now - 10));

    // Exact boundary: |35_000_000 - 25_000_000| = 10_000_000 <= 10_000_000 -> accepted.
    c.submit_data(&o_weather, &wt(), &kk(), &35_000_000, &95u32, &(now - 5));
    assert_eq!(c.get_data(&wt(), &kk()).value, 35_000_000);

    // One unit over boundary: |35_000_001 - 25_000_000| = 10_000_001 > 10_000_000 -> rejected.
    let res = c.try_submit_data(&o_weather, &wt(), &kk(), &35_000_001, &95u32, &(now - 4));
    assert_eq!(res, Err(Ok(Error::CrossValidationFailed)));

    // Remove rule -> large divergence now succeeds.
    c.remove_cross_validation_rule(&admin, &wt(), &temp);
    c.submit_data(&o_weather, &wt(), &kk(), &100_000_000, &95u32, &(now - 3));
    assert_eq!(c.get_data(&wt(), &kk()).value, 100_000_000);
}

/// `check_freshness` and `check_staleness` at one second inside, exactly at,
/// and one second outside `max_age` (return values only; no event assertions).
#[test]
fn freshness_and_staleness_exact_threshold_boundaries() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);

    let t0 = env.ledger().timestamp();
    c.set_data_type_max_age(&admin, &wt(), &600u64);
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[(30_000_000, 100)],
            timestamp: t0,
        },
    );

    // 1 second inside: age = 599 <= 600 -> fresh, not stale.
    env.ledger().set_timestamp(t0 + 599);
    let f_in = c.check_freshness(&wt(), &kk());
    assert!(f_in.is_fresh);
    assert_eq!(f_in.newest_age, 599);
    assert_eq!(f_in.fresh_count, 1);
    let s_in = c.check_staleness(&wt(), &kk());
    assert!(!s_in.is_stale);
    assert_eq!(s_in.stale_count, 0);
    assert_eq!(s_in.oldest_fresh_age, 599);
    assert_eq!(s_in.newest_age, 599);
    assert_eq!(s_in.freshness_ratio_bps, 10_000);

    // Exactly at threshold: age = 600 <= 600 -> fresh, not stale.
    env.ledger().set_timestamp(t0 + 600);
    let f_at = c.check_freshness(&wt(), &kk());
    assert!(f_at.is_fresh);
    assert_eq!(f_at.newest_age, 600);
    assert_eq!(f_at.fresh_count, 1);
    let s_at = c.check_staleness(&wt(), &kk());
    assert!(!s_at.is_stale);
    assert_eq!(s_at.stale_count, 0);
    assert_eq!(s_at.oldest_fresh_age, 600);
    assert_eq!(s_at.newest_age, 600);
    assert_eq!(s_at.freshness_ratio_bps, 10_000);

    // 1 second outside: age = 601 > 600 -> not fresh, stale.
    env.ledger().set_timestamp(t0 + 601);
    let f_out = c.check_freshness(&wt(), &kk());
    assert!(!f_out.is_fresh);
    assert_eq!(f_out.newest_age, 601);
    assert_eq!(f_out.fresh_count, 0);
    let s_out = c.check_staleness(&wt(), &kk());
    assert!(s_out.is_stale);
    assert_eq!(s_out.stale_count, 1);
    assert_eq!(s_out.oldest_fresh_age, 0);
    assert_eq!(s_out.newest_age, 601);
    assert_eq!(s_out.freshness_ratio_bps, 0);
}

/// `check_staleness` on a key with no stored data reports stale with sentinel
/// ages and zero counts.
#[test]
fn check_staleness_no_data_stored_returns_stale_report() {
    let (env, _admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);

    let report = c.check_staleness(&wt(), &kk());
    assert!(report.is_stale);
    assert_eq!(report.total_count, 0);
    assert_eq!(report.stale_count, 0);
    assert_eq!(report.oldest_fresh_age, 0);
    assert_eq!(report.newest_age, u64::MAX);
    assert_eq!(report.freshness_ratio_bps, 0);
}

/// A submission timestamp ahead of the ledger (within `TimestampFutureBuffer`
/// = 60s) saturates age to 0, so both `check_freshness` and `check_staleness`
/// treat it as fresh with age 0.
#[test]
fn freshness_and_staleness_with_future_timestamp_within_buffer() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let now = env.ledger().timestamp();

    // Timestamp 30s in the future (allowed by default 60s future buffer).
    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[(25_000_000, 100)],
            timestamp: now + 30,
        },
    );

    let fresh = c.check_freshness(&wt(), &kk());
    assert!(fresh.is_fresh);
    assert_eq!(fresh.newest_age, 0);
    assert_eq!(fresh.fresh_count, 1);

    let stale = c.check_staleness(&wt(), &kk());
    assert!(!stale.is_stale);
    assert_eq!(stale.newest_age, 0);
    assert_eq!(stale.oldest_fresh_age, 0);
    assert_eq!(stale.stale_count, 0);
    assert_eq!(stale.freshness_ratio_bps, 10_000);
}

/// Zero global `max_data_age`: only age == 0 is fresh; age == 1 is stale.
#[test]
fn freshness_and_staleness_with_zero_max_age() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let t0 = env.ledger().timestamp();
    c.set_max_data_age(&admin, &0u64);

    submit_from_many(
        &env,
        &c,
        &admin,
        BulkSubmission {
            data_type: wt(),
            key: kk(),
            readings: &[(25_000_000, 100)],
            timestamp: t0,
        },
    );

    // At t0: age = 0 <= 0 -> fresh, not stale.
    let f0 = c.check_freshness(&wt(), &kk());
    assert!(f0.is_fresh);
    assert_eq!(f0.max_age, 0);
    assert_eq!(f0.newest_age, 0);
    let s0 = c.check_staleness(&wt(), &kk());
    assert!(!s0.is_stale);
    assert_eq!(s0.max_age, 0);
    assert_eq!(s0.stale_count, 0);
    assert_eq!(s0.freshness_ratio_bps, 10_000);

    // At t0 + 1: age = 1 > 0 -> not fresh, stale.
    env.ledger().set_timestamp(t0 + 1);
    let f1 = c.check_freshness(&wt(), &kk());
    assert!(!f1.is_fresh);
    assert_eq!(f1.fresh_count, 0);
    assert_eq!(f1.newest_age, 1);
    let s1 = c.check_staleness(&wt(), &kk());
    assert!(s1.is_stale);
    assert_eq!(s1.stale_count, 1);
    assert_eq!(s1.freshness_ratio_bps, 0);
}

/// `check_staleness` flags stale only when strictly more than half of
/// submissions are stale (`stale_count > total_count / 2`), and reports
/// `freshness_ratio_bps = fresh_count * 10_000 / total_count`.
#[test]
fn check_staleness_majority_threshold_and_freshness_ratio() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let t0 = env.ledger().timestamp();
    c.set_data_type_max_age(&admin, &wt(), &1_000u64);

    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    let o3 = Address::generate(&env);
    c.add_oracle(&admin, &o1, &wt(), &100u32);
    c.add_oracle(&admin, &o2, &wt(), &100u32);
    c.add_oracle(&admin, &o3, &wt(), &100u32);

    // o1 submits at t0, o2 submits at t0 + 500 (both fresh at t0 + 500).
    c.submit_data(&o1, &wt(), &kk(), &10_000_000, &90u32, &t0);
    env.ledger().set_timestamp(t0 + 500);
    c.submit_data(&o2, &wt(), &kk(), &20_000_000, &90u32, &(t0 + 500));

    // At t0 + 1100: o1 age = 1100 (stale), o2 age = 600 (fresh).
    // stale_count = 1, total_count = 2 -> 1 > 2/2 (1 > 1) is false -> not stale.
    // freshness_ratio_bps = 1 * 10_000 / 2 = 5_000.
    env.ledger().set_timestamp(t0 + 1_100);
    let rep_half = c.check_staleness(&wt(), &kk());
    assert!(!rep_half.is_stale);
    assert_eq!(rep_half.stale_count, 1);
    assert_eq!(rep_half.total_count, 2);
    assert_eq!(rep_half.oldest_fresh_age, 600);
    assert_eq!(rep_half.newest_age, 600);
    assert_eq!(rep_half.freshness_ratio_bps, 5_000);

    // Add a second stale-to-be submission on a fresh key to test 2-of-3 stale.
    let key3 = symbol_short!("maj3");
    let t1 = t0 + 2_000;
    env.ledger().set_timestamp(t1);
    c.submit_data(&o1, &wt(), &key3, &10_000_000, &90u32, &t1);
    c.submit_data(&o2, &wt(), &key3, &20_000_000, &90u32, &t1);
    env.ledger().set_timestamp(t1 + 500);
    c.submit_data(&o3, &wt(), &key3, &30_000_000, &90u32, &(t1 + 500));

    // At t1 + 1100: o1 & o2 age = 1100 (stale), o3 age = 600 (fresh).
    // stale_count = 2 > 3/2 (= 1) -> is_stale = true.
    // freshness_ratio_bps = 1 * 10_000 / 3 = 3_333.
    env.ledger().set_timestamp(t1 + 1_100);
    let rep_two_thirds = c.check_staleness(&wt(), &key3);
    assert!(rep_two_thirds.is_stale);
    assert_eq!(rep_two_thirds.stale_count, 2);
    assert_eq!(rep_two_thirds.total_count, 3);
    assert_eq!(rep_two_thirds.freshness_ratio_bps, 3_333);
}
