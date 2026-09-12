//! `postkit apps` — show and write local OAuth app files.
use crate::app::{check_name, fail};
use crate::output::{emit_raw, human_line};
use clap::Subcommand;
use postkit::{app_source, AppConfig, AppStore, FileAppStore, OAuthApp, Site};
use std::path::Path;

#[derive(Subcommand, Debug)]
pub(crate) enum AppsCmd {
    Show {
        site: String,
    },
    Set {
        site: String,
        #[arg(long)]
        client_id: String,
        #[arg(long)]
        client_secret: String,
        #[arg(long)]
        redirect_uri: String,
    },
}

pub(crate) fn run(cmd: AppsCmd, home: &Path, json: bool) -> Result<(), i32> {
    match cmd {
        AppsCmd::Set {
            site,
            client_id,
            client_secret,
            redirect_uri,
        } => {
            check_name(&site, json)?;
            let apps = FileAppStore::new(home).map_err(|e| fail(&e, json))?;
            apps.put(&AppConfig {
                site: Site::new(&site),
                oauth: Some(OAuthApp {
                    client_id,
                    client_secret,
                    redirect_uri,
                }),
                extra: serde_json::json!({}),
            })
            .map_err(|e| fail(&e, json))?;
            if app_source(&Site::new(&site)) == "env" {
                let key = site.to_ascii_uppercase().replace('-', "_");
                eprintln!(
                    "note: POSTKIT_{key}_CLIENT_ID/_CLIENT_SECRET are set and take precedence over apps/{site}.json"
                );
            }
            if json {
                emit_raw(&serde_json::json!({ "site": site }));
            } else {
                eprintln!("wrote {}/apps/{site}.json", home.display());
            }
            Ok(())
        }
        AppsCmd::Show { site } => {
            check_name(&site, json)?;
            let apps = FileAppStore::new(home).map_err(|e| fail(&e, json))?;
            let cfg = apps.get(&Site::new(&site)).map_err(|e| fail(&e, json))?;
            if site == "whatsapp_cloud" {
                let phone_number_id = cfg
                    .extra
                    .get("phone_number_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let webhook_signing = cfg
                    .extra
                    .get("app_secret")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|secret| !secret.is_empty());
                let webhook_verify = cfg
                    .extra
                    .get("verify_token")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|token| !token.is_empty());
                let source = app_source(&Site::new(&site));
                if json {
                    emit_raw(&serde_json::json!({
                        "site": site,
                        "source": source,
                        "phone_number_id": phone_number_id,
                        "webhook_signing": webhook_signing,
                        "webhook_verify": webhook_verify,
                    }));
                } else {
                    human_line(format!(
                        "site=whatsapp_cloud source={source} phone_number_id={phone_number_id} webhook_signing={webhook_signing} webhook_verify={webhook_verify} app_secret=[redacted] verify_token=[redacted]"
                    ));
                }
                return Ok(());
            }
            let id = cfg
                .oauth
                .as_ref()
                .map(|o| o.client_id.as_str())
                .unwrap_or("");
            let redir = cfg
                .oauth
                .as_ref()
                .map(|o| o.redirect_uri.as_str())
                .unwrap_or("");
            let source = app_source(&Site::new(&site));
            if json {
                emit_raw(&serde_json::json!({
                    "site": site,
                    "source": source,
                    "client_id": id,
                    "redirect_uri": redir,
                }));
            } else {
                human_line(format!(
                    "site={site} source={source} client_id={id} redirect_uri={redir} client_secret=[redacted]"
                ));
            }
            Ok(())
        }
    }
}
