#![allow(clippy::inconsistent_digit_grouping)]
//! Reinsurance tests (issue #382): configuring an external reinsurance
//! arrangement, ceding premium to it, and recovering catastrophic claim
//! losses from it.

#![cfg(test)]

extern crate std;

use soroban_sdk::{testutils::Address as _, token, Address, Env, Symbol};

use crate::{RiskPool, RiskPoolClient};

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
