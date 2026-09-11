//! Operator HTTP keys (`pk_live_`). Not platform tokens.
//!
//! `postkit serve` authenticates callers with these. The plaintext is shown
//! once at create; disk stores only SHA-256 of the full `pk_live_…` string.
//! Do not put these in `AccountCreds` — that file is Threads/Bluesky, not
//! "who may call this process".

use crate::error::Error;
use crate::types::{valid_name, Site};
use crate::vault_file::{atomic_write, ensure_dir};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub const KEY_PREFIX: &str = "pk_live_";
const KEY_BYTES: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyMeta {
    pub name: String,
    pub prefix: String,
    pub sha256: String,
    pub created_at: String,
}

/// Shown once. Never written to disk.
#[derive(Clone, Debug)]
pub struct CreatedKey {
    pub name: String,
    pub token: String,
}

pub struct FileKeyStore {
    root: PathBuf,
}

impl FileKeyStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, Error> {
        let root = root.into();
        ensure_dir(&root.join("keys"))?;
        Ok(Self { root })
    }

    fn path(&self, name: &str) -> Result<PathBuf, Error> {
        if !valid_name(name) {
            return Err(Error::InvalidName(name.into()));
        }
        Ok(self.root.join("keys").join(format!("{name}.json")))
    }

    pub fn create(&self, name: &str) -> Result<CreatedKey, Error> {
        let path = self.path(name)?;
        if path.exists() {
            return Err(Error::InvalidQuery {
                site: Site::new(""),
                reason: "key_exists".into(),
            });
        }
        let token = generate_token()?;
        let meta = KeyMeta {
            name: name.to_string(),
            prefix: KEY_PREFIX.into(),
            sha256: hex_sha256(token.as_bytes()),
            created_at: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into()),
        };
        atomic_write(&path, &serde_json::to_vec_pretty(&meta)?)?;
        Ok(CreatedKey {
            name: name.to_string(),
            token,
        })
    }

    pub fn list(&self) -> Result<Vec<KeyMeta>, Error> {
        let dir = self.root.join("keys");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out: Vec<KeyMeta> = Vec::new();
        for ent in fs::read_dir(&dir)? {
            let ent = ent?;
            let name = match ent.file_name().to_str() {
                Some(n) => n.to_string(),
                None => continue,
            };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if !valid_name(stem) {
                continue;
            }
            let data = fs::read(ent.path())?;
            out.push(serde_json::from_slice(&data)?);
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn delete(&self, name: &str) -> Result<(), Error> {
        let path = self.path(name)?;
        fs::remove_file(&path).map_err(|_| Error::InvalidQuery {
            site: Site::new(""),
            reason: "unknown_key".into(),
        })
    }

    /// Hash the presented bearer once, then compare every stored hash in
    /// constant time. Do not return on the first match: which key matched
    /// must not leak through response timing.
    pub fn verify(&self, presented: &str) -> Result<KeyMeta, Error> {
        if !presented.starts_with(KEY_PREFIX) {
            return Err(invalid_key());
        }
        let want = hex_sha256(presented.as_bytes());
        let mut matched: Option<KeyMeta> = None;
        for meta in self.list()? {
            if ct_eq(want.as_bytes(), meta.sha256.as_bytes()) {
                matched = Some(meta);
            }
        }
        matched.ok_or_else(invalid_key)
    }
}

fn generate_token() -> Result<String, Error> {
    let mut raw = [0u8; KEY_BYTES];
    getrandom::fill(&mut raw).map_err(|_| Error::Auth {
        site: Site::new(""),
        reason: "os_rng".into(),
    })?;
    Ok(format!("{KEY_PREFIX}{}", URL_SAFE_NO_PAD.encode(raw)))
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut acc = 0u8;
    for (x, y) in a.iter().zip(b) {
        acc |= x ^ y;
    }
    acc == 0
}

fn invalid_key() -> Error {
    Error::Auth {
        site: Site::new(""),
        reason: "invalid_key".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_lists_and_verifies_without_storing_plaintext() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileKeyStore::new(tmp.path()).unwrap();
        let created = store.create("n8n").unwrap();
        assert!(created.token.starts_with(KEY_PREFIX));
        assert!(!created.token.contains('/'));
        assert!(!created.token.contains('+'));
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "n8n");
        assert_eq!(listed[0].prefix, KEY_PREFIX);
        let disk = fs::read_to_string(tmp.path().join("keys/n8n.json")).unwrap();
        assert!(!disk.contains(&created.token[KEY_PREFIX.len()..]));
        let meta = store.verify(&created.token).unwrap();
        assert_eq!(meta.name, "n8n");
        let err = store.verify("pk_live_not-a-real-key").unwrap_err();
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "invalid_key"));
    }

    #[test]
    fn create_refuses_duplicate_and_illegal_names() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileKeyStore::new(tmp.path()).unwrap();
        store.create("ok").unwrap();
        let dup = store.create("ok").unwrap_err();
        assert!(matches!(dup, Error::InvalidQuery { reason, .. } if reason == "key_exists"));
        let bad = store.create("bad/name").unwrap_err();
        assert!(matches!(bad, Error::InvalidName(_)));
    }

    #[test]
    fn revoke_then_verify_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileKeyStore::new(tmp.path()).unwrap();
        let created = store.create("agent").unwrap();
        store.delete("agent").unwrap();
        let err = store.verify(&created.token).unwrap_err();
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "invalid_key"));
    }

    #[test]
    #[cfg(unix)]
    fn key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let store = FileKeyStore::new(tmp.path()).unwrap();
        store.create("n8n").unwrap();
        let mode = fs::metadata(tmp.path().join("keys/n8n.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
