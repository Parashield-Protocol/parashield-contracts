//! Product name uniqueness (issue #514) and the lifetime of the name index
//! (issue #570).
//!
//! `create_product` refuses a name that is already mapped to a live product.
//! For that index to mean "the name is taken by a product users can actually
//! buy" rather than "the name was once typed into this contract", retiring a
//! product has to release its name — the same way it already releases its
//! `(category, oracle_key)` slot.
#![cfg(test)]

use super::*;
use soroban_sdk::{symbol_short, testutils::Address as _, Env, Symbol};

fn setup() -> (Env, Address, PolicyEngineClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let usdc = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let id = env.register(PolicyEngine, ());
    let client = PolicyEngineClient::new(&env, &id);
    client.initialize(&admin, &usdc, &Address::generate(&env));
    (env, admin, client)
}

/// Unwrap a `try_*` client call that is expected to succeed.
///
/// The error types are spelled out rather than left generic because `expect`
/// needs them to be `Debug`, and because every generated client method returns
/// exactly these two.
fn expect_ok<T>(
    res: Result<
        Result<T, soroban_sdk::Error>,
        Result<soroban_sdk::Error, soroban_sdk::InvokeError>,
    >,
) -> T {
    res.expect("expected the call to succeed")
        .expect("expected the call to succeed without raising a contract error")
}

/// Assert a `try_*` client call fails, without pinning which error it raises.
fn expect_rejected<T>(
    res: Result<
        Result<T, soroban_sdk::Error>,
        Result<soroban_sdk::Error, soroban_sdk::InvokeError>,
    >,
) {
    assert!(res.is_err(), "expected the call to be rejected");
}

fn params(name: Symbol, category: Symbol, oracle_key: Symbol) -> CreateProductParams {
    CreateProductParams {
        name,
        category,
        oracle_key,
        trigger_type: TriggerType::Threshold,
        oracle_data_type: symbol_short!("weather"),
        trigger_threshold: 50_000_000,
        trigger_comparison: TriggerComparison::LessThan,
        coverage_min: 100_000_000,
        coverage_max: 10_000_000_000,
        premium_rate_bps: 500,
        max_duration_days: 365,
    }
}

#[test]
fn distinct_names_are_accepted() {
    let (_, admin, client) = setup();
    let a = client.create_product(&admin, &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")));
    let b = client.create_product(&admin, &params(symbol_short!("drought"), symbol_short!("crop"), symbol_short!("key_b")));
    assert_ne!(a, b);
}

#[test]
#[should_panic(expected = "Error(Contract, #34)")]
fn duplicate_name_same_category_is_rejected() {
    let (_, admin, client) = setup();
    client.create_product(&admin, &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")));
    client.create_product(&admin, &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_b")));
}

#[test]
#[should_panic(expected = "Error(Contract, #34)")]
fn duplicate_name_across_categories_is_rejected() {
    let (_, admin, client) = setup();
    client.create_product(&admin, &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")));
    client.create_product(&admin, &params(symbol_short!("rain"), symbol_short!("flight"), symbol_short!("key_b")));
}

#[test]
fn rejected_duplicate_does_not_consume_a_slot_or_id() {
    let (_, admin, client) = setup();
    let first = client.create_product(&admin, &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")));
    let dup = client.try_create_product(&admin, &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_b")));
    assert!(dup.is_err());
    // The failed attempt must not have reserved the oracle key or an id.
    let next = client.create_product(&admin, &params(symbol_short!("hail"), symbol_short!("crop"), symbol_short!("key_b")));
    assert_eq!(next, first + 1);
}

// ── Name index lifetime (issue #570) ───────────────────────────────────────

/// Retiring a product frees its name, so the successor can be launched under
/// it. Without the release, `create_product`'s uniqueness check rejects the
/// name forever and the product line can never be re-launched.
#[test]
fn deprecating_a_product_frees_its_name() {
    let (_, admin, client) = setup();
    let original = client.create_product(
        &admin,
        &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")),
    );
    client.deprecate_product(&admin, &original);

    let successor = expect_ok(client.try_create_product(
        &admin,
        &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")),
    ));
    assert_ne!(successor, original, "a successor must be a new product id");
}

/// The name *and* the `(category, oracle_key)` slot are both released, so a
/// successor can reuse the whole identity of the retired product rather than
/// just half of it.
#[test]
fn deprecating_a_product_frees_name_and_oracle_key() {
    let (_, admin, client) = setup();
    let original = client.create_product(
        &admin,
        &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")),
    );
    client.deprecate_product(&admin, &original);

    // Same name *and* same (category, oracle_key): the slot is free again.
    let successor = expect_ok(client.try_create_product(
        &admin,
        &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")),
    ));
    assert_ne!(successor, original);
}

/// Releasing a name must not touch a *different* product's name mapping. The
/// removal is keyed on the product id, so deprecating one product can only ever
/// clear the entry that still points at that product.
#[test]
fn deprecating_one_product_does_not_clear_another_products_name() {
    let (_, admin, client) = setup();
    let rain = client.create_product(
        &admin,
        &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")),
    );
    let hail = client.create_product(
        &admin,
        &params(symbol_short!("hail"), symbol_short!("crop"), symbol_short!("key_b")),
    );

    client.deprecate_product(&admin, &rain);

    // `hail` is untouched and still holds its name, so the name is refused.
    expect_rejected(client.try_create_product(
        &admin,
        &params(symbol_short!("hail"), symbol_short!("crop"), symbol_short!("key_c")),
    ));
    // And the live product is still addressable under its own name.
    let replacement = expect_ok(client.try_create_product(
        &admin,
        &params(symbol_short!("drought"), symbol_short!("crop"), symbol_short!("key_c")),
    ));
    assert!(replacement > hail);
}

/// A name is released exactly once: the successor takes it over, and the
/// uniqueness invariant is intact again from that point — a third product
/// under the same name is refused.
#[test]
fn name_is_reusable_exactly_once_after_deprecation() {
    let (_, admin, client) = setup();
    let original = client.create_product(
        &admin,
        &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")),
    );
    client.deprecate_product(&admin, &original);

    let successor = expect_ok(client.try_create_product(
        &admin,
        &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")),
    ));

    // The name now belongs to the successor, so a third claim on it fails and
    // the successor keeps it.
    expect_rejected(client.try_create_product(
        &admin,
        &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_z")),
    ));
    let live = client.get_product(&successor);
    assert_eq!(
        live.status,
        ProductStatus::Active,
        "the successor must be the live owner of the re-registered name"
    );
}

/// Pausing is a reversible operational state, not a retirement, so the name
/// stays reserved. This is the boundary between the two: only
/// `deprecate_product` frees a name.
#[test]
fn pausing_a_product_keeps_its_name_reserved() {
    let (_, admin, client) = setup();
    let original = client.create_product(
        &admin,
        &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_a")),
    );
    client.pause_product(&admin, &original);

    expect_rejected(client.try_create_product(
        &admin,
        &params(symbol_short!("rain"), symbol_short!("crop"), symbol_short!("key_b")),
    ));
}
