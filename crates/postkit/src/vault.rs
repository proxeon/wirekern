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
}

#[derive(Default)]
pub struct MemoryVault {
    inner: Mutex<HashMap<AccountKey, AccountCreds>>,
    outcomes: Mutex<HashMap<(AccountKey, String), Outcome>>,
}

impl MemoryVault {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            outcomes: Mutex::new(HashMap::new()),
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
}
