//! Optional connector facets. The kernel seam is [`Publisher`](crate::Publisher).
//!
//! A new site implements `Publisher` (publish + auth). Extra verbs — insights,
//! ads, pages, media, messaging — are separate traits attached on
//! [`Connector`](crate::Connector). Client looks up the facet, not a default
//! method on `Publisher`, so the next connector cannot enlarge the kernel
//! trait by accident.

use crate::ads::{
    AdReviewStatus, AdReviewStatusRequest, CreateLinkAdCreativeRequest, CreatePausedAdRequest,
    CreatedAd, CreatedAdCreative, CreativePreview, CreativePreviewRequest, UploadAdImageRequest,
    UploadedAdImage,
};
use crate::error::Error;
use crate::insights::{AdAccountsReply, InsightsQuery, InsightsReply};
use crate::media::{MediaQuery, MediaReply};
use crate::pages::PagesReply;
#[cfg(feature = "whatsapp-cloud")]
use crate::types::Outcome;
use crate::types::{AccountCreds, AppConfig, Deadline};
#[cfg(feature = "whatsapp-cloud")]
use crate::whatsapp::{
    WhatsAppMediaMeta, WhatsAppMediaUpload, WhatsAppSendRequest, WhatsAppUploadedMedia,
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

    async fn create_link_ad_creative(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        request: &CreateLinkAdCreativeRequest,
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
