use crate::apps::{env_override, resolve_app_config, AppStore};
use crate::error::Error;
use crate::types::{valid_name, AccountCreds, AccountKey, AppConfig, Outcome, Site};
use crate::vault::{Claim, Vault};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// A claim older than this is presumed abandoned (holder crashed without
/// releasing) and may be stolen. Must comfortably exceed the longest
/// legitimate publish — caller deadlines are seconds-to-minutes, so a
/// quarter hour errs far on the safe side while bounding how long a
/// crashed holder can block a key.
const CLAIM_STALE_AFTER: Duration = Duration::from_secs(15 * 60);

// A PID plus wall-clock timestamp can collide between two threads on a coarse
// clock. A collision in `exclusive_atomic_write` lets one caller link bytes
// written by another caller while returning its own secret. Keep the suffix
// process-unique without relying on clock resolution.
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

    /// Idempotency ledger path: `idempotency/<site>/<account>/<key>.json`.
    /// Same `valid_name` discipline as account paths — an idempotency key
    /// is caller input and names a file.
    fn outcome_path(&self, key: &AccountKey, idem: &str) -> Result<PathBuf, Error> {
        if !valid_name(key.site.as_str()) || !valid_name(&key.name) {
            return Err(Error::InvalidName(key.name.clone()));
        }
        if !valid_name(idem) {
            return Err(Error::InvalidName(idem.into()));
        }
        Ok(self
            .root
            .join("idempotency")
            .join(key.site.as_str())
            .join(&key.name)
            .join(format!("{idem}.json")))
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
                // Strip exactly one `.json`: trim_end_matches would also eat
                // the suffix from a legacy `x.json.json`, aliasing two
                // accounts onto one file. Stems that are no longer legal
                // names are skipped — a listing must only contain accounts
                // that actually resolve.
                let Some(stem) = name.strip_suffix(".json") else {
                    continue;
                };
                if !valid_name(stem) {
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

    fn put_outcome(&self, key: &AccountKey, idem: &str, out: &Outcome) -> Result<(), Error> {
        let path = self.outcome_path(key, idem)?;
        if let Some(parent) = path.parent() {
            self.ensure_dir_under_root(parent)?;
        }
        atomic_write(&path, &serde_json::to_vec_pretty(out)?)
    }

    fn get_outcome(&self, key: &AccountKey, idem: &str) -> Result<Option<Outcome>, Error> {
        let path = self.outcome_path(key, idem)?;
        match fs::read(&path) {
            Ok(data) => Ok(Some(serde_json::from_slice(&data)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn claim_outcome(&self, key: &AccountKey, idem: &str) -> Result<Claim, Error> {
        let json_path = self.outcome_path(key, idem)?;
        let path = json_path.with_extension("claim");
        if let Some(parent) = path.parent() {
            self.ensure_dir_under_root(parent)?;
        }
        // `create_new` is O_EXCL: exactly one caller system-wide can create
        // the claim file, which is the whole cross-process guarantee. The
        // content (pid + wall clock) is diagnostic only.
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                use std::io::Write;
                let _ = writeln!(
                    f,
                    "pid={} unix_ms={}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0)
                );
                set_mode(&path, 0o600)?;
                Ok(Claim::Free)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // A crashed holder never releases; a claim older than the
                // TTL is stolen. The TTL must comfortably exceed the longest
                // legitimate publish (deadline + retries + refresh) — 15
                // minutes dwarfs any documented deadline. mtime in the
                // future (clock skew) reads as not-stale: the safe side.
                let stale = fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > CLAIM_STALE_AFTER);
                if !stale {
                    return Ok(Claim::Taken);
                }
                let _ = fs::remove_file(&path);
                // Retrying the create races other stealers fairly: O_EXCL
                // still admits exactly one winner.
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(_) => {
                        set_mode(&path, 0o600)?;
                        Ok(Claim::Free)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(Claim::Taken),
                    Err(e) => Err(e.into()),
                }
            }
            Err(e) => Err(e.into()),
        }
    }

    fn release_outcome(&self, key: &AccountKey, idem: &str) -> Result<(), Error> {
        let json_path = self.outcome_path(key, idem)?;
        let path = json_path.with_extension("claim");
        // NotFound is success: the key is open either way.
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
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
        // OAuth config remains an atomic env override. WhatsApp is resolved
        // below so an env sender ID can retain a file-backed webhook secret.
        if site.as_str() != "whatsapp_cloud" {
            if let Some(from_env) = env_override(site) {
                return Ok(from_env);
            }
        }
        if !valid_name(site.as_str()) {
            return Err(Error::InvalidName(site.as_str().into()));
        }
        let path = self
            .root
            .join("apps")
            .join(format!("{}.json", site.as_str()));
        // Missing is a normal environment-only deployment; other read and
        // parse failures remain errors rather than silently hiding a broken
        // local configuration behind an incomplete merged result.
        let from_file = match fs::read(&path) {
            Ok(data) => Some(serde_json::from_slice(&data)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => {
                return Err(Error::Auth {
                    site: site.clone(),
                    reason: "missing_app_config".into(),
                });
            }
        };
        resolve_app_config(site, from_file).ok_or_else(|| Error::Auth {
            site: site.clone(),
            reason: "missing_app_config".into(),
        })
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

pub(crate) fn ensure_dir(path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path)?;
    set_mode(path, 0o700)?;
    Ok(())
}

/// Write `bytes` to `path` via a same-dir tmp file + rename, so readers
/// never see a half-written document.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    // Unique per process: two postkit runs writing the same account get
    // distinct tmp files instead of interleaving writes into one shared
    // name and renaming a torn document into place. Same-process writes
    // are sequential (sync fs calls), so the pid is enough.
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    // Create the tmp 0600 from the first instant it exists: fs::write would
    // create it on the process umask (typically 0644), leaving the token or
    // client_secret world-readable for the window until a separate chmod
    // caught up. OpenOptions.mode applies at creation, closing that window.
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?
    };
    #[cfg(not(unix))]
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    drop(f);
    // rename(2) onto an existing path is atomic and the mode travels with
    // the inode, so the committed file appears fully written and 0600 in
    // one step. (create+truncate rather than create_new also self-heals a
    // stale tmp left by a crashed pid reuse instead of erroring on it.)
    fs::rename(&tmp, path)?;
    // Belt-and-braces for pre-existing files that carry looser modes;
    // a no-op on non-unix.
    set_mode(path, 0o600)?;
    Ok(())
}

/// Write `bytes` so `path` is created exactly once. Two racing callers: one
/// `Ok`, the other `AlreadyExists`. Bytes go to a 0600 tmp first; `hard_link`
/// into `path` is the exclusive step (link fails if the dest exists), so the
/// dest is never an empty placeholder a concurrent `list` could parse.
pub(crate) fn exclusive_atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::{ErrorKind, Write};
    let tmp = path.with_extension(format!(
        "json.tmp.{}.{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?
    };
    #[cfg(not(unix))]
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    drop(f);
    match fs::hard_link(&tmp, path) {
        Ok(()) => {
            let _ = fs::remove_file(&tmp);
            set_mode(path, 0o600)?;
            Ok(())
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&tmp);
            Err(e.into())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e.into())
        }
    }
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

    /// 023: the claim is exclusive until released, and the claim file sits
    /// beside its ledger entry under 0600.
    #[test]
    fn claim_is_exclusive_until_released() {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path()).unwrap();
        let key = AccountKey::new("threads", "default");
        assert_eq!(v.claim_outcome(&key, "k").unwrap(), Claim::Free);
        assert_eq!(v.claim_outcome(&key, "k").unwrap(), Claim::Taken);
        // A different key is a different reservation.
        assert_eq!(v.claim_outcome(&key, "other").unwrap(), Claim::Free);
        v.release_outcome(&key, "other").unwrap();
        v.release_outcome(&key, "k").unwrap();
        // Double release is idempotent — the key is open either way.
        v.release_outcome(&key, "k").unwrap();
        assert_eq!(v.claim_outcome(&key, "k").unwrap(), Claim::Free);
        let claim_path = tmp
            .path()
            .join("idempotency")
            .join("threads")
            .join("default")
            .join("k.claim");
        assert!(claim_path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&claim_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        v.release_outcome(&key, "k").unwrap();
        assert!(!claim_path.exists());
    }

    /// 023: a crashed holder never releases; a claim older than the TTL is
    /// stolen, and a fresh claim is not.
    #[test]
    fn stale_claim_is_stolen_after_ttl() {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path()).unwrap();
        let key = AccountKey::new("threads", "default");
        assert_eq!(v.claim_outcome(&key, "k").unwrap(), Claim::Free);
        let claim_path = tmp
            .path()
            .join("idempotency")
            .join("threads")
            .join("default")
            .join("k.claim");
        // Age the claim past the TTL by rewriting its mtime.
        let old =
            std::time::SystemTime::now() - CLAIM_STALE_AFTER - std::time::Duration::from_secs(1);
        let f = fs::File::options().write(true).open(&claim_path).unwrap();
        f.set_modified(old).unwrap();
        drop(f);
        assert_eq!(v.claim_outcome(&key, "k").unwrap(), Claim::Free);
        // A fresh claim (mtime now) must read as Taken, never stolen.
        assert_eq!(v.claim_outcome(&key, "k").unwrap(), Claim::Taken);
        // An idempotency key is caller input and names a claim file too.
        let err = v.claim_outcome(&key, "../escape").unwrap_err();
        assert!(matches!(err, Error::InvalidName(n) if n == "../escape"));
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
    fn list_strips_exactly_one_json_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path()).unwrap();
        v.put(&AccountKey::new("threads", "foo"), &creds("t"))
            .unwrap();
        // legacy artifact from before .json-suffixed names were rejected;
        // its stem is not a legal name, so it must not be listed
        let dir = tmp.path().join("accounts").join("threads");
        fs::write(dir.join("legacy.json.json"), "{}").unwrap();
        let listed: Vec<String> = v.list(None).unwrap().into_iter().map(|k| k.name).collect();
        assert_eq!(listed, vec!["foo"]);
    }

    #[test]
    fn json_suffix_names_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path()).unwrap();
        let err = v
            .put(&AccountKey::new("threads", "foo.json"), &creds("t"))
            .unwrap_err();
        assert!(matches!(err, Error::InvalidName(n) if n == "foo.json"));
        // dotted names without the reserved suffix still pass
        v.put(&AccountKey::new("threads", "a.b_c-d"), &creds("t"))
            .unwrap();
    }

    #[test]
    fn list_missing_accounts_dir_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path().join("nested")).unwrap();
        assert!(v.list(None).unwrap().is_empty());
    }

    #[test]
    fn outcome_ledger_roundtrip_and_names() {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileVault::new(tmp.path()).unwrap();
        let key = AccountKey::new("threads", "default");
        let out = Outcome {
            site: Site::new("threads"),
            id: Some("123".into()),
            url: Some("https://example.test/123".into()),
            limits: None,
        };
        assert!(v.get_outcome(&key, "k").unwrap().is_none());
        v.put_outcome(&key, "k", &out).unwrap();
        assert_eq!(
            v.get_outcome(&key, "k").unwrap().unwrap().id.as_deref(),
            Some("123")
        );
        // lands under idempotency/<site>/<account>/<key>.json
        assert!(tmp
            .path()
            .join("idempotency")
            .join("threads")
            .join("default")
            .join("k.json")
            .exists());
        // an idempotency key is caller input and names a file
        let err = v.put_outcome(&key, "../escape", &out).unwrap_err();
        assert!(matches!(err, Error::InvalidName(n) if n == "../escape"));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_tmp_is_0600_from_creation_with_unique_name() {
        use std::os::unix::fs::PermissionsExt;
        // umask 0000: anything created on umask alone would land 0666, so
        // this pins the mode to the creation call itself, not a later
        // chmod. The old fs::write-then-chmod left a world-readable tmp
        // holding the token for the window between the two.
        let prev_umask = unsafe { libc::umask(0o000) };
        // Point the write at a directory so rename fails and the tmp file
        // stays behind for inspection.
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("accounts");
        fs::create_dir(&target).unwrap();
        let err = atomic_write(&target, b"secret").unwrap_err();
        unsafe { libc::umask(prev_umask) };

        assert!(matches!(err, Error::Io(_)), "rename onto a dir must fail");

        let leftovers: Vec<String> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "accounts")
            .collect();
        assert_eq!(leftovers.len(), 1, "exactly one tmp file: {leftovers:?}");
        // pid-suffixed, so concurrent processes never share a tmp name
        assert_eq!(
            leftovers[0],
            format!("accounts.json.tmp.{}", std::process::id())
        );
        let mode = fs::metadata(tmp.path().join(&leftovers[0]))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
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
