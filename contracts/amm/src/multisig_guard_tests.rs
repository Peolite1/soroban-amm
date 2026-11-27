//! Guards on the multisig emergency-withdraw paths of `AmmPool` (#1576).
//!
//! Each test sets up a withdrawal that is valid in every respect except the
//! one guarded condition, asserts the typed error, and checks that reserves,
//! balances and the proposal are left unchanged. Where it helps, the same
//! setup is then shown to succeed once that one condition is met, so the
//! rejection cannot come from some other missing precondition.

extern crate std;

use super::*;
use crate::tests::{setup_pool, TestSetup};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env,
};

const EMERGENCY_DELAY: u64 = 24 * 60 * 60;

/// Reserves, pool balances and the recipient's balances.
#[derive(Debug, PartialEq)]
struct Funds {
    info: PoolInfo,
    accrued: (i128, i128),
    pool_a: i128,
    pool_b: i128,
    recipient_a: i128,
    recipient_b: i128,
    drained: bool,
}

fn funds(ts: &TestSetup, recipient: &Address) -> Funds {
    let amm = amm(ts);
    let token_a = TokenClient::new(&ts.env, &ts.ta_addr);
    let token_b = TokenClient::new(&ts.env, &ts.tb_addr);
    Funds {
        info: amm.get_info(),
        accrued: amm.get_accrued_fees(),
        pool_a: token_a.balance(&ts.amm_addr),
        pool_b: token_b.balance(&ts.amm_addr),
        recipient_a: token_a.balance(recipient),
        recipient_b: token_b.balance(recipient),
        drained: amm.is_drained(),
    }
}

fn amm(ts: &TestSetup) -> AmmPoolClient<'_> {
    AmmPoolClient::new(&ts.env, &ts.amm_addr)
}

fn seeded_pool() -> TestSetup {
    let ts = setup_pool(30);
    let provider = Address::generate(&ts.env);
    StellarAssetClient::new(&ts.env, &ts.ta_addr).mint(&provider, &1_000_000);
    StellarAssetClient::new(&ts.env, &ts.tb_addr).mint(&provider, &1_000_000);
    amm(&ts).add_liquidity(&provider, &1_000_000, &1_000_000, &0, &u64::MAX);
    ts
}

fn advance(env: &Env, secs: u64) {
    env.ledger().set_timestamp(env.ledger().timestamp() + secs);
}

#[test]
fn test_single_admin_emergency_withdraw_rejected_while_multisig_active() {
    let ts = seeded_pool();
    let env = &ts.env;
    let recipient = Address::generate(env);
    let signer = Address::generate(env);

    // A fully valid single-admin withdrawal: announced, paused, delay elapsed.
    amm(&ts).announce_emergency_withdraw(&ts.admin, &recipient);
    amm(&ts).pause();
    advance(env, EMERGENCY_DELAY);
    // Configuring the guard afterwards must still close the single-admin path.
    amm(&ts).set_multisig(&ts.admin, &vec![env, signer], &1);
    let before = funds(&ts, &recipient);
    assert!(before.pool_a > 0 && before.pool_b > 0);

    assert_eq!(
        amm(&ts).try_emergency_withdraw(&ts.admin, &recipient),
        Err(Ok(AmmError::Unauthorized))
    );
    assert_eq!(funds(&ts, &recipient), before);
    assert_eq!(
        amm(&ts).get_emergency_announcement().map(|a| a.to),
        Some(recipient)
    );
}

#[test]
fn test_single_admin_emergency_withdraw_succeeds_without_multisig() {
    // Control for the test above: the identical setup minus the multisig
    // guard drains the pool.
    let ts = seeded_pool();
    let recipient = Address::generate(&ts.env);
    amm(&ts).announce_emergency_withdraw(&ts.admin, &recipient);
    amm(&ts).pause();
    advance(&ts.env, EMERGENCY_DELAY);

    amm(&ts).emergency_withdraw(&ts.admin, &recipient);
    let after = funds(&ts, &recipient);
    assert!(after.drained);
    assert_eq!((after.pool_a, after.pool_b), (0, 0));
}

#[test]
fn test_exec_multisig_emergency_wd_requires_pause() {
    let ts = seeded_pool();
    let env = &ts.env;
    let recipient = Address::generate(env);
    let signer = Address::generate(env);
    amm(&ts).set_multisig(&ts.admin, &vec![env, signer.clone()], &1);
    let pid = amm(&ts).propose_emergency_withdraw(&signer, &recipient);
    advance(env, EMERGENCY_DELAY);
    assert!(!amm(&ts).is_paused());
    let funds_before = funds(&ts, &recipient);
    let proposal_before = amm(&ts).get_multisig_proposal(&pid).unwrap();

    assert_eq!(
        amm(&ts).try_exec_multisig_emergency_wd(&signer, &pid),
        Err(Ok(AmmError::Paused))
    );
    assert_eq!(funds(&ts, &recipient), funds_before);
    assert_eq!(
        amm(&ts).get_multisig_proposal(&pid).unwrap(),
        proposal_before
    );
    assert!(!proposal_before.executed);

    // Pausing is the only thing missing.
    amm(&ts).pause();
    amm(&ts).exec_multisig_emergency_wd(&signer, &pid);
    assert!(amm(&ts).is_drained());
    assert!(amm(&ts).get_multisig_proposal(&pid).unwrap().executed);
}

#[test]
fn test_non_signer_cannot_propose_approve_or_execute_multisig_withdrawal() {
    let ts = seeded_pool();
    let env = &ts.env;
    let recipient = Address::generate(env);
    let stranger = Address::generate(env);
    let s1 = Address::generate(env);
    let s2 = Address::generate(env);

    // Without a configured guard nobody is a signer.
    assert_eq!(
        amm(&ts).try_propose_emergency_withdraw(&stranger, &recipient),
        Err(Ok(AmmError::Unauthorized))
    );

    amm(&ts).set_multisig(&ts.admin, &vec![env, s1.clone(), s2.clone()], &2);
    assert_eq!(
        amm(&ts).try_propose_emergency_withdraw(&stranger, &recipient),
        Err(Ok(AmmError::Unauthorized))
    );
    assert_eq!(amm(&ts).get_multisig_proposal(&1), None);

    let pid = amm(&ts).propose_emergency_withdraw(&s1, &recipient);
    let proposal_before = amm(&ts).get_multisig_proposal(&pid).unwrap();
    assert_eq!(
        amm(&ts).try_approve_multisig_proposal(&stranger, &pid),
        Err(Ok(AmmError::Unauthorized))
    );
    assert_eq!(
        amm(&ts).get_multisig_proposal(&pid).unwrap(),
        proposal_before
    );

    amm(&ts).approve_multisig_proposal(&s2, &pid);
    amm(&ts).pause();
    advance(env, EMERGENCY_DELAY);
    let funds_before = funds(&ts, &recipient);
    let proposal_before = amm(&ts).get_multisig_proposal(&pid).unwrap();
    assert_eq!(
        amm(&ts).try_exec_multisig_emergency_wd(&stranger, &pid),
        Err(Ok(AmmError::Unauthorized))
    );
    assert_eq!(funds(&ts, &recipient), funds_before);
    assert_eq!(
        amm(&ts).get_multisig_proposal(&pid).unwrap(),
        proposal_before
    );
}
