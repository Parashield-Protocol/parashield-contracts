#![cfg(test)]

//! Tests for Issue #681: OracleVerifier stake withdrawal challenge period and slashing.
//!
//! Validates:
//! - Oracles cannot withdraw stake while active.
//! - Oracles must initiate a withdrawal request which enters a challenge period.
//! - Early withdrawal before challenge period elapsed is rejected.
//! - Admin can slash bad/fraudulent data during the challenge period.
//! - Slashed amount is deducted from the pending withdrawal.
//! - Normal withdrawal succeeds after the challenge period.
//! - Full slashing voids the stake withdrawal.
//! - Withdrawal request can be cancelled.

use parashield_oracle_verifier::{Error, OracleVerifier, OracleVerifierClient};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    token, Address, Env, Symbol,
};

const NOW_TS: u64 = 1_748_736_000;

fn weather() -> Symbol {
    symbol_short!("weather")
}

struct TestFixture {
    env: Env,
    admin: Address,
    oracle: Address,
    token_admin: Address,
    token_addr: Address,
    client: OracleVerifierClient<'static>,
}

fn setup_stake_fixture() -> TestFixture {
    let env = Env::default();
    env.ledger().set_timestamp(NOW_TS);
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let contract_id = env.register(OracleVerifier, ());
    let client = OracleVerifierClient::new(&env, &contract_id);
    client.initialize(&admin);

    let token_admin = Address::generate(&env);
    let token_contract = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_addr = token_contract.address();

    client.set_stake_token(&admin, &token_addr);

    let oracle = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_addr).mint(&oracle, &10_000_000);

    // Register oracle
    client.add_oracle(&admin, &oracle, &weather(), &50u32);

    TestFixture {
        env,
        admin,
        oracle,
        token_admin,
        token_addr,
        client,
    }
}

#[test]
fn stake_withdrawal_requires_challenge_period_and_succeeds_after_unlock() {
    let f = setup_stake_fixture();
    let stake_amt = 1_000_000i128;

    // Stake tokens
    f.client.stake(&f.oracle, &weather(), &stake_amt);
    assert_eq!(f.client.get_oracle_stake(&weather(), &f.oracle), stake_amt);

    // Cannot request withdrawal while oracle is still active
    let res = f.client.try_request_stake_withdrawal(&f.oracle, &weather());
    assert_eq!(res.unwrap_err().unwrap(), Error::OracleStillActive.into());

    // Deactivate oracle
    f.client.remove_oracle(&f.admin, &f.oracle, &weather());

    // Request withdrawal - starts 7-day challenge period
    f.client.request_stake_withdrawal(&f.oracle, &weather());

    let req = f.client.get_stake_withdrawal_request(&weather(), &f.oracle).unwrap();
    assert_eq!(req.amount, stake_amt);
    assert_eq!(req.requested_at, NOW_TS);
    let challenge_period = f.client.get_challenge_period();
    assert_eq!(req.unlock_at, NOW_TS + challenge_period);

    // Early withdrawal fails
    f.env.ledger().set_timestamp(NOW_TS + challenge_period - 1);
    let early_res = f.client.try_withdraw_stake(&f.oracle, &weather());
    assert_eq!(early_res.unwrap_err().unwrap(), Error::ChallengePeriodNotElapsed.into());

    // After challenge period expires, withdrawal succeeds
    f.env.ledger().set_timestamp(NOW_TS + challenge_period + 1);
    f.client.withdraw_stake(&f.oracle, &weather());

    assert_eq!(f.client.get_oracle_stake(&weather(), &f.oracle), 0);
    assert!(f.client.get_stake_withdrawal_request(&weather(), &f.oracle).is_none());

    let oracle_token_balance = token::Client::new(&f.env, &f.token_addr).balance(&f.oracle);
    assert_eq!(oracle_token_balance, 10_000_000);
}

#[test]
fn stake_slashing_during_challenge_period_reduces_withdrawable_amount() {
    let f = setup_stake_fixture();
    let stake_amt = 1_000_000i128;

    f.client.stake(&f.oracle, &weather(), &stake_amt);
    f.client.remove_oracle(&f.admin, &f.oracle, &weather());
    f.client.request_stake_withdrawal(&f.oracle, &weather());

    // Bad data discovered during challenge period - admin slashes 400,000
    let slash_amt = 400_000i128;
    f.client.slash_oracle(
        &f.admin,
        &f.oracle,
        &weather(),
        &slash_amt,
        &symbol_short!("baddata"),
    );

    // Verify stake and pending request were reduced
    assert_eq!(f.client.get_oracle_stake(&weather(), &f.oracle), 600_000);
    let req = f.client.get_stake_withdrawal_request(&weather(), &f.oracle).unwrap();
    assert_eq!(req.amount, 600_000);

    // Fast-forward past challenge period
    let challenge_period = f.client.get_challenge_period();
    f.env.ledger().set_timestamp(NOW_TS + challenge_period + 10);

    // Oracle withdraws remaining stake
    f.client.withdraw_stake(&f.oracle, &weather());

    assert_eq!(f.client.get_oracle_stake(&weather(), &f.oracle), 0);
    let oracle_token_balance = token::Client::new(&f.env, &f.token_addr).balance(&f.oracle);
    // Oracle started with 10_000_000, deposited 1_000_000 (had 9_000_000), got back 600_000 -> 9_600_000
    assert_eq!(oracle_token_balance, 9_600_000);
}

#[test]
fn full_slashing_during_challenge_period_voids_withdrawal() {
    let f = setup_stake_fixture();
    let stake_amt = 500_000i128;

    f.client.stake(&f.oracle, &weather(), &stake_amt);
    f.client.remove_oracle(&f.admin, &f.oracle, &weather());
    f.client.request_stake_withdrawal(&f.oracle, &weather());

    // Admin slashes 100% of stake for severe malicious data
    f.client.slash_oracle(
        &f.admin,
        &f.oracle,
        &weather(),
        &stake_amt,
        &symbol_short!("fraud"),
    );

    assert_eq!(f.client.get_oracle_stake(&weather(), &f.oracle), 0);

    let challenge_period = f.client.get_challenge_period();
    f.env.ledger().set_timestamp(NOW_TS + challenge_period + 10);

    // Attempting to withdraw after being completely slashed returns NoStakeWithdrawalRequest
    // because the pending withdrawal request was completely voided/removed upon full slash.
    let res = f.client.try_withdraw_stake(&f.oracle, &weather());
    assert_eq!(res.unwrap_err().unwrap(), Error::NoStakeWithdrawalRequest.into());
}

#[test]
fn cancel_stake_withdrawal_restores_pending_state() {
    let f = setup_stake_fixture();
    let stake_amt = 500_000i128;

    f.client.stake(&f.oracle, &weather(), &stake_amt);
    f.client.remove_oracle(&f.admin, &f.oracle, &weather());
    f.client.request_stake_withdrawal(&f.oracle, &weather());

    assert!(f.client.get_stake_withdrawal_request(&weather(), &f.oracle).is_some());

    // Cancel withdrawal
    f.client.cancel_stake_withdrawal(&f.oracle, &weather());
    assert!(f.client.get_stake_withdrawal_request(&weather(), &f.oracle).is_none());

    // Stake still remains in contract
    assert_eq!(f.client.get_oracle_stake(&weather(), &f.oracle), stake_amt);
}

#[test]
fn stake_deposit_is_recorded_and_challenge_period_is_enforced() {
    let f = setup_stake_fixture();
    f.client.stake(&f.oracle, &weather(), &250_000i128);
    assert_eq!(f.client.get_oracle_stake(&weather(), &f.oracle), 250_000);

    f.client.remove_oracle(&f.admin, &f.oracle, &weather());
    f.client.request_stake_withdrawal(&f.oracle, &weather());
    let unlock_at = f.client.get_stake_withdrawal_request(&weather(), &f.oracle).unwrap().unlock_at;
    f.env.ledger().set_timestamp(unlock_at - 1);
    assert_eq!(f.client.try_withdraw_stake(&f.oracle, &weather()).unwrap_err().unwrap(), Error::ChallengePeriodNotElapsed.into());
}
