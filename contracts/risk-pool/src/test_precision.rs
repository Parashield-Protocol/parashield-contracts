#![allow(clippy::inconsistent_digit_grouping)]
#![cfg(test)]

//! Regression and unit tests for issue #770:
//! RiskPool share price calculation loses precision on small deposits.
//! Covers precise arithmetic, consistent round-half-up rounding policy,
//! and `get_share_price` / `calculate_deposit_shares`.

use soroban_sdk::{testutils::Address as _, token, Address, Env, Symbol};

use crate::{RiskPool, RiskPoolClient, MIN_DEPOSIT, SHARE_PRICE_SCALE};

fn setup() -> (Env, RiskPoolClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let policy_engine = Address::generate(&env);
    let claims_processor = Address::generate(&env);

    let usdc_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let backstop_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let pool_id = env.register(RiskPool, ());
    let pool = RiskPoolClient::new(&env, &pool_id);

    pool.initialize(
        &admin,
        &usdc_id,
        &treasury,
        &backstop_id,
        &Symbol::new(&env, "crop"),
        &policy_engine,
        &claims_processor,
    );
    (env, pool, admin, usdc_id)
}

#[test]
fn test_initial_share_price_equals_scale() {
    let (_, pool, _, _) = setup();
    // In empty pool, share price must equal base SHARE_PRICE_SCALE (1e9)
    assert_eq!(pool.get_share_price(), SHARE_PRICE_SCALE);
}

#[test]
fn test_small_deposit_exact_shares_and_no_value_leak() {
    let (env, pool, _, usdc_id) = setup();
    let lp1 = Address::generate(&env);
    let lp2 = Address::generate(&env);
    let token = token::StellarAssetClient::new(&env, &usdc_id);

    // Initial deposit by LP1: 1,000 USDC (10,000,000,000 stroops)
    token.mint(&lp1, &1_000_0000000i128);
    let lp1_shares = pool.deposit(&lp1, &1_000_0000000i128, &0i128, &false);
    assert!(lp1_shares > 0);

    // Initial share price after first deposit (1,000 burned shares from 1e19 total minted)
    let price = pool.get_share_price();
    assert_eq!(price, SHARE_PRICE_SCALE);

    // LP2 deposits minimum deposit unit (1_000_000 stroops = 0.1 USDC)
    token.mint(&lp2, &MIN_DEPOSIT);
    let expected_shares = pool.calculate_deposit_shares(&MIN_DEPOSIT);
    let lp2_shares = pool.deposit(&lp2, &MIN_DEPOSIT, &0i128, &false);

    // Shares minted MUST match calculate_deposit_shares
    assert_eq!(lp2_shares, expected_shares);
    // 1_000_000 stroops = 1e6 * 1e9 = 1e15 shares. With round-half-up, the 1000 burned shares
    // do not cause truncation loss: LP2 receives exactly 1_000_000_000_000_000 shares!
    assert_eq!(lp2_shares, 1_000_000_000_000_000i128);
}

#[test]
fn test_consecutive_small_deposits_round_consistently() {
    let (env, pool, _, usdc_id) = setup();
    let lp1 = Address::generate(&env);
    let lp2 = Address::generate(&env);
    let lp3 = Address::generate(&env);
    let token = token::StellarAssetClient::new(&env, &usdc_id);

    token.mint(&lp1, &500_0000000i128);
    pool.deposit(&lp1, &500_0000000i128, &0i128, &false);

    // LP2 and LP3 both deposit identical small amounts
    token.mint(&lp2, &MIN_DEPOSIT);
    token.mint(&lp3, &MIN_DEPOSIT);

    let shares2 = pool.deposit(&lp2, &MIN_DEPOSIT, &0i128, &false);
    let shares3 = pool.deposit(&lp3, &MIN_DEPOSIT, &0i128, &false);

    // Consecutive identical small deposits must receive consistent shares (no drift or leak)
    assert_eq!(shares2, shares3);
    assert_eq!(shares2, 1_000_000_000_000_000i128);
}
