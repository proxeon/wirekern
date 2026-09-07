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

/// `POSTKIT_THREADS_CLIENT_ID` / `_CLIENT_SECRET` / `_REDIRECT_URI` (site uppercased).
pub fn env_override(site: &Site) -> Option<AppConfig> {
    let key = site.as_str().to_ascii_uppercase().replace('-', "_");
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
