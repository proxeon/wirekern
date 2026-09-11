//! Official-API execution kernel: send now through your apps, with your vault, no calendar.
//!
//! Default features are empty. Inject `Vault` / `AppStore`, or enable `vault-file`.

mod ads;
mod apps;
mod bundle;
mod client;
mod error;
mod facets;
mod insights;
mod media;
mod pages;
mod policy;
mod publisher;
mod registry;
mod types;
mod vault;
#[cfg(feature = "whatsapp-cloud")]
mod whatsapp;

#[cfg(feature = "vault-file")]
mod keys;
#[cfg(feature = "vault-file")]
mod vault_file;

// Draft orchestration is an optional composition layer over the Tier B
// client methods; it pulls sha2 for the manifest fingerprint.
#[cfg(feature = "draft")]
pub mod draft;

#[cfg(feature = "client")]
mod http;
#[cfg(feature = "client")]
pub use http::Http;

// Shared form primitive for oauth + connectors; gated so a bluesky-only
// build does not compile (and warn about) unused code.
#[cfg(any(feature = "oauth", feature = "threads"))]
pub(crate) mod form;

#[cfg(feature = "oauth")]
mod oauth;
// `new_state` included deliberately: it is oauth's public feature API
// (issue 026) — without the re-export an oauth-only build reports it as
// dead code because no connector references it there.
#[cfg(feature = "oauth")]
pub use oauth::{
    authorize_url, exchange_code, extract_code, new_state, query_param, verify_state, TokenResponse,
};

#[cfg(any(
    feature = "threads",
    feature = "bluesky",
    feature = "meta-ads",
    feature = "facebook-pages",
    feature = "instagram",
    feature = "linkedin",
    feature = "whatsapp-cloud"
))]
pub mod connectors;

pub use ads::{
    AdEntity, AdPreviewFormat, AdReviewIssue, AdReviewStatus, AdReviewStatusRequest, AdReviewWait,
    BidStrategy, CampaignObjective, CreateLinkAdCreativeRequest, CreatePausedAdRequest, CreatedAd,
    CreatedAdCreative, CreativePreview, CreativePreviewRequest, LinkAdCreative, LinkCallToAction,
    PausedAd, PausedAdCreate, PausedAdset, PausedCampaign, UploadAdImageRequest, UploadedAdImage,
};
pub use apps::{app_source, env_override, AppStore, MemoryAppStore};
pub use bundle::bundled_registry;
pub use client::{refresh_is_due, Client};
#[cfg(feature = "draft")]
pub use draft::{
    manifest_fingerprint, DraftImage, DraftStage, DraftStatusReply, DraftStep, DraftStore,
    FileDraftStore, PausedDraftManifest, PausedDraftResult, PausedDraftState, RunPausedDraft,
    CONFIGURED_PAUSED, MIN_DAILY_BUDGET,
};
pub use error::{Error, WireError};
#[cfg(feature = "whatsapp-cloud")]
pub use facets::WhatsAppSender;
pub use facets::{AdsManager, InsightsSource, MediaReader, PageDirectory};
pub use insights::{
    AdAccount, AdAccountsReply, AttributionWindow, Breakdown, DateRange, InsightRow, InsightsLevel,
    InsightsQuery, InsightsReply, Metric, MAX_RANGE_DAYS,
};
pub use media::{MediaQuery, MediaReply, PublishedMedia, DEFAULT_MEDIA_LIMIT, MAX_MEDIA_LIMIT};
pub use pages::{PageAccount, PagesReply};
pub use policy::{AdsAction, AdsPolicy, PausedOnlyAdsPolicy};
#[cfg(feature = "whatsapp-cloud")]
pub use policy::{AllowWhatsAppSendsPolicy, NoWhatsAppSendsPolicy, WhatsAppAction, WhatsAppPolicy};
pub use publisher::{AuthKind, AuthReply, AuthStart, Publisher};
pub use registry::{Connector, Registry};
pub use types::{
    valid_name, AccountCreds, AccountKey, AppConfig, Body, Capability, Deadline, Image, Intent,
    Limits, OAuthApp, Outcome, PostRequest, Probe, Site, WhoAmI, USER_AGENT,
};
pub use vault::{MemoryVault, Vault};
#[cfg(feature = "whatsapp-cloud")]
pub use whatsapp::{
    DeliveryConversation, DeliveryError, DeliveryPricing, DeliveryStatus, DeliveryStatusKind,
    InboundContact, InboundInteractive, InboundLocation,
    InboundMedia, InboundMessage, InboundMessages, InboundOrder, InboundReaction, InboundReferral,
    InboundUnsupported, WebhookParseOptions, WhatsAppMessage,
    WhatsAppSendRequest, MAX_REPLY_TEXT,
};

#[cfg(feature = "vault-file")]
pub use keys::{CreatedKey, FileKeyStore, KeyMeta, KEY_PREFIX};
#[cfg(feature = "vault-file")]
pub use vault_file::{FileAppStore, FileVault};

#[cfg(test)]
mod tests;
