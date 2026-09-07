use crate::error::Error;
use crate::types::{AccountCreds, AccountKey, Site};
use std::collections::HashMap;
use std::sync::Mutex;

pub trait Vault: Send + Sync {
    fn get(&self, key: &AccountKey) -> Result<AccountCreds, Error>;
    fn put(&self, key: &AccountKey, creds: &AccountCreds) -> Result<(), Error>;
    fn list(&self, site: Option<&Site>) -> Result<Vec<AccountKey>, Error>;
    fn delete(&self, key: &AccountKey) -> Result<(), Error>;
}

#[derive(Default)]
pub struct MemoryVault {
    inner: Mutex<HashMap<AccountKey, AccountCreds>>,
}

impl MemoryVault {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
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
}
