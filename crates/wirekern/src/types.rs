use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{Duration, Instant};

/// Open id. Connectors register themselves; core has no closed site enum.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Site(pub String);

impl Site {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Site {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Which credentials to use. Not where the post lands (`Intent.params`).
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct AccountKey {
    pub site: Site,
    /// Operator alias, not necessarily the platform handle.
    pub name: String,
}

impl AccountKey {
    pub fn new(site: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            site: Site(site.into()),
            name: name.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Capability {
    #[serde(rename = "publish.text")]
    PublishText,
    #[serde(rename = "publish.image")]
    PublishImage,
    #[serde(rename = "publish.carousel")]
    PublishCarousel,
    #[serde(rename = "publish.video")]
    PublishVideo,
    #[serde(rename = "send.reply")]
    SendReply,
    /// A private direct message whose transport and consent model do not
    /// match a public post or Meta's customer-service message classes.
    #[serde(rename = "send.direct_message")]
    SendDirectMessage,
    /// In-window service text with no `context` (Meta customer-service window).
    #[serde(rename = "send.text")]
    SendText,
    #[serde(rename = "send.template")]
    SendTemplate,
    #[serde(rename = "send.media")]
    SendMedia,
    #[serde(rename = "send.interactive")]
    SendInteractive,
    #[serde(rename = "send.location")]
    SendLocation,
    #[serde(rename = "send.contacts")]
    SendContacts,
    #[serde(rename = "send.reaction")]
    SendReaction,
    #[serde(rename = "send.read")]
    MarkRead,
    #[serde(rename = "send.typing")]
    SendTyping,
    #[serde(rename = "send.catalog")]
    SendCatalog,
    #[serde(rename = "send.flow")]
    SendFlow,
    #[serde(rename = "read.flows")]
    ReadFlows,
    #[serde(rename = "manage.flows")]
    ManageFlows,
    #[serde(rename = "read.whatsapp_account")]
    ReadWhatsAppAccount,
    #[serde(rename = "manage.whatsapp_phone")]
    ManageWhatsAppPhone,
    #[serde(rename = "manage.whatsapp_media")]
    ManageWhatsAppMedia,
    #[serde(rename = "read.whatsapp_media")]
    ReadWhatsAppMedia,
    #[serde(rename = "read.templates")]
    ReadTemplates,
    #[serde(rename = "manage.templates")]
    ManageTemplates,
    #[serde(rename = "read.webhook_messages")]
    ReadWebhookMessages,
    #[serde(rename = "read.webhook_statuses")]
    ReadWebhookStatuses,
    #[serde(rename = "read.metrics")]
    ReadMetrics,
    #[serde(rename = "read.ad_accounts")]
    ReadAdAccounts,
    #[serde(rename = "read.pages")]
    ReadPages,
    #[serde(rename = "read.media")]
    ReadMedia,
    #[serde(rename = "read.ad_previews")]
    ReadAdPreviews,
    #[serde(rename = "read.ad_review_status")]
    ReadAdReviewStatus,
    #[serde(rename = "read.ads_inventory")]
    ReadAdsInventory,
    #[serde(rename = "create.paused_ads")]
    CreatePausedAds,
    #[serde(rename = "create.ad_creative")]
    CreateAdCreative,
    #[serde(rename = "manage.ads_lifecycle")]
    ManageAdsLifecycle,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PublishText => "publish.text",
            Self::PublishImage => "publish.image",
            Self::PublishCarousel => "publish.carousel",
            Self::PublishVideo => "publish.video",
            Self::SendReply => "send.reply",
            Self::SendDirectMessage => "send.direct_message",
            Self::SendText => "send.text",
            Self::SendTemplate => "send.template",
            Self::SendMedia => "send.media",
            Self::SendInteractive => "send.interactive",
            Self::SendLocation => "send.location",
            Self::SendContacts => "send.contacts",
            Self::SendReaction => "send.reaction",
            Self::MarkRead => "send.read",
            Self::SendTyping => "send.typing",
            Self::SendCatalog => "send.catalog",
            Self::SendFlow => "send.flow",
            Self::ReadFlows => "read.flows",
            Self::ManageFlows => "manage.flows",
            Self::ReadWhatsAppAccount => "read.whatsapp_account",
            Self::ManageWhatsAppPhone => "manage.whatsapp_phone",
            Self::ManageWhatsAppMedia => "manage.whatsapp_media",
            Self::ReadWhatsAppMedia => "read.whatsapp_media",
            Self::ReadTemplates => "read.templates",
            Self::ManageTemplates => "manage.templates",
            Self::ReadWebhookMessages => "read.webhook_messages",
            Self::ReadWebhookStatuses => "read.webhook_statuses",
            Self::ReadMetrics => "read.metrics",
            Self::ReadAdAccounts => "read.ad_accounts",
            Self::ReadPages => "read.pages",
            Self::ReadMedia => "read.media",
            Self::ReadAdPreviews => "read.ad_previews",
            Self::ReadAdReviewStatus => "read.ad_review_status",
            Self::ReadAdsInventory => "read.ads_inventory",
            Self::CreatePausedAds => "create.paused_ads",
            Self::CreateAdCreative => "create.ad_creative",
            Self::ManageAdsLifecycle => "manage.ads_lifecycle",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub site: Site,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthApp>,
    #[serde(default)]
    pub extra: serde_json::Value,
}

impl std::fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppConfig")
            .field("site", &self.site)
            .field("oauth", &self.oauth)
            // Connector extras may carry non-OAuth secrets such as a
            // WhatsApp webhook app secret. Treat the extension bag as opaque
            // instead of trusting every future connector to redact fields.
            .field("extra", &"[opaque]")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthApp {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

/// A short-lived OAuth PKCE authorization session retained only until the
/// authorization server redirects back. The verifier is a credential: its
/// debug output is deliberately redacted and file-backed vaults store it
/// owner-only just like an access token.
#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthPkceSession {
    pub state: String,
    pub code_verifier: String,
    pub expires_at: u64,
}

impl std::fmt::Debug for OAuthPkceSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthPkceSession")
            .field("state", &self.state)
            .field("code_verifier", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl std::fmt::Debug for OAuthApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthApp")
            .field("client_id", &self.client_id)
            .field("client_secret", &"[redacted]")
            .field("redirect_uri", &self.redirect_uri)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccountCreds {
    OAuth2 {
        access_token: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refresh_token: Option<String>,
        #[serde(default)]
        extra: serde_json::Value,
    },
    AppPassword {
        identifier: String,
        secret: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pds: Option<String>,
    },
    BotToken {
        token: String,
    },
}

impl std::fmt::Debug for AccountCreds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OAuth2 { extra, .. } => f
                .debug_struct("OAuth2")
                .field("access_token", &"[redacted]")
                .field("refresh_token", &"[redacted]")
                .field("extra", extra)
                .finish(),
            Self::AppPassword {
                identifier, pds, ..
            } => f
                .debug_struct("AppPassword")
                .field("identifier", identifier)
                .field("secret", &"[redacted]")
                .field("pds", pds)
                .finish(),
            Self::BotToken { .. } => f
                .debug_struct("BotToken")
                .field("token", &"[redacted]")
                .finish(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Body {
    Text {
        text: String,
    },
    /// One image with an optional caption. The caption obeys the site's
    /// text rule when present (Threads: 500 UTF-8 bytes; Bluesky: 300
    /// graphemes; Instagram: 2,200 Unicode scalar values) and is omitted
    /// from the wire entirely when `None`.
    Image {
        text: Option<String>,
        image: Image,
        /// Accessibility text for embed-capable sites. It is part of the
        /// media, not a platform param: Bluesky embeds it (required by the
        /// lexicon, empty string allowed); Threads and Instagram v1 have no
        /// verified field for it and ignore it.
        #[serde(default)]
        alt: String,
    },
    /// A single feed carousel, not a batch of independent posts. Images keep
    /// the same URL/bytes model as `Image`, but each connector must advertise
    /// `publish.carousel` before the Client will route this distinct wire
    /// grammar to it. V1 does not carry generic alt text because one string
    /// cannot truthfully describe multiple slides.
    Carousel {
        text: Option<String>,
        images: Vec<Image>,
    },
}

impl Body {
    pub fn required_capability(&self) -> Capability {
        match self {
            Self::Text { .. } => Capability::PublishText,
            Self::Image { .. } => Capability::PublishImage,
            Self::Carousel { .. } => Capability::PublishCarousel,
        }
    }
}

/// One image for a post, in the two forms platforms actually ingest
/// (plans/001/015 D1). Bluesky uploads bytes (`uploadBlob`); Threads and
/// Instagram crawl a public https URL (`image_url`) and offer no organic
/// upload. The
/// kernel deliberately never bridges the two: fetching an operator URL is
/// an SSRF-shaped power it has never had, and hosting bytes to synthesize
/// a URL is a product decision (015 D5). Each connector refuses the form
/// it cannot honor at the door, before credentials or HTTP.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Image {
    /// Operator-selected bytes. `filename` is a bare basename — the CLI is
    /// the only filesystem boundary (the `UploadAdImageRequest` rule:
    /// local paths never reach errors or payloads).
    Bytes { filename: String, bytes: Vec<u8> },
    /// A publicly reachable https URL. Whether the target really is a
    /// supported image of legal size is the platform's definitive call —
    /// locally only the scheme is checkable.
    Url(String),
}

impl Image {
    /// Local, zero-I/O validation. Reasons are stable strings in the
    /// `invalid_post` family (exit 2).
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Bytes { filename, bytes } => {
                // A bare basename only: separators would smuggle path
                // components into the multipart filename (or an error).
                if filename.trim().is_empty() || filename.contains(['/', '\\']) {
                    return Err("invalid_image_filename".into());
                }
                if bytes.is_empty() {
                    return Err("image_file_empty".into());
                }
            }
            Self::Url(url) => {
                require_https_url("image_url", url)?;
            }
        }
        Ok(())
    }
}

/// https-only URL rule shared by every surface that takes one (posts,
/// creatives, draft manifests). A public authority is required; whitespace
/// or a missing host means the platform would only echo a less actionable
/// form error after the fact.
pub(crate) fn require_https_url(field: &str, value: &str) -> Result<(), String> {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Intent {
    pub site: Site,
    #[serde(default)]
    pub params: serde_json::Value,
    pub body: Body,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub site: Site,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<Limits>,
}

/// Result of a create-only probe (027). Deliberately NOT an `Outcome`:
/// `container_id` refers to an unpublished container, not a post — nothing
/// is live, there is no permalink, and the container expires unused after
/// 24h. Conflating the two would let a script treat a probe as a post
/// (e.g. feed the id into `reply_to_id`, which references posts, or into
/// an idempotency ledger keyed on publishes).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Probe {
    pub site: Site,
    /// Meta's creation_id: an unpublished media container. expires_in_hours
    /// after creation it disappears; wirekern never publishes it.
    pub container_id: String,
    /// How long the platform keeps the unpublished container.
    pub expires_in_hours: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Limits {
    pub remaining: Option<u32>,
    pub reset_unix: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WhoAmI {
    pub site: Site,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

/// CLI `--stdin` / HTTP body. `account` is not inside `target`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PostRequest {
    #[serde(default = "default_account")]
    pub account: String,
    pub target: serde_json::Value,
    pub body: Body,
}

fn default_account() -> String {
    "default".into()
}

impl PostRequest {
    pub fn into_key_intent(self) -> Result<(AccountKey, Intent), crate::Error> {
        let Some(site) = self.target.get("site").and_then(|v| v.as_str()) else {
            return Err(crate::Error::InvalidPost {
                site: Site::new(""),
                reason: "missing_site".into(),
                limit: None,
            });
        };
        let site = Site::new(site);
        let mut params = self.target;
        if let Some(obj) = params.as_object_mut() {
            obj.remove("site");
        }
        Ok((
            AccountKey::new(site.as_str(), self.account),
            Intent {
                site,
                params,
                body: self.body,
                idempotency_key: None,
            },
        ))
    }
}

/// Caller-owned instant. HTTP client uses `remaining()` as the request timeout.
#[derive(Clone, Copy, Debug)]
pub struct Deadline(pub Instant);

impl Deadline {
    pub fn from_secs(secs: u64) -> Self {
        Self(Instant::now() + Duration::from_secs(secs))
    }

    pub fn remaining(self) -> Duration {
        self.0.saturating_duration_since(Instant::now())
    }

    pub fn check(self, site: &Site) -> Result<(), crate::Error> {
        if Instant::now() >= self.0 {
            Err(crate::Error::DeadlineExceeded { site: site.clone() })
        } else {
            Ok(())
        }
    }
}

pub const USER_AGENT: &str = concat!("wirekern/", env!("CARGO_PKG_VERSION"));

/// Account / site file names: `[A-Za-z0-9._-]+`.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        // `.json` is the reserved file extension in the vault; a name
        // ending in it cannot round-trip through list()/get() without
        // aliasing onto another account's file.
        && !s.ends_with(".json")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}
