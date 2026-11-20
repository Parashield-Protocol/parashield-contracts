#![allow(clippy::inconsistent_digit_grouping)]
//! Reinsurance tests (issue #382): configuring an external reinsurance
//! arrangement, ceding premium to it, and recovering catastrophic claim
//! losses from it.

#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke},
    token, Address, Env, IntoVal, Symbol,
};

use crate::{Error, RiskPool, RiskPoolClient};

// Each mock reinsurer lives in its own module: `#[contractimpl]` generates
// module-scoped spec constants keyed only by function name (e.g.
// `__SPEC_XDR_FN_RECOVER`), so two `#[contract]` types in the same module
// both exposing `recover`/`init` collide at compile time. The function
// *names* still have to match across all three (`init`, `recover`) since
// `recover` is the on-chain entry point `ReinsurerClient` dispatches to.

/// A well-behaved reinsurer: pays out exactly what was requested and
/// reports having paid exactly that.
mod mock_reinsurer {
    use soroban_sdk::{token, Address, Env, Symbol};

    #[soroban_sdk::contract]
    pub struct MockReinsurer;

    #[soroban_sdk::contractimpl]
    impl MockReinsurer {
        pub fn init(env: Env, usdc: Address) {
            env.storage().instance().set(&Symbol::new(&env, "usdc"), &usdc);
        }

        pub fn recover(env: Env, caller: Address, _policy_id: u128, amount: i128) -> i128 {
            let usdc: Address = env.storage().instance().get(&Symbol::new(&env, "usdc")).unwrap();
            token::Client::new(&env, &usdc).transfer(&env.current_contract_address(), &caller, &amount);
            amount
        }
    }
}
pub use mock_reinsurer::{MockReinsurer, MockReinsurerClient};

/// A reinsurer whose own capacity is exhausted: it only ever pays out (and
/// reports) half of what is requested.
mod mock_stingy_reinsurer {
    use soroban_sdk::{token, Address, Env, Symbol};

    #[soroban_sdk::contract]
    pub struct MockStingyReinsurer;

    #[soroban_sdk::contractimpl]
    impl MockStingyReinsurer {
        pub fn init(env: Env, usdc: Address) {
            env.storage().instance().set(&Symbol::new(&env, "usdc"), &usdc);
        }

        pub fn recover(env: Env, caller: Address, _policy_id: u128, amount: i128) -> i128 {
            let usdc: Address = env.storage().instance().get(&Symbol::new(&env, "usdc")).unwrap();
            let half = amount / 2;
            token::Client::new(&env, &usdc).transfer(&env.current_contract_address(), &caller, &half);
            half
        }
    }
}
pub use mock_stingy_reinsurer::{MockStingyReinsurer, MockStingyReinsurerClient};

/// A misbehaving (or buggy) reinsurer: transfers only what was requested
/// but *reports* having paid double. Exercises the pool's defensive clamp.
mod mock_over_reporting_reinsurer {
    use soroban_sdk::{token, Address, Env, Symbol};

    #[soroban_sdk::contract]
    pub struct MockOverReportingReinsurer;

    #[soroban_sdk::contractimpl]
    impl MockOverReportingReinsurer {
        pub fn init(env: Env, usdc: Address) {
            env.storage().instance().set(&Symbol::new(&env, "usdc"), &usdc);
        }

        pub fn recover(env: Env, caller: Address, _policy_id: u128, amount: i128) -> i128 {
            let usdc: Address = env.storage().instance().get(&Symbol::new(&env, "usdc")).unwrap();
            token::Client::new(&env, &usdc).transfer(&env.current_contract_address(), &caller, &amount);
            amount * 2
        }
    }
}
pub use mock_over_reporting_reinsurer::{MockOverReportingReinsurer, MockOverReportingReinsurerClient};

fn setup() -> (Env, RiskPoolClient<'static>, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let policy_engine = Address::generate(&env);
    let claims_processor = Address::generate(&env);

    let usdc_id = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let backstop_id = env.register_stellar_asset_contract_v2(admin.clone()).address();
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

    (env, pool, usdc_id, admin, claims_processor)
}

#[test]
fn set_reinsurance_requires_admin() {
    let (env, pool, _usdc, _admin, _cp) = setup();
    let reinsurer = Address::generate(&env);
    let impostor = Address::generate(&env);
    let result = pool.try_set_reinsurance(&impostor, &reinsurer, &1_000_0000000i128, &100_000_0000000i128);
    assert!(result.is_err());
}

#[test]
fn get_reinsurance_is_none_until_configured() {
    let (_env, pool, _usdc, _admin, _cp) = setup();
    assert_eq!(pool.get_reinsurance(), None);
    let stats = pool.get_reinsurance_stats();
    assert_eq!(stats.total_premium_paid, 0);
    assert_eq!(stats.total_recovered, 0);
    assert_eq!(stats.coverage_remaining, 0);
}

#[test]
fn admin_can_configure_reinsurance() {
    let (env, pool, _usdc, admin, _cp) = setup();
    let reinsurer = Address::generate(&env);
    pool.set_reinsurance(&admin, &reinsurer, &1_000_0000000i128, &100_000_0000000i128);

    let cfg = pool.get_reinsurance().unwrap();
    assert_eq!(cfg.reinsurer, reinsurer);
    assert_eq!(cfg.attachment_point, 1_000_0000000i128);
    assert_eq!(cfg.coverage_limit, 100_000_0000000i128);
    assert!(cfg.active);
}

#[test]
fn send_premium_to_reinsurer_requires_configuration() {
    let (_env, pool, _usdc, admin, _cp) = setup();
    let result = pool.try_send_premium_to_reinsurer(&admin, &1_000_0000000i128);
    assert!(result.is_err());
}

#[test]
fn send_premium_to_reinsurer_transfers_and_tracks_total() {
    let (env, pool, usdc, admin, _cp) = setup();
    let reinsurer = Address::generate(&env);
    pool.set_reinsurance(&admin, &reinsurer, &1_000_0000000i128, &100_000_0000000i128);

    // Fund the pool itself, as premium income would.
    token::StellarAssetClient::new(&env, &usdc).mint(&pool.address, &50_000_0000000i128);

    pool.send_premium_to_reinsurer(&admin, &10_000_0000000i128);

    assert_eq!(token::Client::new(&env, &usdc).balance(&reinsurer), 10_000_0000000i128);
    assert_eq!(pool.get_reinsurance_stats().total_premium_paid, 10_000_0000000i128);
}

/// A claim loss below the attachment point is retained by the pool
/// entirely — no recovery is requested or paid.
#[test]
fn loss_below_attachment_point_recovers_nothing() {
    let (env, pool, usdc, admin, claims_processor) = setup();
    let reinsurer_id = env.register(MockReinsurer, ());
    MockReinsurerClient::new(&env, &reinsurer_id).init(&usdc);
    token::StellarAssetClient::new(&env, &usdc).mint(&reinsurer_id, &1_000_000_0000000i128);

    pool.set_reinsurance(&admin, &reinsurer_id, &10_000_0000000i128, &1_000_000_0000000i128);

    let recovered = pool.request_reinsurance_recovery(&claims_processor, &1u128, &5_000_0000000i128);
    assert_eq!(recovered, 0);
    assert_eq!(pool.get_reinsurance_stats().total_recovered, 0);
}

/// A claim loss above the attachment point recovers the excess from the
/// reinsurer, and that recovery replenishes `total_deposited`.
#[test]
fn loss_above_attachment_point_recovers_the_excess() {
    let (env, pool, usdc, admin, claims_processor) = setup();
    let reinsurer_id = env.register(MockReinsurer, ());
    MockReinsurerClient::new(&env, &reinsurer_id).init(&usdc);
    token::StellarAssetClient::new(&env, &usdc).mint(&reinsurer_id, &1_000_000_0000000i128);

    pool.set_reinsurance(&admin, &reinsurer_id, &10_000_0000000i128, &1_000_000_0000000i128);

    let deposited_before = pool.get_stats().total_deposited;

    // Loss of 50,000; attachment point 10,000 → excess of 40,000 eligible.
    let recovered = pool.request_reinsurance_recovery(&claims_processor, &1u128, &50_000_0000000i128);
    assert_eq!(recovered, 40_000_0000000i128);

    let stats = pool.get_reinsurance_stats();
    assert_eq!(stats.total_recovered, 40_000_0000000i128);
    assert_eq!(stats.coverage_remaining, 1_000_000_0000000i128 - 40_000_0000000i128);
    assert_eq!(pool.get_stats().total_deposited, deposited_before + 40_000_0000000i128);
    assert_eq!(token::Client::new(&env, &usdc).balance(&pool.address), 40_000_0000000i128);
}

/// Recovery never exceeds the cumulative `coverage_limit`, even across
/// multiple claims.
#[test]
fn recovery_is_capped_by_coverage_limit() {
    let (env, pool, usdc, admin, claims_processor) = setup();
    let reinsurer_id = env.register(MockReinsurer, ());
    MockReinsurerClient::new(&env, &reinsurer_id).init(&usdc);
    token::StellarAssetClient::new(&env, &usdc).mint(&reinsurer_id, &1_000_000_0000000i128);

    // Small lifetime coverage limit: 15,000.
    pool.set_reinsurance(&admin, &reinsurer_id, &0i128, &15_000_0000000i128);

    let first = pool.request_reinsurance_recovery(&claims_processor, &1u128, &10_000_0000000i128);
    assert_eq!(first, 10_000_0000000i128);

    // Second claim requests 10,000 more, but only 5,000 of coverage remains.
    let second = pool.request_reinsurance_recovery(&claims_processor, &2u128, &10_000_0000000i128);
    assert_eq!(second, 5_000_0000000i128);
    assert_eq!(pool.get_reinsurance_stats().coverage_remaining, 0);

    // A third claim recovers nothing further — coverage is exhausted.
    let third = pool.request_reinsurance_recovery(&claims_processor, &3u128, &10_000_0000000i128);
    assert_eq!(third, 0);
}

/// A reinsurer whose own capacity is short pays (and reports) less than
/// requested — the pool records only what actually came back.
#[test]
fn reinsurer_short_capacity_partial_recovery_is_recorded_honestly() {
    let (env, pool, usdc, admin, claims_processor) = setup();
    let reinsurer_id = env.register(MockStingyReinsurer, ());
    MockStingyReinsurerClient::new(&env, &reinsurer_id).init(&usdc);
    token::StellarAssetClient::new(&env, &usdc).mint(&reinsurer_id, &1_000_000_0000000i128);

    pool.set_reinsurance(&admin, &reinsurer_id, &0i128, &1_000_000_0000000i128);

    let recovered = pool.request_reinsurance_recovery(&claims_processor, &1u128, &10_000_0000000i128);
    assert_eq!(recovered, 5_000_0000000i128); // stingy reinsurer only pays half
    assert_eq!(pool.get_reinsurance_stats().total_recovered, 5_000_0000000i128);
}

/// A reinsurer that reports paying more than was requested is clamped to
/// the requested amount — its self-report is never trusted past that.
#[test]
fn over_reporting_reinsurer_is_clamped_to_requested_amount() {
    let (env, pool, usdc, admin, claims_processor) = setup();
    let reinsurer_id = env.register(MockOverReportingReinsurer, ());
    MockOverReportingReinsurerClient::new(&env, &reinsurer_id).init(&usdc);
    token::StellarAssetClient::new(&env, &usdc).mint(&reinsurer_id, &1_000_000_0000000i128);

    // Coverage limit smaller than what the reinsurer would over-report,
    // so a bug that trusted the return value verbatim would blow past it.
    pool.set_reinsurance(&admin, &reinsurer_id, &0i128, &12_000_0000000i128);

    let recovered = pool.request_reinsurance_recovery(&claims_processor, &1u128, &10_000_0000000i128);
    // Requested 10,000; reinsurer reports 20,000 — clamped to 10,000.
    assert_eq!(recovered, 10_000_0000000i128);
    assert_eq!(pool.get_reinsurance_stats().total_recovered, 10_000_0000000i128);
    assert_eq!(pool.get_reinsurance_stats().coverage_remaining, 2_000_0000000i128);
}

/// Pausing the arrangement preserves its configuration and stats but
/// blocks further recoveries until it is resumed.
#[test]
fn inactive_reinsurance_recovers_nothing_but_keeps_config() {
    let (env, pool, usdc, admin, claims_processor) = setup();
    let reinsurer_id = env.register(MockReinsurer, ());
    MockReinsurerClient::new(&env, &reinsurer_id).init(&usdc);
    token::StellarAssetClient::new(&env, &usdc).mint(&reinsurer_id, &1_000_000_0000000i128);

    pool.set_reinsurance(&admin, &reinsurer_id, &0i128, &1_000_000_0000000i128);
    pool.set_reinsurance_active(&admin, &false);

    let recovered = pool.request_reinsurance_recovery(&claims_processor, &1u128, &10_000_0000000i128);
    assert_eq!(recovered, 0);
    assert!(!pool.get_reinsurance().unwrap().active);

    pool.set_reinsurance_active(&admin, &true);
    let recovered = pool.request_reinsurance_recovery(&claims_processor, &2u128, &10_000_0000000i128);
    assert_eq!(recovered, 10_000_0000000i128);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn request_reinsurance_recovery_requires_protocol_caller() {
    let (env, pool, usdc, admin, _cp) = setup();
    let reinsurer_id = env.register(MockReinsurer, ());
    MockReinsurerClient::new(&env, &reinsurer_id).init(&usdc);
    pool.set_reinsurance(&admin, &reinsurer_id, &0i128, &1_000_000_0000000i128);

    let impostor = Address::generate(&env);
    pool.request_reinsurance_recovery(&impostor, &1u128, &10_000_0000000i128);
}

// ── Issue #837: reinsurance coverage_limit boundaries & liquidation tests ────

/// When no reinsurance arrangement is configured, `request_reinsurance_recovery`
/// returns 0 without panicking and leaves pool balances and stats untouched.
#[test]
fn reinsurance_recovery_when_unconfigured_returns_zero() {
    let (env, pool, usdc, _admin, claims_processor) = setup();
    let recovered =
        pool.request_reinsurance_recovery(&claims_processor, &1u128, &50_000_0000000i128);
    assert_eq!(recovered, 0);
    assert_eq!(pool.get_reinsurance_stats().total_recovered, 0);
    assert_eq!(pool.get_reinsurance_stats().coverage_remaining, 0);
    assert_eq!(token::Client::new(&env, &usdc).balance(&pool.address), 0);
}

/// Reinsurance recovery at exact attachment point, below coverage_limit,
/// hitting coverage_limit exactly across repeated recoveries, and a single
/// claim exceeding coverage_limit.
#[test]
fn reinsurance_recovery_below_at_and_above_coverage_limit() {
    let (env, pool, usdc, admin, claims_processor) = setup();
    let reinsurer_id = env.register(MockReinsurer, ());
    MockReinsurerClient::new(&env, &reinsurer_id).init(&usdc);
    token::StellarAssetClient::new(&env, &usdc).mint(&reinsurer_id, &500_000_0000000i128);

    // attachment_point = 10,000 USDC, coverage_limit = 50,000 USDC.
    let attachment = 10_000_0000000i128;
    let limit = 50_000_0000000i128;
    pool.set_reinsurance(&admin, &reinsurer_id, &attachment, &limit);

    // Exact attachment point: loss == 10,000 -> excess = 0 -> recovers 0.
    assert_eq!(
        pool.request_reinsurance_recovery(&claims_processor, &1u128, &attachment),
        0
    );

    // Below limit: loss = 30,000 -> excess = 20,000 < 50,000 -> recovers 20,000.
    let r1 = pool.request_reinsurance_recovery(&claims_processor, &2u128, &30_000_0000000i128);
    assert_eq!(r1, 20_000_0000000i128);
    assert_eq!(pool.get_reinsurance_stats().total_recovered, 20_000_0000000i128);
    assert_eq!(
        pool.get_reinsurance_stats().coverage_remaining,
        30_000_0000000i128
    );

    // Repeated recovery hitting coverage_limit exactly:
    // loss = 40,000 -> excess = 30,000 == remaining 30,000 -> recovers 30,000.
    let r2 = pool.request_reinsurance_recovery(&claims_processor, &3u128, &40_000_0000000i128);
    assert_eq!(r2, 30_000_0000000i128);
    assert_eq!(pool.get_reinsurance_stats().total_recovered, limit);
    assert_eq!(pool.get_reinsurance_stats().coverage_remaining, 0);
    assert_eq!(
        token::Client::new(&env, &usdc).balance(&pool.address),
        limit
    );

    // Fresh pool: single claim with excess above coverage_limit (90,000 > 50,000)
    // is capped at coverage_limit (50,000).
    let (env2, pool2, usdc2, admin2, cp2) = setup();
    let reinsurer2 = env2.register(MockReinsurer, ());
    MockReinsurerClient::new(&env2, &reinsurer2).init(&usdc2);
    token::StellarAssetClient::new(&env2, &usdc2).mint(&reinsurer2, &500_000_0000000i128);
    pool2.set_reinsurance(&admin2, &reinsurer2, &attachment, &limit);

    // loss = 100,000 -> excess = 90,000, capped to coverage_limit = 50,000.
    let r_cap = pool2.request_reinsurance_recovery(&cp2, &1u128, &100_000_0000000i128);
    assert_eq!(r_cap, limit);
    assert_eq!(pool2.get_reinsurance_stats().coverage_remaining, 0);
}

/// Partial liquidation seizes shares proportional to undercollateralization
/// (`(10_000 - collateralization_bps) / 10_000`) and conserves share totals
/// and contract token balance.
#[test]
fn liquidate_position_partial_seizure_and_conservation() {
    let (env, pool, usdc, admin, _cp) = setup();
    env.ledger().set_timestamp(1_750_000_000);

    let lp1 = Address::generate(&env);
    let lp2 = Address::generate(&env);
    let sac = token::StellarAssetClient::new(&env, &usdc);
    let tok = token::Client::new(&env, &usdc);
    let deposit_amt = 100_0000000i128; // 100 USDC each
    sac.mint(&lp1, &deposit_amt);
    sac.mint(&lp2, &deposit_amt);

    let s1 = pool.deposit(&lp1, &deposit_amt, &0, &false);
    let s2 = pool.deposit(&lp2, &deposit_amt, &0, &false);
    assert_eq!(s1, deposit_amt);
    assert_eq!(s2, deposit_amt);

    // Lock 160 USDC out of 200 USDC -> available = 40 USDC.
    // For lp1 (100 of 200 shares):
    //   position_value     = 100 * 40 / 200 = 20 USDC
    //   position_liability = 100 * 160 / 200 = 80 USDC
    //   collateralization_bps = 20 * 10_000 / 80 = 2_500 (25%)
    //   undercollateral_bps   = 10_000 - 2_500 = 7_500 (75%)
    //   shares_seized    = 100_0000000 * 7_500 / 10_000 = 75_0000000
    //   amount_recovered = 75_0000000 * 200_0000000 / 200_0000000 = 75_0000000
    pool.lock_for_policy(&admin, &1u128, &160_0000000i128);

    let pool_usdc_before = tok.balance(&pool.address);
    let res = pool.liquidate_position(&admin, &lp1);
    assert_eq!(res.provider, lp1);
    assert_eq!(res.previous_ratio_bps, 2_500);
    assert_eq!(res.shares_seized, 75_0000000i128);
    assert_eq!(res.amount_recovered, 75_0000000i128);

    // Conservation checks: sum of LP shares == total_shares, USDC stays in pool.
    let lp1_after = pool.get_position(&lp1).unwrap().shares;
    let lp2_after = pool.get_position(&lp2).unwrap().shares;
    assert_eq!(lp1_after, 25_0000000i128);
    assert_eq!(lp2_after, 100_0000000i128);
    assert_eq!(lp1_after + lp2_after, pool.get_stats().total_shares);
    assert_eq!(tok.balance(&pool.address), pool_usdc_before);
}

/// Full liquidation at 0% collateralization seizes 100% of shares, removes the
/// LP position, and conserves remaining share totals and pool token balance.
#[test]
fn liquidate_position_full_seizure_removes_position_and_conserves_totals() {
    let (env, pool, usdc, admin, _cp) = setup();
    env.ledger().set_timestamp(1_750_000_000);

    let lp1 = Address::generate(&env);
    let lp2 = Address::generate(&env);
    let sac = token::StellarAssetClient::new(&env, &usdc);
    let tok = token::Client::new(&env, &usdc);
    let deposit_amt = 100_0000000i128;
    sac.mint(&lp1, &deposit_amt);
    sac.mint(&lp2, &deposit_amt);

    pool.deposit(&lp1, &deposit_amt, &0, &false);
    pool.deposit(&lp2, &deposit_amt, &0, &false);

    // Lock 100% of deposited capital (200 USDC) -> available = 0 -> ratio = 0 bps.
    // undercollateral_bps = 10_000 -> shares_seized = 100_0000000 (100%).
    pool.lock_for_policy(&admin, &1u128, &200_0000000i128);

    let res = pool.liquidate_position(&admin, &lp1);
    assert_eq!(res.previous_ratio_bps, 0);
    assert_eq!(res.shares_seized, deposit_amt);
    assert_eq!(res.amount_recovered, deposit_amt);

    assert_eq!(pool.get_position(&lp1), None);
    let lp2_after = pool.get_position(&lp2).unwrap().shares;
    assert_eq!(lp2_after, deposit_amt);
    assert_eq!(pool.get_stats().total_shares, lp2_after);
    assert_eq!(tok.balance(&pool.address), 2 * deposit_amt);
}

/// Ineligible positions (no liability, or collateralization >= 10,000 bps) are
/// rejected by `liquidate_position` with `Error::Undercollateralized` (#11).
#[test]
fn liquidate_position_rejects_ineligible_positions() {
    let (env, pool, usdc, admin, _cp) = setup();
    env.ledger().set_timestamp(1_750_000_000);

    let lp1 = Address::generate(&env);
    let lp2 = Address::generate(&env);
    let sac = token::StellarAssetClient::new(&env, &usdc);
    sac.mint(&lp1, &100_0000000i128);
    sac.mint(&lp2, &100_0000000i128);

    pool.deposit(&lp1, &100_0000000i128, &0, &false);
    pool.deposit(&lp2, &100_0000000i128, &0, &false);

    // 1. No locked capital -> position_liability == 0 -> rejected (#11).
    assert_eq!(
        pool.try_liquidate_position(&admin, &lp1),
        Err(Ok(Error::Undercollateralized))
    );

    // 2. Lock 50% (100 of 200 USDC): available = 100, locked = 100 ->
    // collateralization_bps == 10_000 (100%, exact boundary) -> rejected (#11).
    pool.lock_for_policy(&admin, &1u128, &100_0000000i128);
    assert_eq!(
        pool.try_liquidate_position(&admin, &lp1),
        Err(Ok(Error::Undercollateralized))
    );
}

/// Unauthorized caller attempting `liquidate_position` without blanket auth
/// mocking is rejected with `Error::Unauthorized` (#3).
#[test]
fn liquidate_position_rejects_unauthorized_caller_without_blanket_auth() {
    let (env, pool, usdc, admin, _cp) = setup();
    env.ledger().set_timestamp(1_750_000_000);

    let lp1 = Address::generate(&env);
    let outsider = Address::generate(&env);
    token::StellarAssetClient::new(&env, &usdc).mint(&lp1, &100_0000000i128);
    pool.deposit(&lp1, &100_0000000i128, &0, &false);
    pool.lock_for_policy(&admin, &1u128, &80_0000000i128);

    // Replace blanket auth mocking with explicit auth only for `outsider`.
    env.mock_auths(&[MockAuth {
        address: &outsider,
        invoke: &MockAuthInvoke {
            contract: &pool.address,
            fn_name: "liquidate_position",
            args: (&outsider, &lp1).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    let res = pool.try_liquidate_position(&outsider, &lp1);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
    assert_eq!(pool.get_position(&lp1).unwrap().shares, 100_0000000i128);
}

/// Liquidating a position that has a pending exit reservation seizes the
/// proportional shares and preserves share/token conservation.
#[test]
fn liquidate_position_with_pending_exit_reservation_seizes_shares() {
    let (env, pool, usdc, admin, _cp) = setup();
    env.ledger().set_timestamp(1_750_000_000);
    pool.set_exit_delay(&admin, &3_600u64);

    let lp1 = Address::generate(&env);
    let lp2 = Address::generate(&env);
    let sac = token::StellarAssetClient::new(&env, &usdc);
    let tok = token::Client::new(&env, &usdc);
    sac.mint(&lp1, &100_0000000i128);
    sac.mint(&lp2, &100_0000000i128);

    pool.deposit(&lp1, &100_0000000i128, &0, &false);
    pool.deposit(&lp2, &100_0000000i128, &0, &false);
    pool.request_exit(&lp1, &50_0000000i128);

    // Lock 160 of 200 USDC -> collateralization_bps = 2_500 -> seize 75% (75 shares).
    pool.lock_for_policy(&admin, &1u128, &160_0000000i128);

    let res = pool.liquidate_position(&admin, &lp1);
    assert_eq!(res.shares_seized, 75_0000000i128);
    assert_eq!(pool.get_position(&lp1).unwrap().shares, 25_0000000i128);
    assert_eq!(pool.get_stats().total_shares, 125_0000000i128);
    assert_eq!(tok.balance(&pool.address), 200_0000000i128);
}

/// Suspected bug: `liquidate_position` does not adjust or clear the provider's
/// pending `ExitReq` or `QueuedExitShares`, leaving the queued reservation
/// larger than the provider's remaining shares.
#[test]
#[ignore = "Suspected bug: liquidate_position leaves stale ExitReq and QueuedExitShares unadjusted"]
fn liquidate_position_reconciles_pending_exit_reservation() {
    let (env, pool, usdc, admin, _cp) = setup();
    env.ledger().set_timestamp(1_750_000_000);
    pool.set_exit_delay(&admin, &3_600u64);

    let lp1 = Address::generate(&env);
    let lp2 = Address::generate(&env);
    let sac = token::StellarAssetClient::new(&env, &usdc);
    sac.mint(&lp1, &100_0000000i128);
    sac.mint(&lp2, &100_0000000i128);

    pool.deposit(&lp1, &100_0000000i128, &0, &false);
    pool.deposit(&lp2, &100_0000000i128, &0, &false);
    pool.request_exit(&lp1, &50_0000000i128);
    pool.lock_for_policy(&admin, &1u128, &160_0000000i128);

    pool.liquidate_position(&admin, &lp1);
    let remaining = pool.get_position(&lp1).unwrap().shares; // 25_0000000
    assert!(pool.get_exit_info(&lp1).shares <= remaining);
    assert!(pool.get_queued_exit_shares() <= remaining);
}
