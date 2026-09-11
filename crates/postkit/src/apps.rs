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
        let from_file = self.inner.lock().expect("apps").get(site).cloned();
        resolve_app_config(site, from_file).ok_or_else(|| Error::Auth {
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

const WHATSAPP_CLOUD_SITE: &str = "whatsapp_cloud";

/// The two WhatsApp settings have independent sources. Keeping the raw
/// presence of each variable lets a deployment replace its sender ID without
/// accidentally erasing a separately stored webhook-signing secret.
#[derive(Clone, Debug, Default)]
struct WhatsAppEnvOverride {
    phone_number_id: Option<String>,
    app_secret: Option<String>,
    waba_id: Option<String>,
}

impl WhatsAppEnvOverride {
    fn from_process() -> Self {
        Self {
            phone_number_id: std::env::var("POSTKIT_WHATSAPP_PHONE_NUMBER_ID").ok(),
            app_secret: std::env::var("POSTKIT_WHATSAPP_APP_SECRET").ok(),
            waba_id: std::env::var("POSTKIT_WHATSAPP_WABA_ID").ok(),
        }
    }

    fn is_empty(&self) -> bool {
        self.phone_number_id.is_none() && self.app_secret.is_none() && self.waba_id.is_none()
    }
}

/// Resolve the config a store loaded with the applicable environment values.
/// OAuth sites retain their long-standing all-or-nothing environment override.
/// WhatsApp is intentionally different: the phone-number ID and webhook app
/// secret are independent operational settings, so each environment variable
/// replaces only its corresponding file field.
pub(crate) fn resolve_app_config(site: &Site, from_file: Option<AppConfig>) -> Option<AppConfig> {
    if site.as_str() == WHATSAPP_CLOUD_SITE {
        return merge_whatsapp_config(site, from_file, WhatsAppEnvOverride::from_process());
    }
    env_override(site).or(from_file)
}

/// Merge a saved WhatsApp configuration with explicit environment values.
/// This small pure function keeps precedence testable without mutating the
/// process-global environment that Rust tests share.
fn merge_whatsapp_config(
    site: &Site,
    from_file: Option<AppConfig>,
    from_env: WhatsAppEnvOverride,
) -> Option<AppConfig> {
    // A secret alone cannot identify the Cloud API sender. Preserve the old
    // env-only contract by requiring either a saved config or an env phone ID.
    if from_file.is_none() && from_env.phone_number_id.is_none() {
        return None;
    }

    let mut config = from_file.unwrap_or(AppConfig {
        site: site.clone(),
        oauth: None,
        extra: serde_json::json!({}),
    });
    // `extra` is extensible connector-owned data. Start with every saved
    // key, then replace only the two fields this connector exposes through
    // the environment; future metadata therefore cannot be silently lost.
    let mut extra = config.extra.as_object().cloned().unwrap_or_default();
    if let Some(phone_number_id) = from_env.phone_number_id {
        extra.insert("phone_number_id".into(), phone_number_id.into());
    }
    if let Some(app_secret) = from_env.app_secret {
        extra.insert("app_secret".into(), app_secret.into());
    }
    if let Some(waba_id) = from_env.waba_id {
        extra.insert("waba_id".into(), waba_id.into());
    }
    config.extra = serde_json::Value::Object(extra);
    Some(config)
}

/// OAuth sites use `POSTKIT_THREADS_CLIENT_ID` / `_CLIENT_SECRET` /
/// `_REDIRECT_URI` (site uppercased). WhatsApp Cloud is intentionally the
/// exception: it has a static System User token in the vault and needs only a
/// phone-number ID plus optional webhook app secret as application config.
pub fn env_override(site: &Site) -> Option<AppConfig> {
    let key = site.as_str().to_ascii_uppercase().replace('-', "_");
    if site.as_str() == WHATSAPP_CLOUD_SITE {
        // `env_override` remains the environment-only view for callers that
        // need it. AppStore::get uses `resolve_app_config` above to add the
        // file fallback for fields this view does not contain.
        return merge_whatsapp_config(site, None, WhatsAppEnvOverride::from_process());
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

/// Whether any environment value participates in the resolved configuration.
/// OAuth credentials replace the file as a unit. WhatsApp fields are merged,
/// so `"env"` means at least one of its two fields comes from the environment,
/// not that a saved webhook secret has been discarded.
pub fn app_source(site: &Site) -> &'static str {
    if site.as_str() == WHATSAPP_CLOUD_SITE {
        return if WhatsAppEnvOverride::from_process().is_empty() {
            "file"
        } else {
            "env"
        };
    }
    if env_override(site).is_some() {
        "env"
    } else {
        "file"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn whatsapp_file_config() -> AppConfig {
        AppConfig {
            site: Site::new(WHATSAPP_CLOUD_SITE),
            oauth: None,
            extra: json!({
                "phone_number_id": "file-phone",
                "app_secret": "file-secret",
                "future_setting": "preserved",
            }),
        }
    }

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

    #[test]
    fn whatsapp_phone_env_override_keeps_file_webhook_secret() {
        let site = Site::new(WHATSAPP_CLOUD_SITE);
        let merged = merge_whatsapp_config(
            &site,
            Some(whatsapp_file_config()),
            WhatsAppEnvOverride {
                phone_number_id: Some("env-phone".into()),
                app_secret: None,
                waba_id: None,
            },
        )
        .unwrap();

        assert_eq!(merged.extra["phone_number_id"], "env-phone");
        assert_eq!(merged.extra["app_secret"], "file-secret");
        assert_eq!(merged.extra["future_setting"], "preserved");
    }

    #[test]
    fn whatsapp_secret_env_override_wins_without_changing_file_phone() {
        let site = Site::new(WHATSAPP_CLOUD_SITE);
        let merged = merge_whatsapp_config(
            &site,
            Some(whatsapp_file_config()),
            WhatsAppEnvOverride {
                phone_number_id: None,
                app_secret: Some("env-secret".into()),
                waba_id: None,
            },
        )
        .unwrap();

        assert_eq!(merged.extra["phone_number_id"], "file-phone");
        assert_eq!(merged.extra["app_secret"], "env-secret");
    }

    #[test]
    fn whatsapp_env_only_needs_a_phone_and_never_leaks_secret_in_debug() {
        let site = Site::new(WHATSAPP_CLOUD_SITE);
        assert!(merge_whatsapp_config(
            &site,
            None,
            WhatsAppEnvOverride {
                phone_number_id: None,
                app_secret: Some("secret-without-sender".into()),
                waba_id: None,
            },
        )
        .is_none());

        let merged = merge_whatsapp_config(
            &site,
            None,
            WhatsAppEnvOverride {
                phone_number_id: Some("env-phone".into()),
                app_secret: Some("secret-not-for-diagnostics".into()),
                waba_id: Some("102290129340398".into()),
            },
        )
        .unwrap();
        assert!(format!("{merged:?}").contains("[opaque]"));
        assert!(!format!("{merged:?}").contains("secret-not-for-diagnostics"));
        assert_eq!(merged.extra["waba_id"], "102290129340398");
    }
}
