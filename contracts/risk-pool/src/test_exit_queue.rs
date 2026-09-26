//! Auto-processing of expired exit requests (issue #511).
#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Env, Symbol,
};

use crate::{ExitStatus, RiskPool, RiskPoolClient};

const DELAY: u64 = 3_600;
const AMOUNT: i128 = 100_0000000;

struct World {
    env: Env,
    pool: RiskPoolClient<'static>,
    admin: Address,
    usdc: Address,
}

fn setup() -> World {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let usdc = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let backstop = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let pool_id = env.register(RiskPool, ());
    let pool = RiskPoolClient::new(&env, &pool_id);
    pool.initialize(
        &admin,
        &usdc,
        &Address::generate(&env),
        &backstop,
        &Symbol::new(&env, "crop"),
        &Address::generate(&env),
        &Address::generate(&env),
    );
    pool.set_exit_delay(&admin, &DELAY);
    World { env, pool, admin, usdc }
}

fn lp(w: &World) -> Address {
    let lp = Address::generate(&w.env);
    token::StellarAssetClient::new(&w.env, &w.usdc).mint(&lp, &(10 * AMOUNT));
    lp
}

fn balance(w: &World, who: &Address) -> i128 {
    token::Client::new(&w.env, &w.usdc).balance(who)
}

fn advance(w: &World, secs: u64) {
    let now = w.env.ledger().timestamp();
    w.env.ledger().set_timestamp(now + secs);
}

/// Deposit `AMOUNT` and queue an exit of all shares.
fn deposit_and_queue(w: &World) -> (Address, i128) {
    let who = lp(w);
    let shares = w.pool.deposit(&who, &AMOUNT, &0, &false);
    w.pool.request_exit(&who, &shares);
    (who, shares)
}

#[test]
fn deposit_by_another_lp_settles_expired_exit() {
    let w = setup();
    let (leaver, _) = deposit_and_queue(&w);
    let stayer = lp(&w);
    w.pool.deposit(&stayer, &AMOUNT, &0, &false);
    let before = balance(&w, &leaver);

    advance(&w, DELAY + 1);
    let newcomer = lp(&w);
    w.pool.deposit(&newcomer, &AMOUNT, &0, &false);

    assert_eq!(balance(&w, &leaver) - before, AMOUNT);
    assert_eq!(w.pool.get_exit_info(&leaver).status, ExitStatus::None);
    assert_eq!(w.pool.get_position(&leaver).unwrap().shares, 0);
}

#[test]
fn unexpired_exit_is_left_queued() {
    let w = setup();
    let (leaver, shares) = deposit_and_queue(&w);
    advance(&w, DELAY - 1);
    assert_eq!(w.pool.process_expired_exits(&5), 0);

    let info = w.pool.get_exit_info(&leaver);
    assert_eq!(info.status, ExitStatus::Pending);
    assert_eq!(info.shares, shares);
}

#[test]
fn process_expired_exits_is_permissionless_and_pays_provider() {
    let w = setup();
    let (leaver, _) = deposit_and_queue(&w);
    let before = balance(&w, &leaver);
    advance(&w, DELAY);

    assert_eq!(w.pool.process_expired_exits(&5), 1);
    assert_eq!(balance(&w, &leaver) - before, AMOUNT);
    // Nothing left to do on a second pass.
    assert_eq!(w.pool.process_expired_exits(&5), 0);
}

#[test]
fn zero_scan_budget_does_nothing() {
    let w = setup();
    deposit_and_queue(&w);
    advance(&w, DELAY);
    assert_eq!(w.pool.process_expired_exits(&0), 0);
}

#[test]
fn unaffordable_exit_stays_queued_and_does_not_revert_the_caller() {
    let w = setup();
    let (leaver, _) = deposit_and_queue(&w);
    // Lock every unit so nothing is withdrawable.
    w.pool.lock_for_policy(&w.admin, &1u128, &AMOUNT);
    advance(&w, DELAY);

    let newcomer = lp(&w);
    // The deposit itself must still succeed.
    w.pool.deposit(&newcomer, &AMOUNT, &0, &false);
    assert_eq!(w.pool.get_exit_info(&leaver).status, ExitStatus::Claimable);
}

#[test]
fn callers_own_expired_exit_is_not_consumed_by_their_withdraw() {
    let w = setup();
    let (who, shares) = deposit_and_queue(&w);
    advance(&w, DELAY);

    // A direct withdraw of every share still works even though the same
    // shares have an expired request outstanding.
    let returned = w.pool.withdraw(&who, &shares);
    assert_eq!(returned, AMOUNT);
}

#[test]
fn paused_pool_does_not_auto_process() {
    let w = setup();
    let (leaver, _) = deposit_and_queue(&w);
    advance(&w, DELAY);
    w.pool.pause(&w.admin);

    assert_eq!(w.pool.process_expired_exits(&5), 0);
    assert_eq!(w.pool.get_exit_info(&leaver).status, ExitStatus::Claimable);
}

#[test]
fn stale_request_whose_shares_are_gone_is_dropped() {
    let w = setup();
    let (leaver, shares) = deposit_and_queue(&w);
    // Withdraw directly while the request is outstanding.
    w.pool.withdraw(&leaver, &shares);
    advance(&w, DELAY);

    assert_eq!(w.pool.process_expired_exits(&5), 0);
    assert_eq!(w.pool.get_exit_info(&leaver).status, ExitStatus::None);
    assert_eq!(w.pool.get_queued_exit_shares(), 0);
}

#[test]
fn cursor_rotates_so_every_provider_is_eventually_swept() {
    let w = setup();
    let mut leavers = std::vec::Vec::new();
    for _ in 0..8 {
        leavers.push(deposit_and_queue(&w).0);
    }
    advance(&w, DELAY);

    // One pass scans at most 5 slots; the next covers the rest.
    assert_eq!(w.pool.process_expired_exits(&100), 5);
    assert_eq!(w.pool.process_expired_exits(&100), 3);
    for who in &leavers {
        assert_eq!(w.pool.get_exit_info(who).status, ExitStatus::None);
    }
}

#[test]
fn request_exit_by_another_lp_also_triggers_the_sweep() {
    let w = setup();
    let (leaver, _) = deposit_and_queue(&w);
    let other = lp(&w);
    let shares = w.pool.deposit(&other, &AMOUNT, &0, &false);
    advance(&w, DELAY);

    w.pool.request_exit(&other, &shares);
    assert_eq!(w.pool.get_exit_info(&leaver).status, ExitStatus::None);
}
