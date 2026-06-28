#![allow(clippy::inconsistent_digit_grouping)]
//! Advanced risk-pool tests: multi-LP scenarios, yield accounting, LP count.
#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::Address as _,
    token, Address, Env, Symbol,
};

use crate::{RiskPool, RiskPoolClient};

fn setup_multi() -> (Env, RiskPoolClient<'static>, Address, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin    = Address::generate(&env);
    let treasury = Address::generate(&env);
    let lp1      = Address::generate(&env);
    let lp2      = Address::generate(&env);

    let usdc_id = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let pool_id = env.register(RiskPool, ());
    let pool    = RiskPoolClient::new(&env, &pool_id);

    let mint = |addr: &Address| {
        token::StellarAssetClient::new(&env, &usdc_id).mint(addr, &10_000_0000000i128);
    };
    mint(&lp1);
    mint(&lp2);

    pool.initialize(&admin, &usdc_id, &treasury, &Symbol::new(&env, "defi"));

    (env, pool, usdc_id, admin, treasury, lp1, lp2)
}

#[test]
fn lp_count_tracks_unique_depositors() {
    let (_, pool, _, _, _, lp1, lp2) = setup_multi();
    assert_eq!(pool.get_lp_count(), 0);
    pool.deposit(&lp1, &100_0000000i128);
    assert_eq!(pool.get_lp_count(), 1);
    pool.deposit(&lp2, &200_0000000i128);
    assert_eq!(pool.get_lp_count(), 2);
    // Second deposit from lp1 should not increment count
    pool.deposit(&lp1, &50_0000000i128);
    assert_eq!(pool.get_lp_count(), 2);
}

#[test]
fn available_liquidity_decreases_with_locks() {
    let (_, pool, _, admin, _, lp1, _) = setup_multi();
    pool.deposit(&lp1, &500_0000000i128);
    assert_eq!(pool.get_available_liquidity(), 500_0000000i128);
    pool.lock_for_policy(&admin, &1u128, &200_0000000i128);
    assert_eq!(pool.get_available_liquidity(), 300_0000000i128);
    pool.release_for_claim(&admin, &1u128);
    assert_eq!(pool.get_available_liquidity(), 500_0000000i128);
}

#[test]
fn two_lps_receive_proportional_yield() {
    let (_, pool, _, _, _, lp1, lp2) = setup_multi();
    pool.deposit(&lp1, &300_0000000i128);  // 3/4 of pool
    pool.deposit(&lp2, &100_0000000i128);  // 1/4 of pool

    // premium: 400 USDC → 320 USDC to LP accumulated (80%)
    pool.receive_premium(&lp1, &400_0000000i128);

    let y1 = pool.claim_yield(&lp1);
    let y2 = pool.claim_yield(&lp2);
    // lp1 should get 3x lp2's yield
    assert_eq!(y1, 3 * y2);
    assert_eq!(y1 + y2, 320_0000000i128);
}

#[test]
fn get_stats_reflects_all_operations() {
    let (_, pool, _, admin, _, lp1, lp2) = setup_multi();
    pool.deposit(&lp1, &300_0000000i128);
    pool.deposit(&lp2, &100_0000000i128);
    pool.lock_for_policy(&admin, &5u128, &80_0000000i128);
    pool.receive_premium(&lp1, &100_0000000i128);

    let stats = pool.get_stats();
    assert_eq!(stats.total_deposited,     400_0000000i128);
    assert_eq!(stats.total_locked,        80_0000000i128);
    assert_eq!(stats.accumulated_premium, 80_0000000i128);  // 80% of 100
}
