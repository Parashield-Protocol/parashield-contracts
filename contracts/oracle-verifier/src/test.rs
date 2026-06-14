use super::*;
use soroban_sdk::{symbol_short, testutils::{Address as _, Ledger}, Env, Symbol};

fn setup() -> (Env, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let admin  = Address::generate(&env);
    let oracle = Address::generate(&env);
    let contract_id = env.register(OracleVerifier, ());
    OracleVerifierClient::new(&env, &contract_id).initialize(&admin);
    (env, admin, contract_id)
}

fn weather() -> soroban_sdk::Symbol {
    symbol_short!("weather")
}
fn kisumu_key() -> soroban_sdk::Symbol {
    symbol_short!("kis2606")  // "rainfall:kisumu:2026-06" compressed to 9 chars
}

// ── Initialization ────────────────────────────────────────────────────────────

#[test]
fn test_initialize_sets_admin() {
    let (env, admin, contract_id) = setup();
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
    let client  = OracleVerifierClient::new(&env, &contract_id);
    let oracle  = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &50u32);
    let list = client.get_oracles();
    assert_eq!(list.len(), 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_non_admin_cannot_add_oracle() {
    let (env, _admin, contract_id) = setup();
    let client   = OracleVerifierClient::new(&env, &contract_id);
    let impostor = Address::generate(&env);
    let oracle   = Address::generate(&env);
    client.add_oracle(&impostor, &oracle, &weather(), &50u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_cannot_add_same_oracle_twice() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &50u32);
    client.add_oracle(&admin, &oracle, &weather(), &50u32);
}

#[test]
fn test_remove_oracle_deactivates() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &80u32);
    client.remove_oracle(&admin, &oracle, &weather());
    // After removal, submit_data should panic (unauthorized)
}

// ── Data submission ───────────────────────────────────────────────────────────

#[test]
fn test_authorized_oracle_can_submit() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &90u32);
    client.submit_data(&oracle, &weather(), &kisumu_key(), &32_000_000i128, &95u32, &1748736000u64);
    let point = client.get_data(&weather(), &kisumu_key());
    assert_eq!(point.value, 32_000_000);
    assert_eq!(point.confidence, 95);
    assert_eq!(point.oracle, oracle);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_unregistered_oracle_cannot_submit() {
    let (env, _admin, contract_id) = setup();
    let client  = OracleVerifierClient::new(&env, &contract_id);
    let stranger = Address::generate(&env);
    client.submit_data(&stranger, &weather(), &kisumu_key(), &32_000_000i128, &95u32, &1748736000u64);
}

#[test]
fn test_duplicate_submission_overwrites() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &90u32);
    client.submit_data(&oracle, &weather(), &kisumu_key(), &32_000_000i128, &90u32, &1748736000u64);
    // Second submission — value updates, count stays at 1
    client.submit_data(&oracle, &weather(), &kisumu_key(), &28_000_000i128, &85u32, &1748822400u64);
    let agg = client.get_aggregated(&weather(), &kisumu_key());
    assert_eq!(agg.oracle_count, 1);
    assert_eq!(agg.median_value, 28_000_000);
}

// ── verify_trigger ────────────────────────────────────────────────────────────

#[test]
fn test_verify_trigger_less_than_met() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &90u32);
    // 32mm observed, threshold 50mm — trigger MET (drought)
    client.submit_data(&oracle, &weather(), &kisumu_key(), &32_000_000i128, &95u32, &1748736000u64);
    let condition = TriggerCondition {
        data_type: weather(),
        key: kisumu_key(),
        threshold: 50_000_000,
        comparison: TriggerComparison::LessThan,
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
    client.submit_data(&oracle, &weather(), &kisumu_key(), &72_000_000i128, &95u32, &1748736000u64);
    let condition = TriggerCondition {
        data_type: weather(),
        key: kisumu_key(),
        threshold: 50_000_000,
        comparison: TriggerComparison::LessThan,
    };
    assert!(!client.verify_trigger(&weather(), &kisumu_key(), &condition));
}

#[test]
fn test_verify_trigger_greater_than() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    let wind = symbol_short!("wind");
    let key  = symbol_short!("sto2606");
    client.add_oracle(&admin, &oracle, &wind, &80u32);
    // 150 km/h wind speed > 120 threshold → trigger MET
    client.submit_data(&oracle, &wind, &key, &1_500_000_000i128, &90u32, &1748736000u64);
    let condition = TriggerCondition {
        data_type:  wind.clone(),
        key:        key.clone(),
        threshold:  1_200_000_000,
        comparison: TriggerComparison::GreaterThan,
    };
    assert!(client.verify_trigger(&wind, &key, &condition));
}

// ── Multi-oracle median ───────────────────────────────────────────────────────

#[test]
fn test_multi_oracle_median() {
    let (env, admin, contract_id) = setup();
    let client  = OracleVerifierClient::new(&env, &contract_id);
    let oracle1 = Address::generate(&env);
    let oracle2 = Address::generate(&env);
    let oracle3 = Address::generate(&env);
    client.add_oracle(&admin, &oracle1, &weather(), &80u32);
    client.add_oracle(&admin, &oracle2, &weather(), &80u32);
    client.add_oracle(&admin, &oracle3, &weather(), &80u32);
    // Three submissions: 30mm, 34mm, 38mm → median = 34mm
    client.submit_data(&oracle1, &weather(), &kisumu_key(), &30_000_000i128, &90u32, &1748736000u64);
    client.submit_data(&oracle2, &weather(), &kisumu_key(), &34_000_000i128, &90u32, &1748736000u64);
    client.submit_data(&oracle3, &weather(), &kisumu_key(), &38_000_000i128, &90u32, &1748736000u64);
    let agg = client.get_aggregated(&weather(), &kisumu_key());
    assert_eq!(agg.oracle_count, 3);
    assert_eq!(agg.median_value, 34_000_000);
}

#[test]
fn test_median_even_count() {
    let (env, admin, contract_id) = setup();
    let client  = OracleVerifierClient::new(&env, &contract_id);
    let oracle1 = Address::generate(&env);
    let oracle2 = Address::generate(&env);
    client.add_oracle(&admin, &oracle1, &weather(), &80u32);
    client.add_oracle(&admin, &oracle2, &weather(), &80u32);
    // Two submissions: 30mm, 40mm → median = (30+40)/2 = 35mm
    client.submit_data(&oracle1, &weather(), &kisumu_key(), &30_000_000i128, &90u32, &1748736000u64);
    client.submit_data(&oracle2, &weather(), &kisumu_key(), &40_000_000i128, &90u32, &1748736000u64);
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
    client.submit_data(&oracle, &weather(), &kisumu_key(), &30_000_000i128, &95u32, &now);
    let condition = TriggerCondition {
        data_type:  weather(),
        key:        kisumu_key(),
        threshold:  50_000_000i128,
        comparison: TriggerComparison::LessThan,
    };
    // data is fresh (age = 0s), max_age = 3600s
    let result = client.verify_trigger_fresh(&weather(), &kisumu_key(), &condition, &3600u64);
    assert!(result);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn verify_trigger_fresh_rejects_stale_data() {
    let (env, admin, contract_id) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let oracle = Address::generate(&env);
    client.add_oracle(&admin, &oracle, &weather(), &90u32);
    // Submit at t=100, ledger timestamp at t=100+86401 (>24h later)
    client.submit_data(&oracle, &weather(), &kisumu_key(), &30_000_000i128, &95u32, &100u64);
    env.ledger().with_mut(|l| l.timestamp = 100 + 86_401);
    let condition = TriggerCondition {
        data_type:  weather(),
        key:        kisumu_key(),
        threshold:  50_000_000i128,
        comparison: TriggerComparison::LessThan,
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
