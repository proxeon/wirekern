use crate::apps::AppStore;
use crate::error::Error;
use crate::insights::{InsightsQuery, InsightsReply};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::registry::Registry;
use crate::types::{
    AccountCreds, AccountKey, AppConfig, Capability, Deadline, Intent, Outcome, Probe, Site, WhoAmI,
};
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
        // Client-side idempotency: a retry with the same key returns the
        // first completed Outcome without touching the network. Only
        // *completed* publishes are remembered — an attempt that died after
        // the platform already created the post was never learned, so it
        // cannot dedupe (see docs/cli.md, --idempotency).
        let idem = intent.idempotency_key.clone();
        if let Some(idem) = idem.as_deref() {
            if let Some(out) = self.vault.get_outcome(key, idem)? {
                return Ok(out);
            }
        }
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut creds = self.vault.get(key)?;
        creds = self.maybe_refresh(&*publisher, &app, key, creds).await?;
        let out = match publisher
            .publish(&app, &creds, intent.clone(), deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds).await?;
                self.vault.put(key, &new)?;
                publisher.publish(&app, &new, intent, deadline).await
            }
            other => other,
        }?;
        // Record after success only: a failed attempt must stay retryable.
        if let Some(idem) = idem.as_deref() {
            self.vault.put_outcome(key, idem, &out)?;
        }
        Ok(out)
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

    /// Read metrics (026 read seam). Same orchestration as [`publish`](Self::publish)
    /// minus everything only a publication is entitled to: no idempotency
    /// (a read has no side effect to dedupe) and no ledger. The range is
    /// re-validated here even though the CLI checks it — the library cannot
    /// trust its callers to have done so.
    pub async fn insights(
        &self,
        key: &AccountKey,
        query: InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
        query
            .range
            .validate()
            .map_err(|reason| Error::InvalidQuery {
                site: key.site.clone(),
                reason,
            })?;
        let publisher = self.publisher(&key.site)?;
        if !publisher.capabilities().contains(&Capability::ReadMetrics) {
            return Err(Error::UnsupportedCapability {
                site: key.site.clone(),
                need: Capability::ReadMetrics,
            });
        }
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut creds = self.vault.get(key)?;
        creds = self.maybe_refresh(&*publisher, &app, key, creds).await?;
        match publisher.insights(&app, &creds, &query, deadline).await {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds).await?;
                self.vault.put(key, &new)?;
                publisher.insights(&app, &new, &query, deadline).await
            }
            other => other,
        }
    }

    /// Create-only probe (027): same routing, capability check and
    /// credentials as [`publish`](Self::publish), minus everything that
    /// only a real publication is entitled to.
    ///
    /// - No idempotency, read *or* write. The ledger stores completed
    ///   publishes; a probe that recorded its result would make a later
    ///   real publish with the same key "succeed" by replaying the probe,
    ///   and a probe that consulted it could be silenced by an old
    ///   publish. Neither state belongs to the other.
    /// - No proactive `maybe_refresh`. Refresh-on-publish is an
    ///   optimization; here the probe's own response is the instrument —
    ///   it reports the token state exactly as a publish would see it.
    ///   The reactive `token_expired` → refresh → retry mapping is kept,
    ///   because a probe that fails on a refreshable token would report
    ///   "broken" where the next publish would have self-healed.
    pub async fn probe(
        &self,
        key: &AccountKey,
        intent: Intent,
        deadline: Deadline,
    ) -> Result<Probe, Error> {
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
        let creds = self.vault.get(key)?;
        match publisher
            .probe(&app, &creds, intent.clone(), deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds).await?;
                self.vault.put(key, &new)?;
                publisher.probe(&app, &new, intent, deadline).await
            }
            other => other,
        }
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
        // Verify the token *before* anything lands in the vault: the old
        // store-then-whoami order persisted an invalid token and only then
        // rejected it. The id that comes back is persisted into `extra`,
        // matching the OAuth path (`creds_from_long`), so both auth shapes
        // publish against /{user_id}/threads instead of this path leaning
        // on the /me alias for its whole lifetime.
        let me = publisher.whoami(&app, &creds).await?;
        let creds = AccountCreds::OAuth2 {
            access_token: token.to_string(),
            refresh_token: None,
            extra: serde_json::json!({ "user_id": me.id }),
        };
        self.vault.put(key, &creds)?;
        Ok(me)
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
            // Proactive refresh is an optimization, not a prerequisite: it
            // fires while the stored token is still valid (up to 7 days
            // left), so a transient failure — network, 5xx, rate limit,
            // timeout — must degrade to publishing with the current token.
            // The next publish retries the refresh. Auth failures stay
            // fatal: a rejected refresh means the session is dead, and
            // failing fast with a re-auth error beats dying later inside
            // publish.
            Err(
                Error::Network { .. }
                | Error::RateLimited { .. }
                | Error::Platform { .. }
                | Error::DeadlineExceeded { .. },
            ) => Ok(creds),
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
