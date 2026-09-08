//! Publish kernel: official APIs, BYO credentials, no calendar.
//!
//! Default features are empty. Inject `Vault` / `AppStore`, or enable `vault-file`.

mod apps;
mod client;
mod error;
mod publisher;
mod registry;
mod types;
mod vault;

#[cfg(feature = "vault-file")]
mod vault_file;

#[cfg(feature = "client")]
mod http;
#[cfg(feature = "client")]
pub use http::Http;

#[cfg(feature = "oauth")]
mod oauth;
#[cfg(feature = "oauth")]
pub use oauth::{
    authorize_url, exchange_code, extract_code, query_param, verify_state, TokenResponse,
};

#[cfg(any(feature = "threads", feature = "bluesky"))]
pub mod connectors;

pub use apps::{app_source, env_override, AppStore, MemoryAppStore};
pub use client::{refresh_is_due, Client};
pub use error::{Error, WireError};
pub use publisher::{AuthKind, AuthReply, AuthStart, Publisher};
pub use registry::Registry;
pub use types::{
    valid_name, AccountCreds, AccountKey, AppConfig, Body, Capability, Deadline, Intent, Limits,
    OAuthApp, Outcome, PostRequest, Site, WhoAmI, USER_AGENT,
};
pub use vault::{MemoryVault, Vault};

#[cfg(feature = "vault-file")]
pub use vault_file::{FileAppStore, FileVault};

#[cfg(test)]
mod tests;
