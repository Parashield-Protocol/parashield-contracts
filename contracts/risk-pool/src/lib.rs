//! Parashield Risk Pool — v2 (not yet implemented)
//!
//! Liquidity providers deposit USDC into categorised risk pools.
//! Pool tokens (Stellar assets on the built-in DEX) represent LP shares.
//! Premiums flow in as yield; approved claims reduce pool balance.
//!
//! Full design: see ARCHITECTURE.md § Risk Pool
//!
//! Economics
//! ──────────
//!   Premium flow: 80% → pool (LP yield), 10% → protocol treasury, 10% → backstop fund
//!   Claims flow:  settled from pool balance; LP share value decreases proportionally
//!   Target APY:   8-15% for low-risk pools, 20-40% for high-risk
#![no_std]

use soroban_sdk::{contract, contractimpl, Address, Env};

#[contract]
pub struct RiskPool;

#[contractimpl]
impl RiskPool {
    pub fn initialize(_env: Env, _admin: Address) {
        unimplemented!("RiskPool is scheduled for v2. See ARCHITECTURE.md for design.")
    }

    /// Deposit USDC into the pool and receive pool-share tokens.
    pub fn deposit(_env: Env, _provider: Address, _amount: i128) -> u128 {
        unimplemented!()
    }

    /// Burn pool-share tokens and withdraw USDC.
    pub fn withdraw(_env: Env, _provider: Address, _shares: u128) -> i128 {
        unimplemented!()
    }

    /// Harvest accumulated premium yield.
    pub fn claim_yield(_env: Env, _provider: Address) -> i128 {
        unimplemented!()
    }

    /// Called by Policy Engine when a policy is created — locks capital.
    pub fn lock_for_policy(_env: Env, _caller: Address, _policy_id: u128, _amount: i128) {
        unimplemented!()
    }

    /// Called by Claims Processor after payout — reduces locked capital.
    pub fn release_for_claim(_env: Env, _caller: Address, _policy_id: u128, _amount: i128) {
        unimplemented!()
    }

    /// Called when a policy expires with no claim — returns locked capital to pool.
    pub fn release_expired(_env: Env, _caller: Address, _policy_id: u128) {
        unimplemented!()
    }
}
