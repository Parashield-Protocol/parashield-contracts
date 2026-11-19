use parashield_policy_engine::{
    CreateProductParams, Error, PolicyEngine, PolicyEngineClient, TriggerComparison, TriggerType,
};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Env,
};

fn setup() -> (Env, Address, Address, PolicyEngineClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let usdc = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let id = env.register(PolicyEngine, ());
    let client = PolicyEngineClient::new(&env, &id);
    client.initialize(&admin, &usdc, &Address::generate(&env));
    (env, admin, usdc, client)
}

#[test]
fn buy_policy_scheduled_time_overflow_returns_overflow_error() {
    let (env, admin, usdc, client) = setup();
    let pid = client.create_product(
        &admin,
        &CreateProductParams {
            name: symbol_short!("p_crop"),
            category: symbol_short!("crop"),
            oracle_key: symbol_short!("kis2606"),
            expected_token: usdc.clone(),
            trigger_type: TriggerType::Threshold,
            oracle_data_type: symbol_short!("weather"),
            trigger_threshold: 50_000_000,
            trigger_comparison: TriggerComparison::LessThan,
            coverage_min: 100_000_000,
            coverage_max: 10_000_000_000,
            premium_rate_bps: 500,
            max_duration_days: 365,
        },
    );

    let buyer = Address::generate(&env);
    StellarAssetClient::new(&env, &usdc).mint(&buyer, &1_000_000_000i128);

    env.ledger().with_mut(|l| l.timestamp = 1_000);

    // start_time = u64::MAX causes start_time.checked_add(duration_secs) to overflow.
    // Must return Error::Overflow (#21), not Error::CoverageOutOfRange (#8).
    let res = client.try_buy_policy_scheduled(
        &buyer,
        &pid,
        &1_000_000_000i128,
        &30u32,
        &symbol_short!("kis2606"),
        &u64::MAX,
        &usdc,
    );

    assert_eq!(
        res.unwrap_err().unwrap(),
        soroban_sdk::Error::from_contract_error(Error::Overflow as u32)
    );
}
