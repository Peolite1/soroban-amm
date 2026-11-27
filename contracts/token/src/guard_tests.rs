//! Guard regression tests for #1188.
//!
//! Every rejected call is wrapped in [`assert_unchanged`], which compares a
//! [`Snapshot`] taken before and after the call: balances, `TotalSupply`,
//! `Locked`, every `LockEntry`/`LockHolders` pair, the account and supply
//! checkpoint histories, the admin slots, the raw storage of the token
//! contract and the number of contract events. Snapshots are taken inside each
//! test, so the assertions do not depend on absolute event counts or on the
//! exact checkpoint layout.

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events as _},
    xdr::{LedgerEntryData, LedgerKey, ScAddress},
    Address, Env, String,
};
use std::vec::Vec as StdVec;

struct TokenFixture {
    env: Env,
    admin: Address,
    locker: Address,
    contract: Address,
}

impl TokenFixture {
    fn client(&self) -> LpTokenClient<'_> {
        LpTokenClient::new(&self.env, &self.contract)
    }
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

/// A checkpoint history: the tracker plus every retained checkpoint, oldest first.
type History = (Option<CheckpointTracker>, StdVec<Checkpoint>);

/// Everything a rejected call must leave untouched.
#[derive(Debug, PartialEq)]
struct Snapshot {
    total_supply: i128,
    balances: StdVec<i128>,
    locked: StdVec<i128>,
    /// `LockEntry(account, locker)` for every tracked account and every
    /// candidate locker (the configured locker plus every tracked account).
    lock_entries: StdVec<i128>,
    lock_holders: StdVec<Option<Vec<Address>>>,
    /// Per-account checkpoint histories: a new checkpoint would bump the
    /// tracker's `total_count` and append to the last page.
    account_checkpoints: StdVec<History>,
    supply_checkpoints: History,
    admin: Option<Address>,
    pending_admin: Option<Option<Address>>,
    /// Every ledger entry owned by the token contract (key and data, TTL
    /// ignored), as a catch-all for state the fields above do not name.
    contract_storage: StdVec<(LedgerKey, LedgerEntryData)>,
    events: u32,
}

fn read_history(env: &Env, tracker_key: &DataKey, page_key: impl Fn(u32) -> DataKey) -> History {
    let storage = env.storage().persistent();
    let tracker: Option<CheckpointTracker> = storage.get(tracker_key);
    let mut checkpoints = StdVec::new();
    if let Some(tracker) = &tracker {
        let end_page = tracker.total_count.div_ceil(LpToken::PAGE_SIZE);
        for page in tracker.start_page..end_page {
            let entries: Vec<Checkpoint> = storage.get(&page_key(page)).unwrap_or(Vec::new(env));
            checkpoints.extend(entries.iter());
        }
    }
    (tracker, checkpoints)
}

fn contract_storage(env: &Env, contract: &Address) -> StdVec<(LedgerKey, LedgerEntryData)> {
    let contract = ScAddress::from(contract);
    let mut entries: StdVec<_> = env
        .to_ledger_snapshot()
        .ledger_entries
        .into_iter()
        .filter_map(|(key, (entry, _))| match *key {
            LedgerKey::ContractData(ref data_key) if data_key.contract == contract => {
                Some((*key, entry.data))
            }
            _ => None,
        })
        .collect();
    assert!(
        !entries.is_empty(),
        "the token contract must own ledger entries"
    );
    entries.sort();
    entries
}

fn snapshot(f: &TokenFixture, accounts: &[&Address]) -> Snapshot {
    let env = &f.env;
    let mut lockers: StdVec<&Address> = std::vec![&f.locker];
    lockers.extend_from_slice(accounts);
    let core = env.as_contract(&f.contract, || {
        let persistent = env.storage().persistent();
        let instance = env.storage().instance();
        Snapshot {
            total_supply: instance.get(&DataKey::TotalSupply).unwrap_or(0),
            balances: accounts
                .iter()
                .map(|a| persistent.get(&DataKey::Balance((*a).clone())).unwrap_or(0))
                .collect(),
            locked: accounts
                .iter()
                .map(|a| persistent.get(&DataKey::Locked((*a).clone())).unwrap_or(0))
                .collect(),
            lock_entries: accounts
                .iter()
                .flat_map(|a| lockers.iter().map(move |l| (*a, *l)))
                .map(|(a, l)| {
                    persistent
                        .get(&DataKey::LockEntry(a.clone(), l.clone()))
                        .unwrap_or(0)
                })
                .collect(),
            lock_holders: accounts
                .iter()
                .map(|a| persistent.get(&DataKey::LockHolders((*a).clone())))
                .collect(),
            account_checkpoints: accounts
                .iter()
                .map(|a| {
                    read_history(env, &DataKey::CheckpointTracker((*a).clone()), |page| {
                        DataKey::CheckpointPage((*a).clone(), page)
                    })
                })
                .collect(),
            supply_checkpoints: read_history(
                env,
                &DataKey::SupplyCheckpointTracker,
                DataKey::SupplyCheckpointPage,
            ),
            admin: instance.get(&DataKey::Admin),
            pending_admin: instance.get(&DataKey::PendingAdmin),
            contract_storage: StdVec::new(),
            events: 0,
        }
    });
    Snapshot {
        contract_storage: contract_storage(env, &f.contract),
        events: env.events().all().len(),
        ..core
    }
}

/// Run `call` and assert it changed nothing in the token's state or events.
fn assert_unchanged<R>(f: &TokenFixture, accounts: &[&Address], call: impl FnOnce() -> R) -> R {
    let before = snapshot(f, accounts);
    let result = call();
    assert_eq!(
        snapshot(f, accounts),
        before,
        "a rejected call must leave balances, supply, locks, checkpoints, storage and events unchanged"
    );
    result
}

/// A holder with an existing balance (and therefore an existing checkpoint).
fn funded_holder(f: &TokenFixture, amount: i128) -> Address {
    let holder = Address::generate(&f.env);
    f.client().mint(&holder, &amount);
    holder
}

/// Simulate pre-#556 state: a `Locked(holder)` total with no per-locker
/// `LockEntry`, which is exactly what `migrate_legacy_lock` exists to split.
fn write_legacy_locked_total(f: &TokenFixture, holder: &Address, amount: i128) {
    f.env.as_contract(&f.contract, || {
        f.env
            .storage()
            .persistent()
            .set(&DataKey::Locked(holder.clone()), &amount);
    });
}

// ── mint ─────────────────────────────────────────────────────────────────────

#[test]
fn mint_rejects_zero_without_state_change() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    assert_eq!(
        assert_unchanged(&f, &[&holder], || client.try_mint(&holder, &0)),
        Err(Ok(TokenError::InvalidAmount))
    );
}

#[test]
fn mint_rejects_negative_one() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    assert_eq!(
        assert_unchanged(&f, &[&holder], || client.try_mint(&holder, &-1)),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!(client.total_supply(), 100);
}

#[test]
fn mint_rejects_i128_min() {
    let f = setup();
    let client = f.client();
    let holder = Address::generate(&f.env);
    assert_eq!(
        assert_unchanged(&f, &[&holder], || client.try_mint(&holder, &i128::MIN)),
        Err(Ok(TokenError::InvalidAmount))
    );
    assert_eq!(client.balance(&holder), 0);
    assert_eq!(client.total_supply(), 0);
}

#[test]
fn mint_rejects_i128_min_without_event_or_checkpoint() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    let before = snapshot(&f, &[&holder]);
    assert_eq!(
        client.try_mint(&holder, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    let after = snapshot(&f, &[&holder]);
    assert_eq!(after.events, before.events);
    assert_eq!(after.account_checkpoints, before.account_checkpoints);
    assert_eq!(after.supply_checkpoints, before.supply_checkpoints);
    assert_eq!(after, before);
}

// ── burn ─────────────────────────────────────────────────────────────────────

#[test]
fn burn_rejects_zero_without_state_change() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    assert_eq!(
        assert_unchanged(&f, &[&holder], || client.try_burn(&holder, &0)),
        Err(Ok(TokenError::InvalidAmount))
    );
}

#[test]
fn burn_rejects_negative_and_i128_min() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    for amount in [-1, i128::MIN] {
        assert_eq!(
            assert_unchanged(&f, &[&holder], || client.try_burn(&holder, &amount)),
            Err(Ok(TokenError::InvalidAmount)),
            "burn({amount})"
        );
    }
    assert_eq!(client.balance(&holder), 100);
    assert_eq!(client.total_supply(), 100);
}

#[test]
fn burn_rejects_i128_min_without_event_or_checkpoint() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    let before = snapshot(&f, &[&holder]);
    assert_eq!(
        client.try_burn(&holder, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    let after = snapshot(&f, &[&holder]);
    assert_eq!(after.events, before.events);
    assert_eq!(after.account_checkpoints, before.account_checkpoints);
    assert_eq!(after.supply_checkpoints, before.supply_checkpoints);
    assert_eq!(after, before);
}

#[test]
fn burn_exact_balance_succeeds_and_overbalance_fails() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    assert_eq!(
        assert_unchanged(&f, &[&holder], || client.try_burn(&holder, &101)),
        Err(Ok(TokenError::InsufficientBalance))
    );
    client.burn(&holder, &100);
    assert_eq!(client.balance(&holder), 0);
    assert_eq!(client.total_supply(), 0);
}

// ── lock ─────────────────────────────────────────────────────────────────────

#[test]
fn lock_rejects_zero_negative_and_i128_min() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    for amount in [0, -1, i128::MIN] {
        assert_eq!(
            assert_unchanged(&f, &[&holder], || client.try_lock(&holder, &amount)),
            Err(Ok(TokenError::InvalidAmount)),
            "lock({amount})"
        );
    }
    assert_eq!(client.locked_balance(&holder), 0);
}

#[test]
fn lock_rejects_i128_min_without_event_or_locked_change() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    client.lock(&holder, &10);
    let before = snapshot(&f, &[&holder]);
    assert_eq!(
        client.try_lock(&holder, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    let after = snapshot(&f, &[&holder]);
    assert_eq!(after.events, before.events);
    assert_eq!(after.locked, std::vec![10]);
    assert_eq!(after, before);
}

#[test]
fn lock_exact_unlocked_balance_succeeds_and_overlock_fails() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    client.lock(&holder, &100);
    assert_eq!(client.locked_balance(&holder), 100);
    assert_eq!(
        assert_unchanged(&f, &[&holder], || client.try_lock(&holder, &1)),
        Err(Ok(TokenError::InsufficientBalance))
    );
}

#[test]
fn partial_lock_accounts_for_previous_lock_before_overlock() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    client.lock(&holder, &60);
    assert_eq!(client.locked_balance(&holder), 60);
    assert_eq!(
        assert_unchanged(&f, &[&holder], || client.try_lock(&holder, &41)),
        Err(Ok(TokenError::InsufficientBalance))
    );
    client.lock(&holder, &40);
    assert_eq!(client.locked_balance(&holder), 100);
}

// ── unlock ───────────────────────────────────────────────────────────────────

#[test]
fn unlock_rejects_zero_negative_and_i128_min() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    client.lock(&holder, &50);
    for amount in [0, -1, i128::MIN] {
        assert_eq!(
            assert_unchanged(&f, &[&holder], || client
                .try_unlock(&holder, &f.locker, &amount)),
            Err(Ok(TokenError::InvalidAmount)),
            "unlock({amount})"
        );
    }
    assert_eq!(client.locked_balance(&holder), 50);
}

#[test]
fn unlock_rejects_i128_min_without_event_or_locked_change() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    client.lock(&holder, &50);
    let before = snapshot(&f, &[&holder]);
    assert_eq!(
        client.try_unlock(&holder, &f.locker, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    let after = snapshot(&f, &[&holder]);
    assert_eq!(after.events, before.events);
    assert_eq!(after.locked, std::vec![50]);
    assert_eq!(after, before);
}

#[test]
fn unlock_rejects_per_locker_overage() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    client.lock(&holder, &40);
    assert_eq!(
        assert_unchanged(&f, &[&holder], || client
            .try_unlock(&holder, &f.locker, &41)),
        Err(Ok(TokenError::InsufficientLocked))
    );
    assert_eq!(client.locked_balance(&holder), 40);
}

#[test]
fn unlock_rejects_total_locked_overage_across_locker_entries() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    let locker_b = Address::generate(&f.env);

    // Two lockers hold real locks: the original locker locks 40, then the
    // locker is rotated and the new one locks 31 (Locked = 71).
    client.lock(&holder, &40);
    client.set_locker(&locker_b);
    client.lock(&holder, &31);
    assert_eq!(client.locked_balance(&holder), 71);

    // `lock`, `unlock` and `migrate_legacy_lock` keep every `LockEntry` <= the
    // sum of entries <= `Locked(holder)`, so no sequence of calls can make one
    // locker's entry exceed the holder's total. The second `InsufficientLocked`
    // check only guards against an aggregate that undercounts the entries
    // (for example legacy state written by an older contract version). Model
    // exactly that and nothing else: every entry, the `LockHolders` index and
    // the balances stay as the real calls left them; only the aggregate is
    // lowered below locker B's entry.
    f.env.as_contract(&f.contract, || {
        f.env
            .storage()
            .persistent()
            .set(&DataKey::Locked(holder.clone()), &30_i128);
    });

    // Locker B's own entry (31) covers the amount, so only the total check
    // can reject it.
    assert_eq!(
        assert_unchanged(&f, &[&holder, &locker_b], || client
            .try_unlock(&holder, &locker_b, &31)),
        Err(Ok(TokenError::InsufficientLocked))
    );
    assert_eq!(client.locked_balance(&holder), 30);
}

// ── migrate_legacy_lock ──────────────────────────────────────────────────────

#[test]
fn migrate_legacy_lock_rejects_zero_negative_and_i128_min() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    write_legacy_locked_total(&f, &holder, 100);
    for amount in [0, -1, i128::MIN] {
        assert_eq!(
            assert_unchanged(&f, &[&holder], || client
                .try_migrate_legacy_lock(&holder, &f.locker, &amount)),
            Err(Ok(TokenError::InvalidAmount)),
            "migrate_legacy_lock({amount})"
        );
    }
}

#[test]
fn migrate_legacy_lock_rejects_i128_min_without_entry() {
    let f = setup();
    let client = f.client();
    let holder = Address::generate(&f.env);
    let before = snapshot(&f, &[&holder]);
    assert_eq!(
        client.try_migrate_legacy_lock(&holder, &f.locker, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    let after = snapshot(&f, &[&holder]);
    assert_eq!(after.lock_entries, std::vec![0, 0]);
    assert_eq!(after.lock_holders, std::vec![None]);
    assert_eq!(after, before);
}

#[test]
fn migrate_legacy_lock_rejects_amount_above_total_locked() {
    let f = setup();
    let client = f.client();
    let holder = funded_holder(&f, 100);
    write_legacy_locked_total(&f, &holder, 100);
    // The first `MigrationOverflow` check (`amount > total_locked`) is
    // logically shadowed by the `grand_total > total_locked` check after it:
    // `grand_total = other entries + existing + amount >= amount` because
    // entries are never negative. This test pins the behaviour; no test can
    // single out the first check.
    assert_eq!(
        assert_unchanged(&f, &[&holder], || client
            .try_migrate_legacy_lock(&holder, &f.locker, &101)),
        Err(Ok(TokenError::MigrationOverflow))
    );
    // The exact total is the accepted edge.
    client.migrate_legacy_lock(&holder, &f.locker, &100);
}

// ── propose_admin ────────────────────────────────────────────────────────────

#[test]
fn propose_admin_rejects_stranger_without_state_change() {
    let f = setup();
    let client = f.client();
    let stranger = Address::generate(&f.env);
    let nominee = Address::generate(&f.env);
    // `mock_all_auths` signs for the stranger, so only the stored-admin
    // comparison can reject this.
    assert_eq!(
        assert_unchanged(&f, &[&stranger, &nominee], || client
            .try_propose_admin(&stranger, &nominee)),
        Err(Ok(TokenError::Unauthorized))
    );
    assert_eq!(client.admin(), f.admin);
    assert_eq!(
        client.try_accept_admin(&nominee),
        Err(Ok(TokenError::Unauthorized))
    );
}

// ── transfer / transfer_from ─────────────────────────────────────────────────
//
// Decision (#1188): `transfer` and `transfer_from` keep their own
// `amount <= 0 -> InvalidAmount` check even though the shared `_transfer`
// helper repeats it. The entry-point copies reject before `require_auth`, the
// allowance read and (for `transfer_from`) the allowance rewrite and `approve`
// event; the `_transfer` copy protects any future internal caller. For the
// error a caller sees, each copy is shadowed by the other, so these tests pin
// the behaviour (exact error, nothing changed) per entry point rather than a
// specific copy, and keep it covered if either copy is removed.

#[test]
fn transfer_rejects_zero_negative_and_i128_min() {
    let f = setup();
    let client = f.client();
    let from = funded_holder(&f, 100);
    let to = Address::generate(&f.env);
    for amount in [0, -1, i128::MIN] {
        assert_eq!(
            assert_unchanged(&f, &[&from, &to], || client
                .try_transfer(&from, &to, &amount)),
            Err(Ok(TokenError::InvalidAmount)),
            "transfer({amount})"
        );
    }
}

#[test]
fn transfer_rejects_i128_min_without_event() {
    let f = setup();
    let client = f.client();
    let from = funded_holder(&f, 100);
    let to = Address::generate(&f.env);
    let before = snapshot(&f, &[&from, &to]);
    assert_eq!(
        client.try_transfer(&from, &to, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    let after = snapshot(&f, &[&from, &to]);
    assert_eq!(after.events, before.events);
    assert_eq!(after, before);
}

#[test]
fn transfer_from_rejects_zero_negative_and_i128_min() {
    let f = setup();
    let client = f.client();
    let spender = Address::generate(&f.env);
    let from = funded_holder(&f, 100);
    let to = Address::generate(&f.env);
    for amount in [0, -1, i128::MIN] {
        assert_eq!(
            assert_unchanged(&f, &[&spender, &from, &to], || client
                .try_transfer_from(&spender, &from, &to, &amount)),
            Err(Ok(TokenError::InvalidAmount)),
            "transfer_from({amount})"
        );
    }
}

#[test]
fn transfer_from_rejects_i128_min_without_event() {
    let f = setup();
    let client = f.client();
    let spender = Address::generate(&f.env);
    let from = funded_holder(&f, 100);
    let to = Address::generate(&f.env);
    client.approve(&from, &spender, &50, &1_000);
    let before = snapshot(&f, &[&spender, &from, &to]);
    assert_eq!(
        client.try_transfer_from(&spender, &from, &to, &i128::MIN),
        Err(Ok(TokenError::InvalidAmount))
    );
    let after = snapshot(&f, &[&spender, &from, &to]);
    assert_eq!(after.events, before.events);
    assert_eq!(after, before);
    assert_eq!(client.allowance(&from, &spender).amount, 50);
}
