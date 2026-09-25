#![cfg(test)]

//! Issue #458: risk-pool exit-queue concurrency coverage.
//!
//! Placed as an integration test rather than a `mod` under `src/` because
//! the crate's `lib_test` binary has pre-existing compile errors on `main`
//! (55 at the time of writing). Integration tests compile per-file and are
//! unaffected.
//!
//! Behavioural change under test: `request_exit` used to reject a second
//! call while a previous one was still queued (`Error::ExitAlreadyQueued`).
//! It now accumulates — the new shares are added to the existing entry and
//! the original `claimable_at` is preserved so a top-up cannot silently
//! defer the initial unlock. The balance check runs against the summed
//! total, so a top-up that would exceed the provider's live share balance
//! is rejected with `InsufficientFunds` and leaves the existing entry
//! untouched.

use parashield_risk_pool::{RiskPool, RiskPoolClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Env, Symbol,
};

fn setup() -> (Env, RiskPoolClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    // Anchor the ledger at a non-zero timestamp so `requested_at` /
    // `claimable_at` comparisons in the accumulate path are meaningful.
    env.ledger().set_timestamp(1_748_736_000);

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let lp = Address::generate(&env);
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

    token::StellarAssetClient::new(&env, &usdc_id).mint(&lp, &1_000_000_000_0000000i128);

    pool.initialize(
        &admin,
        &usdc_id,
        &treasury,
        &backstop_id,
        &Symbol::new(&env, "crop"),
        &policy_engine,
        &claims_processor,
    );

    (env, pool, lp, usdc_id)
}

/// Deposit 1000 USDC and return the shares issued so a spec can call
/// `request_exit` with a known ceiling.
fn deposit(pool: &RiskPoolClient, lp: &Address, amount: i128) -> i128 {
    pool.deposit(lp, &amount, &0i128, &false)
}

// First-deposit share issuance is `amount * 1e9`, so a deposit of 1e13 stroops
// mints 1e22 shares. Downstream `withdraw_inner` computes
// `withdraw_amount = shares * total_deposited / total_shares`, which truncates
// to 0 for any exit share count below ~1e9. Every exit here uses a base unit
// far above that floor so the arithmetic yields a real payout at claim time.
const EXIT_100: i128 = 100_000_000_000; // "100" scaled to survive the truncation
const EXIT_50: i128 = 50_000_000_000; // "50"
const EXIT_150: i128 = 150_000_000_000; // "150" (100 + 50)
const EXIT_20: i128 = 20_000_000_000; // over-the-limit top-up amount

#[test]
fn sequential_exit_requests_accumulate_into_one_entry() {
    let (_env, pool, lp, _usdc) = setup();
    let shares = deposit(&pool, &lp, 1_000_000_0000000i128);
    assert!(shares >= EXIT_150, "sanity: minted enough shares to cover the top-up");

    pool.request_exit(&lp, &EXIT_100);
    pool.request_exit(&lp, &EXIT_50);

    let info = pool.get_exit_info(&lp);
    assert_eq!(info.shares, EXIT_150, "second request must add to the first");
    // Pool-wide queued counter also reflects the accumulation.
    assert_eq!(pool.get_queued_exit_shares(), EXIT_150);
}

#[test]
fn second_request_preserves_original_claimable_at() {
    let (env, pool, lp, _usdc) = setup();
    deposit(&pool, &lp, 1_000_000_0000000i128);

    pool.request_exit(&lp, &EXIT_100);
    let first_info = pool.get_exit_info(&lp);
    let first_claimable = first_info.claimable_at;
    let first_requested = first_info.requested_at;

    // Move the clock forward, then top up. The unlock timestamp must not
    // shift with the top-up — otherwise a provider could re-request every
    // few seconds to keep their exit permanently locked at their discretion.
    env.ledger().with_mut(|l| l.timestamp += 1_000);

    pool.request_exit(&lp, &EXIT_50);
    let second_info = pool.get_exit_info(&lp);
    assert_eq!(second_info.claimable_at, first_claimable);
    assert_eq!(second_info.requested_at, first_requested);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn top_up_exceeding_share_balance_reverts_and_leaves_first_entry_untouched() {
    let (_env, pool, lp, _usdc) = setup();
    let shares = deposit(&pool, &lp, 1_000_000_0000000i128);

    // Queue an exit for (shares - 10). A follow-up top-up for EXIT_20 would
    // push the total well past the LP's live position and must revert with
    // InsufficientFunds (Error #4). The existing entry stays intact — see
    // the sibling `reverted_top_up_does_not_clobber_the_existing_entry` test
    // for the state-assertions after the revert.
    pool.request_exit(&lp, &(shares - 10));
    pool.request_exit(&lp, &EXIT_20);
}

#[test]
fn reverted_top_up_does_not_clobber_the_existing_entry() {
    let (_env, pool, lp, _usdc) = setup();
    let shares = deposit(&pool, &lp, 1_000_000_0000000i128);

    pool.request_exit(&lp, &(shares - 10));
    let before = pool.get_exit_info(&lp);

    // The top-up would overflow the position; expect it to revert. `try_*`
    // returns Result rather than panicking so this test can inspect state
    // after the revert.
    let result = pool.try_request_exit(&lp, &EXIT_20);
    assert!(result.is_err());

    let after = pool.get_exit_info(&lp);
    assert_eq!(after.shares, before.shares);
    assert_eq!(after.claimable_at, before.claimable_at);
    assert_eq!(after.requested_at, before.requested_at);
    assert_eq!(pool.get_queued_exit_shares(), before.shares);
}

#[test]
fn fresh_request_after_claim_starts_a_new_entry() {
    let (env, pool, lp, _usdc) = setup();
    deposit(&pool, &lp, 1_000_000_0000000i128);

    pool.request_exit(&lp, &EXIT_100);
    // Wait past the exit delay so the claim can settle.
    let delay = pool.get_exit_delay();
    env.ledger().with_mut(|l| l.timestamp += delay + 1);
    pool.claim_exit(&lp);

    // Queue counter cleared, no pending request left.
    assert_eq!(pool.get_queued_exit_shares(), 0);

    // A new exit request now must open a fresh entry, not attempt to
    // accumulate on the freed slot.
    pool.request_exit(&lp, &EXIT_50);
    let info = pool.get_exit_info(&lp);
    assert_eq!(info.shares, EXIT_50);
    assert_eq!(pool.get_queued_exit_shares(), EXIT_50);
}
