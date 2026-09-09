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
    #[serde(rename = "publish.video")]
    PublishVideo,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PublishText => "publish.text",
            Self::PublishImage => "publish.image",
            Self::PublishVideo => "publish.video",
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
            .field("extra", &self.extra)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthApp {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
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
    Text { text: String },
}

impl Body {
    pub fn required_capability(&self) -> Capability {
        match self {
            Self::Text { .. } => Capability::PublishText,
        }
    }
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
    /// after creation it disappears; postkit never publishes it.
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

pub const USER_AGENT: &str = concat!("postkit/", env!("CARGO_PKG_VERSION"));

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
