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
use crate::vault::{Claim, Vault};
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
            // Claim the key before publishing (issue 023): without it, two
            // concurrent same-key callers both pass the ledger check above
            // and both publish — a duplicate public post reported as two
            // successes. A Taken answer is a distinct transient error; the
            // caller retries and the ledger then answers.
            match self.vault.claim_outcome(key, idem)? {
                Claim::Free => {}
                Claim::Taken => {
                    return Err(Error::IdempotencyInFlight {
                        site: key.site.clone(),
                        key: idem.to_string(),
                    })
                }
            }
        }
        // One confined attempt so the claim has exactly one release point:
        // every early `?` inside publish_once lands here, not in the caller.
        let attempt = self.publish_once(&*publisher, key, intent, deadline).await;
        let out = match attempt {
            Ok(out) => out,
            Err(e) => {
                // A failed attempt must stay retryable — the claim must
                // not outlive it.
                self.release_claim(key, idem.as_deref());
                return Err(e);
            }
        };
        // Record after success only: a failed attempt must stay retryable.
        if let Some(idem) = idem.as_deref() {
            let recorded = self.vault.put_outcome(key, idem, &out);
            self.release_claim(key, Some(idem));
            recorded?;
        }
        Ok(out)
    }

    /// The claim-guarded publish: app lookup, credential fetch, proactive
    /// refresh, publish, and the one reactive token-expiry retry. Extracted
    /// from `publish` so the idempotency claim can bracket it with a single
    /// release point — an early `?` here releases the claim on return,
    /// never leaks the key until the TTL steals it.
    async fn publish_once(
        &self,
        publisher: &dyn Publisher,
        key: &AccountKey,
        intent: Intent,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut creds = self.vault.get(key)?;
        creds = self
            .maybe_refresh(publisher, &app, key, creds, deadline)
            .await?;
        let out = match publisher
            .publish(&app, &creds, intent.clone(), deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds, deadline).await?;
                self.vault.put(key, &new)?;
                publisher.publish(&app, &new, intent, deadline).await
            }
            other => other,
        }?;
        Ok(out)
    }

    /// Release an idempotency claim on every exit path. Failures are
    /// swallowed deliberately: the file claim's TTL self-heals a stuck
    /// claim, and a release error must not mask the publish's real result.
    fn release_claim(&self, key: &AccountKey, idem: Option<&str>) {
        if let Some(idem) = idem {
            let _ = self.vault.release_outcome(key, idem);
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
        creds = self
            .maybe_refresh(&*publisher, &app, key, creds, deadline)
            .await?;
        match publisher.insights(&app, &creds, &query, deadline).await {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds, deadline).await?;
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
        creds = self
            .maybe_refresh(&*publisher, &app, key, creds, deadline)
            .await?;
        match publisher.ad_accounts(&app, &creds, deadline).await {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds, deadline).await?;
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
        creds = self
            .maybe_refresh(&*publisher, &app, key, creds, deadline)
            .await?;
        match publisher
            .create_paused_ad(&app, &creds, &request, deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds, deadline).await?;
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
        creds = self
            .maybe_refresh(&*publisher, &app, key, creds, deadline)
            .await?;
        match publisher
            .upload_ad_image(&app, &creds, &request, deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds, deadline).await?;
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
        creds = self
            .maybe_refresh(&*publisher, &app, key, creds, deadline)
            .await?;
        match publisher
            .create_link_ad_creative(&app, &creds, &request, deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds, deadline).await?;
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
        creds = self
            .maybe_refresh(&*publisher, &app, key, creds, deadline)
            .await?;
        match publisher
            .preview_ad_creative(&app, &creds, &request, deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds, deadline).await?;
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
        creds = self
            .maybe_refresh(&*publisher, &app, key, creds, deadline)
            .await?;
        match publisher
            .ad_review_status(&app, &creds, &request, deadline)
            .await
        {
            Err(Error::Auth { ref reason, .. }) if reason == "token_expired" => {
                let new = publisher.refresh(&app, &creds, deadline).await?;
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
        creds = self
            .maybe_refresh(&*publisher, &app, key, creds, deadline)
            .await?;
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
                    creds = publisher.refresh(&app, &creds, deadline).await?;
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
                let new = publisher.refresh(&app, &creds, deadline).await?;
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
        deadline: Deadline,
    ) -> Result<AccountCreds, Error> {
        if !refresh_is_due(&creds) {
            return Ok(creds);
        }
        // The caller's deadline governs the refresh too (issue 024): one
        // budget for refresh plus the request it serves. A deadline spent
        // here degrades below — publish then fails fast with the same
        // DeadlineExceeded instead of duplicating the wait.
        match publisher.refresh(app, &creds, deadline).await {
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

/// Draft orchestration (plans/001/013). Everything here composes the Tier B
/// methods above; no new remote verb is introduced. The only new state is
/// the operator-selected checkpoint file, and the only new protocol is the
/// write-ahead `in_flight` marker that turns an ambiguous remote write into
/// a refusal instead of a guess.
#[cfg(feature = "draft")]
mod draft_run {
    use super::Client;
    use crate::ads::{
        AdEntity, AdReviewStatusRequest, CreateLinkAdCreativeRequest, CreatePausedAdRequest,
        LinkAdCreative, PausedAd, PausedAdCreate, PausedAdset, PausedCampaign,
        UploadAdImageRequest,
    };
    use crate::draft::{
        adoption_entity, manifest_fingerprint, DraftImage, DraftStage, DraftStatusReply, DraftStep,
        DraftStore, PausedDraftManifest, PausedDraftResult, PausedDraftState, CONFIGURED_PAUSED,
        RECONCILE_GUIDANCE,
    };
    use crate::error::Error;
    use crate::policy::AdsAction;
    use crate::types::{AccountKey, Deadline, Site};
    use std::path::Path;

    impl Client {
        /// Zero-I/O validation: the answer to `validate-draft`. Checks the
        /// manifest's closed contracts (pairing table, budget floor, enums,
        /// HTTPS destination, image basename) and returns the account and
        /// fingerprint a run would use. Never reads the vault, the image
        /// file, or any state.
        pub fn validate_paused_draft(
            &self,
            site: &Site,
            manifest: &PausedDraftManifest,
        ) -> Result<(String, String), Error> {
            let account = manifest
                .normalized_account()
                .map_err(|reason| Error::InvalidQuery {
                    site: site.clone(),
                    reason,
                })?;
            manifest.validate().map_err(|reason| Error::InvalidQuery {
                site: site.clone(),
                reason,
            })?;
            Ok((account, manifest_fingerprint(manifest)))
        }

        /// Execute or resume the manifest's paused hierarchy, checkpointing
        /// after every confirmed remote write. `resume` selects the
        /// existing-state contract (`resume-draft`); without it the state
        /// path must be new (`create-draft`).
        ///
        /// Return contract: `Ok(ReconciliationRequired)` is a *successful*
        /// protocol outcome — an ambiguous write leaves the `in_flight`
        /// marker in place and demands human reconciliation. A `Err` means
        /// the run definitively failed (Meta answered, or nothing left the
        /// machine) and is safe to re-run from the last checkpoint.
        pub async fn run_paused_draft(
            &self,
            run: crate::draft::RunPausedDraft<'_>,
        ) -> Result<PausedDraftResult, Error> {
            let crate::draft::RunPausedDraft {
                key,
                manifest,
                image,
                store,
                state_path,
                resume,
                deadline,
            } = run;
            let site = key.site.clone();
            manifest.validate().map_err(|reason| Error::InvalidQuery {
                site: site.clone(),
                reason,
            })?;
            let fingerprint = manifest_fingerprint(manifest);
            let account = manifest
                .normalized_account()
                .map_err(|reason| Error::InvalidQuery {
                    site: site.clone(),
                    reason,
                })?;
            // Exclusive lock first: two concurrent runs would both issue
            // the next write, which is exactly the duplicate the protocol
            // exists to prevent.
            let _lock = store
                .try_lock(state_path)
                .map_err(|reason| Error::InvalidQuery {
                    site: site.clone(),
                    reason,
                })?;
            let mut state = if resume {
                let state = store
                    .read(state_path)
                    .map_err(|reason| Error::InvalidQuery {
                        site: site.clone(),
                        reason,
                    })?;
                // A different manifest (budget, audience, copy, account)
                // must never inherit a partial hierarchy built from the old
                // one; the canonical fingerprint makes that a hard refusal.
                if state.manifest_fingerprint != fingerprint {
                    return Err(Error::InvalidQuery {
                        site: site.clone(),
                        reason: "draft_manifest_changed".into(),
                    });
                }
                if state.site != site.as_str() {
                    return Err(Error::InvalidQuery {
                        site: site.clone(),
                        reason: "draft_state_site".into(),
                    });
                }
                if state.account_id != account {
                    return Err(Error::InvalidQuery {
                        site: site.clone(),
                        reason: "draft_state_account".into(),
                    });
                }
                if let Some(step) = state.in_flight {
                    return Ok(PausedDraftResult::ReconciliationRequired {
                        site: site.clone(),
                        account_id: state.account_id.clone(),
                        step: step.as_str(),
                        guidance: RECONCILE_GUIDANCE,
                    });
                }
                // A completed hierarchy is read-only: no further creates.
                if state.stage == DraftStage::Completed {
                    return completed_result(&site, &state);
                }
                state
            } else {
                let state = PausedDraftState::new(&site, account.clone(), fingerprint.clone());
                store
                    .create_new(state_path, &state)
                    .map_err(|reason| Error::InvalidQuery {
                        site: site.clone(),
                        reason,
                    })?;
                state
            };

            for step in DraftStep::ALL {
                if !state.step_pending(step) {
                    continue; // checkpointed by a previous run
                }
                deadline.check(&site)?;
                // Policy before the marker: a denied step must leave the
                // state exactly as it was found.
                self.authorize_draft_step(&site, step)?;
                let upload = self.image_upload_for(&site, image, &account, step)?;
                // Write-ahead: record that this write is about to happen
                // before it can, so a crash can never leave "no marker and
                // an existing remote object".
                state.in_flight = Some(step);
                store
                    .checkpoint(state_path, &state)
                    .map_err(|reason| Error::InvalidQuery {
                        site: site.clone(),
                        reason,
                    })?;

                let outcome: Result<String, Error> = match step {
                    DraftStep::Image => self
                        .upload_ad_image(key, upload.expect("checked above"), deadline)
                        .await
                        .map(|uploaded| uploaded.hash),
                    DraftStep::Campaign => {
                        let request = CreatePausedAdRequest {
                            account: Some(account.clone()),
                            create: PausedAdCreate::Campaign(PausedCampaign {
                                name: manifest.campaign.name.clone(),
                                objective: manifest.campaign.objective,
                                special_ad_categories: manifest
                                    .campaign
                                    .special_ad_categories
                                    .clone(),
                            }),
                        };
                        self.create_paused_ad(key, request, deadline)
                            .await
                            .map(|created| created.id)
                    }
                    DraftStep::Adset => {
                        let request = CreatePausedAdRequest {
                            account: Some(account.clone()),
                            create: PausedAdCreate::Adset(PausedAdset {
                                name: manifest.adset.name.clone(),
                                // Only a checkpointed campaign ID is ever
                                // wired in — the operator never retypes it.
                                campaign_id: state
                                    .campaign_id
                                    .clone()
                                    .expect("lattice guarantees the campaign"),
                                daily_budget: manifest.adset.daily_budget,
                                bid_strategy: manifest.adset.bid_strategy,
                                billing_event: manifest.adset.billing_event.clone(),
                                optimization_goal: manifest.adset.optimization_goal.clone(),
                                targeting: manifest.adset.targeting.clone(),
                            }),
                        };
                        self.create_paused_ad(key, request, deadline)
                            .await
                            .map(|created| created.id)
                    }
                    DraftStep::Creative => {
                        let request = CreateLinkAdCreativeRequest {
                            account: Some(account.clone()),
                            creative: LinkAdCreative {
                                name: manifest.creative.name.clone(),
                                page_id: manifest.creative.page_id.clone(),
                                image_hash: state
                                    .image_hash
                                    .clone()
                                    .expect("lattice guarantees the image"),
                                message: manifest.creative.message.clone(),
                                headline: manifest.creative.headline.clone(),
                                destination_url: manifest.creative.destination_url.clone(),
                                call_to_action: manifest.creative.call_to_action,
                            },
                        };
                        self.create_link_ad_creative(key, request, deadline)
                            .await
                            .map(|created| created.id)
                    }
                    DraftStep::Ad => {
                        let request = CreatePausedAdRequest {
                            account: Some(account.clone()),
                            create: PausedAdCreate::Ad(PausedAd {
                                name: manifest.ad.name.clone(),
                                adset_id: state
                                    .adset_id
                                    .clone()
                                    .expect("lattice guarantees the ad set"),
                                creative_id: state
                                    .creative_id
                                    .clone()
                                    .expect("lattice guarantees the creative"),
                            }),
                        };
                        self.create_paused_ad(key, request, deadline)
                            .await
                            .map(|created| created.id)
                    }
                };
                match outcome {
                    Ok(id) => {
                        state.set_output(step, id);
                        store.checkpoint(state_path, &state).map_err(|reason| {
                            Error::InvalidQuery {
                                site: site.clone(),
                                reason,
                            }
                        })?;
                    }
                    // No HTTP response arrived: Meta may or may not have
                    // created the object. The marker stays and every later
                    // mutating command refuses until a human reconciles —
                    // a conservative false positive beats a duplicate
                    // paused hierarchy.
                    Err(e @ (Error::Network { .. } | Error::DeadlineExceeded { .. })) => {
                        let _ = e; // already durably recorded in the state
                        return Ok(PausedDraftResult::ReconciliationRequired {
                            site: site.clone(),
                            account_id: state.account_id.clone(),
                            step: step.as_str(),
                            guidance: RECONCILE_GUIDANCE,
                        });
                    }
                    // Every other failure is definitive: Meta answered with
                    // an error, or the request never left (validation, auth,
                    // policy). Clearing the marker keeps the step retryable.
                    Err(e) => {
                        state.in_flight = None;
                        store.checkpoint(state_path, &state).map_err(|reason| {
                            Error::InvalidQuery {
                                site: site.clone(),
                                reason,
                            }
                        })?;
                        return Err(e);
                    }
                }
            }
            completed_result(&site, &state)
        }

        /// Read-only snapshot for `status-draft`. Takes no lock: checkpoints
        /// are atomically replaced, so a concurrent run can never expose a
        /// partial read.
        pub async fn paused_draft_status(
            &self,
            key: &AccountKey,
            store: &dyn DraftStore,
            state_path: &Path,
            deadline: Deadline,
        ) -> Result<DraftStatusReply, Error> {
            let state = store
                .read(state_path)
                .map_err(|reason| Error::InvalidQuery {
                    site: key.site.clone(),
                    reason,
                })?;
            if state.site != key.site.as_str() {
                return Err(Error::InvalidQuery {
                    site: key.site.clone(),
                    reason: "draft_state_site".into(),
                });
            }
            let mut review = Vec::new();
            let mut pending = false;
            for (id, entity) in [
                (&state.campaign_id, AdEntity::Campaign),
                (&state.adset_id, AdEntity::Adset),
                (&state.ad_id, AdEntity::Ad),
            ] {
                let Some(id) = id else {
                    continue;
                };
                let status = self
                    .ad_review_status(
                        key,
                        AdReviewStatusRequest {
                            entity,
                            id: id.clone(),
                        },
                        deadline,
                    )
                    .await?;
                pending |= status.is_pending_review();
                review.push(status);
            }
            Ok(DraftStatusReply {
                site: key.site.clone(),
                account_id: state.account_id.clone(),
                stage: state.stage.as_str(),
                image_hash: state.image_hash.clone(),
                creative_id: state.creative_id.clone(),
                in_flight: state.in_flight.map(|step| step.as_str()),
                review,
                pending,
            })
        }

        /// Poll [`Self::paused_draft_status`](Self::paused_draft_status)
        /// until no object is pending review or `deadline` expires. Expiry
        /// returns the **last observed reply** — a still-pending review is
        /// normal Meta latency and an explicit, retryable result, never a
        /// timeout error. (Regression: the loop used to start a poll with
        /// the deadline already spent, surfacing a raw `timeout` from the
        /// connector.) `poll_interval` is injected so tests are
        /// deterministic, matching `wait_for_ad_review_with_interval`.
        pub async fn paused_draft_status_wait(
            &self,
            key: &AccountKey,
            store: &dyn DraftStore,
            state_path: &Path,
            deadline: Deadline,
            poll_interval: std::time::Duration,
        ) -> Result<DraftStatusReply, Error> {
            loop {
                let reply = self
                    .paused_draft_status(key, store, state_path, deadline)
                    .await?;
                if !reply.pending {
                    return Ok(reply);
                }
                let remaining = deadline.remaining();
                if remaining.is_zero() {
                    return Ok(reply);
                }
                let delay = if poll_interval.is_zero() {
                    remaining
                } else {
                    poll_interval.min(remaining)
                };
                tokio::time::sleep(delay).await;
            }
        }

        /// Record the human-resolved outcome of an ambiguous write. The
        /// `in_flight` marker must name this exact step; a delivery object
        /// (campaign/ad set/ad) is additionally verified remotely to still
        /// be configured `PAUSED` before the ID enters the state. Image
        /// hashes and creatives have no review edge — their adoption is a
        /// recorded human decision, proven only when the next step uses them.
        pub async fn adopt_paused_draft_step(
            &self,
            key: &AccountKey,
            store: &dyn DraftStore,
            state_path: &Path,
            step: DraftStep,
            remote_id: String,
            deadline: Deadline,
        ) -> Result<PausedDraftResult, Error> {
            let _lock = store
                .try_lock(state_path)
                .map_err(|reason| Error::InvalidQuery {
                    site: key.site.clone(),
                    reason,
                })?;
            let mut state = store
                .read(state_path)
                .map_err(|reason| Error::InvalidQuery {
                    site: key.site.clone(),
                    reason,
                })?;
            if state.site != key.site.as_str() {
                return Err(Error::InvalidQuery {
                    site: key.site.clone(),
                    reason: "draft_state_site".into(),
                });
            }
            if state.in_flight != Some(step) {
                return Err(Error::InvalidQuery {
                    site: key.site.clone(),
                    reason: format!("draft_not_in_flight:{}", step.as_str()),
                });
            }
            if let Some(entity) = adoption_entity(step) {
                let status = self
                    .ad_review_status(
                        key,
                        AdReviewStatusRequest {
                            entity,
                            id: remote_id.clone(),
                        },
                        deadline,
                    )
                    .await?;
                // An ACTIVE or deleted object must never enter a paused
                // hierarchy's checkpoint, whatever Ads Manager shows.
                if status.configured_status != CONFIGURED_PAUSED {
                    return Err(Error::InvalidQuery {
                        site: key.site.clone(),
                        reason: format!("adopt_not_paused:{}", status.configured_status),
                    });
                }
            }
            state.set_output(step, remote_id);
            store
                .checkpoint(state_path, &state)
                .map_err(|reason| Error::InvalidQuery {
                    site: key.site.clone(),
                    reason,
                })?;
            if state.stage == DraftStage::Completed {
                return completed_result(&key.site, &state);
            }
            Ok(PausedDraftResult::InProgress {
                site: key.site.clone(),
                account_id: state.account_id.clone(),
                stage: state.stage.as_str(),
                remaining: state.remaining_steps().iter().map(|s| s.as_str()).collect(),
            })
        }

        fn authorize_draft_step(&self, site: &Site, step: DraftStep) -> Result<(), Error> {
            let action = match step {
                DraftStep::Image => AdsAction::UploadAdImage,
                DraftStep::Campaign => AdsAction::CreatePausedCampaign,
                DraftStep::Adset => AdsAction::CreatePausedAdset,
                DraftStep::Creative => AdsAction::CreateLinkAdCreative,
                DraftStep::Ad => AdsAction::CreatePausedAd,
            };
            self.ads_policy.authorize(site, action)
        }

        /// The image bytes are needed only while the upload step is pending;
        /// demanding them earlier would make `resume` after the upload fail
        /// on a deleted local file for no protocol reason.
        fn image_upload_for(
            &self,
            site: &Site,
            image: Option<&DraftImage>,
            account: &str,
            step: DraftStep,
        ) -> Result<Option<UploadAdImageRequest>, Error> {
            if step != DraftStep::Image {
                return Ok(None);
            }
            let Some(image) = image else {
                return Err(Error::InvalidQuery {
                    site: site.clone(),
                    reason: "image_bytes_required".into(),
                });
            };
            Ok(Some(UploadAdImageRequest {
                account: Some(account.to_string()),
                filename: image.filename.clone(),
                bytes: image.bytes.clone(),
            }))
        }
    }

    fn completed_result(site: &Site, state: &PausedDraftState) -> Result<PausedDraftResult, Error> {
        // The validated lattice guarantees all five outputs here; anything
        // else is a corrupt file that must refuse, not unwrap.
        let (Some(image_hash), Some(campaign_id), Some(adset_id), Some(creative_id), Some(ad_id)) = (
            &state.image_hash,
            &state.campaign_id,
            &state.adset_id,
            &state.creative_id,
            &state.ad_id,
        ) else {
            return Err(Error::InvalidQuery {
                site: site.clone(),
                reason: "draft_state_stage".into(),
            });
        };
        Ok(PausedDraftResult::Completed {
            site: site.clone(),
            account_id: state.account_id.clone(),
            image_hash: image_hash.clone(),
            campaign_id: campaign_id.clone(),
            adset_id: adset_id.clone(),
            creative_id: creative_id.clone(),
            ad_id: ad_id.clone(),
            configured_status: CONFIGURED_PAUSED,
        })
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
