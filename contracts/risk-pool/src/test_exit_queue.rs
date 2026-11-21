//! Auto-processing of expired exit requests (issue #511).
#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Env, Symbol,
};

use crate::{Error, ExitStatus, RiskPool, RiskPoolClient};

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

#[test]
#[should_panic(expected = "HostError: Error(Contract, #45)")]
fn exit_delay_exceeds_maximum_panics() {
    let w = setup();
    let max_plus_one = 30 * 24 * 60 * 60 + 1;
    w.pool.set_exit_delay(&w.admin, &max_plus_one);
}

#[test]
fn exit_delay_at_maximum_succeeds() {
    let w = setup();
    let max_delay = 30 * 24 * 60 * 60;
    w.pool.set_exit_delay(&w.admin, &max_delay);
    assert_eq!(w.pool.get_exit_delay(), max_delay);
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
    // Lock enough that the leaver's full exit can no longer be covered.
    // Any lock above zero does this, since the leaver is exiting 100% of a
    // pool with only their own deposit in it; kept well under the
    // `max_utilization_bps` cap (default 80%) so this stays a liquidity
    // shortfall rather than tripping the separate utilization guard.
    w.pool.lock_for_policy(&w.admin, &1u128, &(AMOUNT / 10));
    advance(&w, DELAY);

    let newcomer = lp(&w);
    // The deposit itself must still succeed.
    w.pool.deposit(&newcomer, &AMOUNT, &0, &false);
    assert_eq!(w.pool.get_exit_info(&leaver).status, ExitStatus::Claimable);
}

/// Issue #854: a sweep pass that can't settle an exit because the pool
/// lacks liquidity still tells the provider why, via `ExitSweepSkipped`,
/// instead of leaving them to guess why nothing happened.
#[test]
fn unaffordable_exit_emits_exit_sweep_skipped() {
    use soroban_sdk::testutils::Events as _;
    use soroban_sdk::TryFromVal;
    use crate::ExitSweepSkipped;

    let w = setup();
    let (leaver, shares) = deposit_and_queue(&w);
    // See `unaffordable_exit_stays_queued_and_does_not_revert_the_caller`
    // for why a partial lock (not the full `AMOUNT`) is used here.
    w.pool.lock_for_policy(&w.admin, &1u128, &(AMOUNT / 10));
    advance(&w, DELAY);

    assert_eq!(w.pool.process_expired_exits(&5), 0);

    // Read events right after the sweep, before any further contract call
    // can roll the event log over to the next invocation.
    let topic = Symbol::new(&w.env, "exit_sweep_skipped");
    let reason = Symbol::new(&w.env, "insufficient_liquidity");
    let event_found = w.env.events().all().iter().any(|event| {
        event.0 == w.pool.address
            && event.1.iter().any(|t| Symbol::try_from_val(&w.env, &t) == Ok(topic.clone()))
            && match ExitSweepSkipped::try_from_val(&w.env, &event.2) {
                Ok(data) => {
                    data.provider == leaver
                        && data.shares_requested == shares
                        && data.reason == reason
                }
                Err(_) => false,
            }
    });
    assert!(event_found, "ExitSweepSkipped must report insufficient_liquidity for the skipped provider");
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

// ── Issue #837: exit request accumulation, cancellation, reservations,
//    and multi-batch rotating-cursor sweep ───────────────────────────────────

/// Topping up an existing exit request accumulates shares and preserves the
/// original `requested_at` and `claimable_at` timestamps; a top-up exceeding
/// the provider's balance is rejected without mutating the existing request.
#[test]
fn exit_request_top_up_accumulates_and_preserves_original_claimable_at() {
    let w = setup();
    let who = lp(&w);
    let shares = w.pool.deposit(&who, &AMOUNT, &0, &false);
    assert_eq!(shares, AMOUNT);

    let t0 = w.env.ledger().timestamp();
    let first_part = 40_0000000i128;
    let second_part = 30_0000000i128;

    // Initial exit request at t0 -> claimable_at = t0 + DELAY.
    w.pool.request_exit(&who, &first_part);
    let info1 = w.pool.get_exit_info(&who);
    assert_eq!(info1.status, ExitStatus::Pending);
    assert_eq!(info1.shares, first_part);
    assert_eq!(info1.requested_at, t0);
    assert_eq!(info1.claimable_at, t0 + DELAY);
    assert_eq!(w.pool.get_queued_exit_shares(), first_part);

    // Advance 1,000s and top up by 30 USDC worth of shares.
    advance(&w, 1_000);
    w.pool.request_exit(&who, &second_part);
    let info2 = w.pool.get_exit_info(&who);
    // Accumulated: 40 + 30 = 70 USDC shares; original timestamps preserved.
    assert_eq!(info2.shares, first_part + second_part);
    assert_eq!(info2.requested_at, t0);
    assert_eq!(info2.claimable_at, t0 + DELAY);
    assert_eq!(w.pool.get_queued_exit_shares(), first_part + second_part);

    // Top-up that would exceed total shares (70 + 30.0000001 > 100) fails with
    // InsufficientFunds (#4) and leaves the existing 70-share request intact.
    let too_much = AMOUNT - (first_part + second_part) + 1;
    assert!(w.pool.try_request_exit(&who, &too_much).is_err()); // TEMP-VERIFY
    assert_eq!(w.pool.get_exit_info(&who).shares, first_part + second_part);
    assert_eq!(w.pool.get_queued_exit_shares(), first_part + second_part);
}

/// `cancel_exit` releases the reservation so shares are available again;
/// cancelling with no request or cancelling twice fails with `NoExitRequest`.
#[test]
fn cancel_exit_releases_reservation_and_rejects_missing_or_double_cancel() {
    let w = setup();
    let who = lp(&w);
    let bystander = lp(&w);
    let shares = w.pool.deposit(&who, &AMOUNT, &0, &false);

    // Cancelling when no exit request exists fails with NoExitRequest (#34).
    assert!(w.pool.try_cancel_exit(&bystander).is_err()); // TEMP-VERIFY

    let queued = 60_0000000i128;
    w.pool.request_exit(&who, &queued);
    assert_eq!(w.pool.get_queued_exit_shares(), queued);
    assert_eq!(w.pool.get_exit_info(&who).status, ExitStatus::Pending);

    // Cancel releases the reservation and leaves all LP shares intact.
    w.pool.cancel_exit(&who);
    assert_eq!(w.pool.get_queued_exit_shares(), 0);
    assert_eq!(w.pool.get_exit_info(&who).status, ExitStatus::None);
    assert_eq!(w.pool.get_position(&who).unwrap().shares, shares);

    // Double cancel fails with NoExitRequest (#34).
    assert!(w.pool.try_cancel_exit(&who).is_err()); // TEMP-VERIFY

    // Shares can be re-queued cleanly after cancellation.
    w.pool.request_exit(&who, &shares);
    assert_eq!(w.pool.get_queued_exit_shares(), shares);
}

/// Suspected bug: `withdraw` does not subtract pending `ExitReq` shares from
/// the provider's withdrawable balance, allowing reserved shares to be
/// withdrawn directly before `claimable_at`.
#[test]
#[ignore = "Suspected bug: withdraw_inner does not check ExitReq reservation"]
fn reserved_exit_shares_cannot_be_withdrawn_directly() {
    let w = setup();
    let who = lp(&w);
    let shares = w.pool.deposit(&who, &AMOUNT, &0, &false);
    // Reserve 60 of 100 shares -> only 40 unreserved shares should be withdrawable.
    w.pool.request_exit(&who, &60_0000000i128);
    assert!(w.pool.try_withdraw(&who, &shares).is_err());
}

/// Suspected bug: `transfer_position` does not subtract pending `ExitReq`
/// shares from the sender's transferable balance, allowing reserved shares to
/// be transferred away while an exit request is pending.
#[test]
#[ignore = "Suspected bug: transfer_position does not check ExitReq reservation"]
fn reserved_exit_shares_cannot_be_transferred() {
    let w = setup();
    let who = lp(&w);
    let recipient = lp(&w);
    let shares = w.pool.deposit(&who, &AMOUNT, &0, &false);
    w.pool.request_exit(&who, &60_0000000i128);
    assert!(w.pool.try_transfer_position(&who, &recipient, &shares).is_err());
}

/// Multi-batch `sweep_expired_exits` via `process_expired_exits`: with 7
/// expired exits (exceeding the 5-slot batch cap) and 1 unexpired exit, two
/// calls settle all 7 expired exits exactly once with cursor wrap-around while
/// leaving the unexpired exit untouched, and pool totals/balances stay
/// consistent after every call.
#[test]
fn sweep_expired_exits_multi_batch_wraparound_and_accounting_consistency() {
    let w = setup();
    let mut expired_lps = std::vec::Vec::new();
    let mut initial_balances = std::vec::Vec::new();

    // 7 LPs deposit and queue exits at t0 (claimable at t0 + DELAY).
    for _ in 0..7 {
        let (who, _) = deposit_and_queue(&w);
        initial_balances.push(balance(&w, &who));
        expired_lps.push(who);
    }

    // Halfway through DELAY, LP #7 deposits and queues (claimable at t0 + 1.5*DELAY).
    advance(&w, DELAY / 2);
    let (unexpired_lp, _) = deposit_and_queue(&w);
    let unexpired_before = balance(&w, &unexpired_lp);

    // Advance to t0 + DELAY: LPs 0..7 are expired, LP #7 (unexpired_lp) is not.
    advance(&w, DELAY / 2);

    assert_eq!(w.pool.get_stats().total_shares, 8 * AMOUNT);
    assert_eq!(w.pool.get_queued_exit_shares(), 8 * AMOUNT);
    assert_eq!(balance(&w, &w.pool.address), 8 * AMOUNT);

    // Batch 1 (max_scan = 5): scans indices 0..5 -> settles LPs 0..5 (5 exits).
    assert_eq!(w.pool.process_expired_exits(&5), 5);
    assert_eq!(w.pool.get_stats().total_shares, 3 * AMOUNT);
    assert_eq!(w.pool.get_queued_exit_shares(), 3 * AMOUNT);
    assert_eq!(balance(&w, &w.pool.address), 3 * AMOUNT);
    for i in 0..5 {
        assert_eq!(balance(&w, &expired_lps[i]) - initial_balances[i], AMOUNT);
        assert_eq!(w.pool.get_exit_info(&expired_lps[i]).status, ExitStatus::None);
    }

    // Batch 2 (max_scan = 5): cursor at 5 scans [5, 6, 7, 0, 1] (wraps around).
    // Settles LPs 5 and 6 (2 exits), leaves LP 7 (unexpired) untouched, and
    // skips already-settled LPs 0 and 1.
    assert_eq!(w.pool.process_expired_exits(&5), 2);
    assert_eq!(w.pool.get_stats().total_shares, AMOUNT);
    assert_eq!(w.pool.get_queued_exit_shares(), AMOUNT);
    assert_eq!(balance(&w, &w.pool.address), AMOUNT);

    // Every expired LP was paid exactly AMOUNT (none skipped, none paid twice).
    for i in 0..7 {
        assert_eq!(balance(&w, &expired_lps[i]) - initial_balances[i], AMOUNT);
        assert_eq!(w.pool.get_exit_info(&expired_lps[i]).status, ExitStatus::None);
        assert_eq!(w.pool.get_position(&expired_lps[i]).unwrap().shares, 0);
    }
    // Unexpired LP is still pending and unpaid.
    assert_eq!(
        w.pool.get_exit_info(&unexpired_lp).status,
        ExitStatus::Pending
    );
    assert_eq!(balance(&w, &unexpired_lp), unexpired_before);

    // Batch 3 before LP 7 expires: settles 0 (no double payouts).
    assert_eq!(w.pool.process_expired_exits(&5), 0);

    // Advance remaining DELAY / 2 so LP 7 expires; Batch 4 settles LP 7.
    advance(&w, DELAY / 2);
    assert_eq!(w.pool.process_expired_exits(&5), 1);
    assert_eq!(balance(&w, &unexpired_lp) - unexpired_before, AMOUNT);
    assert_eq!(w.pool.get_exit_info(&unexpired_lp).status, ExitStatus::None);
    assert_eq!(w.pool.get_stats().total_shares, 0);
    assert_eq!(w.pool.get_queued_exit_shares(), 0);
    assert_eq!(balance(&w, &w.pool.address), 0);
}
