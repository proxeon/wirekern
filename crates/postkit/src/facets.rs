//! Optional connector facets. The kernel seam is [`Publisher`](crate::Publisher).
//!
//! A new site implements `Publisher` (publish + auth). Extra verbs — insights,
//! ads, pages, media, messaging — are separate traits attached on
//! [`Connector`](crate::Connector). Client looks up the facet, not a default
//! method on `Publisher`, so the next connector cannot enlarge the kernel
//! trait by accident.

use crate::ads::{
    AdReviewStatus, AdReviewStatusRequest, AdsTokenInspection, CreateLinkAdCreativeRequest,
    CreatePausedAdRequest, CreatedAd, CreatedAdCreative, CreativePreview, CreativePreviewRequest,
    AdVideoStatus, AdVideoStatusRequest, CreateVideoAdCreativeRequest, MarketingApiAccessTier,
    UploadAdImageRequest, UploadAdVideoRequest, UploadedAdImage, UploadedAdVideo,
};
use crate::error::Error;
use crate::insights::{AdAccountsReply, InsightsJob, InsightsQuery, InsightsReply};
use crate::media::{MediaQuery, MediaReply};
use crate::pages::PagesReply;
#[cfg(feature = "whatsapp-cloud")]
use crate::types::Outcome;
use crate::types::{AccountCreds, AppConfig, Capability, Deadline, Site};
#[cfg(feature = "whatsapp-cloud")]
use crate::whatsapp::{
    WhatsAppFlowDraft, WhatsAppFlowList, WhatsAppFlowRecord, WhatsAppMediaMeta,
    WhatsAppMediaUpload, WhatsAppPageQuery, WhatsAppPhoneNumber, WhatsAppPhoneNumberList,
    WhatsAppSendRequest, WhatsAppSystemUserList, WhatsAppTemplateDraft, WhatsAppTemplateList,
    WhatsAppTemplateQuery, WhatsAppTemplateRecord, WhatsAppUploadedMedia, WhatsAppWabaList,
};
use async_trait::async_trait;

/// Spend/performance reads. Distinct from publish because a metrics-only
/// connector must not grow a write path merely by sharing one trait.
#[async_trait]
pub trait InsightsSource: Send + Sync {
    async fn insights(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        query: &InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsReply, Error>;

    async fn ad_accounts(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<AdAccountsReply, Error>;

    async fn start_insights_job(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _query: &InsightsQuery,
        _deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        Err(Error::UnsupportedCapability {
            site: Site::new(""),
            need: Capability::ReadMetrics,
        })
    }

    async fn insights_job(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _job_id: &str,
        _deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        Err(Error::UnsupportedCapability {
            site: Site::new(""),
            need: Capability::ReadMetrics,
        })
    }

    async fn insights_job_result(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _job_id: &str,
        _query: &InsightsQuery,
        _deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
        Err(Error::UnsupportedCapability {
            site: Site::new(""),
            need: Capability::ReadMetrics,
        })
    }

    async fn cancel_insights_job(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _job_id: &str,
        _deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        Err(Error::UnsupportedCapability {
            site: Site::new(""),
            need: Capability::ReadMetrics,
        })
    }
}

/// Advertising-management writes and review reads. Policy still sits in
/// `Client`; this trait is only the wire. Activation is not a method here
/// so a future spend verb cannot hide as a default on `Publisher`.
#[async_trait]
pub trait AdsManager: Send + Sync {
    async fn create_paused_ad(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        request: &CreatePausedAdRequest,
        deadline: Deadline,
    ) -> Result<CreatedAd, Error>;

    async fn upload_ad_image(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        request: &UploadAdImageRequest,
        deadline: Deadline,
    ) -> Result<UploadedAdImage, Error>;

    async fn upload_ad_video(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        request: &UploadAdVideoRequest,
        deadline: Deadline,
    ) -> Result<UploadedAdVideo, Error>;

    async fn ad_video_status(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        request: &AdVideoStatusRequest,
        deadline: Deadline,
    ) -> Result<AdVideoStatus, Error>;

    async fn create_link_ad_creative(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        request: &CreateLinkAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error>;

    async fn create_video_ad_creative(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        request: &CreateVideoAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error>;

    async fn preview_ad_creative(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        request: &CreativePreviewRequest,
        deadline: Deadline,
    ) -> Result<CreativePreview, Error>;

    async fn ad_review_status(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        request: &AdReviewStatusRequest,
        deadline: Deadline,
    ) -> Result<AdReviewStatus, Error>;

    /// Store a Business Manager System User token. Default refuses so a
    /// paused-create mock cannot accidentally grow an auth path.
    async fn bootstrap_system_user_token(
        &self,
        _app: &AppConfig,
        _token: &str,
        _deadline: Deadline,
    ) -> Result<AccountCreds, Error> {
        Err(Error::UnsupportedCapability {
            site: Site::new(""),
            need: Capability::ReadAdAccounts,
        })
    }

    async fn inspect_access_token(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _deadline: Deadline,
    ) -> Result<AdsTokenInspection, Error> {
        Err(Error::UnsupportedCapability {
            site: Site::new(""),
            need: Capability::ReadAdAccounts,
        })
    }

    async fn marketing_api_access_tier(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _deadline: Deadline,
    ) -> Result<MarketingApiAccessTier, Error> {
        Err(Error::UnsupportedCapability {
            site: Site::new(""),
            need: Capability::ReadAdAccounts,
        })
    }
}

/// Page identity discovery. Separate from ad-account listing: a Page is an
/// organic publish target, not a property of an advertising account.
#[async_trait]
pub trait PageDirectory: Send + Sync {
    async fn pages(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<PagesReply, Error>;
}

/// Bounded first-page read of already-published media. A write-only social
/// connector must not become a profile reader just by accepting images.
#[async_trait]
pub trait MediaReader: Send + Sync {
    async fn media(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        query: &MediaQuery,
        deadline: Deadline,
    ) -> Result<MediaReply, Error>;
}

/// Typed private business messaging. Deliberately not `publish`: recipient,
/// reply context, template approval and billing cannot share `Intent`.
#[cfg(feature = "whatsapp-cloud")]
#[async_trait]
pub trait WhatsAppSender: Send + Sync {
    async fn send_whatsapp(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        request: &WhatsAppSendRequest,
        deadline: Deadline,
    ) -> Result<Outcome, Error>;
}

/// Cloud API media upload/get/download/delete. Distinct from `WhatsAppSender`
/// so a send-only mock does not have to fake Graph multipart.
#[cfg(feature = "whatsapp-cloud")]
#[async_trait]
pub trait WhatsAppAssets: Send + Sync {
    async fn upload_media(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        upload: &WhatsAppMediaUpload,
        deadline: Deadline,
    ) -> Result<WhatsAppUploadedMedia, Error>;

    async fn media_metadata(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<WhatsAppMediaMeta, Error>;

    async fn download_media(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<Vec<u8>, Error>;

    async fn delete_media(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<(), Error>;
}

/// Business Management API template lifecycle. Distinct from `WhatsAppSender`
/// so sending an approved template does not imply WABA write access.
#[cfg(feature = "whatsapp-cloud")]
#[async_trait]
pub trait WhatsAppTemplates: Send + Sync {
    async fn list_templates(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        query: &WhatsAppTemplateQuery,
        deadline: Deadline,
    ) -> Result<WhatsAppTemplateList, Error>;

    async fn get_template(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        template_id: &str,
        deadline: Deadline,
    ) -> Result<WhatsAppTemplateRecord, Error>;

    /// Create submits the template for Meta review (`status` starts PENDING).
    async fn create_template(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        draft: &WhatsAppTemplateDraft,
        deadline: Deadline,
    ) -> Result<WhatsAppTemplateRecord, Error>;

    async fn edit_template(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        template_id: &str,
        draft: &WhatsAppTemplateDraft,
        deadline: Deadline,
    ) -> Result<WhatsAppTemplateRecord, Error>;

    async fn delete_template(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        name: &str,
        deadline: Deadline,
    ) -> Result<(), Error>;
}

/// WhatsApp Flows endpoint (not `/messages`). List/get expose publishing
/// state; create/publish write the schema. Distinct from sending a Flow CTA.
#[cfg(feature = "whatsapp-cloud")]
#[async_trait]
pub trait WhatsAppFlows: Send + Sync {
    async fn list_flows(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        query: &WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<WhatsAppFlowList, Error>;

    async fn get_flow(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        flow_id: &str,
        deadline: Deadline,
    ) -> Result<WhatsAppFlowRecord, Error>;

    async fn create_flow(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        draft: &WhatsAppFlowDraft,
        deadline: Deadline,
    ) -> Result<WhatsAppFlowRecord, Error>;

    async fn publish_flow(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        flow_id: &str,
        deadline: Deadline,
    ) -> Result<WhatsAppFlowRecord, Error>;
}

/// WABA/phone reads and Cloud API registration. Distinct from sending.
#[cfg(feature = "whatsapp-cloud")]
#[async_trait]
pub trait WhatsAppAccount: Send + Sync {
    async fn list_wabas(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        query: &WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<WhatsAppWabaList, Error>;

    async fn list_phone_numbers(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        query: &WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<WhatsAppPhoneNumberList, Error>;

    async fn phone_health(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<WhatsAppPhoneNumber, Error>;

    async fn subscribe_apps(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<(), Error>;

    async fn register_phone(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        pin: &str,
        deadline: Deadline,
    ) -> Result<(), Error>;

    async fn set_two_step_pin(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        pin: &str,
        deadline: Deadline,
    ) -> Result<(), Error>;

    async fn list_system_users(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        query: &WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<WhatsAppSystemUserList, Error>;
}
