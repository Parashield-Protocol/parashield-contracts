#![allow(clippy::inconsistent_digit_grouping)]
#![allow(unused_variables)]
use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    Env, Symbol,
};

fn setup() -> (Env, Address, Address) {
    let env = Env::default();

    env.ledger().set_timestamp(1748736000);

    env.mock_all_auths();

    let admin = Address::generate(&env);
    let contract_id = env.register(OracleVerifier, ());

    let client = OracleVerifierClient::new(&env, &contract_id);
  
    client.initialize(&admin);

    (env, admin, contract_id)
}

fn weather() -> soroban_sdk::Symbol {
    symbol_short!("weather")
}

fn kisumu_key() -> soroban_sdk::Symbol {
    symbol_short!("kis2606") // "rainfall:kisumu:2026-06" compressed to 9 chars
}

// ── Initialization ────────────────────────────────────────────────────────────

#[test]
fn test_initialize_sets_admin() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    assert_eq!(client.get_admin(), admin);
}

#[test]
fn test_initialize_allows_contract_admin() {
    let env = Env::default();
    env.ledger().with_mut(|l| l.timestamp = 1748736000u64);
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let contract_id = env.register(OracleVerifier, ());
    OracleVerifierClient::new(&env, &contract_id).initialize(&admin);
    let client = OracleVerifierClient::new(&env, &contract_id);
    assert_eq!(client.get_admin(), admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_double_initialize_panics() {
    let (env, admin, contract_id) = setup();
    OracleVerifierClient::new(&env, &contract_id).initialize(&admin);
}

// ── Oracle registration ───────────────────────────────────────────────────────

#[test]
fn test_add_oracle_and_list() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &50u32);
    let list = client.get_oracles();
    assert_eq!(list.len(), 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_non_admin_cannot_add_oracle() {
    let (env, _admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let impostor = Address::generate(&env);
    let oracle = Address::generate(&env);
    client.add_oracle(&impostor, &oracle, &weather(), &50u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_cannot_reregister_oracle_with_different_weight() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &50u32);
    client.add_oracle(&admin, &oracle, &weather(), &80u32);
}

#[test]
fn test_update_oracle_weight_changes_aggregation() {
    let (env, admin, contract_id) = setup();

    // Wind back the clock for this specific test case's mock data
    env.ledger().set_timestamp(1);

    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle1 = Address::generate(&env);
    let oracle2 = Address::generate(&env);
    let oracle3 = Address::generate(&env);

    client.add_oracle(&admin, &oracle1, &weather(), &60u32);
    client.add_oracle(&admin, &oracle2, &weather(), &20u32);
    client.add_oracle(&admin, &oracle3, &weather(), &20u32);
    client.submit_data(&oracle1, &weather(), &kisumu_key(), &10i128, &100u32, &1748736000u64);
    client.submit_data(&oracle2, &weather(), &kisumu_key(), &20i128, &100u32, &1748736000u64);
    client.submit_data(&oracle3, &weather(), &kisumu_key(), &30i128, &100u32, &1748736000u64);
    assert_eq!(
        client
            .get_aggregated(&weather(), &kisumu_key())
            .median_value,
        10
    );

    client.update_oracle_weight(&admin, &oracle1, &weather(), &10u32);
    assert_eq!(
        client
            .get_aggregated(&weather(), &kisumu_key())
            .median_value,
        20
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_cannot_update_unregistered_oracle_weight() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.update_oracle_weight(&admin, &oracle, &weather(), &80u32);
}

/// Adding an oracle with weight == 0 must be rejected with InvalidWeight (#8).
/// Weight must be 1-100 for all oracle registrations.
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_add_oracle_with_zero_weight_rejected() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    
    // Attempt to add oracle with weight = 0 — must panic with InvalidWeight (#8)
    client.add_oracle(&admin, &oracle, &weather(), &0u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_non_admin_cannot_update_oracle_weight() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    let impostor = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &50u32);
    client.update_oracle_weight(&impostor, &oracle, &weather(), &80u32);
}

#[test]
fn test_remove_oracle_deactivates() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle1 = Address::generate(&env);
    let oracle2 = Address::generate(&env);
    
    client.add_oracle(&admin, &oracle1, &weather(), &80u32);
    client.add_oracle(&admin, &oracle2, &weather(), &80u32);
    
    client.submit_data(&oracle1, &weather(), &kisumu_key(), &10_000_000i128, &90u32, &1748736000u64);
    client.submit_data(&oracle2, &weather(), &kisumu_key(), &50_000_000i128, &90u32, &1748736000u64);
    
    let agg_before = client.get_aggregated(&weather(), &kisumu_key());
    assert_eq!(agg_before.oracle_count, 2);
    assert_eq!(agg_before.median_value, 30_000_000i128); // (10M + 50M) / 2
    
    // Remove oracle1
    client.remove_oracle(&admin, &oracle1, &weather());
    
    // Aggregation should now only include oracle2
    let agg_after = client.get_aggregated(&weather(), &kisumu_key());
    assert_eq!(agg_after.oracle_count, 1);
    assert_eq!(agg_after.median_value, 50_000_000i128);
}

// ── Issue #135: OracleList must not retain soft-deleted addresses ─────────────

#[test]
fn test_remove_oracle_prunes_oracle_list() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle1 = Address::generate(&env);
    let oracle2 = Address::generate(&env);

    client.add_oracle(&admin, &oracle1, &weather(), &80u32);
    client.add_oracle(&admin, &oracle2, &weather(), &80u32);
    assert_eq!(client.get_oracles().len(), 2);

    client.remove_oracle(&admin, &oracle1, &weather());

    // get_oracles() must no longer report the deactivated address.
    let remaining = client.get_oracles();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining.get_unchecked(0), oracle2);
}

// ── Issue #136: active_oracle_count vs oracle_count (submissions) ─────────────

#[test]
fn test_active_oracle_count_reflects_registrations_not_submissions() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle1 = Address::generate(&env);
    let oracle2 = Address::generate(&env);
    let oracle3 = Address::generate(&env);

    // 3 oracles registered, but only 2 submit data for this key.
    client.add_oracle(&admin, &oracle1, &weather(), &80u32);
    client.add_oracle(&admin, &oracle2, &weather(), &80u32);
    client.add_oracle(&admin, &oracle3, &weather(), &80u32);

    client.submit_data(&oracle1, &weather(), &kisumu_key(), &10_000_000i128, &90u32, &1748736000u64);
    client.submit_data(&oracle2, &weather(), &kisumu_key(), &50_000_000i128, &90u32, &1748736000u64);

    let agg = client.get_aggregated(&weather(), &kisumu_key());
    assert_eq!(agg.oracle_count, 2, "oracle_count is submissions for this key");
    assert_eq!(
        agg.active_oracle_count, 3,
        "active_oracle_count is all active registrations for the data_type"
    );

    // Deactivating a registered-but-not-submitted oracle drops active_oracle_count
    // without touching oracle_count (submissions for this key are unaffected).
    client.remove_oracle(&admin, &oracle3, &weather());
    let agg_after = client.get_aggregated(&weather(), &kisumu_key());
    assert_eq!(agg_after.oracle_count, 2);
    assert_eq!(agg_after.active_oracle_count, 2);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_removed_oracle_cannot_submit() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &80u32);
    client.remove_oracle(&admin, &oracle, &weather());
    client.submit_data(&oracle, &weather(), &kisumu_key(), &10_000_000i128, &90u32, &1748736000u64);
}

/// Test that an oracle that was never registered cannot submit data.
/// This verifies the OracleNotRegistered error path is properly enforced.
#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_deregistered_oracle_cannot_submit() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    
    let oracle = Address::generate(&env); // Never registered
    
    // Attempt to submit from an oracle that was never registered
    // Must panic with OracleNotRegistered (#4)
    client.submit_data(&oracle, &weather(), &kisumu_key(), &20_000_000i128, &90u32, &1748736000u64);
}

#[test]
fn test_set_min_confidence() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    client.set_min_confidence(&admin, &50u32);
    // Should be able to set it. We'll verify its effect in another test.
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_set_min_confidence_invalid() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    client.set_min_confidence(&admin, &101u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_set_min_confidence_unauthorized() {
    let (env, _admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let impostor = Address::generate(&env);
    client.set_min_confidence(&impostor, &50u32);
}

// ── Data submission ───────────────────────────────────────────────────────────

#[test]
fn test_authorized_oracle_can_submit() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &90u32);
    client.submit_data(
        &oracle,
        &weather(),
        &kisumu_key(),
        &32_000_000i128,
        &95u32,
        &1748736000u64,
    );
    let point = client.get_data(&weather(), &kisumu_key());
    assert_eq!(point.value, 32_000_000);
    assert_eq!(point.confidence, 95);
    assert_eq!(point.oracle, oracle);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_unregistered_oracle_cannot_submit() {
    let (env, _admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let stranger = Address::generate(&env);
    client.submit_data(
        &stranger,
        &weather(),
        &kisumu_key(),
        &32_000_000i128,
        &95u32,
        &1748736000u64,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_submit_data_zero_confidence() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &90u32);
    client.submit_data(
        &oracle,
        &weather(),
        &kisumu_key(),
        &32_000_000i128,
        &0u32,
        &1748736000u64,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_submit_data_over_100_confidence() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &90u32);
    client.submit_data(
        &oracle,
        &weather(),
        &kisumu_key(),
        &32_000_000i128,
        &101u32,
        &1748736000u64,
    );
}

#[test]
fn test_duplicate_submission_overwrites() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let oracle = Address::generate(&env);

    let topic = soroban_sdk::Symbol::new(&env, "weather");
    let source = soroban_sdk::Symbol::new(&env, "kis2606");
    let timestamp: u64 = 1748822400;

    client.add_oracle(&admin, &oracle, &topic, &100u32);

    env.ledger().set_timestamp(timestamp);

    client.submit_data(
        &oracle,
        &topic,
        &source,
        &28_000_000i128,
        &85u32,
        &timestamp,
    );

    client.submit_data(
        &oracle,
        &topic,
        &source,
        &32_000_000i128,
        &90u32,
        &timestamp,
    );
}

#[test]
fn test_aggregated_confidence_uses_weighted_average() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    env.ledger().set_timestamp(1748822400);

    let oracle1 = Address::generate(&env);
    let oracle2 = Address::generate(&env);
    client.add_oracle(&admin, &oracle1, &weather(), &50u32);
    client.add_oracle(&admin, &oracle2, &weather(), &50u32);

    client.submit_data(
        &oracle1,
        &weather(),
        &kisumu_key(),
        &95_000_000i128,
        &95u32,
        &1748736000u64,
    );
    client.submit_data(
        &oracle2,
        &weather(),
        &kisumu_key(),
        &10_000_000i128,
        &10u32,
        &1748822400u64,
    );

    let agg = client.get_aggregated(&weather(), &kisumu_key());
    assert_eq!(agg.confidence, 52);
}

// ── verify_trigger ────────────────────────────────────────────────────────────

#[test]
fn test_verify_trigger_less_than_met() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &90u32);
    // 32mm observed, threshold 50mm — trigger MET (drought)
    client.submit_data(
        &oracle,
        &weather(),
        &kisumu_key(),
        &32_000_000i128,
        &95u32,
        &1748736000u64,
    );
    let condition = TriggerCondition {
        data_type: weather(),
        key: kisumu_key(),
        threshold: 50_000_000,
        comparison: TriggerComparison::LessThan,
        tolerance: 0i128,
    };
    assert!(client.verify_trigger(&weather(), &kisumu_key(), &condition));
}

#[test]
fn test_verify_trigger_less_than_not_met() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &90u32);
    // 72mm observed — above threshold, no drought
    client.submit_data(
        &oracle,
        &weather(),
        &kisumu_key(),
        &72_000_000i128,
        &95u32,
        &1748736000u64,
    );
    let condition = TriggerCondition {
        data_type: weather(),
        key: kisumu_key(),
        threshold: 50_000_000,
        comparison: TriggerComparison::LessThan,
        tolerance: 0i128,
    };
    assert!(!client.verify_trigger(&weather(), &kisumu_key(), &condition));
}

#[test]
fn test_verify_trigger_greater_than() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    let wind = symbol_short!("wind");
    let key = symbol_short!("sto2606");
    client.add_oracle(&admin, &oracle, &wind, &80u32);
    // 150 km/h wind speed > 120 threshold → trigger MET
    client.submit_data(
        &oracle,
        &wind,
        &key,
        &1_500_000_000i128,
        &90u32,
        &1748736000u64,
    );
    let condition = TriggerCondition {
        data_type: wind.clone(),
        key: key.clone(),
        threshold: 1_200_000_000,
        comparison: TriggerComparison::GreaterThan,
        tolerance: 0i128,
    };
    assert!(client.verify_trigger(&wind, &key, &condition));
}

#[test]
fn test_verify_trigger_skips_low_confidence() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle1 = Address::generate(&env);
    let oracle2 = Address::generate(&env);
    let oracle3 = Address::generate(&env);

    client.add_oracle(&admin, &oracle1, &weather(), &100u32);
    client.add_oracle(&admin, &oracle2, &weather(), &100u32);
    client.add_oracle(&admin, &oracle3, &weather(), &100u32);

    client.set_min_confidence(&admin, &80u32);

    // oracle1: high confidence (90), low value (10mm)
    client.submit_data(
        &oracle1,
        &weather(),
        &kisumu_key(),
        &10_000_000i128,
        &90u32,
        &1748736000u64,
    );
    // oracle2: low confidence (60), high value (90mm)
    client.submit_data(
        &oracle2,
        &weather(),
        &kisumu_key(),
        &90_000_000i128,
        &60u32,
        &1748736000u64,
    );
    // oracle3: high confidence (85), low value (15mm)
    client.submit_data(
        &oracle3,
        &weather(),
        &kisumu_key(),
        &15_000_000i128,
        &85u32,
        &1748736000u64,
    );

    let agg = client.get_aggregated(&weather(), &kisumu_key());
    // Only two valid points left (10mm, 15mm) -> median = (10+15)/2 = 12.5mm
    assert_eq!(agg.median_value, 12_500_000i128);

    let condition = TriggerCondition {
        data_type: weather(),
        key: kisumu_key(),
        threshold: 50_000_000,
        comparison: TriggerComparison::LessThan,
        tolerance: 0i128,
    };
    // If it included oracle2 (90mm), median would be 15mm. Both are < 50mm, so still true.
    // Let's test GreaterThan with threshold 40mm. Median is 12.5 (without oracle2) so false.
    let condition2 = TriggerCondition {
        data_type: weather(),
        key: kisumu_key(),
        threshold: 40_000_000,
        comparison: TriggerComparison::GreaterThan,
        tolerance: 0i128,
    };
    assert!(!client.verify_trigger(&weather(), &kisumu_key(), &condition2));
}

// ── Multi-oracle median ───────────────────────────────────────────────────────

#[test]
fn test_multi_oracle_median() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle1 = Address::generate(&env);
    let oracle2 = Address::generate(&env);
    let oracle3 = Address::generate(&env);
    client.add_oracle(&admin, &oracle1, &weather(), &80u32);
    client.add_oracle(&admin, &oracle2, &weather(), &80u32);
    client.add_oracle(&admin, &oracle3, &weather(), &80u32);
    // Three submissions: 30mm, 34mm, 38mm → median = 34mm
    client.submit_data(
        &oracle1,
        &weather(),
        &kisumu_key(),
        &30_000_000i128,
        &90u32,
        &1748736000u64,
    );
    client.submit_data(
        &oracle2,
        &weather(),
        &kisumu_key(),
        &34_000_000i128,
        &90u32,
        &1748736000u64,
    );
    client.submit_data(
        &oracle3,
        &weather(),
        &kisumu_key(),
        &38_000_000i128,
        &90u32,
        &1748736000u64,
    );
    let agg = client.get_aggregated(&weather(), &kisumu_key());
    assert_eq!(agg.oracle_count, 3);
    assert_eq!(agg.median_value, 34_000_000);
}

#[test]
fn test_median_even_count() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle1 = Address::generate(&env);
    let oracle2 = Address::generate(&env);
    client.add_oracle(&admin, &oracle1, &weather(), &80u32);
    client.add_oracle(&admin, &oracle2, &weather(), &80u32);
    // Two submissions: 30mm, 40mm → median = (30+40)/2 = 35mm
    client.submit_data(
        &oracle1,
        &weather(),
        &kisumu_key(),
        &30_000_000i128,
        &90u32,
        &1748736000u64,
    );
    client.submit_data(
        &oracle2,
        &weather(),
        &kisumu_key(),
        &40_000_000i128,
        &90u32,
        &1748736000u64,
    );
    let agg = client.get_aggregated(&weather(), &kisumu_key());
    assert_eq!(agg.median_value, 35_000_000);
}

// ── Staleness and batch tests ─────────────────────────────────────────────────

#[test]
fn verify_trigger_fresh_passes_with_current_data() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &90u32);
    let now: u64 = 1_748_736_000;
    env.ledger().with_mut(|l| l.timestamp = now);
    client.submit_data(
        &oracle,
        &weather(),
        &kisumu_key(),
        &30_000_000i128,
        &95u32,
        &now,
    );
    let condition = TriggerCondition {
        data_type: weather(),
        key: kisumu_key(),
        threshold: 50_000_000i128,
        comparison: TriggerComparison::LessThan,
        tolerance: 0i128,
    };
    // data is fresh (age = 0s), max_age = 3600s
    let result = client.verify_trigger_fresh(&weather(), &kisumu_key(), &condition, &3600u64);
    assert!(result);
}

#[test]
#[should_panic(expected = "HostError: Error(Contract, #9)")]
fn verify_trigger_fresh_rejects_stale_data() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);

    env.ledger().set_timestamp(100);
    client.add_oracle(&admin, &oracle, &weather(), &90u32);
    // Submit at t=100, ledger timestamp at t=100+86401 (>24h later)
    client.submit_data(
        &oracle,
        &weather(),
        &kisumu_key(),
        &30_000_000i128,
        &95u32,
        &100u64,
    );
    env.ledger().with_mut(|l| l.timestamp = 100 + 86_401);
    let condition = TriggerCondition {
        data_type: weather(),
        key: kisumu_key(),
        threshold: 50_000_000i128,
        comparison: TriggerComparison::LessThan,
        tolerance: 0i128,
    };

    client.verify_trigger_fresh(&weather(), &kisumu_key(), &condition, &86_400u64);
}

#[test]
fn batch_submit_data_stores_all_keys() {
    use soroban_sdk::Vec;

    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &90u32);

    let flight_key = symbol_short!("flightKQ");
    let mut subs: Vec<(Symbol, i128, u32, u64)> = Vec::new(&env);
    subs.push_back((kisumu_key(), 30_000_000i128, 90u32, 1_748_736_000u64));
    subs.push_back((flight_key.clone(), 120_000_000i128, 85u32, 1_748_736_000u64));

    client.batch_submit_data(&oracle, &weather(), &subs);

    let dp1 = client.get_data(&weather(), &kisumu_key());
    let dp2 = client.get_data(&weather(), &flight_key);
    assert_eq!(dp1.value, 30_000_000i128);
    assert_eq!(dp2.value, 120_000_000i128);
}

// ── Oracle count cap and aggregation overflow safety (Issue #38) ──────────────

/// Registering MAX_ORACLES (100) oracles, all at max weight, and submitting the
/// max fixed-point value must aggregate without overflowing i128.
#[test]
fn test_max_oracles_no_overflow() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    // 7-decimal fixed-point near the protocol's upper bound (~10^15).
    let big_value: i128 = 1_000_000_000_000_000i128;

    for _ in 0..MAX_ORACLES {
        let oracle = Address::generate(&env);
        client.add_oracle(&admin, &oracle, &weather(), &100u32);
        client.submit_data(
            &oracle,
            &weather(),
            &kisumu_key(),
            &big_value,
            &100u32,
            &1_748_736_000u64,
        );
    }

    assert_eq!(client.get_oracles().len(), MAX_ORACLES);

    // Aggregation must not overflow; median of identical values is that value.
    let agg = client.get_aggregated(&weather(), &kisumu_key());
    assert_eq!(agg.oracle_count, MAX_ORACLES);
    assert_eq!(agg.median_value, big_value);
}

/// The 101st oracle must be rejected with TooManyOracles (#10).
#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_oracle_cap_rejects_extra() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    for _ in 0..MAX_ORACLES {
        let oracle = Address::generate(&env);
        client.add_oracle(&admin, &oracle, &weather(), &100u32);
    }
    let extra = Address::generate(&env);
    client.add_oracle(&admin, &extra, &weather(), &100u32);
}

#[test]
fn test_get_and_set_max_data_age() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    // Default in our setup is 3_000_000_000, let's reset to default for this test
    client.set_max_data_age(&admin, &604_800);
    assert_eq!(client.get_max_data_age(), 604_800);

    // Admin updates it (1 day)
    client.set_max_data_age(&admin, &86_400);
    assert_eq!(client.get_max_data_age(), 86_400);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_verify_trigger_rejects_stale_data() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    client.set_max_data_age(&admin, &604_800); // Reset back to default max age
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &100u32);

    // T = t0 (1,000,000)
    let t0 = 1_000_000u64;
    env.ledger().with_mut(|l| l.timestamp = t0);
    client.submit_data(
        &oracle,
        &weather(),
        &kisumu_key(),
        &30_000_000i128,
        &90u32,
        &t0,
    );

    let condition = TriggerCondition {
        data_type: weather(),
        key: kisumu_key(),
        threshold: 50_000_000i128,
        comparison: TriggerComparison::LessThan,
        tolerance: 0i128,
    };

    // T = t0 + 1 hour: should pass
    env.ledger().with_mut(|l| l.timestamp = t0 + 3600);
    assert!(client.verify_trigger(&weather(), &kisumu_key(), &condition));

    // T = t0 + 8 days (T0 + 691,200): must panic with NoDataAvailable (#6)
    env.ledger().with_mut(|l| l.timestamp = t0 + 691_200);
    client.verify_trigger(&weather(), &kisumu_key(), &condition);
}

#[test]
fn test_min_oracle_count_enforcement() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle1 = Address::generate(&env);

    client.add_oracle(&admin, &oracle1, &weather(), &100u32);

    // Set MIN_ORACLE_COUNT to 3
    client.set_min_oracle_count(&admin, &3u32);
    assert_eq!(client.get_min_oracle_count(), 3);

    // Submit from 1 oracle
    client.submit_data(
        &oracle1,
        &weather(),
        &kisumu_key(),
        &30_000_000i128,
        &90u32,
        &env.ledger().timestamp(),
    );

    let condition = TriggerCondition {
        data_type: weather(),
        key: kisumu_key(),
        threshold: 50_000_000i128,
        comparison: TriggerComparison::LessThan,
        tolerance: 0i128,
    };

    // Verify trigger should fail because 1 < 3
    let res = client.try_verify_trigger(&weather(), &kisumu_key(), &condition);
    assert!(res.is_err());
}

// ── Issue #162: reject readings when fewer than min_oracle_count submit ──────────

/// Submit from 2 out of 3 required oracles and verify aggregation correctly
/// rejects the result. This is the core safety invariant of the oracle system:
/// a trigger must not fire when insufficient oracle diversity is met.
#[test]
fn test_reject_when_fewer_than_min_oracle_count_submit() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle1 = Address::generate(&env);
    let oracle2 = Address::generate(&env);
    let oracle3 = Address::generate(&env);

    client.add_oracle(&admin, &oracle1, &weather(), &80u32);
    client.add_oracle(&admin, &oracle2, &weather(), &80u32);
    client.add_oracle(&admin, &oracle3, &weather(), &80u32);

    // Require at least 3 oracle submissions
    client.set_min_oracle_count(&admin, &3u32);

    // Only 2 of 3 oracles submit data — below threshold
    client.submit_data(
        &oracle1,
        &weather(),
        &kisumu_key(),
        &30_000_000i128,
        &90u32,
        &env.ledger().timestamp(),
    );
    client.submit_data(
        &oracle2,
        &weather(),
        &kisumu_key(),
        &35_000_000i128,
        &90u32,
        &env.ledger().timestamp(),
    );

    let condition = TriggerCondition {
        data_type: weather(),
        key: kisumu_key(),
        threshold: 50_000_000,
        comparison: TriggerComparison::LessThan,
        tolerance: 0i128,
    };

    // Aggregation must reject because 2 < 3 (min_oracle_count)
    let res = client.try_verify_trigger(&weather(), &kisumu_key(), &condition);
    assert!(res.is_err());

    // Now the 3rd oracle submits — aggregation should succeed
    client.submit_data(
        &oracle3,
        &weather(),
        &kisumu_key(),
        &40_000_000i128,
        &90u32,
        &env.ledger().timestamp(),
    );

    // With 3 submissions, the median should be 35mm (30, 35, 40) < 50mm → true
    let result = client.verify_trigger(&weather(), &kisumu_key(), &condition);
    assert!(result);
}
