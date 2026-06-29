#![allow(clippy::inconsistent_digit_grouping)]
#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::Address as _,
    token, Address, Env, Symbol,
};

use crate::{RiskPool, RiskPoolClient};

// ── helpers ───────────────────────────────────────────────────────────────────

fn setup() -> (Env, RiskPoolClient<'static>, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin    = Address::generate(&env);
    let treasury = Address::generate(&env);
    let lp1      = Address::generate(&env);

    let usdc_id = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let pool_id = env.register(RiskPool, ());
    let pool    = RiskPoolClient::new(&env, &pool_id);

    let usdc_admin_client = token::StellarAssetClient::new(&env, &usdc_id);
    usdc_admin_client.mint(&lp1, &1_000_000_000_0000000i128);

    pool.initialize(
        &admin,
        &usdc_id,
        &treasury,
        &Symbol::new(&env, "crop"),
    );

    (env, pool, usdc_id, admin, treasury, lp1)
}


// ── initialization ────────────────────────────────────────────────────────────

#[test]
fn initialize_sets_state() {
    let (_, pool, _, _, _, _) = setup();
    let stats = pool.get_stats();
    assert_eq!(stats.total_deposited, 0);
    assert_eq!(stats.total_shares,    0);
    assert_eq!(stats.total_locked,    0);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn cannot_initialize_twice() {
    let (env, pool, usdc, admin, treasury, _) = setup();
    pool.initialize(&admin, &usdc, &treasury, &Symbol::new(&env, "crop"));
}

// ── deposits ──────────────────────────────────────────────────────────────────

#[test]
fn first_deposit_mints_one_to_one_shares() {
    let (_, pool, _, _, _, lp1) = setup();
    let shares = pool.deposit(&lp1, &500_000_0000000i128);
    assert_eq!(shares, 500_000_0000000i128 * 1_000_000_000);

    let stats = pool.get_stats();
    assert_eq!(stats.total_deposited, 500_000_0000000i128);
    assert_eq!(stats.total_shares,    500_000_0000000i128 * 1_000_000_000);
}

#[test]
fn second_deposit_proportional_shares() {
    let (env, pool, usdc_id, _admin, _, lp1) = setup();
    let lp2 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &usdc_id).mint(&lp2, &500_000_0000000i128);

    pool.deposit(&lp1, &500_000_0000000i128);
    let shares2 = pool.deposit(&lp2, &250_000_0000000i128);
    // shares2 should be half of lp1's shares
    assert_eq!(shares2, 250_000_0000000i128 * 1_000_000_000);
}

#[test]
fn utilization_zero_before_locks() {
    let (_, pool, _, _, _, lp1) = setup();
    pool.deposit(&lp1, &1_000_0000000i128);
    assert_eq!(pool.get_utilization_rate(), 0);
}

// ── withdrawals ───────────────────────────────────────────────────────────────

#[test]
fn withdraw_full_position() {
    let (_, pool, _, _, _, lp1) = setup();
    let amount = 400_0000000i128;
    let shares = pool.deposit(&lp1, &amount);
    let returned = pool.withdraw(&lp1, &shares);
    assert_eq!(returned, amount);

    let stats = pool.get_stats();
    assert_eq!(stats.total_deposited, 0);
}

#[test]
fn withdraw_uses_available_liquidity_after_locks() {
    let (_, pool, _, admin, _, lp1) = setup();
    let amount = 1000_0000000i128;
    let shares = pool.deposit(&lp1, &amount);

    pool.lock_for_policy(&admin, &1u128, &300_0000000i128);
    let returned = pool.withdraw(&lp1, &shares);

    assert_eq!(returned, 700_0000000i128);
}

#[test]
fn withdraw_partial_position_decrements_shares() {
    let (_, pool, _, _, _, lp1) = setup();
    let amount = 1000_0000000i128;
    // deposit 1000 USDC
    let shares = pool.deposit(&lp1, &amount);
    
    // withdraw half the shares
    let half_shares = shares / 2;
    let returned = pool.withdraw(&lp1, &half_shares);
    assert_eq!(returned, amount / 2);

    let stats = pool.get_stats();
    // Verify total shares and total deposited are decremented by half
    assert_eq!(stats.total_deposited, amount / 2);
    assert_eq!(stats.total_shares, half_shares);

    // Verify LP's position is decremented
    let pos = pool.get_position(&lp1).unwrap();
    assert_eq!(pos.shares, half_shares);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn withdraw_without_position_fails() {
    let (env, pool, _, _, _, _) = setup();
    let stranger = Address::generate(&env);
    pool.withdraw(&stranger, &1_0000000i128);
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn withdraw_locked_capital_fails() {
    let (_env, pool, _, admin, _, lp1) = setup();
    let amount = 100_0000000i128;
    let shares = pool.deposit(&lp1, &amount);

    pool.lock_for_policy(&admin, &1u128, &amount);  // lock all capital
    pool.withdraw(&lp1, &shares);  // should fail
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_withdraw_negative_shares_panics() {
    let (_, pool, _, _, _, lp1) = setup();
    pool.withdraw(&lp1, &-100_0000000i128); // negative entries must trigger Error::ZeroAmount (#5)
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_lock_negative_coverage_panics() {
    let (_, pool, _, admin, _, _) = setup();
    pool.lock_for_policy(&admin, &1u128, &-500i128); // negative entries must trigger Error::ZeroAmount (#5)
}

// ── premium routing ────────────────────────────────────────────────────────────

#[test]
fn receive_premium_adds_lp_share() {
    let (_, pool, _, _, _, lp1) = setup();
    pool.deposit(&lp1, &1_000_0000000i128);

    let before = pool.get_stats().accumulated_premium;
    pool.receive_premium(&lp1, &100_0000000i128);
    let after = pool.get_stats().accumulated_premium;
    // 80% goes to LP accumulated
    assert!(after > before);
    assert_eq!(after - before, 80_0000000i128);
}

#[test]
fn claim_yield_proportional_to_shares() {
    let (env, pool, usdc_id, _admin, _, lp1) = setup();
    let lp2 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &usdc_id).mint(&lp2, &1_000_0000000i128);

    pool.deposit(&lp1, &500_0000000i128);
    pool.deposit(&lp2, &500_0000000i128);
    pool.receive_premium(&lp1, &200_0000000i128);  // 160 USDC to LP accumulated

    let yield1 = pool.claim_yield(&lp1);
    let yield2 = pool.claim_yield(&lp2);
    // both hold equal shares, so yield should be equal
    assert_eq!(yield1, yield2);
    assert_eq!(yield1, 80_0000000i128);
}

// ── capital locks ─────────────────────────────────────────────────────────────

#[test]
fn lock_and_release_round_trip() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.deposit(&lp1, &200_0000000i128);

    pool.lock_for_policy(&admin, &42u128, &100_0000000i128);
    assert_eq!(pool.get_utilization_rate(), 5_000u32);  // 50% utilization in bps

    pool.release_for_claim(&admin, &42u128);
    assert_eq!(pool.get_utilization_rate(), 0u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn double_lock_fails() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.deposit(&lp1, &200_0000000i128);
    pool.lock_for_policy(&admin, &1u128, &50_0000000i128);
    pool.lock_for_policy(&admin, &1u128, &50_0000000i128);  // duplicate
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn double_release_fails() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.deposit(&lp1, &200_0000000i128);
    pool.lock_for_policy(&admin, &99u128, &50_0000000i128);
    pool.release_for_claim(&admin, &99u128);
    pool.release_for_claim(&admin, &99u128);  // already released
}

// ── expiry lock release (Issue #11) ─────────────────────────────────────────────

/// Test that release_for_expiry properly releases locked coverage when a policy expires.
#[test]
fn lock_and_release_for_expiry_round_trip() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.deposit(&lp1, &200_0000000i128);

    pool.lock_for_policy(&admin, &42u128, &100_0000000i128);
    assert_eq!(pool.get_utilization_rate(), 5_000u32);  // 50% utilization in bps

    pool.release_for_expiry(&admin, &42u128);
    assert_eq!(pool.get_utilization_rate(), 0u32);
}

/// Test acceptance criteria: lock 100, release 100 → total_locked returns to 0
#[test]
fn lock_100_release_100_returns_total_locked_to_zero() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.deposit(&lp1, &200_0000000i128);

    let lock_amount = 100_0000000i128;
    pool.lock_for_policy(&admin, &1u128, &lock_amount);
    assert_eq!(pool.get_stats().total_locked, lock_amount);

    pool.release_for_expiry(&admin, &1u128);
    assert_eq!(pool.get_stats().total_locked, 0i128);
}

/// Double release_for_expiry should fail with AlreadyReleased
#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn double_release_for_expiry_fails() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.deposit(&lp1, &200_0000000i128);
    pool.lock_for_policy(&admin, &99u128, &50_0000000i128);
    pool.release_for_expiry(&admin, &99u128);
    pool.release_for_expiry(&admin, &99u128);  // already released
}

// ── pause / resume ────────────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn pool_deposit_while_paused_fails() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.pause(&admin);
    pool.deposit(&lp1, &100_0000000i128);
}

#[test]
fn resume_allows_deposit() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.pause(&admin);
    pool.resume(&admin);
    let shares = pool.deposit(&lp1, &100_0000000i128);
    assert!(shares > 0);
}

// ── position queries ──────────────────────────────────────────────────────────

#[test]
fn get_position_returns_correct_state() {
    let (_, pool, _, _, _, lp1) = setup();
    pool.deposit(&lp1, &300_0000000i128);
    let pos = pool.get_position(&lp1).unwrap();
    assert_eq!(pos.deposited, 300_0000000i128);
    assert_eq!(pos.shares,    300_0000000i128 * 1_000_000_000);
    assert_eq!(pos.yield_claimed, 0);
}

#[test]
fn get_position_none_for_non_participant() {
    let (env, pool, _, _, _, _) = setup();
    let nobody = Address::generate(&env);
    assert!(pool.get_position(&nobody).is_none());
}

#[test]
fn test_deposit_precision_loss_prevented() {
    let (env, pool, usdc_id, _admin, _, lp1) = setup();
    let lp2 = Address::generate(&env);
    
    // LP1 deposits 1000 USDC
    pool.deposit(&lp1, &1000_0000000i128);

    // LP2 deposits just 1 stroop (smallest possible unit)
    token::StellarAssetClient::new(&env, &usdc_id).mint(&lp2, &1i128);
    let shares = pool.deposit(&lp2, &1i128);

    // Because of the 1e9 precision multiplier, 1 stroop yields 1e9 shares.
    // This prevents truncation to 0 even if the pool's deposited amount grew 
    // significantly out of proportion to shares in the future.
    assert_eq!(shares, 1_000_000_000i128);
    assert!(shares > 0);
}

#[test]
fn test_get_lp_list_pagination() {
    let (env, pool, usdc_id, _admin, _, lp1) = setup();
    env.budget().reset_unlimited();
    
    let usdc_client = token::StellarAssetClient::new(&env, &usdc_id);
    for _ in 0..200 {
        let lp = Address::generate(&env);
        usdc_client.mint(&lp, &10_000_000i128);
        pool.deposit(&lp, &10_000_000i128);
    }
    
    assert_eq!(pool.get_lp_count(), 200);
    
    // query offset=100, limit=50 (proves pagination works beyond default limit)
    let paginated = pool.get_lp_list(&Some(100), &Some(50));
    assert_eq!(paginated.total_count, 200);
    assert_eq!(paginated.lps.len(), 50);
}
