//! `postkit keys` — mint, list, revoke operator HTTP keys.
//!
//! The store is [`postkit::FileKeyStore`]. This module is CLI I/O only;
//! `postkit-serve` verifies keys and never creates them. Covered by
//! `FileKeyStore` tests plus `serve_and_keys_commands_parse`.

use crate::app::fail;
use crate::output::{emit_raw, human_line};
use postkit::FileKeyStore;
use std::path::Path;

pub fn create(home: &Path, name: &str, json: bool) -> Result<(), i32> {
    let store = FileKeyStore::new(home).map_err(|e| fail(&e, json))?;
    let created = store.create(name).map_err(|e| fail(&e, json))?;
    if json {
        emit_raw(&serde_json::json!({
            "name": created.name,
            "token": created.token,
        }));
    } else {
        eprintln!("shown once; store the hash only:");
        human_line(&created.token);
    }
    Ok(())
}

pub fn list(home: &Path, json: bool) -> Result<(), i32> {
    let store = FileKeyStore::new(home).map_err(|e| fail(&e, json))?;
    let keys = store.list().map_err(|e| fail(&e, json))?;
    if json {
        emit_raw(&serde_json::json!({ "keys": keys }));
    } else {
        for k in keys {
            human_line(format!("{} {}", k.name, k.created_at));
        }
    }
    Ok(())
}

pub fn revoke(home: &Path, name: &str, yes: bool, json: bool) -> Result<(), i32> {
    if !yes {
        eprintln!("pass --yes to revoke key {name}");
        return Err(2);
    }
    let store = FileKeyStore::new(home).map_err(|e| fail(&e, json))?;
    store.delete(name).map_err(|e| fail(&e, json))?;
    if json {
        emit_raw(&serde_json::json!({ "revoked": name }));
    } else {
        eprintln!("revoked {name}");
    }
    Ok(())
}
