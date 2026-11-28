//! Guard regression tests for #1191: `NoLeftoverFunds` in
//! `recover_leftover_funds` (the #547 repeated-recovery scenario) and
//! `NoLpSupply` in the legacy `claim_rewards` path.
//!
//! Recovery tests drive real stakes and claims. The campaign pays
//! `RATE * (END - START) == FUNDING`, so a stake held over the whole window
//! and claimed after `END` distributes the funding exactly and leaves a
//! leftover of zero by construction.

extern crate std;

use super::*;
use amm::{AmmPool, AmmPoolClient};
use proptest::prelude::*;
use proptest::test_runner::{RngSeed, TestCaseError};
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env, IntoVal,
};
use std::vec::Vec as StdVec;
use token::{LpToken, LpTokenClient};

const START: u64 = 1_000;
const END: u64 = 2_000;
const RATE: i128 = 100;
const FUNDING: i128 = RATE * (END - START) as i128;
/// First timestamp at which recovery is allowed for a default-grace campaign.
const RECOVERY_TIME: u64 = END + DEFAULT_CLAIM_GRACE_SECS + 1;
/// LP shares the provider receives from the pool's first deposit
/// (10_000 minus the 1_000 the AMM locks).
const PROVIDER_LP: i128 = 9_000;

struct Fx {
    env: Env,
    incentives: Address,
    pool: Address,
    lp: Address,
    reward: Address,
    provider: Address,
    gov: Address,
    treasury: Address,
}

impl Fx {
    fn client(&self) -> IncentiveCampaignsClient<'_> {
        IncentiveCampaignsClient::new(&self.env, &self.incentives)
    }

    fn lp_token(&self) -> LpTokenClient<'_> {
        LpTokenClient::new(&self.env, &self.lp)
    }

    fn reward_balance(&self, who: &Address) -> i128 {
        TokenClient::new(&self.env, &self.reward).balance(who)
    }

    fn set_time(&self, timestamp: u64) {
        self.env.ledger().set_timestamp(timestamp);
    }

    fn campaign(&self, id: u64) -> Campaign {
        self.client().get_campaign(&id)
    }

    /// `leftover_recovered` events published by the incentives contract.
    fn recovery_events(&self) -> usize {
        let wanted: soroban_sdk::Vec<soroban_sdk::Val> =
            (Symbol::new(&self.env, "leftover_recovered"),).into_val(&self.env);
        self.env
            .events()
            .all()
            .iter()
            .filter(|event| event.0 == self.incentives && event.1 == wanted)
            .count()
    }

    /// Reward-token balances of everyone a recovery or claim could pay.
    fn balances(&self, others: &[&Address]) -> StdVec<i128> {
        let mut who = std::vec![&self.incentives, &self.gov, &self.treasury, &self.provider];
        who.extend_from_slice(others);
        who.into_iter().map(|a| self.reward_balance(a)).collect()
    }
}

fn setup() -> Fx {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let gov = Address::generate(&env);
    let provider = Address::generate(&env);
    let treasury = Address::generate(&env);
    let pool = env.register_contract(None, AmmPool);
    let lp = env.register_contract(None, LpToken);
    LpTokenClient::new(&env, &lp).initialize(
        &pool,
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
    let amm = AmmPoolClient::new(&env, &pool);
    amm.initialize(&gov, &ta, &tb, &lp, &30, &gov, &0);
    StellarAssetClient::new(&env, &ta).mint(&provider, &10_000);
    StellarAssetClient::new(&env, &tb).mint(&provider, &10_000);
    amm.add_liquidity(&provider, &10_000, &10_000, &0, &u64::MAX);
    StellarAssetClient::new(&env, &reward).mint(&gov, &1_000_000);
    let incentives = env.register_contract(None, IncentiveCampaigns);
    IncentiveCampaignsClient::new(&env, &incentives).initialize(&gov);
    let f = Fx {
        env,
        incentives,
        pool,
        lp,
        reward,
        provider,
        gov,
        treasury,
    };
    assert_eq!(f.lp_token().balance(&f.provider), PROVIDER_LP);
    f
}

fn create_campaign(f: &Fx) -> u64 {
    f.client().create_campaign(
        &f.gov, &f.pool, &f.lp, &f.reward, &START, &END, &RATE, &FUNDING,
    )
}

/// A campaign in which the provider stakes 100 LP from `START`.
fn staked_campaign(f: &Fx) -> u64 {
    let id = create_campaign(f);
    f.client().stake(&f.provider, &id, &100);
    id
}

/// Claim everything the provider accrued over the whole window, distributing
/// the full funding. Returns the amount claimed.
fn distribute_everything(f: &Fx, id: u64) -> i128 {
    f.set_time(END + 1);
    let claimed = f.client().claim_rewards(&f.provider, &id);
    assert_eq!(claimed, FUNDING);
    assert_eq!(
        f.campaign(id).total_distributed,
        f.campaign(id).funding_amount
    );
    claimed
}

/// Overwrite `total_distributed`. Claims are clamped to
/// `funding_amount - total_distributed`, so no sequence of real calls can push
/// `total_distributed` above `funding_amount`; only legacy or corrupted
/// accounting can. The guard must still answer with `NoLeftoverFunds` instead
/// of attempting a negative transfer, so this is the one place the tests
/// write campaign state directly.
fn force_total_distributed(f: &Fx, id: u64, total_distributed: i128) {
    f.env.as_contract(&f.incentives, || {
        let key = DataKey::Campaign(id);
        let mut campaign: Campaign = f.env.storage().persistent().get(&key).unwrap();
        campaign.total_distributed = total_distributed;
        f.env.storage().persistent().set(&key, &campaign);
    });
}

/// Drop the stake marker, turning a campaign into a pre-#1115 one that uses
/// the live-balance accounting. `create_campaign` only makes stake-based
/// campaigns, and `NoLpSupply` lives on the legacy path, so this mirrors the
/// `make_legacy` helper of the main test module.
fn make_legacy(f: &Fx, id: u64) {
    f.env.as_contract(&f.incentives, || {
        f.env
            .storage()
            .persistent()
            .remove(&DataKey::StakeBased(id));
    });
}

/// Redeem every LP share so `total_supply` reaches zero through real burns:
/// the provider withdraws its shares from the pool, and the pool (the LP
/// token's admin) burns the `MINIMUM_LIQUIDITY` shares it locked on the first
/// deposit, which no holder can redeem.
fn redeem_all_lp(f: &Fx) {
    AmmPoolClient::new(&f.env, &f.pool).remove_liquidity(
        &f.provider,
        &PROVIDER_LP,
        &0,
        &0,
        &u64::MAX,
    );
    let lp = f.lp_token();
    lp.burn(&f.pool, &lp.balance(&f.pool));
    assert_eq!(lp.total_supply(), 0);
    assert_eq!(lp.balance(&f.provider), 0);
}

/// A legacy campaign whose LP supply has been redeemed to zero while the LP
/// token still reports a balance for the provider.
///
/// `LpToken` keeps `total_supply` equal to the sum of balances, so a holder
/// balance with zero supply cannot be produced by calls to it: with a
/// consistent token, a holder with no LP stops at `NoLpBalance` first (see
/// `claim_after_all_lp_is_redeemed_returns_typed_error`). The supply is
/// redeemed with real burns; the only direct write models an LP token that
/// keeps reporting the holder's balance after its supply was redeemed, which
/// is the case the guard exists for.
fn legacy_campaign_with_redeemed_supply(f: &Fx, register_first: bool) -> u64 {
    let id = create_campaign(f);
    make_legacy(f, id);
    if register_first {
        // The first legacy claim registers the provider and pays nothing.
        assert_eq!(f.client().claim_rewards(&f.provider, &id), 0);
    }
    f.set_time(START + 500);
    redeem_all_lp(f);
    f.env.as_contract(&f.lp, || {
        f.env
            .storage()
            .persistent()
            .set(&token::DataKey::Balance(f.provider.clone()), &PROVIDER_LP);
    });
    assert_eq!(f.lp_token().balance(&f.provider), PROVIDER_LP);
    assert_eq!(f.lp_token().total_supply(), 0);
    id
}

// ── recover_leftover_funds ───────────────────────────────────────────────────

#[test]
fn repeated_recovery_leaves_governance_and_contract_balances_unchanged() {
    let f = setup();
    let client = f.client();
    let id = staked_campaign(&f);
    f.set_time(START + 500);
    let claimed = client.claim_rewards(&f.provider, &id);
    assert_eq!(claimed, FUNDING / 2);

    f.set_time(RECOVERY_TIME);
    let contract_before = f.reward_balance(&f.incentives);
    let gov_before = f.reward_balance(&f.gov);
    let treasury_before = f.reward_balance(&f.treasury);
    let events_before = f.recovery_events();

    let recovered = client.recover_leftover_funds(&f.gov, &id, &f.treasury);
    assert_eq!(recovered, FUNDING - claimed);
    assert_eq!(f.reward_balance(&f.incentives), contract_before - recovered);
    assert_eq!(f.reward_balance(&f.treasury), treasury_before + recovered);
    assert_eq!(f.reward_balance(&f.gov), gov_before);
    assert_eq!(f.recovery_events(), events_before + 1);

    // A second recovery, to the same recipient or to governance itself, moves
    // nothing and emits no recovery event.
    let after_first = f.balances(&[]);
    for recipient in [&f.treasury, &f.gov] {
        assert_eq!(
            client.try_recover_leftover_funds(&f.gov, &id, recipient),
            Err(Ok(IncentiveError::CampaignInactive))
        );
        assert_eq!(f.balances(&[]), after_first);
        assert_eq!(f.recovery_events(), events_before + 1);
    }
}

#[test]
fn full_distribution_recovery_returns_no_leftover() {
    let f = setup();
    let client = f.client();
    let id = staked_campaign(&f);
    distribute_everything(&f, id);
    f.set_time(RECOVERY_TIME);
    let before = f.balances(&[]);
    let events_before = f.recovery_events();

    assert_eq!(
        client.try_recover_leftover_funds(&f.gov, &id, &f.treasury),
        Err(Ok(IncentiveError::NoLeftoverFunds))
    );
    assert_eq!(f.balances(&[]), before);
    assert_eq!(f.recovery_events(), events_before);
    // The rejected recovery did not wind the campaign down either.
    assert!(f.campaign(id).active);
}

#[test]
fn overdistribution_recovery_returns_no_leftover() {
    let f = setup();
    let client = f.client();
    let id = staked_campaign(&f);
    // The rounding corner: one unit more than the funding was accounted.
    force_total_distributed(&f, id, FUNDING + 1);
    f.set_time(RECOVERY_TIME);
    let before = f.balances(&[]);
    assert_eq!(
        client.try_recover_leftover_funds(&f.gov, &id, &f.treasury),
        Err(Ok(IncentiveError::NoLeftoverFunds))
    );
    assert_eq!(f.balances(&[]), before);
}

#[test]
fn campaign_leftover_is_never_negative_when_full_or_overdistributed() {
    let f = setup();
    let client = f.client();
    let id = staked_campaign(&f);
    distribute_everything(&f, id);
    f.set_time(RECOVERY_TIME);
    let before = f.balances(&[]);
    for total_distributed in [FUNDING, FUNDING + 500] {
        force_total_distributed(&f, id, total_distributed);
        assert_eq!(
            client.try_recover_leftover_funds(&f.gov, &id, &f.treasury),
            Err(Ok(IncentiveError::NoLeftoverFunds)),
            "total_distributed {total_distributed}"
        );
        assert_eq!(f.balances(&[]), before);
        assert!(f.campaign(id).active);
    }
}

#[test]
fn recovery_event_is_emitted_once() {
    let f = setup();
    let id = staked_campaign(&f);
    f.set_time(RECOVERY_TIME);
    let before = f.recovery_events();
    f.client().recover_leftover_funds(&f.gov, &id, &f.treasury);
    assert_eq!(f.recovery_events(), before + 1);
}

#[test]
fn second_recovery_has_no_extra_event() {
    let f = setup();
    let client = f.client();
    let id = staked_campaign(&f);
    f.set_time(RECOVERY_TIME);
    client.recover_leftover_funds(&f.gov, &id, &f.treasury);
    let events = f.recovery_events();
    assert_eq!(
        client.try_recover_leftover_funds(&f.gov, &id, &f.treasury),
        Err(Ok(IncentiveError::CampaignInactive))
    );
    assert_eq!(f.recovery_events(), events);
}

#[test]
fn recovery_deactivates_campaign_and_later_claim_is_inactive() {
    let f = setup();
    let client = f.client();
    let id = staked_campaign(&f);
    f.set_time(RECOVERY_TIME);
    // The provider never claimed, so it still has pending rewards.
    assert_eq!(client.get_pending_rewards(&id, &f.provider), FUNDING);
    let recovered = client.recover_leftover_funds(&f.gov, &id, &f.treasury);
    assert_eq!(recovered, FUNDING);

    let campaign = f.campaign(id);
    assert!(!campaign.active);
    assert_eq!(campaign.funding_amount, FUNDING);
    assert_eq!(campaign.total_distributed, 0);

    let before = f.balances(&[]);
    assert_eq!(
        client.try_claim_rewards(&f.provider, &id),
        Err(Ok(IncentiveError::CampaignInactive))
    );
    assert_eq!(f.balances(&[]), before);
    // Principal is never held hostage: the stake can still be withdrawn.
    client.unstake(&f.provider, &id, &100);
    assert_eq!(f.lp_token().balance(&f.provider), PROVIDER_LP);
}

// ── claim_rewards with zero LP supply ────────────────────────────────────────

#[test]
fn zero_lp_supply_returns_typed_error_without_claimed_accrual() {
    let f = setup();
    let client = f.client();
    let id = legacy_campaign_with_redeemed_supply(&f, true);
    let accrual_before = client.get_campaign_accrual(&id);
    let campaign_before = f.campaign(id);
    let lp_before = f.lp_token().balance(&f.provider);
    assert_eq!(
        client.try_claim_rewards(&f.provider, &id),
        Err(Ok(IncentiveError::NoLpSupply))
    );
    assert_eq!(f.lp_token().balance(&f.provider), lp_before);
    assert_eq!(client.get_campaign_accrual(&id), accrual_before);
    assert_eq!(f.campaign(id), campaign_before);
}

#[test]
fn holder_balance_with_zero_total_supply_cannot_panic_claim() {
    let f = setup();
    let client = f.client();
    let id = legacy_campaign_with_redeemed_supply(&f, false);
    // A typed contract error, not a trap from the per-share division.
    assert_eq!(
        client.try_claim_rewards(&f.provider, &id),
        Err(Ok(IncentiveError::NoLpSupply))
    );
    assert_eq!(client.get_distribution_count(&id), 0);
}

#[test]
fn zero_supply_claim_does_not_change_reward_balance() {
    let f = setup();
    let client = f.client();
    let id = legacy_campaign_with_redeemed_supply(&f, true);
    let before = f.balances(&[]);
    assert_eq!(
        client.try_claim_rewards(&f.provider, &id),
        Err(Ok(IncentiveError::NoLpSupply))
    );
    assert_eq!(f.balances(&[]), before);
}

#[test]
fn claim_after_all_lp_is_redeemed_returns_typed_error() {
    let f = setup();
    let client = f.client();
    let id = create_campaign(&f);
    make_legacy(&f, id);
    assert_eq!(client.claim_rewards(&f.provider, &id), 0);
    f.set_time(START + 500);
    redeem_all_lp(&f);
    // With a consistent LP token the provider holds nothing once the supply is
    // gone, so the balance check answers first; the call still fails with a
    // typed error rather than trapping.
    let before = f.balances(&[]);
    assert_eq!(
        client.try_claim_rewards(&f.provider, &id),
        Err(Ok(IncentiveError::NoLpBalance))
    );
    assert_eq!(f.balances(&[]), before);
}

// ── Property tests ───────────────────────────────────────────────────────────

/// A random claim schedule over `stakers` equal stakes of `stake` LP each.
///
/// `stakers` is drawn from {1, 2, 4, 5} and `stake` from powers of ten, so
/// `stakers * stake` divides `RATE * PRECISION` and `stakers` divides `RATE`:
/// every accrual step and every payout is then exact, and claiming everything
/// after `END` distributes exactly `FUNDING` with no rounding dust.
#[derive(Clone, Debug)]
struct ClaimPlan {
    stakers: usize,
    stake: i128,
    /// `(staker index, seconds after START)`, applied in time order.
    claims: StdVec<(usize, u64)>,
    /// Every staker claims after `END`, distributing the full funding.
    claim_all_after_end: bool,
}

fn claim_plan(claim_all_after_end: BoxedStrategy<bool>) -> impl Strategy<Value = ClaimPlan> {
    (
        prop::sample::select(std::vec![1_usize, 2, 4, 5]),
        prop::sample::select(std::vec![1_i128, 10, 100, 1_000]),
        prop::collection::vec((0_usize..5, 0_u64..=(END - START) + 200), 0..8),
        claim_all_after_end,
    )
        .prop_map(|(stakers, stake, mut claims, claim_all_after_end)| {
            claims.sort_by_key(|(_, offset)| *offset);
            ClaimPlan {
                stakers,
                stake,
                claims,
                claim_all_after_end,
            }
        })
}

fn deterministic_config(seed: u64) -> ProptestConfig {
    ProptestConfig {
        cases: 64,
        failure_persistence: None,
        rng_seed: RngSeed::Fixed(seed),
        ..ProptestConfig::default()
    }
}

/// Stake, then run the plan's claims. Returns the stakers and the total claimed.
fn run_claims(f: &Fx, id: u64, plan: &ClaimPlan) -> Result<(StdVec<Address>, i128), TestCaseError> {
    let client = f.client();
    let stakers: StdVec<Address> = (0..plan.stakers)
        .map(|_| {
            let staker = Address::generate(&f.env);
            f.lp_token().transfer(&f.provider, &staker, &plan.stake);
            client.stake(&staker, &id, &plan.stake);
            staker
        })
        .collect();

    let mut claimed = 0_i128;
    let mut claim = |staker: &Address| -> Result<(), TestCaseError> {
        match client.try_claim_rewards(staker, &id) {
            Ok(Ok(amount)) => {
                prop_assert!(amount > 0);
                claimed += amount;
            }
            Err(Ok(IncentiveError::NoPendingRewards)) => {}
            other => {
                return Err(TestCaseError::fail(std::format!(
                    "unexpected claim result {other:?}"
                )));
            }
        }
        Ok(())
    };
    for (index, offset) in &plan.claims {
        f.set_time(START + offset);
        claim(&stakers[index % plan.stakers])?;
    }
    if plan.claim_all_after_end {
        f.set_time(f.env.ledger().timestamp().max(END + 1));
        for staker in &stakers {
            claim(staker)?;
        }
    }

    let paid: i128 = stakers.iter().map(|s| f.reward_balance(s)).sum();
    prop_assert_eq!(paid, claimed);
    prop_assert_eq!(f.campaign(id).total_distributed, claimed);
    if plan.claim_all_after_end {
        prop_assert_eq!(claimed, FUNDING);
    }
    Ok((stakers, claimed))
}

proptest! {
    #![proptest_config(deterministic_config(0x1191_0001))]

    /// Sum of claims plus the recovered leftover never exceeds the funding.
    ///
    /// Half the plans claim everything after `END`, so the leftover-zero case
    /// is reached in about half of the cases; there a recovery must be
    /// rejected with `NoLeftoverFunds` and move nothing. Elsewhere the single
    /// recovery must pay exactly the positive leftover, and a second one must
    /// move nothing.
    #[test]
    fn conservation_property_claims_plus_recovery_never_exceed_funding(
        plan in claim_plan(any::<bool>().boxed()),
    ) {
        let f = setup();
        let client = f.client();
        let id = create_campaign(&f);
        let (stakers, claimed) = run_claims(&f, id, &plan)?;

        f.set_time(RECOVERY_TIME);
        let before = f.balances(&stakers.iter().collect::<StdVec<_>>());
        let leftover = FUNDING - claimed;
        let recovered = match client.try_recover_leftover_funds(&f.gov, &id, &f.treasury) {
            Ok(Ok(recovered)) => {
                prop_assert!(recovered > 0, "a successful recovery must move funds");
                prop_assert_eq!(recovered, leftover);
                prop_assert_eq!(f.reward_balance(&f.treasury), before[2] + recovered);
                prop_assert_eq!(f.reward_balance(&f.incentives), before[0] - recovered);
                recovered
            }
            Err(Ok(IncentiveError::NoLeftoverFunds)) => {
                prop_assert_eq!(leftover, 0);
                prop_assert_eq!(f.balances(&stakers.iter().collect::<StdVec<_>>()), before.clone());
                0
            }
            other => {
                return Err(TestCaseError::fail(std::format!(
                    "unexpected recovery result {other:?}"
                )));
            }
        };

        let after_first = f.balances(&stakers.iter().collect::<StdVec<_>>());
        let second = client.try_recover_leftover_funds(&f.gov, &id, &f.treasury);
        prop_assert!(
            second == Err(Ok(IncentiveError::CampaignInactive))
                || second == Err(Ok(IncentiveError::NoLeftoverFunds)),
            "unexpected second recovery result {:?}", second
        );
        prop_assert_eq!(f.balances(&stakers.iter().collect::<StdVec<_>>()), after_first);

        prop_assert!(claimed + recovered <= FUNDING);
        prop_assert_eq!(f.reward_balance(&f.incentives), FUNDING - claimed - recovered);
    }

    /// Recovery after a full distribution through real claims is rejected.
    #[test]
    fn recovery_after_full_distribution_is_rejected(
        plan in claim_plan(Just(true).boxed()),
    ) {
        let f = setup();
        let client = f.client();
        let id = create_campaign(&f);
        let (stakers, claimed) = run_claims(&f, id, &plan)?;
        prop_assert_eq!(claimed, FUNDING);

        f.set_time(RECOVERY_TIME);
        let others: StdVec<&Address> = stakers.iter().collect();
        let before = f.balances(&others);
        let events_before = f.recovery_events();
        prop_assert_eq!(
            client.try_recover_leftover_funds(&f.gov, &id, &f.treasury),
            Err(Ok(IncentiveError::NoLeftoverFunds))
        );
        prop_assert_eq!(f.balances(&others), before);
        prop_assert_eq!(f.recovery_events(), events_before);
        prop_assert!(f.campaign(id).active);
    }
}
