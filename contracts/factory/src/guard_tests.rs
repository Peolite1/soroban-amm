extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

fn setup(env: &Env) -> (FactoryClient<'_>, Address, Address, Address, Address) {
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);
    let admin = Address::generate(env);
    let factory_addr = env.register_contract(None, Factory);
    let factory = FactoryClient::new(env, &factory_addr);
    let amm_hash = BytesN::from_array(env, &[1u8; 32]);
    let token_hash = BytesN::from_array(env, &[2u8; 32]);
    factory.initialize(&admin, &amm_hash, &token_hash);
    let fee_asset = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let caller = Address::generate(env);
    (factory, factory_addr, admin, caller, fee_asset)
}

fn pool_count(env: &Env, factory: &Address) -> u64 {
    env.as_contract(factory, || {
        env.storage()
            .instance()
            .get(&DataKey::PoolCount)
            .unwrap_or(0)
    })
}

fn cl_pool_count(env: &Env, factory: &Address) -> u64 {
    env.as_contract(factory, || {
        env.storage()
            .instance()
            .get(&DataKey::ClPoolCount)
            .unwrap_or(0)
    })
}

#[test]
fn initialize_twice_preserves_admin_and_hashes() {
    let env = Env::default();
    let (factory, _, admin, _, _) = setup(&env);
    let amm_before = env.as_contract(&factory.address, || {
        env.storage()
            .instance()
            .get::<DataKey, BytesN<32>>(&DataKey::AmmWasmHash)
    });
    let token_before = env.as_contract(&factory.address, || {
        env.storage()
            .instance()
            .get::<DataKey, BytesN<32>>(&DataKey::TokenWasmHash)
    });
    assert_eq!(factory.get_admin(), Some(admin));

    assert_eq!(
        factory.try_initialize(
            &Address::generate(&env),
            &BytesN::from_array(&env, &[9u8; 32]),
            &BytesN::from_array(&env, &[8u8; 32]),
        ),
        Err(Ok(FactoryError::AlreadyInitialized))
    );
    let amm_after = env.as_contract(&factory.address, || {
        env.storage()
            .instance()
            .get::<DataKey, BytesN<32>>(&DataKey::AmmWasmHash)
    });
    let token_after = env.as_contract(&factory.address, || {
        env.storage()
            .instance()
            .get::<DataKey, BytesN<32>>(&DataKey::TokenWasmHash)
    });
    assert_eq!(amm_after, amm_before);
    assert_eq!(token_after, token_before);
}

fn configure_creation_fee(
    env: &Env,
    factory: &FactoryClient<'_>,
    admin: &Address,
    caller: &Address,
    fee_asset: &Address,
) {
    factory.set_permissionless_mode(&true);
    factory.set_rate_limit(&0);
    factory.set_pool_creation_fee(fee_asset, &100);
    let fee_admin = StellarAssetClient::new(env, fee_asset);
    fee_admin.mint(caller, &1_000);
}

#[test]
fn amm_pool_rejects_negative_fee_before_charge() {
    let env = Env::default();
    let (factory, addr, admin, caller, fee_asset) = setup(&env);
    configure_creation_fee(&env, &factory, &admin, &caller, &fee_asset);
    let ta = Address::generate(&env);
    let tb = Address::generate(&env);
    let before_caller = TokenClient::new(&env, &fee_asset).balance(&caller);
    let before_treasury = TokenClient::new(&env, &fee_asset).balance(&admin);

    assert_eq!(
        factory.try_create_pool_with_fee_bps(&caller, &ta, &tb, &-1, &None),
        Err(Ok(FactoryError::InvalidFeeBps))
    );
    assert_eq!(
        TokenClient::new(&env, &fee_asset).balance(&caller),
        before_caller
    );
    assert_eq!(
        TokenClient::new(&env, &fee_asset).balance(&admin),
        before_treasury
    );
    assert_eq!(pool_count(&env, &addr), 0);
}

#[test]
fn amm_pool_rejects_fee_above_100_percent_before_charge() {
    let env = Env::default();
    let (factory, addr, admin, caller, fee_asset) = setup(&env);
    configure_creation_fee(&env, &factory, &admin, &caller, &fee_asset);
    let ta = Address::generate(&env);
    let tb = Address::generate(&env);
    let before_caller = TokenClient::new(&env, &fee_asset).balance(&caller);
    let before_treasury = TokenClient::new(&env, &fee_asset).balance(&admin);

    assert_eq!(
        factory.try_create_pool_with_fee_bps(&caller, &ta, &tb, &10_001, &None),
        Err(Ok(FactoryError::InvalidFeeBps))
    );
    assert_eq!(
        TokenClient::new(&env, &fee_asset).balance(&caller),
        before_caller
    );
    assert_eq!(
        TokenClient::new(&env, &fee_asset).balance(&admin),
        before_treasury
    );
    assert_eq!(pool_count(&env, &addr), 0);
}

#[test]
fn cl_pool_rejects_negative_fee_before_charge() {
    let env = Env::default();
    let (factory, addr, admin, caller, fee_asset) = setup(&env);
    configure_creation_fee(&env, &factory, &admin, &caller, &fee_asset);
    let ta = Address::generate(&env);
    let tb = Address::generate(&env);
    let before_caller = TokenClient::new(&env, &fee_asset).balance(&caller);
    let before_treasury = TokenClient::new(&env, &fee_asset).balance(&admin);

    assert_eq!(
        factory.try_create_cl_pool(&caller, &ta, &tb, &-1, &0),
        Err(Ok(FactoryError::InvalidFeeBps))
    );
    assert_eq!(
        TokenClient::new(&env, &fee_asset).balance(&caller),
        before_caller
    );
    assert_eq!(
        TokenClient::new(&env, &fee_asset).balance(&admin),
        before_treasury
    );
    assert_eq!(cl_pool_count(&env, &addr), 0);
}

#[test]
fn cl_pool_rejects_fee_above_100_percent_before_charge() {
    let env = Env::default();
    let (factory, addr, admin, caller, fee_asset) = setup(&env);
    configure_creation_fee(&env, &factory, &admin, &caller, &fee_asset);
    let ta = Address::generate(&env);
    let tb = Address::generate(&env);
    let before_caller = TokenClient::new(&env, &fee_asset).balance(&caller);

    assert_eq!(
        factory.try_create_cl_pool(&caller, &ta, &tb, &10_001, &0),
        Err(Ok(FactoryError::InvalidFeeBps))
    );
    assert_eq!(
        TokenClient::new(&env, &fee_asset).balance(&caller),
        before_caller
    );
    assert_eq!(cl_pool_count(&env, &addr), 0);
}

#[test]
fn global_fee_accepts_nonzero_in_range_value() {
    let env = Env::default();
    let (factory, _, admin, _, _) = setup(&env);
    factory.set_treasury(&admin, &Address::generate(&env), &0);
    assert_eq!(factory.set_global_fee(&admin, &1), 0);
}

#[test]
fn amm_pool_rejects_fee_at_100_percent() {
    let env = Env::default();
    let (factory, _, _, _, _) = setup(&env);
    let ta = Address::generate(&env);
    let tb = Address::generate(&env);
    assert_eq!(
        factory.try_create_pool_with_fee_bps(&Address::generate(&env), &ta, &tb, &10_000, &None),
        Err(Ok(FactoryError::InvalidFeeBps))
    );
}

#[test]
fn set_pool_creation_fee_rejects_zero() {
    let env = Env::default();
    let (factory, _, _, _, fee_asset) = setup(&env);
    assert_eq!(
        factory.try_set_pool_creation_fee(&fee_asset, &0),
        Err(Ok(FactoryError::FeeNotConfigured))
    );
}

#[test]
fn set_pool_creation_fee_rejects_negative() {
    let env = Env::default();
    let (factory, _, _, _, fee_asset) = setup(&env);
    assert_eq!(
        factory.try_set_pool_creation_fee(&fee_asset, &-1),
        Err(Ok(FactoryError::FeeNotConfigured))
    );
}

#[test]
fn set_pool_creation_fee_stores_positive_value() {
    let env = Env::default();
    let (factory, _, _, _, fee_asset) = setup(&env);
    factory.set_pool_creation_fee(&fee_asset, &123);
    let stored = env.as_contract(&factory.address, || {
        env.storage()
            .instance()
            .get::<DataKey, i128>(&DataKey::PoolCreationFee)
    });
    assert_eq!(stored, Some(123));
}

#[test]
fn set_global_fee_rejects_negative() {
    let env = Env::default();
    let (factory, _, admin, _, _) = setup(&env);
    let treasury = Address::generate(&env);
    factory.set_treasury(&admin, &treasury, &0);
    assert_eq!(
        factory.try_set_global_fee(&admin, &-1),
        Err(Ok(FactoryError::InvalidFeeBps))
    );
}

#[test]
fn set_global_fee_rejects_above_100_percent() {
    let env = Env::default();
    let (factory, _, admin, _, _) = setup(&env);
    let treasury = Address::generate(&env);
    factory.set_treasury(&admin, &treasury, &0);
    assert_eq!(
        factory.try_set_global_fee(&admin, &10_001),
        Err(Ok(FactoryError::InvalidFeeBps))
    );
}

#[test]
fn set_global_fee_accepts_zero_and_100_percent() {
    let env = Env::default();
    let (factory, _, admin, _, _) = setup(&env);
    let treasury = Address::generate(&env);
    factory.set_treasury(&admin, &treasury, &0);
    assert_eq!(factory.set_global_fee(&admin, &0), Ok(0));
    assert_eq!(factory.set_global_fee(&admin, &10_000), Ok(0));
}

#[test]
fn paginated_global_fee_requires_treasury() {
    let env = Env::default();
    let (factory, _, admin, _, _) = setup(&env);
    assert_eq!(
        factory.try_set_global_fee_paginated(&admin, &0, &10),
        Err(Ok(FactoryError::FeeNotConfigured))
    );
    assert_eq!(factory.get_treasury(), None);
}

#[test]
fn cl_pool_rejects_exact_10000_fee() {
    let env = Env::default();
    let (factory, addr, _, caller, fee_asset) = setup(&env);
    configure_creation_fee(
        &env,
        &factory,
        &Address::generate(&env),
        &caller,
        &fee_asset,
    );
    assert_eq!(
        factory.try_create_cl_pool(
            &caller,
            &Address::generate(&env),
            &Address::generate(&env),
            &10_000,
            &0,
        ),
        Err(Ok(FactoryError::InvalidFeeBps))
    );
    assert_eq!(cl_pool_count(&env, &addr), 0);
}

#[test]
fn set_treasury_rejects_invalid_fee_without_setting_treasury() {
    let env = Env::default();
    let (factory, _, admin, _, _) = setup(&env);
    let treasury = Address::generate(&env);
    assert_eq!(
        factory.try_set_treasury(&admin, &treasury, &10_001),
        Err(Ok(FactoryError::InvalidFeeBps))
    );
    assert_eq!(factory.get_treasury(), None);
}
