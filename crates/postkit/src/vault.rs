use crate::error::Error;
use crate::types::{AccountCreds, AccountKey, Outcome, Site};
use std::collections::HashMap;
use std::sync::Mutex;

pub trait Vault: Send + Sync {
    fn get(&self, key: &AccountKey) -> Result<AccountCreds, Error>;
    fn put(&self, key: &AccountKey, creds: &AccountCreds) -> Result<(), Error>;
    fn list(&self, site: Option<&Site>) -> Result<Vec<AccountKey>, Error>;
    fn delete(&self, key: &AccountKey) -> Result<(), Error>;

    /// Idempotency ledger: remember the `Outcome` of a **completed**
    /// publish under a caller-chosen key, so a retry with the same key
    /// returns it without touching the network. Deliberately required
    /// (no silent default no-op): a vault that cannot persist outcomes
    /// must say so at compile time, not quietly disable dedupe.
    fn put_outcome(&self, key: &AccountKey, idem: &str, out: &Outcome) -> Result<(), Error>;
    fn get_outcome(&self, key: &AccountKey, idem: &str) -> Result<Option<Outcome>, Error>;

    /// Reserve the right to publish under an idempotency key — issue 023.
    /// The ledger alone cannot prevent duplicates: read-ledger → publish →
    /// write-ledger lets two concurrent callers both see "no outcome" and
    /// both publish. `claim` closes that window; `release` reopens the key
    /// (after success with the outcome recorded, or after failure so a
    /// retry is possible). Required like the ledger: a vault that cannot
    /// reserve must say so at compile time.
    fn claim_outcome(&self, key: &AccountKey, idem: &str) -> Result<Claim, Error>;
    fn release_outcome(&self, key: &AccountKey, idem: &str) -> Result<(), Error>;
}

/// A claim attempt's answer: `Free` means the caller now holds the
/// reservation and owes a `release_outcome`; `Taken` means another
/// publish under this key is in flight right now.
#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    Free,
    Taken,
}

#[derive(Default)]
pub struct MemoryVault {
    inner: Mutex<HashMap<AccountKey, AccountCreds>>,
    outcomes: Mutex<HashMap<(AccountKey, String), Outcome>>,
    /// Held claims. In-process only: two tasks sharing one `Client` (or
    /// vault) still cannot double-publish; cross-process serialization is
    /// the file vault's job.
    claims: Mutex<std::collections::HashSet<(AccountKey, String)>>,
}

impl MemoryVault {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            outcomes: Mutex::new(HashMap::new()),
            claims: Mutex::new(std::collections::HashSet::new()),
        }
    }
}

impl Vault for MemoryVault {
    fn get(&self, key: &AccountKey) -> Result<AccountCreds, Error> {
        self.inner
            .lock()
            .expect("vault")
            .get(key)
            .cloned()
            .ok_or_else(|| Error::UnknownAccount(key.clone()))
    }

    fn put(&self, key: &AccountKey, creds: &AccountCreds) -> Result<(), Error> {
        self.inner
            .lock()
            .expect("vault")
            .insert(key.clone(), creds.clone());
        Ok(())
    }

    fn list(&self, site: Option<&Site>) -> Result<Vec<AccountKey>, Error> {
        let g = self.inner.lock().expect("vault");
        Ok(g.keys()
            .filter(|k| site.map(|s| k.site == *s).unwrap_or(true))
            .cloned()
            .collect())
    }

    fn delete(&self, key: &AccountKey) -> Result<(), Error> {
        self.inner
            .lock()
            .expect("vault")
            .remove(key)
            .ok_or_else(|| Error::UnknownAccount(key.clone()))
            .map(|_| ())
    }

    fn put_outcome(&self, key: &AccountKey, idem: &str, out: &Outcome) -> Result<(), Error> {
        self.outcomes
            .lock()
            .expect("vault")
            .insert((key.clone(), idem.to_string()), out.clone());
        Ok(())
    }

    fn get_outcome(&self, key: &AccountKey, idem: &str) -> Result<Option<Outcome>, Error> {
        Ok(self
            .outcomes
            .lock()
            .expect("vault")
            .get(&(key.clone(), idem.to_string()))
            .cloned())
    }

    fn claim_outcome(&self, key: &AccountKey, idem: &str) -> Result<Claim, Error> {
        // `insert` returns false when the key was already held — the whole
        // atomicity story for in-process callers is this one map insert.
        let first = self
            .claims
            .lock()
            .expect("vault")
            .insert((key.clone(), idem.to_string()));
        Ok(if first { Claim::Free } else { Claim::Taken })
    }

    fn release_outcome(&self, key: &AccountKey, idem: &str) -> Result<(), Error> {
        self.claims
            .lock()
            .expect("vault")
            .remove(&(key.clone(), idem.to_string()));
        Ok(())
    }
}
