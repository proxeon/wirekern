//! Creative formats, CTA values, asset uploads, and preview requests.
use crate::types::Site;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use super::helpers::{
    require_https_url, require_name, require_numeric_id, require_text, validate_account,
};

/// Image-link CTAs Meta documents on `AdCreativeLinkDataCallToAction`.
/// Website types share `value.link` with the destination URL. Page-click
/// types send `value.page`. `get_directions` and `install_app` need extra
/// typed fields (geo link / application + app link) or they fail locally.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkCallToAction {
    #[default]
    LearnMore,
    ShopNow,
    SignUp,
    Download,
    ApplyNow,
    BookNow,
    Subscribe,
    BuyNow,
    ContactUs,
    GetQuote,
    OrderNow,
    CallNow,
    LikePage,
    WhatsAppMessage,
    GetDirections,
    InstallApp,
}

impl LinkCallToAction {
    pub fn meta_value(self) -> &'static str {
        match self {
            Self::LearnMore => "LEARN_MORE",
            Self::ShopNow => "SHOP_NOW",
            Self::SignUp => "SIGN_UP",
            Self::Download => "DOWNLOAD",
            Self::ApplyNow => "APPLY_NOW",
            Self::BookNow => "BOOK_NOW",
            Self::Subscribe => "SUBSCRIBE",
            Self::BuyNow => "BUY_NOW",
            Self::ContactUs => "CONTACT_US",
            Self::GetQuote => "GET_QUOTE",
            Self::OrderNow => "ORDER_NOW",
            Self::CallNow => "CALL_NOW",
            Self::LikePage => "LIKE_PAGE",
            Self::WhatsAppMessage => "WHATSAPP_MESSAGE",
            Self::GetDirections => "GET_DIRECTIONS",
            Self::InstallApp => "INSTALL_MOBILE_APP",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LearnMore => "learn_more",
            Self::ShopNow => "shop_now",
            Self::SignUp => "sign_up",
            Self::Download => "download",
            Self::ApplyNow => "apply_now",
            Self::BookNow => "book_now",
            Self::Subscribe => "subscribe",
            Self::BuyNow => "buy_now",
            Self::ContactUs => "contact_us",
            Self::GetQuote => "get_quote",
            Self::OrderNow => "order_now",
            Self::CallNow => "call_now",
            Self::LikePage => "like_page",
            Self::WhatsAppMessage => "whatsapp_message",
            Self::GetDirections => "get_directions",
            Self::InstallApp => "install_app",
        }
    }

    /// Page-identity CTAs: Meta's value uses `page`, not a destination link.
    pub fn uses_page(self) -> bool {
        matches!(self, Self::LikePage | Self::CallNow)
    }

    pub fn requires_geo_link(self) -> bool {
        matches!(self, Self::GetDirections)
    }

    pub fn requires_app(self) -> bool {
        matches!(self, Self::InstallApp)
    }
}

impl FromStr for LinkCallToAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "learn_more" | "LEARN_MORE" => Ok(Self::LearnMore),
            "shop_now" | "SHOP_NOW" => Ok(Self::ShopNow),
            "sign_up" | "SIGN_UP" => Ok(Self::SignUp),
            "download" | "DOWNLOAD" => Ok(Self::Download),
            "apply_now" | "APPLY_NOW" => Ok(Self::ApplyNow),
            "book_now" | "BOOK_NOW" => Ok(Self::BookNow),
            "subscribe" | "SUBSCRIBE" => Ok(Self::Subscribe),
            "buy_now" | "BUY_NOW" => Ok(Self::BuyNow),
            "contact_us" | "CONTACT_US" => Ok(Self::ContactUs),
            "get_quote" | "GET_QUOTE" => Ok(Self::GetQuote),
            "order_now" | "ORDER_NOW" => Ok(Self::OrderNow),
            "call_now" | "CALL_NOW" => Ok(Self::CallNow),
            "like_page" | "LIKE_PAGE" => Ok(Self::LikePage),
            "whatsapp_message" | "WHATSAPP_MESSAGE" => Ok(Self::WhatsAppMessage),
            "get_directions" | "GET_DIRECTIONS" => Ok(Self::GetDirections),
            "install_app" | "INSTALL_MOBILE_APP" | "INSTALL_APP" => Ok(Self::InstallApp),
            other => Err(format!("unknown_link_call_to_action:{other}")),
        }
    }
}

/// The initial preview placements are deliberately a small closed set. Meta
/// exposes many placement names, but accepting arbitrary strings would turn a
/// typo into a remote error and would suggest a placement is safe to review
/// before Wirekern has tested its rendering contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdPreviewFormat {
    DesktopFeedStandard,
    MobileFeedStandard,
    /// WhatsApp Status placement. Documented on generatepreviews `ad_format`.
    WhatsappStatusMedia,
}

impl AdPreviewFormat {
    pub fn meta_value(self) -> &'static str {
        match self {
            Self::DesktopFeedStandard => "DESKTOP_FEED_STANDARD",
            Self::MobileFeedStandard => "MOBILE_FEED_STANDARD",
            Self::WhatsappStatusMedia => "WHATSAPP_STATUS_MEDIA",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::DesktopFeedStandard => "desktop_feed_standard",
            Self::MobileFeedStandard => "mobile_feed_standard",
            Self::WhatsappStatusMedia => "whatsapp_status_media",
        }
    }
}

impl FromStr for AdPreviewFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "desktop_feed_standard" => Ok(Self::DesktopFeedStandard),
            "mobile_feed_standard" => Ok(Self::MobileFeedStandard),
            "whatsapp_status_media" => Ok(Self::WhatsappStatusMedia),
            other => Err(format!("unknown_ad_preview_format:{other}")),
        }
    }
}

/// The bytes for one account-scoped ad image. The library takes bytes rather
/// than a filesystem path so its core remains usable by non-CLI callers; the
/// CLI reads the operator-selected file and deliberately never reports its
/// local path in an error.
#[derive(Clone, Debug, PartialEq)]
pub struct UploadAdImageRequest {
    pub account: Option<String>,
    pub filename: String,
    pub bytes: Vec<u8>,
}

impl UploadAdImageRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_account(self.account.as_deref())?;
        if self.filename.trim().is_empty()
            || self.filename.contains('/')
            || self.filename.contains('\\')
        {
            return Err("invalid_image_filename".into());
        }
        if self.bytes.is_empty() {
            return Err("image_file_empty".into());
        }
        Ok(())
    }
}

/// Bytes for one account-scoped ad video. Same path-free contract as images:
/// the CLI reads the file; the library never sees a filesystem path.
#[derive(Clone, Debug, PartialEq)]
pub struct UploadAdVideoRequest {
    pub account: Option<String>,
    pub filename: String,
    pub bytes: Vec<u8>,
}

impl UploadAdVideoRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_account(self.account.as_deref())?;
        if self.filename.trim().is_empty()
            || self.filename.contains('/')
            || self.filename.contains('\\')
        {
            return Err("invalid_video_filename".into());
        }
        if self.bytes.is_empty() {
            return Err("video_file_empty".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UploadedAdVideo {
    pub site: Site,
    pub account_id: String,
    pub id: String,
}

/// Meta `status.video_status`: ready, processing, uploading, error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdVideoStatusKind {
    Ready,
    Processing,
    Uploading,
    Error,
}

impl AdVideoStatusKind {
    pub fn from_meta(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ready" => Self::Ready,
            "error" => Self::Error,
            "uploading" => Self::Uploading,
            _ => Self::Processing,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Processing => "processing",
            Self::Uploading => "uploading",
            Self::Error => "error",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Ready | Self::Error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdVideoStatusRequest {
    pub video_id: String,
}

impl AdVideoStatusRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("video_id", &self.video_id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdVideoStatus {
    pub site: Site,
    pub video_id: String,
    pub video_status: AdVideoStatusKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

/// Bounded poll result. Pending at deadline is not a delivery claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AdVideoWait {
    Ready(AdVideoStatus),
    Error(AdVideoStatus),
    Pending(AdVideoStatus),
}

/// Page-backed video creative. Thumbnail `image_hash` is required so Meta
/// does not invent a frame. `video_id` is the `/advideos` asset.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VideoAdCreative {
    pub name: String,
    pub page_id: String,
    pub video_id: String,
    pub image_hash: String,
    pub message: String,
    pub destination_url: String,
    pub call_to_action: LinkCallToAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geo_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instagram_user_id: Option<String>,
    #[serde(default)]
    pub advantage_plus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_identity: Option<WhatsAppStatusIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateVideoAdCreativeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub creative: VideoAdCreative,
}

impl CreateVideoAdCreativeRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_account(self.account.as_deref())?;
        require_name(&self.creative.name)?;
        require_numeric_id("page_id", &self.creative.page_id)?;
        require_numeric_id("video_id", &self.creative.video_id)?;
        require_text("image_hash", &self.creative.image_hash)?;
        require_text("message", &self.creative.message)?;
        require_https_url("destination_url", &self.creative.destination_url)?;
        validate_link_cta_values(&LinkAdCreative {
            name: self.creative.name.clone(),
            page_id: self.creative.page_id.clone(),
            image_hash: self.creative.image_hash.clone(),
            message: self.creative.message.clone(),
            headline: String::new(),
            destination_url: self.creative.destination_url.clone(),
            call_to_action: self.creative.call_to_action,
            geo_link: self.creative.geo_link.clone(),
            application_id: self.creative.application_id.clone(),
            app_link: self.creative.app_link.clone(),
            instagram_user_id: self.creative.instagram_user_id.clone(),
            advantage_plus: self.creative.advantage_plus,
            whatsapp_identity: self.creative.whatsapp_identity.clone(),
        })?;
        validate_creative_identity(
            self.creative.instagram_user_id.as_deref(),
            self.creative.whatsapp_identity.as_ref(),
        )?;
        validate_status_creative_compat(
            self.creative.whatsapp_identity.as_ref(),
            self.creative.advantage_plus,
            true,
        )?;
        Ok(())
    }
}

/// One carousel card. Meta allows 2–10; 3+ is recommended.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CarouselCard {
    pub image_hash: String,
    pub link: String,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CarouselAdCreative {
    pub name: String,
    pub page_id: String,
    pub message: String,
    pub call_to_action: LinkCallToAction,
    pub cards: Vec<CarouselCard>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instagram_user_id: Option<String>,
    #[serde(default)]
    pub advantage_plus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_identity: Option<WhatsAppStatusIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CatalogAdCreative {
    pub name: String,
    pub page_id: String,
    pub product_set_id: String,
    pub link: String,
    pub message: String,
    pub call_to_action: LinkCallToAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instagram_user_id: Option<String>,
    #[serde(default)]
    pub advantage_plus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_identity: Option<WhatsAppStatusIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LeadFormAdCreative {
    pub name: String,
    pub page_id: String,
    pub image_hash: String,
    pub message: String,
    pub headline: String,
    pub destination_url: String,
    pub lead_gen_form_id: String,
    pub call_to_action: LinkCallToAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instagram_user_id: Option<String>,
    #[serde(default)]
    pub advantage_plus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_identity: Option<WhatsAppStatusIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppInstallAdCreative {
    pub name: String,
    pub page_id: String,
    pub image_hash: String,
    pub message: String,
    pub application_id: String,
    pub object_store_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instagram_user_id: Option<String>,
    #[serde(default)]
    pub advantage_plus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_identity: Option<WhatsAppStatusIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdCreativeKind {
    Carousel(CarouselAdCreative),
    Catalog(CatalogAdCreative),
    LeadForm(LeadFormAdCreative),
    AppInstall(AppInstallAdCreative),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateAdCreativeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub kind: AdCreativeKind,
}

impl CreateAdCreativeRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_account(self.account.as_deref())?;
        match &self.kind {
            AdCreativeKind::Carousel(c) => {
                require_name(&c.name)?;
                require_numeric_id("page_id", &c.page_id)?;
                require_text("message", &c.message)?;
                if c.cards.len() < 2 || c.cards.len() > 10 {
                    return Err("carousel_cards_out_of_range".into());
                }
                for (i, card) in c.cards.iter().enumerate() {
                    require_text("image_hash", &card.image_hash)
                        .map_err(|_| format!("missing_card_image_hash:{i}"))?;
                    require_https_url("link", &card.link)?;
                    require_text("name", &card.name)
                        .map_err(|_| format!("missing_card_name:{i}"))?;
                }
                validate_creative_identity(
                    c.instagram_user_id.as_deref(),
                    c.whatsapp_identity.as_ref(),
                )?;
                validate_status_creative_compat(
                    c.whatsapp_identity.as_ref(),
                    c.advantage_plus,
                    false,
                )?;
            }
            AdCreativeKind::Catalog(c) => {
                require_name(&c.name)?;
                require_numeric_id("page_id", &c.page_id)?;
                require_numeric_id("product_set_id", &c.product_set_id)?;
                require_https_url("link", &c.link)?;
                require_text("message", &c.message)?;
                validate_creative_identity(
                    c.instagram_user_id.as_deref(),
                    c.whatsapp_identity.as_ref(),
                )?;
                validate_status_creative_compat(
                    c.whatsapp_identity.as_ref(),
                    c.advantage_plus,
                    false,
                )?;
            }
            AdCreativeKind::LeadForm(c) => {
                require_name(&c.name)?;
                require_numeric_id("page_id", &c.page_id)?;
                require_text("image_hash", &c.image_hash)?;
                require_text("message", &c.message)?;
                require_text("headline", &c.headline)?;
                require_https_url("destination_url", &c.destination_url)?;
                require_numeric_id("lead_gen_form_id", &c.lead_gen_form_id)?;
                if !matches!(
                    c.call_to_action,
                    LinkCallToAction::SignUp
                        | LinkCallToAction::ApplyNow
                        | LinkCallToAction::LearnMore
                        | LinkCallToAction::Download
                ) {
                    return Err("lead_form_cta_unsupported".into());
                }
                validate_creative_identity(
                    c.instagram_user_id.as_deref(),
                    c.whatsapp_identity.as_ref(),
                )?;
                validate_status_creative_compat(
                    c.whatsapp_identity.as_ref(),
                    c.advantage_plus,
                    true,
                )?;
            }
            AdCreativeKind::AppInstall(c) => {
                require_name(&c.name)?;
                require_numeric_id("page_id", &c.page_id)?;
                require_text("image_hash", &c.image_hash)?;
                require_text("message", &c.message)?;
                require_numeric_id("application_id", &c.application_id)?;
                require_https_url("object_store_url", &c.object_store_url)?;
                validate_creative_identity(
                    c.instagram_user_id.as_deref(),
                    c.whatsapp_identity.as_ref(),
                )?;
                validate_status_creative_compat(
                    c.whatsapp_identity.as_ref(),
                    c.advantage_plus,
                    true,
                )?;
            }
        }
        Ok(())
    }
}

/// The exact input for a static image website creative. It purposefully has
/// no implicit Page, media, copy, destination, or CTA: these determine the
/// future ad even though the creative alone cannot deliver.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinkAdCreative {
    pub name: String,
    pub page_id: String,
    pub image_hash: String,
    pub message: String,
    pub headline: String,
    pub destination_url: String,
    pub call_to_action: LinkCallToAction,
    /// Required for `get_directions` (`fbgeo://…` or HTTPS maps). Refused on
    /// every other CTA so a leftover geo link cannot hitch a ride.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geo_link: Option<String>,
    /// Required with `app_link` for `install_app`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_link: Option<String>,
    /// Instagram professional account that the ad posts as. Numeric Graph ID,
    /// not the deprecated `instagram_actor_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instagram_user_id: Option<String>,
    /// `degrees_of_freedom_spec.standard_enhancements` OPT_IN.
    #[serde(default)]
    pub advantage_plus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_identity: Option<WhatsAppStatusIdentity>,
}

/// Third-party WhatsApp Status creatives must name the identity (v26.0).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppStatusIdentity {
    pub identity_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
}

impl WhatsAppStatusIdentity {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("wamo_whatsapp_identity_id", &self.identity_id)
    }

    pub fn meta_json(&self) -> serde_json::Value {
        let mut obj = serde_json::json!({ "wamo_whatsapp_identity_id": self.identity_id });
        if let Some(phone) = &self.phone_number {
            obj["whatsapp_phone_number"] = serde_json::Value::String(phone.clone());
        }
        obj
    }
}

/// Account override plus one image-link creative. Unlike a campaign/ad set/ad
/// this object has no status because Meta creatives cannot independently
/// deliver; only the later ad object is structurally forced to `PAUSED`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateLinkAdCreativeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub creative: LinkAdCreative,
}

impl CreateLinkAdCreativeRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_account(self.account.as_deref())?;
        require_name(&self.creative.name)?;
        require_numeric_id("page_id", &self.creative.page_id)?;
        require_text("image_hash", &self.creative.image_hash)?;
        require_text("message", &self.creative.message)?;
        require_text("headline", &self.creative.headline)?;
        require_https_url("destination_url", &self.creative.destination_url)?;
        validate_link_cta_values(&self.creative)?;
        validate_creative_identity(
            self.creative.instagram_user_id.as_deref(),
            self.creative.whatsapp_identity.as_ref(),
        )?;
        validate_status_creative_compat(
            self.creative.whatsapp_identity.as_ref(),
            self.creative.advantage_plus,
            true,
        )?;
        Ok(())
    }
}

pub fn validate_status_creative_compat(
    whatsapp_identity: Option<&WhatsAppStatusIdentity>,
    advantage_plus: bool,
    allows_status: bool,
) -> Result<(), String> {
    if whatsapp_identity.is_none() {
        return Ok(());
    }
    // Status docs: carousel/collection/flexible formats and Advantage+
    // creative tools are not supported on this placement.
    if !allows_status {
        return Err("whatsapp_status_unsupported_creative_format".into());
    }
    if advantage_plus {
        return Err("whatsapp_status_incompatible_with_advantage_plus".into());
    }
    Ok(())
}

pub fn validate_creative_identity(
    instagram_user_id: Option<&str>,
    whatsapp_identity: Option<&WhatsAppStatusIdentity>,
) -> Result<(), String> {
    if let Some(id) = instagram_user_id {
        require_numeric_id("instagram_user_id", id)?;
    }
    if let Some(ident) = whatsapp_identity {
        ident.validate()?;
    }
    Ok(())
}

/// Extra CTA value fields must match the type. Meta's call-to-action value
/// object has no phone-number field; CALL_NOW/LIKE_PAGE/WHATSAPP_MESSAGE use
/// `page`. GET_DIRECTIONS needs a geo link; INSTALL_MOBILE_APP needs the app.
pub fn validate_link_cta_values(creative: &LinkAdCreative) -> Result<(), String> {
    let cta = creative.call_to_action;
    if cta.requires_geo_link() {
        let geo = creative
            .geo_link
            .as_deref()
            .ok_or_else(|| "missing_geo_link".to_string())?;
        if geo.starts_with("fbgeo://") {
            if geo.trim().len() <= "fbgeo://".len() {
                return Err("missing_geo_link".into());
            }
        } else {
            require_https_url("geo_link", geo)?;
        }
    } else if creative.geo_link.is_some() {
        return Err("geo_link_without_get_directions".into());
    }
    if cta.requires_app() {
        let application_id = creative
            .application_id
            .as_deref()
            .ok_or_else(|| "missing_application_id".to_string())?;
        require_numeric_id("application_id", application_id)?;
        let app_link = creative
            .app_link
            .as_deref()
            .ok_or_else(|| "missing_app_link".to_string())?;
        require_https_url("app_link", app_link)?;
    } else if creative.application_id.is_some() || creative.app_link.is_some() {
        return Err("app_fields_without_install_app".into());
    }
    Ok(())
}

/// Meta `call_to_action.value` for this image-link creative.
pub fn link_cta_value_json(creative: &LinkAdCreative) -> serde_json::Value {
    if creative.call_to_action == LinkCallToAction::WhatsAppMessage {
        // Status / click-to-WhatsApp: Meta's documented value is
        // `app_destination=whatsapp`, not `page`.
        serde_json::json!({ "app_destination": "whatsapp" })
    } else if creative.call_to_action.uses_page() {
        serde_json::json!({ "page": creative.page_id })
    } else if creative.call_to_action.requires_geo_link() {
        serde_json::json!({ "link": creative.geo_link.as_deref().unwrap_or_default() })
    } else if creative.call_to_action.requires_app() {
        serde_json::json!({
            "application": creative.application_id.as_deref().unwrap_or_default(),
            "app_link": creative.app_link.as_deref().unwrap_or_default(),
            "link": creative.destination_url,
        })
    } else {
        serde_json::json!({ "link": creative.destination_url })
    }
}

/// Request a render for an existing Meta creative. An ad creative ID is
/// globally addressable by Graph, so the preview edge has no account path or
/// account override; the selected credential is still required to read it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreativePreviewRequest {
    pub creative_id: String,
    pub ad_format: AdPreviewFormat,
}

impl CreativePreviewRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("creative_id", &self.creative_id)
    }
}
