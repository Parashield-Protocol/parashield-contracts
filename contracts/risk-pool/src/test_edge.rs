#![allow(clippy::inconsistent_digit_grouping)]
//! Edge case tests for the risk pool — zero shares, full withdrawal, stress.
#![cfg(test)]

extern crate std;

use soroban_sdk::{testutils::Address as _, token, Address, Env, Symbol};

use crate::{RiskPool, RiskPoolClient};

fn setup() -> (Env, RiskPoolClient<'static>, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin            = Address::generate(&env);
    let treasury         = Address::generate(&env);
    let lp1              = Address::generate(&env);
    let policy_engine    = Address::generate(&env);
    let claims_processor = Address::generate(&env);

    let usdc_id     = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let backstop_id = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let pool_id     = env.register(RiskPool, ());
    let pool        = RiskPoolClient::new(&env, &pool_id);

    token::StellarAssetClient::new(&env, &usdc_id).mint(&lp1, &100_000_0000000i128);

    pool.initialize(
        &admin,
        &usdc_id,
        &treasury,
        &backstop_id,
        &Symbol::new(&env, "crop"),
        &policy_engine,
        &claims_processor,
    );

    (env, pool, admin, treasury, lp1)
}

#[test]
fn zero_accumulated_premium_yields_zero_claim() {
    let (_, pool, _, _, lp1) = setup();
    pool.deposit(&lp1, &100_0000000i128, &0i128);
    let yield_amount = pool.claim_yield(&lp1);
    assert_eq!(yield_amount, 0);
}

#[test]
fn full_deposit_withdraw_round_trip_no_premium() {
    let (_, pool, _, _, lp1) = setup();
    let amount = 500_0000000i128;
    let shares = pool.deposit(&lp1, &amount, &0i128);
    let returned = pool.withdraw(&lp1, &shares);
    assert_eq!(returned, amount);
    let stats = pool.get_stats();
    assert_eq!(stats.total_deposited, 0);
    assert_eq!(stats.total_shares, 0);
}

#[test]
fn utilization_100_pct_after_locking_all() {
    let (_, pool, admin, _, lp1) = setup();
    let amount = 200_0000000i128;
    pool.deposit(&lp1, &amount, &0i128);
    pool.lock_for_policy(&admin, &10u128, &amount);
    assert_eq!(pool.get_utilization_rate(), 10_000u32);  // 100% in bps
    assert_eq!(pool.get_available_liquidity(), 0);
}

#[test]
fn multiple_locks_and_releases_track_correctly() {
    let (_, pool, admin, _, lp1) = setup();
    pool.deposit(&lp1, &1000_0000000i128, &0i128);
    pool.lock_for_policy(&admin, &1u128, &300_0000000i128);
    pool.lock_for_policy(&admin, &2u128, &200_0000000i128);
    assert_eq!(pool.get_stats().total_locked, 500_0000000i128);

    pool.release_for_claim(&admin, &1u128);
    assert_eq!(pool.get_stats().total_locked, 200_0000000i128);

    pool.release_for_claim(&admin, &2u128);
    assert_eq!(pool.get_stats().total_locked, 0);
    assert_eq!(pool.get_available_liquidity(), 1000_0000000i128);
}
