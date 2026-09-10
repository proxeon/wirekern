use crate::types::{AccountKey, Capability, Site};
use serde::Serialize;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("unknown site {0}")]
    UnknownSite(Site),
    #[error("unsupported capability {}", need.as_str())]
    UnsupportedCapability { site: Site, need: Capability },
    #[error("invalid post: {reason}")]
    InvalidPost {
        site: Site,
        reason: String,
        limit: Option<u32>,
    },
    #[error("invalid query: {reason}")]
    InvalidQuery { site: Site, reason: String },
    #[error("policy denied {action}: {reason}")]
    PolicyDenied {
        site: Site,
        action: String,
        reason: String,
    },
    #[error("auth: {reason}")]
    Auth { site: Site, reason: String },
    #[error("rate limited")]
    RateLimited {
        site: Site,
        retry_after: Option<Duration>,
    },
    #[error("idempotency key in flight: {key}")]
    IdempotencyInFlight { site: Site, key: String },
    #[error("platform {code}: {message}")]
    Platform {
        site: Site,
        code: String,
        message: String,
    },
    #[error("network: {message}")]
    Network { site: Site, message: String },
    #[error("deadline exceeded")]
    DeadlineExceeded { site: Site },
    #[error("unknown account {site}/{name}", site = .0.site, name = .0.name)]
    UnknownAccount(AccountKey),
    #[error("invalid name {0}")]
    InvalidName(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Closed `"error"` tag for `--json` / future HTTP. Not `thiserror` Display.
#[derive(Debug, Serialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum WireError {
    UnknownSite {
        site: Site,
    },
    UnknownAccount {
        site: Site,
        name: String,
    },
    Unsupported {
        site: Site,
        need: String,
    },
    InvalidPost {
        site: Site,
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    InvalidQuery {
        site: Site,
        reason: String,
    },
    PolicyDenied {
        site: Site,
        action: String,
        reason: String,
    },
    Auth {
        site: Site,
        reason: String,
    },
    RateLimited {
        site: Site,
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_after: Option<u64>,
    },
    Idempotency {
        site: Site,
        key: String,
    },
    Platform {
        site: Site,
        code: String,
        message: String,
    },
    Network {
        site: Site,
    },
    Timeout {
        site: Site,
    },
}

impl From<&Error> for WireError {
    fn from(e: &Error) -> Self {
        match e {
            Error::UnknownSite(site) => Self::UnknownSite { site: site.clone() },
            Error::UnknownAccount(key) => Self::UnknownAccount {
                site: key.site.clone(),
                name: key.name.clone(),
            },
            Error::UnsupportedCapability { site, need } => Self::Unsupported {
                site: site.clone(),
                need: need.as_str().into(),
            },
            Error::InvalidPost {
                site,
                reason,
                limit,
            } => Self::InvalidPost {
                site: site.clone(),
                reason: reason.clone(),
                limit: *limit,
            },
            Error::Auth { site, reason } => Self::Auth {
                site: site.clone(),
                reason: reason.clone(),
            },
            Error::RateLimited { site, retry_after } => Self::RateLimited {
                site: site.clone(),
                retry_after: retry_after.map(|d| d.as_secs()),
            },
            Error::IdempotencyInFlight { site, key } => Self::Idempotency {
                site: site.clone(),
                key: key.clone(),
            },
            Error::Platform {
                site,
                code,
                message,
            } => Self::Platform {
                site: site.clone(),
                code: code.clone(),
                message: message.clone(),
            },
            Error::Network { site, .. } => Self::Network { site: site.clone() },
            Error::Io(_) | Error::Json(_) => Self::Network {
                site: Site::new(""),
            },
            Error::DeadlineExceeded { site } => Self::Timeout { site: site.clone() },
            Error::InvalidName(name) => Self::InvalidPost {
                site: Site::new(""),
                reason: format!("invalid_name:{name}"),
                limit: None,
            },
            Error::InvalidQuery { site, reason } => Self::InvalidQuery {
                site: site.clone(),
                reason: reason.clone(),
            },
            Error::PolicyDenied {
                site,
                action,
                reason,
            } => Self::PolicyDenied {
                site: site.clone(),
                action: action.clone(),
                reason: reason.clone(),
            },
        }
    }
}

impl WireError {
    /// 009 exit table. 0 is success (not this type).
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::UnknownSite { .. }
            | Self::UnknownAccount { .. }
            | Self::Unsupported { .. }
            | Self::InvalidPost { .. }
            | Self::InvalidQuery { .. }
            | Self::PolicyDenied { .. } => 2,
            Self::Auth { .. } => 3,
            // Transient, retry-later family: an in-flight idempotency key
            // sits with rate limiting rather than the invalid-input bucket —
            // the caller did nothing wrong, the answer is "try again soon".
            Self::RateLimited { .. } | Self::Idempotency { .. } => 4,
            Self::Platform { .. } | Self::Network { .. } | Self::Timeout { .. } => 5,
        }
    }
}

impl Error {
    /// Reqwest often renders the complete request URL in transport errors.
    /// OAuth connectors may place access tokens or client secrets in those
    /// URLs, so public errors deliberately retain none of that diagnostic.
    #[cfg(feature = "client")]
    pub(crate) fn request_failed(site: &Site) -> Self {
        Self::Network {
            site: site.clone(),
            message: "request failed".into(),
        }
    }

    pub fn exit_code(&self) -> i32 {
        WireError::from(self).exit_code()
    }
}
