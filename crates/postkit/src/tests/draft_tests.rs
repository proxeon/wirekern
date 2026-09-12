//! Draft orchestration (plans/001/013). A dedicated publisher mock (not the
//! shared `MockPub`) because these tests script *failures at exact write
//! indexes — the write-ahead `in_flight` protocol's behavior depends on
//! which write died and how — and an in-memory `DraftStore` so checkpoint
//! atomicity, duplicate refusal, and locking are asserted without disk.
use crate::ads::{
    AdReviewStatus, AdReviewStatusRequest, CreateLinkAdCreativeRequest, CreatePausedAdRequest,
    CreatedAd, CreatedAdCreative, CreativePreview, CreativePreviewRequest, UploadAdImageRequest,
    UploadedAdImage,
};
use crate::apps::MemoryAppStore;
use crate::client::Client;
use crate::draft::{
    DraftImage, DraftStage, DraftStep, DraftStore, PausedDraftManifest, PausedDraftResult,
    PausedDraftState, RunPausedDraft,
};
use crate::error::Error;
use crate::facets::AdsManager;
use crate::policy::{AdsAction, AdsPolicy};
use crate::publisher::{AuthKind, Publisher};
use crate::registry::{Connector, Registry};
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
}

#[async_trait]
impl AdsManager for DraftMock {
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
    async fn upload_ad_video(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _request: &crate::ads::UploadAdVideoRequest,
        _deadline: Deadline,
    ) -> Result<crate::ads::UploadedAdVideo, Error> {
        self.write_gate()?;
        Ok(crate::ads::UploadedAdVideo {
            site: self.site.clone(),
            account_id: "act_777".into(),
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
    async fn create_video_ad_creative(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _request: &crate::ads::CreateVideoAdCreativeRequest,
        _deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        self.write_gate()?;
        Ok(CreatedAdCreative {
            site: self.site.clone(),
            account_id: "act_777".into(),
            id: format!("3{:03}", self.creatives.fetch_add(1, Ordering::SeqCst)),
        })
    }
    async fn create_ad_creative(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _request: &crate::ads::CreateAdCreativeRequest,
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
    fn create_new(&self, path: &std::path::Path, state: &PausedDraftState) -> Result<(), String> {
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
    fn checkpoint(&self, path: &std::path::Path, state: &PausedDraftState) -> Result<(), String> {
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
    reg.register_connector(Connector::from_publisher(mock.clone()).ads(mock));
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
    reg.register_connector(Connector::from_publisher(mock.clone()).ads(mock.clone()));
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
    let client = Client::new(reg, vault, Arc::new(MemoryAppStore::new()))
        .with_ads_policy(Arc::new(DenyAllAds));
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
    manifest.adset.daily_budget = Some(manifest.adset.daily_budget.unwrap() + 1);
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
    assert!(matches!(err, Error::InvalidQuery { reason, .. } if reason == "draft_state_exists"));
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
