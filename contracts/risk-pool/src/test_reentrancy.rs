#![cfg(test)]

use soroban_sdk::{
    contract, contractimpl, panic_with_error, symbol_short, testutils::Address as _,
    Address, Env, Symbol,
};

use crate::{Error, RiskPool, RiskPoolClient};

#[contract]
pub struct MaliciousToken;

#[contractimpl]
impl MaliciousToken {
    pub fn set_pool(env: Env, pool: Address) {
        env.storage().instance().set(&Symbol::new(&env, "pool"), &pool);
    }

    pub fn transfer(env: Env, from: Address, _to: Address, amount: i128) {
        // Try to re-enter risk pool deposit during token transfer
        if let Some(pool_addr) = env.storage().instance().get::<_, Address>(&Symbol::new(&env, "pool")) {
            let pool = RiskPoolClient::new(&env, &pool_addr);
            // This should trigger the reentrancy guard
            pool.deposit(&from, &amount, &0, &false);
        }
    }

    pub fn balance(_env: Env, _id: Address) -> i128 {
        1_000_000_000_000
    }
}

#[test]
fn reentrancy_guard_blocks_recursive_call() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let backstop = Address::generate(&env);
    let lp = Address::generate(&env);
    let policy_engine = Address::generate(&env);
    let claims_processor = Address::generate(&env);

    let mal_token_id = env.register(MaliciousToken, ());
    let mal_client = MaliciousTokenClient::new(&env, &mal_token_id);

    let pool_id = env.register(RiskPool, ());
    let pool = RiskPoolClient::new(&env, &pool_id);

    mal_client.set_pool(&pool_id);

    pool.initialize(
        &admin,
        &mal_token_id,
        &treasury,
        &backstop,
        &Symbol::new(&env, "crop"),
        &policy_engine,
        &claims_processor,
    );

    // Calling deposit triggers token transfer which tries to re-enter deposit.
    // The re-entrant call fails with Error::Reentrancy (#47), causing the invocation to fail.
    let res = pool.try_deposit(&lp, &100_000_000, &0, &false);
    assert!(res.is_err(), "Reentrancy attack should be blocked by guard");
}

#[test]
fn reentrancy_guard_resets_after_normal_call() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let backstop = Address::generate(&env);
    let lp = Address::generate(&env);
    let policy_engine = Address::generate(&env);
    let claims_processor = Address::generate(&env);

    let usdc_id = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let usdc_admin = soroban_sdk::token::StellarAssetClient::new(&env, &usdc_id);
    usdc_admin.mint(&lp, &10_000_000_000);

    let pool_id = env.register(RiskPool, ());
    let pool = RiskPoolClient::new(&env, &pool_id);

    pool.initialize(
        &admin,
        &usdc_id,
        &treasury,
        &backstop,
        &Symbol::new(&env, "crop"),
        &policy_engine,
        &claims_processor,
    );

    // Sequential calls should succeed normally (guard is properly released after each call)
    let shares1 = pool.deposit(&lp, &100_000_000, &0, &false);
    assert!(shares1 > 0);

    let shares2 = pool.deposit(&lp, &100_000_000, &0, &false);
    assert!(shares2 > 0);
}

// ── Issue #822: transfer_position reentrancy guard tests ──────────────────────

/// Stateful mock token that tracks balances accurately (so `deposit` and
/// `receive_premium` succeed during setup) and, once armed, attempts a
/// reentrant call into `RiskPool` during `transfer` (such as `pay_out_yield`).
#[contract]
pub struct ReentrantYieldToken;

#[contractimpl]
impl ReentrantYieldToken {
    pub fn mint(env: Env, to: Address, amount: i128) {
        let key = (symbol_short!("bal"), to);
        let cur: i128 = env.storage().instance().get(&key).unwrap_or(0);
        env.storage().instance().set(&key, &(cur + amount));
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        env.storage().instance().get(&(symbol_short!("bal"), id)).unwrap_or(0)
    }

    pub fn arm_attack(
        env: Env,
        pool: Address,
        target: Symbol,
        actor: Address,
        recipient: Address,
        amount: i128,
    ) {
        env.storage().instance().set(&symbol_short!("armed"), &true);
        env.storage().instance().set(&symbol_short!("pool"), &pool);
        env.storage().instance().set(&symbol_short!("target"), &target);
        env.storage().instance().set(&symbol_short!("actor"), &actor);
        env.storage().instance().set(&symbol_short!("recip"), &recipient);
        env.storage().instance().set(&symbol_short!("amt"), &amount);
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        let from_key = (symbol_short!("bal"), from);
        let to_key = (symbol_short!("bal"), to);
        let from_bal: i128 = env.storage().instance().get(&from_key).unwrap_or(0);
        let to_bal: i128 = env.storage().instance().get(&to_key).unwrap_or(0);
        env.storage().instance().set(&from_key, &(from_bal - amount));
        env.storage().instance().set(&to_key, &(to_bal + amount));

        let armed: bool = env.storage().instance().get(&symbol_short!("armed")).unwrap_or(false);
        if !armed {
            return;
        }
        // Disarm before re-entering to avoid infinite recursion if a guard were absent.
        env.storage().instance().set(&symbol_short!("armed"), &false);

        let pool_addr: Address = env.storage().instance().get(&symbol_short!("pool")).unwrap();
        let target: Symbol = env.storage().instance().get(&symbol_short!("target")).unwrap();
        let actor: Address = env.storage().instance().get(&symbol_short!("actor")).unwrap();
        let recipient: Address = env.storage().instance().get(&symbol_short!("recip")).unwrap();
        let atk_amount: i128 = env.storage().instance().get(&symbol_short!("amt")).unwrap();

        let pool = RiskPoolClient::new(&env, &pool_addr);
        let reentrancy_err = soroban_sdk::Error::from_contract_error(Error::Reentrancy as u32);

        let inner_err = if target == symbol_short!("xfer") {
            contract_error(pool.try_transfer_position(&actor, &recipient, &atk_amount))
        } else if target == symbol_short!("deposit") {
            contract_error(pool.try_deposit(&actor, &atk_amount, &0, &false))
        } else if target == symbol_short!("withdraw") {
            contract_error(pool.try_withdraw(&actor, &atk_amount))
        } else if target == symbol_short!("claim") {
            contract_error(pool.try_claim_yield(&actor))
        } else if target == symbol_short!("premium") {
            contract_error(pool.try_receive_premium(&actor, &atk_amount))
        } else {
            None
        };

        if inner_err == Some(reentrancy_err) {
            panic_with_error!(&env, Error::Reentrancy);
        }
    }
}

fn contract_error<T, E>(
    res: Result<Result<T, E>, Result<soroban_sdk::Error, soroban_sdk::InvokeError>>,
) -> Option<soroban_sdk::Error> {
    res.err().and_then(|inner| inner.ok())
}

struct TransferReentrancyFixture<'a> {
    token: ReentrantYieldTokenClient<'a>,
    pool_id: Address,
    pool: RiskPoolClient<'a>,
    lp1: Address,
    lp2: Address,
    shares: i128,
}

fn setup_transfer_reentrancy_pool(env: &Env) -> TransferReentrancyFixture<'_> {
    let admin = Address::generate(env);
    let treasury = Address::generate(env);
    let backstop = Address::generate(env);
    let lp1 = Address::generate(env);
    let lp2 = Address::generate(env);
    let premium_payer = Address::generate(env);
    let policy_engine = Address::generate(env);
    let claims_processor = Address::generate(env);

    let token_id = env.register(ReentrantYieldToken, ());
    let token = ReentrantYieldTokenClient::new(env, &token_id);
    token.mint(&lp1, &10_000_000_000);
    token.mint(&premium_payer, &10_000_000_000);

    let pool_id = env.register(RiskPool, ());
    let pool = RiskPoolClient::new(env, &pool_id);

    pool.initialize(
        &admin,
        &token_id,
        &treasury,
        &backstop,
        &Symbol::new(env, "crop"),
        &policy_engine,
        &claims_processor,
    );

    // Deposit and distribute premium so lp1 has non-zero pending yield,
    // which causes transfer_position to invoke pay_out_yield -> token.transfer.
    let shares = pool.deposit(&lp1, &1_000_000_000, &0, &false);
    pool.receive_premium(&premium_payer, &100_000_000);

    TransferReentrancyFixture {
        token,
        pool_id,
        pool,
        lp1,
        lp2,
        shares,
    }
}

#[test]
fn transfer_position_blocks_reentrant_transfer_position() {
    let env = Env::default();
    env.mock_all_auths();

    let f = setup_transfer_reentrancy_pool(&env);
    let lp3 = Address::generate(&env);

    f.token.arm_attack(
        &f.pool_id,
        &symbol_short!("xfer"),
        &f.lp1,
        &lp3,
        &(f.shares / 4),
    );

    let res = f.pool.try_transfer_position(&f.lp1, &f.lp2, &(f.shares / 2));
    assert_eq!(
        contract_error(res),
        Some(soroban_sdk::Error::from_contract_error(Error::Reentrancy as u32)),
        "re-entering transfer_position during yield payout must fail with Error::Reentrancy"
    );
}

#[test]
fn transfer_position_blocks_reentrant_deposit() {
    let env = Env::default();
    env.mock_all_auths();

    let f = setup_transfer_reentrancy_pool(&env);

    f.token.arm_attack(
        &f.pool_id,
        &symbol_short!("deposit"),
        &f.lp1,
        &f.lp2,
        &100_000_000,
    );

    let res = f.pool.try_transfer_position(&f.lp1, &f.lp2, &(f.shares / 2));
    assert_eq!(
        contract_error(res),
        Some(soroban_sdk::Error::from_contract_error(Error::Reentrancy as u32)),
        "re-entering deposit during transfer_position payout must fail with Error::Reentrancy"
    );
}

#[test]
fn transfer_position_blocks_reentrant_withdraw() {
    let env = Env::default();
    env.mock_all_auths();

    let f = setup_transfer_reentrancy_pool(&env);

    f.token.arm_attack(
        &f.pool_id,
        &symbol_short!("withdraw"),
        &f.lp1,
        &f.lp2,
        &(f.shares / 4),
    );

    let res = f.pool.try_transfer_position(&f.lp1, &f.lp2, &(f.shares / 2));
    assert_eq!(
        contract_error(res),
        Some(soroban_sdk::Error::from_contract_error(Error::Reentrancy as u32)),
        "re-entering withdraw during transfer_position payout must fail with Error::Reentrancy"
    );
}

#[test]
fn transfer_position_blocks_reentrant_claim_yield() {
    let env = Env::default();
    env.mock_all_auths();

    let f = setup_transfer_reentrancy_pool(&env);

    f.token.arm_attack(
        &f.pool_id,
        &symbol_short!("claim"),
        &f.lp1,
        &f.lp2,
        &0,
    );

    let res = f.pool.try_transfer_position(&f.lp1, &f.lp2, &(f.shares / 2));
    assert_eq!(
        contract_error(res),
        Some(soroban_sdk::Error::from_contract_error(Error::Reentrancy as u32)),
        "re-entering claim_yield during transfer_position payout must fail with Error::Reentrancy"
    );
}

#[test]
fn transfer_position_blocks_reentrant_receive_premium() {
    let env = Env::default();
    env.mock_all_auths();

    let f = setup_transfer_reentrancy_pool(&env);

    f.token.arm_attack(
        &f.pool_id,
        &symbol_short!("premium"),
        &f.lp1,
        &f.lp2,
        &10_000_000,
    );

    let res = f.pool.try_transfer_position(&f.lp1, &f.lp2, &(f.shares / 2));
    assert_eq!(
        contract_error(res),
        Some(soroban_sdk::Error::from_contract_error(Error::Reentrancy as u32)),
        "re-entering receive_premium during transfer_position payout must fail with Error::Reentrancy"
    );
}

#[test]
fn transfer_position_normal_and_sequential_calls_succeed() {
    let env = Env::default();
    env.mock_all_auths();

    let f = setup_transfer_reentrancy_pool(&env);

    let amount1 = f.pool.transfer_position(&f.lp1, &f.lp2, &(f.shares / 4));
    assert!(amount1 > 0);

    // Guard must be released so a second sequential transfer succeeds.
    let amount2 = f.pool.transfer_position(&f.lp1, &f.lp2, &(f.shares / 4));
    assert!(amount2 > 0);

    let pos2 = f.pool.get_position(&f.lp2).unwrap();
    assert_eq!(pos2.shares, (f.shares / 4) * 2);
}

#[test]
fn transfer_position_releases_guard_after_failed_transfer_insufficient_shares() {
    let env = Env::default();
    env.mock_all_auths();

    let f = setup_transfer_reentrancy_pool(&env);

    // Fails with InsufficientShares; guard must still be released.
    let failed = f.pool.try_transfer_position(&f.lp1, &f.lp2, &(f.shares + 1));
    assert_eq!(
        contract_error(failed),
        Some(soroban_sdk::Error::from_contract_error(Error::InsufficientShares as u32))
    );

    // Subsequent valid transfer must succeed.
    let amount = f.pool.transfer_position(&f.lp1, &f.lp2, &(f.shares / 2));
    assert!(amount > 0);
}

#[test]
fn transfer_position_releases_guard_after_unauthorized_call() {
    let env = Env::default();
    env.mock_all_auths();

    let f = setup_transfer_reentrancy_pool(&env);

    // Clear mocked auths so `from.require_auth()` rejects the call.
    env.mock_auths(&[]);
    let failed = f.pool.try_transfer_position(&f.lp1, &f.lp2, &(f.shares / 2));
    assert!(failed.is_err(), "unauthorized transfer_position must fail");

    // Re-enable auths: guard must have been released so a subsequent transfer succeeds.
    env.mock_all_auths();
    let amount = f.pool.transfer_position(&f.lp1, &f.lp2, &(f.shares / 2));
    assert!(amount > 0);
}

#[test]
fn emergency_withdraw_rejects_reentrant_call() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let backstop = Address::generate(&env);
    let lp = Address::generate(&env);
    let policy_engine = Address::generate(&env);
    let claims_processor = Address::generate(&env);

    let mal_token_id = env.register(MaliciousToken, ());
    let mal_client = MaliciousTokenClient::new(&env, &mal_token_id);

    let pool_id = env.register(RiskPool, ());
    let pool = RiskPoolClient::new(&env, &pool_id);

    pool.initialize(
        &admin,
        &mal_token_id,
        &treasury,
        &backstop,
        &Symbol::new(&env, "crop"),
        &policy_engine,
        &claims_processor,
    );

    // Initial deposit
    let shares = pool.deposit(&lp, &100_000_000, &0, &false);
    assert!(shares > 0);

    // Arm MaliciousToken with pool_id so the token transfer during emergency_withdraw tries to re-enter deposit
    mal_client.set_pool(&pool_id);

    // emergency_withdraw triggers token.transfer which tries to re-enter deposit.
    // The ReentrancyGuard rejects the reentrant call, causing the operation to fail.
    let res = pool.try_emergency_withdraw(&lp, &admin, &shares);
    assert!(res.is_err(), "emergency_withdraw reentrant attempt must fail");
}
