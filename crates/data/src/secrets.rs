//! Every secret tangul keeps (database and SSH passwords, key passphrases, API keys,
//! the MCP server's token) in one Keychain item, `tangul` / `secrets`: a JSON object
//! by account, read once per session. macOS asks before a new build of the app reads
//! an item (an app without a Team ID is trusted per build), so an update asks once,
//! not once per secret.
//!
//! Before, each secret was an item of its own (`tangul` / account, earlier `rusdb` /
//! account). The first time, every account tangul knows is moved into the one item,
//! and an old item is deleted only once the one item holds its secret. One that
//! can't be read then (e.g. "Deny") stays and is moved when it is next needed.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::config::LEGACY_NAME;

const SERVICE: &str = "tangul";
/// The one item's account.
const ALL: &str = "secrets";

/// Where items are kept: the Keychain, or a map in tests.
pub(crate) trait Store: Send + Sync {
    /// `Ok(None)`: there is no such item.
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, String>;
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), String>;
    /// Deletes the item if there is one.
    fn delete(&self, service: &str, account: &str);
}

/// The Keychain (keyring's mock store on other platforms: nothing is kept).
struct Keychain;

impl Store for Keychain {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, String> {
        let entry = keyring::Entry::new(service, account).map_err(|e| e.to_string())?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), String> {
        keyring::Entry::new(service, account)
            .and_then(|e| e.set_password(secret))
            .map_err(|e| e.to_string())
    }

    fn delete(&self, service: &str, account: &str) {
        if let Ok(entry) = keyring::Entry::new(service, account) {
            let _ = entry.delete_credential();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum State {
    Unread,
    /// Read, or not there yet: written with every change.
    Open,
    /// It couldn't be read (denied, or not a JSON object): never written, so
    /// nothing in it is lost; changes go to items of their own, as before.
    Unusable,
}

pub(crate) struct Secrets {
    /// `None`: only for this session (tests, tools).
    store: Option<Box<dyn Store>>,
    state: State,
    /// Every secret known this session.
    known: HashMap<String, String>,
    /// Accounts looked for in vain this session.
    absent: HashSet<String>,
}

impl Secrets {
    pub(crate) fn keychain() -> Self {
        Self::with(Some(Box::new(Keychain)))
    }

    pub(crate) fn in_memory() -> Self {
        Self::with(None)
    }

    fn with(store: Option<Box<dyn Store>>) -> Self {
        Secrets {
            store,
            state: State::Unread,
            known: HashMap::new(),
            absent: HashSet::new(),
        }
    }

    /// The secret of `account`. `accounts`: every account tangul knows, moved into
    /// the one item the first time.
    pub(crate) fn get(
        &mut self,
        account: &str,
        accounts: impl FnOnce() -> Vec<String>,
    ) -> Option<String> {
        if let Some(secret) = self.known.get(account) {
            return Some(secret.clone());
        }
        if self.store.is_none() || self.absent.contains(account) {
            return None;
        }
        self.open(accounts);
        if !self.known.contains_key(account) {
            self.move_old(&[account.to_string()]);
        }
        let secret = self.known.get(account).cloned();
        if secret.is_none() {
            self.absent.insert(account.to_string());
        }
        secret
    }

    pub(crate) fn set(
        &mut self,
        account: &str,
        secret: String,
        accounts: impl FnOnce() -> Vec<String>,
    ) {
        self.absent.remove(account);
        self.open(accounts);
        self.known.insert(account.to_string(), secret.clone());
        let Some(store) = &self.store else {
            return;
        };
        if self.write() {
            self.delete_old(account);
        } else if let Err(e) = store.set(SERVICE, account, &secret) {
            eprintln!("keychain write failed: {e}");
        }
    }

    pub(crate) fn forget(&mut self, account: &str, accounts: impl FnOnce() -> Vec<String>) {
        self.open(accounts);
        self.known.remove(account);
        self.absent.insert(account.to_string());
        self.write();
        self.delete_old(account);
    }

    /// Reads the one item, once; the first time (not there yet) every account's
    /// old item is moved into it.
    fn open(&mut self, accounts: impl FnOnce() -> Vec<String>) {
        let Some(store) = &self.store else {
            return;
        };
        if self.state != State::Unread {
            return;
        }
        self.state = match store.get(SERVICE, ALL) {
            Ok(Some(text)) => match serde_json::from_str::<BTreeMap<String, String>>(&text) {
                Ok(saved) => {
                    for (account, secret) in saved {
                        self.known.entry(account).or_insert(secret);
                    }
                    State::Open
                }
                Err(e) => {
                    eprintln!("keychain: the secrets item can't be read, left as it is: {e}");
                    State::Unusable
                }
            },
            Ok(None) => {
                self.state = State::Open;
                self.move_old(&accounts());
                return;
            }
            Err(e) => {
                eprintln!("keychain: the secrets item can't be read: {e}");
                State::Unusable
            }
        };
    }

    /// Moves the old items of `accounts` (if there are any) into the one item; each
    /// is deleted once the one item holds it.
    fn move_old(&mut self, accounts: &[String]) {
        let Some(store) = &self.store else {
            return;
        };
        let mut moved = Vec::new();
        for account in accounts {
            if self.known.contains_key(account) {
                continue;
            }
            for service in [SERVICE, LEGACY_NAME] {
                match store.get(service, account) {
                    Ok(None) => continue,
                    Ok(Some(secret)) => {
                        self.known.insert(account.clone(), secret);
                        moved.push(account);
                    }
                    Err(e) => eprintln!("keychain: an old item can't be read: {e}"),
                }
                break;
            }
        }
        if !moved.is_empty() && self.write() {
            for account in moved {
                self.delete_old(account);
            }
        }
    }

    /// Writes every known secret to the one item, if it is open; whether it was
    /// written.
    fn write(&self) -> bool {
        let Some(store) = self.store.as_ref().filter(|_| self.state == State::Open) else {
            return false;
        };
        let all: BTreeMap<&str, &str> = (self.known.iter())
            .map(|(account, secret)| (account.as_str(), secret.as_str()))
            .collect();
        let text = serde_json::to_string(&all).expect("strings serialize");
        match store.set(SERVICE, ALL, &text) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("keychain write failed: {e}");
                false
            }
        }
    }

    /// Deletes the items `account` had of its own.
    fn delete_old(&self, account: &str) {
        if let Some(store) = &self.store {
            store.delete(SERVICE, account);
            store.delete(LEGACY_NAME, account);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Items by service and account; reads of `denied` ones fail, as on "Deny".
    #[derive(Default)]
    struct Items {
        items: Mutex<BTreeMap<(String, String), String>>,
        denied: Mutex<HashSet<(String, String)>>,
        reads: Mutex<usize>,
    }

    type Shared = Arc<Items>;

    impl Store for Shared {
        fn get(&self, service: &str, account: &str) -> Result<Option<String>, String> {
            *self.reads.lock().unwrap() += 1;
            let key = (service.to_string(), account.to_string());
            if self.denied.lock().unwrap().contains(&key) {
                return Err("denied".into());
            }
            Ok(self.items.lock().unwrap().get(&key).cloned())
        }

        fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), String> {
            let key = (service.to_string(), account.to_string());
            if self.denied.lock().unwrap().contains(&key) {
                return Err("denied".into());
            }
            self.items.lock().unwrap().insert(key, secret.to_string());
            Ok(())
        }

        fn delete(&self, service: &str, account: &str) {
            let key = (service.to_string(), account.to_string());
            self.items.lock().unwrap().remove(&key);
        }
    }

    impl Items {
        fn with(items: &[(&str, &str, &str)]) -> Shared {
            let shared = Shared::default();
            for (service, account, secret) in items {
                shared.set(service, account, secret).unwrap();
            }
            shared
        }

        fn deny(&self, service: &str, account: &str) {
            let key = (service.to_string(), account.to_string());
            self.denied.lock().unwrap().insert(key);
        }

        fn allow(&self) {
            self.denied.lock().unwrap().clear();
        }

        /// Every item as `service/account`.
        fn names(&self) -> Vec<String> {
            let items = self.items.lock().unwrap();
            items.keys().map(|(s, a)| format!("{s}/{a}")).collect()
        }

        /// The one item's secrets.
        fn all(&self) -> BTreeMap<String, String> {
            let items = self.items.lock().unwrap();
            let text = items.get(&(SERVICE.into(), ALL.into())).unwrap();
            serde_json::from_str(text).unwrap()
        }

        fn reads(&self) -> usize {
            std::mem::take(&mut *self.reads.lock().unwrap())
        }
    }

    fn secrets(store: &Shared) -> Secrets {
        Secrets::with(Some(Box::new(store.clone())))
    }

    fn known() -> Vec<String> {
        ["u1", "u2", "ssh:d", "mcp-token", "gone"]
            .map(String::from)
            .to_vec()
    }

    /// The first time, every known account's old item (under either name) moves
    /// into the one item and is deleted; after that a session reads only that one.
    #[test]
    fn old_items_move_into_one_and_are_deleted() {
        let store = Items::with(&[
            ("tangul", "u1", "one"),
            ("rusdb", "u2", "two"),
            ("tangul", "ssh:d", "pass"),
            ("tangul", "mcp-token", "tok"),
            ("tangul", "unknown", "left"),
        ]);
        let mut s = secrets(&store);
        assert_eq!(s.get("u1", known).as_deref(), Some("one"));
        assert_eq!(store.names(), ["tangul/secrets", "tangul/unknown"]);
        let all = store.all();
        let all: Vec<(&str, &str)> = all.iter().map(|(a, s)| (&**a, &**s)).collect();
        assert_eq!(
            all,
            [
                ("mcp-token", "tok"),
                ("ssh:d", "pass"),
                ("u1", "one"),
                ("u2", "two")
            ]
        );
        // The rest come from the session.
        store.reads();
        assert_eq!(s.get("u2", known).as_deref(), Some("two"));
        assert_eq!(s.get("gone", known), None);
        assert_eq!(s.get("gone", known), None);
        assert_eq!(store.reads(), 2, "looked for once, in both old places");

        // The next session reads the one item, and only it.
        let mut next = secrets(&store);
        assert_eq!(next.get("ssh:d", known).as_deref(), Some("pass"));
        assert_eq!(next.get("mcp-token", known).as_deref(), Some("tok"));
        assert_eq!(store.reads(), 1);
    }

    /// An old item that can't be read stays; it moves when next needed.
    #[test]
    fn a_denied_old_item_stays_until_it_can_be_moved() {
        let store = Items::with(&[("tangul", "u1", "one"), ("tangul", "u2", "two")]);
        store.deny("tangul", "u2");
        let mut s = secrets(&store);
        assert_eq!(s.get("u1", known).as_deref(), Some("one"));
        assert_eq!(s.get("u2", known), None);
        assert_eq!(store.names(), ["tangul/secrets", "tangul/u2"]);

        store.allow();
        let mut next = secrets(&store);
        assert_eq!(next.get("u2", known).as_deref(), Some("two"));
        assert_eq!(store.names(), ["tangul/secrets"]);
        assert_eq!(store.all().len(), 2);
    }

    /// When the one item can't be read it is never written (nothing in it is
    /// lost): old items are read but stay, and new secrets get items of their own.
    #[test]
    fn an_unreadable_item_is_never_overwritten() {
        let store = Items::with(&[("tangul", "secrets", r#"{"u1":"one"}"#)]);
        store.set("tangul", "u2", "two").unwrap();
        store.deny("tangul", "secrets");
        let mut s = secrets(&store);
        assert_eq!(s.get("u1", known), None);
        assert_eq!(s.get("u2", known).as_deref(), Some("two"));
        s.set("u3", "three".into(), known);
        assert_eq!(s.get("u3", known).as_deref(), Some("three"));
        store.allow();
        assert_eq!(store.names(), ["tangul/secrets", "tangul/u2", "tangul/u3"]);
        assert_eq!(store.all().len(), 1);

        // Not JSON: left as it is too.
        let store = Items::with(&[("tangul", "secrets", "garbage")]);
        let mut s = secrets(&store);
        s.set("u1", "one".into(), known);
        let items = store.items.lock().unwrap();
        assert_eq!(items[&("tangul".into(), "secrets".into())], "garbage");
        assert_eq!(items[&("tangul".into(), "u1".into())], "one");
    }

    /// Setting and forgetting change the one item and remove what an account had
    /// of its own.
    #[test]
    fn set_and_forget_change_the_one_item() {
        let store = Items::with(&[("rusdb", "u1", "old")]);
        let mut s = secrets(&store);
        s.set("u2", "two".into(), Vec::new);
        assert_eq!(store.names(), ["rusdb/u1", "tangul/secrets"]);
        s.set("u1", "new".into(), Vec::new);
        assert_eq!(store.names(), ["tangul/secrets"]);
        assert_eq!(store.all()["u1"], "new");
        s.forget("u1", Vec::new);
        assert_eq!(s.get("u1", Vec::new), None);
        assert_eq!(store.all().keys().collect::<Vec<_>>(), ["u2"]);
        // Set again after it was looked for in vain.
        s.set("u1", "back".into(), Vec::new);
        assert_eq!(s.get("u1", Vec::new).as_deref(), Some("back"));
    }

    #[test]
    fn in_memory_keeps_secrets_for_the_session_only() {
        let mut s = Secrets::in_memory();
        assert_eq!(s.get("a", known), None);
        s.set("a", "x".into(), known);
        assert_eq!(s.get("a", known).as_deref(), Some("x"));
        s.forget("a", known);
        assert_eq!(s.get("a", known), None);
    }
}
