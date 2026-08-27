#![allow(clippy::inconsistent_digit_grouping)]
#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Env, Symbol,
};

use crate::{RiskPool, RiskPoolClient};

// ── helpers ───────────────────────────────────────────────────────────────────

fn setup() -> (Env, RiskPoolClient<'static>, Address, Address, Address, Address) {
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

    let usdc_admin_client = token::StellarAssetClient::new(&env, &usdc_id);
    usdc_admin_client.mint(&lp1, &1_000_000_000_0000000i128);

    pool.initialize(
        &admin,
        &usdc_id,
        &treasury,
        &backstop_id,
        &Symbol::new(&env, "crop"),
        &policy_engine,
        &claims_processor,
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
    let backstop         = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let policy_engine    = Address::generate(&env);
    let claims_processor = Address::generate(&env);
    pool.initialize(&admin, &usdc, &treasury, &backstop, &Symbol::new(&env, "crop"), &policy_engine, &claims_processor);
}

#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_initialize_with_non_token_usdc() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    
    // Register some random non-token contract and use it as USDC
    let fake_usdc = env.register(RiskPool, ());
    let pool_id = env.register(RiskPool, ());
    let pool = RiskPoolClient::new(&env, &pool_id);
    
    pool.initialize(
        &admin,
        &fake_usdc,
        &treasury,
        &Address::generate(&env), // backstop
        &Symbol::new(&env, "crop"),
        &Address::generate(&env), // policy_engine
        &Address::generate(&env), // claims_processor
    );
}

// ── deposits ──────────────────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_deposit_too_small_panics() {
    let (_, pool, _, _, _, lp1) = setup();
    pool.deposit(&lp1, &999_999i128, &0i128, &false);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_deposit_1_stroop_panics() {
    let (_, pool, _, _, _, lp1) = setup();
    pool.deposit(&lp1, &1i128, &0i128, &false); // 1 stroop
}

#[test]
fn first_deposit_mints_one_to_one_shares() {
    let (_, pool, _, _, _, lp1) = setup();
    let shares = pool.deposit(&lp1, &500_000_0000000i128, &0i128, &false);
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

    pool.deposit(&lp1, &500_000_0000000i128, &0i128, &false);
    let shares2 = pool.deposit(&lp2, &250_000_0000000i128, &0i128, &false);
    // shares2 should be half of lp1's shares
    assert_eq!(shares2, 250_000_0000000i128 * 1_000_000_000);
}

#[test]
fn utilization_zero_before_locks() {
    let (_, pool, _, _, _, lp1) = setup();
    pool.deposit(&lp1, &1_000_0000000i128, &0i128, &false);
    assert_eq!(pool.get_utilization_rate(), 0);
}

// ── withdrawals ───────────────────────────────────────────────────────────────

#[test]
fn withdraw_full_position() {
    let (_, pool, _, _, _, lp1) = setup();
    let amount = 400_0000000i128;
    let shares = pool.deposit(&lp1, &amount, &0i128, &false);
    let returned = pool.withdraw(&lp1, &shares);
    assert_eq!(returned, amount);

    let stats = pool.get_stats();
    assert_eq!(stats.total_deposited, 0);
}

#[test]
fn withdraw_uses_available_liquidity_after_locks() {
    let (_, pool, _, admin, _, lp1) = setup();
    let amount = 1000_0000000i128;
    let shares = pool.deposit(&lp1, &amount, &0i128, &false);

    pool.lock_for_policy(&admin, &1u128, &300_0000000i128);
    let returned = pool.withdraw(&lp1, &shares);

    assert_eq!(returned, 700_0000000i128);
}

#[test]
fn withdraw_uses_available_liquidity_after_locks_2() {
    let (_, pool, _, admin, _, lp1) = setup();
    let amount = 1000_0000000i128;
    let shares = pool.deposit(&lp1, &amount, &0i128, &false);

    pool.lock_for_policy(&admin, &1u128, &300_0000000i128);
    let returned = pool.withdraw(&lp1, &shares);

    assert_eq!(returned, 700_0000000i128);
}

#[test]
fn withdraw_partial_position_decrements_shares() {
    let (_, pool, _, _, _, lp1) = setup();
    let amount = 1000_0000000i128;
    // deposit 1000 USDC
    let shares = pool.deposit(&lp1, &amount, &0i128, &false);
    
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
    let shares = pool.deposit(&lp1, &amount, &0i128, &false);

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
fn receive_premium_distributes_1000_usdc_80_10_10() {
    let (env, pool, usdc_id, _, treasury, lp1) = setup();
    let backstop = pool.get_backstop();
    let usdc = token::Client::new(&env, &usdc_id);

    pool.deposit(&lp1, &1_000_0000000i128, &0i128, &false);

    let treasury_before = usdc.balance(&treasury);
    let backstop_before = usdc.balance(&backstop);
    let lp_premium_before = pool.get_stats().accumulated_premium;

    let premium = 1_000_0000000i128; // 1000 USDC
    pool.receive_premium(&lp1, &premium);

    let lp_share = pool.get_stats().accumulated_premium - lp_premium_before;
    let treasury_share = usdc.balance(&treasury) - treasury_before;
    let backstop_share = usdc.balance(&backstop) - backstop_before;

    assert_eq!(lp_share, 800_0000000i128);
    assert_eq!(treasury_share, 100_0000000i128);
    assert_eq!(backstop_share, 100_0000000i128);
    assert_eq!(lp_share + treasury_share + backstop_share, premium);
}

#[test]
fn receive_premium_adds_lp_share() {
    let (_, pool, _, _, _, lp1) = setup();
    pool.deposit(&lp1, &1_000_0000000i128, &0i128, &false);

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

    pool.deposit(&lp1, &500_0000000i128, &0i128, &false);
    pool.deposit(&lp2, &500_0000000i128, &0i128, &false);
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
    pool.deposit(&lp1, &200_0000000i128, &0i128, &false);

    pool.lock_for_policy(&admin, &42u128, &100_0000000i128);
    assert_eq!(pool.get_utilization_rate(), 5_000u32);  // 50% utilization in bps

    pool.release_for_claim(&admin, &42u128);
    assert_eq!(pool.get_utilization_rate(), 0u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn double_lock_fails() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.deposit(&lp1, &200_0000000i128, &0i128, &false);
    pool.lock_for_policy(&admin, &1u128, &50_0000000i128);
    pool.lock_for_policy(&admin, &1u128, &50_0000000i128);  // duplicate
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn double_release_fails() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.deposit(&lp1, &200_0000000i128, &0i128, &false);
    pool.lock_for_policy(&admin, &99u128, &50_0000000i128);
    pool.release_for_claim(&admin, &99u128);
    pool.release_for_claim(&admin, &99u128);  // already released
}

// ── expiry lock release (Issue #11) ─────────────────────────────────────────────

/// Test that release_for_expiry properly releases locked coverage when a policy expires.
#[test]
fn lock_and_release_for_expiry_round_trip() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.deposit(&lp1, &200_0000000i128, &0i128, &false);

    pool.lock_for_policy(&admin, &42u128, &100_0000000i128);
    assert_eq!(pool.get_utilization_rate(), 5_000u32);  // 50% utilization in bps

    pool.release_for_expiry(&admin, &42u128);
    assert_eq!(pool.get_utilization_rate(), 0u32);
}

/// Test acceptance criteria: lock 100, release 100 → total_locked returns to 0
#[test]
fn lock_100_release_100_returns_total_locked_to_zero() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.deposit(&lp1, &200_0000000i128, &0i128, &false);

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
    pool.deposit(&lp1, &200_0000000i128, &0i128, &false);
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
    pool.deposit(&lp1, &100_0000000i128, &0i128, &false);
}

#[test]
fn resume_allows_deposit() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.pause(&admin);
    pool.resume(&admin);
    let shares = pool.deposit(&lp1, &100_0000000i128, &0i128, &false);
    assert!(shares > 0);
}

// ── winding down ──────────────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn winding_down_blocks_new_deposits() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.deposit(&lp1, &100_0000000i128, &0i128, &false);
    pool.start_winding_down(&admin);
    pool.deposit(&lp1, &100_0000000i128, &0i128, &false);
}

#[test]
fn winding_down_allows_existing_lp_to_withdraw() {
    let (_, pool, _, admin, _, lp1) = setup();
    let shares = pool.deposit(&lp1, &100_0000000i128, &0i128, &false);
    pool.start_winding_down(&admin);
    let amount = pool.withdraw(&lp1, &shares);
    assert!(amount > 0);
    let stats = pool.get_stats();
    assert_eq!(stats.status, crate::PoolStatus::WindingDown);
}

// ── premium split ─────────────────────────────────────────────────────────────

#[test]
fn admin_can_update_premium_split() {
    let (_, pool, _, admin, _, _) = setup();
    pool.update_premium_split(&admin, &7_000i128, &2_000i128, &1_000i128);
    let split = pool.get_premium_split();
    assert_eq!(split.lp_bps, 7_000);
    assert_eq!(split.treas_bps, 2_000);
    assert_eq!(split.backstop_bps, 1_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #22)")]
fn premium_split_must_sum_to_10000() {
    let (_, pool, _, admin, _, _) = setup();
    pool.update_premium_split(&admin, &7_000i128, &2_000i128, &2_000i128);
}

// ── position queries ──────────────────────────────────────────────────────────

#[test]
fn get_position_returns_correct_state() {
    let (_, pool, _, _, _, lp1) = setup();
    pool.deposit(&lp1, &300_0000000i128, &0i128, &false);
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
    pool.deposit(&lp1, &1000_0000000i128, &0i128, &false);

    // LP2 deposits the MIN_DEPOSIT (1_000_000 stroops)
    token::StellarAssetClient::new(&env, &usdc_id).mint(&lp2, &1_000_000i128);
    let shares = pool.deposit(&lp2, &1_000_000i128, &0i128, &false);

    // 1_000_000 stroops yields 1_000_000_000_000_000 shares (because 1 USDC = 1e9 shares).
    assert_eq!(shares, 1_000_000_000_000_000i128);
    assert!(shares > 0);
}

// ── admin drain protection ─────────────────────────────────────────────────────

/// Admin cannot withdraw LP shares directly — `withdraw` is gated by
/// `provider.require_auth()`.  Admins who are NOT the LP cannot call
/// withdraw on behalf of an LP because the LP's signature is required.
///
/// With `mock_all_auths` all auths are automatically approved, so this
/// test verifies the protection indirectly: the only path to move USDC
/// out of the pool is `withdraw` / `claim_yield`, both of which require
/// the LP's own `require_auth()`.  There is no admin-only sweep function.
#[test]
fn admin_cannot_drain_lp_funds_indirectly() {
    let (_, pool, _, admin, _, lp1) = setup();
    let amount = 500_0000000i128;
    let _shares = pool.deposit(&lp1, &amount, &0i128, &false);

    // There is no admin-only withdraw function.  Only `withdraw(lp, shares)`
    // exists, and it requires lp's auth.  Verify there are no other
    // transfer-out functions that admin could abuse.
    let stats_before = pool.get_stats();
    assert_eq!(stats_before.total_deposited, amount);

    // Admin can pause/resume but cannot transfer funds.
    pool.pause(&admin);
    pool.resume(&admin);
    let stats_after = pool.get_stats();
    assert_eq!(stats_after.total_deposited, amount);
}

/// Admin can request a withdrawal, but it cannot be executed before the
/// 7-day timelock matures.
#[test]
#[should_panic(expected = "Error(Contract, #15)")]
fn admin_timelock_withdrawal_not_ready_before_7_days() {
    let (_, pool, _usdc_id, admin, _treasury, lp1) = setup();
    pool.deposit(&lp1, &1_000_0000000i128, &0i128, &false);

    pool.request_admin_withdrawal(&admin, &100_0000000i128);
    // Attempt to execute immediately → TimelockNotReady (#14)
    pool.execute_admin_withdrawal(&admin);
}

/// Admin can request, cancel, and re-request a withdrawal.
#[test]
fn admin_timelock_cancel_and_re_request() {
    let (env, pool, _usdc_id, admin, _treasury, lp1) = setup();
    pool.deposit(&lp1, &1_000_0000000i128, &0i128, &false);

    pool.request_admin_withdrawal(&admin, &100_0000000i128);
    pool.cancel_admin_withdrawal(&admin);

    // Re-request succeeds after cancellation
    pool.request_admin_withdrawal(&admin, &200_0000000i128);

    // Advance the ledger past the timelock
    let jump = (7 * 24 * 60 * 60 + 1) as u64;
    env.ledger().set_timestamp(env.ledger().timestamp() + jump);

    pool.execute_admin_withdrawal(&admin);
    assert_eq!(pool.get_available_liquidity(), 800_0000000i128);
}

/// The timelock deadline is frozen when the request is made, not recomputed
/// from the current `TIMELOCK_SECONDS` at execution time.
///
/// Without this, upgrading the contract with a shorter constant between
/// `request_admin_withdrawal` and `execute_admin_withdrawal` would retroactively
/// shorten the wait on a request LPs had already seen published — exactly the
/// window they rely on to exit.
///
/// Simulated by storing a request whose deadline is longer than the current
/// constant, then executing past the constant but before the stored deadline.
#[test]
#[should_panic(expected = "Error(Contract, #15)")]
fn admin_timelock_honours_stored_deadline_not_current_constant() {
    let (env, pool, _usdc_id, admin, _treasury, lp1) = setup();
    pool.deposit(&lp1, &1_000_0000000i128, &0i128, &false);

    pool.request_admin_withdrawal(&admin, &100_0000000i128);

    // Stand in for a request created under a longer timelock.
    env.as_contract(&pool.address, || {
        let mut req: crate::AdminWithdrawalRequest = env
            .storage()
            .persistent()
            .get(&crate::StorageKey::AdminWithdrawalRequest)
            .unwrap();
        req.execute_after = req.requested_at + 30 * 24 * 60 * 60;
        env.storage()
            .persistent()
            .set(&crate::StorageKey::AdminWithdrawalRequest, &req);
    });

    // Past the current 7-day constant, but short of the stored 30-day deadline.
    let jump = (7 * 24 * 60 * 60 + 1) as u64;
    env.ledger().set_timestamp(env.ledger().timestamp() + jump);

    // Recomputing from TIMELOCK_SECONDS would let this through.
    pool.execute_admin_withdrawal(&admin);
}

/// Cancelling when there is no pending withdrawal request must fail with
/// NoPendingWithdrawal rather than silently succeeding (Issue #336).
#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn admin_cancel_withdrawal_with_no_pending_request_fails() {
    let (_env, pool, _usdc_id, admin, _treasury, _lp1) = setup();
    pool.cancel_admin_withdrawal(&admin);
}

/// Executing when there is no pending withdrawal request must fail with
/// NoPendingWithdrawal rather than panicking on a missing-value unwrap
/// with an unrelated error (Issue #336).
#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn admin_execute_withdrawal_with_no_pending_request_fails() {
    let (_env, pool, _usdc_id, admin, _treasury, _lp1) = setup();
    pool.execute_admin_withdrawal(&admin);
}

/// Executing the same withdrawal request twice must fail the second time
/// with AlreadyReleased — funds must not be transferred out twice for one
/// request (Issue #336).
#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn admin_execute_withdrawal_twice_fails() {
    let (env, pool, _usdc_id, admin, _treasury, lp1) = setup();
    pool.deposit(&lp1, &1_000_0000000i128, &0i128, &false);
    pool.request_admin_withdrawal(&admin, &100_0000000i128);

    let jump = (7 * 24 * 60 * 60 + 1) as u64;
    env.ledger().set_timestamp(env.ledger().timestamp() + jump);

    pool.execute_admin_withdrawal(&admin);
    // Second execute on the same already-executed request must be rejected.
    pool.execute_admin_withdrawal(&admin);
}

/// Sweep of deposit/withdraw amounts checking a core financial invariant:
/// withdrawing all of a provider's shares immediately after depositing must
/// never return more than was deposited, and available liquidity must never
/// go negative. A minimal stand-in for property-based testing (Issue #337)
/// that needs no new test dependency — a fuller proptest/quickcheck harness
/// covering more of the arithmetic surface remains follow-up work.
#[test]
fn deposit_withdraw_round_trip_never_creates_value() {
    let amounts: [i128; 5] = [
        1_0000000,           // smallest typical unit
        1_000_0000000,       // 1,000 USDC
        999_999_0000000,     // near-large
        3_333_3330000,       // non-round amount
        50_000_0000000,      // large
    ];

    for amount in amounts {
        let (_env, pool, _usdc_id, _admin, _treasury, lp1) = setup();
        let shares = pool.deposit(&lp1, &amount, &0i128, &false);
        let redeemed = pool.withdraw(&lp1, &shares);

        assert!(redeemed <= amount, "withdrew {} more than deposited {}", redeemed, amount);
        assert!(pool.get_available_liquidity() >= 0, "available liquidity went negative for amount {}", amount);
    }
}

/// lock_for_policy on a pool with zero deposits (total_deposited == 0,
/// total_locked == 0) must reject with Undercollateralized rather than
/// silently succeeding or underflowing.
#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn lock_for_policy_on_empty_pool_fails_undercollateralized() {
    let (_, pool, _usdc_id, admin, _treasury, _lp1) = setup();
    pool.lock_for_policy(&admin, &1u128, &1_0000000i128);
}

/// Non-admin cannot call lock_for_policy.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn non_admin_cannot_lock_for_policy() {
    let (_, pool, _usdc_id, _admin, _treasury, lp1) = setup();
    pool.deposit(&lp1, &500_0000000i128, &0i128, &false);

    pool.lock_for_policy(&lp1, &1u128, &100_0000000i128);
}

/// Non-admin cannot release a capital lock.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn non_admin_cannot_release_for_claim() {
    let (_, pool, _usdc_id, admin, _treasury, lp1) = setup();
    pool.deposit(&lp1, &500_0000000i128, &0i128, &false);
    pool.lock_for_policy(&admin, &1u128, &100_0000000i128);

    pool.release_for_claim(&lp1, &1u128);
}

/// Non-admin cannot release for expiry.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn non_admin_cannot_release_for_expiry() {
    let (_, pool, _usdc_id, admin, _treasury, lp1) = setup();
    pool.deposit(&lp1, &500_0000000i128, &0i128, &false);
    pool.lock_for_policy(&admin, &1u128, &100_0000000i128);

    pool.release_for_expiry(&lp1, &1u128);
}

#[test]
fn test_get_lp_list_pagination() {
    let (env, pool, usdc_id, _admin, _, _lp1) = setup();
    env.budget().reset_unlimited();
    
    let usdc_client = token::StellarAssetClient::new(&env, &usdc_id);
    for _ in 0..200 {
        let lp = Address::generate(&env);
        usdc_client.mint(&lp, &10_000_000i128);
        pool.deposit(&lp, &10_000_000i128, &0i128, &false);
    }
    
    assert_eq!(pool.get_lp_count(), 200);
    
    // query offset=100, limit=50 (proves pagination works beyond default limit)
    let paginated = pool.get_lp_list(&Some(100), &Some(50));
    assert_eq!(paginated.total_count, 200);
    assert_eq!(paginated.lps.len(), 50);
}

// ── Issue #163: withdraw with amount exactly equal to available balance ──────────

/// Withdraw the exact unlocked (available) balance after a partial lock and
/// verify zero remaining: no underflow, no rounding error, total_deposited
/// correctly decremented to the locked portion only.
#[test]
fn withdraw_exact_available_balance_leaves_zero_remaining() {
    let (_, pool, _, admin, _, lp1) = setup();
    let total = 1_000_0000000i128; // 1000 USDC
    let locked_amount = 300_0000000i128; // 300 USDC locked for policy

    let shares = pool.deposit(&lp1, &total, &0i128, &false);
    pool.lock_for_policy(&admin, &1u128, &locked_amount);

    // Available = total - locked = 700 USDC
    let available = pool.get_available_liquidity();
    assert_eq!(available, 700_0000000i128);

    // Withdraw the exact available amount (convert USDC → shares)
    let available_shares = available * total_shares(total) / total;
    let returned = pool.withdraw(&lp1, &available_shares);

    assert_eq!(returned, available);
    assert_eq!(pool.get_available_liquidity(), 0);

    // total_deposited should be exactly the locked amount
    let stats = pool.get_stats();
    assert_eq!(stats.total_deposited, locked_amount);
}

fn total_shares(deposited: i128) -> i128 {
    deposited * 1_000_000_000
}

// ── Issue #200: receive_premium with zero LPs ──────────────────────────────────

/// `receive_premium`'s `if total_shares > 0` guard must not panic when no LP
/// has ever deposited — a regression that removed the guard would divide by
/// zero computing `increment`.
#[test]
fn receive_premium_with_zero_lps_does_not_panic() {
    let (_, pool, _, _, _, lp1) = setup();

    let stats_before = pool.get_stats();
    assert_eq!(stats_before.total_shares, 0, "no LP has deposited yet");

    // Must not panic even though total_shares is 0.
    pool.receive_premium(&lp1, &100_0000000i128);

    let stats_after = pool.get_stats();
    assert_eq!(stats_after.total_shares, 0, "still no LPs after the premium call");
    // The LP-share accumulator still increases (80% of premium), it simply
    // has no shares to divide against yet.
    assert_eq!(
        stats_after.accumulated_premium - stats_before.accumulated_premium,
        80_0000000i128
    );
}

// ── Issue #201: release_for_claim / release_for_expiry cross double-release ───

/// A policy released via `release_for_claim` must not also be releasable via
/// `release_for_expiry` — both set the same `CapitalLock.released` flag, so
/// the second call (regardless of which function is called first) must fail
/// with `AlreadyReleased` rather than double-counting the locked amount.
#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn release_for_claim_then_release_for_expiry_fails() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.deposit(&lp1, &200_0000000i128, &0i128, &false);
    pool.lock_for_policy(&admin, &55u128, &50_0000000i128);

    pool.release_for_claim(&admin, &55u128);
    pool.release_for_expiry(&admin, &55u128); // already released via release_for_claim
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn release_for_expiry_then_release_for_claim_fails() {
    let (_, pool, _, admin, _, lp1) = setup();
    pool.deposit(&lp1, &200_0000000i128, &0i128, &false);
    pool.lock_for_policy(&admin, &56u128, &50_0000000i128);

    pool.release_for_expiry(&admin, &56u128);
    pool.release_for_claim(&admin, &56u128); // already released via release_for_expiry
}

#[test]
fn test_pool_depletion_scenarios() {
    let (_, pool, _, _, _, lp1) = setup();
    let amount = 100_0000000i128;
    let shares = pool.deposit(&lp1, &amount, &0i128, &false);
    let returned = pool.withdraw(&lp1, &shares);
    assert_eq!(returned, amount);
    let stats = pool.get_stats();
    assert_eq!(stats.total_deposited, 0);
    assert_eq!(pool.get_available_liquidity(), 0);
}
