//! Guard regression tests for #1190.
//!
//! The creation-fee tests call through the admin path (permissioned mode, or
//! the admin as caller in permissionless mode) with a creation fee configured.
//! On that path a custom fee is allowed, so the standard-tier check never
//! fires and the `0..10_000` range guard in `validate_fee_policy` is the only
//! check that answers `InvalidFeeBps`; without it the call would fall through
//! to the fee window and report `FeeOutOfRange` instead. The positive controls
//! (`*_admin_path_*_custom_fee*`) show the same path accepts an in-range
//! custom fee.

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

struct Fx {
    env: Env,
    factory_addr: Address,
    admin: Address,
    creator: Address,
    fee_token: Address,
}

impl Fx {
    fn factory(&self) -> FactoryClient<'_> {
        FactoryClient::new(&self.env, &self.factory_addr)
    }
}

const CREATION_FEE: i128 = 100;
const STARTING_BALANCE: i128 = 1_000;

fn setup() -> Fx {
    let env = Env::default();
    env.budget().reset_unlimited();
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);
    let admin = Address::generate(&env);
    let creator = Address::generate(&env);
    let factory_addr = env.register_contract(None, Factory);
    let factory = FactoryClient::new(&env, &factory_addr);
    let amm_hash = env.deployer().upload_contract_wasm(amm::WASM);
    let token_hash = env.deployer().upload_contract_wasm(token::WASM);
    factory.initialize(&admin, &amm_hash, &token_hash);
    let fee_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let issuer = StellarAssetClient::new(&env, &fee_token);
    issuer.mint(&creator, &STARTING_BALANCE);
    issuer.mint(&admin, &STARTING_BALANCE);
    Fx {
        env,
        factory_addr,
        admin,
        creator,
        fee_token,
    }
}

/// Configure a creation fee so a charged call would move tokens.
fn configure_creation_fee(f: &Fx) {
    let factory = f.factory();
    factory.set_pool_creation_fee(&f.fee_token, &CREATION_FEE);
    factory.set_rate_limit(&0);
}

/// The two admin paths, on which only the range guard rejects a bad `fee_bps`.
#[derive(Clone, Copy, Debug)]
enum AdminPath {
    /// Permissioned mode: any `caller` goes through admin authorization.
    Permissioned,
    /// Permissionless mode with the admin as caller: the creation fee is
    /// charged (to the admin, who is also the treasury).
    AdminCallerPermissionless,
}

const ADMIN_PATHS: [AdminPath; 2] = [
    AdminPath::Permissioned,
    AdminPath::AdminCallerPermissionless,
];

/// Switch to `path` and return the caller to use on it.
fn enter(f: &Fx, path: AdminPath) -> Address {
    match path {
        AdminPath::Permissioned => {
            f.factory().set_permissionless_mode(&false);
            f.creator.clone()
        }
        AdminPath::AdminCallerPermissionless => {
            f.factory().set_permissionless_mode(&true);
            f.admin.clone()
        }
    }
}

/// Balances and registry state a rejected creation must leave untouched.
#[derive(Debug, PartialEq)]
struct CreationState {
    creator_balance: i128,
    treasury_balance: i128,
    pool_count: u64,
    cl_pool_count: u64,
    pools_for_pair: u32,
}

fn creation_state(f: &Fx, ta: &Address, tb: &Address) -> CreationState {
    let factory = f.factory();
    let fee_token = TokenClient::new(&f.env, &f.fee_token);
    CreationState {
        creator_balance: fee_token.balance(&f.creator),
        treasury_balance: fee_token.balance(&f.admin),
        pool_count: factory.get_pool_count(),
        cl_pool_count: factory.get_cl_pool_count(),
        pools_for_pair: factory.get_pools_for_pair(ta, tb).len(),
    }
}

fn pair(f: &Fx) -> (Address, Address) {
    (Address::generate(&f.env), Address::generate(&f.env))
}

fn assert_amm_fee_rejected_on_admin_paths(fee_bps: i128) {
    let f = setup();
    configure_creation_fee(&f);
    let (ta, tb) = pair(&f);
    for path in ADMIN_PATHS {
        let caller = enter(&f, path);
        let before = creation_state(&f, &ta, &tb);
        assert_eq!(
            f.factory()
                .try_create_pool_with_fee_bps(&caller, &ta, &tb, &fee_bps, &None),
            Err(Ok(FactoryError::InvalidFeeBps)),
            "fee_bps {fee_bps} on {path:?}"
        );
        assert_eq!(creation_state(&f, &ta, &tb), before, "{path:?}");
    }
}

fn assert_cl_fee_rejected_on_admin_paths(fee_bps: i128) {
    let f = setup();
    configure_creation_fee(&f);
    let (ta, tb) = pair(&f);
    for path in ADMIN_PATHS {
        let caller = enter(&f, path);
        let before = creation_state(&f, &ta, &tb);
        assert_eq!(
            f.factory()
                .try_create_cl_pool(&caller, &ta, &tb, &fee_bps, &0),
            Err(Ok(FactoryError::InvalidFeeBps)),
            "fee_bps {fee_bps} on {path:?}"
        );
        assert_eq!(creation_state(&f, &ta, &tb), before, "{path:?}");
    }
}

// ── initialize ───────────────────────────────────────────────────────────────

#[test]
fn initialize_twice_preserves_admin_and_hashes() {
    let f = setup();
    let factory = f.factory();
    let read_hashes = || {
        f.env.as_contract(&f.factory_addr, || {
            let instance = f.env.storage().instance();
            (
                instance.get::<DataKey, BytesN<32>>(&DataKey::AmmWasmHash),
                instance.get::<DataKey, BytesN<32>>(&DataKey::TokenWasmHash),
            )
        })
    };
    let hashes_before = read_hashes();
    assert!(hashes_before.0.is_some() && hashes_before.1.is_some());

    assert_eq!(
        factory.try_initialize(
            &Address::generate(&f.env),
            &BytesN::from_array(&f.env, &[9u8; 32]),
            &BytesN::from_array(&f.env, &[8u8; 32]),
        ),
        Err(Ok(FactoryError::AlreadyInitialized))
    );
    assert_eq!(factory.get_admin(), Some(f.admin.clone()));
    assert_eq!(read_hashes(), hashes_before);
}

// ── create_pool_with_fee_bps ─────────────────────────────────────────────────

#[test]
fn amm_pool_rejects_negative_fee_before_charge() {
    assert_amm_fee_rejected_on_admin_paths(-1);
}

#[test]
fn amm_pool_rejects_fee_above_100_percent_before_charge() {
    assert_amm_fee_rejected_on_admin_paths(10_001);
}

#[test]
fn amm_pool_rejects_exact_10000_fee_on_admin_path() {
    assert_amm_fee_rejected_on_admin_paths(10_000);
}

#[test]
fn amm_admin_path_accepts_custom_fee_inside_range() {
    let f = setup();
    configure_creation_fee(&f);
    let (ta, tb) = pair(&f);
    // 17 and 18 bps are not standard tiers, so only the admin path may use them.
    for (fee_bps, path) in [17_i128, 18].into_iter().zip(ADMIN_PATHS) {
        let caller = enter(&f, path);
        let before = creation_state(&f, &ta, &tb);
        f.factory()
            .create_pool_with_fee_bps(&caller, &ta, &tb, &fee_bps, &None);
        let after = creation_state(&f, &ta, &tb);
        assert_eq!(after.pool_count, before.pool_count + 1, "{path:?}");
        assert_eq!(after.pools_for_pair, before.pools_for_pair + 1, "{path:?}");
        // The admin pays the creation fee to itself, so no balance moves.
        assert_eq!(after.creator_balance, before.creator_balance, "{path:?}");
        assert_eq!(after.treasury_balance, before.treasury_balance, "{path:?}");
    }
}

// ── create_cl_pool ───────────────────────────────────────────────────────────

#[test]
fn cl_pool_rejects_negative_fee_before_charge() {
    assert_cl_fee_rejected_on_admin_paths(-1);
}

#[test]
fn cl_pool_rejects_fee_above_100_percent_before_charge() {
    assert_cl_fee_rejected_on_admin_paths(10_001);
}

#[test]
fn cl_pool_rejects_exact_10000_fee() {
    assert_cl_fee_rejected_on_admin_paths(10_000);
}

#[test]
fn cl_admin_path_passes_fee_validation_for_custom_fee() {
    let f = setup();
    configure_creation_fee(&f);
    let (ta, tb) = pair(&f);
    for path in ADMIN_PATHS {
        let caller = enter(&f, path);
        let before = creation_state(&f, &ta, &tb);
        // 17 bps is not a standard CL tier. On the admin path it passes fee
        // validation and only stops at the missing CL WASM hash, which is
        // checked after the creation fee is charged (and rolled back).
        assert_eq!(
            f.factory().try_create_cl_pool(&caller, &ta, &tb, &17, &0),
            Err(Ok(FactoryError::ClWasmNotSet)),
            "{path:?}"
        );
        assert_eq!(creation_state(&f, &ta, &tb), before, "{path:?}");
    }
}

// ── set_pool_creation_fee ────────────────────────────────────────────────────

fn assert_creation_fee_rejected(fee_amount: i128) {
    let f = setup();
    let factory = f.factory();
    factory.set_pool_creation_fee(&f.fee_token, &CREATION_FEE);
    let other_token = Address::generate(&f.env);
    for token in [&f.fee_token, &other_token] {
        assert_eq!(
            factory.try_set_pool_creation_fee(token, &fee_amount),
            Err(Ok(FactoryError::FeeNotConfigured))
        );
        assert_eq!(
            factory.get_pool_creation_fee(),
            Some((f.fee_token.clone(), CREATION_FEE))
        );
    }
}

#[test]
fn set_pool_creation_fee_rejects_zero() {
    assert_creation_fee_rejected(0);
}

#[test]
fn set_pool_creation_fee_rejects_negative() {
    assert_creation_fee_rejected(-1);
}

#[test]
fn set_pool_creation_fee_stores_positive_value() {
    let f = setup();
    let factory = f.factory();
    assert_eq!(factory.get_pool_creation_fee(), None);
    factory.set_pool_creation_fee(&f.fee_token, &123);
    assert_eq!(
        factory.get_pool_creation_fee(),
        Some((f.fee_token.clone(), 123))
    );
}

// ── set_global_fee / set_global_fee_paginated ────────────────────────────────

/// A factory-administered AMM pool with a 30 bps swap fee.
fn admin_pool(f: &Fx) -> Address {
    let (ta, tb) = pair(f);
    let (pool, _) = f
        .factory()
        .create_pool_with_fee_bps(&f.admin, &ta, &tb, &30, &None);
    pool
}

fn assert_global_fee_rejected(protocol_fee_bps: i128) {
    let f = setup();
    let factory = f.factory();
    let pool = admin_pool(&f);
    let treasury = Address::generate(&f.env);
    factory.set_treasury(&f.admin, &treasury, &7);
    assert_eq!(factory.get_pool_fee_status(&pool), Some((7, true)));

    assert_eq!(
        factory.try_set_global_fee(&f.admin, &protocol_fee_bps),
        Err(Ok(FactoryError::InvalidFeeBps))
    );
    // No pool was touched and the stored rate is unchanged.
    assert_eq!(factory.get_pool_fee_status(&pool), Some((7, true)));
    assert_eq!(factory.get_treasury(), Some((treasury, 7)));
}

#[test]
fn set_global_fee_rejects_negative() {
    assert_global_fee_rejected(-1);
}

#[test]
fn set_global_fee_rejects_above_100_percent() {
    assert_global_fee_rejected(10_001);
}

#[test]
fn set_global_fee_accepts_zero_and_100_percent() {
    let f = setup();
    let factory = f.factory();
    let pool = admin_pool(&f);
    let treasury = Address::generate(&f.env);
    factory.set_treasury(&f.admin, &treasury, &0);

    // 100% is accepted and clamped to the pool's swap fee minus one.
    assert_eq!(factory.set_global_fee(&f.admin, &10_000), 1);
    assert_eq!(factory.get_pool_fee_status(&pool), Some((29, true)));
    assert_eq!(factory.get_treasury(), Some((treasury.clone(), 10_000)));

    assert_eq!(factory.set_global_fee(&f.admin, &0), 1);
    assert_eq!(factory.get_pool_fee_status(&pool), Some((0, true)));
    assert_eq!(factory.get_treasury(), Some((treasury, 0)));
}

#[test]
fn global_fee_accepts_nonzero_in_range_value() {
    let f = setup();
    let factory = f.factory();
    let treasury = Address::generate(&f.env);
    factory.set_treasury(&f.admin, &treasury, &0);
    assert_eq!(factory.set_global_fee(&f.admin, &1), 0);
    assert_eq!(factory.get_treasury(), Some((treasury, 1)));
}

#[test]
fn paginated_global_fee_requires_treasury() {
    let f = setup();
    let factory = f.factory();
    let pool = admin_pool(&f);
    // Without a treasury the pool keeps its creation defaults: no protocol fee
    // and the admin, not the factory, as fee recipient.
    assert_eq!(factory.get_pool_fee_status(&pool), Some((0, false)));

    assert_eq!(
        factory.try_set_global_fee_paginated(&f.admin, &0, &10),
        Err(Ok(FactoryError::FeeNotConfigured))
    );
    assert_eq!(factory.get_pool_fee_status(&pool), Some((0, false)));
    assert_eq!(factory.get_treasury(), None);
}

#[test]
fn paginated_global_fee_propagates_once_treasury_is_set() {
    let f = setup();
    let factory = f.factory();
    let pool = admin_pool(&f);
    let treasury = Address::generate(&f.env);
    factory.set_treasury(&f.admin, &treasury, &7);
    // An empty page touches nothing; the page holding the pool applies the
    // stored rate to it.
    assert_eq!(factory.set_global_fee_paginated(&f.admin, &1, &10), 0);
    assert_eq!(factory.set_global_fee_paginated(&f.admin, &0, &10), 1);
    assert_eq!(factory.get_pool_fee_status(&pool), Some((7, true)));
}
