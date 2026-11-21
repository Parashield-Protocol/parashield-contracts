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

// ── Lowered MAX_DATA_VALUE & per-data-type range validation tests (#839) ─────

/// 1. A value exactly at the new ceiling (MAX_DATA_VALUE = 10^21) is accepted,
/// and one above (MAX_DATA_VALUE + 1) is rejected (also for negative values).
#[test]
fn test_global_max_data_value_ceiling_boundaries() {
    let (env, admin, oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let custom = symbol_short!("custom");
    client.add_oracle(&admin, &oracle, &custom, &100u32);

    // Value exactly at MAX_DATA_VALUE is accepted
    client.submit_data(&oracle, &custom, &key(), &MAX_DATA_VALUE, &100u32, &OBSERVED_AT);
    assert_eq!(client.get_data(&custom, &key()).value, MAX_DATA_VALUE);

    // Negative value exactly at -MAX_DATA_VALUE is accepted
    let k_neg = symbol_short!("custom_n");
    client.submit_data(&oracle, &custom, &k_neg, &-MAX_DATA_VALUE, &100u32, &OBSERVED_AT);
    assert_eq!(client.get_data(&custom, &k_neg).value, -MAX_DATA_VALUE);

    // Value above MAX_DATA_VALUE is rejected
    let k_over = symbol_short!("cust_ov");
    assert_eq!(
        contract_error(client.try_submit_data(&oracle, &custom, &k_over, &(MAX_DATA_VALUE + 1), &100u32, &OBSERVED_AT)),
        Some(invalid_input()),
        "value > MAX_DATA_VALUE must be rejected"
    );

    // Negative value below -MAX_DATA_VALUE is rejected
    let k_under = symbol_short!("cust_un");
    assert_eq!(
        contract_error(client.try_submit_data(&oracle, &custom, &k_under, &(-MAX_DATA_VALUE - 1), &100u32, &OBSERVED_AT)),
        Some(invalid_input()),
        "value < -MAX_DATA_VALUE must be rejected"
    );
}

/// 2. Realistic value for a supported data type (e.g., rainfall = 50 mm = 500_000_000) is accepted.
#[test]
fn test_realistic_data_type_value_accepted() {
    let (env, admin, oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let weather = symbol_short!("weather");
    client.add_oracle(&admin, &oracle, &weather, &100u32);

    let rainfall_50mm = 500_000_000i128; // 50 mm in 7-decimal fixed point
    client.submit_data(&oracle, &weather, &key(), &rainfall_50mm, &100u32, &OBSERVED_AT);
    assert_eq!(client.get_data(&weather, &key()).value, rainfall_50mm);
}

/// 3. A value above a data type's configured/default range but below the global ceiling
/// is rejected (e.g. 10^15 for weather, where DEFAULT_WEATHER_MAX is 10^10).
#[test]
fn test_value_above_type_range_below_global_ceiling_rejected() {
    let (env, admin, oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let weather = symbol_short!("weather");
    client.add_oracle(&admin, &oracle, &weather, &100u32);

    // DEFAULT_WEATHER_MAX is 10_000_000_000 (10,000 mm rainfall = 10^10).
    // A value of 10^15 is well below MAX_DATA_VALUE (10^21), but exceeds weather's range limit.
    let excessive_rainfall = 1_000_000_000_000_000i128;
    assert_eq!(
        contract_error(client.try_submit_data(&oracle, &weather, &key(), &excessive_rainfall, &100u32, &OBSERVED_AT)),
        Some(invalid_input()),
        "rainfall above weather max must be rejected even when below MAX_DATA_VALUE"
    );

    // Also verify in batch_submit_data (issue #839 primary enforcement everywhere values enter)
    let mut batch = soroban_sdk::Vec::new(&env);
    batch.push_back((key(), excessive_rainfall, 100u32, OBSERVED_AT));
    assert_eq!(
        contract_error(client.try_batch_submit_data(&oracle, &weather, &batch)),
        Some(invalid_input()),
        "batch_submit_data must also reject out-of-range rainfall"
    );
}

/// 4. Exact boundaries of a configured range:
/// Pin the inclusive behavior: value == max is accepted; value == max + 1 is rejected.
#[test]
fn test_exact_boundaries_of_configured_range() {
    let (env, admin, oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let custom = symbol_short!("custom");
    client.add_oracle(&admin, &oracle, &custom, &100u32);

    let min_bound = 100_0000000i128;
    let max_bound = 500_0000000i128;
    client.set_data_type_range(&admin, &custom, &min_bound, &max_bound);

    // Exact max boundary is accepted
    let k_max = symbol_short!("k_max");
    client.submit_data(&oracle, &custom, &k_max, &max_bound, &100u32, &OBSERVED_AT);
    assert_eq!(client.get_data(&custom, &k_max).value, max_bound);

    // 1 above max boundary is rejected
    let k_over = symbol_short!("k_over");
    assert_eq!(
        contract_error(client.try_submit_data(&oracle, &custom, &k_over, &(max_bound + 1), &100u32, &OBSERVED_AT)),
        Some(invalid_input())
    );

    // Negative exceeding abs(max) is rejected
    let k_neg_over = symbol_short!("k_neg_ov");
    assert_eq!(
        contract_error(client.try_submit_data(&oracle, &custom, &k_neg_over, &(-max_bound - 1), &100u32, &OBSERVED_AT)),
        Some(invalid_input())
    );
}

/// 5. A data type with no configured range falls back to the global ceiling MAX_DATA_VALUE.
#[test]
fn test_unconfigured_data_type_falls_back_to_global_ceiling() {
    let (env, admin, oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let unknown_type = symbol_short!("unknown");
    client.add_oracle(&admin, &oracle, &unknown_type, &100u32);

    // No range is configured for 'unknown', so it accepts up to MAX_DATA_VALUE
    client.submit_data(&oracle, &unknown_type, &key(), &MAX_DATA_VALUE, &100u32, &OBSERVED_AT);
    assert_eq!(client.get_data(&unknown_type, &key()).value, MAX_DATA_VALUE);

    // But rejects MAX_DATA_VALUE + 1
    let k2 = symbol_short!("k2");
    assert_eq!(
        contract_error(client.try_submit_data(&oracle, &unknown_type, &k2, &(MAX_DATA_VALUE + 1), &100u32, &OBSERVED_AT)),
        Some(invalid_input())
    );
}

/// 6. Changing a range updates validation behavior immediately.
#[test]
fn test_changing_range_updates_validation() {
    let (env, admin, oracle, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let custom = symbol_short!("custom");
    client.add_oracle(&admin, &oracle, &custom, &100u32);

    // Initially range is [0, 100]
    client.set_data_type_range(&admin, &custom, &0i128, &100i128);

    let k1 = symbol_short!("k1");
    // 150 is rejected under [0, 100]
    assert_eq!(
        contract_error(client.try_submit_data(&oracle, &custom, &k1, &150i128, &100u32, &OBSERVED_AT)),
        Some(invalid_input())
    );

    // Admin expands range to [0, 200]
    client.set_data_type_range(&admin, &custom, &0i128, &200i128);

    // 150 is now accepted
    client.submit_data(&oracle, &custom, &k1, &150i128, &100u32, &OBSERVED_AT);
    assert_eq!(client.get_data(&custom, &k1).value, 150i128);
}

/// 7. Overflow headroom check:
/// Verify that MAX_DATA_VALUE * MAX_WEIGHT * MAX_DATA_POINTS fits in i128 with extensive headroom.
#[test]
fn test_overflow_headroom_invariant() {
    let max_weight = 100_000i128;
    let max_submissions = 100i128;
    let worst_case_sum = MAX_DATA_VALUE
        .checked_mul(max_weight)
        .and_then(|v| v.checked_mul(max_submissions));

    assert!(worst_case_sum.is_some(), "Worst-case aggregation must not overflow i128");
    let sum = worst_case_sum.unwrap();
    assert!(sum < i128::MAX);
    // 10^21 * 10^5 * 10^2 = 10^28
    assert_eq!(sum, 10_000_000_000_000_000_000_000_000_000i128);
}
