//! `confidence_score` must stay inside 0-100 on every submission path
//! (issue #571).
//!
//! `submit_data` already carried the bounds check and had its 0 / 101 cases
//! pinned. What was *not* pinned is the rest of the surface: there are four
//! ways to get an `OracleDataPoint` into storage, and a bounds check that only
//! exists on one of them is not a bounds check. An out-of-range score that
//! reaches storage is not cosmetic — `get_aggregated` and
//! `reputation_weighted_confidence` weight the median by it, so a single
//! `u32::MAX` reading can dominate an aggregation that is supposed to be a
//! consensus of 0-100 reliability ratings.
//!
//! Note the range is `[1, 100]`, not `[0, 100]`: a zero-confidence reading
//! carries no reliability information at all and would otherwise satisfy every
//! `min_confidence` filter by accident.
#![cfg(test)]

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    Address, Bytes, BytesN, Env, Symbol, Vec,
};

/// A fixed observation timestamp, comfortably inside the freshness window
/// relative to the ledger time the harness installs.
const OBSERVED_AT: u64 = 1_748_736_000;

/// Values that must never reach storage, from "just out of range" to "as large
/// as the type allows".
const OUT_OF_RANGE: [u32; 5] = [0, 101, 200, 1_000, u32::MAX];

/// The two ends of the accepted range.
const IN_RANGE_BOUNDARIES: [u32; 2] = [1, 100];

fn setup() -> (Env, Address, Address, Address) {
    let env = Env::default();
    env.ledger().set_timestamp(OBSERVED_AT);
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let contract_id = env.register(OracleVerifier, ());
    OracleVerifierClient::new(&env, &contract_id).initialize(&admin);

    let oracle = Address::generate(&env);
    OracleVerifierClient::new(&env, &contract_id).add_oracle(
        &admin,
        &oracle,
        &symbol_short!("weather"),
        &90u32,
    );

    (env, admin, contract_id, oracle)
}

fn data_type() -> Symbol {
    symbol_short!("weather")
}

fn key() -> Symbol {
    symbol_short!("kis2606")
}

fn other_key() -> Symbol {
    symbol_short!("kis2607")
}

fn ciphertext(env: &Env) -> Bytes {
    Bytes::from_slice(env, b"ciphertext-blob")
}

fn nonce(env: &Env) -> BytesN<12> {
    BytesN::from_array(env, &[3u8; 12])
}

/// `Error::InvalidConfidence` — the code every bounds violation must raise.
fn invalid_confidence() -> soroban_sdk::Error {
    soroban_sdk::Error::from_contract_error(7)
}

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

// ── submit_data ─────────────────────────────────────────────────────────────

#[test]
fn submit_data_accepts_both_ends_of_the_range() {
    for confidence in IN_RANGE_BOUNDARIES {
        let (env, _admin, contract_id, oracle) = setup();
        let client = OracleVerifierClient::new(&env, &contract_id);
        client.submit_data(
            &oracle,
            &data_type(),
            &key(),
            &32_000_000i128,
            &confidence,
            &OBSERVED_AT,
        );
        assert_eq!(
            client.get_data(&data_type(), &key()).confidence,
            confidence,
            "confidence {} must be accepted verbatim",
            confidence
        );
    }
}

#[test]
fn submit_data_rejects_every_out_of_range_score() {
    for confidence in OUT_OF_RANGE {
        let (env, _admin, contract_id, oracle) = setup();
        let client = OracleVerifierClient::new(&env, &contract_id);
        assert_eq!(
            contract_error(client.try_submit_data(
                &oracle,
                &data_type(),
                &key(),
                &32_000_000i128,
                &confidence,
                &OBSERVED_AT,
            )),
            Some(invalid_confidence()),
            "confidence {} must be rejected",
            confidence
        );
    }
}

#[test]
fn rejected_submit_data_leaves_no_reading_behind() {
    let (env, _admin, contract_id, oracle) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    assert!(client
        .try_submit_data(
            &oracle,
            &data_type(),
            &key(),
            &32_000_000i128,
            &500,
            &OBSERVED_AT
        )
        .is_err());

    assert!(
        client.try_get_data(&data_type(), &key()).is_err(),
        "an out-of-range submission must not have been persisted"
    );
}

// ── submit_encrypted_data ───────────────────────────────────────────────────

#[test]
fn submit_encrypted_data_accepts_both_ends_of_the_range() {
    for confidence in IN_RANGE_BOUNDARIES {
        let (env, _admin, contract_id, oracle) = setup();
        let client = OracleVerifierClient::new(&env, &contract_id);
        client.submit_encrypted_data(
            &oracle,
            &data_type(),
            &key(),
            &ciphertext(&env),
            &nonce(&env),
            &confidence,
            &OBSERVED_AT,
        );
        let points = client.get_encrypted_data(&data_type(), &key());
        assert_eq!(points.len(), 1);
        assert_eq!(points.get_unchecked(0).confidence, confidence);
    }
}

#[test]
fn submit_encrypted_data_rejects_every_out_of_range_score() {
    for confidence in OUT_OF_RANGE {
        let (env, _admin, contract_id, oracle) = setup();
        let client = OracleVerifierClient::new(&env, &contract_id);
        assert_eq!(
            contract_error(client.try_submit_encrypted_data(
                &oracle,
                &data_type(),
                &key(),
                &ciphertext(&env),
                &nonce(&env),
                &confidence,
                &OBSERVED_AT,
            )),
            Some(invalid_confidence()),
            "confidence {} must be rejected",
            confidence
        );
    }
}

// ── batch_submit_data ───────────────────────────────────────────────────────

#[test]
fn batch_submit_data_accepts_both_ends_of_the_range() {
    for confidence in IN_RANGE_BOUNDARIES {
        let (env, _admin, contract_id, oracle) = setup();
        let client = OracleVerifierClient::new(&env, &contract_id);
        let mut subs: Vec<(Symbol, i128, u32, u64)> = Vec::new(&env);
        subs.push_back((key(), 30_000_000i128, confidence, OBSERVED_AT));
        client.batch_submit_data(&oracle, &data_type(), &subs);
        assert_eq!(client.get_data(&data_type(), &key()).confidence, confidence);
    }
}

#[test]
fn batch_submit_data_rejects_every_out_of_range_score() {
    for confidence in OUT_OF_RANGE {
        let (env, _admin, contract_id, oracle) = setup();
        let client = OracleVerifierClient::new(&env, &contract_id);
        let mut subs: Vec<(Symbol, i128, u32, u64)> = Vec::new(&env);
        subs.push_back((key(), 30_000_000i128, confidence, OBSERVED_AT));
        assert_eq!(
            contract_error(client.try_batch_submit_data(&oracle, &data_type(), &subs)),
            Some(invalid_confidence()),
            "confidence {} must be rejected",
            confidence
        );
    }
}

/// The batch is one transaction, so a single bad reading must take the whole
/// call down — otherwise a rejected reading could still be half-written while
/// its neighbours persisted.
#[test]
fn batch_submit_data_rejects_the_whole_call_on_one_bad_reading() {
    let (env, _admin, contract_id, oracle) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let mut subs: Vec<(Symbol, i128, u32, u64)> = Vec::new(&env);
    subs.push_back((key(), 30_000_000i128, 90u32, OBSERVED_AT));
    subs.push_back((other_key(), 31_000_000i128, u32::MAX, OBSERVED_AT));

    assert_eq!(
        contract_error(client.try_batch_submit_data(&oracle, &data_type(), &subs)),
        Some(invalid_confidence())
    );
    assert!(
        client.try_get_data(&data_type(), &key()).is_err(),
        "the valid sibling reading must not survive the revert"
    );
}

// ── submit_data_batch ───────────────────────────────────────────────────────

fn submissions(env: &Env, confidence: u32) -> Vec<OracleDataSubmission> {
    Vec::from_array(
        env,
        [OracleDataSubmission {
            key: key(),
            value: 30_000_000i128,
            confidence,
            timestamp: OBSERVED_AT,
        }],
    )
}

#[test]
fn submit_data_batch_accepts_both_ends_of_the_range() {
    for confidence in IN_RANGE_BOUNDARIES {
        let (env, _admin, contract_id, oracle) = setup();
        let client = OracleVerifierClient::new(&env, &contract_id);
        client.submit_data_batch(&oracle, &data_type(), &submissions(&env, confidence));
        assert_eq!(client.get_data(&data_type(), &key()).confidence, confidence);
    }
}

#[test]
fn submit_data_batch_rejects_every_out_of_range_score() {
    for confidence in OUT_OF_RANGE {
        let (env, _admin, contract_id, oracle) = setup();
        let client = OracleVerifierClient::new(&env, &contract_id);
        let subs = submissions(&env, confidence);
        assert_eq!(
            contract_error(client.try_submit_data_batch(&oracle, &data_type(), &subs)),
            Some(invalid_confidence()),
            "confidence {} must be rejected",
            confidence
        );
    }
}

#[test]
fn submit_data_batch_rejects_the_whole_call_on_one_bad_reading() {
    let (env, _admin, contract_id, oracle) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);

    let subs = Vec::from_array(
        &env,
        [
            OracleDataSubmission {
                key: key(),
                value: 30_000_000i128,
                confidence: 90u32,
                timestamp: OBSERVED_AT,
            },
            OracleDataSubmission {
                key: other_key(),
                value: 31_000_000i128,
                confidence: 0u32,
                timestamp: OBSERVED_AT,
            },
        ],
    );
    assert_eq!(
        contract_error(client.try_submit_data_batch(&oracle, &data_type(), &subs)),
        Some(invalid_confidence())
    );
    assert!(client.try_get_data(&data_type(), &key()).is_err());
}

// ── A bound on the aggregate itself ────────────────────────────────────────

/// The bounds check is what keeps the weighted average inside 0-100. With
/// every accepted score already in range, the aggregate cannot escape it, and
/// an out-of-range score cannot reach the aggregation in the first place —
/// which is the property the issue is really about.
#[test]
fn aggregated_confidence_stays_within_zero_to_one_hundred() {
    let (env, admin, contract_id, oracle) = setup();
    let client = OracleVerifierClient::new(&env, &contract_id);
    let second = Address::generate(&env);
    client.add_oracle(&admin, &second, &data_type(), &100u32);

    for (addr, confidence) in [(&oracle, 100u32), (&second, 1u32)] {
        client.submit_data(
            addr,
            &data_type(),
            &key(),
            &30_000_000i128,
            &confidence,
            &OBSERVED_AT,
        );
    }

    let agg = client.get_aggregated(&data_type(), &key());
    assert!(
        (1..=100).contains(&agg.confidence),
        "aggregated confidence {} escaped 0-100",
        agg.confidence
    );
    assert!(
        (1..=100).contains(&agg.min_confidence),
        "minimum confidence {} escaped 0-100",
        agg.min_confidence
    );
}
