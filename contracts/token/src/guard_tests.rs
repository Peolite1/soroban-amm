extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env, String,
};

struct TokenFixture {
    env: Env,
    admin: Address,
    locker: Address,
    contract: Address,
}

fn setup() -> TokenFixture {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let locker = Address::generate(&env);
    let contract = env.register_contract(None, LpToken);
    let client = LpTokenClient::new(&env, &contract);
    client.initialize(
        &admin,
        &String::from_str(&env, "Guard LP"),
        &String::from_str(&env, "GLP"),
        &7u32,
    );
    client.set_locker(&locker);
    TokenFixture {
        env,
        admin,
        locker,
        contract,
    }
}

fn snapshot(f: &TokenFixture) -> (i128, i128, i128, usize) {
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    let event_count = f.env.events().all().len();
    (
        client.total_supply(),
        client.balance(&holder),
        f.env.as_contract(&f.contract, || {
            f.env
                .storage()
                .persistent()
                .get(&DataKey::Locked(holder))
                .unwrap_or(0)
        }),
        event_count,
    )
}

#[test]
fn mint_rejects_zero_without_state_change() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    let before = (
        client.total_supply(),
        client.balance(&holder),
        f.env.events().all().len(),
    );
    assert_eq!(
        client.try_mint(&holder, &0),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!(snapshot(&f).0, before.0);
    assert_eq!(snapshot(&f).1, before.1);
}

#[test]
fn mint_rejects_negative_one() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    assert_eq!(
        client.try_mint(&holder, &-1),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!(client.total_supply(), 0);
}

#[test]
fn mint_rejects_i128_min() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    assert_eq!(
        client.try_mint(&holder, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!(client.balance(&holder), 0);
    assert_eq!(client.total_supply(), 0);
}

#[test]
fn burn_rejects_zero_without_state_change() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    client.mint(&holder, &100);
    let before = (client.balance(&holder), client.total_supply());
    assert_eq!(
        client.try_burn(&holder, &0),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!((client.balance(&holder), client.total_supply()), before);
}

#[test]
fn burn_rejects_negative_and_i128_min() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    client.mint(&holder, &100);
    let before = (client.balance(&holder), client.total_supply());
    assert_eq!(
        client.try_burn(&holder, &-1),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!(
        client.try_burn(&holder, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!((client.balance(&holder), client.total_supply()), before);
}

#[test]
fn burn_exact_balance_succeeds_and_overbalance_fails() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    client.mint(&holder, &100);
    assert_eq!(
        client.try_burn(&holder, &101),
        Err(Ok(TokenError::InsufficientBalance))
    );
    client.burn(&holder, &100);
    assert_eq!(client.balance(&holder), 0);
    assert_eq!(client.total_supply(), 0);
}

#[test]
fn lock_rejects_zero_negative_and_i128_min() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    client.mint(&holder, &100);
    for amount in [0, -1, i128::MIN] {
        assert_eq!(
            client.try_lock(&holder, &amount),
            Err(Ok(TokenError::InvalidAmount))
        );
    }
    assert_eq!(client.locked_balance(&holder), 0);
}

#[test]
fn lock_exact_unlocked_balance_succeeds_and_overlock_fails() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    client.mint(&holder, &100);
    client.lock(&holder, &100);
    assert_eq!(client.locked_balance(&holder), 100);
    assert_eq!(
        client.try_lock(&holder, &1),
        Err(Ok(TokenError::InsufficientBalance))
    );
}

#[test]
fn partial_lock_accounts_for_previous_lock_before_overlock() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    client.mint(&holder, &100);
    client.lock(&holder, &60);
    assert_eq!(client.locked_balance(&holder), 60);
    assert_eq!(
        client.try_lock(&holder, &41),
        Err(Ok(TokenError::InsufficientBalance))
    );
    client.lock(&holder, &40);
    assert_eq!(client.locked_balance(&holder), 100);
}

#[test]
fn unlock_rejects_zero_negative_and_i128_min() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    client.mint(&holder, &100);
    client.lock(&holder, &50);
    for amount in [0, -1, i128::MIN] {
        assert_eq!(
            client.try_unlock(&holder, &f.locker, &amount),
            Err(Ok(TokenError::InvalidAmount))
        );
    }
    assert_eq!(client.locked_balance(&holder), 50);
}

#[test]
fn unlock_rejects_per_locker_overage() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    client.mint(&holder, &100);
    client.lock(&holder, &40);
    assert_eq!(
        client.try_unlock(&holder, &f.locker, &41),
        Err(Ok(TokenError::InsufficientLocked))
    );
    assert_eq!(client.locked_balance(&holder), 40);
}

#[test]
fn unlock_rejects_total_locked_overage_across_locker_entries() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    let locker_b = Address::generate(&f.env);
    f.env.as_contract(&f.contract, || {
        f.env.storage().persistent().set(
            &DataKey::LockEntry(holder.clone(), locker_b.clone()),
            &31_i128,
        );
        f.env
            .storage()
            .persistent()
            .set(&DataKey::Locked(holder.clone()), &30_i128);
    });
    assert_eq!(client.locked_balance(&holder), 30);
    assert_eq!(
        client.try_unlock(&holder, &locker_b, &31),
        Err(Ok(TokenError::InsufficientLocked))
    );
}

#[test]
fn migrate_legacy_lock_rejects_zero_negative_and_i128_min() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    for amount in [0, -1, i128::MIN] {
        assert_eq!(
            client.try_migrate_legacy_lock(&holder, &f.locker, &amount),
            Err(Ok(TokenError::InvalidAmount))
        );
    }
}

#[test]
fn migrate_legacy_lock_rejects_amount_above_total_locked() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    f.env.as_contract(&f.contract, || {
        f.env
            .storage()
            .persistent()
            .set(&DataKey::Locked(holder.clone()), &100_i128);
    });
    assert_eq!(
        client.try_migrate_legacy_lock(&holder, &f.locker, &101),
        Err(Ok(TokenError::MigrationOverflow))
    );
}

#[test]
fn propose_admin_rejects_stranger_without_state_change() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let stranger = Address::generate(&f.env);
    let nominee = Address::generate(&f.env);
    assert_eq!(
        client.try_propose_admin(&stranger, &nominee),
        Err(Ok(TokenError::Unauthorized))
    );
    assert_eq!(client.admin(), f.admin);
}

#[test]
fn transfer_rejects_zero_negative_and_i128_min() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let from = Address::generate(&f.env);
    let to = Address::generate(&f.env);
    client.mint(&from, &100);
    let before = (
        client.balance(&from),
        client.balance(&to),
        client.total_supply(),
    );
    for amount in [0, -1, i128::MIN] {
        assert_eq!(
            client.try_transfer(&from, &to, &amount),
            Err(Ok(TokenError::InvalidAmount))
        );
    }
    assert_eq!(
        (
            client.balance(&from),
            client.balance(&to),
            client.total_supply()
        ),
        before
    );
}

#[test]
fn transfer_from_rejects_zero_negative_and_i128_min() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let spender = Address::generate(&f.env);
    let from = Address::generate(&f.env);
    let to = Address::generate(&f.env);
    client.mint(&from, &100);
    let before = (
        client.balance(&from),
        client.balance(&to),
        client.total_supply(),
    );
    for amount in [0, -1, i128::MIN] {
        assert_eq!(
            client.try_transfer_from(&spender, &from, &to, &amount),
            Err(Ok(TokenError::InvalidAmount))
        );
    }
    assert_eq!(
        (
            client.balance(&from),
            client.balance(&to),
            client.total_supply()
        ),
        before
    );
}

#[test]
fn mint_rejects_i128_min_without_event_or_checkpoint() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    let events = f.env.events().all();
    assert_eq!(
        client.try_mint(&holder, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!(f.env.events().all().len(), events.len());
    assert_eq!(client.balance(&holder), 0);
}

#[test]
fn burn_rejects_i128_min_without_event_or_checkpoint() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    client.mint(&holder, &100);
    let supply = client.total_supply();
    let events = f.env.events().all();
    assert_eq!(
        client.try_burn(&holder, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!(f.env.events().all().len(), events.len());
    assert_eq!(client.total_supply(), supply);
}

#[test]
fn lock_rejects_i128_min_without_event_or_locked_change() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    client.mint(&holder, &100);
    let events = f.env.events().all();
    assert_eq!(
        client.try_lock(&holder, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!(f.env.events().all().len(), events.len());
    assert_eq!(client.locked_balance(&holder), 0);
}

#[test]
fn unlock_rejects_i128_min_without_event_or_locked_change() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    client.mint(&holder, &100);
    client.lock(&holder, &50);
    let events = f.env.events().all();
    assert_eq!(
        client.try_unlock(&holder, &f.locker, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!(f.env.events().all().len(), events.len());
    assert_eq!(client.locked_balance(&holder), 50);
}

#[test]
fn transfer_rejects_i128_min_without_event() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let from = Address::generate(&f.env);
    let to = Address::generate(&f.env);
    client.mint(&from, &100);
    let events = f.env.events().all();
    assert_eq!(
        client.try_transfer(&from, &to, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!(f.env.events().all().len(), events.len());
}

#[test]
fn transfer_from_rejects_i128_min_without_event() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let spender = Address::generate(&f.env);
    let from = Address::generate(&f.env);
    let to = Address::generate(&f.env);
    client.mint(&from, &100);
    let events = f.env.events().all();
    assert_eq!(
        client.try_transfer_from(&spender, &from, &to, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!(f.env.events().all().len(), events.len());
}

#[test]
fn migrate_legacy_lock_rejects_i128_min_without_entry() {
    let f = setup();
    let client = LpTokenClient::new(&f.env, &f.contract);
    let holder = Address::generate(&f.env);
    assert_eq!(
        client.try_migrate_legacy_lock(&holder, &f.locker, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!(client.locked_balance(&holder), 0);
}
