//! Tests for multi-recipient payout rounding and dust distribution (Issue #772).
//!
//! Multi-recipient payouts truncate upon integer division. To prevent dust from
//! accumulating in escrow, the protocol dust policy dictates that any remainder
//! (total_payout % num_recipients) is distributed in full to the last recipient,
//! ensuring that the sum of distributed payouts matches the total payout exactly
//! with zero escrow dust left behind.
#![cfg(test)]

use super::*;
use soroban_sdk::Env;

#[test]
fn test_multi_recipient_even_split() {
    let env = Env::default();
    let total = 100i128;
    let count = 2u32;
    let dist = ClaimsProcessor::calculate_multi_recipient_payout(env.clone(), total, count);
    assert_eq!(dist.len(), 2);
    assert_eq!(dist.get(0).unwrap(), 50);
    assert_eq!(dist.get(1).unwrap(), 50);

    let sum: i128 = dist.iter().sum();
    assert_eq!(sum, total);
}

#[test]
fn test_multi_recipient_rounding_dust_distributed_to_last() {
    let env = Env::default();
    let total = 100i128;
    let count = 3u32;
    // 100 / 3 = 33 base with 1 remainder
    let dist = ClaimsProcessor::calculate_multi_recipient_payout(env.clone(), total, count);
    assert_eq!(dist.len(), 3);
    assert_eq!(dist.get(0).unwrap(), 33);
    assert_eq!(dist.get(1).unwrap(), 33);
    assert_eq!(dist.get(2).unwrap(), 34); // 33 + 1 dust remainder

    let sum: i128 = dist.iter().sum();
    assert_eq!(sum, total);
    let dust = total - sum;
    assert_eq!(dust, 0, "Escrow dust must be exactly zero");
}

#[test]
fn test_multi_recipient_large_remainder() {
    let env = Env::default();
    let total = 10i128;
    let count = 4u32;
    // 10 / 4 = 2 base with 2 remainder
    let dist = ClaimsProcessor::calculate_multi_recipient_payout(env.clone(), total, count);
    assert_eq!(dist.len(), 4);
    assert_eq!(dist.get(0).unwrap(), 2);
    assert_eq!(dist.get(1).unwrap(), 2);
    assert_eq!(dist.get(2).unwrap(), 2);
    assert_eq!(dist.get(3).unwrap(), 4); // 2 + 2 dust remainder

    let sum: i128 = dist.iter().sum();
    assert_eq!(sum, total);
    let dust = total - sum;
    assert_eq!(dust, 0);
}

#[test]
fn test_multi_recipient_single_recipient() {
    let env = Env::default();
    let total = 1_000_000i128;
    let count = 1u32;
    let dist = ClaimsProcessor::calculate_multi_recipient_payout(env.clone(), total, count);
    assert_eq!(dist.len(), 1);
    assert_eq!(dist.get(0).unwrap(), 1_000_000);

    let sum: i128 = dist.iter().sum();
    assert_eq!(sum, total);
}

#[test]
fn test_multi_recipient_zero_amount() {
    let env = Env::default();
    let total = 0i128;
    let count = 5u32;
    let dist = ClaimsProcessor::calculate_multi_recipient_payout(env.clone(), total, count);
    assert_eq!(dist.len(), 5);
    for i in 0..5 {
        assert_eq!(dist.get(i).unwrap(), 0);
    }
    let sum: i128 = dist.iter().sum();
    assert_eq!(sum, 0);
}

#[test]
fn test_multi_recipient_more_recipients_than_amount() {
    let env = Env::default();
    let total = 2i128;
    let count = 5u32;
    // 2 / 5 = 0 base with 2 remainder
    let dist = ClaimsProcessor::calculate_multi_recipient_payout(env.clone(), total, count);
    assert_eq!(dist.len(), 5);
    assert_eq!(dist.get(0).unwrap(), 0);
    assert_eq!(dist.get(1).unwrap(), 0);
    assert_eq!(dist.get(2).unwrap(), 0);
    assert_eq!(dist.get(3).unwrap(), 0);
    assert_eq!(dist.get(4).unwrap(), 2); // 0 + 2 dust remainder

    let sum: i128 = dist.iter().sum();
    assert_eq!(sum, total);
}

#[test]
#[should_panic(expected = "Error(Contract, #22)")]
fn test_multi_recipient_zero_recipients_fails() {
    let env = Env::default();
    ClaimsProcessor::calculate_multi_recipient_payout(env, 100, 0);
}

#[test]
fn test_multi_recipient_via_client() {
    let env = Env::default();
    let contract_id = env.register(ClaimsProcessor, ());
    let client = ClaimsProcessorClient::new(&env, &contract_id);

    let dist = client.calculate_multi_recipient_payout(&100i128, &3u32);
    assert_eq!(dist.len(), 3);
    assert_eq!(dist.get(0).unwrap(), 33);
    assert_eq!(dist.get(1).unwrap(), 33);
    assert_eq!(dist.get(2).unwrap(), 34);
}
