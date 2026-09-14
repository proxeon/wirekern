//! `wirekern accounts` — list and delete vault aliases.
use crate::app::fail;
use crate::output::{emit_raw, human_line};
use clap::Subcommand;
use std::path::Path;
use wirekern::{AccountKey, FileVault, Site, Vault};

#[derive(Subcommand, Debug)]
pub(crate) enum AccountsCmd {
    List {
        #[arg(long)]
        site: Option<String>,
    },
    Delete {
        site: String,
        #[arg(long)]
        yes: bool,
    },
}

pub(crate) fn run(cmd: AccountsCmd, home: &Path, account: &str, json: bool) -> Result<(), i32> {
    match cmd {
        AccountsCmd::List { site } => {
            let vault = FileVault::new(home).map_err(|e| fail(&e, json))?;
            let filter = site.as_deref().map(Site::new);
            let keys = vault.list(filter.as_ref()).map_err(|e| fail(&e, json))?;
            if json {
                let rows: Vec<_> = keys
                    .iter()
                    .map(|k| serde_json::json!({ "site": k.site, "name": k.name }))
                    .collect();
                emit_raw(&serde_json::json!({ "accounts": rows }));
            } else {
                for k in keys {
                    human_line(format!("{}/{}", k.site, k.name));
                }
            }
            Ok(())
        }
        AccountsCmd::Delete { site, yes } => {
            if !yes {
                eprintln!("pass --yes to delete {site}/{account}");
                return Err(2);
            }
            let vault = FileVault::new(home).map_err(|e| fail(&e, json))?;
            let key = AccountKey::new(&site, account);
            vault.delete(&key).map_err(|e| fail(&e, json))?;
            if json {
                emit_raw(&serde_json::json!({ "deleted": { "site": site, "name": account } }));
            } else {
                eprintln!("deleted {site}/{account}");
            }
            Ok(())
        }
    }
}
