//! O(1) per-product statistics counters and idempotent backfill (issue #821).
#![cfg(test)]

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Bytes, Env, Symbol,
};

const START_TS: u64 = 1_748_736_000;
const COVERAGE: i128 = 1_000_000_000; // 100 USDC

struct StatsFixture<'a> {
    admin: Address,
    claims_processor: Address,
    usdc: Address,
    contract_id: Address,
    client: PolicyEngineClient<'a>,
}

fn setup(env: &Env) -> StatsFixture<'_> {
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = START_TS);

    let admin = Address::generate(env);
    let claims_processor = Address::generate(env);
    let oracle = Address::generate(env);
    let usdc = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let contract_id = env.register(PolicyEngine, ());
    let client = PolicyEngineClient::new(env, &contract_id);
    client.initialize(&admin, &usdc, &oracle);
    client.set_claims_processor(&admin, &claims_processor);

    // Fund contract so claim payouts succeed.
    StellarAssetClient::new(env, &usdc).mint(&contract_id, &100_000_000_000i128);

    StatsFixture {
        admin,
        claims_processor,
        usdc,
        contract_id,
        client,
    }
}

fn create_product(
    f: &StatsFixture,
    name: Symbol,
    oracle_key: Symbol,
    rate_bps: u32,
) -> u128 {
    f.client.create_product(
        &f.admin,
        &CreateProductParams {
            name,
            category: symbol_short!("crop"),
            oracle_key,
            expected_token: f.usdc.clone(),
            trigger_type: TriggerType::Threshold,
            oracle_data_type: symbol_short!("weather"),
            trigger_threshold: 50_000_000,
            trigger_comparison: TriggerComparison::LessThan,
            coverage_min: 100_000_000,
            coverage_max: 10_000_000_000,
            premium_rate_bps: rate_bps,
            max_duration_days: 365,
        },
    )
}

fn funded_buyer(env: &Env, usdc: &Address) -> Address {
    let buyer = Address::generate(env);
    StellarAssetClient::new(env, usdc).mint(&buyer, &50_000_000_000i128);
    buyer
}

/// Reference brute-force scan over `1..NextPolicyId` used as the oracle for
/// testing counter equality.
fn brute_force_product_stats(env: &Env, contract_id: &Address, product_id: u128) -> ProductStats {
    env.as_contract(contract_id, || {
        if !env
            .storage()
            .persistent()
            .has(&StorageKey::Product(product_id))
        {
            return ProductStats {
                product_id,
                total_policies: 0,
                active_policies: 0,
                total_coverage: 0,
                total_premium_collected: 0,
            };
        }

        let mut total_policies: u32 = 0;
        let mut active_policies: u32 = 0;
        let mut total_coverage: i128 = 0;
        let mut total_premium_collected: i128 = 0;

        let next_id: u128 = env
            .storage()
            .instance()
            .get(&StorageKey::NextPolicyId)
            .unwrap_or(1);

        for pid in 1..next_id {
            if let Some(policy) = env
                .storage()
                .persistent()
                .get::<_, Policy>(&StorageKey::Policy(pid))
            {
                if policy.product_id == product_id {
                    total_policies += 1;
                    if policy.status == PolicyStatus::Active {
                        active_policies += 1;
                    }
                    total_coverage = total_coverage.saturating_add(policy.coverage_amount);
                    total_premium_collected =
                        total_premium_collected.saturating_add(policy.premium_paid);
                }
            }
        }

        ProductStats {
            product_id,
            total_policies,
            active_policies,
            total_coverage,
            total_premium_collected,
        }
    })
}

fn contract_error<T, E>(
    res: Result<Result<T, E>, Result<soroban_sdk::Error, soroban_sdk::InvokeError>>,
) -> Option<soroban_sdk::Error> {
    res.err().and_then(|inner| inner.ok())
}

#[test]
fn stats_match_brute_force_across_full_lifecycle() {
    let env = Env::default();
    let f = setup(&env);

    let prod_a = create_product(&f, symbol_short!("rain_a"), symbol_short!("kis2601"), 500);
    let prod_b = create_product(&f, symbol_short!("rain_b"), symbol_short!("kis2602"), 800);

    let b1 = funded_buyer(&env, &f.usdc);
    let b2 = funded_buyer(&env, &f.usdc);
    let b3 = funded_buyer(&env, &f.usdc);
    let b4 = funded_buyer(&env, &f.usdc);
    let b5 = funded_buyer(&env, &f.usdc);

    // Create policies across both products.
    let p1 = f.client.buy_policy(&b1, &prod_a, &COVERAGE, &30, &symbol_short!("kis2601"), &f.usdc);
    let p2 = f.client.buy_policy(&b2, &prod_a, &(COVERAGE * 2), &60, &symbol_short!("kis2601"), &f.usdc);
    let p3 = f.client.buy_policy(&b3, &prod_a, &(COVERAGE * 3), &90, &symbol_short!("kis2601"), &f.usdc);
    let p4 = f.client.buy_policy(&b4, &prod_a, &COVERAGE, &45, &symbol_short!("kis2601"), &f.usdc);
    let _p5 = f.client.buy_policy(&b5, &prod_b, &(COVERAGE * 4), &30, &symbol_short!("kis2602"), &f.usdc);

    assert_eq!(
        f.client.get_product_stats(&prod_a),
        brute_force_product_stats(&env, &f.contract_id, prod_a)
    );
    assert_eq!(
        f.client.get_product_stats(&prod_b),
        brute_force_product_stats(&env, &f.contract_id, prod_b)
    );

    // Cancel p1.
    f.client.cancel_policy(&b1, &p1, &Bytes::from_slice(&env, b"changed mind"));
    assert_eq!(
        f.client.get_product_stats(&prod_a),
        brute_force_product_stats(&env, &f.contract_id, prod_a)
    );

    // Expire p2.
    f.client.expire_policy(&f.claims_processor, &p2);
    assert_eq!(
        f.client.get_product_stats(&prod_a),
        brute_force_product_stats(&env, &f.contract_id, prod_a)
    );

    // Claim p3.
    f.client.pay_claim(&f.claims_processor, &p3);
    assert_eq!(
        f.client.get_product_stats(&prod_a),
        brute_force_product_stats(&env, &f.contract_id, prod_a)
    );

    // Transfer p4 (status remains Active, product unchanged).
    let b6 = Address::generate(&env);
    f.client.transfer_policy(&b4, &b6, &p4);
    assert_eq!(
        f.client.get_product_stats(&prod_a),
        brute_force_product_stats(&env, &f.contract_id, prod_a)
    );

    // Deprecate prod_a — stats must remain intact and match brute force.
    f.client.deprecate_product(&f.admin, &prod_a);
    let stats_a = f.client.get_product_stats(&prod_a);
    assert_eq!(stats_a, brute_force_product_stats(&env, &f.contract_id, prod_a));
    assert_eq!(stats_a.total_policies, 4);
    assert_eq!(stats_a.active_policies, 1);
}

#[test]
fn stats_for_empty_and_nonexistent_and_deprecated_products() {
    let env = Env::default();
    let f = setup(&env);

    // Non-existent product returns zeros.
    let missing = f.client.get_product_stats(&999);
    assert_eq!(
        missing,
        ProductStats {
            product_id: 999,
            total_policies: 0,
            active_policies: 0,
            total_coverage: 0,
            total_premium_collected: 0,
        }
    );

    // Product with no policies returns zeros.
    let prod = create_product(&f, symbol_short!("empty_p"), symbol_short!("emp2601"), 500);
    let empty_stats = f.client.get_product_stats(&prod);
    assert_eq!(empty_stats.total_policies, 0);
    assert_eq!(empty_stats.active_policies, 0);
    assert_eq!(empty_stats.total_coverage, 0);
    assert_eq!(empty_stats.total_premium_collected, 0);

    // Deprecated product with no policies still returns zeros.
    f.client.deprecate_product(&f.admin, &prod);
    assert_eq!(f.client.get_product_stats(&prod), empty_stats);
}

#[test]
fn stats_handles_sparse_ids_and_constant_budget_cost() {
    let env = Env::default();
    let f = setup(&env);

    let prod_a = create_product(&f, symbol_short!("sparse_a"), symbol_short!("spa2601"), 500);
    let prod_b = create_product(&f, symbol_short!("sparse_b"), symbol_short!("spb2601"), 500);

    let b0 = funded_buyer(&env, &f.usdc);
    f.client.buy_policy(&b0, &prod_a, &COVERAGE, &30, &symbol_short!("spa2601"), &f.usdc);

    // Measure CPU cost of get_product_stats with 1 policy.
    env.cost_estimate().budget().reset_unlimited();
    let cpu_start_small = env.cost_estimate().budget().cpu_instruction_cost();
    let stats_small = f.client.get_product_stats(&prod_a);
    let cpu_small = env.cost_estimate().budget().cpu_instruction_cost() - cpu_start_small;
    assert_eq!(stats_small.total_policies, 1);

    // Create many policies for prod_b and simulate sparse gaps in NextPolicyId.
    env.cost_estimate().budget().reset_unlimited();
    for _ in 0..40 {
        let buyer = funded_buyer(&env, &f.usdc);
        f.client.buy_policy(&buyer, &prod_b, &COVERAGE, &30, &symbol_short!("spb2601"), &f.usdc);
    }
    // Jump NextPolicyId forward to simulate thousands of sparse IDs.
    env.as_contract(&f.contract_id, || {
        env.storage().instance().set(&StorageKey::NextPolicyId, &5_000u128);
    });

    // Measure CPU cost of get_product_stats with NextPolicyId == 5_000.
    env.cost_estimate().budget().reset_unlimited();
    let cpu_start_large = env.cost_estimate().budget().cpu_instruction_cost();
    let stats_large = f.client.get_product_stats(&prod_a);
    let cpu_large = env.cost_estimate().budget().cpu_instruction_cost() - cpu_start_large;

    assert_eq!(stats_large, stats_small);
    // O(1) lookup: CPU cost must stay essentially constant regardless of NextPolicyId.
    assert!(
        cpu_large <= cpu_small.saturating_mul(2),
        "get_product_stats cost must not grow with NextPolicyId (small={}, large={})",
        cpu_small,
        cpu_large
    );
}

#[test]
fn backfill_migrates_legacy_policies_paginated_and_is_idempotent() {
    let env = Env::default();
    let f = setup(&env);

    let prod = create_product(&f, symbol_short!("legacy_p"), symbol_short!("leg2601"), 500);
    let holder = Address::generate(&env);

    // Inject 5 legacy policies directly into storage (simulating pre-upgrade state
    // where ProductStats and PolicyStatsCounted do not exist), with a sparse gap at ID 4.
    let statuses = [
        (1u128, PolicyStatus::Active),
        (2u128, PolicyStatus::Cancelled),
        (3u128, PolicyStatus::Claimed),
        (5u128, PolicyStatus::Expired),
        (6u128, PolicyStatus::Active),
    ];
    env.as_contract(&f.contract_id, || {
        for (pid, status) in statuses.iter() {
            let policy = Policy {
                id: *pid,
                product_id: prod,
                policyholder: holder.clone(),
                coverage_amount: COVERAGE,
                premium_paid: 4_109_589,
                oracle_key: symbol_short!("leg2601"),
                oracle_data_type: symbol_short!("weather"),
                trigger_threshold: 50_000_000,
                trigger_comparison: TriggerComparison::LessThan,
                start_time: START_TS,
                end_time: START_TS + 30 * 86_400,
                status: status.clone(),
                created_at: START_TS,
            };
            env.storage().persistent().set(&StorageKey::Policy(*pid), &policy);
        }
        env.storage().instance().set(&StorageKey::NextPolicyId, &7u128);
    });

    // Before backfill, counter is absent (returns zeros).
    assert_eq!(f.client.get_product_stats(&prod).total_policies, 0);

    // Paginated backfill in batches of 3.
    let n1 = f.client.backfill_product_stats(&f.admin, &1u128, &3u32);
    assert_eq!(n1, 3);
    let n2 = f.client.backfill_product_stats(&f.admin, &4u128, &3u32);
    assert_eq!(n2, 2); // ID 4 is a sparse gap; IDs 5 and 6 exist.

    let expected = brute_force_product_stats(&env, &f.contract_id, prod);
    assert_eq!(f.client.get_product_stats(&prod), expected);
    assert_eq!(expected.total_policies, 5);
    assert_eq!(expected.active_policies, 2);

    // Running backfill again over the full range must be a no-op (strictly idempotent).
    let n_repeat = f.client.backfill_product_stats(&f.admin, &1u128, &50u32);
    assert_eq!(n_repeat, 0);
    assert_eq!(f.client.get_product_stats(&prod), expected);

    // Buying a new policy after backfill and re-running backfill must not double-count it.
    let buyer = funded_buyer(&env, &f.usdc);
    f.client.buy_policy(&buyer, &prod, &COVERAGE, &30, &symbol_short!("leg2601"), &f.usdc);
    let n_after_buy = f.client.backfill_product_stats(&f.admin, &1u128, &50u32);
    assert_eq!(n_after_buy, 0);
    assert_eq!(
        f.client.get_product_stats(&prod),
        brute_force_product_stats(&env, &f.contract_id, prod)
    );
}

#[test]
fn backfill_rejects_non_admin() {
    let env = Env::default();
    let f = setup(&env);

    let stranger = Address::generate(&env);
    let res = f.client.try_backfill_product_stats(&stranger, &1u128, &10u32);
    assert_eq!(
        contract_error(res),
        Some(soroban_sdk::Error::from_contract_error(Error::Unauthorized as u32))
    );
}

#[test]
fn counters_never_underflow_when_legacy_policy_deactivated_before_backfill() {
    let env = Env::default();
    let f = setup(&env);

    let prod = create_product(&f, symbol_short!("undef_p"), symbol_short!("und2601"), 500);
    let holder = Address::generate(&env);

    // Inject an active legacy policy without ProductStats or PolicyStatsCounted.
    env.as_contract(&f.contract_id, || {
        let policy = Policy {
            id: 1,
            product_id: prod,
            policyholder: holder.clone(),
            coverage_amount: COVERAGE,
            premium_paid: 4_109_589,
            oracle_key: symbol_short!("und2601"),
            oracle_data_type: symbol_short!("weather"),
            trigger_threshold: 50_000_000,
            trigger_comparison: TriggerComparison::LessThan,
            start_time: START_TS,
            end_time: START_TS + 30 * 86_400,
            status: PolicyStatus::Active,
            created_at: START_TS,
        };
        env.storage().persistent().set(&StorageKey::Policy(1), &policy);
        env.storage().instance().set(&StorageKey::NextPolicyId, &2u128);
    });

    // Deactivating before backfill must not panic or underflow active_policies.
    f.client.expire_policy(&f.claims_processor, &1u128);
    assert_eq!(f.client.get_product_stats(&prod).active_policies, 0);

    // Subsequent backfill records the expired policy with active_policies == 0.
    let backfilled = f.client.backfill_product_stats(&f.admin, &1u128, &10u32);
    assert_eq!(backfilled, 1);

    let stats = f.client.get_product_stats(&prod);
    assert_eq!(stats, brute_force_product_stats(&env, &f.contract_id, prod));
    assert_eq!(stats.total_policies, 1);
    assert_eq!(stats.active_policies, 0);
}
