use crate::apps::{env_override, AppStore};
use crate::error::Error;
use crate::types::{valid_name, AccountCreds, AccountKey, AppConfig, Site};
use crate::vault::Vault;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub struct FileVault {
    root: PathBuf,
}

impl FileVault {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, Error> {
        let root = root.into();
        ensure_dir(&root)?;
        Ok(Self { root })
    }

    fn account_path(&self, key: &AccountKey) -> Result<PathBuf, Error> {
        if !valid_name(key.site.as_str()) {
            return Err(Error::InvalidName(key.site.as_str().into()));
        }
        if !valid_name(&key.name) {
            return Err(Error::InvalidName(key.name.clone()));
        }
        Ok(self
            .root
            .join("accounts")
            .join(key.site.as_str())
            .join(format!("{}.json", key.name)))
    }
}

impl Vault for FileVault {
    fn get(&self, key: &AccountKey) -> Result<AccountCreds, Error> {
        let path = self.account_path(key)?;
        let data = fs::read(&path).map_err(|_| Error::UnknownAccount(key.clone()))?;
        Ok(serde_json::from_slice(&data)?)
    }

    fn put(&self, key: &AccountKey, creds: &AccountCreds) -> Result<(), Error> {
        let path = self.account_path(key)?;
        if let Some(parent) = path.parent() {
            ensure_dir(parent)?;
        }
        atomic_write(&path, &serde_json::to_vec_pretty(creds)?)?;
        Ok(())
    }

    fn list(&self, site: Option<&Site>) -> Result<Vec<AccountKey>, Error> {
        let accounts = self.root.join("accounts");
        if !accounts.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let sites: Vec<Site> = if let Some(s) = site {
            vec![s.clone()]
        } else {
            read_dir_names(&accounts)?
                .into_iter()
                .map(Site::new)
                .collect()
        };
        for s in sites {
            let dir = accounts.join(s.as_str());
            if !dir.exists() {
                continue;
            }
            for name in read_dir_names(&dir)? {
                let stem = name.trim_end_matches(".json");
                if stem == name {
                    continue;
                }
                out.push(AccountKey::new(s.as_str(), stem));
            }
        }
        Ok(out)
    }

    fn delete(&self, key: &AccountKey) -> Result<(), Error> {
        let path = self.account_path(key)?;
        fs::remove_file(&path).map_err(|_| Error::UnknownAccount(key.clone()))
    }
}

pub struct FileAppStore {
    root: PathBuf,
}

impl FileAppStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, Error> {
        let root = root.into();
        ensure_dir(&root.join("apps"))?;
        Ok(Self { root })
    }
}

impl AppStore for FileAppStore {
    fn get(&self, site: &Site) -> Result<AppConfig, Error> {
        if let Some(from_env) = env_override(site) {
            return Ok(from_env);
        }
        if !valid_name(site.as_str()) {
            return Err(Error::InvalidName(site.as_str().into()));
        }
        let path = self.root.join("apps").join(format!("{}.json", site.as_str()));
        let data = fs::read(&path).map_err(|_| Error::Auth {
            site: site.clone(),
            reason: "missing_app_config".into(),
        })?;
        Ok(serde_json::from_slice(&data)?)
    }

    fn put(&self, cfg: &AppConfig) -> Result<(), Error> {
        if !valid_name(cfg.site.as_str()) {
            return Err(Error::InvalidName(cfg.site.as_str().into()));
        }
        let dir = self.root.join("apps");
        ensure_dir(&dir)?;
        let path = dir.join(format!("{}.json", cfg.site.as_str()));
        atomic_write(&path, &serde_json::to_vec_pretty(cfg)?)?;
        Ok(())
    }
}

fn read_dir_names(dir: &Path) -> Result<Vec<String>, Error> {
    let mut names = Vec::new();
    for ent in fs::read_dir(dir)? {
        let ent = ent?;
        if let Some(s) = ent.file_name().to_str() {
            names.push(s.to_string());
        }
    }
    names.sort();
    Ok(names)
}

fn ensure_dir(path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path)?;
    set_mode(path, 0o700)?;
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    set_mode(&tmp, 0o600)?;
    fs::rename(&tmp, path)?;
    set_mode(path, 0o600)?;
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<(), Error> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}
