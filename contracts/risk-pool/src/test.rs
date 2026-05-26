#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Env, Symbol,
};

use crate::{Error, RiskPool, RiskPoolClient};

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

fn ledger_ts(env: &Env) -> u64 {
    env.ledger().timestamp()
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
    assert_eq!(shares, 500_000_0000000i128);

    let stats = pool.get_stats();
    assert_eq!(stats.total_deposited, 500_000_0000000i128);
    assert_eq!(stats.total_shares,    500_000_0000000i128);
}

#[test]
fn second_deposit_proportional_shares() {
    let (env, pool, usdc_id, admin, _, lp1) = setup();
    let lp2 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &usdc_id).mint(&lp2, &500_000_0000000i128);

    pool.deposit(&lp1, &500_000_0000000i128);
    let shares2 = pool.deposit(&lp2, &250_000_0000000i128);
    // shares2 should be half of lp1's shares
    assert_eq!(shares2, 250_000_0000000i128);
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
#[should_panic(expected = "Error(Contract, #7)")]
fn withdraw_without_position_fails() {
    let (env, pool, _, _, _, _) = setup();
    let stranger = Address::generate(&env);
    pool.withdraw(&stranger, &1_0000000i128);
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn withdraw_locked_capital_fails() {
    let (env, pool, _, admin, _, lp1) = setup();
    let amount = 100_0000000i128;
    let shares = pool.deposit(&lp1, &amount);

    pool.lock_for_policy(&admin, &1u128, &amount);  // lock all capital
    pool.withdraw(&lp1, &shares);  // should fail
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
    let (env, pool, usdc_id, admin, _, lp1) = setup();
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

// ── pause / resume ────────────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn deposit_while_paused_fails() {
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
    assert_eq!(pos.shares,    300_0000000i128);
    assert_eq!(pos.yield_claimed, 0);
}

#[test]
fn get_position_none_for_non_participant() {
    let (env, pool, _, _, _, _) = setup();
    let nobody = Address::generate(&env);
    assert!(pool.get_position(&nobody).is_none());
}
