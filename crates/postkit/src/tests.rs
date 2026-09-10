use crate::ads::{
    AdEntity, AdPreviewFormat, AdReviewIssue, AdReviewStatus, AdReviewStatusRequest,
    CampaignObjective, CreateLinkAdCreativeRequest, CreatePausedAdRequest, CreatedAd,
    CreatedAdCreative, CreativePreview, CreativePreviewRequest, PausedAdCreate, PausedCampaign,
    UploadAdImageRequest, UploadedAdImage,
};
use crate::apps::{AppStore, MemoryAppStore};
use crate::client::Client;
use crate::error::Error;
use crate::insights::{
    AdAccount, AdAccountsReply, AttributionWindow, InsightRow, InsightsLevel, InsightsQuery,
    InsightsReply, Metric,
};
use crate::pages::{PageAccount, PagesReply};
use crate::policy::{AdsAction, AdsPolicy};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::registry::Registry;
use crate::types::{
    AccountCreds, AccountKey, AppConfig, Body, Capability, Deadline, Intent, Outcome, Probe, Site,
    WhoAmI,
};
use crate::vault::{MemoryVault, Vault};
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct MockPub {
    site: Site,
    caps: Vec<Capability>,
    auth_kind: AuthKind,
    fail_auth_once: bool,
    fail_publish: bool,
    whoami_fails: bool,
    refresh_network_err: bool,
    refresh_dead_session: bool,
    /// 023 concurrency scripting: `publish_started` fires when `publish`
    /// is entered and `publish_gate` parks it until released — together
    /// they hold one publish inside the claim window deterministically,
    /// without real timing races. Mutex'd because send/await consume the
    /// channels by value while the trait only lends `&self`.
    publish_started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    publish_gate: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    /// 024: the deadline refresh actually received — tests assert the
    /// caller's budget was threaded through, not a private 30s.
    refresh_deadline: std::sync::Mutex<Option<Deadline>>,
    publishes: AtomicUsize,
    probes: AtomicUsize,
    paused_creates: AtomicUsize,
    image_uploads: AtomicUsize,
    creative_creates: AtomicUsize,
    creative_previews: AtomicUsize,
    review_status_reads: AtomicUsize,
    page_reads: AtomicUsize,
    review_status_pending_reads: usize,
}

impl MockPub {
    fn text(site: &str) -> Self {
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
            review_status_pending_reads: 0,
        }
    }

    fn metrics(site: &str) -> Self {
        Self {
            caps: vec![Capability::ReadMetrics],
            ..Self::text(site)
        }
    }

    fn ad_accounts(site: &str) -> Self {
        Self {
            caps: vec![Capability::ReadAdAccounts],
            ..Self::text(site)
        }
    }

    fn pages(site: &str) -> Self {
        Self {
            caps: vec![Capability::ReadPages],
            ..Self::text(site)
        }
    }

    fn paused_ads(site: &str) -> Self {
        Self {
            caps: vec![Capability::CreatePausedAds],
            ..Self::text(site)
        }
    }

    fn creative_assets(site: &str) -> Self {
        Self {
            caps: vec![Capability::CreateAdCreative],
            ..Self::text(site)
        }
    }

    fn creative_previews(site: &str) -> Self {
        Self {
            caps: vec![Capability::ReadAdPreviews],
            ..Self::text(site)
        }
    }

    fn review_statuses(site: &str, pending_reads: usize) -> Self {
        Self {
            caps: vec![Capability::ReadAdReviewStatus],
            review_status_pending_reads: pending_reads,
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
        // The mock models one text publish and one image publish; the
        // distinct id keeps Client tests honest about which body routed.
        let label = match intent.body {
            Body::Text { text } => text,
            Body::Image { text, .. } => text.unwrap_or_else(|| "image".into()),
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

    async fn insights(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        query: &InsightsQuery,
        _deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
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

    async fn preview_ad_creative(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &CreativePreviewRequest,
        _deadline: Deadline,
    ) -> Result<CreativePreview, Error> {
        let n = self.creative_previews.fetch_add(1, Ordering::SeqCst);
        // Reuse the mock's one-shot expiry switch so this read path proves it
        // gets the same token-refresh recovery as existing insights reads.
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

    async fn ad_review_status(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        request: &AdReviewStatusRequest,
        _deadline: Deadline,
    ) -> Result<AdReviewStatus, Error> {
        let n = self.review_status_reads.fetch_add(1, Ordering::SeqCst);
        // The counter provides a deterministic PENDING_REVIEW → PAUSED
        // sequence, so the Client test proves polling reads repeatedly without
        // a real clock or Meta account.
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

/// A deliberately strict application policy used to prove Client calls the
/// policy before it looks up credentials or routes to a connector.
struct DenyAds;

impl AdsPolicy for DenyAds {
    fn authorize(&self, site: &Site, action: AdsAction) -> Result<(), Error> {
        Err(Error::PolicyDenied {
            site: site.clone(),
            action: action.as_str().into(),
            reason: "test_denied".into(),
        })
    }
}

fn setup(p: MockPub) -> (Client, AccountKey) {
    let mut reg = Registry::new();
    let site = p.site.clone();
    reg.register(Arc::new(p));
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

fn intent(site: &str, text: &str) -> Intent {
    Intent {
        site: Site::new(site),
        params: serde_json::json!({}),
        body: Body::Text { text: text.into() },
        idempotency_key: None,
    }
}

/// Client whose stored token expires in an hour — inside the 7-day window,
/// so every publish attempts a proactive refresh first.
fn setup_expiring(p: MockPub) -> (Client, AccountKey) {
    let mut reg = Registry::new();
    let site = p.site.clone();
    reg.register(Arc::new(p));
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

fn intent_with_idem(site: &str, text: &str, idem: &str) -> Intent {
    Intent {
        idempotency_key: Some(idem.into()),
        ..intent(site, text)
    }
}

/// Shared scaffolding for the 024 deadline-threading tests: a client whose
/// publisher records the deadline its `refresh` received, plus stored
/// credentials that optionally sit inside the proactive-refresh window.
fn setup_refresh_probe(mock: MockPub, expiring: bool) -> (Client, AccountKey, Arc<MockPub>) {
    let mock = Arc::new(mock);
    let mut reg = Registry::new();
    reg.register(mock.clone());
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

/// 024: the reactive token-expiry retry refreshes under the caller's
/// deadline — the same budget object, never a private fixed timeout.
#[tokio::test]
async fn reactive_refresh_shares_the_publish_deadline() {
    let (c, key, mock) = setup_refresh_probe(
        MockPub {
            fail_auth_once: true,
            ..MockPub::text("threads")
        },
        false,
    );
    let d = Deadline::from_secs(30);
    c.publish(&key, intent("threads", "hi"), d).await.unwrap();
    let got = mock.refresh_deadline.lock().unwrap();
    assert_eq!(
        got.unwrap().0,
        d.0,
        "refresh must receive the caller's budget"
    );
}

/// 024: the proactive pre-publish refresh runs under the caller's deadline
/// too — `--deadline` bounds refresh plus publish end-to-end.
#[tokio::test]
async fn proactive_refresh_shares_the_publish_deadline() {
    let (c, key, mock) = setup_refresh_probe(MockPub::text("threads"), true);
    let d = Deadline::from_secs(30);
    c.publish(&key, intent("threads", "hi"), d).await.unwrap();
    let got = mock.refresh_deadline.lock().unwrap();
    assert_eq!(
        got.unwrap().0,
        d.0,
        "refresh must receive the caller's budget"
    );
}

#[tokio::test]
async fn unknown_site() {
    let (c, key) = setup(MockPub::text("threads"));
    let err = c
        .publish(&key, intent("bluesky", "hi"), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidPost { reason, .. } if reason == "site_mismatch"));
}

#[tokio::test]
async fn unknown_site_registry() {
    let (c, _) = setup(MockPub::text("threads"));
    let key = AccountKey::new("bluesky", "default");
    let err = c
        .publish(&key, intent("bluesky", "hi"), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::UnknownSite(s) if s.as_str() == "bluesky"));
}

#[tokio::test]
async fn unknown_account() {
    let (c, _) = setup(MockPub::text("threads"));
    let key = AccountKey::new("threads", "work");
    let err = c
        .publish(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::UnknownAccount(k) if k.name == "work"));
}

#[tokio::test]
async fn publish_without_app_config() {
    let mut reg = Registry::new();
    reg.register(Arc::new(MockPub::text("threads")));
    let vault = Arc::new(MemoryVault::new());
    let apps = Arc::new(MemoryAppStore::new());
    let key = AccountKey::new("threads", "default");
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
    let c = Client::new(reg, vault, apps);
    let out = c
        .publish(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(out.id.as_deref(), Some("id-hi"));
}

#[tokio::test]
async fn happy_publish() {
    let (c, key) = setup(MockPub::text("threads"));
    let out = c
        .publish(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(out.id.as_deref(), Some("id-hi"));
    assert!(out.url.is_some());
}

#[tokio::test]
async fn retries_once_on_token_expired() {
    let mut p = MockPub::text("threads");
    p.fail_auth_once = true;
    let (c, key) = setup(p);
    let out = c
        .publish(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(out.id.as_deref(), Some("id-hi"));
}

#[tokio::test]
async fn auth_start_without_app_config() {
    let mut reg = Registry::new();
    reg.register(Arc::new(MockPub::text("bluesky")));
    let c = Client::new(
        reg,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let start = c.auth_start(&Site::new("bluesky")).await.unwrap();
    assert!(matches!(start, AuthStart::PasteInstructions { .. }));
}

#[tokio::test]
async fn auth_finish_without_app_config() {
    let mut reg = Registry::new();
    reg.register(Arc::new(MockPub::text("bluesky")));
    let vault = Arc::new(MemoryVault::new());
    let c = Client::new(reg, vault.clone(), Arc::new(MemoryAppStore::new()));
    let key = AccountKey::new("bluesky", "you.bsky.social");
    let me = c
        .auth_finish(
            &key,
            AuthReply::AppPassword {
                identifier: "you.bsky.social".into(),
                secret: "xxxx-xxxx".into(),
                pds: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(me.handle.as_deref(), Some("tester"));
    match vault.get(&key).unwrap() {
        AccountCreds::AppPassword { identifier, .. } => {
            assert_eq!(identifier, "you.bsky.social");
        }
        other => panic!("{other:?}"),
    }
}

/// Implements `Publisher` without overriding `probe` — the shape every
/// connector that has no create/publish split (Bluesky's createRecord is
/// atomic) keeps forever. Exercises the trait's *default* refusal.
struct Bare {
    caps: Vec<Capability>,
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

/// 027: the default `probe` must refuse — never fall through to a real
/// publish — on sites with no creation/publication split.
#[tokio::test]
async fn default_probe_refuses_instead_of_publishing() {
    let mut reg = Registry::new();
    reg.register(Arc::new(Bare {
        caps: vec![Capability::PublishText],
    }));
    let vault = Arc::new(MemoryVault::new());
    let key = AccountKey::new("bluesky", "default");
    vault
        .put(
            &key,
            &AccountCreds::AppPassword {
                identifier: "you.bsky.social".into(),
                secret: "xxxx".into(),
                pds: None,
            },
        )
        .unwrap();
    let c = Client::new(reg, vault, Arc::new(MemoryAppStore::new()));
    let err = c
        .probe(&key, intent("bluesky", "hi"), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidPost { ref reason, .. } if reason == "dry_run_unsupported")
    );
}

/// Page discovery defaults to the same fail-closed posture as probes: a
/// connector must implement the exact token-scrubbing contract before it can
/// expose this remote list.
#[tokio::test]
async fn default_pages_refuses_without_an_explicit_connector_method() {
    let publisher = Bare {
        caps: vec![Capability::PublishText],
    };
    let error = publisher
        .pages(
            &AppConfig {
                site: Site::new("bluesky"),
                oauth: None,
                extra: serde_json::json!({}),
            },
            &AccountCreds::BotToken {
                token: "not-used".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::UnsupportedCapability { need, .. } if need == Capability::ReadPages
    ));
}

/// 027: a probe runs the capability gate exactly like a publish.
#[tokio::test]
async fn probe_checks_capability() {
    let mut reg = Registry::new();
    reg.register(Arc::new(Bare { caps: vec![] }));
    let key = AccountKey::new("bluesky", "default");
    let c = Client::new(
        reg,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let err = c
        .probe(&key, intent("bluesky", "hi"), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::UnsupportedCapability { .. }));
}

/// 027: probes never read or write the idempotency ledger. A stored
/// publish outcome must not silence a probe, and a probe must not make a
/// later publish "succeed" by replaying the probe's result.
#[tokio::test]
async fn probe_neither_reads_nor_writes_the_idempotency_ledger() {
    let mut reg = Registry::new();
    reg.register(Arc::new(MockPub::text("threads")));
    let vault = Arc::new(MemoryVault::new());
    let key = AccountKey::new("threads", "default");
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
    // an old completed publish under the same idempotency key
    let seeded = Outcome {
        site: Site::new("threads"),
        id: Some("old-post".into()),
        url: Some("https://example.test/old".into()),
        limits: None,
    };
    vault.put_outcome(&key, "k", &seeded).unwrap();
    let c = Client::new(reg, vault.clone(), Arc::new(MemoryAppStore::new()));

    // read side: the probe ignores the ledger and answers from the platform
    let probe = c
        .probe(
            &key,
            intent_with_idem("threads", "hi", "k"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(probe.container_id, "container-hi");

    // write side: the ledger still replays the *old publish*, not the probe
    let replay = c
        .publish(
            &key,
            intent_with_idem("threads", "hi", "k"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(replay.id.as_deref(), Some("old-post"));
}

/// 027: the reactive token_expired → refresh → retry mapping applies to
/// probes too — a refreshable token must not read as "broken".
#[tokio::test]
async fn probe_retries_once_on_token_expired() {
    let mut p = MockPub::text("threads");
    p.fail_auth_once = true;
    let (c, key) = setup(p);
    let probe = c
        .probe(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(probe.container_id, "container-hi");
}

#[tokio::test]
async fn put_token_then_whoami() {
    let mut reg = Registry::new();
    reg.register(Arc::new(MockPub::text("threads")));
    let c = Client::new(
        reg,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let key = AccountKey::new("threads", "default");
    let me = c.put_token(&key, "THQVJ").await.unwrap();
    assert_eq!(me.id, "user-1");
}

#[tokio::test]
async fn put_token_persists_whoami_id() {
    // The id that whoami already fetches lands in extra — same shape as
    // the OAuth path — so both auth flows publish against /{user_id}/….
    let mut reg = Registry::new();
    reg.register(Arc::new(MockPub::text("threads")));
    let vault = Arc::new(MemoryVault::new());
    let c = Client::new(reg, vault.clone(), Arc::new(MemoryAppStore::new()));
    let key = AccountKey::new("threads", "default");
    let me = c.put_token(&key, "THQVJ").await.unwrap();
    assert_eq!(me.id, "user-1");
    match vault.get(&key).unwrap() {
        AccountCreds::OAuth2 { extra, .. } => {
            assert_eq!(
                extra.get("user_id").and_then(|v| v.as_str()),
                Some("user-1")
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn put_token_rejects_bad_token_before_vault_write() {
    // whoami verifies before the store: an invalid token never lands in
    // the vault to shadow the next publish.
    let mut p = MockPub::text("threads");
    p.whoami_fails = true;
    let mut reg = Registry::new();
    reg.register(Arc::new(p));
    let vault = Arc::new(MemoryVault::new());
    let c = Client::new(reg, vault.clone(), Arc::new(MemoryAppStore::new()));
    let key = AccountKey::new("threads", "default");
    let err = c.put_token(&key, "bogus").await.unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "invalid_token"));
    assert!(
        matches!(vault.get(&key), Err(Error::UnknownAccount(_))),
        "an unverified token must not be stored"
    );
}

#[tokio::test]
async fn put_token_refused_for_app_password_sites() {
    // `auth bluesky --token x` used to store OAuth2 creds that publish
    // rejected much later; the guard must refuse at the door and leave the
    // vault untouched.
    let mut mock = MockPub::text("bluesky");
    mock.auth_kind = AuthKind::AppPassword;
    let mut reg = Registry::new();
    reg.register(Arc::new(mock));
    let vault = Arc::new(MemoryVault::new());
    let c = Client::new(reg, vault.clone(), Arc::new(MemoryAppStore::new()));
    let key = AccountKey::new("bluesky", "you.bsky.social");
    let err = c.put_token(&key, "whatever").await.unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "token_bootstrap_unsupported"));
    assert!(
        matches!(vault.get(&key), Err(Error::UnknownAccount(_))),
        "no creds may be written on refusal"
    );
}

/// 023: while one publish under a key is in flight, a second caller with
/// the same key gets a distinct transient error and the connector is hit
/// exactly once. The gate holds the first publish inside the claim window
/// deterministically — no timing race, the parked publish *is* the window.
#[tokio::test]
async fn concurrent_same_key_publishes_once() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
    let mut mock = MockPub::text("threads");
    mock.publish_started = std::sync::Mutex::new(Some(started_tx));
    mock.publish_gate = std::sync::Mutex::new(Some(gate_rx));
    let mock = Arc::new(mock);
    let mut reg = Registry::new();
    reg.register(mock.clone());
    let vault = Arc::new(MemoryVault::new());
    let key = AccountKey::new("threads", "default");
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
    let c = Arc::new(Client::new(
        reg,
        vault.clone(),
        Arc::new(MemoryAppStore::new()),
    ));

    let first = {
        let c = c.clone();
        let key = key.clone();
        tokio::spawn(async move {
            c.publish(
                &key,
                intent_with_idem("threads", "hi", "k"),
                Deadline::from_secs(30),
            )
            .await
        })
    };
    // Wait until the first publish is inside the connector (claim held).
    started_rx.await.unwrap();
    // Second caller with the same key: refused while the first is in
    // flight, transient enough to retry (wire exit 4), never a duplicate.
    let err = c
        .publish(
            &key,
            intent_with_idem("threads", "hi", "k"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::IdempotencyInFlight { key: k, .. } if k == "k"),
        "got {err:?}"
    );
    assert_eq!(err.exit_code(), 4);
    assert_eq!(mock.publishes.load(Ordering::SeqCst), 1);
    // Release the gate: the first completes, records, and releases; a
    // retry with the same key now replays the stored outcome.
    gate_tx.send(()).unwrap();
    let out = first.await.unwrap().unwrap();
    assert_eq!(out.id.as_deref(), Some("id-hi"));
    let retry = c
        .publish(
            &key,
            intent_with_idem("threads", "hi", "k"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(retry.id, out.id);
    assert_eq!(mock.publishes.load(Ordering::SeqCst), 1);
}

/// 023: a failed attempt must release the claim — the key stays retryable.
#[tokio::test]
async fn failed_publish_releases_the_claim() {
    let mock = Arc::new(MockPub {
        fail_publish: true,
        ..MockPub::text("threads")
    });
    let mut reg = Registry::new();
    reg.register(mock.clone());
    let vault = Arc::new(MemoryVault::new());
    let key = AccountKey::new("threads", "default");
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
    let c = Client::new(reg, vault, Arc::new(MemoryAppStore::new()));
    let d = Deadline::from_secs(30);

    let err = c
        .publish(&key, intent_with_idem("threads", "hi", "k"), d)
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Platform { ref code, .. } if code == "boom"),
        "got {err:?}"
    );
    // A leaked claim would answer IdempotencyInFlight here; the retry
    // must reach the connector again.
    let err2 = c
        .publish(&key, intent_with_idem("threads", "hi", "k"), d)
        .await
        .unwrap_err();
    assert!(
        matches!(err2, Error::Platform { ref code, .. } if code == "boom"),
        "claim leaked: {err2:?}"
    );
    assert_eq!(mock.publishes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn idempotency_retry_returns_stored_outcome() {
    // Same key twice: exactly one connector publish; the retry replays
    // the stored Outcome without HTTP.
    let mock = Arc::new(MockPub::text("threads"));
    let mut reg = Registry::new();
    reg.register(mock.clone());
    let vault = Arc::new(MemoryVault::new());
    let apps = Arc::new(MemoryAppStore::new());
    let key = AccountKey::new("threads", "default");
    apps.put(&AppConfig {
        site: Site::new("threads"),
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
    let c = Client::new(reg, vault, apps);
    let d = Deadline::from_secs(30);

    let out1 = c
        .publish(&key, intent_with_idem("threads", "hi", "k"), d)
        .await
        .unwrap();
    // retry with the same key — even with different text — must not republish
    let out2 = c
        .publish(&key, intent_with_idem("threads", "CHANGED", "k"), d)
        .await
        .unwrap();
    assert_eq!(out1.id, out2.id);
    assert_eq!(mock.publishes.load(Ordering::SeqCst), 1);

    // a different key posts again; no key, no dedupe
    c.publish(&key, intent_with_idem("threads", "hi", "k2"), d)
        .await
        .unwrap();
    c.publish(&key, intent("threads", "plain"), d)
        .await
        .unwrap();
    assert_eq!(mock.publishes.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn failed_publish_is_not_recorded() {
    // A failed attempt must stay retryable: nothing enters the ledger.
    let mut p = MockPub::text("threads");
    p.fail_publish = true;
    let (c, key) = setup(p);
    let err = c
        .publish(
            &key,
            intent_with_idem("threads", "hi", "k"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Platform { .. }));
    assert!(
        c.vault().get_outcome(&key, "k").unwrap().is_none(),
        "ledger must stay empty after a failure"
    );
}

#[tokio::test]
async fn transient_refresh_failure_still_publishes() {
    // Proactive refresh is an optimization: the stored token is still
    // valid for another hour, so a network failure on the refresh endpoint
    // must degrade to publishing with the current token, not abort it.
    let mut p = MockPub::text("threads");
    p.refresh_network_err = true;
    let (c, key) = setup_expiring(p);
    let out = c
        .publish(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(out.id.as_deref(), Some("id-hi"));
}

#[tokio::test]
async fn dead_session_refresh_fails_fast() {
    // A refresh rejected by the platform means the session is dead;
    // failing fast with the auth error beats dying later inside publish.
    let mut p = MockPub::text("threads");
    p.refresh_dead_session = true;
    let (c, key) = setup_expiring(p);
    let err = c
        .publish(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "session_expired"));
}

#[tokio::test]
async fn wire_error_no_ok_field() {
    let err = Error::InvalidPost {
        site: Site::new("threads"),
        reason: "text_too_long".into(),
        limit: Some(500),
    };
    let v = serde_json::to_value(crate::WireError::from(&err)).unwrap();
    assert_eq!(v["error"], "invalid_post");
    assert!(v.get("ok").is_none());
}

#[tokio::test]
async fn secrets_debug_redacted() {
    let creds = AccountCreds::OAuth2 {
        access_token: "secret-token".into(),
        refresh_token: Some("r".into()),
        extra: serde_json::json!({}),
    };
    let d = format!("{creds:?}");
    assert!(!d.contains("secret-token"));
    assert!(d.contains("[redacted]"));
}

fn insights_query(from: &str, to: &str) -> InsightsQuery {
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
    }
}

fn paused_campaign_request(name: &str) -> CreatePausedAdRequest {
    CreatePausedAdRequest {
        account: Some("act_1".into()),
        create: PausedAdCreate::Campaign(PausedCampaign {
            name: name.into(),
            objective: CampaignObjective::Sales,
            special_ad_categories: vec![],
        }),
    }
}

/// 026 read seam: Client routes the query to the connector, and the
/// capability gate turns a publish-only site away before any HTTP.
#[tokio::test]
async fn client_insights_routes_and_checks_capability() {
    let (c, key) = setup(MockPub::metrics("meta_ads"));
    let reply = c
        .insights(
            &key,
            insights_query("2026-06-01", "2026-06-02"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(reply.account_id, "act_1");
    assert_eq!(reply.rows.len(), 1);

    // a site without read.metrics is refused before reaching the connector
    let (text_only, key) = setup(MockPub::text("meta_ads"));
    let err = text_only
        .insights(
            &key,
            insights_query("2026-06-01", "2026-06-02"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadMetrics)
    );
}

/// Remote ad-account discovery is a separately gated read. This prevents a
/// connector from accidentally treating local vault aliases as account IDs.
#[tokio::test]
async fn client_ad_accounts_routes_and_checks_capability() {
    let (c, key) = setup(MockPub::ad_accounts("meta_ads"));
    let reply = c.ad_accounts(&key, Deadline::from_secs(30)).await.unwrap();
    assert_eq!(reply.accounts[0].id, "act_1");
    assert_eq!(reply.accounts[0].name.as_deref(), Some("Main"));

    let (text_only, key) = setup(MockPub::text("meta_ads"));
    let err = text_only
        .ad_accounts(&key, Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadAdAccounts)
    );
}

/// Page discovery is its own capability: a social publisher does not gain an
/// account-listing read merely by supporting a text body. The mock expires on
/// its first read so this also proves the Client's one refresh/retry rule.
#[tokio::test]
async fn client_pages_routes_retries_expired_token_and_checks_capability() {
    let mut publisher = MockPub::pages("facebook_pages");
    publisher.fail_auth_once = true;
    let (client, key) = setup(publisher);
    let reply = client.pages(&key, Deadline::from_secs(30)).await.unwrap();
    assert_eq!(reply.pages[0].id, "10");
    assert_eq!(reply.pages[0].tasks, vec!["CREATE_CONTENT"]);

    // No credential is installed here. `ReadPages` must refuse at the
    // capability gate first, rather than leaking an unrelated
    // `unknown_account` and making the caller provision a token for a site
    // that cannot list Pages anyway.
    let mut registry = Registry::new();
    registry.register(Arc::new(MockPub::text("facebook_pages")));
    let client = Client::new(
        registry,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let key = AccountKey::new("facebook_pages", "default");
    let error = client
        .pages(&key, Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::UnsupportedCapability { need, .. } if need == Capability::ReadPages
    ));
}

/// Tier B follows the same capability routing discipline as reads, with an
/// extra policy gate before it can touch a credential or issue a write.
#[tokio::test]
async fn client_paused_create_routes_and_refuses_before_vault_access() {
    let (client, key) = setup(MockPub::paused_ads("meta_ads"));
    let created = client
        .create_paused_ad(
            &key,
            paused_campaign_request("draft"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(created.entity.as_str(), "campaign");
    assert_eq!(created.status, "PAUSED");

    let (no_management, key) = setup(MockPub::text("meta_ads"));
    let err = no_management
        .create_paused_ad(
            &key,
            paused_campaign_request("draft"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::CreatePausedAds)
    );

    // Deliberately leave the vault empty. Policy denial must win over an
    // `unknown_account` error, proving the gate is before credential access.
    let mut registry = Registry::new();
    registry.register(Arc::new(MockPub::paused_ads("meta_ads")));
    let denied = Client::with_ads_policy(
        registry,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
        Arc::new(DenyAds),
    )
    .create_paused_ad(
        &AccountKey::new("meta_ads", "default"),
        paused_campaign_request("draft"),
        Deadline::from_secs(30),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(denied, Error::PolicyDenied { action, reason, .. } if action == "create_paused_campaign" && reason == "test_denied")
    );

    // Validation is also local: a malformed draft cannot reach vault lookup
    // or HTTP, even under the normal paused-only policy.
    let empty_vault = Client::new(
        Registry::new(),
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let err = empty_vault
        .create_paused_ad(
            &AccountKey::new("meta_ads", "default"),
            paused_campaign_request("   "),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidQuery { reason, .. } if reason == "missing_name"));
}

/// Creative assets use their own capability and policy labels, while keeping
/// the same "validate and authorize before vault" invariant as paused ads.
#[tokio::test]
async fn client_creative_assets_route_and_refuse_before_vault_access() {
    let (client, key) = setup(MockPub::creative_assets("meta_ads"));
    let uploaded = client
        .upload_ad_image(
            &key,
            UploadAdImageRequest {
                account: None,
                filename: "hero.png".into(),
                bytes: b"image bytes".to_vec(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(uploaded.hash, "image-0");

    let created = client
        .create_link_ad_creative(
            &key,
            CreateLinkAdCreativeRequest {
                account: None,
                creative: crate::ads::LinkAdCreative {
                    name: "Hero".into(),
                    page_id: "456".into(),
                    image_hash: uploaded.hash,
                    message: "A clear benefit".into(),
                    headline: "Learn more".into(),
                    destination_url: "https://example.com/offer".into(),
                    call_to_action: crate::ads::LinkCallToAction::LearnMore,
                },
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(created.id, "creative-0");

    let (no_creative_capability, key) = setup(MockPub::text("meta_ads"));
    let err = no_creative_capability
        .upload_ad_image(
            &key,
            UploadAdImageRequest {
                account: None,
                filename: "hero.png".into(),
                bytes: b"image bytes".to_vec(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::CreateAdCreative)
    );

    // The vault is empty on purpose. A stricter policy must reject before
    // token lookup, proving an asset write cannot cause a credential side
    // effect when an embedding application disallows it.
    let mut registry = Registry::new();
    registry.register(Arc::new(MockPub::creative_assets("meta_ads")));
    let denied = Client::with_ads_policy(
        registry,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
        Arc::new(DenyAds),
    )
    .upload_ad_image(
        &AccountKey::new("meta_ads", "default"),
        UploadAdImageRequest {
            account: None,
            filename: "hero.png".into(),
            bytes: b"image bytes".to_vec(),
        },
        Deadline::from_secs(30),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(denied, Error::PolicyDenied { action, reason, .. } if action == "upload_ad_image" && reason == "test_denied")
    );

    // Validation also wins before registry/vault lookup, which avoids local
    // filesystem details and network behavior hiding a bad destination URL.
    let empty_vault = Client::new(
        Registry::new(),
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let err = empty_vault
        .create_link_ad_creative(
            &AccountKey::new("meta_ads", "default"),
            CreateLinkAdCreativeRequest {
                account: None,
                creative: crate::ads::LinkAdCreative {
                    name: "Hero".into(),
                    page_id: "456".into(),
                    image_hash: "hash".into(),
                    message: "Copy".into(),
                    headline: "Headline".into(),
                    destination_url: "http://example.com".into(),
                    call_to_action: crate::ads::LinkCallToAction::LearnMore,
                },
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { reason, .. } if reason == "destination_url_must_be_https")
    );
}

/// Previewing is a separately declared read: it neither reuses a creative
/// write capability nor touches `AdsPolicy`, because a GET cannot change an
/// auction, draft, budget, or payment state.
#[tokio::test]
async fn client_creative_preview_routes_validates_and_retries_expired_tokens() {
    let mut preview_connector = MockPub::creative_previews("meta_ads");
    preview_connector.fail_auth_once = true;
    let (client, key) = setup(preview_connector);
    let preview = client
        .preview_ad_creative(
            &key,
            CreativePreviewRequest {
                creative_id: "123".into(),
                ad_format: AdPreviewFormat::DesktopFeedStandard,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(preview.creative_id, "123");
    assert_eq!(preview.ad_format, AdPreviewFormat::DesktopFeedStandard);
    assert!(preview.body.contains("data-preview=\"1\""));

    let (no_preview_capability, key) = setup(MockPub::text("meta_ads"));
    let err = no_preview_capability
        .preview_ad_creative(
            &key,
            CreativePreviewRequest {
                creative_id: "123".into(),
                ad_format: AdPreviewFormat::MobileFeedStandard,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadAdPreviews)
    );

    // Request validation is the first operation. An invalid creative ID must
    // not reveal whether a vault alias exists or attempt a connector read.
    let empty = Client::new(
        Registry::new(),
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let err = empty
        .preview_ad_creative(
            &AccountKey::new("meta_ads", "default"),
            CreativePreviewRequest {
                creative_id: "bad-id".into(),
                ad_format: AdPreviewFormat::DesktopFeedStandard,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { reason, .. } if reason == "bad_creative_id:bad-id")
    );
}

/// Review status is a separately declared, GET-only capability. It validates
/// before vault access and represents Meta's transitional state explicitly;
/// `PENDING_REVIEW` is never mistaken for an active delivery request.
#[tokio::test]
async fn client_ad_review_status_routes_validates_and_checks_capability() {
    let (client, key) = setup(MockPub::review_statuses("meta_ads", 1));
    let status = client
        .ad_review_status(
            &key,
            AdReviewStatusRequest {
                entity: AdEntity::Ad,
                id: "123".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert!(status.is_pending_review());
    assert_eq!(status.configured_status, "PAUSED");
    assert_eq!(status.issues[0].summary.as_deref(), Some("Review pending"));

    let (no_status_capability, key) = setup(MockPub::text("meta_ads"));
    let err = no_status_capability
        .ad_review_status(
            &key,
            AdReviewStatusRequest {
                entity: AdEntity::Adset,
                id: "123".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadAdReviewStatus)
    );

    let empty = Client::new(
        Registry::new(),
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let err = empty
        .ad_review_status(
            &AccountKey::new("meta_ads", "default"),
            AdReviewStatusRequest {
                entity: AdEntity::Campaign,
                id: "bad-id".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { reason, .. } if reason == "bad_ad_entity_id:bad-id")
    );
}

/// A poller is useful only if it stops on the platform's final state. The
/// short test interval is private to the Client test; production uses the
/// conservative two-second interval and always honors the global deadline.
#[cfg(feature = "meta-ads")]
#[tokio::test]
async fn client_ad_review_wait_polls_pending_status_until_settled() {
    let (client, key) = setup(MockPub::review_statuses("meta_ads", 1));
    let result = client
        .wait_for_ad_review_with_interval(
            &key,
            AdReviewStatusRequest {
                entity: AdEntity::Ad,
                id: "123".into(),
            },
            Deadline::from_secs(1),
            std::time::Duration::from_millis(1),
        )
        .await
        .unwrap();
    assert!(matches!(
        result,
        crate::ads::AdReviewWait::Settled(AdReviewStatus { effective_status, .. })
            if effective_status == "PAUSED"
    ));
}

/// The trait's default `insights` must refuse — the same honesty the
/// default `probe` keeps: a connector that never implemented reads cannot
/// let one slip through as something else.
#[tokio::test]
async fn default_insights_refuses() {
    let mut reg = Registry::new();
    reg.register(Arc::new(Bare {
        caps: vec![Capability::PublishText],
    }));
    let vault = Arc::new(MemoryVault::new());
    let apps = Arc::new(MemoryAppStore::new());
    let key = AccountKey::new("bluesky", "default");
    vault
        .put(
            &key,
            &AccountCreds::AppPassword {
                identifier: "you".into(),
                secret: "x".into(),
                pds: None,
            },
        )
        .unwrap();
    let c = Client::new(reg, vault, apps);
    let err = c
        .insights(
            &key,
            insights_query("2026-06-01", "2026-06-02"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadMetrics)
    );
}

/// Range bounds are enforced in the kernel, not just the CLI: a 91-day
/// query is invalid no matter which caller built it.
#[tokio::test]
async fn insights_range_validated_before_any_routing() {
    let (c, key) = setup(MockPub::metrics("meta_ads"));
    let err = c
        .insights(
            &key,
            insights_query("2026-01-01", "2026-04-01"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidQuery { reason, .. } if reason == "range_too_long:91"));
}

#[test]
fn image_form_validation_is_local_and_stable() {
    use crate::types::Image;
    let ok = Image::Bytes {
        filename: "hero.png".into(),
        bytes: b"x".to_vec(),
    };
    ok.validate().unwrap();
    assert_eq!(
        Image::Bytes {
            filename: "a/hero.png".into(),
            bytes: b"x".to_vec()
        }
        .validate()
        .unwrap_err(),
        "invalid_image_filename"
    );
    assert_eq!(
        Image::Bytes {
            filename: "hero.png".into(),
            bytes: vec![]
        }
        .validate()
        .unwrap_err(),
        "image_file_empty"
    );
    assert_eq!(
        Image::Url("http://cdn.test/h.png".into())
            .validate()
            .unwrap_err(),
        "image_url_must_be_https"
    );
    assert_eq!(
        Image::Url("https:///no-authority".into())
            .validate()
            .unwrap_err(),
        "image_url_must_be_https"
    );
    Image::Url("https://cdn.test/h.png".into())
        .validate()
        .unwrap();
}

#[tokio::test]
async fn client_routes_image_bodies_by_capability() {
    // A text-only publisher must refuse an image body at the capability
    // check, before the vault is read — the same door every body faces.
    let (c, key) = setup(MockPub::text("threads"));
    let intent = Intent {
        site: Site::new("threads"),
        params: serde_json::json!({}),
        body: crate::types::Body::Image {
            text: None,
            image: crate::types::Image::Url("https://cdn.test/h.png".into()),
            alt: String::new(),
        },
        idempotency_key: None,
    };
    let err = c
        .publish(&key, intent, Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        Error::UnsupportedCapability {
            need: crate::types::Capability::PublishImage,
            ..
        }
    ));
}

/// Draft orchestration (plans/001/013). A dedicated publisher mock (not the
/// shared `MockPub`) because these tests script *failures at exact write
/// indexes* — the write-ahead `in_flight` protocol's behavior depends on
/// which write died and how — and an in-memory `DraftStore` so checkpoint
/// atomicity, duplicate refusal, and locking are asserted without disk.
#[cfg(feature = "draft")]
mod draft_tests {
    use crate::ads::{
        AdReviewStatus, AdReviewStatusRequest, CreateLinkAdCreativeRequest, CreatePausedAdRequest,
        CreatedAd, CreatedAdCreative, CreativePreview, CreativePreviewRequest,
        UploadAdImageRequest, UploadedAdImage,
    };
    use crate::apps::MemoryAppStore;
    use crate::client::Client;
    use crate::draft::{
        DraftImage, DraftStage, DraftStep, DraftStore, PausedDraftManifest, PausedDraftResult,
        PausedDraftState, RunPausedDraft,
    };
    use crate::error::Error;
    use crate::policy::{AdsAction, AdsPolicy};
    use crate::publisher::{AuthKind, Publisher};
    use crate::registry::Registry;
    use crate::types::{AccountCreds, AccountKey, AppConfig, Capability, Deadline, Site, WhoAmI};
    use crate::vault::{MemoryVault, Vault};
    use async_trait::async_trait;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// How the Nth remote write (global index: upload=0, campaign=1, adset=2,
    /// creative=3, ad=4) fails. Network/timeout model an *ambiguous* write
    /// (no HTTP response); Platform models a definitive Meta rejection.
    #[derive(Clone, Copy, Debug)]
    enum FailAt {
        Network(usize),
        Platform(usize),
    }

    struct DraftMock {
        site: Site,
        /// Global write counter across all ads writes.
        writes: AtomicUsize,
        uploads: AtomicUsize,
        campaigns: AtomicUsize,
        adsets: AtomicUsize,
        creatives: AtomicUsize,
        ads: AtomicUsize,
        fail_at: Mutex<Option<FailAt>>,
        /// configured_status per review ID (default PAUSED) — drives the
        /// adoption validation tests.
        review_configured: Mutex<HashMap<String, String>>,
        /// While > 0, ad_review_status reports effective PENDING_REVIEW and
        /// counts down — models Meta's fresh-object review latency for the
        /// bounded-wait tests. usize::MAX means "never settles".
        review_pending_reads: AtomicUsize,
    }

    impl DraftMock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                site: Site::new("meta_ads"),
                writes: AtomicUsize::new(0),
                uploads: AtomicUsize::new(0),
                campaigns: AtomicUsize::new(0),
                adsets: AtomicUsize::new(0),
                creatives: AtomicUsize::new(0),
                ads: AtomicUsize::new(0),
                fail_at: Mutex::new(None),
                review_configured: Mutex::new(HashMap::new()),
                review_pending_reads: AtomicUsize::new(0),
            })
        }

        /// Script the Nth remote write's failure. Mutable through the Arc so
        /// the registered mock and the test's assertions share one counter
        /// set — a "copy with failure" mock would silently fork them.
        fn fail(&self, at: FailAt) {
            *self.fail_at.lock().unwrap() = Some(at);
        }

        fn pending_reviews(&self, n: usize) {
            self.review_pending_reads.store(n, Ordering::SeqCst);
        }

        /// One write attempt: consumes the global index and answers the
        /// scripted failure if this is the doomed one.
        fn write_gate(&self) -> Result<(), Error> {
            let n = self.writes.fetch_add(1, Ordering::SeqCst);
            match *self.fail_at.lock().unwrap() {
                Some(FailAt::Network(i)) if i == n => Err(Error::Network {
                    site: self.site.clone(),
                    message: "connection reset".into(),
                }),
                Some(FailAt::Platform(i)) if i == n => Err(Error::Platform {
                    site: self.site.clone(),
                    code: "100".into(),
                    message: "invalid parameter".into(),
                }),
                _ => Ok(()),
            }
        }
    }

    #[async_trait]
    impl Publisher for DraftMock {
        fn site(&self) -> &Site {
            &self.site
        }
        fn capabilities(&self) -> &[Capability] {
            &[
                Capability::CreatePausedAds,
                Capability::CreateAdCreative,
                Capability::ReadAdReviewStatus,
            ]
        }
        fn auth_kind(&self) -> AuthKind {
            AuthKind::OAuth2AuthCode
        }
        async fn publish(
            &self,
            _app: &AppConfig,
            _creds: &AccountCreds,
            _intent: crate::types::Intent,
            _deadline: Deadline,
        ) -> Result<crate::types::Outcome, Error> {
            Err(Error::UnsupportedCapability {
                site: self.site.clone(),
                need: Capability::PublishText,
            })
        }
        async fn whoami(&self, _app: &AppConfig, _creds: &AccountCreds) -> Result<WhoAmI, Error> {
            Ok(WhoAmI {
                site: self.site.clone(),
                id: "1000".into(),
                handle: Some("operator".into()),
            })
        }
        async fn create_paused_ad(
            &self,
            _app: &AppConfig,
            _creds: &AccountCreds,
            request: &CreatePausedAdRequest,
            _deadline: Deadline,
        ) -> Result<CreatedAd, Error> {
            self.write_gate()?;
            let (entity, id): (&str, String) = match &request.create {
                crate::ads::PausedAdCreate::Campaign(_) => (
                    "campaign",
                    format!("1{:03}", self.campaigns.fetch_add(1, Ordering::SeqCst)),
                ),
                crate::ads::PausedAdCreate::Adset(_) => (
                    "adset",
                    format!("2{:03}", self.adsets.fetch_add(1, Ordering::SeqCst)),
                ),
                crate::ads::PausedAdCreate::Ad(_) => (
                    "ad",
                    format!("4{:03}", self.ads.fetch_add(1, Ordering::SeqCst)),
                ),
            };
            Ok(CreatedAd {
                site: self.site.clone(),
                account_id: request.account.clone().unwrap_or_else(|| "act_777".into()),
                entity: entity.parse().unwrap(),
                id,
                status: "PAUSED".into(),
            })
        }
        async fn upload_ad_image(
            &self,
            _app: &AppConfig,
            _creds: &AccountCreds,
            _request: &UploadAdImageRequest,
            _deadline: Deadline,
        ) -> Result<UploadedAdImage, Error> {
            self.write_gate()?;
            Ok(UploadedAdImage {
                site: self.site.clone(),
                account_id: "act_777".into(),
                hash: format!("img-{}", self.uploads.fetch_add(1, Ordering::SeqCst)),
            })
        }
        async fn create_link_ad_creative(
            &self,
            _app: &AppConfig,
            _creds: &AccountCreds,
            _request: &CreateLinkAdCreativeRequest,
            _deadline: Deadline,
        ) -> Result<CreatedAdCreative, Error> {
            self.write_gate()?;
            Ok(CreatedAdCreative {
                site: self.site.clone(),
                account_id: "act_777".into(),
                id: format!("3{:03}", self.creatives.fetch_add(1, Ordering::SeqCst)),
            })
        }
        async fn preview_ad_creative(
            &self,
            _app: &AppConfig,
            _creds: &AccountCreds,
            _request: &CreativePreviewRequest,
            _deadline: Deadline,
        ) -> Result<CreativePreview, Error> {
            Err(Error::UnsupportedCapability {
                site: self.site.clone(),
                need: Capability::ReadAdPreviews,
            })
        }
        async fn ad_review_status(
            &self,
            _app: &AppConfig,
            _creds: &AccountCreds,
            request: &AdReviewStatusRequest,
            _deadline: Deadline,
        ) -> Result<AdReviewStatus, Error> {
            let configured = self
                .review_configured
                .lock()
                .unwrap()
                .get(&request.id)
                .cloned()
                .unwrap_or_else(|| "PAUSED".into());
            let countdown = self.review_pending_reads.load(Ordering::SeqCst);
            let still_pending = countdown > 0;
            if still_pending {
                self.review_pending_reads
                    .store(countdown - 1, Ordering::SeqCst);
            }
            Ok(AdReviewStatus {
                site: self.site.clone(),
                entity: request.entity,
                id: request.id.clone(),
                name: None,
                configured_status: configured,
                effective_status: if still_pending {
                    "PENDING_REVIEW".into()
                } else {
                    "PAUSED".into()
                },
                issues: vec![],
            })
        }
    }

    /// In-memory `DraftStore`: mirrors the file store's refusal contract
    /// (duplicate create, busy lock) and counts checkpoint writes.
    #[derive(Default)]
    struct MemStore {
        files: Mutex<HashMap<PathBuf, String>>,
        locks: Arc<Mutex<HashSet<PathBuf>>>,
        checkpoints: AtomicUsize,
    }

    struct MemLock {
        path: PathBuf,
        locks: Arc<Mutex<HashSet<PathBuf>>>,
    }

    impl Drop for MemLock {
        fn drop(&mut self) {
            self.locks.lock().unwrap().remove(&self.path);
        }
    }

    impl crate::draft::DraftLock for MemLock {}

    impl DraftStore for MemStore {
        fn create_new(
            &self,
            path: &std::path::Path,
            state: &PausedDraftState,
        ) -> Result<(), String> {
            let mut files = self.files.lock().unwrap();
            if files.contains_key(path) {
                return Err("draft_state_exists".into());
            }
            files.insert(
                path.to_path_buf(),
                serde_json::to_string(state).expect("state serializes"),
            );
            Ok(())
        }
        fn read(&self, path: &std::path::Path) -> Result<PausedDraftState, String> {
            self.files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| "draft_state_missing".to_string())
                .and_then(|raw| {
                    serde_json::from_str(&raw).map_err(|_| "draft_state_corrupt".to_string())
                })
                .and_then(|state: PausedDraftState| {
                    state.validate()?;
                    Ok(state)
                })
        }
        fn checkpoint(
            &self,
            path: &std::path::Path,
            state: &PausedDraftState,
        ) -> Result<(), String> {
            self.checkpoints.fetch_add(1, Ordering::SeqCst);
            self.files.lock().unwrap().insert(
                path.to_path_buf(),
                serde_json::to_string(state).expect("state serializes"),
            );
            Ok(())
        }
        fn try_lock(
            &self,
            path: &std::path::Path,
        ) -> Result<Box<dyn crate::draft::DraftLock + Send + Sync>, String> {
            let mut locks = self.locks.lock().unwrap();
            if locks.contains(path) {
                return Err("draft_busy".into());
            }
            locks.insert(path.to_path_buf());
            Ok(Box::new(MemLock {
                path: path.to_path_buf(),
                locks: Arc::clone(&self.locks),
            }))
        }
    }

    fn draft_manifest() -> PausedDraftManifest {
        serde_json::from_str(
            r#"{
                "version": 1,
                "ad_account": "act_777",
                "campaign": { "name": "Launch", "objective": "awareness", "special_ad_categories": [] },
                "adset": {
                    "name": "Launch set", "daily_budget": 2500,
                    "bid_strategy": "lowest_cost_without_cap",
                    "billing_event": "IMPRESSIONS", "optimization_goal": "REACH",
                    "targeting": { "geo_locations": { "countries": ["MY"] } }
                },
                "creative": {
                    "name": "Launch creative", "image_file": "/tmp/hero.png",
                    "page_id": "456", "message": "m", "headline": "h",
                    "destination_url": "https://example.com/x", "call_to_action": "learn_more"
                },
                "ad": { "name": "Launch ad" }
            }"#,
        )
        .unwrap()
    }

    fn draft_image() -> DraftImage {
        DraftImage {
            filename: "hero.png".into(),
            bytes: b"png".to_vec(),
        }
    }

    fn setup(mock: Arc<DraftMock>) -> (Client, AccountKey, Arc<MemStore>) {
        let mut reg = Registry::new();
        reg.register(mock);
        let vault = Arc::new(MemoryVault::new());
        let key = AccountKey::new("meta_ads", "default");
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
        let store = Arc::new(MemStore::default());
        (
            Client::new(reg, vault, Arc::new(MemoryAppStore::new())),
            key,
            store,
        )
    }

    const PATH: &str = "/virtual/launch.state.json";
    fn path() -> std::path::PathBuf {
        PathBuf::from(PATH)
    }

    #[tokio::test]
    async fn happy_path_runs_five_writes_and_checkpoints_each() {
        let mock = DraftMock::new();
        let (client, key, store) = setup(mock.clone());
        let manifest = draft_manifest();
        let result = client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &manifest,
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: false,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap();
        let PausedDraftResult::Completed {
            image_hash,
            campaign_id,
            adset_id,
            creative_id,
            ad_id,
            configured_status,
            ..
        } = result
        else {
            panic!("expected completion");
        };
        assert_eq!(image_hash, "img-0");
        assert_eq!(campaign_id, "1000");
        assert_eq!(adset_id, "2000");
        assert_eq!(creative_id, "3000");
        assert_eq!(ad_id, "4000");
        assert_eq!(configured_status, "PAUSED");
        // Exactly one of each write — the checkpoint contract is "no step
        // ever runs twice in a single successful run".
        assert_eq!(mock.uploads.load(Ordering::SeqCst), 1);
        assert_eq!(mock.campaigns.load(Ordering::SeqCst), 1);
        assert_eq!(mock.adsets.load(Ordering::SeqCst), 1);
        assert_eq!(mock.creatives.load(Ordering::SeqCst), 1);
        assert_eq!(mock.ads.load(Ordering::SeqCst), 1);
        // Checkpoints: 1 create + 5 write-ahead markers + 5 confirmations.
        assert_eq!(store.checkpoints.load(Ordering::SeqCst), 10);
        assert_eq!(store.read(&path()).unwrap().stage, DraftStage::Completed);
    }

    #[tokio::test]
    async fn platform_failure_mid_chain_resumes_without_duplicate_earlier_steps() {
        let mock = DraftMock::new();
        mock.fail(FailAt::Platform(1)); // campaign write
        let (client, key, store) = setup(mock.clone());
        let manifest = draft_manifest();
        // First run: image checkpointed, campaign definitively rejected.
        let err = client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &manifest,
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: false,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Platform { code, .. } if code == "100"));
        let state = store.read(&path()).unwrap();
        assert_eq!(state.stage, DraftStage::ImageUploaded);
        assert_eq!(
            state.in_flight, None,
            "definitive failure clears the marker"
        );
        assert_eq!(state.image_hash.as_deref(), Some("img-0"));

        // Resume: only campaign..ad run; the image is never re-uploaded.
        let result = client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &manifest,
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: true,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap();
        assert!(matches!(result, PausedDraftResult::Completed { .. }));
        assert_eq!(mock.uploads.load(Ordering::SeqCst), 1);
        // Per-kind counters record successes only — the rejected attempt is
        // visible in the global write count (6 writes, 5 successes).
        assert_eq!(
            mock.campaigns.load(Ordering::SeqCst),
            1,
            "rejection + one success"
        );
        assert_eq!(mock.writes.load(Ordering::SeqCst), 6);
        assert_eq!(mock.adsets.load(Ordering::SeqCst), 1);
        assert_eq!(mock.creatives.load(Ordering::SeqCst), 1);
        assert_eq!(mock.ads.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.read(&path()).unwrap().campaign_id.as_deref(),
            Some("1000")
        );
    }

    #[tokio::test]
    async fn ambiguous_write_refuses_resume_until_human_adoption() {
        let mock = DraftMock::new();
        mock.fail(FailAt::Network(2)); // adset write
        let (client, key, store) = setup(mock.clone());
        let manifest = draft_manifest();
        let result = client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &manifest,
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: false,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap();
        // Ambiguity is a *result*, not an error: the protocol worked.
        assert!(matches!(
            &result,
            PausedDraftResult::ReconciliationRequired { step, .. } if *step == "adset"
        ));
        let state = store.read(&path()).unwrap();
        assert_eq!(state.in_flight, Some(DraftStep::Adset));
        assert_eq!(state.stage, DraftStage::CampaignCreated);
        assert_eq!(state.adset_id, None);
        // The ambiguous attempt consumed the write but never succeeded.
        assert_eq!(mock.adsets.load(Ordering::SeqCst), 0);

        // Repeated resumes keep refusing and never re-send the write.
        for _ in 0..2 {
            let again = client
                .run_paused_draft(RunPausedDraft {
                    key: &key,
                    manifest: &manifest,
                    image: Some(&draft_image()),
                    store: &*store,
                    state_path: &path(),
                    resume: true,
                    deadline: Deadline::from_secs(30),
                })
                .await
                .unwrap();
            assert!(matches!(
                again,
                PausedDraftResult::ReconciliationRequired { .. }
            ));
        }
        assert_eq!(mock.adsets.load(Ordering::SeqCst), 0);

        // Status surfaces the blocked step read-only.
        let status = client
            .paused_draft_status(&key, &*store, &path(), Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(status.in_flight, Some("adset"));
        assert_eq!(status.review.len(), 1, "only the campaign is known");

        // Human adoption records the remote ID and the run completes.
        let adopted = client
            .adopt_paused_draft_step(
                &key,
                &*store,
                &path(),
                DraftStep::Adset,
                "2999".into(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert!(matches!(adopted, PausedDraftResult::InProgress { .. }));
        let result = client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &manifest,
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: true,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap();
        assert!(matches!(result, PausedDraftResult::Completed { .. }));
        let state = store.read(&path()).unwrap();
        assert_eq!(state.adset_id.as_deref(), Some("2999"));
        assert_eq!(
            mock.adsets.load(Ordering::SeqCst),
            0,
            "adoption never re-creates"
        );
    }

    #[tokio::test]
    async fn adopt_validates_step_and_paused_status() {
        let mock = DraftMock::new();
        mock.fail(FailAt::Network(2)); // adset write goes ambiguous
        let (client, key, store) = setup(mock);
        let manifest = draft_manifest();
        client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &manifest,
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: false,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap();

        // Wrong step: the marker names adset, not creative.
        let err = client
            .adopt_paused_draft_step(
                &key,
                &*store,
                &path(),
                DraftStep::Creative,
                "9".into(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidQuery { reason, .. } if reason == "draft_not_in_flight:creative")
        );

        // A paused remote object is accepted and advances the stage.
        let adopted = client
            .adopt_paused_draft_step(
                &key,
                &*store,
                &path(),
                DraftStep::Adset,
                "2888".into(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert!(matches!(adopted, PausedDraftResult::InProgress { .. }));
        let state = store.read(&path()).unwrap();
        assert_eq!(state.adset_id.as_deref(), Some("2888"));
        assert_eq!(state.in_flight, None);
    }

    #[tokio::test]
    async fn adopt_refuses_active_object() {
        let mock = DraftMock::new();
        mock.fail(FailAt::Network(2)); // adset write goes ambiguous
        mock.review_configured
            .lock()
            .unwrap()
            .insert("1777".into(), "ACTIVE".into());
        let (client, key, store) = setup(mock);
        client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &draft_manifest(),
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: false,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap();
        let err = client
            .adopt_paused_draft_step(
                &key,
                &*store,
                &path(),
                DraftStep::Adset,
                "1777".into(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidQuery { reason, .. } if reason == "adopt_not_paused:ACTIVE")
        );
    }

    struct DenyAllAds;
    impl AdsPolicy for DenyAllAds {
        fn authorize(&self, site: &Site, action: AdsAction) -> Result<(), Error> {
            Err(Error::PolicyDenied {
                site: site.clone(),
                action: action.as_str().into(),
                reason: "test_deny".into(),
            })
        }
    }

    #[tokio::test]
    async fn policy_denial_happens_before_any_remote_write() {
        let mock = DraftMock::new();
        let mut reg = Registry::new();
        reg.register(mock.clone());
        let vault = Arc::new(MemoryVault::new());
        let key = AccountKey::new("meta_ads", "default");
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
        let client = Client::with_ads_policy(
            reg,
            vault,
            Arc::new(MemoryAppStore::new()),
            Arc::new(DenyAllAds),
        );
        let store = Arc::new(MemStore::default());
        let err = client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &draft_manifest(),
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: false,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, Error::PolicyDenied { action, .. } if action == "upload_ad_image"));
        // No marker, no remote write; the New checkpoint is left durable.
        assert_eq!(mock.writes.load(Ordering::SeqCst), 0);
        let state = store.read(&path()).unwrap();
        assert_eq!(state.stage, DraftStage::New);
        assert_eq!(state.in_flight, None);
    }

    #[tokio::test]
    async fn busy_lock_refuses_before_vault_or_writes() {
        let mock = DraftMock::new();
        let (client, key, store) = setup(mock.clone());
        let _held = store.try_lock(&path()).unwrap();
        let err = client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &draft_manifest(),
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: false,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidQuery { reason, .. } if reason == "draft_busy"));
        assert_eq!(mock.writes.load(Ordering::SeqCst), 0);
        assert!(store.read(&path()).is_err(), "no state was created");
    }

    #[tokio::test]
    async fn resume_refuses_changed_manifest_and_duplicate_create() {
        let mock = DraftMock::new();
        let (client, key, store) = setup(mock.clone());
        let mut manifest = draft_manifest();
        client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &manifest,
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: false,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap();

        // A semantic manifest change can never inherit the old hierarchy.
        manifest.adset.daily_budget += 1;
        let err = client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &manifest,
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: true,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidQuery { reason, .. } if reason == "draft_manifest_changed")
        );

        // The original manifest still resumes — read-only, zero writes.
        let manifest = draft_manifest();
        let result = client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &manifest,
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: true,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap();
        assert!(matches!(result, PausedDraftResult::Completed { .. }));
        assert_eq!(mock.writes.load(Ordering::SeqCst), 5);

        // And create-draft never overwrites an existing state file.
        let err = client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &manifest,
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: false,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidQuery { reason, .. } if reason == "draft_state_exists")
        );
    }

    #[tokio::test]
    async fn status_wait_settles_when_review_clears() {
        let mock = DraftMock::new();
        let (client, key, store) = setup(mock.clone());
        client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &draft_manifest(),
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: false,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap();
        // Three objects, six reads pending first — review latency in fast
        // forward; a tiny interval keeps the test instantaneous.
        mock.pending_reviews(6);
        let reply = client
            .paused_draft_status_wait(
                &key,
                &*store,
                &path(),
                Deadline::from_secs(30),
                std::time::Duration::from_millis(1),
            )
            .await
            .unwrap();
        assert!(!reply.pending);
        assert!(reply.review.iter().all(|s| s.effective_status == "PAUSED"));
    }

    #[tokio::test]
    async fn status_wait_returns_last_state_at_deadline_not_a_timeout() {
        // Regression (48b4a49): a poll started with the deadline spent used
        // to surface Error::DeadlineExceeded from inside the connector. The
        // contract is the last observed reply, explicitly still pending.
        let mock = DraftMock::new();
        let (client, key, store) = setup(mock.clone());
        client
            .run_paused_draft(RunPausedDraft {
                key: &key,
                manifest: &draft_manifest(),
                image: Some(&draft_image()),
                store: &*store,
                state_path: &path(),
                resume: false,
                deadline: Deadline::from_secs(30),
            })
            .await
            .unwrap();
        mock.pending_reviews(usize::MAX); // review never settles
                                          // A zero-second deadline: the first poll succeeds (mocks do not
                                          // enforce deadlines), then remaining == 0 must end the wait with
                                          // the observed state instead of a second, doomed poll.
        let reply = client
            .paused_draft_status_wait(
                &key,
                &*store,
                &path(),
                Deadline::from_secs(0),
                std::time::Duration::from_millis(1),
            )
            .await
            .unwrap();
        assert!(reply.pending);
        assert!(reply
            .review
            .iter()
            .all(|s| s.effective_status == "PENDING_REVIEW"));
    }
}
