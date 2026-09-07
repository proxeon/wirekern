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

    /// `create_dir_all` leaves intermediates (e.g. `accounts/`) on umask
    /// perms; tighten every dir from `dir` up to the vault root.
    fn ensure_dir_under_root(&self, dir: &Path) -> Result<(), Error> {
        ensure_dir(dir)?;
        let mut cur = dir;
        while cur != self.root {
            cur = cur
                .parent()
                .ok_or_else(|| std::io::Error::other("vault dir has no parent"))?;
            set_mode(cur, 0o700)?;
        }
        Ok(())
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
            self.ensure_dir_under_root(parent)?;
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
        let path = self
            .root
            .join("apps")
            .join(format!("{}.json", site.as_str()));
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

#[cfg(all(test, feature = "vault-file"))]
mod tests {
    use super::*;
    use crate::types::AccountCreds;
    use crate::types::OAuthApp;
    use serde_json::json;

    fn creds(tok: &str) -> AccountCreds {
        AccountCreds::OAuth2 {
            access_token: tok.into(),
            refresh_token: None,
            extra: json!({}),
        }
    }

    fn token_of(c: &AccountCreds) -> &str {
        match c {
            AccountCreds::OAuth2 { access_token, .. } => access_token,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn put_get_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path()).unwrap();
        let key = AccountKey::new("threads", "default");
        v.put(&key, &creds("tok")).unwrap();
        assert_eq!(token_of(&v.get(&key).unwrap()), "tok");
    }

    #[test]
    fn overwrite_replaces_creds() {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path()).unwrap();
        let key = AccountKey::new("threads", "default");
        v.put(&key, &creds("one")).unwrap();
        v.put(&key, &creds("two")).unwrap();
        assert_eq!(token_of(&v.get(&key).unwrap()), "two");
        let dir = tmp.path().join("accounts").join("threads");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn get_missing_is_unknown_account() {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path()).unwrap();
        let key = AccountKey::new("threads", "default");
        let err = v.get(&key).unwrap_err();
        assert!(matches!(err, Error::UnknownAccount(k) if k == key));
    }

    #[test]
    fn delete_removes_and_missing_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path()).unwrap();
        let key = AccountKey::new("threads", "default");
        v.put(&key, &creds("tok")).unwrap();
        v.delete(&key).unwrap();
        assert!(matches!(v.get(&key).unwrap_err(), Error::UnknownAccount(_)));
        assert!(matches!(
            v.delete(&key).unwrap_err(),
            Error::UnknownAccount(_)
        ));
    }

    #[test]
    fn invalid_names_rejected_before_any_io() {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path()).unwrap();
        let err = v
            .put(&AccountKey::new("threads", "../escape"), &creds("t"))
            .unwrap_err();
        assert!(matches!(err, Error::InvalidName(n) if n == "../escape"));
        let err = v
            .put(&AccountKey::new("a/b", "default"), &creds("t"))
            .unwrap_err();
        assert!(matches!(err, Error::InvalidName(n) if n == "a/b"));
        assert!(!tmp.path().join("accounts").exists());
    }

    #[test]
    fn list_filters_sites_and_skips_non_json() {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path()).unwrap();
        v.put(&AccountKey::new("threads", "work"), &creds("t"))
            .unwrap();
        v.put(&AccountKey::new("threads", "default"), &creds("t"))
            .unwrap();
        v.put(&AccountKey::new("bluesky", "you"), &creds("t"))
            .unwrap();
        fs::write(
            tmp.path()
                .join("accounts")
                .join("threads")
                .join("notes.txt"),
            "ignore me",
        )
        .unwrap();

        let all = v.list(None).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0], AccountKey::new("bluesky", "you"));
        assert_eq!(all[1], AccountKey::new("threads", "default"));
        assert_eq!(all[2], AccountKey::new("threads", "work"));

        let threads: Vec<_> = v
            .list(Some(&Site::new("threads")))
            .unwrap()
            .into_iter()
            .map(|k| k.name)
            .collect();
        assert_eq!(threads, vec!["default", "work"]);
    }

    #[test]
    fn list_missing_accounts_dir_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path().join("nested")).unwrap();
        assert!(v.list(None).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn secret_file_and_dir_modes() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path()).unwrap();
        v.put(&AccountKey::new("threads", "default"), &creds("t"))
            .unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&tmp.path().join("accounts")), 0o700);
        assert_eq!(mode(&tmp.path().join("accounts").join("threads")), 0o700);
        assert_eq!(
            mode(
                &tmp.path()
                    .join("accounts")
                    .join("threads")
                    .join("default.json")
            ),
            0o600
        );
    }

    // Site name avoids POSTKIT_* env_override collisions in FileAppStore::get.
    #[test]
    fn app_store_roundtrip_missing_and_names() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = FileAppStore::new(tmp.path()).unwrap();
        let site = Site::new("ztests");
        apps.put(&AppConfig {
            site: site.clone(),
            oauth: Some(OAuthApp {
                client_id: "id".into(),
                client_secret: "sec".into(),
                redirect_uri: "https://localhost/callback".into(),
            }),
            extra: json!({}),
        })
        .unwrap();
        let got = apps.get(&site).unwrap();
        assert_eq!(got.oauth.as_ref().unwrap().client_secret, "sec");

        let err = apps.get(&Site::new("nosuch")).unwrap_err();
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "missing_app_config"));

        let err = apps
            .put(&AppConfig {
                site: Site::new("a/b"),
                oauth: None,
                extra: json!({}),
            })
            .unwrap_err();
        assert!(matches!(err, Error::InvalidName(n) if n == "a/b"));
        assert!(!tmp.path().join("apps").join("a").exists());
    }
}
