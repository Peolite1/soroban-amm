//! Regression tests that every admin-only `Governance` entrypoint demands the
//! right signature.
//!
//! These tests never call `env.mock_all_auths()` around the call under test.
//! Each entrypoint is invoked with (a) no authorization, (b) only a stranger's
//! authorization, and (c) only the expected signer's authorization, so deleting
//! a `require_auth()` makes at least one case fail. Functions that also take an
//! explicit `admin` argument get a second check that a stranger passed in that
//! slot is refused with `GovernanceError::Unauthorized`.

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    xdr::{ContractExecutable, Hash, LedgerEntryData, LedgerKey, ScAddress, ScVal},
    Address, Env, IntoVal, Symbol, Val,
};

const WASM_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/wasm32v1-none/release/governance.wasm"
);

fn invoke_catching(env: &Env, invoke: &MockAuthInvoke) -> Result<(), std::string::String> {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(std::boxed::Box::new(|_| {}));
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.invoke_contract::<Val>(
            invoke.contract,
            &Symbol::new(env, invoke.fn_name),
            invoke.args.clone(),
        );
    }));
    std::panic::set_hook(prev_hook);
    res.map_err(|err| {
        if let Some(s) = err.downcast_ref::<&str>() {
            std::string::String::from(*s)
        } else if let Some(s) = err.downcast_ref::<std::string::String>() {
            s.clone()
        } else {
            std::string::String::new()
        }
    })
}

fn assert_auth_err(env: &Env, invoke: &MockAuthInvoke, context: &str) {
    let msg = invoke_catching(env, invoke).expect_err(&std::format!(
        "{} should fail under {}",
        invoke.fn_name,
        context
    ));
    assert!(
        msg.contains("Error(Auth, InvalidAction)"),
        "{} under {}: expected Error(Auth, InvalidAction), got: {}",
        invoke.fn_name,
        context,
        msg
    );
}

/// No authorizations and a stranger's alone must fail with an auth error;
/// `signer_expected`'s alone must succeed.
fn assert_requires_auth(env: &Env, signer_expected: &Address, invoke: &MockAuthInvoke) {
    let stranger = Address::generate(env);

    env.set_auths(&[]);
    assert_auth_err(env, invoke, "no authorizations");

    env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke,
    }]);
    assert_auth_err(env, invoke, "a stranger's authorization");

    env.mock_auths(&[MockAuth {
        address: signer_expected,
        invoke,
    }]);
    if let Err(msg) = invoke_catching(env, invoke) {
        panic!(
            "{} should succeed with authorization from {:?}, failed: {}",
            invoke.fn_name, signer_expected, msg
        );
    }
}

struct Fixture {
    env: Env,
    addr: Address,
    admin: Address,
}

impl Fixture {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        env.budget().reset_unlimited();
        let admin = Address::generate(&env);
        let addr = env.register_contract(None, Governance);
        GovernanceClient::new(&env, &addr).initialize(
            &admin,
            &Address::generate(&env), // amm pool: never called by the admin entrypoints
            &Address::generate(&env), // lp token
            &(7 * 24 * 60 * 60_u64),
            &(2 * 24 * 60 * 60_u64),
            &1_000_i128,
            &100_i128,
        );
        Self { env, addr, admin }
    }

    fn client(&self) -> GovernanceClient<'_> {
        GovernanceClient::new(&self.env, &self.addr)
    }

    fn check<A: IntoVal<Env, soroban_sdk::Vec<Val>>>(
        &self,
        signer: &Address,
        fn_name: &'static str,
        args: A,
    ) {
        let invoke = MockAuthInvoke {
            contract: &self.addr,
            fn_name,
            args: args.into_val(&self.env),
            sub_invokes: &[],
        };
        assert_requires_auth(&self.env, signer, &invoke);
    }
}

#[test]
fn test_set_quorum_decay_bps_per_day_requires_admin_auth() {
    let f = Fixture::new();
    f.check(&f.admin, "set_quorum_decay_bps_per_day", (25_i128,));
}

#[test]
fn test_set_min_proposer_stake_bps_requires_admin_auth() {
    let f = Fixture::new();
    f.check(&f.admin, "set_min_proposer_stake_bps", (200_i128,));
}

#[test]
fn test_set_timelock_delay_requires_admin_auth() {
    let f = Fixture::new();
    f.check(&f.admin, "set_timelock_delay", (3 * 24 * 60 * 60_u64,));
}

#[test]
fn test_set_veto_multisig_requires_admin_auth() {
    let f = Fixture::new();
    let multisig = Address::generate(&f.env);
    f.check(&f.admin, "set_veto_multisig", (multisig,));
}

/// `veto` takes no caller argument: it authorizes the address stored by
/// `set_veto_multisig`, so any other caller is stopped by the host auth check.
/// It must never report `GovernanceError::NotVetoMultisig` — that discriminant
/// is reserved and unreachable (issue #1326), and a typed error would make
/// `assert_auth_err` fail here instead.
#[test]
fn test_veto_requires_stored_multisig_auth() {
    let f = Fixture::new();
    let multisig = Address::generate(&f.env);
    let stranger = Address::generate(&f.env);
    f.client().set_veto_multisig(&multisig);

    let invoke = MockAuthInvoke {
        contract: &f.addr,
        fn_name: "veto",
        args: (0_u32,).into_val(&f.env),
        sub_invokes: &[],
    };

    f.env.set_auths(&[]);
    assert_auth_err(&f.env, &invoke, "no authorizations");

    f.env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &invoke,
    }]);
    assert_auth_err(&f.env, &invoke, "a non-multisig caller's authorization");
}

#[test]
fn test_propose_admin_requires_admin_auth() {
    let f = Fixture::new();
    let nominee = Address::generate(&f.env);
    f.check(
        &f.admin,
        "propose_admin",
        (f.admin.clone(), nominee.clone()),
    );
    let stranger = Address::generate(&f.env);
    f.env.mock_all_auths();
    assert_eq!(
        f.client().try_propose_admin(&stranger, &nominee),
        Err(Ok(GovernanceError::Unauthorized))
    );
}

#[test]
fn test_accept_admin_requires_nominee_auth() {
    let f = Fixture::new();
    let nominee = Address::generate(&f.env);
    let stranger = Address::generate(&f.env);
    f.client().propose_admin(&f.admin, &nominee);
    assert_eq!(
        f.client().try_accept_admin(&stranger),
        Err(Ok(GovernanceError::Unauthorized))
    );
    f.check(&nominee, "accept_admin", (nominee.clone(),));
    assert_eq!(f.client().get_admin(), nominee);
}

/// The executable (WASM hash, or the built-in marker for a natively
/// registered test contract) recorded in the contract's instance entry.
fn contract_executable(env: &Env, contract_id: &Address) -> ContractExecutable {
    let contract_address = ScAddress::from(contract_id);
    env.to_ledger_snapshot()
        .ledger_entries
        .into_iter()
        .find_map(|(key, (entry, _))| match (*key, entry.data) {
            (LedgerKey::ContractData(key), LedgerEntryData::ContractData(data))
                if key.contract == contract_address
                    && key.key == ScVal::LedgerKeyContractInstance =>
            {
                match data.val {
                    ScVal::ContractInstance(instance) => Some(instance.executable),
                    _ => None,
                }
            }
            _ => None,
        })
        .expect("governance contract instance must exist")
}

fn upload_governance_wasm(env: &Env) -> soroban_sdk::BytesN<32> {
    let wasm = soroban_sdk::Bytes::from_slice(env, &std::fs::read(WASM_PATH).unwrap());
    env.deployer().upload_contract_wasm(wasm)
}

#[test]
fn test_upgrade_requires_admin_auth() {
    let f = Fixture::new();
    let hash = upload_governance_wasm(&f.env);
    f.check(&f.admin, "upgrade", (f.admin.clone(), hash));
}

/// Runs against the natively registered contract, before any upgrade: once
/// `upgrade` succeeds the contract executes the prebuilt WASM instead of this
/// crate's source, so a check placed after it would not test the source.
#[test]
fn test_upgrade_rejects_stranger_as_admin_without_code_change() {
    let f = Fixture::new();
    let hash = upload_governance_wasm(&f.env);
    let stranger = Address::generate(&f.env);
    let executable_before = contract_executable(&f.env, &f.addr);
    // Sanity check: a successful upgrade would change the executable.
    assert_ne!(
        executable_before,
        ContractExecutable::Wasm(Hash(hash.to_array()))
    );

    f.env.mock_all_auths();
    assert_eq!(
        f.client().try_upgrade(&stranger, &hash),
        Err(Ok(GovernanceError::Unauthorized))
    );
    assert_eq!(contract_executable(&f.env, &f.addr), executable_before);
    assert_eq!(f.client().get_admin(), f.admin);
}
