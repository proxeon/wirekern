//! Shared CLI process helpers: vault home, Client construction, output, fail.
//!
//! Surfaces (subcommand modules and later `serve`) call these instead of
//! reaching into `main`. `make_client` is the shared Client constructor.

use crate::output::{emit_err, emit_raw, human_line};
use postkit::{
    valid_name, AllowWhatsAppSendsPolicy, Client, Error, FileAppStore, FileVault, Registry, Site,
};
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) fn print_results(results: &[serde_json::Value], json: bool) {
    if json {
        emit_raw(&serde_json::json!({ "results": results }));
    } else {
        for r in results {
            human_line(result_line(r));
        }
    }
}

pub(crate) fn result_line(v: &serde_json::Value) -> String {
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        let site = v.get("site").and_then(|s| s.as_str()).unwrap_or("-");
        let reason = v.get("reason").and_then(|r| r.as_str()).unwrap_or("");
        return format!("{err} {site} {reason}").trim_end().into();
    }
    let site = v.get("site").and_then(|s| s.as_str()).unwrap_or("-");
    // probe rows carry container_id, not id — never render them like posts
    if let Some(c) = v.get("container_id").and_then(|i| i.as_str()) {
        return format!("{site} {c} dry-run");
    }
    let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("-");
    let url = v.get("url").and_then(|u| u.as_str()).unwrap_or("");
    format!("{site} {id} {url}").trim_end().into()
}

pub(crate) fn parse_params(param: &[String], json: bool) -> Result<serde_json::Value, i32> {
    let mut map = serde_json::Map::new();
    for p in param {
        let Some((k, v)) = p.split_once('=') else {
            return Err(fail(
                &Error::InvalidPost {
                    site: Site::new(""),
                    reason: format!("param_not_k_eq_v:{p}"),
                    limit: None,
                },
                json,
            ));
        };
        map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
    }
    Ok(serde_json::Value::Object(map))
}

pub(crate) fn make_client(
    home: &std::path::Path,
    allow_whatsapp_send: bool,
) -> Result<Client, Error> {
    let mut registry = Registry::new();
    // Publisher-only sites register the frozen seam. Extra verbs attach as
    // facets on Connector so the next site cannot enlarge Publisher.
    registry.register(Arc::new(postkit::connectors::threads::Threads::new()?));
    registry.register(Arc::new(postkit::connectors::bluesky::Bluesky::new()?));
    registry.register_connector(postkit::connectors::meta_ads::MetaAds::new()?.connector());
    registry
        .register_connector(postkit::connectors::facebook_pages::FacebookPages::new()?.connector());
    registry.register_connector(postkit::connectors::instagram::Instagram::new()?.connector());
    registry
        .register_connector(postkit::connectors::whatsapp_cloud::WhatsAppCloud::new()?.connector());
    let vault = Arc::new(FileVault::new(home)?);
    let apps = Arc::new(FileAppStore::new(home)?);
    if allow_whatsapp_send {
        // The flag is intentionally inspected before client construction:
        // the default client has a deny-all WhatsApp policy, so a new command
        // cannot accidentally become a real customer-message write.
        Ok(Client::with_whatsapp_policy(
            registry,
            vault,
            apps,
            Arc::new(AllowWhatsAppSendsPolicy),
        ))
    } else {
        Ok(Client::new(registry, vault, apps))
    }
}

pub(crate) fn invalid_post(site: &str, reason: &str) -> postkit::Error {
    postkit::Error::InvalidPost {
        site: postkit::Site::new(site),
        reason: reason.into(),
        limit: None,
    }
}

pub(crate) fn resolve_home(
    flag_or_env: Option<PathBuf>,
    user_home: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if let Some(p) = flag_or_env {
        return Ok(p);
    }
    user_home.map(|h| h.join(".postkit")).ok_or_else(|| {
        "POSTKIT_HOME or HOME must be set to locate the vault; refusing to guess from the current directory".into()
    })
}

pub(crate) fn check_name(s: &str, json: bool) -> Result<(), i32> {
    if valid_name(s) {
        Ok(())
    } else {
        Err(fail(&Error::InvalidName(s.into()), json))
    }
}

pub(crate) fn fail(e: &Error, json: bool) -> i32 {
    emit_err(e, json);
    e.exit_code()
}
