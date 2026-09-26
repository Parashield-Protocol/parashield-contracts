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
fn accept_admin_succeeds_after_timelock() {
    let (env, admin, cid) = setup();
    let c = OracleVerifierClient::new(&env, &cid);
    let new_admin = Address::generate(&env);
    c.propose_new_admin(&admin, &new_admin);
    env.ledger().with_mut(|l| l.timestamp += 48 * 60 * 60 + 1);
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
    assert_eq!(cfg.threshold_bps, DEFAULT_OUTLIER_THRESHOLD_BPS);
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
    c.set_outlier_config(&admin, &wt(), &true, &DEFAULT_OUTLIER_THRESHOLD_BPS, &DEFAULT_OUTLIER_MIN_SAMPLE_SIZE);

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
    c.set_outlier_config(&admin, &wt(), &true, &DEFAULT_OUTLIER_THRESHOLD_BPS, &4u32);

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
