//! Resumable paused-draft manifests (plans/001/013).
//!
//! Tier B primitives create one paused object per command, which forces the
//! operator to copy IDs between five invocations and re-derive progress after
//! any interruption. This module adds the *local* orchestration vocabulary —
//! a reviewed manifest, a checkpointed state file, and a strict write
//! protocol — while reusing the existing Tier B `Client` methods for every
//! remote write. No new remote verb exists here.
//!
//! Safety posture (unchanged from Tier B):
//!
//! - The manifest has no `status` field and no active variant anywhere; the
//!   connector remains the only component that can set a status, and it sets
//!   `PAUSED`.
//! - A successful run can only produce non-delivering assets; activation and
//!   budget mutation stay Tier C behind `AdsPolicy`.
//! - There is no exactly-once remote create. When a write's outcome is
//!   unknown (process died mid-write, network died after send), the state
//!   keeps an `in_flight` marker and every later command *refuses* until a
//!   human reconciles. Duplicate paused assets are the failure this refusal
//!   buys protection from.

use crate::ads::{AdEntity, CampaignObjective};
use crate::types::Site;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Bumped only when the state or manifest contract changes in a way an old
/// file cannot safely satisfy. A state file with a different schema is a
/// hard refusal, never migrated in place: checkpoints record remote object
/// IDs and a lossy migration could silently point a resume at the wrong
/// hierarchy.
pub const DRAFT_SCHEMA: u32 = 1;

/// Local lower bound for `daily_budget`, in the account currency's minor
/// unit. Meta documents a ~USD 1/day minimum; rejecting locally below this
/// floor turns what would be a remote ad-set error (after the campaign
/// already exists) into a pre-I/O validation failure.
pub const MIN_DAILY_BUDGET: u64 = 100;

/// Every delivery object this workflow can create is configured `PAUSED` by
/// the connector; the result reports it as a constant rather than an echoed
/// platform string so "paused by construction" and "paused by luck" cannot
/// be confused in operator tooling.
pub const CONFIGURED_PAUSED: &str = "PAUSED";

/// The objective → (optimization_goal, billing_event) pairs postkit will
/// orchestrate. Meta's full matrix is larger, but shipping a pairing here
/// means postkit has verified its wire form; an unlisted pairing fails
/// *locally* (before any remote object exists) with a
/// `unsupported_adset_pairing:` reason instead of remotely after the
/// campaign was already created. Extend this table only with a live-tested
/// combination — the same rule `BidStrategy` follows.
const SUPPORTED_ADSET_PAIRINGS: &[(CampaignObjective, &str, &str)] = &[
    (CampaignObjective::Awareness, "REACH", "IMPRESSIONS"),
    (
        CampaignObjective::Awareness,
        "BRAND_AWARENESS",
        "IMPRESSIONS",
    ),
];

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// One reviewed launch: exactly one campaign, one ad set, one image-link
/// creative, and one final ad. Unknown keys are rejected so a typo such as
/// `"budgt"` cannot silently fall back to a default.
///
/// The manifest deliberately has no ID fields (`campaign_id`, `image_hash`,
/// …): those are only ever derived from checkpointed outputs of prior steps,
/// never re-typed by the operator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PausedDraftManifest {
    /// Must be `1`. Anything else is an unsupported contract, not an error
    /// to paper over.
    pub version: u32,
    /// The ad account the whole hierarchy is created under. Accepts
    /// `act_<id>` (as copied from account discovery) or a bare numeric ID.
    pub ad_account: String,
    pub campaign: DraftCampaign,
    pub adset: DraftAdset,
    pub creative: DraftCreative,
    pub ad: DraftAd,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftCampaign {
    pub name: String,
    /// One of the closed `CampaignObjective` values (`awareness`, …).
    pub objective: CampaignObjective,
    /// Meta requires the field on every create; `[]` means "none apply".
    #[serde(default)]
    pub special_ad_categories: Vec<String>,
}

/// Budget is the ad account's minor currency unit (e.g. sen for MYR),
/// matching Meta's integer `daily_budget` wire field exactly. It configures
/// future delivery but cannot spend while every delivery object is paused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftAdset {
    pub name: String,
    pub daily_budget: u64,
    pub bid_strategy: crate::ads::BidStrategy,
    /// Meta uppercase token, e.g. `IMPRESSIONS`. Kept as a string because
    /// the closed pairing table below — not a closed enum — is the checked
    /// contract (see [`SUPPORTED_ADSET_PAIRINGS`]).
    pub billing_event: String,
    /// Meta uppercase token, e.g. `REACH`; validated the same way.
    pub optimization_goal: String,
    /// Raw Meta targeting spec. Only its object-ness is checked locally;
    /// platform-specific rules belong to the connector's remote validation.
    pub targeting: serde_json::Value,
}

/// The image reference is a *CLI-boundary* path: the CLI resolves it to
/// bytes + basename before anything enters the library, which never sees a
/// filesystem path (the same rule `UploadAdImageRequest` follows).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftCreative {
    pub name: String,
    /// Local image file whose *basename* becomes the upload filename. The
    /// basename must be a plain filename — no `/`, no `\`, no traversal.
    pub image_file: String,
    pub page_id: String,
    pub message: String,
    pub headline: String,
    pub destination_url: String,
    pub call_to_action: crate::ads::LinkCallToAction,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftAd {
    pub name: String,
}

impl PausedDraftManifest {
    /// Local, zero-I/O validation: everything checkable before the vault,
    /// the image file, or the state file is touched. Reasons are stable
    /// strings in the `invalid_query` family (exit 2).
    pub fn validate(&self) -> Result<(), String> {
        if self.version != DRAFT_SCHEMA {
            return Err(format!("unsupported_manifest_version:{}", self.version));
        }
        self.normalized_account()?;
        // Campaign
        require_name(&self.campaign.name)?;
        for category in &self.campaign.special_ad_categories {
            if category.trim().is_empty() {
                return Err("empty_special_ad_category".into());
            }
        }
        // Ad set
        require_name(&self.adset.name)?;
        if self.adset.daily_budget == 0 {
            return Err("daily_budget_must_be_positive".into());
        }
        if self.adset.daily_budget < MIN_DAILY_BUDGET {
            return Err(format!("daily_budget_below_minimum:{MIN_DAILY_BUDGET}"));
        }
        if !self.has_supported_pairing() {
            return Err(format!(
                "unsupported_adset_pairing:{}:{}:{}",
                self.campaign.objective.as_str(),
                self.adset.optimization_goal,
                self.adset.billing_event
            ));
        }
        if !self.adset.targeting.is_object() {
            return Err("targeting_must_be_object".into());
        }
        // Creative
        require_name(&self.creative.name)?;
        require_numeric_id("page_id", &self.creative.page_id)?;
        require_text("message", &self.creative.message)?;
        require_text("headline", &self.creative.headline)?;
        require_https_url("destination_url", &self.creative.destination_url)?;
        self.image_filename()?;
        // Ad
        require_name(&self.ad.name)?;
        Ok(())
    }

    /// `act_<digits>`, or a reason. Accepts the `act_` spelling operators
    /// copy from account discovery and a bare numeric API ID.
    pub fn normalized_account(&self) -> Result<String, String> {
        let bare = self
            .ad_account
            .strip_prefix("act_")
            .unwrap_or(&self.ad_account);
        if bare.is_empty() || !bare.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!("bad_ad_account:{}", self.ad_account));
        }
        Ok(format!("act_{bare}"))
    }

    /// The upload filename is the manifest image path's basename. It must be
    /// a real basename (no separators) so an operator cannot smuggle a path
    /// component into Meta's stored filename or a local path into an error.
    pub fn image_filename(&self) -> Result<String, String> {
        let name = self
            .creative
            .image_file
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default();
        if name.trim().is_empty() || name.contains(['/', '\\']) {
            return Err("invalid_image_filename".into());
        }
        Ok(name.to_string())
    }

    fn has_supported_pairing(&self) -> bool {
        let goal = self.adset.optimization_goal.trim().to_ascii_uppercase();
        let billing = self.adset.billing_event.trim().to_ascii_uppercase();
        SUPPORTED_ADSET_PAIRINGS.iter().any(|(objective, g, b)| {
            *objective == self.campaign.objective && *g == goal && *b == billing
        })
    }
}

/// One draft run handed to [`Client::run_paused_draft`](crate::Client::run_paused_draft):
/// `resume: false` is `create-draft` (the state path must be new), `true` is
/// `resume-draft` (it must exist and match the manifest fingerprint).
pub struct RunPausedDraft<'a> {
    pub key: &'a crate::types::AccountKey,
    pub manifest: &'a PausedDraftManifest,
    pub image: Option<&'a DraftImage>,
    pub store: &'a dyn DraftStore,
    pub state_path: &'a Path,
    pub resume: bool,
    pub deadline: crate::types::Deadline,
}

/// Bytes resolved from the manifest's `image_file` by the CLI. Carried
/// separately so the library performs no filesystem I/O and local paths
/// never reach errors or network payloads.
#[derive(Clone, Debug, PartialEq)]
pub struct DraftImage {
    pub filename: String,
    pub bytes: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Steps and stages
// ---------------------------------------------------------------------------

/// The five remote writes, in execution order. The image uploads *first*:
/// it is the only step that reads local disk and the most likely to fail
/// for operator reasons (wrong format, empty file), and failing there costs
/// zero remote objects instead of a campaign + ad set to clean up in Ads
/// Manager.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftStep {
    Image,
    Campaign,
    Adset,
    Creative,
    Ad,
}

impl DraftStep {
    pub const ALL: [DraftStep; 5] = [
        DraftStep::Image,
        DraftStep::Campaign,
        DraftStep::Adset,
        DraftStep::Creative,
        DraftStep::Ad,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Campaign => "campaign",
            Self::Adset => "adset",
            Self::Creative => "creative",
            Self::Ad => "ad",
        }
    }

    /// The stage reached once this step's output is durably checkpointed.
    /// Declared to keep the state lattice exhaustive over future steps.
    pub fn stage_after(self) -> DraftStage {
        match self {
            Self::Image => DraftStage::ImageUploaded,
            Self::Campaign => DraftStage::CampaignCreated,
            Self::Adset => DraftStage::AdsetCreated,
            Self::Creative => DraftStage::CreativeCreated,
            Self::Ad => DraftStage::Completed,
        }
    }
}

impl std::str::FromStr for DraftStep {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "image" => Ok(Self::Image),
            "campaign" => Ok(Self::Campaign),
            "adset" => Ok(Self::Adset),
            "creative" => Ok(Self::Creative),
            "ad" => Ok(Self::Ad),
            other => Err(format!("unknown_draft_step:{other}")),
        }
    }
}

/// Linear progress marker. Variant order is execution order, so `Ord`
/// answers "is this step's output already checkpointed?" without a
/// separate progress counter to keep consistent.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftStage {
    New,
    ImageUploaded,
    CampaignCreated,
    AdsetCreated,
    CreativeCreated,
    AdCreated,
    Completed,
}

impl DraftStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::ImageUploaded => "image_uploaded",
            Self::CampaignCreated => "campaign_created",
            Self::AdsetCreated => "adset_created",
            Self::CreativeCreated => "creative_created",
            Self::AdCreated => "ad_created",
            Self::Completed => "completed",
        }
    }
}

// ---------------------------------------------------------------------------
// Checkpoint state
// ---------------------------------------------------------------------------

/// The operator-owned checkpoint file. It records remote IDs only — never a
/// token, app secret, image bytes, or payment data — plus the single
/// `in_flight` marker that makes the write protocol crash-safe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PausedDraftState {
    pub schema: u32,
    pub site: String,
    /// Normalized `act_<id>`; must match the manifest on every resume.
    pub account_id: String,
    /// SHA-256 of the manifest's canonical form (see [`manifest_fingerprint`]).
    pub manifest_fingerprint: String,
    /// Time-derived identifier for operator reconciliation only; it carries
    /// no uniqueness or security contract.
    pub run_id: String,
    pub stage: DraftStage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creative_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ad_id: Option<String>,
    /// The one write whose outcome is not durably known. Present ⇒ every
    /// mutating command refuses until a human reconciles (see
    /// [`RECONCILE_GUIDANCE`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_flight: Option<DraftStep>,
}

impl PausedDraftState {
    pub fn new(site: &Site, account_id: String, manifest_fingerprint: String) -> Self {
        Self {
            schema: DRAFT_SCHEMA,
            site: site.as_str().to_string(),
            account_id,
            manifest_fingerprint,
            run_id: new_run_id(),
            stage: DraftStage::New,
            image_hash: None,
            campaign_id: None,
            adset_id: None,
            creative_id: None,
            ad_id: None,
            in_flight: None,
        }
    }

    /// A step is pending when the stage has not advanced past its output.
    pub fn step_pending(&self, step: DraftStep) -> bool {
        self.stage < step.stage_after()
    }

    /// The steps still between `stage` and completion.
    pub fn remaining_steps(&self) -> Vec<DraftStep> {
        DraftStep::ALL
            .into_iter()
            .filter(|s| self.step_pending(*s))
            .collect()
    }

    /// Record one confirmed output and advance the stage. Called only after
    /// a durable checkpoint of the same write is about to be written, or by
    /// adoption after human reconciliation.
    pub fn set_output(&mut self, step: DraftStep, id: String) {
        let slot = match step {
            DraftStep::Image => &mut self.image_hash,
            DraftStep::Campaign => &mut self.campaign_id,
            DraftStep::Adset => &mut self.adset_id,
            DraftStep::Creative => &mut self.creative_id,
            DraftStep::Ad => &mut self.ad_id,
        };
        // A present output means either a corrupted file or a code bug in
        // the protocol; both must stop the run, not overwrite history.
        debug_assert!(slot.is_none(), "output for {step:?} already recorded");
        *slot = Some(id);
        self.stage = step.stage_after();
        self.in_flight = None;
    }

    /// Structural consistency of a state file we just read: the stage must
    /// be exactly "the first k steps have outputs" for some k — no gaps
    /// (ad ID without an ad set), no orphans (output past the stage). This
    /// is what makes a hand-edited or half-written file refuse loudly
    /// instead of resuming from a fabricated position.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != DRAFT_SCHEMA {
            return Err(format!("draft_state_schema:{}", self.schema));
        }
        if self.site.trim().is_empty() {
            return Err("draft_state_site".into());
        }
        if !self.account_id.starts_with("act_")
            || !self.account_id[4..].chars().all(|c| c.is_ascii_digit())
        {
            return Err(format!("draft_state_account:{}", self.account_id));
        }
        if self.manifest_fingerprint.len() != 64
            || !self
                .manifest_fingerprint
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        {
            return Err("draft_state_fingerprint".into());
        }
        let outputs = [
            self.image_hash.is_some(),
            self.campaign_id.is_some(),
            self.adset_id.is_some(),
            self.creative_id.is_some(),
            self.ad_id.is_some(),
        ];
        // Prefix property: outputs are the first `done` steps, none after.
        let done = outputs.iter().filter(|d| **d).count();
        if outputs[..done].iter().any(|d| !*d) || outputs[done..].iter().any(|d| *d) {
            return Err("draft_state_stage".into());
        }
        let expected_stage = if done == 0 {
            DraftStage::New
        } else {
            DraftStep::ALL[done - 1].stage_after()
        };
        if self.stage != expected_stage {
            return Err("draft_state_stage".into());
        }
        Ok(())
    }
}

/// SHA-256 of the manifest's *canonical* form: re-serialized through
/// `serde_json::Value`, whose object map is key-sorted. Whitespace and key
/// order in the reviewed file are therefore irrelevant, while any semantic
/// change — budget, audience, copy, destination — changes the hash and
/// blocks resume. Exact-byte hashing was considered and rejected: it also
/// rejects harmless reformatting, and JSON has no comments, so there is no
/// byte-level change that is not already visible to the canonical form.
pub fn manifest_fingerprint(manifest: &PausedDraftManifest) -> String {
    let canonical = serde_json::to_value(manifest).expect("manifest serializes");
    let bytes = serde_json::to_vec(&canonical).expect("canonical form serializes");
    let digest = Sha256::digest(&bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Time-derived run identifier: enough for an operator to correlate a state
/// file with Ads Manager activity; no uniqueness or security claim.
pub fn new_run_id() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("run-{secs:x}")
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// Operator-facing outcome of a draft command. `reconciliation_required` is
/// a *result*, not an error: the command obeyed its protocol, and the next
/// move is a human decision, not a retry.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PausedDraftResult {
    Completed {
        site: Site,
        account_id: String,
        image_hash: String,
        campaign_id: String,
        adset_id: String,
        creative_id: String,
        ad_id: String,
        /// Constant, not echoed: every delivery object is `PAUSED` by
        /// construction, and this run activated and spent nothing.
        configured_status: &'static str,
    },
    InProgress {
        site: Site,
        account_id: String,
        stage: &'static str,
        remaining: Vec<&'static str>,
    },
    ReconciliationRequired {
        site: Site,
        account_id: String,
        step: &'static str,
        guidance: &'static str,
    },
}

/// The single documented route out of an ambiguous write. There is no
/// `--force`: a blind retry could create a duplicate paused object, and the
/// whole value of the checkpoint protocol is refusing that guess.
pub const RECONCILE_GUIDANCE: &str = "the write may or may not have reached Meta; \
check Ads Manager for the clearly named paused object of this step. If it exists, \
record it with `postkit ads adopt-draft-step`. If it does not, create it with the \
matching single-step ads command and adopt the returned ID.";

/// Read-only snapshot for `status-draft`: checkpoint positions plus one
/// live review read per known delivery object. `pending` summarizes whether
/// any object is still in a transitional Meta review state.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DraftStatusReply {
    pub site: Site,
    pub account_id: String,
    pub stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creative_id: Option<String>,
    /// Set when an ambiguous write still blocks progress.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_flight: Option<&'static str>,
    pub review: Vec<crate::ads::AdReviewStatus>,
    pub pending: bool,
}

// ---------------------------------------------------------------------------
// State store
// ---------------------------------------------------------------------------

/// Persistence seam for the checkpoint file, injected like `Vault` and
/// `AppStore` so orchestration tests can simulate atomic writes, missing
/// files, and locks without touching disk. Reasons are stable strings in
/// the `draft_*` family surfaced as `invalid_query` (exit 2).
pub trait DraftStore: Send + Sync {
    /// Create a brand-new state file; must refuse if the path exists so a
    /// second launch can never silently adopt the first one's checkpoints.
    fn create_new(&self, path: &Path, state: &PausedDraftState) -> Result<(), String>;
    fn read(&self, path: &Path) -> Result<PausedDraftState, String>;
    /// Durably replace the file's contents: the write must be atomic
    /// (temp + rename) and synced, because a partially written checkpoint
    /// is indistinguishable from a lost one at crash time.
    fn checkpoint(&self, path: &Path, state: &PausedDraftState) -> Result<(), String>;
    /// Exclusive advisory lock held for the duration of one mutating
    /// command. Two concurrent `resume`s would otherwise both read the
    /// same stage and both issue the next write — exactly the duplicate
    /// the protocol exists to prevent.
    fn try_lock(&self, path: &Path) -> Result<Box<dyn DraftLock + Send + Sync>, String>;
}

/// Marker for a held store lock; the implementation releases on drop.
pub trait DraftLock: Send + Sync {}

/// Filesystem `DraftStore`. Follows the vault's write discipline:
/// owner-only files (`0600`, set at creation so no umask window exists),
/// pid-suffixed temp files, `sync_all` before the atomic rename.
pub struct FileDraftStore;

impl FileDraftStore {
    fn serialize(state: &PausedDraftState) -> String {
        // Pretty-printed: the state file is operator-inspected during
        // reconciliation, and the fingerprint is computed from the manifest,
        // never from these bytes.
        serde_json::to_string_pretty(state).expect("state serializes")
    }
}

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as FilePermExt;

#[cfg(unix)]
fn create_file(path: &Path, exclusive: bool, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    if exclusive {
        opts.create_new(true);
    }
    // 0600 at creation: the file holds remote object IDs, and there is no
    // reason any other local user should read launch structure.
    let mut file = opts.mode(0o600).open(path)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn create_file(path: &Path, exclusive: bool, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    if exclusive {
        opts.create_new(true);
    }
    let mut file = opts.open(path)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

impl DraftStore for FileDraftStore {
    fn create_new(&self, path: &Path, state: &PausedDraftState) -> Result<(), String> {
        match create_file(path, true, &Self::serialize(state)) {
            Ok(()) => Ok(()),
            // O_EXCL is the atomic "did not exist" check; nothing can race
            // between the check and the create.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err("draft_state_exists".into())
            }
            Err(_) => Err("draft_state_unwritable".into()),
        }
    }

    fn read(&self, path: &Path) -> Result<PausedDraftState, String> {
        let bytes = std::fs::read_to_string(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "draft_state_missing".to_string(),
            _ => "draft_state_unreadable".to_string(),
        })?;
        let state: PausedDraftState =
            serde_json::from_str(&bytes).map_err(|_| "draft_state_corrupt".to_string())?;
        state.validate()?;
        Ok(state)
    }

    fn checkpoint(&self, path: &Path, state: &PausedDraftState) -> Result<(), String> {
        let tmp = PathBuf::from(format!("{}.tmp.{}", path.display(), std::process::id()));
        if let Err(e) = create_file(&tmp, false, &Self::serialize(state)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(match e.kind() {
                std::io::ErrorKind::NotFound => "draft_state_missing".to_string(),
                _ => "draft_state_unwritable".to_string(),
            });
        }
        // rename(2) replaces atomically: a reader (or a crash) sees either
        // the old checkpoint or the new one, never a partial file.
        std::fs::rename(&tmp, path).map_err(|_| "draft_state_unwritable".to_string())?;
        Ok(())
    }

    fn try_lock(&self, path: &Path) -> Result<Box<dyn DraftLock + Send + Sync>, String> {
        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        // create_new is the lock: only one process can own the lock file.
        // A stale lock after a crash stays until the operator removes it —
        // the `draft_busy` reason names the file to delete.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => Ok(Box::new(FileDraftLock(lock_path))),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err("draft_busy".into()),
            Err(_) => Err("draft_lock_failed".into()),
        }
    }
}

/// Releases the lock by removing its file on drop. If removal fails the
/// lock goes stale, which fails safe: the next command refuses until a
/// human deletes the lock file.
struct FileDraftLock(PathBuf);

impl Drop for FileDraftLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl DraftLock for FileDraftLock {}

// Shared validators — same contracts as `ads.rs`, kept local so the draft
// surface fails with the identical stable reasons the primitives use.
fn require_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        Err("missing_name".into())
    } else {
        Ok(())
    }
}

fn require_numeric_id(field: &str, id: &str) -> Result<(), String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        Err(format!("bad_{field}:{id}"))
    } else {
        Ok(())
    }
}

fn require_text(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("missing_{field}"))
    } else {
        Ok(())
    }
}

fn require_https_url(field: &str, value: &str) -> Result<(), String> {
    let Some(authority_and_rest) = value.strip_prefix("https://") else {
        return Err(format!("{field}_must_be_https"));
    };
    let authority = authority_and_rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(format!("{field}_must_be_https"));
    }
    Ok(())
}

/// Entity for remote adoption validation. Image and creative have no review
/// edge (an image hash is only proven by use, a creative by the ad that
/// references it), so their adoption is a recorded human decision rather
/// than a remotely verified fact.
pub fn adoption_entity(step: DraftStep) -> Option<AdEntity> {
    match step {
        DraftStep::Campaign => Some(AdEntity::Campaign),
        DraftStep::Adset => Some(AdEntity::Adset),
        DraftStep::Ad => Some(AdEntity::Ad),
        DraftStep::Image | DraftStep::Creative => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The plan document's example manifest, trimmed to test-local values.
    fn example_manifest_json() -> String {
        json!({
            "version": 1,
            "ad_account": "act_123456",
            "campaign": {
                "name": "Postkit launch — do not activate",
                "objective": "awareness",
                "special_ad_categories": []
            },
            "adset": {
                "name": "Postkit launch ad set — do not activate",
                "daily_budget": 2500,
                "bid_strategy": "lowest_cost_without_cap",
                "billing_event": "IMPRESSIONS",
                "optimization_goal": "REACH",
                "targeting": {
                    "geo_locations": { "countries": ["MY"] },
                    "age_min": 18,
                    "age_max": 65
                }
            },
            "creative": {
                "name": "Postkit launch creative",
                "image_file": "/tmp/hero.png",
                "page_id": "1413299108523738",
                "message": "A clear benefit.",
                "headline": "Learn more",
                "destination_url": "https://example.com/offer",
                "call_to_action": "learn_more"
            },
            "ad": { "name": "Postkit launch ad — do not activate" }
        })
        .to_string()
    }

    fn example_manifest() -> PausedDraftManifest {
        serde_json::from_str(&example_manifest_json()).unwrap()
    }

    #[test]
    fn example_manifest_validates_and_normalizes() {
        let manifest = example_manifest();
        manifest.validate().unwrap();
        assert_eq!(manifest.normalized_account().unwrap(), "act_123456");
        // A bare numeric account is accepted and normalized.
        let mut bare = manifest.clone();
        bare.ad_account = "123456".into();
        assert_eq!(bare.normalized_account().unwrap(), "act_123456");
        // The image path contributes only a basename.
        assert_eq!(manifest.image_filename().unwrap(), "hero.png");
    }

    #[test]
    fn unknown_keys_and_bad_versions_are_refused() {
        let raw = example_manifest_json();
        // `json!` serializes compactly (no space after the colon).
        let with_typo = raw.replace("\"daily_budget\":2500", "\"budgt\":2500");
        assert!(serde_json::from_str::<PausedDraftManifest>(&with_typo).is_err());

        let mut manifest = example_manifest();
        manifest.version = 2;
        assert_eq!(
            manifest.validate().unwrap_err(),
            "unsupported_manifest_version:2"
        );
    }

    #[test]
    fn pairing_table_is_closed_and_local() {
        let mut manifest = example_manifest();
        manifest.validate().unwrap();

        // A goal the objective does not support must fail locally, before
        // any remote object exists.
        manifest.adset.optimization_goal = "LINK_CLICKS".into();
        assert_eq!(
            manifest.validate().unwrap_err(),
            "unsupported_adset_pairing:awareness:LINK_CLICKS:IMPRESSIONS"
        );

        // Unverified objective families are refused the same way even when
        // the pairing itself would be legal on Meta's side.
        manifest.campaign.objective = CampaignObjective::Traffic;
        manifest.adset.optimization_goal = "REACH".into();
        assert!(manifest
            .validate()
            .unwrap_err()
            .starts_with("unsupported_adset_pairing:traffic:"));

        // Case and surrounding whitespace on the Meta tokens are tolerated;
        // the reviewed meaning is unchanged.
        manifest.campaign.objective = CampaignObjective::Awareness;
        manifest.adset.optimization_goal = " reach ".into();
        manifest.adset.billing_event = "impressions".into();
        manifest.validate().unwrap();
    }

    #[test]
    fn budget_floor_is_enforced_locally() {
        let mut manifest = example_manifest();
        manifest.adset.daily_budget = 0;
        assert_eq!(
            manifest.validate().unwrap_err(),
            "daily_budget_must_be_positive"
        );
        manifest.adset.daily_budget = 50; // RM0.50: below Meta's practical floor
        assert_eq!(
            manifest.validate().unwrap_err(),
            "daily_budget_below_minimum:100"
        );
        manifest.adset.daily_budget = MIN_DAILY_BUDGET;
        manifest.validate().unwrap();
    }

    #[test]
    fn fingerprint_is_semantic_not_byte_wise() {
        let manifest = example_manifest();
        let base = manifest_fingerprint(&manifest);

        // Same meaning, different bytes: reformatting must not strand the
        // operator (review decision on plans/001/013).
        let reindented = serde_json::from_str::<PausedDraftManifest>(
            &serde_json::to_string_pretty(
                &serde_json::from_str::<serde_json::Value>(&example_manifest_json()).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(manifest_fingerprint(&reindented), base);

        // Any semantic change — the budget — must block resume.
        let mut pricier = manifest.clone();
        pricier.adset.daily_budget += 1;
        assert_ne!(manifest_fingerprint(&pricier), base);
    }

    #[test]
    fn state_lattice_rejects_gaps_orphans_and_bad_metadata() {
        let site = Site::new("meta_ads");
        let mut state = PausedDraftState::new(&site, "act_1".into(), "a".repeat(64));
        state.validate().unwrap();

        // Outputs must form a prefix: an ad without its ad set refuses.
        state.ad_id = Some("99".into());
        assert_eq!(state.validate().unwrap_err(), "draft_state_stage");

        // Stage must match exactly the recorded outputs.
        let mut done = PausedDraftState::new(&site, "act_1".into(), "a".repeat(64));
        done.set_output(DraftStep::Image, "hash".into());
        assert_eq!(done.stage, DraftStage::ImageUploaded);
        done.validate().unwrap();
        assert_eq!(done.remaining_steps().len(), 4);

        // Metadata damage refuses with its own reason.
        let mut bad = PausedDraftState::new(&site, "act_1".into(), "a".repeat(64));
        bad.schema = 7;
        assert_eq!(bad.validate().unwrap_err(), "draft_state_schema:7");
        let bad = PausedDraftState::new(&site, "act_1".into(), "short".into());
        assert_eq!(bad.validate().unwrap_err(), "draft_state_fingerprint");
    }

    #[test]
    fn file_store_creates_owner_only_refuses_duplicates_and_locks() {
        let dir = std::env::temp_dir().join(format!("postkit-draft-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("state-{}.json", new_run_id()));
        let store = FileDraftStore;
        let state = PausedDraftState::new(&Site::new("meta_ads"), "act_1".into(), "f".repeat(64));

        store.create_new(&path, &state).unwrap();
        // A second launch on the same state file must refuse, not overwrite.
        assert_eq!(
            store.create_new(&path, &state).unwrap_err(),
            "draft_state_exists"
        );
        // Checkpoint round-trips through the lattice validation — outputs
        // must be recorded in execution order (image before campaign).
        let mut advanced = state.clone();
        advanced.set_output(DraftStep::Image, "hash-1".into());
        advanced.set_output(DraftStep::Campaign, "42".into());
        store.checkpoint(&path, &advanced).unwrap();
        assert_eq!(store.read(&path).unwrap(), advanced);
        // No temp files survive a checkpoint.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "temp file survived checkpoint");

        // The lock is exclusive while held and released on drop.
        let held = store.try_lock(&path).unwrap();
        assert_eq!(store.try_lock(&path).err(), Some("draft_busy".to_string()));
        drop(held);
        drop(store.try_lock(&path).unwrap());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn state_file_is_owner_only() {
        let dir = std::env::temp_dir().join(format!("postkit-draft-mode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let state = PausedDraftState::new(&Site::new("meta_ads"), "act_1".into(), "f".repeat(64));
        FileDraftStore.create_new(&path, &state).unwrap();
        // The create-time mode decides: a plain `File::create` would land
        // at 0644 under the usual umask (vault issue 002's lesson).
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).ok();
    }
}
