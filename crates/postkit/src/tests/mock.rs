//! Shared Client-test fixtures: MockPub, vault/app setup, request builders.
use crate::ads::{
    AdEntity, AdReviewIssue, AdReviewStatus, AdReviewStatusRequest, AdsActivateRequest,
    AdsInspectReply, AdsInspectRequest, AdsInventoryItem, AdsInventoryReply, AdsInventoryRequest,
    AdsStatusUpdateRequest, AdsTargetingReadback, CampaignObjective, CreateLinkAdCreativeRequest,
    CreatePausedAdRequest, CreatedAd, CreatedAdCreative, CreativePreview, CreativePreviewRequest,
    PausedAdCreate, PausedCampaign, UploadAdImageRequest, UploadedAdImage,
};
use crate::apps::{AppStore, MemoryAppStore};
use crate::client::Client;
use crate::error::Error;
#[cfg(feature = "whatsapp-cloud")]
use crate::facets::WhatsAppSender;
use crate::facets::{AdsManager, InsightsSource, MediaReader, PageDirectory};
use crate::insights::{
    AdAccount, AdAccountsReply, AttributionWindow, InsightRow, InsightsLevel, InsightsQuery,
    InsightsReply, Metric,
};
use crate::media::{MediaQuery, MediaReply, PublishedMedia};
use crate::pages::{PageAccount, PagesReply};
use crate::policy::{AdsAction, AdsPolicy};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::registry::{Connector, Registry};
use crate::types::{
    AccountCreds, AccountKey, AppConfig, Body, Capability, Deadline, Intent, Outcome, Probe, Site,
    WhoAmI,
};
use crate::vault::{MemoryVault, Vault};
#[cfg(feature = "whatsapp-cloud")]
use crate::whatsapp::WhatsAppSendRequest;
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub(super) struct MockPub {
    pub(super) site: Site,
    pub(super) caps: Vec<Capability>,
    pub(super) auth_kind: AuthKind,
    pub(super) fail_auth_once: bool,
    pub(super) fail_publish: bool,
    pub(super) whoami_fails: bool,
    pub(super) refresh_network_err: bool,
    pub(super) refresh_dead_session: bool,
    /// 023 concurrency scripting: `publish_started` fires when `publish`
    /// is entered and `publish_gate` parks it until released — together
    /// they hold one publish inside the claim window deterministically,
    /// without real timing races. Mutex'd because send/await consume the
    /// channels by value while the trait only lends `&self`.
    pub(super) publish_started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    pub(super) publish_gate: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    /// 024: the deadline refresh actually received — tests assert the
    /// caller's budget was threaded through, not a private 30s.
    pub(super) refresh_deadline: std::sync::Mutex<Option<Deadline>>,
    pub(super) publishes: AtomicUsize,
    pub(super) probes: AtomicUsize,
    pub(super) paused_creates: AtomicUsize,
    pub(super) image_uploads: AtomicUsize,
    pub(super) creative_creates: AtomicUsize,
    pub(super) creative_previews: AtomicUsize,
    pub(super) review_status_reads: AtomicUsize,
    pub(super) page_reads: AtomicUsize,
    pub(super) media_reads: AtomicUsize,
    pub(super) inspect_daily_budget: Option<String>,
    pub(super) inspect_lifetime_budget: Option<String>,
    #[cfg(feature = "whatsapp-cloud")]
    pub(super) whatsapp_sends: AtomicUsize,
    #[cfg(feature = "whatsapp-cloud")]
    pub(super) whatsapp_phone_ids: std::sync::Mutex<Vec<String>>,
    pub(super) review_status_pending_reads: usize,
}

impl MockPub {
    pub(super) fn text(site: &str) -> Self {
        Self {
            site: Site::new(site),
            caps: vec![Capability::PublishText],
            auth_kind: AuthKind::OAuth2AuthCode,
            fail_auth_once: false,
            fail_publish: false,
            whoami_fails: false,
            refresh_network_err: false,
            refresh_dead_session: false,
            publish_started: std::sync::Mutex::new(None),
            publish_gate: std::sync::Mutex::new(None),
            refresh_deadline: std::sync::Mutex::new(None),
            publishes: AtomicUsize::new(0),
            probes: AtomicUsize::new(0),
            paused_creates: AtomicUsize::new(0),
            image_uploads: AtomicUsize::new(0),
            creative_creates: AtomicUsize::new(0),
            creative_previews: AtomicUsize::new(0),
            review_status_reads: AtomicUsize::new(0),
            page_reads: AtomicUsize::new(0),
            media_reads: AtomicUsize::new(0),
            inspect_daily_budget: Some("500".into()),
            inspect_lifetime_budget: Some("10000".into()),
            #[cfg(feature = "whatsapp-cloud")]
            whatsapp_sends: AtomicUsize::new(0),
            #[cfg(feature = "whatsapp-cloud")]
            whatsapp_phone_ids: std::sync::Mutex::new(Vec::new()),
            review_status_pending_reads: 0,
        }
    }

    pub(super) fn metrics(site: &str) -> Self {
        Self {
            caps: vec![Capability::ReadMetrics],
            ..Self::text(site)
        }
    }

    pub(super) fn ad_accounts(site: &str) -> Self {
        Self {
            caps: vec![Capability::ReadAdAccounts],
            ..Self::text(site)
        }
    }

    pub(super) fn pages(site: &str) -> Self {
        Self {
            caps: vec![Capability::ReadPages],
            ..Self::text(site)
        }
    }

    pub(super) fn media(site: &str) -> Self {
        Self {
            caps: vec![Capability::ReadMedia],
            ..Self::text(site)
        }
    }

    pub(super) fn paused_ads(site: &str) -> Self {
        Self {
            caps: vec![Capability::CreatePausedAds],
            ..Self::text(site)
        }
    }

    pub(super) fn creative_assets(site: &str) -> Self {
        Self {
            caps: vec![Capability::CreateAdCreative],
            ..Self::text(site)
        }
    }

    pub(super) fn creative_previews(site: &str) -> Self {
        Self {
            caps: vec![Capability::ReadAdPreviews],
            ..Self::text(site)
        }
    }

    pub(super) fn review_statuses(site: &str, pending_reads: usize) -> Self {
        Self {
            caps: vec![Capability::ReadAdReviewStatus],
            review_status_pending_reads: pending_reads,
            ..Self::text(site)
        }
    }

    pub(super) fn ads_inventory(site: &str) -> Self {
        Self {
            caps: vec![Capability::ReadAdsInventory],
            ..Self::text(site)
        }
    }

    pub(super) fn ads_lifecycle(site: &str) -> Self {
        Self {
            caps: vec![
                Capability::ReadAdReviewStatus,
                Capability::ReadAdsInventory,
                Capability::ManageAdsLifecycle,
            ],
            // Daily and lifetime budgets are mutually exclusive on a real
            // delivery object. Keep lifecycle fixtures coherent so a test
            // never has to weaken the confirmation request to match a fake
            // Graph response.
            inspect_daily_budget: Some("500".into()),
            inspect_lifetime_budget: None,
            ..Self::text(site)
        }
    }

    /// A separate coherent lifetime-budget object for confirmation tests.
    pub(super) fn ads_lifecycle_with_lifetime_budget(site: &str) -> Self {
        Self {
            caps: vec![
                Capability::ReadAdReviewStatus,
                Capability::ReadAdsInventory,
                Capability::ManageAdsLifecycle,
            ],
            inspect_daily_budget: None,
            inspect_lifetime_budget: Some("10000".into()),
            ..Self::text(site)
        }
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub(super) fn whatsapp(site: &str) -> Self {
        Self {
            caps: vec![
                Capability::SendReply,
                Capability::SendText,
                Capability::SendTemplate,
            ],
            auth_kind: AuthKind::StaticToken,
            ..Self::text(site)
        }
    }
}

#[async_trait]
impl Publisher for MockPub {
    fn site(&self) -> &Site {
        &self.site
    }
    fn capabilities(&self) -> &[Capability] {
        &self.caps
    }
    fn auth_kind(&self) -> AuthKind {
        self.auth_kind
    }

    async fn publish(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        intent: Intent,
        _deadline: Deadline,
    ) -> Result<Outcome, Error> {
        let n = self.publishes.fetch_add(1, Ordering::SeqCst);
        // Announce entry, then park if gated — this is what lets the 023
        // concurrency tests hold a publish inside the claim window. The
        // guard is dropped before the await: a std MutexGuard is not Send
        // and must not ride across an await point.
        if let Some(tx) = self.publish_started.lock().unwrap().take() {
            let _ = tx.send(());
        }
        let gate = self.publish_gate.lock().unwrap().take();
        if let Some(rx) = gate {
            let _ = rx.await;
        }
        if self.fail_publish {
            return Err(Error::Platform {
                site: self.site.clone(),
                code: "boom".into(),
                message: "mock failure".into(),
            });
        }
        if self.fail_auth_once && n == 0 {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "token_expired".into(),
            });
        }
        let label = match intent.body {
            Body::Text { text } => text,
            Body::Image { text, .. } => text.unwrap_or_else(|| "image".into()),
            Body::Carousel { text, .. } => text.unwrap_or_else(|| "carousel".into()),
        };
        Ok(Outcome {
            site: intent.site,
            id: Some(format!("id-{label}")),
            url: Some(format!("https://example.test/{label}")),
            limits: None,
        })
    }

    async fn probe(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        intent: Intent,
        _deadline: Deadline,
    ) -> Result<Probe, Error> {
        let n = self.probes.fetch_add(1, Ordering::SeqCst);
        // same reactive-expiry behavior as publish, so Client::probe's
        // refresh mapping is exercised by the same fail_auth_once switch
        if self.fail_auth_once && n == 0 {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "token_expired".into(),
            });
        }
        // The probe contract is text-only (015 D4); the mock refuses the
        // image body the same way the real connectors do.
        let Body::Text { text } = intent.body else {
            return Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: "probe_image_unsupported".into(),
                limit: None,
            });
        };
        Ok(Probe {
            site: intent.site,
            container_id: format!("container-{text}"),
            expires_in_hours: 24,
        })
    }

    async fn whoami(&self, _app: &AppConfig, _creds: &AccountCreds) -> Result<WhoAmI, Error> {
        if self.whoami_fails {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "invalid_token".into(),
            });
        }
        Ok(WhoAmI {
            site: self.site.clone(),
            id: "user-1".into(),
            handle: Some("tester".into()),
        })
    }

    async fn auth_start(&self, _app: &AppConfig) -> Result<AuthStart, Error> {
        Ok(AuthStart::PasteInstructions {
            hint: "app password".into(),
        })
    }

    async fn auth_finish(&self, _app: &AppConfig, reply: AuthReply) -> Result<AccountCreds, Error> {
        match reply {
            AuthReply::AppPassword {
                identifier,
                secret,
                pds,
            } => Ok(AccountCreds::AppPassword {
                identifier,
                secret,
                pds,
            }),
            _ => Err(Error::Auth {
                site: self.site.clone(),
                reason: "unsupported_auth".into(),
            }),
        }
    }

    async fn refresh(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<AccountCreds, Error> {
        *self.refresh_deadline.lock().unwrap() = Some(deadline);
        if self.refresh_network_err {
            return Err(Error::Network {
                site: self.site.clone(),
                message: "mock refresh outage".into(),
            });
        }
        if self.refresh_dead_session {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "session_expired".into(),
            });
        }
        match creds {
            AccountCreds::OAuth2 { extra, .. } => Ok(AccountCreds::OAuth2 {
                access_token: "refreshed".into(),
                refresh_token: None,
                extra: extra.clone(),
            }),
            other => Ok(other.clone()),
        }
    }
}

#[async_trait]
impl InsightsSource for MockPub {
    async fn insights(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        query: &InsightsQuery,
        _deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
        let n = self.publishes.fetch_add(1, Ordering::SeqCst);
        // Reuse fail_auth_once so Client::with_creds is proven on a
        // non-publish verb — a missed retry would only show up here.
        if self.fail_auth_once && n == 0 {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "token_expired".into(),
            });
        }
        let mut metrics = serde_json::Map::new();
        metrics.insert("spend".into(), serde_json::json!(10.0));
        Ok(InsightsReply {
            site: self.site.clone(),
            account_id: "act_1".into(),
            currency: Some("MYR".into()),
            rows: vec![InsightRow {
                entity_id: "1".into(),
                level: query.level,
                date_start: query.range.from.clone(),
                dimensions: serde_json::Map::new(),
                metrics,
            }],
        })
    }

    async fn ad_accounts(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _deadline: Deadline,
    ) -> Result<AdAccountsReply, Error> {
        Ok(AdAccountsReply {
            site: self.site.clone(),
            accounts: vec![AdAccount {
                id: "act_1".into(),
                name: Some("Main".into()),
                currency: Some("MYR".into()),
                timezone: None,
                status: Some("1".into()),
            }],
        })
    }
}

#[async_trait]
impl AdsManager for MockPub {
    async fn create_paused_ad(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &CreatePausedAdRequest,
        _deadline: Deadline,
    ) -> Result<CreatedAd, Error> {
        let n = self.paused_creates.fetch_add(1, Ordering::SeqCst);
        Ok(CreatedAd {
            site: self.site.clone(),
            account_id: request.account.clone().unwrap_or_else(|| "act_1".into()),
            entity: request.create.entity(),
            id: format!("draft-{n}"),
            status: "PAUSED".into(),
        })
    }

    async fn upload_ad_image(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &UploadAdImageRequest,
        _deadline: Deadline,
    ) -> Result<UploadedAdImage, Error> {
        let n = self.image_uploads.fetch_add(1, Ordering::SeqCst);
        Ok(UploadedAdImage {
            site: self.site.clone(),
            account_id: request.account.clone().unwrap_or_else(|| "act_1".into()),
            hash: format!("image-{n}"),
        })
    }

    async fn upload_ad_video(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &crate::ads::UploadAdVideoRequest,
        _deadline: Deadline,
    ) -> Result<crate::ads::UploadedAdVideo, Error> {
        Ok(crate::ads::UploadedAdVideo {
            site: self.site.clone(),
            account_id: request.account.clone().unwrap_or_else(|| "act_1".into()),
            id: "9001".into(),
        })
    }

    async fn ad_video_status(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &crate::ads::AdVideoStatusRequest,
        _deadline: Deadline,
    ) -> Result<crate::ads::AdVideoStatus, Error> {
        Ok(crate::ads::AdVideoStatus {
            site: self.site.clone(),
            video_id: request.video_id.clone(),
            video_status: crate::ads::AdVideoStatusKind::Ready,
            raw: Some("ready".into()),
        })
    }

    async fn create_link_ad_creative(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &CreateLinkAdCreativeRequest,
        _deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        let n = self.creative_creates.fetch_add(1, Ordering::SeqCst);
        Ok(CreatedAdCreative {
            site: self.site.clone(),
            account_id: request.account.clone().unwrap_or_else(|| "act_1".into()),
            id: format!("creative-{n}"),
        })
    }

    async fn create_video_ad_creative(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &crate::ads::CreateVideoAdCreativeRequest,
        _deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        Ok(CreatedAdCreative {
            site: self.site.clone(),
            account_id: request.account.clone().unwrap_or_else(|| "act_1".into()),
            id: "video-creative-0".into(),
        })
    }

    async fn create_ad_creative(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &crate::ads::CreateAdCreativeRequest,
        _deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        Ok(CreatedAdCreative {
            site: self.site.clone(),
            account_id: request.account.clone().unwrap_or_else(|| "act_1".into()),
            id: "typed-creative-0".into(),
        })
    }

    async fn preview_ad_creative(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &CreativePreviewRequest,
        _deadline: Deadline,
    ) -> Result<CreativePreview, Error> {
        let n = self.creative_previews.fetch_add(1, Ordering::SeqCst);
        if self.fail_auth_once && n == 0 {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "token_expired".into(),
            });
        }
        Ok(CreativePreview {
            site: self.site.clone(),
            creative_id: request.creative_id.clone(),
            ad_format: request.ad_format,
            body: format!("<iframe data-preview=\"{n}\"></iframe>"),
        })
    }

    async fn list_ads_inventory(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &AdsInventoryRequest,
        _deadline: Deadline,
    ) -> Result<AdsInventoryReply, Error> {
        Ok(AdsInventoryReply {
            site: self.site.clone(),
            account_id: request.account.clone().unwrap_or_else(|| "act_1".into()),
            kind: request.kind,
            items: vec![AdsInventoryItem {
                id: "100".into(),
                name: Some("Paused draft".into()),
                configured_status: Some("PAUSED".into()),
                effective_status: Some("PAUSED".into()),
                status: None,
                campaign_id: None,
                adset_id: None,
                objective: Some("OUTCOME_TRAFFIC".into()),
                object_type: None,
            }],
        })
    }

    async fn inspect_ads_object(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &AdsInspectRequest,
        _deadline: Deadline,
    ) -> Result<AdsInspectReply, Error> {
        Ok(AdsInspectReply {
            site: self.site.clone(),
            kind: request.kind,
            id: request.id.clone(),
            name: Some("Paused set".into()),
            configured_status: Some("PAUSED".into()),
            effective_status: Some("PAUSED".into()),
            status: None,
            daily_budget: self.inspect_daily_budget.clone(),
            lifetime_budget: self.inspect_lifetime_budget.clone(),
            bid_strategy: Some("LOWEST_COST_WITHOUT_CAP".into()),
            bid_amount: None,
            roas_average_floor: None,
            targeting: Some(AdsTargetingReadback {
                countries: vec!["MY".into()],
                ..AdsTargetingReadback::default()
            }),
            page_id: Some("111".into()),
            destination: Some("https://example.com".into()),
            destination_type: Some("WEBSITE".into()),
            call_to_action_type: None,
            product_set_id: None,
            instagram_user_id: None,
            whatsapp_identity_id: None,
            campaign_id: Some("100".into()),
            adset_id: None,
            creative_id: None,
            objective: None,
        })
    }

    async fn update_ad_status(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &AdsStatusUpdateRequest,
        _deadline: Deadline,
    ) -> Result<AdReviewStatus, Error> {
        Ok(AdReviewStatus {
            site: self.site.clone(),
            entity: request.entity,
            id: request.id.clone(),
            name: Some("Paused set".into()),
            configured_status: request.status.meta_value().into(),
            effective_status: request.status.meta_value().into(),
            issues: vec![],
        })
    }

    async fn duplicate_ad_object(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &crate::ads::AdsDuplicateRequest,
        _deadline: Deadline,
    ) -> Result<crate::ads::AdsDuplicateReply, Error> {
        Ok(crate::ads::AdsDuplicateReply {
            site: self.site.clone(),
            entity: request.entity,
            source_id: request.id.clone(),
            copied_id: "999".into(),
            status: "PAUSED".into(),
        })
    }

    async fn post_ad_update(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _id: &str,
        _fields: &[(String, String)],
        _deadline: Deadline,
    ) -> Result<(), Error> {
        Ok(())
    }

    async fn read_ad_targeting_json(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _id: &str,
        _deadline: Deadline,
    ) -> Result<serde_json::Value, Error> {
        Ok(serde_json::json!({
            "geo_locations": { "countries": ["MY"] },
            "publisher_platforms": ["facebook"]
        }))
    }

    async fn read_special_ad_categories(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _id: &str,
        _deadline: Deadline,
    ) -> Result<Vec<String>, Error> {
        Ok(vec![])
    }

    async fn ad_review_status(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &AdReviewStatusRequest,
        _deadline: Deadline,
    ) -> Result<AdReviewStatus, Error> {
        let n = self.review_status_reads.fetch_add(1, Ordering::SeqCst);
        let pending = n < self.review_status_pending_reads;
        Ok(AdReviewStatus {
            site: self.site.clone(),
            entity: request.entity,
            id: request.id.clone(),
            name: Some("Paused draft".into()),
            configured_status: "PAUSED".into(),
            effective_status: if pending {
                "PENDING_REVIEW".into()
            } else {
                "PAUSED".into()
            },
            issues: if pending {
                vec![AdReviewIssue {
                    code: Some("100".into()),
                    summary: Some("Review pending".into()),
                    message: None,
                    level: Some("WARNING".into()),
                }]
            } else {
                vec![]
            },
        })
    }
}

#[async_trait]
impl PageDirectory for MockPub {
    async fn pages(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _deadline: Deadline,
    ) -> Result<PagesReply, Error> {
        let read = self.page_reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_auth_once && read == 0 {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "token_expired".into(),
            });
        }
        Ok(PagesReply {
            site: self.site.clone(),
            pages: vec![PageAccount {
                id: "10".into(),
                name: Some("Test Page".into()),
                tasks: vec!["CREATE_CONTENT".into()],
            }],
        })
    }
}

#[async_trait]
impl MediaReader for MockPub {
    async fn media(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        query: &MediaQuery,
        _deadline: Deadline,
    ) -> Result<MediaReply, Error> {
        let read = self.media_reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_auth_once && read == 0 {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "token_expired".into(),
            });
        }
        Ok(MediaReply {
            site: self.site.clone(),
            media: vec![PublishedMedia {
                id: format!("recent-{}", query.limit),
                permalink: Some("https://example.test/recent".into()),
                caption: Some("test post".into()),
                media_type: Some("IMAGE".into()),
                timestamp: None,
            }],
        })
    }
}

#[cfg(feature = "whatsapp-cloud")]
#[async_trait]
impl WhatsAppSender for MockPub {
    async fn send_whatsapp(
        &self,
        app: &AppConfig,
        _creds: &AccountCreds,
        _request: &WhatsAppSendRequest,
        _deadline: Deadline,
    ) -> Result<Outcome, Error> {
        // Capture the selected config in the mock so sender-routing tests
        // prove the Client changes only the in-memory phone ID it passes to
        // the connector, never the saved primary configuration.
        if let Some(phone) = app.extra.get("phone_number_id").and_then(|v| v.as_str()) {
            self.whatsapp_phone_ids
                .lock()
                .expect("phone ids")
                .push(phone.to_string());
        }
        let index = self.whatsapp_sends.fetch_add(1, Ordering::SeqCst);
        Ok(Outcome {
            site: self.site.clone(),
            id: Some(format!("wamid-{index}")),
            url: None,
            limits: None,
        })
    }
}

/// Implements `Publisher` without overriding `probe` — the shape every
/// connector that has no create/publish split (Bluesky's createRecord is
/// atomic) keeps forever. Exercises the trait's *default* refusal.
pub(super) struct Bare {
    pub(super) caps: Vec<Capability>,
}

#[async_trait]
impl Publisher for Bare {
    fn site(&self) -> &Site {
        static SITE: std::sync::OnceLock<Site> = std::sync::OnceLock::new();
        SITE.get_or_init(|| Site::new("bluesky"))
    }
    fn capabilities(&self) -> &[Capability] {
        &self.caps
    }
    fn auth_kind(&self) -> AuthKind {
        AuthKind::AppPassword
    }
    async fn publish(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _intent: Intent,
        _deadline: Deadline,
    ) -> Result<Outcome, Error> {
        unreachable!("not under test")
    }
    async fn whoami(&self, _app: &AppConfig, _creds: &AccountCreds) -> Result<WhoAmI, Error> {
        Ok(WhoAmI {
            site: Site::new("bluesky"),
            id: "user-1".into(),
            handle: Some("tester".into()),
        })
    }
}

/// Attach every facet MockPub implements. Client still gates on
/// `capabilities()`, so a text-only mock will not grow insights by accident.
pub(super) fn register_mock(reg: &mut Registry, mock: Arc<MockPub>) {
    let connector = Connector::from_publisher(mock.clone())
        .insights(mock.clone())
        .ads(mock.clone())
        .pages(mock.clone())
        .media(mock.clone());
    #[cfg(feature = "whatsapp-cloud")]
    let connector = connector.whatsapp(mock);
    #[cfg(not(feature = "whatsapp-cloud"))]
    let _ = mock;
    reg.register_connector(connector);
}

/// A deliberately strict application policy used to prove Client calls the
/// policy before it looks up credentials or routes to a connector.
pub(super) struct DenyAds;

impl AdsPolicy for DenyAds {
    fn authorize(&self, site: &Site, action: AdsAction) -> Result<(), Error> {
        Err(Error::PolicyDenied {
            site: site.clone(),
            action: action.as_str().into(),
            reason: "test_denied".into(),
        })
    }
}

pub(super) fn setup(p: MockPub) -> (Client, AccountKey) {
    let mut reg = Registry::new();
    let site = p.site.clone();
    register_mock(&mut reg, Arc::new(p));
    let vault = Arc::new(MemoryVault::new());
    let apps = Arc::new(MemoryAppStore::new());
    let key = AccountKey::new(site.as_str(), "default");
    apps.put(&AppConfig {
        site: site.clone(),
        oauth: None,
        extra: serde_json::json!({}),
    })
    .unwrap();
    vault
        .put(
            &key,
            &AccountCreds::OAuth2 {
                access_token: "tok".into(),
                refresh_token: None,
                extra: serde_json::json!({}),
            },
        )
        .unwrap();
    (Client::new(reg, vault, apps), key)
}

pub(super) fn intent(site: &str, text: &str) -> Intent {
    Intent {
        site: Site::new(site),
        params: serde_json::json!({}),
        body: Body::Text { text: text.into() },
        idempotency_key: None,
    }
}

/// Client whose stored token expires in an hour — inside the 7-day window,
/// so every publish attempts a proactive refresh first.
pub(super) fn setup_expiring(p: MockPub) -> (Client, AccountKey) {
    let mut reg = Registry::new();
    let site = p.site.clone();
    register_mock(&mut reg, Arc::new(p));
    let vault = Arc::new(MemoryVault::new());
    let apps = Arc::new(MemoryAppStore::new());
    let key = AccountKey::new(site.as_str(), "default");
    apps.put(&AppConfig {
        site: site.clone(),
        oauth: None,
        extra: serde_json::json!({}),
    })
    .unwrap();
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    vault
        .put(
            &key,
            &AccountCreds::OAuth2 {
                access_token: "tok".into(),
                refresh_token: None,
                extra: serde_json::json!({ "expires_at": expires_at, "refreshed_at": 0 }),
            },
        )
        .unwrap();
    (Client::new(reg, vault, apps), key)
}

pub(super) fn intent_with_idem(site: &str, text: &str, idem: &str) -> Intent {
    Intent {
        idempotency_key: Some(idem.into()),
        ..intent(site, text)
    }
}

/// Shared scaffolding for the 024 deadline-threading tests: a client whose
/// publisher records the deadline its `refresh` received, plus stored
/// credentials that optionally sit inside the proactive-refresh window.
pub(super) fn setup_refresh_probe(
    mock: MockPub,
    expiring: bool,
) -> (Client, AccountKey, Arc<MockPub>) {
    let mock = Arc::new(mock);
    let mut reg = Registry::new();
    register_mock(&mut reg, mock.clone());
    let vault = Arc::new(MemoryVault::new());
    let key = AccountKey::new("threads", "default");
    let extra = if expiring {
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        serde_json::json!({ "expires_at": expires_at, "refreshed_at": 0 })
    } else {
        serde_json::json!({})
    };
    vault
        .put(
            &key,
            &AccountCreds::OAuth2 {
                access_token: "tok".into(),
                refresh_token: None,
                extra,
            },
        )
        .unwrap();
    (
        Client::new(reg, vault, Arc::new(MemoryAppStore::new())),
        key,
        mock,
    )
}

pub(super) fn insights_query(from: &str, to: &str) -> InsightsQuery {
    InsightsQuery {
        level: InsightsLevel::Campaign,
        metrics: vec![Metric::Spend],
        range: crate::insights::DateRange {
            from: from.into(),
            to: to.into(),
        },
        attribution: AttributionWindow::SevenDayClickOneDayView,
        account: None,
        entity_ids: vec![],
        breakdowns: vec![],
        report: crate::insights::InsightsReportKind::Performance,
    }
}

pub(super) fn paused_campaign_request(name: &str) -> CreatePausedAdRequest {
    CreatePausedAdRequest {
        account: Some("act_1".into()),
        create: PausedAdCreate::Campaign(PausedCampaign {
            name: name.into(),
            objective: CampaignObjective::Sales,
            special_ad_categories: vec![],
            daily_budget: None,
            lifetime_budget: None,
            is_adset_budget_sharing_enabled: false,
        }),
    }
}

pub(super) fn activate_request() -> AdsActivateRequest {
    AdsActivateRequest {
        entity: AdEntity::Adset,
        id: "456".into(),
        confirm_id: "456".into(),
        confirm_daily_budget: Some(500),
        confirm_lifetime_budget: None,
    }
}
