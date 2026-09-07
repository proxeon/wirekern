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
    #[error("auth: {reason}")]
    Auth { site: Site, reason: String },
    #[error("rate limited")]
    RateLimited {
        site: Site,
        retry_after: Option<Duration>,
    },
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
    Auth {
        site: Site,
        reason: String,
    },
    RateLimited {
        site: Site,
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_after: Option<u64>,
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
            | Self::InvalidPost { .. } => 2,
            Self::Auth { .. } => 3,
            Self::RateLimited { .. } => 4,
            Self::Platform { .. } | Self::Network { .. } | Self::Timeout { .. } => 5,
        }
    }
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        WireError::from(self).exit_code()
    }
}
