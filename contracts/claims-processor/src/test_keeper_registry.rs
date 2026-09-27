//! Keeper registry — address-format validation on `add_keeper` (issue #572).
//!
//! `add_keeper` is the only way to mint settlement authority, so it now
//! applies the same `validate_stellar_address` check that `initialize`
//! applies to the four addresses it wires up. The format rule itself lives in
//! the pure helper `is_well_formed_strkey`, which is what these tests drive
//! directly: a real `Address` is always canonically encoded by the host, so
//! the string-level rule is only reachable — and therefore only regression-
//! testable — as a pure function.
#![cfg(test)]

extern crate alloc;

use super::*;
use alloc::vec::Vec;
use soroban_sdk::testutils::Address as _;

/// The contract error a `try_*` client call raised, or `None` if it did not
/// fail with one.
///
/// Returning `Option` rather than unwrapping keeps the failure path in the
/// caller's `assert_eq!`, which reports the actual error code on mismatch and
/// needs no `panic!` of its own.
fn contract_error<T, E>(
    res: Result<Result<T, E>, Result<soroban_sdk::Error, soroban_sdk::InvokeError>>,
) -> Option<soroban_sdk::Error> {
    res.err().and_then(|inner| inner.ok())
}

/// A 56-byte `StrKey` whose body cycles through the full base32 alphabet
/// (`A`–`Z`, `2`–`7`) under a leading `G`, so the alphabet boundary itself is
/// exercised rather than just the happy path.
fn full_alphabet_strkey() -> Vec<u8> {
    let mut alphabet = Vec::new();
    for b in b'A'..=b'Z' {
        alphabet.push(b);
    }
    for b in b'2'..=b'7' {
        alphabet.push(b);
    }
    assert_eq!(alphabet.len(), 32);
    assert!(
        STRKEY_LEN - 1 > alphabet.len(),
        "body must wrap to cover the alphabet twice"
    );

    let mut bytes = Vec::new();
    bytes.push(b'G');
    for i in 0..(STRKEY_LEN - 1) {
        bytes.push(alphabet[i % alphabet.len()]);
    }
    assert_eq!(bytes.len(), STRKEY_LEN);
    bytes
}

/// Copy an `Address`'s canonical encoding into a fixed-size buffer.
fn strkey_bytes(address: &Address) -> [u8; STRKEY_LEN] {
    let mut buf = [0u8; STRKEY_LEN];
    address.to_string().copy_into_slice(&mut buf);
    buf
}

// ── Format rule ─────────────────────────────────────────────────────────────

#[test]
fn strkey_accepts_generated_account_address() {
    let env = Env::default();
    let addr = Address::generate(&env);
    assert!(
        is_well_formed_strkey(&strkey_bytes(&addr)),
        "a generated account address must pass format validation"
    );
}

#[test]
fn strkey_accepts_generated_contract_address() {
    let env = Env::default();
    let contract_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
    assert!(
        is_well_formed_strkey(&strkey_bytes(&contract_id.address())),
        "a registered contract address must pass format validation"
    );
}

#[test]
fn strkey_accepts_full_base32_alphabet() {
    assert!(is_well_formed_strkey(&full_alphabet_strkey()));
}

#[test]
fn strkey_accepts_contract_version_prefix() {
    let mut bytes = full_alphabet_strkey();
    bytes[0] = b'C';
    assert!(is_well_formed_strkey(&bytes));
}

#[test]
fn strkey_rejects_wrong_length() {
    let bytes = full_alphabet_strkey();
    assert!(!is_well_formed_strkey(&bytes[..STRKEY_LEN - 1]), "55 bytes");
    assert!(!is_well_formed_strkey(&[]), "empty");
    let mut long = bytes.clone();
    long.push(b'A');
    assert!(!is_well_formed_strkey(&long), "57 bytes");
}

#[test]
fn strkey_rejects_unknown_version_prefix() {
    // Muxed accounts (M…), secret seeds (S…), and bare base32 payloads are all
    // invalid here: a keeper is registered by an account or contract id,
    // nothing else.
    for &prefix in b"MSA02gc" {
        let mut bytes = full_alphabet_strkey();
        bytes[0] = prefix;
        assert!(
            !is_well_formed_strkey(&bytes),
            "prefix {:?} must be rejected",
            prefix as char
        );
    }
}

#[test]
fn strkey_rejects_non_base32_body() {
    // Base32 is `A`–`Z` + `2`–`7`; `0`, `1`, `8`, `9`, `-` and `=` (RFC 4648
    // padding) are not. The old length-and-prefix check accepted all of these.
    for &bad in b"0189-=! " {
        let mut bytes = full_alphabet_strkey();
        bytes[10] = bad;
        assert!(
            !is_well_formed_strkey(&bytes),
            "body byte {:?} must be rejected",
            bad as char
        );
    }
}

#[test]
fn strkey_rejects_non_base32_in_the_final_position() {
    let mut bytes = full_alphabet_strkey();
    let last = bytes.len() - 1;
    bytes[last] = b'8';
    assert!(!is_well_formed_strkey(&bytes));
}

// ── Wiring: the contract entry points that consume the rule ─────────────────

struct Deployed {
    env: Env,
    admin: Address,
    claims_id: Address,
}

fn deploy() -> Deployed {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    // Contract addresses for the three cross-contract links — the same `C…`
    // shape `initialize` requires for a deployed protocol.
    let policy_engine = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let risk_pool = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let oracle_verifier = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let claims_id = env.register(ClaimsProcessor, ());

    ClaimsProcessorClient::new(&env, &claims_id).initialize(
        &admin,
        &policy_engine.address(),
        &risk_pool.address(),
        &oracle_verifier.address(),
        &604_800u64,
    );

    Deployed {
        env,
        admin,
        claims_id,
    }
}

#[test]
fn initialize_accepts_account_and_contract_shaped_addresses() {
    let d = deploy();
    // initialize did not panic, so the admin's `G…` address and the three
    // `C…` links all cleared the format check. Assert the round-trip so a
    // future tightening that rejects real addresses fails here rather than in
    // production.
    let client = ClaimsProcessorClient::new(&d.env, &d.claims_id);
    assert_eq!(client.get_admin(), d.admin);
    assert!(!client.is_paused());
}

#[test]
fn add_keeper_accepts_account_and_contract_addresses() {
    let d = deploy();
    let account_keeper = Address::generate(&d.env);
    let contract_keeper = d
        .env
        .register_stellar_asset_contract_v2(Address::generate(&d.env))
        .address();

    let client = ClaimsProcessorClient::new(&d.env, &d.claims_id);
    client.add_keeper(&d.admin, &account_keeper);
    client.add_keeper(&d.admin, &contract_keeper);

    assert!(client.is_keeper(&account_keeper));
    assert!(client.is_keeper(&contract_keeper));
}

#[test]
fn add_keeper_rejects_non_admin() {
    let d = deploy();
    let stranger = Address::generate(&d.env);
    let keeper = Address::generate(&d.env);
    let client = ClaimsProcessorClient::new(&d.env, &d.claims_id);

    let res = client.try_add_keeper(&stranger, &keeper);
    assert_eq!(
        contract_error(res),
        Some(soroban_sdk::Error::from_contract_error(3)),
        "a non-admin must be rejected as Unauthorized, never admitted as a keeper"
    );
    assert!(
        !client.is_keeper(&keeper),
        "a rejected call must not write state"
    );
}

#[test]
fn remove_keeper_revokes_settlement_authority() {
    let d = deploy();
    let keeper = Address::generate(&d.env);
    let client = ClaimsProcessorClient::new(&d.env, &d.claims_id);

    client.add_keeper(&d.admin, &keeper);
    assert!(client.is_keeper(&keeper));

    client.remove_keeper(&d.admin, &keeper);
    assert!(!client.is_keeper(&keeper));

    // Removing an address that was never registered is a no-op, not an error —
    // which is what keeps the registry safely repairable and is why
    // `remove_keeper` deliberately does not re-run the format check that
    // `add_keeper` applies.
    client.remove_keeper(&d.admin, &keeper);
}
