extern crate std;

use super::*;
use amm::{AmmPool, AmmPoolClient};
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    token::StellarAssetClient,
    Address, Env, IntoVal,
};
use token::{LpToken, LpTokenClient};

fn setup() -> (
    Env,
    Address,
    Address,
    Address,
    Address,
    Address,
    Address,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);
    let gov = Address::generate(&env);
    let provider = Address::generate(&env);
    let treasury = Address::generate(&env);
    let amm = env.register_contract(None, AmmPool);
    let lp = env.register_contract(None, LpToken);
    LpTokenClient::new(&env, &lp).initialize(
        &amm,
        &soroban_sdk::String::from_str(&env, "LP"),
        &soroban_sdk::String::from_str(&env, "LP"),
        &7u32,
    );
    let ta = env
        .register_stellar_asset_contract_v2(gov.clone())
        .address();
    let tb = env
        .register_stellar_asset_contract_v2(gov.clone())
        .address();
    let reward = env
        .register_stellar_asset_contract_v2(gov.clone())
        .address();
    AmmPoolClient::new(&env, &amm).initialize(&gov, &ta, &tb, &lp, &30, &gov, &0);
    StellarAssetClient::new(&env, &ta).mint(&provider, &10_000);
    StellarAssetClient::new(&env, &tb).mint(&provider, &10_000);
    AmmPoolClient::new(&env, &amm).add_liquidity(&provider, &10_000, &10_000, &0, &u64::MAX);
    StellarAssetClient::new(&env, &reward).mint(&gov, &1_000_000);
    let incentives = env.register_contract(None, IncentiveCampaigns);
    let client = IncentiveCampaignsClient::new(&env, &incentives);
    client.initialize(&gov);
    (env, incentives, amm, lp, reward, provider, gov, treasury)
}

fn create_campaign(
    env: &Env,
    incentives: &Address,
    pool: &Address,
    lp: &Address,
    reward: &Address,
    provider: &Address,
    gov: &Address,
) -> u64 {
    let client = IncentiveCampaignsClient::new(env, incentives);
    let id = client.create_campaign(gov, pool, lp, reward, &1_000, &2_000, &100, &100_000);
    client.stake(provider, &id, &100);
    id
}

fn leftover_recovery_event_count(env: &Env, incentives: &Address) -> usize {
    let wanted: soroban_sdk::Vec<soroban_sdk::Val> =
        (Symbol::new(env, "leftover_recovered"),).into_val(env);
    env.events()
        .all()
        .iter()
        .filter(|event| &event.0 == incentives && event.1 == wanted)
        .count()
}

#[test]
fn repeated_recovery_leaves_governance_and_contract_balances_unchanged() {
    let (env, incentives, pool, lp, reward, provider, gov, treasury) = setup();
    let client = IncentiveCampaignsClient::new(&env, &incentives);
    let id = create_campaign(&env, &incentives, &pool, &lp, &reward, &provider, &gov);
    env.ledger()
        .set_timestamp(2_000 + DEFAULT_CLAIM_GRACE_SECS + 1);
    let reward_client = soroban_sdk::token::TokenClient::new(&env, &reward);
    let contract_before = reward_client.balance(&incentives);
    let treasury_before = reward_client.balance(&treasury);
    let events_before = env.events().all().len();

    let recovered = client.recover_leftover_funds(&gov, &id, &treasury);
    assert!(recovered > 0);
    assert_eq!(
        reward_client.balance(&treasury),
        treasury_before + recovered
    );
    assert_eq!(
        reward_client.balance(&incentives),
        contract_before - recovered
    );
    assert_eq!(env.events().all().len(), events_before + 1);

    let contract_after = reward_client.balance(&incentives);
    let treasury_after = reward_client.balance(&treasury);
    assert_eq!(
        client.try_recover_leftover_funds(&gov, &id, &treasury),
        Err(Ok(IncentiveError::CampaignInactive))
    );
    assert_eq!(reward_client.balance(&incentives), contract_after);
    assert_eq!(reward_client.balance(&treasury), treasury_after);
}

#[test]
fn full_distribution_recovery_returns_no_leftover() {
    let (env, incentives, pool, lp, reward, provider, gov, treasury) = setup();
    let client = IncentiveCampaignsClient::new(&env, &incentives);
    let id = create_campaign(&env, &incentives, &pool, &lp, &reward, &provider, &gov);
    env.ledger()
        .set_timestamp(2_000 + DEFAULT_CLAIM_GRACE_SECS + 1);
    env.as_contract(&incentives, || {
        let mut campaign: Campaign = env
            .storage()
            .persistent()
            .get(&DataKey::Campaign(id))
            .unwrap();
        campaign.total_distributed = campaign.funding_amount;
        env.storage()
            .persistent()
            .set(&DataKey::Campaign(id), &campaign);
    });
    let reward_client = soroban_sdk::token::TokenClient::new(&env, &reward);
    let treasury_before = reward_client.balance(&treasury);
    assert_eq!(
        client.try_recover_leftover_funds(&gov, &id, &treasury),
        Err(Ok(IncentiveError::NoLeftoverFunds))
    );
    assert_eq!(reward_client.balance(&treasury), treasury_before);
}

#[test]
fn overdistribution_recovery_returns_no_leftover() {
    let (env, incentives, pool, lp, reward, provider, gov, treasury) = setup();
    let client = IncentiveCampaignsClient::new(&env, &incentives);
    let id = create_campaign(&env, &incentives, &pool, &lp, &reward, &provider, &gov);
    env.ledger()
        .set_timestamp(2_000 + DEFAULT_CLAIM_GRACE_SECS + 1);
    env.as_contract(&incentives, || {
        let mut campaign: Campaign = env
            .storage()
            .persistent()
            .get(&DataKey::Campaign(id))
            .unwrap();
        campaign.total_distributed = campaign.funding_amount + 1;
        env.storage()
            .persistent()
            .set(&DataKey::Campaign(id), &campaign);
    });
    assert_eq!(
        client.try_recover_leftover_funds(&gov, &id, &treasury),
        Err(Ok(IncentiveError::NoLeftoverFunds))
    );
}

#[test]
fn recovery_event_is_emitted_once() {
    let (env, incentives, pool, lp, reward, provider, gov, treasury) = setup();
    let client = IncentiveCampaignsClient::new(&env, &incentives);
    let id = create_campaign(&env, &incentives, &pool, &lp, &reward, &provider, &gov);
    env.ledger()
        .set_timestamp(2_000 + DEFAULT_CLAIM_GRACE_SECS + 1);
    let before = leftover_recovery_event_count(&env, &incentives);
    client.recover_leftover_funds(&gov, &id, &treasury);
    assert_eq!(leftover_recovery_event_count(&env, &incentives), before + 1);
}

#[test]
fn recovery_deactivates_campaign_and_later_claim_is_inactive() {
    let (env, incentives, pool, lp, reward, provider, gov, treasury) = setup();
    let client = IncentiveCampaignsClient::new(&env, &incentives);
    let id = create_campaign(&env, &incentives, &pool, &lp, &reward, &provider, &gov);
    env.ledger()
        .set_timestamp(2_000 + DEFAULT_CLAIM_GRACE_SECS + 1);
    client.recover_leftover_funds(&gov, &id, &treasury);
    assert!(!client.get_campaign(&id).unwrap().active);
    assert_eq!(
        client.try_claim_rewards(&provider, &id),
        Err(Ok(IncentiveError::CampaignInactive))
    );
}

#[test]
fn zero_lp_supply_returns_typed_error_without_claimed_accrual() {
    let (env, incentives, pool, lp, reward, provider, gov, _treasury) = setup();
    let client = IncentiveCampaignsClient::new(&env, &incentives);
    let id = create_campaign(&env, &incentives, &pool, &lp, &reward, &provider, &gov);
    env.as_contract(&incentives, || {
        env.storage().persistent().remove(&DataKey::StakeBased(id));
    });
    env.as_contract(&lp, || {
        env.storage()
            .instance()
            .set(&token::DataKey::TotalSupply, &0_i128);
    });
    let balance_before = LpTokenClient::new(&env, &lp).balance(&provider);
    let accrual_before = client.get_campaign_accrual(&id).unwrap();
    assert_eq!(
        client.try_claim_rewards(&provider, &id),
        Err(Ok(IncentiveError::NoLpSupply))
    );
    assert_eq!(
        LpTokenClient::new(&env, &lp).balance(&provider),
        balance_before
    );
    assert_eq!(client.get_campaign_accrual(&id).unwrap(), accrual_before);
}

#[test]
fn holder_balance_with_zero_total_supply_cannot_panic_claim() {
    let (env, incentives, pool, lp, reward, provider, gov, _treasury) = setup();
    let client = IncentiveCampaignsClient::new(&env, &incentives);
    let id = create_campaign(&env, &incentives, &pool, &lp, &reward, &provider, &gov);
    env.as_contract(&incentives, || {
        env.storage().persistent().remove(&DataKey::StakeBased(id));
    });
    env.as_contract(&lp, || {
        env.storage()
            .persistent()
            .set(&token::DataKey::Balance(provider.clone()), &100_i128);
        env.storage()
            .instance()
            .set(&token::DataKey::TotalSupply, &0_i128);
    });
    assert_eq!(
        client.try_claim_rewards(&provider, &id),
        Err(Ok(IncentiveError::NoLpSupply))
    );
}

#[test]
fn zero_supply_claim_does_not_change_reward_balance() {
    let (env, incentives, pool, lp, reward, provider, gov, _treasury) = setup();
    let client = IncentiveCampaignsClient::new(&env, &incentives);
    let id = create_campaign(&env, &incentives, &pool, &lp, &reward, &provider, &gov);
    env.as_contract(&incentives, || {
        env.storage().persistent().remove(&DataKey::StakeBased(id));
    });
    env.as_contract(&lp, || {
        env.storage()
            .instance()
            .set(&token::DataKey::TotalSupply, &0_i128);
    });
    let reward_balance = soroban_sdk::token::TokenClient::new(&env, &reward).balance(&provider);
    assert_eq!(
        client.try_claim_rewards(&provider, &id),
        Err(Ok(IncentiveError::NoLpSupply))
    );
    assert_eq!(
        soroban_sdk::token::TokenClient::new(&env, &reward).balance(&provider),
        reward_balance
    );
}

#[test]
fn campaign_leftover_is_never_negative_when_full_or_overdistributed() {
    let (env, incentives, pool, lp, reward, provider, gov, treasury) = setup();
    let client = IncentiveCampaignsClient::new(&env, &incentives);
    let id = create_campaign(&env, &incentives, &pool, &lp, &reward, &provider, &gov);
    env.ledger()
        .set_timestamp(2_000 + DEFAULT_CLAIM_GRACE_SECS + 1);
    env.as_contract(&incentives, || {
        let mut campaign: Campaign = env
            .storage()
            .persistent()
            .get(&DataKey::Campaign(id))
            .unwrap();
        campaign.total_distributed = campaign.funding_amount + 500;
        env.storage()
            .persistent()
            .set(&DataKey::Campaign(id), &campaign);
    });
    let balance = soroban_sdk::token::TokenClient::new(&env, &reward).balance(&treasury);
    assert_eq!(
        client.try_recover_leftover_funds(&gov, &id, &treasury),
        Err(Ok(IncentiveError::NoLeftoverFunds))
    );
    assert_eq!(
        soroban_sdk::token::TokenClient::new(&env, &reward).balance(&treasury),
        balance
    );
}

#[test]
fn second_recovery_has_no_extra_event() {
    let (env, incentives, pool, lp, reward, provider, gov, treasury) = setup();
    let client = IncentiveCampaignsClient::new(&env, &incentives);
    let id = create_campaign(&env, &incentives, &pool, &lp, &reward, &provider, &gov);
    env.ledger()
        .set_timestamp(2_000 + DEFAULT_CLAIM_GRACE_SECS + 1);
    client.recover_leftover_funds(&gov, &id, &treasury);
    let events = leftover_recovery_event_count(&env, &incentives);
    let _ = client.try_recover_leftover_funds(&gov, &id, &treasury);
    assert_eq!(leftover_recovery_event_count(&env, &incentives), events);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn conservation_property_claims_plus_recovery_never_exceed_funding(
        distributed in 0_i128..=100_000,
    ) {
        let (env, incentives, pool, lp, reward, provider, gov, treasury) = setup();
        let client = IncentiveCampaignsClient::new(&env, &incentives);
        let id = create_campaign(&env, &incentives, &pool, &lp, &reward, &provider, &gov);
        env.ledger().set_timestamp(2_000 + DEFAULT_CLAIM_GRACE_SECS + 1);
        env.as_contract(&incentives, || {
            let mut campaign: Campaign = env.storage().persistent().get(&DataKey::Campaign(id)).unwrap();
            campaign.total_distributed = distributed;
            env.storage().persistent().set(&DataKey::Campaign(id), &campaign);
        });

        let recovered = if distributed == 100_000 {
            prop_assert_eq!(
                client.try_recover_leftover_funds(&gov, &id, &treasury),
                Err(Ok(IncentiveError::NoLeftoverFunds))
            );
            0
        } else {
            client.recover_leftover_funds(&gov, &id, &treasury)
        };
        prop_assert!(distributed + recovered <= 100_000);
    }

    #[test]
    fn recovery_after_full_or_over_distribution_is_rejected(
        over_distribution in 0_i128..100_000,
    ) {
        let (env, incentives, pool, lp, reward, provider, gov, treasury) = setup();
        let client = IncentiveCampaignsClient::new(&env, &incentives);
        let id = create_campaign(&env, &incentives, &pool, &lp, &reward, &provider, &gov);
        env.ledger().set_timestamp(2_000 + DEFAULT_CLAIM_GRACE_SECS + 1);
        env.as_contract(&incentives, || {
            let mut campaign: Campaign = env.storage().persistent().get(&DataKey::Campaign(id)).unwrap();
            campaign.total_distributed = campaign.funding_amount + over_distribution;
            env.storage().persistent().set(&DataKey::Campaign(id), &campaign);
        });

        let reward_token = soroban_sdk::token::TokenClient::new(&env, &reward);
        let contract_balance = reward_token.balance(&incentives);
        let treasury_balance = reward_token.balance(&treasury);
        prop_assert_eq!(
            client.try_recover_leftover_funds(&gov, &id, &treasury),
            Err(Ok(IncentiveError::NoLeftoverFunds))
        );
        prop_assert_eq!(reward_token.balance(&incentives), contract_balance);
        prop_assert_eq!(reward_token.balance(&treasury), treasury_balance);
    }
}
