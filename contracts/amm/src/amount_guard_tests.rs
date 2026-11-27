//! Non-positive amount guards on the fund-moving `AmmPool` entrypoints (#1576).
//!
//! Every `return Err(AmmError::ZeroAmount)` below is driven with an input that
//! the guard itself must reject; a later check would otherwise either accept
//! the call or fail it with a different error. Each test asserts the typed
//! error and that pool reserves, share supply, accrued fees and the caller's
//! balances are unchanged by the rejected call.
//!
//! Negative amounts are driven through pools whose tokens are this
//! workspace's `token::LpToken`, which rejects them with its own
//! `TokenError::InvalidAmount`. A Stellar Asset Contract rejects a negative
//! transfer with `NegativeAmountError`, whose contract error code (8) is the
//! same as `AmmError::ZeroAmount`, so on SAC pools a missing guard would be
//! masked by the token's failure decoding as the pool's own error.
//!
//! The quote-side guards in `get_amount_in` (after the division) and in
//! `swap_exact_out` (on the quoted input) cannot fire: the quote is
//! `numerator / denominator + 1` with both terms positive, so it is always at
//! least 1. Those two survivors are recorded in `mutations.toml` instead.

extern crate std;

use super::*;
use crate::tests::{setup_pool, MockFlashLoanReceiver, MockFlashLoanReceiverClient, TestSetup};
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Bytes, Env, IntoVal,
};

/// Everything a rejected call must leave untouched.
#[derive(Debug, PartialEq)]
struct Snapshot {
    info: PoolInfo,
    accrued: (i128, i128),
    pool_a: i128,
    pool_b: i128,
    user_a: i128,
    user_b: i128,
    user_shares: i128,
}

fn snapshot(env: &Env, amm_addr: &Address, user: &Address) -> Snapshot {
    let amm = AmmPoolClient::new(env, amm_addr);
    let info = amm.get_info();
    let token_a = TokenClient::new(env, &info.token_a);
    let token_b = TokenClient::new(env, &info.token_b);
    Snapshot {
        accrued: amm.get_accrued_fees(),
        pool_a: token_a.balance(amm_addr),
        pool_b: token_b.balance(amm_addr),
        user_a: token_a.balance(user),
        user_b: token_b.balance(user),
        user_shares: amm.shares_of(user),
        info,
    }
}

fn amm(ts: &TestSetup) -> AmmPoolClient<'_> {
    AmmPoolClient::new(&ts.env, &ts.amm_addr)
}

/// A user holding `amount` of both pool tokens.
fn funded_user(ts: &TestSetup, amount: i128) -> Address {
    let user = Address::generate(&ts.env);
    StellarAssetClient::new(&ts.env, &ts.ta_addr).mint(&user, &amount);
    StellarAssetClient::new(&ts.env, &ts.tb_addr).mint(&user, &amount);
    user
}

/// Mint `amount` of `token` to `to`. SACs, `token::LpToken` and the mock
/// tokens below all expose `mint(to, amount)`.
fn mint(env: &Env, token: &Address, to: &Address, amount: i128) {
    env.invoke_contract::<()>(
        token,
        &soroban_sdk::Symbol::new(env, "mint"),
        soroban_sdk::vec![env, to.into_val(env), amount.into_val(env)],
    );
}

/// An initialized, unseeded pool over `(token_a, token_b)` that reuses the
/// env and admin of `base`.
fn pool_over(base: &TestSetup, token_a: Address, token_b: Address) -> TestSetup {
    let env = &base.env;
    let amm_addr = env.register_contract(None, AmmPool);
    let lp_addr = env.register_contract(None, token::LpToken);
    token::LpTokenClient::new(env, &lp_addr).initialize(
        &amm_addr,
        &soroban_sdk::String::from_str(env, "AMM LP Token"),
        &soroban_sdk::String::from_str(env, "ALP"),
        &7_u32,
    );
    AmmPoolClient::new(env, &amm_addr).initialize(
        &base.admin,
        &token_a,
        &token_b,
        &lp_addr,
        &30,
        &base.admin,
        &0,
    );
    TestSetup {
        env: env.clone(),
        amm_addr,
        lp_addr,
        ta_addr: token_a,
        tb_addr: token_b,
        admin: base.admin.clone(),
    }
}

/// An unseeded pool whose tokens are `token::LpToken` instances, which reject
/// non-positive transfers with `TokenError::InvalidAmount`, plus a user
/// holding 10M of each.
fn strict_token_pool() -> (TestSetup, Address) {
    let base = setup_pool(30);
    let env = &base.env;
    let new_token = |symbol: &str| {
        let addr = env.register_contract(None, token::LpToken);
        token::LpTokenClient::new(env, &addr).initialize(
            &base.admin,
            &soroban_sdk::String::from_str(env, symbol),
            &soroban_sdk::String::from_str(env, symbol),
            &7_u32,
        );
        addr
    };
    let pool = pool_over(&base, new_token("TKA"), new_token("TKB"));
    let user = Address::generate(env);
    mint(env, &pool.ta_addr, &user, 10_000_000);
    mint(env, &pool.tb_addr, &user, 10_000_000);
    (pool, user)
}

/// A pool seeded with `(amount_a, amount_b)`; returns the LP provider.
fn seeded_pool(amount_a: i128, amount_b: i128) -> (TestSetup, Address) {
    let ts = setup_pool(30);
    let provider = funded_user(&ts, amount_a.max(amount_b));
    amm(&ts).add_liquidity(&provider, &amount_a, &amount_b, &0, &u64::MAX);
    (ts, provider)
}

#[test]
fn test_add_liquidity_rejects_negative_first_deposit_without_state_change() {
    // On an empty pool both amounts negative give a positive product, so
    // without the guard `sqrt(amount_a * amount_b)` would mint shares and the
    // call would go on to move negative token amounts.
    let (ts, provider) = strict_token_pool();
    let before = snapshot(&ts.env, &ts.amm_addr, &provider);

    assert_eq!(
        amm(&ts).try_add_liquidity(&provider, &-4_000, &-4_000, &0, &u64::MAX),
        Err(Ok(AmmError::ZeroAmount))
    );
    assert_eq!(
        amm(&ts).try_add_liquidity(&provider, &0, &4_000, &0, &u64::MAX),
        Err(Ok(AmmError::ZeroAmount))
    );
    assert_eq!(snapshot(&ts.env, &ts.amm_addr, &provider), before);
}

#[test]
fn test_add_liquidity_rejects_deposit_that_mints_zero_shares_without_state_change() {
    // reserve_a (4M) exceeds the share supply (sqrt(4M * 1M) = 2M), so a
    // 1-unit deposit of each token is worth 0 shares after rounding down.
    let (ts, _) = seeded_pool(4_000_000, 1_000_000);
    assert_eq!(amm(&ts).get_info().total_shares, 2_000_000);
    let depositor = funded_user(&ts, 10);
    let before = snapshot(&ts.env, &ts.amm_addr, &depositor);

    assert_eq!(
        amm(&ts).try_add_liquidity(&depositor, &1, &1, &0, &u64::MAX),
        Err(Ok(AmmError::ZeroAmount))
    );
    assert_eq!(snapshot(&ts.env, &ts.amm_addr, &depositor), before);
}

#[test]
fn test_add_liquidity_fot_rejects_non_positive_amounts_without_state_change() {
    let (ts, provider) = strict_token_pool();
    amm(&ts).add_liquidity(&provider, &1_000_000, &1_000_000, &0, &u64::MAX);
    let before = snapshot(&ts.env, &ts.amm_addr, &provider);

    for (amount_a, amount_b) in [(-1_000, 1_000), (1_000, -1_000), (0, 1_000)] {
        assert_eq!(
            amm(&ts).try_add_liquidity_fot(&provider, &amount_a, &amount_b, &0, &0, &0, &u64::MAX),
            Err(Ok(AmmError::ZeroAmount)),
            "add_liquidity_fot({amount_a}, {amount_b})"
        );
    }
    assert_eq!(snapshot(&ts.env, &ts.amm_addr, &provider), before);
}

#[test]
fn test_remove_liquidity_rejects_non_positive_shares_without_state_change() {
    let (ts, provider) = seeded_pool(1_000_000, 1_000_000);
    let before = snapshot(&ts.env, &ts.amm_addr, &provider);
    assert!(before.user_shares > 0);

    for shares in [0, -1_000] {
        assert_eq!(
            amm(&ts).try_remove_liquidity(&provider, &shares, &i128::MIN, &i128::MIN, &u64::MAX),
            Err(Ok(AmmError::ZeroAmount)),
            "remove_liquidity({shares})"
        );
    }
    assert_eq!(snapshot(&ts.env, &ts.amm_addr, &provider), before);
}

#[test]
fn test_remove_liquidity_one_sided_rejects_non_positive_shares_without_state_change() {
    let (ts, provider) = seeded_pool(1_000_000, 1_000_000);
    let before = snapshot(&ts.env, &ts.amm_addr, &provider);

    for shares in [0, -1_000] {
        assert_eq!(
            amm(&ts).try_remove_liquidity_one_sided(
                &provider,
                &shares,
                &ts.ta_addr,
                &i128::MIN,
                &u64::MAX
            ),
            Err(Ok(AmmError::ZeroAmount)),
            "remove_liquidity_one_sided({shares})"
        );
    }
    assert_eq!(snapshot(&ts.env, &ts.amm_addr, &provider), before);
}

#[test]
fn test_swap_rejects_non_positive_amount_in_without_state_change() {
    let (ts, _) = seeded_pool(1_000_000, 1_000_000);
    let trader = funded_user(&ts, 10_000);
    let before = snapshot(&ts.env, &ts.amm_addr, &trader);

    for amount_in in [0, -1_000] {
        assert_eq!(
            amm(&ts).try_swap(&trader, &ts.ta_addr, &amount_in, &i128::MIN, &u64::MAX),
            Err(Ok(AmmError::ZeroAmount)),
            "swap({amount_in})"
        );
    }
    assert_eq!(snapshot(&ts.env, &ts.amm_addr, &trader), before);
}

#[test]
fn test_swap_exact_out_reports_zero_amount_before_pool_and_token_checks() {
    // The quote in `get_amount_in` rejects a zero `amount_out` too, but only
    // after its empty-pool and token checks, so the up-front guard is what
    // makes a zero request report `ZeroAmount` like every other entrypoint.
    let empty = setup_pool(30);
    let trader = funded_user(&empty, 10_000);
    let before = snapshot(&empty.env, &empty.amm_addr, &trader);
    assert_eq!(
        amm(&empty).try_swap_exact_out(&trader, &empty.tb_addr, &0, &i128::MAX, &u64::MAX),
        Err(Ok(AmmError::ZeroAmount))
    );
    assert_eq!(snapshot(&empty.env, &empty.amm_addr, &trader), before);

    let (ts, _) = seeded_pool(1_000_000, 1_000_000);
    let trader = funded_user(&ts, 10_000);
    let foreign_token = Address::generate(&ts.env);
    let before = snapshot(&ts.env, &ts.amm_addr, &trader);
    for amount_out in [0, -1_000] {
        assert_eq!(
            amm(&ts).try_swap_exact_out(
                &trader,
                &foreign_token,
                &amount_out,
                &i128::MAX,
                &u64::MAX
            ),
            Err(Ok(AmmError::ZeroAmount)),
            "swap_exact_out({amount_out})"
        );
    }
    assert_eq!(snapshot(&ts.env, &ts.amm_addr, &trader), before);
}

#[test]
fn test_flash_loan_rejects_empty_or_negative_amounts_without_state_change() {
    let (ts, _) = seeded_pool(1_000_000, 1_000_000);
    let receiver = ts.env.register_contract(None, MockFlashLoanReceiver);
    MockFlashLoanReceiverClient::new(&ts.env, &receiver).initialize(
        &ts.amm_addr,
        &ts.ta_addr,
        &ts.tb_addr,
        &true,
    );
    StellarAssetClient::new(&ts.env, &ts.ta_addr).mint(&receiver, &1_000);
    StellarAssetClient::new(&ts.env, &ts.tb_addr).mint(&receiver, &1_000);
    let before = snapshot(&ts.env, &ts.amm_addr, &receiver);

    for (amount_a, amount_b) in [(0, 0), (-100, 100), (100, -100)] {
        assert_eq!(
            amm(&ts).try_flash_loan(&receiver, &amount_a, &amount_b, &Bytes::new(&ts.env)),
            Err(Ok(AmmError::ZeroAmount)),
            "flash_loan({amount_a}, {amount_b})"
        );
    }
    assert_eq!(snapshot(&ts.env, &ts.amm_addr, &receiver), before);
}

#[test]
fn test_simulate_swap_rejects_non_positive_amount_in() {
    let (ts, _) = seeded_pool(1_000_000, 1_000_000);
    for amount_in in [0, -1_000] {
        assert_eq!(
            amm(&ts).try_simulate_swap(&ts.ta_addr, &amount_in),
            Err(Ok(AmmError::ZeroAmount)),
            "simulate_swap({amount_in})"
        );
    }
}

#[test]
fn test_swap_fot_rejects_non_positive_amount_in_without_state_change() {
    let (ts, trader) = strict_token_pool();
    amm(&ts).add_liquidity(&trader, &1_000_000, &1_000_000, &0, &u64::MAX);
    let before = snapshot(&ts.env, &ts.amm_addr, &trader);

    for amount_in in [0, -1_000] {
        assert_eq!(
            amm(&ts).try_swap_fot(
                &trader,
                &ts.ta_addr,
                &amount_in,
                &i128::MIN,
                &i128::MIN,
                &u64::MAX,
                &None
            ),
            Err(Ok(AmmError::ZeroAmount)),
            "swap_fot({amount_in})"
        );
    }
    assert_eq!(snapshot(&ts.env, &ts.amm_addr, &trader), before);
}

// ── Tokens that deliver less than the nominal amount ─────────────────────────

/// A token whose transfers credit the recipient `amount * rate_bps / 10_000`
/// while debiting the sender the full `amount`. `10_000` is a normal token,
/// `0` swallows every transfer and a negative rate debits the recipient (a
/// hostile token). Balances may go negative.
#[contract]
struct DeliveryRateToken;

#[contracttype]
enum DeliveryKey {
    Balance(Address),
    RateBps,
}

#[contractimpl]
impl DeliveryRateToken {
    pub fn set_rate_bps(env: Env, rate_bps: i128) {
        env.storage()
            .instance()
            .set(&DeliveryKey::RateBps, &rate_bps);
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let balance = Self::balance(env.clone(), to.clone());
        env.storage()
            .instance()
            .set(&DeliveryKey::Balance(to), &(balance + amount));
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        env.storage()
            .instance()
            .get(&DeliveryKey::Balance(id))
            .unwrap_or(0)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        let rate_bps: i128 = env
            .storage()
            .instance()
            .get(&DeliveryKey::RateBps)
            .unwrap_or(10_000);
        let from_balance = Self::balance(env.clone(), from.clone());
        env.storage()
            .instance()
            .set(&DeliveryKey::Balance(from), &(from_balance - amount));
        let to_balance = Self::balance(env.clone(), to.clone());
        env.storage().instance().set(
            &DeliveryKey::Balance(to),
            &(to_balance + amount * rate_bps / 10_000),
        );
    }
}

/// An initialized, unseeded pool whose token A is a [`DeliveryRateToken`] and
/// token B a regular asset, plus a user holding 10M of each.
fn delivery_rate_pool() -> (TestSetup, Address) {
    let base = setup_pool(30);
    let rate_token = base.env.register_contract(None, DeliveryRateToken);
    let pool = pool_over(&base, rate_token, base.tb_addr.clone());
    let user = Address::generate(&pool.env);
    mint(&pool.env, &pool.ta_addr, &user, 10_000_000);
    mint(&pool.env, &pool.tb_addr, &user, 10_000_000);
    (pool, user)
}

#[test]
fn test_swap_fot_rejects_transfer_that_delivers_nothing_without_state_change() {
    let (ts, user) = delivery_rate_pool();
    amm(&ts).add_liquidity_fot(&user, &1_000_000, &1_000_000, &0, &0, &0, &u64::MAX);
    DeliveryRateTokenClient::new(&ts.env, &ts.ta_addr).set_rate_bps(&0);
    let before = snapshot(&ts.env, &ts.amm_addr, &user);

    // With no output or receipt floor, nothing but the receipt guard stops a
    // swap whose input never reaches the pool.
    assert_eq!(
        amm(&ts).try_swap_fot(&user, &ts.ta_addr, &10_000, &0, &0, &u64::MAX, &None),
        Err(Ok(AmmError::ZeroAmount))
    );
    assert_eq!(snapshot(&ts.env, &ts.amm_addr, &user), before);
}

#[test]
fn test_add_liquidity_fot_rejects_non_positive_receipt_without_state_change() {
    let (ts, user) = delivery_rate_pool();
    let rate_token = DeliveryRateTokenClient::new(&ts.env, &ts.ta_addr);
    let before = snapshot(&ts.env, &ts.amm_addr, &user);

    // A token that swallows the deposit.
    rate_token.set_rate_bps(&0);
    assert_eq!(
        amm(&ts).try_add_liquidity_fot(&user, &1_000_000, &1_000_000, &0, &0, &0, &u64::MAX),
        Err(Ok(AmmError::ZeroAmount))
    );
    // A hostile token that debits the pool on receipt. With the caller's
    // receipt floor opted out, the receipt guard is the only check that stops
    // the negative amount reaching the share calculation.
    rate_token.set_rate_bps(&-10_000);
    assert_eq!(
        amm(&ts).try_add_liquidity_fot(
            &user,
            &1_000_000,
            &1_000_000,
            &i128::MIN,
            &i128::MIN,
            &0,
            &u64::MAX
        ),
        Err(Ok(AmmError::ZeroAmount))
    );
    assert_eq!(snapshot(&ts.env, &ts.amm_addr, &user), before);
}
