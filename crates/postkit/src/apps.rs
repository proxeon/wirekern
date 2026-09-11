use crate::error::Error;
use crate::types::{AppConfig, OAuthApp, Site};
use std::collections::HashMap;
use std::sync::Mutex;

pub trait AppStore: Send + Sync {
    fn get(&self, site: &Site) -> Result<AppConfig, Error>;
    fn put(&self, cfg: &AppConfig) -> Result<(), Error>;
}

#[derive(Default)]
pub struct MemoryAppStore {
    inner: Mutex<HashMap<Site, AppConfig>>,
}

impl MemoryAppStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

impl AppStore for MemoryAppStore {
    fn get(&self, site: &Site) -> Result<AppConfig, Error> {
        if let Some(from_env) = env_override(site) {
            return Ok(from_env);
        }
        self.inner
            .lock()
            .expect("apps")
            .get(site)
            .cloned()
            .ok_or_else(|| Error::Auth {
                site: site.clone(),
                reason: "missing_app_config".into(),
            })
    }

    fn put(&self, cfg: &AppConfig) -> Result<(), Error> {
        self.inner
            .lock()
            .expect("apps")
            .insert(cfg.site.clone(), cfg.clone());
        Ok(())
    }
}

/// OAuth sites use `POSTKIT_THREADS_CLIENT_ID` / `_CLIENT_SECRET` /
/// `_REDIRECT_URI` (site uppercased). WhatsApp Cloud is intentionally the
/// exception: it has a static System User token in the vault and needs only a
/// phone-number ID plus optional webhook app secret as application config.
pub fn env_override(site: &Site) -> Option<AppConfig> {
    let key = site.as_str().to_ascii_uppercase().replace('-', "_");
    if site.as_str() == "whatsapp_cloud" {
        let phone_number_id = std::env::var("POSTKIT_WHATSAPP_PHONE_NUMBER_ID").ok()?;
        let app_secret = std::env::var("POSTKIT_WHATSAPP_APP_SECRET").ok();
        return Some(AppConfig {
            site: site.clone(),
            oauth: None,
            // `AppConfig::Debug` keeps extra opaque: a webhook app secret
            // must be as safe in diagnostics as the bearer token in vault.
            extra: serde_json::json!({
                "phone_number_id": phone_number_id,
                "app_secret": app_secret,
            }),
        });
    }
    let id = std::env::var(format!("POSTKIT_{key}_CLIENT_ID")).ok()?;
    let secret = std::env::var(format!("POSTKIT_{key}_CLIENT_SECRET")).ok()?;
    let redirect = std::env::var(format!("POSTKIT_{key}_REDIRECT_URI"))
        .unwrap_or_else(|_| "https://localhost/callback".into());
    Some(AppConfig {
        site: site.clone(),
        oauth: Some(OAuthApp {
            client_id: id,
            client_secret: secret,
            redirect_uri: redirect,
        }),
        extra: serde_json::json!({}),
    })
}

/// Which layer answers `AppStore::get` for this site: env vars outrank the
/// `apps/<site>.json` file, and that shadowing must be visible — `apps set`
/// warns when its write will be shadowed, `apps show` reports the source.
pub fn app_source(site: &Site) -> &'static str {
    if env_override(site).is_some() {
        "env"
    } else {
        "file"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unique site name: env tests set process-global vars, so the name must
    // not collide with any other test's site.
    #[test]
    fn app_source_reports_env_shadowing() {
        let site = Site::new("zzenvtests");
        std::env::remove_var("POSTKIT_ZZENVTESTS_CLIENT_ID");
        assert_eq!(app_source(&site), "file");

        std::env::set_var("POSTKIT_ZZENVTESTS_CLIENT_ID", "id");
        // client_id alone is not enough — env_override needs the secret too
        assert_eq!(app_source(&site), "file");

        std::env::set_var("POSTKIT_ZZENVTESTS_CLIENT_SECRET", "sec");
        assert_eq!(app_source(&site), "env");

        std::env::remove_var("POSTKIT_ZZENVTESTS_CLIENT_ID");
        std::env::remove_var("POSTKIT_ZZENVTESTS_CLIENT_SECRET");
    }
}
