use crate::apps::AppStore;
use crate::error::Error;
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::registry::Registry;
use crate::types::{AccountCreds, AccountKey, AppConfig, Deadline, Intent, Outcome, Site, WhoAmI};
use crate::vault::Vault;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Client {
    registry: Registry,
    vault: Arc<dyn Vault>,
    apps: Arc<dyn AppStore>,
}

impl Client {
    pub fn new(registry: Registry, vault: Arc<dyn Vault>, apps: Arc<dyn AppStore>) -> Self {
        Self {
            registry,
            vault,
            apps,
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn vault(&self) -> &dyn Vault {
        &*self.vault
    }

    pub fn apps(&self) -> &dyn AppStore {
        &*self.apps
    }

    pub async fn publish(
        &self,
        key: &AccountKey,
        intent: Intent,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        if key.site != intent.site {
            return Err(Error::InvalidPost {
                site: intent.site,
                reason: "site_mismatch".into(),
                limit: None,
            });
        }
        let publisher = self.publisher(&intent.site)?;
        let need = intent.body.required_capability();
        if !publisher.capabilities().contains(&need) {
            return Err(Error::UnsupportedCapability {
                site: intent.site.clone(),
                need,
            });
        }
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut creds = self.vault.get(key)?;
        creds = self.maybe_refresh(&*publisher, &app, key, creds).await?;
        match publisher
            .publish(&app, &creds, intent.clone(), deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds).await?;
                self.vault.put(key, &new)?;
                publisher.publish(&app, &new, intent, deadline).await
            }
            other => other,
        }
    }

    pub async fn whoami(&self, key: &AccountKey) -> Result<WhoAmI, Error> {
        let publisher = self.publisher(&key.site)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        publisher.whoami(&app, &creds).await
    }

    pub async fn auth_start(&self, site: &Site) -> Result<AuthStart, Error> {
        let publisher = self.publisher(site)?;
        let app = self.apps.get(site).unwrap_or_else(|_| empty_app(site));
        publisher.auth_start(&app).await
    }

    pub async fn auth_finish(&self, key: &AccountKey, reply: AuthReply) -> Result<WhoAmI, Error> {
        let publisher = self.publisher(&key.site)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = publisher.auth_finish(&app, reply).await?;
        self.vault.put(key, &creds)?;
        publisher.whoami(&app, &creds).await
    }

    /// 009 bootstrap. Does not require an app file.
    pub async fn put_token(&self, key: &AccountKey, token: &str) -> Result<WhoAmI, Error> {
        let publisher = self.publisher(&key.site)?;
        // A raw token is an OAuth2 bootstrap. On app-password sites (Bluesky)
        // the write used to succeed and publish failed much later with a
        // cred-kind error far from the actual mistake. The connector's
        // declared auth kind is the contract; enforce it at the door so the
        // refusal lands where the flag was typed.
        if publisher.auth_kind() != AuthKind::OAuth2AuthCode {
            return Err(Error::Auth {
                site: key.site.clone(),
                reason: "token_bootstrap_unsupported".into(),
            });
        }
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = AccountCreds::OAuth2 {
            access_token: token.to_string(),
            refresh_token: None,
            extra: serde_json::json!({}),
        };
        self.vault.put(key, &creds)?;
        publisher.whoami(&app, &creds).await
    }

    fn publisher(&self, site: &Site) -> Result<Arc<dyn Publisher>, Error> {
        self.registry
            .get(site)
            .ok_or_else(|| Error::UnknownSite(site.clone()))
    }

    async fn maybe_refresh(
        &self,
        publisher: &dyn Publisher,
        app: &AppConfig,
        key: &AccountKey,
        creds: AccountCreds,
    ) -> Result<AccountCreds, Error> {
        if !refresh_is_due(&creds) {
            return Ok(creds);
        }
        match publisher.refresh(app, &creds).await {
            Ok(new) => {
                self.vault.put(key, &new)?;
                Ok(new)
            }
            Err(Error::Auth { reason, .. }) if reason == "no_refresh" => Ok(creds),
            Err(e) => Err(e),
        }
    }
}

fn empty_app(site: &Site) -> AppConfig {
    AppConfig {
        site: site.clone(),
        oauth: None,
        extra: serde_json::json!({}),
    }
}

/// Proactive refresh: `expires_at` within 7 days and last refresh ≥ 24h (009).
pub fn refresh_is_due(creds: &AccountCreds) -> bool {
    let AccountCreds::OAuth2 { extra, .. } = creds else {
        return false;
    };
    let expires_at = extra.get("expires_at").and_then(|v| v.as_u64());
    let Some(expires_at) = expires_at else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if expires_at.saturating_sub(now) > 7 * 24 * 3600 {
        return false;
    }
    let refreshed_at = extra
        .get("refreshed_at")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    now.saturating_sub(refreshed_at) >= 24 * 3600
}
