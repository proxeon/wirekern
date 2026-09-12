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
#[cfg(feature = "whatsapp-cloud")]
mod whatsapp_ops;

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
    billing_event_allowed, link_cta_value_json, parse_adset_datetime, promoted_object_required,
    supported_adset_pairing, validate_adset_schedule, validate_bid_constraints,
    validate_link_cta_values, validate_promoted_object, AdCreativeKind, AdEntity, AdPreviewFormat,
    AdReviewIssue, AdReviewStatus, AdReviewStatusRequest, AdReviewWait, AdTargeting, AdVideoStatus,
    AdVideoStatusKind, AdVideoStatusRequest, AdVideoWait, AdsActivateRequest, AdsArchiveRequest,
    AdsBidUpdateRequest, AdsBudgetUpdateRequest, AdsConfiguredStatus, AdsCreativeSwapRequest,
    AdsDeleteRequest, AdsDuplicateReply, AdsDuplicateRequest, AdsInspectReply, AdsInspectRequest,
    AdsInventoryItem, AdsInventoryKind, AdsInventoryReply, AdsInventoryRequest,
    AdsLifecycleCheckpoint, AdsLifecycleOutcome, AdsPauseRequest, AdsPlacementUpdateRequest,
    AdsScheduleUpdateRequest, AdsStatusUpdateRequest, AdsTargetingDiff, AdsTargetingReadback,
    AdsTargetingUpdateRequest, AdsTokenInspection, AdsTokenKind, AppInstallAdCreative, BidStrategy,
    BillingEvent, CampaignObjective, CarouselAdCreative, CarouselCard, CatalogAdCreative,
    CreateAdCreativeRequest, CreateLinkAdCreativeRequest, CreatePausedAdRequest,
    CreateVideoAdCreativeRequest, CreatedAd, CreatedAdCreative, CreativePreview,
    CreativePreviewRequest, CustomEventType, FacebookPosition, GeoLocations, InstagramPosition,
    LeadFormAdCreative, LinkAdCreative, LinkCallToAction, MarketingApiAccessTier,
    MarketingApiAccessTierKind, OptimizationGoal, PausedAd, PausedAdCreate, PausedAdset,
    PausedCampaign, PromotedObject, PublisherPlatform, UploadAdImageRequest, UploadAdVideoRequest,
    UploadedAdImage, UploadedAdVideo, VideoAdCreative, WhatsAppPosition, WhatsAppStatusIdentity,
    ACTIVATE_RECONCILE_GUIDANCE, ARCHIVE_RECONCILE_GUIDANCE, DEFAULT_BUDGET_MAX_CHANGE_RATIO,
    DELETE_RECONCILE_GUIDANCE, MARKETING_API_ACCESS_TIER_DASHBOARD, PAUSE_RECONCILE_GUIDANCE,
    SYSTEM_USER_TOKEN_KIND,
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
pub use facets::{AdsManager, InsightsSource, MediaReader, PageDirectory};
#[cfg(feature = "whatsapp-cloud")]
pub use facets::{
    WhatsAppAccount, WhatsAppAssets, WhatsAppFlows, WhatsAppSender, WhatsAppTemplates,
};
pub use insights::{
    validate_breakdowns, AdAccount, AdAccountsReply, AttributionWindow, Breakdown, DateRange,
    InsightRow, InsightsJob, InsightsJobStatus, InsightsJobWait, InsightsLevel, InsightsQuery,
    InsightsReply, InsightsReportKind, Metric, MAX_INSIGHTS_BREAKDOWNS, MAX_INSIGHTS_RESULT_ROWS,
    MAX_RANGE_DAYS,
};
pub use media::{MediaQuery, MediaReply, PublishedMedia, DEFAULT_MEDIA_LIMIT, MAX_MEDIA_LIMIT};
pub use pages::{PageAccount, PagesReply};
pub use policy::{AdsAction, AdsPolicy, AllowAdsActionPolicy, PausedOnlyAdsPolicy};
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
    embedded_signup_url, validate_two_step_pin, DeliveryConversation, DeliveryError,
    DeliveryPricing, DeliveryStatus, DeliveryStatusKind, InboundContact, InboundInteractive,
    InboundLocation, InboundMedia, InboundMessage, InboundMessages, InboundOrder, InboundReaction,
    InboundReferral, InboundUnsupported, LimitedTimeOffer, MediaRef, NamedBodyParameter,
    ParameterFormat, ProductSection, RecipientType, TemplateButton, TemplateCreateButton,
    TemplateCreateComponent, TemplateHeader, WebhookParseOptions, WhatsAppFlowDraft,
    WhatsAppFlowList, WhatsAppFlowRecord, WhatsAppMediaMeta, WhatsAppMediaUpload, WhatsAppMessage,
    WhatsAppOutboundSender, WhatsAppPageQuery, WhatsAppPhoneNumber, WhatsAppPhoneNumberList,
    WhatsAppSendRequest, WhatsAppSystemUser, WhatsAppSystemUserList, WhatsAppTemplateDraft,
    WhatsAppTemplateList, WhatsAppTemplateQuery, WhatsAppTemplateRecord, WhatsAppUploadedMedia,
    WhatsAppWaba, WhatsAppWabaList, MAX_REPLY_TEXT,
};
#[cfg(feature = "whatsapp-cloud")]
pub use whatsapp_ops::{
    customer_window_open, ingest_parsed, verify_webhook_challenge, ConsentKind, ConsentRecord,
    LedgerApply, MemoryWhatsAppConsent, MemoryWhatsAppLedger, ThroughputQueue, WhatsAppConsent,
    WhatsAppLedger, WhatsAppLedgerRecord, CUSTOMER_WINDOW_SECS, DEFAULT_THROUGHPUT_PER_SEC,
};
#[cfg(all(feature = "whatsapp-cloud", feature = "vault-file"))]
pub use whatsapp_ops::{FileWhatsAppConsent, FileWhatsAppLedger};

#[cfg(feature = "vault-file")]
pub use keys::{CreatedKey, FileKeyStore, KeyMeta, KEY_PREFIX};
#[cfg(feature = "vault-file")]
pub use vault_file::{FileAppStore, FileVault};

#[cfg(test)]
mod tests;
