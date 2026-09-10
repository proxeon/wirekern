#[cfg(feature = "meta-ads")]
use crate::ads::AdReviewWait;
use crate::ads::{
    AdReviewStatus, AdReviewStatusRequest, CreateLinkAdCreativeRequest, CreatePausedAdRequest,
    CreatedAd, CreatedAdCreative, CreativePreview, CreativePreviewRequest, UploadAdImageRequest,
    UploadedAdImage,
};
use crate::apps::AppStore;
use crate::error::Error;
use crate::insights::{AdAccountsReply, InsightsQuery, InsightsReply};
use crate::policy::{AdsAction, AdsPolicy, PausedOnlyAdsPolicy};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::registry::Registry;
use crate::types::{
    AccountCreds, AccountKey, AppConfig, Capability, Deadline, Intent, Outcome, Probe, Site, WhoAmI,
};
use crate::vault::Vault;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Meta review normally takes longer than a single HTTP response but should
/// never turn a CLI call into an unbounded background worker. The public wait
/// method always caps polls with the caller's existing `Deadline`.
#[cfg(feature = "meta-ads")]
const REVIEW_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

pub struct Client {
    registry: Registry,
    vault: Arc<dyn Vault>,
    apps: Arc<dyn AppStore>,
    ads_policy: Arc<dyn AdsPolicy>,
}

impl Client {
    pub fn new(registry: Registry, vault: Arc<dyn Vault>, apps: Arc<dyn AppStore>) -> Self {
        Self::with_ads_policy(registry, vault, apps, Arc::new(PausedOnlyAdsPolicy))
    }

    /// Construct a client with an explicitly chosen advertising policy. The
    /// normal constructor installs `PausedOnlyAdsPolicy`; callers can only
    /// loosen that contract by passing an intentional policy object here.
    pub fn with_ads_policy(
        registry: Registry,
        vault: Arc<dyn Vault>,
        apps: Arc<dyn AppStore>,
        ads_policy: Arc<dyn AdsPolicy>,
    ) -> Self {
        Self {
            registry,
            vault,
            apps,
            ads_policy,
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

    /// Discover remote advertising accounts for the credential. This is a
    /// read-only sibling of `insights`, not `Vault::list`: the latter returns
    /// local aliases while this call returns the platform's `act_<id>`s.
    pub async fn ad_accounts(
        &self,
        key: &AccountKey,
        deadline: Deadline,
    ) -> Result<AdAccountsReply, Error> {
        let publisher = self.publisher(&key.site)?;
        if !publisher
            .capabilities()
            .contains(&Capability::ReadAdAccounts)
        {
            return Err(Error::UnsupportedCapability {
                site: key.site.clone(),
                need: Capability::ReadAdAccounts,
            });
        }
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut creds = self.vault.get(key)?;
        creds = self.maybe_refresh(&*publisher, &app, key, creds).await?;
        match publisher.ad_accounts(&app, &creds, deadline).await {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds).await?;
                self.vault.put(key, &new)?;
                publisher.ad_accounts(&app, &new, deadline).await
            }
            other => other,
        }
    }

    /// Create a Meta advertising draft under the policy boundary. Approval
    /// happens before the vault is read, so a denied future spend action
    /// cannot refresh a token or send a request as a side effect.
    pub async fn create_paused_ad(
        &self,
        key: &AccountKey,
        request: CreatePausedAdRequest,
        deadline: Deadline,
    ) -> Result<CreatedAd, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::for_paused_create(&request.create))?;
        let publisher = self.publisher(&key.site)?;
        if !publisher
            .capabilities()
            .contains(&Capability::CreatePausedAds)
        {
            return Err(Error::UnsupportedCapability {
                site: key.site.clone(),
                need: Capability::CreatePausedAds,
            });
        }
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut creds = self.vault.get(key)?;
        creds = self.maybe_refresh(&*publisher, &app, key, creds).await?;
        match publisher
            .create_paused_ad(&app, &creds, &request, deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds).await?;
                self.vault.put(key, &new)?;
                publisher
                    .create_paused_ad(&app, &new, &request, deadline)
                    .await
            }
            other => other,
        }
    }

    /// Upload an image only after local validation and policy approval. An
    /// upload has no delivery status, but it is still a remote asset write and
    /// must not reach credentials or HTTP when a stricter policy refuses it.
    pub async fn upload_ad_image(
        &self,
        key: &AccountKey,
        request: UploadAdImageRequest,
        deadline: Deadline,
    ) -> Result<UploadedAdImage, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UploadAdImage)?;
        let publisher = self.publisher(&key.site)?;
        if !publisher
            .capabilities()
            .contains(&Capability::CreateAdCreative)
        {
            return Err(Error::UnsupportedCapability {
                site: key.site.clone(),
                need: Capability::CreateAdCreative,
            });
        }
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut creds = self.vault.get(key)?;
        creds = self.maybe_refresh(&*publisher, &app, key, creds).await?;
        match publisher
            .upload_ad_image(&app, &creds, &request, deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds).await?;
                self.vault.put(key, &new)?;
                publisher
                    .upload_ad_image(&app, &new, &request, deadline)
                    .await
            }
            other => other,
        }
    }

    /// Create a Page-backed image-link creative behind the same validation,
    /// policy, capability, and refresh ordering as every other Tier B write.
    /// The returned creative cannot deliver until a separate paused ad uses it.
    pub async fn create_link_ad_creative(
        &self,
        key: &AccountKey,
        request: CreateLinkAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::CreateLinkAdCreative)?;
        let publisher = self.publisher(&key.site)?;
        if !publisher
            .capabilities()
            .contains(&Capability::CreateAdCreative)
        {
            return Err(Error::UnsupportedCapability {
                site: key.site.clone(),
                need: Capability::CreateAdCreative,
            });
        }
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut creds = self.vault.get(key)?;
        creds = self.maybe_refresh(&*publisher, &app, key, creds).await?;
        match publisher
            .create_link_ad_creative(&app, &creds, &request, deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds).await?;
                self.vault.put(key, &new)?;
                publisher
                    .create_link_ad_creative(&app, &new, &request, deadline)
                    .await
            }
            other => other,
        }
    }

    /// Read the platform's rendering of an existing creative. Unlike the
    /// creative/upload methods above this has no policy decision: it is a
    /// GET-only review operation and cannot affect delivery, budget, billing,
    /// or the creative itself.
    pub async fn preview_ad_creative(
        &self,
        key: &AccountKey,
        request: CreativePreviewRequest,
        deadline: Deadline,
    ) -> Result<CreativePreview, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        let publisher = self.publisher(&key.site)?;
        if !publisher
            .capabilities()
            .contains(&Capability::ReadAdPreviews)
        {
            return Err(Error::UnsupportedCapability {
                site: key.site.clone(),
                need: Capability::ReadAdPreviews,
            });
        }
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut creds = self.vault.get(key)?;
        creds = self.maybe_refresh(&*publisher, &app, key, creds).await?;
        match publisher
            .preview_ad_creative(&app, &creds, &request, deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds).await?;
                self.vault.put(key, &new)?;
                publisher
                    .preview_ad_creative(&app, &new, &request, deadline)
                    .await
            }
            other => other,
        }
    }

    /// Read one ad object's configured and effective state once. This is a
    /// GET-only operation, so it bypasses `AdsPolicy`: inspecting a Meta
    /// review cannot activate an object, alter a budget, or affect billing.
    pub async fn ad_review_status(
        &self,
        key: &AccountKey,
        request: AdReviewStatusRequest,
        deadline: Deadline,
    ) -> Result<AdReviewStatus, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        let publisher = self.publisher(&key.site)?;
        if !publisher
            .capabilities()
            .contains(&Capability::ReadAdReviewStatus)
        {
            return Err(Error::UnsupportedCapability {
                site: key.site.clone(),
                need: Capability::ReadAdReviewStatus,
            });
        }
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut creds = self.vault.get(key)?;
        creds = self.maybe_refresh(&*publisher, &app, key, creds).await?;
        match publisher
            .ad_review_status(&app, &creds, &request, deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds).await?;
                self.vault.put(key, &new)?;
                publisher
                    .ad_review_status(&app, &new, &request, deadline)
                    .await
            }
            other => other,
        }
    }

    /// Poll review state only until `deadline`. The `PendingReview` reply is
    /// successful but explicit: it preserves the final known state and tells
    /// a script to retry later without recasting normal Meta review latency as
    /// a network timeout. This method exists only with the Meta connector,
    /// where Tokio's timer dependency is already part of the feature.
    #[cfg(feature = "meta-ads")]
    pub async fn wait_for_ad_review(
        &self,
        key: &AccountKey,
        request: AdReviewStatusRequest,
        deadline: Deadline,
    ) -> Result<AdReviewWait, Error> {
        self.wait_for_ad_review_with_interval(key, request, deadline, REVIEW_POLL_INTERVAL)
            .await
    }

    /// The interval-bearing helper keeps the production interval conservative
    /// while letting the deterministic client test prove the bounded polling
    /// behavior without sleeping for seconds.
    #[cfg(feature = "meta-ads")]
    pub(crate) async fn wait_for_ad_review_with_interval(
        &self,
        key: &AccountKey,
        request: AdReviewStatusRequest,
        deadline: Deadline,
        poll_interval: std::time::Duration,
    ) -> Result<AdReviewWait, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        let publisher = self.publisher(&key.site)?;
        if !publisher
            .capabilities()
            .contains(&Capability::ReadAdReviewStatus)
        {
            return Err(Error::UnsupportedCapability {
                site: key.site.clone(),
                need: Capability::ReadAdReviewStatus,
            });
        }
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut creds = self.vault.get(key)?;
        creds = self.maybe_refresh(&*publisher, &app, key, creds).await?;
        let mut retried_expired_token = false;

        loop {
            let status = match publisher
                .ad_review_status(&app, &creds, &request, deadline)
                .await
            {
                Err(Error::Auth { ref reason, .. })
                    if reason == "token_expired" && !retried_expired_token =>
                {
                    // Match every other Client read: an expired token gets
                    // one refresh and one retry, never an unbounded refresh
                    // loop hidden inside a status poller.
                    creds = publisher.refresh(&app, &creds).await?;
                    self.vault.put(key, &creds)?;
                    retried_expired_token = true;
                    continue;
                }
                other => other,
            }?;
            if !status.is_pending_review() {
                return Ok(AdReviewWait::Settled(status));
            }

            let remaining = deadline.remaining();
            if remaining.is_zero() {
                return Ok(AdReviewWait::PendingReview(status));
            }
            // A zero supplied interval would otherwise busy-spin in an
            // embedding program. Sleeping the remaining deadline makes it a
            // single bounded observation instead.
            let delay = if poll_interval.is_zero() {
                remaining
            } else {
                poll_interval.min(remaining)
            };
            tokio::time::sleep(delay).await;
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
