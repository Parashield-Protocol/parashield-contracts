#![allow(clippy::inconsistent_digit_grouping)]
//! Extended integration tests for the Claims Processor.
//! Covers batch processing, edge cases, and cross-contract error propagation.
#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Ledger as _}, // <-- Imported the Ledger trait for .with_mut() tracking
    token, Address, Env,
};

use parashield_oracle_verifier::{OracleVerifier, OracleVerifierClient};
use parashield_policy_engine::{
    CreateProductParams, PolicyEngine, PolicyEngineClient,
    TriggerComparison, TriggerType,
};
use crate::{ClaimsProcessor, ClaimsProcessorClient, ClaimResult};

// ── helpers ───────────────────────────────────────────────────────────────────

use soroban_sdk::{symbol_short, Symbol};

fn weather() -> Symbol { symbol_short!("weather") }

struct TestEnv {
    env:       Env,
    oracle:    Address,
    policy:    Address,
    claims:    Address,
    admin:     Address,
    usdc:      Address,
    oracle_node: Address,
}

fn full_setup() -> TestEnv {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let oracle_node = Address::generate(&env);

    let usdc_id = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let oracle_id = env.register(OracleVerifier, ());
    let policy_id = env.register(PolicyEngine, ());
    let claims_id = env.register(ClaimsProcessor, ());

    OracleVerifierClient::new(&env, &oracle_id).initialize(&admin);
    PolicyEngineClient::new(&env, &policy_id)
        .initialize(&admin, &usdc_id, &oracle_id);
    ClaimsProcessorClient::new(&env, &claims_id)
        .initialize(&admin, &policy_id, &oracle_id, &604_800u64);
    PolicyEngineClient::new(&env, &policy_id)
        .set_claims_processor(&admin, &claims_id);

    TestEnv { env, oracle: oracle_id, policy: policy_id, claims: claims_id, admin, usdc: usdc_id, oracle_node }
}

fn create_drought_product(te: &TestEnv) -> u128 {
    PolicyEngineClient::new(&te.env, &te.policy).create_product(
        &te.admin,
        &CreateProductParams {
            name:               symbol_short!("drght"),
            category:           symbol_short!("crop"),
            trigger_type:       TriggerType::Threshold,
            oracle_data_type:   weather(),
            trigger_threshold:  500_000_000i128,
            trigger_comparison: TriggerComparison::LessThan,
            coverage_min:       1_000_0000000i128,
            coverage_max:       100_000_0000000i128,
            premium_rate_bps:   300u32,
            max_duration_days:  180u32,
        },
    )
}

// ── batch_auto_process tests ──────────────────────────────────────────────────

#[test]
fn batch_processes_multiple_pending_claims() {
    let te = full_setup();
    let claims_client = ClaimsProcessorClient::new(&te.env, &te.claims);
    let policy_client = PolicyEngineClient::new(&te.env, &te.policy);
    let oracle_client = OracleVerifierClient::new(&te.env, &te.oracle);

    let prod_id = create_drought_product(&te);

    oracle_client.add_oracle(&te.admin, &te.oracle_node, &weather(), &90u32);
    oracle_client.submit_data(
        &te.oracle_node, &weather(), &symbol_short!("kis2606"),
        &30_000_000i128, &95u32, &1_748_736_000u64,
    );

    let farmer1 = Address::generate(&te.env);
    let farmer2 = Address::generate(&te.env);
    token::StellarAssetClient::new(&te.env, &te.usdc).mint(&farmer1, &10_000_0000000i128);
    token::StellarAssetClient::new(&te.env, &te.usdc).mint(&farmer2, &10_000_0000000i128);
    token::StellarAssetClient::new(&te.env, &te.usdc).mint(
        &te.policy, &1_000_000_0000000i128,
    );

    let p1 = policy_client.buy_policy(
        &farmer1, &prod_id, &1_000_0000000i128, &30u32, &symbol_short!("kis2606"),
    );
    let p2 = policy_client.buy_policy(
        &farmer2, &prod_id, &2_000_0000000i128, &30u32, &symbol_short!("kis2606"),
    );

    claims_client.submit_claim(&farmer1, &p1);
    claims_client.submit_claim(&farmer2, &p2);

    let results = claims_client.batch_auto_process(&te.admin, &10u32);
    assert_eq!(results.len(), 2);
    // Both should trigger (rainfall 30mm < 50mm threshold)
    for i in 0..results.len() {
        let (_cid, result) = results.get_unchecked(i);
        assert_eq!(result, ClaimResult::Paid);
    }
}

#[test]
fn batch_skips_non_pending_claims() {
    let te = full_setup();
    let claims_client = ClaimsProcessorClient::new(&te.env, &te.claims);
    let policy_client = PolicyEngineClient::new(&te.env, &te.policy);
    let oracle_client = OracleVerifierClient::new(&te.env, &te.oracle);

    let prod_id = create_drought_product(&te);

    oracle_client.add_oracle(&te.admin, &te.oracle_node, &weather(), &90u32);
    oracle_client.submit_data(
        &te.oracle_node, &weather(), &symbol_short!("kis2606"),
        &30_000_000i128, &95u32, &1_748_736_000u64,
    );

    let farmer = Address::generate(&te.env);
    token::StellarAssetClient::new(&te.env, &te.usdc).mint(&farmer, &10_000_0000000i128);
    token::StellarAssetClient::new(&te.env, &te.usdc).mint(&te.policy, &1_000_000_0000000i128);

    let p1 = policy_client.buy_policy(
        &farmer, &prod_id, &1_000_0000000i128, &30u32, &symbol_short!("kis2606"),
    );
    let _claim_id = claims_client.submit_claim(&farmer, &p1);
    // Process it once
    claims_client.auto_process(&te.admin, &p1);
    // Batch with limit=10 should return 0 results (already processed)
    let results = claims_client.batch_auto_process(&te.admin, &10u32);
    assert_eq!(results.len(), 0);
}

// ── Oracle staleness rejection (Issue #3) ─────────────────────────────────────

/// auto_process must panic when oracle data is older than the staleness threshold.
/// Verifies that verify_trigger_fresh rejects stale data before any state write.
#[test]
#[should_panic]
fn test_stale_oracle_data_rejected() {
    let te = full_setup();
    let claims_client = ClaimsProcessorClient::new(&te.env, &te.claims);
    let policy_client = PolicyEngineClient::new(&te.env, &te.policy);
    let oracle_client = OracleVerifierClient::new(&te.env, &te.oracle);

    let prod_id = create_drought_product(&te);

    oracle_client.add_oracle(&te.admin, &te.oracle_node, &weather(), &90u32);

    // Submit oracle data with an old timestamp (well within a recognisable epoch)
    let data_ts: u64 = 1_000_000;
    oracle_client.submit_data(
        &te.oracle_node, &weather(), &symbol_short!("kis2606"),
        &30_000_000i128, &95u32, &data_ts,
    );

    let farmer = Address::generate(&te.env);
    token::StellarAssetClient::new(&te.env, &te.usdc).mint(&farmer, &10_000_0000000i128);
    token::StellarAssetClient::new(&te.env, &te.usdc).mint(&te.policy, &1_000_000_0000000i128);

    let pol_id = policy_client.buy_policy(
        &farmer, &prod_id, &1_000_0000000i128, &30u32, &symbol_short!("kis2606"),
    );

    // Advance ledger to more than staleness_threshold (604_800 s) past the data timestamp
    let stale_now = data_ts + 604_800 + 1;
    te.env.ledger().with_mut(|l| l.timestamp = stale_now);

    // auto_process must panic — StaleData error from oracle-verifier
    claims_client.auto_process(&te.admin, &pol_id);
}

/// auto_process succeeds when oracle data is within the staleness threshold.
#[test]
fn test_fresh_oracle_data_accepted() {
    let te = full_setup();
    let claims_client = ClaimsProcessorClient::new(&te.env, &te.claims);
    let policy_client = PolicyEngineClient::new(&te.env, &te.policy);
    let oracle_client = OracleVerifierClient::new(&te.env, &te.oracle);

    let prod_id = create_drought_product(&te);

    oracle_client.add_oracle(&te.admin, &te.oracle_node, &weather(), &90u32);

    // Submit oracle data at timestamp 1_000_000
    let data_ts: u64 = 1_000_000;
    oracle_client.submit_data(
        &te.oracle_node, &weather(), &symbol_short!("kis2606"),
        &30_000_000i128, &95u32, &data_ts,
    );

    let farmer = Address::generate(&te.env);
    token::StellarAssetClient::new(&te.env, &te.usdc).mint(&farmer, &10_000_0000000i128);
    token::StellarAssetClient::new(&te.env, &te.usdc).mint(&te.policy, &1_000_000_0000000i128);

    let pol_id = policy_client.buy_policy(
        &farmer, &prod_id, &1_000_0000000i128, &30u32, &symbol_short!("kis2606"),
    );

    // Set ledger to within the staleness threshold (data is fresh)
    let fresh_now = data_ts + 3_600; // 1 hour after data timestamp
    te.env.ledger().with_mut(|l| l.timestamp = fresh_now);

    // Should succeed and pay out (30mm < 500mm threshold triggers drought product)
    let result = claims_client.auto_process(&te.admin, &pol_id);
    assert_eq!(result, ClaimResult::Paid);
}