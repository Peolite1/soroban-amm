extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env, Vec,
};

fn setup(env: &Env) -> (StakingClient<'_>, Address, Address, Address) {
    env.mock_all_auths();
    let contract_id = env.register_contract(None, Staking);
    let lp_token = Address::generate(env);
    let reward_token = Address::generate(env);
    let admin = Address::generate(env);
    let staking = StakingClient::new(env, &contract_id);
    staking.initialize(&lp_token, &reward_token, &admin);
    (staking, contract_id, lp_token, admin)
}

fn config_with_boost(
    env: &Env,
    min_boost: i128,
    max_boost: i128,
    min_lock: u64,
    max_lock: u64,
) -> (StakingClient<'_>, Address) {
    env.mock_all_auths();
    let contract_id = env.register_contract(None, Staking);
    let staking = StakingClient::new(env, &contract_id);
    let lp_token = Address::generate(env);
    let reward_token = Address::generate(env);
    let admin = Address::generate(env);
    staking.initialize_with_boost_config(
        &lp_token,
        &reward_token,
        &admin,
        &min_boost,
        &max_boost,
        &min_lock,
        &max_lock,
    );
    (staking, contract_id)
}

fn ledger_dump(env: &Env) -> std::string::String {
    env.to_ledger_snapshot()
        .ledger_entries
        .into_iter()
        .map(|(key, (entry, ttl))| std::format!("{key:?}|{entry:?}|{ttl:?}"))
        .collect::<std::vec::Vec<_>>()
        .join("\n")
}

fn reward_state(env: &Env, contract_id: &Address) -> (i128, i128, i128) {
    env.as_contract(contract_id, || {
        (
            env.storage()
                .instance()
                .get(&DataKey::RewardPoolBalance)
                .unwrap_or(0),
            env.storage()
                .instance()
                .get(&DataKey::OwedRewards)
                .unwrap_or(0),
            env.storage()
                .instance()
                .get(&DataKey::AccumulatedRewardsPerShare)
                .unwrap_or(0),
        )
    })
}

fn staker_count(env: &Env, contract_id: &Address) -> u32 {
    env.as_contract(contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::StakerCount)
            .unwrap_or(0)
    })
}

fn register_staked_addresses(env: &Env, contract_id: &Address, count: u32) -> Vec<Address> {
    let mut stakers = Vec::new(env);
    env.as_contract(contract_id, || {
        for _ in 0..count {
            let staker = Address::generate(env);
            env.storage()
                .persistent()
                .set(&DataKey::StakerAmount(staker.clone()), &1_i128);
            stakers.push_back(staker);
        }
    });
    stakers
}

#[test]
fn stranger_cannot_unpause_and_paused_state_is_preserved() {
    let env = Env::default();
    let (staking, _, _, admin) = setup(&env);
    staking.pause(&admin);
    let stranger = Address::generate(&env);

    assert_eq!(
        staking.try_unpause(&stranger),
        Err(Ok(StakingError::Unauthorized))
    );
    assert!(staking.is_paused());
}

#[test]
fn stranger_cannot_upgrade_and_contract_code_is_unchanged() {
    let env = Env::default();
    let (staking, contract_id, _, _) = setup(&env);
    let stranger = Address::generate(&env);
    let proposed_hash = soroban_sdk::BytesN::from_array(&env, &[7; 32]);
    let code_before = ledger_dump(&env);

    assert_eq!(
        staking.try_upgrade(&stranger, &proposed_hash),
        Err(Ok(StakingError::Unauthorized))
    );
    let code_after = ledger_dump(&env);
    assert_eq!(code_after, code_before);
}

#[test]
fn boost_initializer_cannot_overwrite_existing_config() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, Staking);
    let staking = StakingClient::new(&env, &contract_id);
    let lp_token = Address::generate(&env);
    let reward_token = Address::generate(&env);
    let admin = Address::generate(&env);
    staking.initialize_with_boost_config(
        &lp_token,
        &reward_token,
        &admin,
        &10_000,
        &20_000,
        &100,
        &200,
    );
    let config_before = staking.get_config();

    assert_eq!(
        staking.try_initialize_with_boost_config(
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env),
            &30_000,
            &40_000,
            &300,
            &400,
        ),
        Err(Ok(StakingError::AlreadyInitialized))
    );
    assert_eq!(staking.get_config(), config_before);
}

#[test]
fn boost_configuration_rejects_zero_minimum() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, Staking);
    let staking = StakingClient::new(&env, &contract_id);
    let lp_token = Address::generate(&env);
    let reward_token = Address::generate(&env);
    let admin = Address::generate(&env);

    assert_eq!(
        staking.try_initialize_with_boost_config(
            &lp_token,
            &reward_token,
            &admin,
            &0,
            &10_000,
            &1,
            &2,
        ),
        Err(Ok(StakingError::InvalidBoostConfig))
    );
    assert_eq!(
        staking.try_get_config(),
        Err(Ok(StakingError::NotInitialized))
    );
}

#[test]
fn boost_configuration_rejects_negative_minimum() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, Staking);
    let staking = StakingClient::new(&env, &contract_id);

    assert_eq!(
        staking.try_initialize_with_boost_config(
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env),
            &-1,
            &10_000,
            &1,
            &2,
        ),
        Err(Ok(StakingError::InvalidBoostConfig))
    );
    assert_eq!(
        staking.try_get_config(),
        Err(Ok(StakingError::NotInitialized))
    );
}

#[test]
fn boost_configuration_rejects_maximum_below_minimum() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, Staking);
    let staking = StakingClient::new(&env, &contract_id);

    assert_eq!(
        staking.try_initialize_with_boost_config(
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env),
            &10_001,
            &10_000,
            &1,
            &2,
        ),
        Err(Ok(StakingError::InvalidBoostConfig))
    );
    assert_eq!(
        staking.try_get_config(),
        Err(Ok(StakingError::NotInitialized))
    );
}

#[test]
fn boost_configuration_rejects_negative_maximum() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, Staking);
    let staking = StakingClient::new(&env, &contract_id);

    assert_eq!(
        staking.try_initialize_with_boost_config(
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env),
            &1,
            &-1,
            &1,
            &2,
        ),
        Err(Ok(StakingError::InvalidBoostConfig))
    );
    assert_eq!(
        staking.try_get_config(),
        Err(Ok(StakingError::NotInitialized))
    );
}

#[test]
fn lock_configuration_rejects_zero_minimum() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, Staking);
    let staking = StakingClient::new(&env, &contract_id);

    assert_eq!(
        staking.try_initialize_with_boost_config(
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env),
            &10_000,
            &20_000,
            &0,
            &2,
        ),
        Err(Ok(StakingError::InvalidLockDuration))
    );
    assert_eq!(
        staking.try_get_config(),
        Err(Ok(StakingError::NotInitialized))
    );
}

#[test]
fn lock_configuration_rejects_zero_maximum() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, Staking);
    let staking = StakingClient::new(&env, &contract_id);

    assert_eq!(
        staking.try_initialize_with_boost_config(
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env),
            &10_000,
            &20_000,
            &1,
            &0,
        ),
        Err(Ok(StakingError::InvalidLockDuration))
    );
    assert_eq!(
        staking.try_get_config(),
        Err(Ok(StakingError::NotInitialized))
    );
}

#[test]
fn lock_configuration_rejects_maximum_below_minimum() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, Staking);
    let staking = StakingClient::new(&env, &contract_id);

    assert_eq!(
        staking.try_initialize_with_boost_config(
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env),
            &10_000,
            &20_000,
            &2,
            &1,
        ),
        Err(Ok(StakingError::InvalidLockDuration))
    );
    assert_eq!(
        staking.try_get_config(),
        Err(Ok(StakingError::NotInitialized))
    );
}

#[test]
fn equal_boost_bounds_at_minimum_are_accepted() {
    let env = Env::default();
    let (staking, _) = config_with_boost(&env, 1, 1, 1, 2);
    let config = staking.get_config();
    assert_eq!(config.min_boost, 1);
    assert_eq!(config.max_boost, 1);
}

#[test]
fn equal_boost_bounds_at_standard_scale_are_accepted() {
    let env = Env::default();
    let (staking, _) = config_with_boost(&env, BOOST_SCALE, BOOST_SCALE, 1, 2);
    let config = staking.get_config();
    assert_eq!(config.min_boost, BOOST_SCALE);
    assert_eq!(config.max_boost, BOOST_SCALE);
}

#[test]
fn equal_lock_bounds_at_one_second_are_accepted() {
    let env = Env::default();
    let (staking, _) = config_with_boost(&env, 1, 2, 1, 1);
    let config = staking.get_config();
    assert_eq!(config.min_lock_duration, 1);
    assert_eq!(config.max_lock_duration, 1);
}

#[test]
fn equal_lock_bounds_at_maximum_duration_are_accepted() {
    let env = Env::default();
    let (staking, _) = config_with_boost(&env, 1, 2, MAX_LOCK_DURATION, MAX_LOCK_DURATION);
    let config = staking.get_config();
    assert_eq!(config.min_lock_duration, MAX_LOCK_DURATION);
    assert_eq!(config.max_lock_duration, MAX_LOCK_DURATION);
}

#[test]
fn ordered_boost_and_lock_ranges_are_accepted() {
    let env = Env::default();
    let (staking, _) = config_with_boost(&env, 1, BOOST_SCALE, 1, MAX_LOCK_DURATION);
    let config = staking.get_config();
    assert_eq!(config.min_boost, 1);
    assert_eq!(config.max_boost, BOOST_SCALE);
    assert_eq!(config.min_lock_duration, 1);
    assert_eq!(config.max_lock_duration, MAX_LOCK_DURATION);
}

#[test]
fn extend_lock_rejects_never_staked_account_with_active_expiry() {
    let env = Env::default();
    let (staking, contract_id, _, _) = setup(&env);
    let staker = Address::generate(&env);
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &DataKey::LockExpiry(staker.clone()),
            &(env.ledger().timestamp() + MIN_LOCK_DURATION),
        );
    });

    assert_eq!(
        staking.try_extend_lock(&staker, &MIN_LOCK_DURATION),
        Err(Ok(StakingError::NothingStaked))
    );
}

#[test]
fn extend_lock_rejects_fully_unstaked_account_with_active_expiry() {
    let env = Env::default();
    let (staking, contract_id, _, _) = setup(&env);
    let staker = Address::generate(&env);
    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::StakerAmount(staker.clone()), &0_i128);
        env.storage().persistent().set(
            &DataKey::LockExpiry(staker.clone()),
            &(env.ledger().timestamp() + MIN_LOCK_DURATION),
        );
    });

    assert_eq!(
        staking.try_extend_lock(&staker, &MIN_LOCK_DURATION),
        Err(Ok(StakingError::NothingStaked))
    );
}

#[test]
fn stranger_cannot_set_max_reward_pool_balance() {
    let env = Env::default();
    let (staking, contract_id, _, _) = setup(&env);
    let stranger = Address::generate(&env);
    let cap_before: i128 = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get(&DataKey::ConfigMaxRewardPoolBalance)
            .unwrap_or(0)
    });

    assert_eq!(
        staking.try_set_max_reward_pool_balance(&stranger, &1_000),
        Err(Ok(StakingError::Unauthorized))
    );
    let cap_after: i128 = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get(&DataKey::ConfigMaxRewardPoolBalance)
            .unwrap_or(0)
    });
    assert_eq!(cap_after, cap_before);
}

#[test]
fn stranger_cannot_update_rewards() {
    let env = Env::default();
    let (staking, contract_id, _, _) = setup(&env);
    let stranger = Address::generate(&env);
    let before = reward_state(&env, &contract_id);

    assert_eq!(
        staking.try_update_rewards(&stranger, &100),
        Err(Ok(StakingError::Unauthorized))
    );
    assert_eq!(reward_state(&env, &contract_id), before);
}

#[test]
fn update_rewards_rejects_zero_without_changing_accounting() {
    let env = Env::default();
    let (staking, contract_id, _, admin) = setup(&env);
    let before = reward_state(&env, &contract_id);

    assert_eq!(
        staking.try_update_rewards(&admin, &0),
        Err(Ok(StakingError::InvalidAmount))
    );
    assert_eq!(reward_state(&env, &contract_id), before);
}

#[test]
fn update_rewards_rejects_negative_without_changing_accounting() {
    let env = Env::default();
    let (staking, contract_id, _, admin) = setup(&env);
    let before = reward_state(&env, &contract_id);

    assert_eq!(
        staking.try_update_rewards(&admin, &-1),
        Err(Ok(StakingError::InvalidAmount))
    );
    assert_eq!(reward_state(&env, &contract_id), before);
}

#[test]
fn emergency_withdraw_rejects_empty_staker() {
    let env = Env::default();
    let (staking, _, _, admin) = setup(&env);
    staking.set_emergency_mode(&admin, &true);
    let staker = Address::generate(&env);

    assert_eq!(
        staking.try_emergency_withdraw(&staker),
        Err(Ok(StakingError::NothingStaked))
    );
}

#[test]
fn register_existing_stakers_accepts_maximum_batch_size() {
    let env = Env::default();
    let (staking, contract_id, _, _) = setup(&env);
    let stakers = register_staked_addresses(&env, &contract_id, MAX_BATCH_SIZE);

    staking.register_existing_stakers(&stakers);
    assert_eq!(staker_count(&env, &contract_id), MAX_BATCH_SIZE);
}

#[test]
fn register_existing_stakers_rejects_oversized_batch_without_registration() {
    let env = Env::default();
    let (staking, contract_id, _, _) = setup(&env);
    let stakers = register_staked_addresses(&env, &contract_id, MAX_BATCH_SIZE + 1);

    assert_eq!(
        staking.try_register_existing_stakers(&stakers),
        Err(Ok(StakingError::BatchTooLarge))
    );
    assert_eq!(staker_count(&env, &contract_id), 0);
    env.as_contract(&contract_id, || {
        for staker in stakers.iter() {
            assert!(!env.storage().persistent().has(&DataKey::StakerSlot(staker)));
        }
    });
}
