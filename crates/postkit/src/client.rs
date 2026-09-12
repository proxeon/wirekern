#[cfg(feature = "meta-ads")]
use crate::ads::AdReviewWait;
use crate::ads::{
    AdReviewStatus, AdReviewStatusRequest, AdsActivateRequest, AdsArchiveRequest,
    AdsBidUpdateRequest, AdsBudgetUpdateRequest, AdsConfiguredStatus, AdsCreativeSwapRequest,
    AdsDeleteRequest, AdsDuplicateReply, AdsDuplicateRequest, AdsEditOutcome, AdsInspectReply,
    AdsInspectRequest, AdsInventoryKind, AdsInventoryReply, AdsInventoryRequest,
    AdsLifecycleOutcome, AdsLifetimeBudgetUpdateRequest, AdsPauseRequest,
    AdsPlacementUpdateRequest, AdsScheduleUpdateRequest, AdsStatusUpdateRequest, AdsTargetingDiff,
    AdsTargetingUpdateRequest, AdsTokenInspection, CreateLinkAdCreativeRequest,
    CreatePausedAdRequest, CreatedAd, CreatedAdCreative, CreativePreview, CreativePreviewRequest,
    MarketingApiAccessTier, UploadAdImageRequest, UploadedAdImage, ACTIVATE_RECONCILE_GUIDANCE,
    ARCHIVE_RECONCILE_GUIDANCE, DELETE_RECONCILE_GUIDANCE, EDIT_RECONCILE_GUIDANCE,
    PAUSE_RECONCILE_GUIDANCE, SYSTEM_USER_TOKEN_KIND,
};
use crate::apps::AppStore;
use crate::error::Error;
#[cfg(feature = "whatsapp-cloud")]
use crate::facets::WhatsAppSender;
use crate::facets::{AdsManager, InsightsSource, MediaReader, PageDirectory};
use crate::insights::{AdAccountsReply, InsightsJob, InsightsQuery, InsightsReply};
#[cfg(feature = "meta-ads")]
use crate::insights::{InsightsJobStatus, InsightsJobWait};
use crate::media::{MediaQuery, MediaReply};
use crate::pages::PagesReply;
use crate::policy::{AdsAction, AdsPolicy, PausedOnlyAdsPolicy};
#[cfg(feature = "whatsapp-cloud")]
use crate::policy::{NoWhatsAppSendsPolicy, WhatsAppAction, WhatsAppPolicy};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::registry::Registry;
use crate::types::{
    AccountCreds, AccountKey, AppConfig, Capability, Deadline, Intent, Outcome, Probe, Site, WhoAmI,
};
use crate::vault::{Claim, Vault};
#[cfg(feature = "whatsapp-cloud")]
use crate::whatsapp::{WhatsAppMessage, WhatsAppSendRequest};
#[cfg(feature = "whatsapp-cloud")]
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(feature = "whatsapp-cloud")]
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// One credentialed attempt. Boxed so `with_creds` can call the same
/// operation twice (first try, then once after a `token_expired` refresh)
/// on MSRV 1.80 without async closures.
type CredOp<T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send>>;

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
    #[cfg(feature = "whatsapp-cloud")]
    whatsapp_policy: Arc<dyn WhatsAppPolicy>,
    #[cfg(feature = "whatsapp-cloud")]
    whatsapp_ledger: Option<Arc<dyn crate::whatsapp_ops::WhatsAppLedger>>,
    #[cfg(feature = "whatsapp-cloud")]
    whatsapp_consent: Option<Arc<dyn crate::whatsapp_ops::WhatsAppConsent>>,
    #[cfg(feature = "whatsapp-cloud")]
    // Pacing belongs to the Client, not an individual batch. Otherwise two
    // callers can both believe they own the next process-local send slot.
    whatsapp_throughput:
        Arc<Mutex<HashMap<WhatsAppPacingKey, Arc<crate::whatsapp_ops::ThroughputQueue>>>>,
}

/// The Cloud API quota applies to a phone number, while Postkit credentials
/// are selected by an account alias. Retaining both avoids coupling two
/// independent sender aliases to one local queue just because they share a
/// token, and avoids splitting queues across different stored credentials.
#[cfg(feature = "whatsapp-cloud")]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct WhatsAppPacingKey {
    account: AccountKey,
    phone_number_id: String,
}

impl Client {
    pub fn new(registry: Registry, vault: Arc<dyn Vault>, apps: Arc<dyn AppStore>) -> Self {
        Self {
            registry,
            vault,
            apps,
            ads_policy: Arc::new(PausedOnlyAdsPolicy),
            #[cfg(feature = "whatsapp-cloud")]
            whatsapp_policy: Arc::new(NoWhatsAppSendsPolicy),
            #[cfg(feature = "whatsapp-cloud")]
            whatsapp_ledger: None,
            #[cfg(feature = "whatsapp-cloud")]
            whatsapp_consent: None,
            #[cfg(feature = "whatsapp-cloud")]
            whatsapp_throughput: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Replace only the advertising policy. Chain with
    /// [`Self::with_whatsapp_policy`] — the two domains are independent and
    /// must not reset each other.
    pub fn with_ads_policy(mut self, ads_policy: Arc<dyn AdsPolicy>) -> Self {
        self.ads_policy = ads_policy;
        self
    }

    /// Replace only the WhatsApp send policy. Ads stay whatever they were
    /// (paused-only by default).
    #[cfg(feature = "whatsapp-cloud")]
    pub fn with_whatsapp_policy(mut self, whatsapp_policy: Arc<dyn WhatsAppPolicy>) -> Self {
        self.whatsapp_policy = whatsapp_policy;
        self
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn with_whatsapp_ledger(
        mut self,
        ledger: Arc<dyn crate::whatsapp_ops::WhatsAppLedger>,
    ) -> Self {
        self.whatsapp_ledger = Some(ledger);
        self
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn with_whatsapp_consent(
        mut self,
        consent: Arc<dyn crate::whatsapp_ops::WhatsAppConsent>,
    ) -> Self {
        self.whatsapp_consent = Some(consent);
        self
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
            // Re-check the ledger after winning the claim. The first check
            // and the claim are not one atomic step: a concurrent holder
            // can record its outcome and release between them, and this
            // caller would then claim Free against an already-published
            // key. Because the holder records *before* releasing, any
            // claim we win here happens after that record — one recheck
            // closes the last interleaving.
            if let Some(out) = self.vault.get_outcome(key, idem)? {
                self.release_claim(key, Some(idem));
                return Ok(out);
            }
        }
        // One confined attempt so the claim has exactly one release point:
        // every early `?` inside publish_once lands here, not in the caller.
        let attempt = self.publish_once(publisher, key, intent, deadline).await;
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

    /// Send one private WhatsApp Cloud message through the separate messaging
    /// contract. The normal policy denies it before vault access; callers
    /// must explicitly install an allowing `WhatsAppPolicy`. A `wamid` means
    /// Meta accepted the request, not that the recipient received it — status
    /// webhooks provide the final delivery state.
    #[cfg(feature = "whatsapp-cloud")]
    pub async fn send_whatsapp(
        &self,
        key: &AccountKey,
        request: WhatsAppSendRequest,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        self.send_whatsapp_from(key, None, request, deadline).await
    }

    /// Send through the primary configured phone or an explicitly configured
    /// sender alias. The alias is resolved locally before Graph is called;
    /// callers cannot use this method to target an arbitrary phone ID.
    ///
    /// Idempotency and pacing are namespaced by the resolved phone ID. Reusing
    /// an idempotency key on two different senders therefore cannot replay a
    /// delivery result from the wrong business number.
    #[cfg(feature = "whatsapp-cloud")]
    pub async fn send_whatsapp_from(
        &self,
        key: &AccountKey,
        sender_alias: Option<&str>,
        request: WhatsAppSendRequest,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        request.validate().map_err(|reason| Error::InvalidPost {
            site: key.site.clone(),
            reason,
            limit: None,
        })?;
        let action = match &request.message {
            WhatsAppMessage::Reply { .. } => WhatsAppAction::SendReply,
            WhatsAppMessage::Text { .. } => WhatsAppAction::SendText,
            WhatsAppMessage::Template { .. } => WhatsAppAction::SendTemplate,
            WhatsAppMessage::Image { .. }
            | WhatsAppMessage::Document { .. }
            | WhatsAppMessage::Audio { .. }
            | WhatsAppMessage::Video { .. }
            | WhatsAppMessage::Sticker { .. } => WhatsAppAction::SendMedia,
            WhatsAppMessage::Buttons { .. }
            | WhatsAppMessage::List { .. }
            | WhatsAppMessage::CtaUrl { .. }
            | WhatsAppMessage::LocationRequest { .. }
            | WhatsAppMessage::VoiceCall { .. }
            | WhatsAppMessage::AddressRequest { .. } => WhatsAppAction::SendInteractive,
            WhatsAppMessage::Location { .. } => WhatsAppAction::SendLocation,
            WhatsAppMessage::Contacts { .. } => WhatsAppAction::SendContacts,
            WhatsAppMessage::Reaction { .. } => WhatsAppAction::SendReaction,
            WhatsAppMessage::MarkRead { .. } => WhatsAppAction::MarkRead,
            WhatsAppMessage::Typing { .. } => WhatsAppAction::SendTyping,
            WhatsAppMessage::Catalog { .. }
            | WhatsAppMessage::Product { .. }
            | WhatsAppMessage::ProductList { .. }
            | WhatsAppMessage::OrderStatus { .. } => WhatsAppAction::SendCatalog,
            WhatsAppMessage::Flow { .. } => WhatsAppAction::SendFlow,
        };
        // Do this before registry/vault lookup. A denied send must reveal
        // neither whether an account is configured nor a bearer token to the
        // connector's HTTP path.
        self.whatsapp_policy.authorize(&key.site, action)?;
        let publisher = self.publisher(&key.site)?;
        let need = request.required_capability();
        if !publisher.capabilities().contains(&need) {
            return Err(Error::UnsupportedCapability {
                site: key.site.clone(),
                need,
            });
        }

        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let (sender_app, phone_number_id) =
            resolve_whatsapp_outbound_sender(&app, sender_alias, &key.site)?;

        // WhatsApp sends require a key, so this follows the same atomic
        // claim/record discipline as public publishing. Only a confirmed
        // response is remembered; an unknown post-send failure remains
        // intentionally ambiguous and must be reconciled via webhook/status.
        let idem = scoped_whatsapp_idempotency(&phone_number_id, &request.idempotency_key);
        if let Some(out) = self.vault.get_outcome(key, &idem)? {
            return Ok(out);
        }
        match self.vault.claim_outcome(key, &idem)? {
            Claim::Free => {}
            Claim::Taken => {
                return Err(Error::IdempotencyInFlight {
                    site: key.site.clone(),
                    key: request.idempotency_key.clone(),
                })
            }
        }
        let after_claim = match self.vault.get_outcome(key, &idem) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.release_claim(key, Some(&idem));
                return Err(error);
            }
        };
        if let Some(out) = after_claim {
            self.release_claim(key, Some(&idem));
            return Ok(out);
        }
        // Keep credential lookup inside the same guarded attempt as the HTTP
        // call. A missing/corrupt vault entry must release the claim just like
        // a rejected platform request, otherwise a later corrected command
        // would be blocked behind an abandoned idempotency key.
        let sender = self.whatsapp_sender(&key.site, need)?;
        let attempt = async {
            // Every Cloud API message, including a one-off send, shares this
            // sender's local pacing queue. A batch is only a loop over this
            // operation, so its second item waits instead of being rejected
            // with a synthetic local rate-limit error.
            self.wait_for_whatsapp_slot(key, &phone_number_id, deadline)
                .await?;
            let creds = self.vault.get(key)?;
            sender
                .send_whatsapp(&sender_app, &creds, &request, deadline)
                .await
        }
        .await;
        let out = match attempt {
            Ok(out) => out,
            Err(error) => {
                self.release_claim(key, Some(&idem));
                return Err(error);
            }
        };
        let recorded = self.vault.put_outcome(key, &idem, &out);
        self.release_claim(key, Some(&idem));
        recorded?;
        Ok(out)
    }

    /// Verify and parse a forwarded Cloud API webhook. This is not a listener:
    /// the caller supplies the exact raw body and `X-Hub-Signature-256`.
    #[cfg(feature = "whatsapp-cloud")]
    pub fn parse_whatsapp_webhook(
        &self,
        signature: &str,
        raw_body: &[u8],
        options: crate::whatsapp::WebhookParseOptions,
    ) -> Result<crate::whatsapp::InboundMessages, Error> {
        let app = self.apps.get(&Site::new("whatsapp_cloud"))?;
        crate::connectors::whatsapp_cloud::WhatsAppCloud::parse_signed_webhook_with(
            &app, signature, raw_body, options,
        )
    }

    /// Meta GET handshake. Echoes `hub.challenge` when the verify token matches.
    #[cfg(feature = "whatsapp-cloud")]
    pub fn verify_whatsapp_callback_challenge(
        &self,
        mode: &str,
        token: &str,
        challenge: &str,
    ) -> Result<String, Error> {
        let app = self.apps.get(&Site::new("whatsapp_cloud"))?;
        crate::connectors::whatsapp_cloud::WhatsAppCloud::verify_callback_challenge(
            &app, mode, token, challenge,
        )
    }

    /// Parse a signed webhook and, if a ledger is attached, persist wamids.
    /// HMAC failures stay errors so Meta can retry; parse failures after a
    /// valid signature should be ACK'd by the host and recorded as dead letters.
    #[cfg(feature = "whatsapp-cloud")]
    pub fn ingest_whatsapp_webhook(
        &self,
        signature: &str,
        raw_body: &[u8],
        options: crate::whatsapp::WebhookParseOptions,
    ) -> Result<crate::whatsapp::InboundMessages, Error> {
        match self.parse_whatsapp_webhook(signature, raw_body, options) {
            Ok(parsed) => {
                if let Some(ledger) = &self.whatsapp_ledger {
                    crate::whatsapp_ops::ingest_parsed(ledger.as_ref(), &parsed)?;
                }
                Ok(parsed)
            }
            Err(error) => {
                // Unsigned junk must not fill the dead-letter log. Only
                // payloads that passed HMAC (or failed later) are recorded.
                let hmac_fail = matches!(
                    &error,
                    Error::InvalidQuery { reason, .. }
                        if reason == "webhook_signature_invalid"
                            || reason == "missing_webhook_app_secret"
                );
                if !hmac_fail {
                    if let Some(ledger) = &self.whatsapp_ledger {
                        let sha = crate::whatsapp_ops::sha256_hex(raw_body);
                        let reason = match &error {
                            Error::InvalidQuery { reason, .. } => reason.as_str(),
                            _ => "webhook_ingest_failed",
                        };
                        let _ = ledger.put_dead_letter(reason, &sha);
                    }
                }
                Err(error)
            }
        }
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn whatsapp_ledger_get(
        &self,
        wamid: &str,
    ) -> Result<Option<crate::whatsapp_ops::WhatsAppLedgerRecord>, Error> {
        match &self.whatsapp_ledger {
            Some(ledger) => ledger.get(wamid),
            None => Err(Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "whatsapp_ledger_disabled".into(),
            }),
        }
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn whatsapp_window_open(&self, wa_id: &str) -> Result<bool, Error> {
        let Some(ledger) = &self.whatsapp_ledger else {
            return Err(Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "whatsapp_ledger_disabled".into(),
            });
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(ledger
            .last_inbound_at(wa_id)?
            .is_some_and(|at| crate::whatsapp_ops::customer_window_open(at, now)))
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn put_whatsapp_consent(
        &self,
        record: crate::whatsapp_ops::ConsentRecord,
    ) -> Result<(), Error> {
        // This is an operator-owned audit signal, not a surrogate for Meta's
        // consent/window decision. `AllowWhatsAppSendsPolicy` stays explicit
        // so incomplete local callback history cannot become a false deny.
        match &self.whatsapp_consent {
            Some(store) => store.put(&record),
            None => Err(Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "whatsapp_consent_disabled".into(),
            }),
        }
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn get_whatsapp_consent(
        &self,
        wa_id: &str,
    ) -> Result<Option<crate::whatsapp_ops::ConsentRecord>, Error> {
        match &self.whatsapp_consent {
            Some(store) => store.get(wa_id),
            None => Err(Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "whatsapp_consent_disabled".into(),
            }),
        }
    }

    /// Remove local delivery records older than `before_unix`.
    ///
    /// This deliberately affects the delivery ledger only. Consent records
    /// can have a separate legal-retention basis, so expiring message history
    /// must never silently erase an explicit opt-out.
    #[cfg(feature = "whatsapp-cloud")]
    pub fn purge_whatsapp_ledger_before(&self, before_unix: u64) -> Result<usize, Error> {
        match &self.whatsapp_ledger {
            Some(ledger) => ledger.purge_before(before_unix),
            None => Err(Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "whatsapp_ledger_disabled".into(),
            }),
        }
    }

    /// Upload bytes to Cloud API media. Not a customer send: no `--allow-send`.
    #[cfg(feature = "whatsapp-cloud")]
    pub async fn upload_whatsapp_media(
        &self,
        key: &AccountKey,
        upload: crate::whatsapp::WhatsAppMediaUpload,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppUploadedMedia, Error> {
        self.require_capability(&key.site, Capability::ManageWhatsAppMedia)?;
        let assets = self.whatsapp_assets(&key.site, Capability::ManageWhatsAppMedia)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        assets.upload_media(&app, &creds, &upload, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn whatsapp_media_metadata(
        &self,
        key: &AccountKey,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppMediaMeta, Error> {
        self.require_capability(&key.site, Capability::ReadWhatsAppMedia)?;
        let assets = self.whatsapp_assets(&key.site, Capability::ReadWhatsAppMedia)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        assets
            .media_metadata(&app, &creds, media_id, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn download_whatsapp_media(
        &self,
        key: &AccountKey,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<Vec<u8>, Error> {
        self.require_capability(&key.site, Capability::ReadWhatsAppMedia)?;
        let assets = self.whatsapp_assets(&key.site, Capability::ReadWhatsAppMedia)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        assets
            .download_media(&app, &creds, media_id, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn delete_whatsapp_media(
        &self,
        key: &AccountKey,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        self.require_capability(&key.site, Capability::ManageWhatsAppMedia)?;
        let assets = self.whatsapp_assets(&key.site, Capability::ManageWhatsAppMedia)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        assets.delete_media(&app, &creds, media_id, deadline).await
    }

    /// List WABA templates (id/name/status/quality). Not a customer send.
    #[cfg(feature = "whatsapp-cloud")]
    pub async fn list_whatsapp_templates(
        &self,
        key: &AccountKey,
        query: crate::whatsapp::WhatsAppTemplateQuery,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppTemplateList, Error> {
        self.require_capability(&key.site, Capability::ReadTemplates)?;
        let templates = self.whatsapp_templates(&key.site, Capability::ReadTemplates)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        templates
            .list_templates(&app, &creds, &query, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn get_whatsapp_template(
        &self,
        key: &AccountKey,
        template_id: &str,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppTemplateRecord, Error> {
        self.require_capability(&key.site, Capability::ReadTemplates)?;
        let templates = self.whatsapp_templates(&key.site, Capability::ReadTemplates)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        templates
            .get_template(&app, &creds, template_id, deadline)
            .await
    }

    /// Create (and auto-submit for review) a typed template. Not a customer
    /// send: no `--allow-send`, but still requires `manage.templates`.
    #[cfg(feature = "whatsapp-cloud")]
    pub async fn create_whatsapp_template(
        &self,
        key: &AccountKey,
        draft: crate::whatsapp::WhatsAppTemplateDraft,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppTemplateRecord, Error> {
        self.require_capability(&key.site, Capability::ManageTemplates)?;
        let templates = self.whatsapp_templates(&key.site, Capability::ManageTemplates)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        templates
            .create_template(&app, &creds, &draft, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn edit_whatsapp_template(
        &self,
        key: &AccountKey,
        template_id: &str,
        draft: crate::whatsapp::WhatsAppTemplateDraft,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppTemplateRecord, Error> {
        self.require_capability(&key.site, Capability::ManageTemplates)?;
        let templates = self.whatsapp_templates(&key.site, Capability::ManageTemplates)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        templates
            .edit_template(&app, &creds, template_id, &draft, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn delete_whatsapp_template(
        &self,
        key: &AccountKey,
        name: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        self.require_capability(&key.site, Capability::ManageTemplates)?;
        let templates = self.whatsapp_templates(&key.site, Capability::ManageTemplates)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        templates
            .delete_template(&app, &creds, name, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn list_whatsapp_flows(
        &self,
        key: &AccountKey,
        query: crate::whatsapp::WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppFlowList, Error> {
        self.require_capability(&key.site, Capability::ReadFlows)?;
        let flows = self.whatsapp_flows(&key.site, Capability::ReadFlows)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        flows.list_flows(&app, &creds, &query, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn get_whatsapp_flow(
        &self,
        key: &AccountKey,
        flow_id: &str,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppFlowRecord, Error> {
        self.require_capability(&key.site, Capability::ReadFlows)?;
        let flows = self.whatsapp_flows(&key.site, Capability::ReadFlows)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        flows.get_flow(&app, &creds, flow_id, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn create_whatsapp_flow(
        &self,
        key: &AccountKey,
        draft: crate::whatsapp::WhatsAppFlowDraft,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppFlowRecord, Error> {
        self.require_capability(&key.site, Capability::ManageFlows)?;
        let flows = self.whatsapp_flows(&key.site, Capability::ManageFlows)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        flows.create_flow(&app, &creds, &draft, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn publish_whatsapp_flow(
        &self,
        key: &AccountKey,
        flow_id: &str,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppFlowRecord, Error> {
        self.require_capability(&key.site, Capability::ManageFlows)?;
        let flows = self.whatsapp_flows(&key.site, Capability::ManageFlows)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        flows.publish_flow(&app, &creds, flow_id, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn list_whatsapp_wabas(
        &self,
        key: &AccountKey,
        query: crate::whatsapp::WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppWabaList, Error> {
        self.require_capability(&key.site, Capability::ReadWhatsAppAccount)?;
        let account = self.whatsapp_account(&key.site, Capability::ReadWhatsAppAccount)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account.list_wabas(&app, &creds, &query, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn list_whatsapp_phone_numbers(
        &self,
        key: &AccountKey,
        query: crate::whatsapp::WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppPhoneNumberList, Error> {
        self.require_capability(&key.site, Capability::ReadWhatsAppAccount)?;
        let account = self.whatsapp_account(&key.site, Capability::ReadWhatsAppAccount)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account
            .list_phone_numbers(&app, &creds, &query, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn whatsapp_phone_health(
        &self,
        key: &AccountKey,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppPhoneNumber, Error> {
        self.require_capability(&key.site, Capability::ReadWhatsAppAccount)?;
        let account = self.whatsapp_account(&key.site, Capability::ReadWhatsAppAccount)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account.phone_health(&app, &creds, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn subscribe_whatsapp_apps(
        &self,
        key: &AccountKey,
        deadline: Deadline,
    ) -> Result<(), Error> {
        self.require_capability(&key.site, Capability::ManageWhatsAppPhone)?;
        let account = self.whatsapp_account(&key.site, Capability::ManageWhatsAppPhone)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account.subscribe_apps(&app, &creds, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn register_whatsapp_phone(
        &self,
        key: &AccountKey,
        pin: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        self.whatsapp_policy
            .authorize(&key.site, WhatsAppAction::ManagePhone)?;
        self.require_capability(&key.site, Capability::ManageWhatsAppPhone)?;
        let account = self.whatsapp_account(&key.site, Capability::ManageWhatsAppPhone)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account.register_phone(&app, &creds, pin, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn set_whatsapp_two_step_pin(
        &self,
        key: &AccountKey,
        pin: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        self.whatsapp_policy
            .authorize(&key.site, WhatsAppAction::ManagePhone)?;
        self.require_capability(&key.site, Capability::ManageWhatsAppPhone)?;
        let account = self.whatsapp_account(&key.site, Capability::ManageWhatsAppPhone)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account.set_two_step_pin(&app, &creds, pin, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn list_whatsapp_system_users(
        &self,
        key: &AccountKey,
        query: crate::whatsapp::WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppSystemUserList, Error> {
        self.require_capability(&key.site, Capability::ReadWhatsAppAccount)?;
        let account = self.whatsapp_account(&key.site, Capability::ReadWhatsAppAccount)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account
            .list_system_users(&app, &creds, &query, deadline)
            .await
    }

    /// Bounded fan-out, not a campaign tool. More than 10 messages is refused.
    /// Every item uses the shared, deadline-aware pacing path from
    /// [`Self::send_whatsapp`], never a batch-local rate-limit shortcut.
    #[cfg(feature = "whatsapp-cloud")]
    pub async fn send_whatsapp_many(
        &self,
        key: &AccountKey,
        requests: Vec<WhatsAppSendRequest>,
        deadline: Deadline,
    ) -> Result<Vec<Outcome>, Error> {
        self.send_whatsapp_many_from(key, None, requests, deadline)
            .await
    }

    /// Bounded multi-send through one configured sender alias. Keep one alias
    /// for the whole batch so its pacing and idempotency scope are obvious to
    /// the operator; mixed-sender fan-out needs a separate reviewed contract.
    #[cfg(feature = "whatsapp-cloud")]
    pub async fn send_whatsapp_many_from(
        &self,
        key: &AccountKey,
        sender_alias: Option<&str>,
        requests: Vec<WhatsAppSendRequest>,
        deadline: Deadline,
    ) -> Result<Vec<Outcome>, Error> {
        if requests.len() > 10 {
            return Err(Error::InvalidPost {
                site: key.site.clone(),
                reason: "whatsapp_batch_too_large".into(),
                limit: Some(10),
            });
        }
        let mut out = Vec::new();
        for request in requests {
            out.push(
                self.send_whatsapp_from(key, sender_alias, request, deadline)
                    .await?,
            );
        }
        Ok(out)
    }

    #[cfg(feature = "whatsapp-cloud")]
    fn whatsapp_throughput_for(
        &self,
        key: &AccountKey,
        phone_number_id: &str,
    ) -> Arc<crate::whatsapp_ops::ThroughputQueue> {
        let mut queues = self
            .whatsapp_throughput
            .lock()
            .expect("whatsapp throughput");
        queues
            .entry(WhatsAppPacingKey {
                account: key.clone(),
                phone_number_id: phone_number_id.to_string(),
            })
            .or_insert_with(|| Arc::new(crate::whatsapp_ops::ThroughputQueue::default_cloud_api()))
            .clone()
    }

    /// Wait for a process-local slot without exceeding the caller's existing
    /// deadline. The queue returns a duration instead of sleeping itself so
    /// this async client never blocks a Tokio worker.
    #[cfg(feature = "whatsapp-cloud")]
    async fn wait_for_whatsapp_slot(
        &self,
        key: &AccountKey,
        phone_number_id: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        let queue = self.whatsapp_throughput_for(key, phone_number_id);
        loop {
            deadline.check(&key.site)?;
            let now_ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let wait = std::time::Duration::from_nanos(queue.wait_ns(now_ns));
            if wait.is_zero() {
                return Ok(());
            }
            // Do not begin a sleep that cannot finish before the request
            // deadline. A caller gets the normal timeout, not a misleading
            // local RateLimited result caused by another batch item.
            if wait >= deadline.remaining() {
                return Err(Error::DeadlineExceeded {
                    site: key.site.clone(),
                });
            }
            tokio::time::sleep(wait).await;
        }
    }

    /// Shared load + optional proactive refresh. Every network verb that
    /// talks with stored OAuth creds goes through here so a missed retry
    /// cannot land on only one of insights/pages/ads.
    async fn prepare_creds(
        &self,
        key: &AccountKey,
        deadline: Deadline,
        proactive: bool,
    ) -> Result<(Arc<dyn Publisher>, AppConfig, AccountCreds), Error> {
        let publisher = self.publisher(&key.site)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut creds = self.vault.get(key)?;
        if proactive {
            creds = self
                .maybe_refresh(&*publisher, &app, key, creds, deadline)
                .await?;
        }
        Ok((publisher, app, creds))
    }

    /// One reactive refresh. `token_expired` is the only error that retries;
    /// every other error is returned as-is so callers cannot accidentally
    /// retry a visible write.
    async fn recover_expired(
        &self,
        publisher: &dyn Publisher,
        app: &AppConfig,
        key: &AccountKey,
        creds: AccountCreds,
        deadline: Deadline,
        err: Error,
    ) -> Result<AccountCreds, Error> {
        match err {
            Error::Auth { reason, .. } if reason == "token_expired" => {
                let new = publisher.refresh(app, &creds, deadline).await?;
                self.vault.put(key, &new)?;
                Ok(new)
            }
            other => Err(other),
        }
    }

    /// Proactive refresh, then the operation, then at most one
    /// `token_expired` → refresh → retry. The next read/write verb must
    /// call this instead of copying the match.
    async fn with_creds<T, F>(
        &self,
        key: &AccountKey,
        deadline: Deadline,
        op: F,
    ) -> Result<T, Error>
    where
        F: Fn(AppConfig, AccountCreds) -> CredOp<T>,
    {
        let (publisher, app, creds) = self.prepare_creds(key, deadline, true).await?;
        match op(app.clone(), creds.clone()).await {
            Err(e) => {
                let creds = self
                    .recover_expired(&*publisher, &app, key, creds, deadline, e)
                    .await?;
                op(app, creds).await
            }
            other => other,
        }
    }

    /// The claim-guarded publish: credential session plus the one reactive
    /// token-expiry retry. Extracted from `publish` so the idempotency claim
    /// can bracket it with a single release point.
    async fn publish_once(
        &self,
        publisher: Arc<dyn Publisher>,
        key: &AccountKey,
        intent: Intent,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        self.with_creds(key, deadline, move |app, creds| {
            let publisher = publisher.clone();
            let intent = intent.clone();
            Box::pin(async move { publisher.publish(&app, &creds, intent, deadline).await })
        })
        .await
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
        query.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadMetrics)?;
        let source = self.insights_source(&key.site, Capability::ReadMetrics)?;
        self.with_creds(key, deadline, move |app, creds| {
            let source = source.clone();
            let query = query.clone();
            Box::pin(async move { source.insights(&app, &creds, &query, deadline).await })
        })
        .await
    }

    pub async fn start_insights_job(
        &self,
        key: &AccountKey,
        query: InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        query.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadMetrics)?;
        let source = self.insights_source(&key.site, Capability::ReadMetrics)?;
        self.with_creds(key, deadline, move |app, creds| {
            let source = source.clone();
            let query = query.clone();
            Box::pin(async move {
                source
                    .start_insights_job(&app, &creds, &query, deadline)
                    .await
            })
        })
        .await
    }

    pub async fn insights_job(
        &self,
        key: &AccountKey,
        job_id: &str,
        deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        self.require_capability(&key.site, Capability::ReadMetrics)?;
        let source = self.insights_source(&key.site, Capability::ReadMetrics)?;
        let job_id = job_id.to_string();
        self.with_creds(key, deadline, move |app, creds| {
            let source = source.clone();
            let job_id = job_id.clone();
            Box::pin(async move { source.insights_job(&app, &creds, &job_id, deadline).await })
        })
        .await
    }

    pub async fn insights_job_result(
        &self,
        key: &AccountKey,
        job_id: &str,
        query: InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
        query.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadMetrics)?;
        let source = self.insights_source(&key.site, Capability::ReadMetrics)?;
        let job_id = job_id.to_string();
        self.with_creds(key, deadline, move |app, creds| {
            let source = source.clone();
            let job_id = job_id.clone();
            let query = query.clone();
            Box::pin(async move {
                source
                    .insights_job_result(&app, &creds, &job_id, &query, deadline)
                    .await
            })
        })
        .await
    }

    pub async fn cancel_insights_job(
        &self,
        key: &AccountKey,
        job_id: &str,
        deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        self.require_capability(&key.site, Capability::ReadMetrics)?;
        let source = self.insights_source(&key.site, Capability::ReadMetrics)?;
        let job_id = job_id.to_string();
        self.with_creds(key, deadline, move |app, creds| {
            let source = source.clone();
            let job_id = job_id.clone();
            Box::pin(async move {
                source
                    .cancel_insights_job(&app, &creds, &job_id, deadline)
                    .await
            })
        })
        .await
    }

    /// Poll until the job is terminal or `deadline` fires. Pending is a
    /// successful document, not a timeout error. Same 2s cadence as ads review.
    #[cfg(feature = "meta-ads")]
    pub async fn wait_for_insights_job(
        &self,
        key: &AccountKey,
        job_id: &str,
        query: InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsJobWait, Error> {
        self.wait_for_insights_job_with_interval(key, job_id, query, deadline, REVIEW_POLL_INTERVAL)
            .await
    }

    #[cfg(feature = "meta-ads")]
    pub(crate) async fn wait_for_insights_job_with_interval(
        &self,
        key: &AccountKey,
        job_id: &str,
        query: InsightsQuery,
        deadline: Deadline,
        poll_interval: std::time::Duration,
    ) -> Result<InsightsJobWait, Error> {
        loop {
            let job = self.insights_job(key, job_id, deadline).await?;
            // Meta: fetch results only when async_status is Job Completed and
            // async_percent_completion is 100.
            if job.status == InsightsJobStatus::Completed && job.percent_complete == 100 {
                let reply = self
                    .insights_job_result(key, job_id, query, deadline)
                    .await?;
                return Ok(InsightsJobWait::Completed(reply));
            }
            if matches!(
                job.status,
                InsightsJobStatus::Failed | InsightsJobStatus::Skipped
            ) {
                return Ok(InsightsJobWait::Failed(job));
            }
            let remaining = deadline.remaining();
            if remaining.is_zero() {
                return Ok(InsightsJobWait::Pending(job));
            }
            let delay = if poll_interval.is_zero() {
                remaining
            } else {
                poll_interval.min(remaining)
            };
            tokio::time::sleep(delay).await;
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
        self.require_capability(&key.site, Capability::ReadAdAccounts)?;
        let source = self.insights_source(&key.site, Capability::ReadAdAccounts)?;
        self.with_creds(key, deadline, move |app, creds| {
            let source = source.clone();
            Box::pin(async move { source.ad_accounts(&app, &creds, deadline).await })
        })
        .await
    }

    /// Discover remote Pages visible to this credential. This follows the
    /// read-only account-discovery shape: capability before vault access,
    /// bounded refresh, then one retry only for a confirmed expired token.
    pub async fn pages(&self, key: &AccountKey, deadline: Deadline) -> Result<PagesReply, Error> {
        self.require_capability(&key.site, Capability::ReadPages)?;
        let directory = self.page_directory(&key.site)?;
        self.with_creds(key, deadline, move |app, creds| {
            let directory = directory.clone();
            Box::pin(async move { directory.pages(&app, &creds, deadline).await })
        })
        .await
    }

    /// Read one intentionally bounded page of published media. Like every
    /// remote read, this validates before credentials are loaded, then uses
    /// the standard one-refresh retry only for a confirmed expired token.
    pub async fn media(
        &self,
        key: &AccountKey,
        query: MediaQuery,
        deadline: Deadline,
    ) -> Result<MediaReply, Error> {
        query.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadMedia)?;
        let reader = self.media_reader(&key.site)?;
        self.with_creds(key, deadline, move |app, creds| {
            let reader = reader.clone();
            Box::pin(async move { reader.media(&app, &creds, &query, deadline).await })
        })
        .await
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
        self.require_capability(&key.site, Capability::CreatePausedAds)?;
        let ads = self.ads_manager(&key.site, Capability::CreatePausedAds)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move { ads.create_paused_ad(&app, &creds, &request, deadline).await })
        })
        .await
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
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move { ads.upload_ad_image(&app, &creds, &request, deadline).await })
        })
        .await
    }

    /// Upload a video after local validation and policy approval. Encoding
    /// is a later status poll; this write only stores the asset.
    pub async fn upload_ad_video(
        &self,
        key: &AccountKey,
        request: crate::ads::UploadAdVideoRequest,
        deadline: Deadline,
    ) -> Result<crate::ads::UploadedAdVideo, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UploadAdVideo)?;
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move { ads.upload_ad_video(&app, &creds, &request, deadline).await })
        })
        .await
    }

    /// One GET of Meta `status.video_status`. Encoding is not delivery.
    pub async fn ad_video_status(
        &self,
        key: &AccountKey,
        request: crate::ads::AdVideoStatusRequest,
        deadline: Deadline,
    ) -> Result<crate::ads::AdVideoStatus, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move { ads.ad_video_status(&app, &creds, &request, deadline).await })
        })
        .await
    }

    /// Poll every 2s until ready/error or the deadline. Pending is success.
    #[cfg(feature = "meta-ads")]
    pub async fn wait_for_ad_video(
        &self,
        key: &AccountKey,
        request: crate::ads::AdVideoStatusRequest,
        deadline: Deadline,
    ) -> Result<crate::ads::AdVideoWait, Error> {
        self.wait_for_ad_video_with_interval(key, request, deadline, REVIEW_POLL_INTERVAL)
            .await
    }

    #[cfg(feature = "meta-ads")]
    pub(crate) async fn wait_for_ad_video_with_interval(
        &self,
        key: &AccountKey,
        request: crate::ads::AdVideoStatusRequest,
        deadline: Deadline,
        poll_interval: std::time::Duration,
    ) -> Result<crate::ads::AdVideoWait, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        let (publisher, app, mut creds) = self.prepare_creds(key, deadline, true).await?;
        let mut retried_expired_token = false;
        loop {
            let status = match ads.ad_video_status(&app, &creds, &request, deadline).await {
                Err(e) if !retried_expired_token => {
                    creds = self
                        .recover_expired(&*publisher, &app, key, creds, deadline, e)
                        .await?;
                    retried_expired_token = true;
                    continue;
                }
                other => other,
            }?;
            match status.video_status {
                crate::ads::AdVideoStatusKind::Ready => {
                    return Ok(crate::ads::AdVideoWait::Ready(status));
                }
                crate::ads::AdVideoStatusKind::Error => {
                    return Ok(crate::ads::AdVideoWait::Error(status));
                }
                _ => {}
            }
            let remaining = deadline.remaining();
            if remaining.is_zero() {
                return Ok(crate::ads::AdVideoWait::Pending(status));
            }
            let delay = if poll_interval.is_zero() {
                remaining
            } else {
                poll_interval.min(remaining)
            };
            tokio::time::sleep(delay).await;
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
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                ads.create_link_ad_creative(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
    }

    /// Page-backed video creative. The video must already exist; this does
    /// not wait for encoding.
    pub async fn create_video_ad_creative(
        &self,
        key: &AccountKey,
        request: crate::ads::CreateVideoAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::CreateLinkAdCreative)?;
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                ads.create_video_ad_creative(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
    }

    pub async fn create_ad_creative(
        &self,
        key: &AccountKey,
        request: crate::ads::CreateAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::CreateLinkAdCreative)?;
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                ads.create_ad_creative(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
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
        self.require_capability(&key.site, Capability::ReadAdPreviews)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdPreviews)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                ads.preview_ad_creative(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
    }

    /// List one kind of advertising object in a selected account. GET-only
    /// and outside `AdsPolicy`: paging through paused drafts cannot activate
    /// them or change a budget.
    pub async fn list_ads_inventory(
        &self,
        key: &AccountKey,
        request: AdsInventoryRequest,
        deadline: Deadline,
    ) -> Result<AdsInventoryReply, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadAdsInventory)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdsInventory)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                ads.list_ads_inventory(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
    }

    /// Read budget, bid, targeting, Page, and destination on one known
    /// object. GET-only and outside `AdsPolicy`: this is the pre-activate
    /// confirmation surface, not an edit.
    pub async fn inspect_ads_object(
        &self,
        key: &AccountKey,
        request: AdsInspectRequest,
        deadline: Deadline,
    ) -> Result<AdsInspectReply, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadAdsInventory)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdsInventory)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                ads.inspect_ads_object(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
    }

    /// Confirmed PAUSED → ACTIVE. Policy, confirmation, and review preflight
    /// all run before the vault. A network/deadline failure after the POST
    /// leaves is `reconciliation_required`, never a second activate.
    pub async fn activate_ad(
        &self,
        key: &AccountKey,
        request: AdsActivateRequest,
        deadline: Deadline,
    ) -> Result<AdsLifecycleOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy.authorize(&key.site, AdsAction::Activate)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(
                async move { activate_ad_inner(&*ads, &app, &creds, &request, deadline).await },
            )
        })
        .await
    }

    /// Emergency `ACTIVE` → `PAUSED`. Allowed by the default policy because
    /// it cannot start spend. Already-paused is idempotent; archived/deleted
    /// objects refuse rather than guessing.
    pub async fn pause_ad(
        &self,
        key: &AccountKey,
        request: AdsPauseRequest,
        deadline: Deadline,
    ) -> Result<AdsLifecycleOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy.authorize(&key.site, AdsAction::Pause)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move { pause_ad_inner(&*ads, &app, &creds, &request, deadline).await })
        })
        .await
    }

    /// Archive a known object. Default policy denies; `--confirm-id` must
    /// match. Deleted objects cannot be archived.
    pub async fn archive_ad(
        &self,
        key: &AccountKey,
        request: AdsArchiveRequest,
        deadline: Deadline,
    ) -> Result<AdsLifecycleOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy.authorize(&key.site, AdsAction::Archive)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                confirmed_status_inner(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    AdsConfiguredStatus::Archived,
                    &["DELETED"],
                    ARCHIVE_RECONCILE_GUIDANCE,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    /// Delete a known object. Irreversible to live. Default policy denies.
    pub async fn delete_ad(
        &self,
        key: &AccountKey,
        request: AdsDeleteRequest,
        deadline: Deadline,
    ) -> Result<AdsLifecycleOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy.authorize(&key.site, AdsAction::Delete)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                confirmed_status_inner(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    AdsConfiguredStatus::Deleted,
                    &[],
                    DELETE_RECONCILE_GUIDANCE,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    /// Copy an object as PAUSED. Default policy denies. Never inherits ACTIVE.
    pub async fn duplicate_ad(
        &self,
        key: &AccountKey,
        request: AdsDuplicateRequest,
        deadline: Deadline,
    ) -> Result<AdsDuplicateReply, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy.authorize(&key.site, AdsAction::Duplicate)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                let inspect = ads
                    .inspect_ads_object(
                        &app,
                        &creds,
                        &AdsInspectRequest {
                            kind: AdsInventoryKind::from_entity(request.entity),
                            id: request.id.clone(),
                        },
                        deadline,
                    )
                    .await?;
                confirm_budget_echo(
                    &inspect,
                    request.confirm_daily_budget,
                    request.confirm_lifetime_budget,
                )
                .map_err(|reason| Error::InvalidQuery {
                    site: inspect.site.clone(),
                    reason,
                })?;
                ads.duplicate_ad_object(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
    }

    pub async fn update_ad_budget(
        &self,
        key: &AccountKey,
        request: AdsBudgetUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UpdateBudget)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                let inspect = ads
                    .inspect_ads_object(
                        &app,
                        &creds,
                        &AdsInspectRequest {
                            kind: AdsInventoryKind::from_entity(request.entity),
                            id: request.id.clone(),
                        },
                        deadline,
                    )
                    .await?;
                if inspect.daily_budget.is_none() {
                    return Err(Error::InvalidQuery {
                        site: inspect.site.clone(),
                        reason: "budget_not_on_object".into(),
                    });
                }
                let current = inspect
                    .daily_budget
                    .as_deref()
                    .and_then(|raw| raw.parse::<u64>().ok());
                if current != Some(request.current_daily_budget) {
                    return Err(Error::InvalidQuery {
                        site: inspect.site.clone(),
                        reason: "current_daily_budget_mismatch".into(),
                    });
                }
                post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    &[("daily_budget".into(), request.new_daily_budget.to_string())],
                    None,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    /// Same confirmation and max-change guard as daily, posting
    /// `lifetime_budget`. Campaign/ad set only.
    pub async fn update_ad_lifetime_budget(
        &self,
        key: &AccountKey,
        request: AdsLifetimeBudgetUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UpdateBudget)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                let inspect = ads
                    .inspect_ads_object(
                        &app,
                        &creds,
                        &AdsInspectRequest {
                            kind: AdsInventoryKind::from_entity(request.entity),
                            id: request.id.clone(),
                        },
                        deadline,
                    )
                    .await?;
                if inspect.lifetime_budget.is_none() {
                    return Err(Error::InvalidQuery {
                        site: inspect.site.clone(),
                        reason: "budget_not_on_object".into(),
                    });
                }
                let current = inspect
                    .lifetime_budget
                    .as_deref()
                    .and_then(|raw| raw.parse::<u64>().ok());
                if current != Some(request.current_lifetime_budget) {
                    return Err(Error::InvalidQuery {
                        site: inspect.site.clone(),
                        reason: "current_lifetime_budget_mismatch".into(),
                    });
                }
                post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    &[(
                        "lifetime_budget".into(),
                        request.new_lifetime_budget.to_string(),
                    )],
                    None,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    pub async fn update_ad_bid(
        &self,
        key: &AccountKey,
        request: AdsBidUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy.authorize(&key.site, AdsAction::UpdateBid)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                // Plan: GET current strategy before POST so a typo cannot
                // mutate an object we have not read.
                ads.inspect_ads_object(
                    &app,
                    &creds,
                    &AdsInspectRequest {
                        kind: AdsInventoryKind::from_entity(request.entity),
                        id: request.id.clone(),
                    },
                    deadline,
                )
                .await?;
                let mut fields = vec![(
                    "bid_strategy".into(),
                    request.bid_strategy.meta_value().into(),
                )];
                if let Some(amount) = request.bid_amount {
                    fields.push(("bid_amount".into(), amount.to_string()));
                }
                if let Some(floor) = request.roas_average_floor {
                    fields.push((
                        "bid_constraints".into(),
                        serde_json::json!({ "roas_average_floor": floor }).to_string(),
                    ));
                }
                post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    &fields,
                    None,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    pub async fn update_ad_schedule(
        &self,
        key: &AccountKey,
        request: AdsScheduleUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UpdateSchedule)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                let mut fields = Vec::new();
                if let Some(start) = &request.start_time {
                    fields.push(("start_time".into(), start.clone()));
                }
                if let Some(end) = &request.end_time {
                    fields.push(("end_time".into(), end.clone()));
                }
                post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    &fields,
                    None,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    pub async fn update_ad_placement(
        &self,
        key: &AccountKey,
        request: AdsPlacementUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UpdatePlacement)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                let mut targeting = ads
                    .read_ad_targeting_json(&app, &creds, &request.id, deadline)
                    .await?;
                if !targeting.is_object() {
                    targeting = serde_json::json!({});
                }
                merge_string_list(
                    &mut targeting,
                    "publisher_platforms",
                    request
                        .publisher_platforms
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                merge_string_list(
                    &mut targeting,
                    "facebook_positions",
                    request
                        .facebook_positions
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                merge_string_list(
                    &mut targeting,
                    "instagram_positions",
                    request
                        .instagram_positions
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                merge_string_list(
                    &mut targeting,
                    "whatsapp_positions",
                    request
                        .whatsapp_positions
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    &[("targeting".into(), targeting.to_string())],
                    None,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    pub async fn update_ad_targeting(
        &self,
        key: &AccountKey,
        request: AdsTargetingUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UpdateTargeting)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                let inspect = ads
                    .inspect_ads_object(
                        &app,
                        &creds,
                        &AdsInspectRequest {
                            kind: AdsInventoryKind::Adset,
                            id: request.id.clone(),
                        },
                        deadline,
                    )
                    .await?;
                let campaign_id =
                    inspect
                        .campaign_id
                        .as_deref()
                        .ok_or_else(|| Error::InvalidQuery {
                            site: inspect.site.clone(),
                            reason: "missing_campaign_id".into(),
                        })?;
                let categories = ads
                    .read_special_ad_categories(&app, &creds, campaign_id, deadline)
                    .await?;
                if !categories.is_empty() {
                    return Err(Error::InvalidQuery {
                        site: inspect.site.clone(),
                        reason: "special_ad_category_contract".into(),
                    });
                }
                let before = inspect.targeting.clone().unwrap_or_default();
                let mut targeting = ads
                    .read_ad_targeting_json(&app, &creds, &request.id, deadline)
                    .await?;
                if !targeting.is_object() {
                    targeting = serde_json::json!({});
                }
                targeting["geo_locations"] = serde_json::to_value(&request.targeting.geo_locations)
                    .unwrap_or(serde_json::Value::Null);
                if let Some(min) = request.targeting.age_min {
                    targeting["age_min"] = serde_json::json!(min);
                }
                if let Some(max) = request.targeting.age_max {
                    targeting["age_max"] = serde_json::json!(max);
                }
                if let Some(unknown) = request.targeting.user_age_unknown {
                    targeting["user_age_unknown"] = serde_json::json!(unknown);
                }
                merge_string_list(
                    &mut targeting,
                    "publisher_platforms",
                    request
                        .targeting
                        .publisher_platforms
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                merge_string_list(
                    &mut targeting,
                    "facebook_positions",
                    request
                        .targeting
                        .facebook_positions
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                merge_string_list(
                    &mut targeting,
                    "instagram_positions",
                    request
                        .targeting
                        .instagram_positions
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                merge_string_list(
                    &mut targeting,
                    "whatsapp_positions",
                    request
                        .targeting
                        .whatsapp_positions
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                let outcome = post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    &[("targeting".into(), targeting.to_string())],
                    None,
                    deadline,
                )
                .await?;
                Ok(match outcome {
                    AdsEditOutcome::Applied { inspect, .. } => {
                        let after = inspect.targeting.clone().unwrap_or_default();
                        AdsEditOutcome::Applied {
                            targeting_diff: Some(AdsTargetingDiff { before, after }),
                            inspect,
                        }
                    }
                    other => other,
                })
            })
        })
        .await
    }

    pub async fn swap_ad_creative(
        &self,
        key: &AccountKey,
        request: AdsCreativeSwapRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::SwapCreative)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    crate::ads::AdEntity::Ad,
                    &request.id,
                    &[(
                        "creative".into(),
                        serde_json::json!({ "creative_id": request.creative_id }).to_string(),
                    )],
                    None,
                    deadline,
                )
                .await
            })
        })
        .await
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
        self.require_capability(&key.site, Capability::ReadAdReviewStatus)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdReviewStatus)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move { ads.ad_review_status(&app, &creds, &request, deadline).await })
        })
        .await
    }

    /// Explicit System User bootstrap. Verifies the token and refuses a
    /// user OAuth token stored as an unattended secret.
    pub async fn put_ads_system_user_token(
        &self,
        key: &AccountKey,
        token: &str,
        deadline: Deadline,
    ) -> Result<WhoAmI, Error> {
        self.require_capability(&key.site, Capability::ReadAdAccounts)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdAccounts)?;
        let publisher = self.publisher(&key.site)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = ads
            .bootstrap_system_user_token(&app, token, deadline)
            .await?;
        self.vault.put(key, &creds)?;
        publisher.whoami(&app, &creds).await
    }

    /// `GET /debug_token` metadata for the stored credential. Never returns
    /// the token or app secret.
    pub async fn inspect_ads_token(
        &self,
        key: &AccountKey,
        deadline: Deadline,
    ) -> Result<AdsTokenInspection, Error> {
        self.require_capability(&key.site, Capability::ReadAdAccounts)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdAccounts)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            Box::pin(async move { ads.inspect_access_token(&app, &creds, deadline).await })
        })
        .await
    }

    /// Operator-facing Marketing API Access Tier. Header mapping is a hint;
    /// Meta's App Dashboard remains authoritative.
    pub async fn ads_access_tier(
        &self,
        key: &AccountKey,
        deadline: Deadline,
    ) -> Result<MarketingApiAccessTier, Error> {
        self.require_capability(&key.site, Capability::ReadAdAccounts)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdAccounts)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            Box::pin(async move { ads.marketing_api_access_tier(&app, &creds, deadline).await })
        })
        .await
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
        self.require_capability(&key.site, Capability::ReadAdReviewStatus)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdReviewStatus)?;
        let (publisher, app, mut creds) = self.prepare_creds(key, deadline, true).await?;
        let mut retried_expired_token = false;

        loop {
            let status = match ads.ad_review_status(&app, &creds, &request, deadline).await {
                Err(e) if !retried_expired_token => {
                    // Match every other Client read: an expired token gets
                    // one refresh and one retry, never an unbounded refresh
                    // loop hidden inside a status poller.
                    creds = self
                        .recover_expired(&*publisher, &app, key, creds, deadline, e)
                        .await?;
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
        let need = intent.body.required_capability();
        self.require_capability(&intent.site, need)?;
        // Probe skips proactive refresh: the probe's own response is the
        // instrument. Reactive token_expired still retries once so a
        // refreshable token is not reported as broken.
        let (publisher, app, creds) = self.prepare_creds(key, deadline, false).await?;
        match publisher
            .probe(&app, &creds, intent.clone(), deadline)
            .await
        {
            Err(e) => {
                let creds = self
                    .recover_expired(&*publisher, &app, key, creds, deadline, e)
                    .await?;
                publisher.probe(&app, &creds, intent, deadline).await
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
        // A raw token is valid only for an explicit bearer-token auth kind.
        // App-password and no-auth sites must refuse it at the flag boundary
        // rather than storing a credential their connector will never use.
        if !matches!(
            publisher.auth_kind(),
            AuthKind::OAuth2AuthCode | AuthKind::StaticToken
        ) {
            return Err(Error::Auth {
                site: key.site.clone(),
                reason: "token_bootstrap_unsupported".into(),
            });
        }
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = match publisher.auth_kind() {
            AuthKind::OAuth2AuthCode => AccountCreds::OAuth2 {
                access_token: token.to_string(),
                refresh_token: None,
                extra: serde_json::json!({}),
            },
            AuthKind::StaticToken => AccountCreds::BotToken {
                token: token.to_string(),
            },
            AuthKind::None | AuthKind::AppPassword => unreachable!("auth kind checked above"),
        };
        // Verify the token *before* anything lands in the vault: the old
        // store-then-whoami order persisted an invalid token and only then
        // rejected it. The id that comes back is persisted into `extra`,
        // matching the OAuth path (`creds_from_long`), so both auth shapes
        // publish against /{user_id}/threads instead of this path leaning
        // on the /me alias for its whole lifetime.
        let me = publisher.whoami(&app, &creds).await?;
        let creds = match publisher.auth_kind() {
            AuthKind::OAuth2AuthCode => AccountCreds::OAuth2 {
                access_token: token.to_string(),
                refresh_token: None,
                extra: serde_json::json!({ "user_id": me.id }),
            },
            // A static token's target belongs to application config (for
            // WhatsApp, the selected phone-number ID), not the vault token.
            AuthKind::StaticToken => AccountCreds::BotToken {
                token: token.to_string(),
            },
            AuthKind::None | AuthKind::AppPassword => unreachable!("auth kind checked above"),
        };
        self.vault.put(key, &creds)?;
        Ok(me)
    }

    fn publisher(&self, site: &Site) -> Result<Arc<dyn Publisher>, Error> {
        self.registry
            .get(site)
            .ok_or_else(|| Error::UnknownSite(site.clone()))
    }

    fn require_capability(&self, site: &Site, need: Capability) -> Result<(), Error> {
        let publisher = self.publisher(site)?;
        if publisher.capabilities().contains(&need) {
            Ok(())
        } else {
            Err(Error::UnsupportedCapability {
                site: site.clone(),
                need,
            })
        }
    }

    /// Facet lookup is fail-closed: advertising a capability without attaching
    /// the matching trait is the same as not implementing it. That is what
    /// keeps `Publisher` frozen — extra verbs cannot sneak in as default
    /// methods the next connector would inherit.
    fn missing_facet(site: &Site, need: Capability) -> Error {
        Error::UnsupportedCapability {
            site: site.clone(),
            need,
        }
    }

    fn insights_source(
        &self,
        site: &Site,
        need: Capability,
    ) -> Result<Arc<dyn InsightsSource>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.insights_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
    }

    fn ads_manager(&self, site: &Site, need: Capability) -> Result<Arc<dyn AdsManager>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.ads_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
    }

    fn page_directory(&self, site: &Site) -> Result<Arc<dyn PageDirectory>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.pages_facet())
            .ok_or_else(|| Self::missing_facet(site, Capability::ReadPages))
    }

    fn media_reader(&self, site: &Site) -> Result<Arc<dyn MediaReader>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.media_facet())
            .ok_or_else(|| Self::missing_facet(site, Capability::ReadMedia))
    }

    #[cfg(feature = "whatsapp-cloud")]
    fn whatsapp_assets(
        &self,
        site: &Site,
        need: Capability,
    ) -> Result<Arc<dyn crate::facets::WhatsAppAssets>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.whatsapp_assets_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
    }

    #[cfg(feature = "whatsapp-cloud")]
    fn whatsapp_account(
        &self,
        site: &Site,
        need: Capability,
    ) -> Result<Arc<dyn crate::facets::WhatsAppAccount>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.whatsapp_account_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
    }

    #[cfg(feature = "whatsapp-cloud")]
    fn whatsapp_flows(
        &self,
        site: &Site,
        need: Capability,
    ) -> Result<Arc<dyn crate::facets::WhatsAppFlows>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.whatsapp_flows_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
    }

    #[cfg(feature = "whatsapp-cloud")]
    fn whatsapp_templates(
        &self,
        site: &Site,
        need: Capability,
    ) -> Result<Arc<dyn crate::facets::WhatsAppTemplates>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.whatsapp_templates_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
    }

    #[cfg(feature = "whatsapp-cloud")]
    fn whatsapp_sender(
        &self,
        site: &Site,
        need: Capability,
    ) -> Result<Arc<dyn WhatsAppSender>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.whatsapp_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
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

#[cfg(feature = "whatsapp-cloud")]
fn resolve_whatsapp_outbound_sender(
    app: &AppConfig,
    sender_alias: Option<&str>,
    site: &Site,
) -> Result<(AppConfig, String), Error> {
    let primary = app
        .extra
        .get("phone_number_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| {
            !id.is_empty() && id.len() <= 32 && id.bytes().all(|byte| byte.is_ascii_digit())
        })
        .map(str::to_owned);

    let selected = match sender_alias {
        // Let the connector retain the established `missing_phone_number_id`
        // failure for an unconfigured primary sender. This also keeps Client
        // generic enough for test/embedding connectors that do not model the
        // WhatsApp app extension at all.
        None => primary.unwrap_or_else(|| "primary".into()),
        Some(alias) => {
            if !crate::types::valid_name(alias) || alias == "primary" {
                return Err(Error::InvalidQuery {
                    site: site.clone(),
                    reason: "whatsapp_sender_alias_invalid".into(),
                });
            }
            let configured = app
                .extra
                .get("senders")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| Error::InvalidQuery {
                    site: site.clone(),
                    reason: "whatsapp_sender_unknown".into(),
                })?;
            let mut found = None;
            for value in configured {
                let sender: crate::whatsapp::WhatsAppOutboundSender =
                    serde_json::from_value(value.clone()).map_err(|_| Error::InvalidQuery {
                        site: site.clone(),
                        reason: "whatsapp_sender_config_invalid".into(),
                    })?;
                sender.validate().map_err(|reason| Error::InvalidQuery {
                    site: site.clone(),
                    reason,
                })?;
                if sender.alias == alias && found.replace(sender.phone_number_id).is_some() {
                    return Err(Error::InvalidQuery {
                        site: site.clone(),
                        reason: "whatsapp_sender_alias_duplicate".into(),
                    });
                }
            }
            found.ok_or_else(|| Error::InvalidQuery {
                site: site.clone(),
                reason: "whatsapp_sender_unknown".into(),
            })?
        }
    };

    // Connector wire methods still read `phone_number_id` from AppConfig.
    // Clone only the in-memory config so selecting a sender never rewrites a
    // user's primary sender or leaks into webhook configuration on disk.
    let mut selected_app = app.clone();
    if selected != "primary" {
        selected_app.extra["phone_number_id"] = serde_json::Value::String(selected.clone());
    }
    Ok((selected_app, selected))
}

async fn activate_ad_inner(
    ads: &dyn AdsManager,
    app: &AppConfig,
    creds: &AccountCreds,
    request: &AdsActivateRequest,
    deadline: Deadline,
) -> Result<AdsLifecycleOutcome, Error> {
    let review = ads
        .ad_review_status(
            app,
            creds,
            &AdReviewStatusRequest {
                entity: request.entity,
                id: request.id.clone(),
            },
            deadline,
        )
        .await?;
    // ARCHIVED stays refused. Meta can restore with status=ACTIVE; Postkit
    // does not treat archive as a pause we can reverse through activate.
    if review.configured_status != "PAUSED" {
        return Err(Error::InvalidQuery {
            site: review.site.clone(),
            reason: format!("not_paused:{}", review.configured_status),
        });
    }
    if review.is_pending_review() {
        return Err(Error::InvalidQuery {
            site: review.site.clone(),
            reason: "review_unresolved".into(),
        });
    }
    if !review.issues.is_empty() {
        return Err(Error::InvalidQuery {
            site: review.site.clone(),
            reason: "review_issues".into(),
        });
    }
    let inspect = ads
        .inspect_ads_object(
            app,
            creds,
            &AdsInspectRequest {
                kind: AdsInventoryKind::from_entity(request.entity),
                id: request.id.clone(),
            },
            deadline,
        )
        .await?;
    confirm_activate_budget(&inspect, request).map_err(|reason| Error::InvalidQuery {
        site: inspect.site.clone(),
        reason,
    })?;
    match ads
        .update_ad_status(
            app,
            creds,
            &AdsStatusUpdateRequest {
                entity: request.entity,
                id: request.id.clone(),
                status: AdsConfiguredStatus::Active,
            },
            deadline,
        )
        .await
    {
        Ok(status) => Ok(AdsLifecycleOutcome::Applied { status }),
        Err(Error::Network { .. } | Error::DeadlineExceeded { .. }) => {
            Ok(AdsLifecycleOutcome::ReconciliationRequired {
                entity: request.entity,
                id: request.id.clone(),
                guidance: ACTIVATE_RECONCILE_GUIDANCE.into(),
            })
        }
        Err(error) => Err(error),
    }
}

fn merge_string_list(targeting: &mut serde_json::Value, key: &str, values: Vec<String>) {
    if values.is_empty() {
        return;
    }
    targeting[key] = serde_json::json!(values);
}

#[allow(clippy::too_many_arguments)]
async fn post_then_inspect(
    ads: &dyn AdsManager,
    app: &AppConfig,
    creds: &AccountCreds,
    entity: crate::ads::AdEntity,
    id: &str,
    fields: &[(String, String)],
    targeting_diff: Option<AdsTargetingDiff>,
    deadline: Deadline,
) -> Result<AdsEditOutcome, Error> {
    match ads.post_ad_update(app, creds, id, fields, deadline).await {
        Ok(()) => match ads
            .inspect_ads_object(
                app,
                creds,
                &AdsInspectRequest {
                    kind: AdsInventoryKind::from_entity(entity),
                    id: id.into(),
                },
                deadline,
            )
            .await
        {
            Ok(inspect) => Ok(AdsEditOutcome::Applied {
                inspect: Box::new(inspect),
                targeting_diff,
            }),
            Err(Error::Network { .. } | Error::DeadlineExceeded { .. }) => {
                Ok(AdsEditOutcome::ReconciliationRequired {
                    entity,
                    id: id.into(),
                    guidance: EDIT_RECONCILE_GUIDANCE.into(),
                })
            }
            Err(error) => Err(error),
        },
        Err(Error::Network { .. } | Error::DeadlineExceeded { .. }) => {
            Ok(AdsEditOutcome::ReconciliationRequired {
                entity,
                id: id.into(),
                guidance: EDIT_RECONCILE_GUIDANCE.into(),
            })
        }
        Err(error) => Err(error),
    }
}

fn confirm_budget_echo(
    inspect: &AdsInspectReply,
    confirm_daily_budget: Option<u64>,
    confirm_lifetime_budget: Option<u64>,
) -> Result<(), String> {
    let daily = inspect
        .daily_budget
        .as_deref()
        .map(|raw| raw.parse::<u64>())
        .transpose()
        .map_err(|_| "bad_inspect_daily_budget".to_string())?;
    let lifetime = inspect
        .lifetime_budget
        .as_deref()
        .map(|raw| raw.parse::<u64>())
        .transpose()
        .map_err(|_| "bad_inspect_lifetime_budget".to_string())?;
    if daily.is_some() && confirm_daily_budget != daily {
        return Err("confirm_daily_budget_mismatch".into());
    }
    if lifetime.is_some() && confirm_lifetime_budget != lifetime {
        return Err("confirm_lifetime_budget_mismatch".into());
    }
    if daily.is_none() && confirm_daily_budget.is_some() {
        return Err("confirm_daily_budget_not_on_object".into());
    }
    if lifetime.is_none() && confirm_lifetime_budget.is_some() {
        return Err("confirm_lifetime_budget_not_on_object".into());
    }
    Ok(())
}

fn confirm_activate_budget(
    inspect: &AdsInspectReply,
    request: &AdsActivateRequest,
) -> Result<(), String> {
    confirm_budget_echo(
        inspect,
        request.confirm_daily_budget,
        request.confirm_lifetime_budget,
    )
}

async fn pause_ad_inner(
    ads: &dyn AdsManager,
    app: &AppConfig,
    creds: &AccountCreds,
    request: &AdsPauseRequest,
    deadline: Deadline,
) -> Result<AdsLifecycleOutcome, Error> {
    let review = ads
        .ad_review_status(
            app,
            creds,
            &AdReviewStatusRequest {
                entity: request.entity,
                id: request.id.clone(),
            },
            deadline,
        )
        .await?;
    match review.configured_status.as_str() {
        "ARCHIVED" | "DELETED" => {
            return Err(Error::InvalidQuery {
                site: review.site.clone(),
                reason: format!("not_pausable:{}", review.configured_status),
            });
        }
        "PAUSED" => return Ok(AdsLifecycleOutcome::Applied { status: review }),
        _ => {}
    }
    match ads
        .update_ad_status(
            app,
            creds,
            &AdsStatusUpdateRequest {
                entity: request.entity,
                id: request.id.clone(),
                status: AdsConfiguredStatus::Paused,
            },
            deadline,
        )
        .await
    {
        Ok(status) => Ok(AdsLifecycleOutcome::Applied { status }),
        Err(Error::Network { .. } | Error::DeadlineExceeded { .. }) => {
            Ok(AdsLifecycleOutcome::ReconciliationRequired {
                entity: request.entity,
                id: request.id.clone(),
                guidance: PAUSE_RECONCILE_GUIDANCE.into(),
            })
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
async fn confirmed_status_inner(
    ads: &dyn AdsManager,
    app: &AppConfig,
    creds: &AccountCreds,
    entity: crate::ads::AdEntity,
    id: &str,
    status: AdsConfiguredStatus,
    refuse: &[&str],
    guidance: &str,
    deadline: Deadline,
) -> Result<AdsLifecycleOutcome, Error> {
    let review = ads
        .ad_review_status(
            app,
            creds,
            &AdReviewStatusRequest {
                entity,
                id: id.into(),
            },
            deadline,
        )
        .await?;
    if refuse
        .iter()
        .any(|value| review.configured_status.eq_ignore_ascii_case(value))
    {
        return Err(Error::InvalidQuery {
            site: review.site.clone(),
            reason: format!("not_{}:{}", status.as_str(), review.configured_status),
        });
    }
    if review
        .configured_status
        .eq_ignore_ascii_case(status.meta_value())
    {
        return Ok(AdsLifecycleOutcome::Applied { status: review });
    }
    match ads
        .update_ad_status(
            app,
            creds,
            &AdsStatusUpdateRequest {
                entity,
                id: id.into(),
                status,
            },
            deadline,
        )
        .await
    {
        Ok(status) => Ok(AdsLifecycleOutcome::Applied { status }),
        Err(Error::Network { .. } | Error::DeadlineExceeded { .. }) => {
            Ok(AdsLifecycleOutcome::ReconciliationRequired {
                entity,
                id: id.into(),
                guidance: guidance.into(),
            })
        }
        Err(error) => Err(error),
    }
}

#[cfg(feature = "whatsapp-cloud")]
fn scoped_whatsapp_idempotency(phone_number_id: &str, idempotency_key: &str) -> String {
    // Both segments are locally validated to `[A-Za-z0-9._-]+`/digits. This
    // becomes a vault-private namespace, not a Meta idempotency header.
    format!("wa-sender-{phone_number_id}-{idempotency_key}")
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
        AdEntity, AdReviewStatusRequest, CreatePausedAdRequest, PausedAd, PausedAdCreate,
        PausedAdset, PausedCampaign, UploadAdImageRequest,
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
                if step == DraftStep::Image && !manifest.needs_image_upload() {
                    state.set_output(
                        DraftStep::Image,
                        manifest
                            .creative
                            .image_hash
                            .clone()
                            .unwrap_or_else(|| "skipped".into()),
                    );
                    store
                        .checkpoint(state_path, &state)
                        .map_err(|reason| Error::InvalidQuery {
                            site: site.clone(),
                            reason,
                        })?;
                    continue;
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
                                daily_budget: manifest.campaign.daily_budget,
                                lifetime_budget: manifest.campaign.lifetime_budget,
                                is_adset_budget_sharing_enabled: manifest
                                    .campaign
                                    .is_adset_budget_sharing_enabled,
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
                                lifetime_budget: manifest.adset.lifetime_budget,
                                bid_strategy: manifest.adset.bid_strategy,
                                bid_amount: manifest.adset.bid_amount,
                                roas_average_floor: manifest.adset.roas_average_floor,
                                billing_event: manifest.adset.billing_event,
                                optimization_goal: manifest.adset.optimization_goal,
                                targeting: manifest.adset.targeting.clone(),
                                start_time: manifest.adset.start_time.clone(),
                                end_time: manifest.adset.end_time.clone(),
                                promoted_object: manifest.adset.promoted_object.clone(),
                            }),
                        };
                        self.create_paused_ad(key, request, deadline)
                            .await
                            .map(|created| created.id)
                    }
                    DraftStep::Creative => {
                        self.draft_create_creative(key, manifest, &state, &account, deadline)
                            .await
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

        async fn draft_create_creative(
            &self,
            key: &AccountKey,
            manifest: &crate::draft::PausedDraftManifest,
            state: &crate::draft::PausedDraftState,
            account: &str,
            deadline: Deadline,
        ) -> Result<String, Error> {
            crate::client::draft_create_creative_inner(
                self, key, manifest, state, account, deadline,
            )
            .await
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

#[cfg(feature = "draft")]
async fn draft_create_creative_inner(
    client: &Client,
    key: &AccountKey,
    manifest: &crate::draft::PausedDraftManifest,
    state: &crate::draft::PausedDraftState,
    account: &str,
    deadline: Deadline,
) -> Result<String, Error> {
    let c = &manifest.creative;
    let hash = state
        .image_hash
        .clone()
        .filter(|h| h != "skipped")
        .or_else(|| c.image_hash.clone())
        .unwrap_or_default();
    match c.kind {
        crate::draft::DraftCreativeKind::Link => {
            let request = crate::ads::CreateLinkAdCreativeRequest {
                account: Some(account.into()),
                creative: crate::ads::LinkAdCreative {
                    name: c.name.clone(),
                    page_id: c.page_id.clone(),
                    image_hash: hash,
                    message: c.message.clone(),
                    headline: c.headline.clone(),
                    destination_url: c.destination_url.clone(),
                    call_to_action: c.call_to_action,
                    geo_link: c.geo_link.clone(),
                    application_id: c.application_id.clone(),
                    app_link: c.app_link.clone(),
                    instagram_user_id: None,
                    advantage_plus: false,
                    whatsapp_identity: None,
                },
            };
            client
                .create_link_ad_creative(key, request, deadline)
                .await
                .map(|created| created.id)
        }
        crate::draft::DraftCreativeKind::Video => {
            let request = crate::ads::CreateVideoAdCreativeRequest {
                account: Some(account.into()),
                creative: crate::ads::VideoAdCreative {
                    name: c.name.clone(),
                    page_id: c.page_id.clone(),
                    video_id: c.video_id.clone().unwrap_or_default(),
                    image_hash: hash,
                    message: c.message.clone(),
                    destination_url: c.destination_url.clone(),
                    call_to_action: c.call_to_action,
                    geo_link: c.geo_link.clone(),
                    application_id: c.application_id.clone(),
                    app_link: c.app_link.clone(),
                    instagram_user_id: None,
                    advantage_plus: false,
                    whatsapp_identity: None,
                },
            };
            client
                .create_video_ad_creative(key, request, deadline)
                .await
                .map(|created| created.id)
        }
        other => {
            let kind = match other {
                crate::draft::DraftCreativeKind::Carousel => {
                    crate::ads::AdCreativeKind::Carousel(crate::ads::CarouselAdCreative {
                        name: c.name.clone(),
                        page_id: c.page_id.clone(),
                        message: c.message.clone(),
                        call_to_action: c.call_to_action,
                        cards: c.cards.clone(),
                        instagram_user_id: None,
                        advantage_plus: false,
                        whatsapp_identity: None,
                    })
                }
                crate::draft::DraftCreativeKind::Catalog => {
                    crate::ads::AdCreativeKind::Catalog(crate::ads::CatalogAdCreative {
                        name: c.name.clone(),
                        page_id: c.page_id.clone(),
                        product_set_id: c.product_set_id.clone().unwrap_or_default(),
                        link: c.link.clone().unwrap_or_else(|| c.destination_url.clone()),
                        message: c.message.clone(),
                        call_to_action: c.call_to_action,
                        instagram_user_id: None,
                        advantage_plus: false,
                        whatsapp_identity: None,
                    })
                }
                crate::draft::DraftCreativeKind::LeadForm => {
                    crate::ads::AdCreativeKind::LeadForm(crate::ads::LeadFormAdCreative {
                        name: c.name.clone(),
                        page_id: c.page_id.clone(),
                        image_hash: hash,
                        message: c.message.clone(),
                        headline: c.headline.clone(),
                        destination_url: c.destination_url.clone(),
                        lead_gen_form_id: c.lead_gen_form_id.clone().unwrap_or_default(),
                        call_to_action: c.call_to_action,
                        instagram_user_id: None,
                        advantage_plus: false,
                        whatsapp_identity: None,
                    })
                }
                crate::draft::DraftCreativeKind::AppInstall => {
                    crate::ads::AdCreativeKind::AppInstall(crate::ads::AppInstallAdCreative {
                        name: c.name.clone(),
                        page_id: c.page_id.clone(),
                        image_hash: hash,
                        message: c.message.clone(),
                        application_id: c.application_id.clone().unwrap_or_default(),
                        object_store_url: c.object_store_url.clone().unwrap_or_default(),
                        instagram_user_id: None,
                        advantage_plus: false,
                        whatsapp_identity: None,
                    })
                }
                crate::draft::DraftCreativeKind::Link | crate::draft::DraftCreativeKind::Video => {
                    unreachable!()
                }
            };
            client
                .create_ad_creative(
                    key,
                    crate::ads::CreateAdCreativeRequest {
                        account: Some(account.into()),
                        kind,
                    },
                    deadline,
                )
                .await
                .map(|created| created.id)
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
    if extra.get("token_kind").and_then(|v| v.as_str()) == Some(SYSTEM_USER_TOKEN_KIND) {
        return false;
    }
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
