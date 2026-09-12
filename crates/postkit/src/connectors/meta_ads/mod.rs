//! Meta Ads connector — Tier A reads, Tier B paused creates, Tier C lifecycle.
//!
//! Creates still hard-code `status=PAUSED`. Activation and other spend-shaped
//! updates exist as explicit `AdsManager` methods; `Client` still calls
//! `policy.rs` before credentials. Auth reuses the Threads paste-code machinery
//! against the Facebook OAuth host; the long-lived exchange is Meta's
//! `fb_exchange_token` grant (~60 days).

mod accounts;
mod auth;
mod create;
mod creatives;
mod graph;
mod insights;
mod inventory;
mod lifecycle;
mod manager;

#[cfg(test)]
mod tests;

use crate::error::Error;
use crate::http::Http;
use crate::oauth::{authorize_url, exchange_code, extract_code, new_state};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::registry::Connector;
use crate::types::{AccountCreds, AppConfig, Capability, Deadline, Intent, Outcome, Site, WhoAmI};
use async_trait::async_trait;
use serde_json::Value;

use accounts::first_ad_account;
use auth::{creds_from_long, long_lived, require_oauth, whoami};
use graph::{access_token, extra_string};

pub const GRAPH_HOST: &str = "graph.facebook.com";
/// Pinned per 026 §3(5): bumped deliberately per release, never silently.
/// v26.0 is current as of 2026-09; versions older than v22 are blocked.
pub const GRAPH_VERSION: &str = "v26.0";
pub const GRAPH_ORIGIN: &str = "https://graph.facebook.com";
pub const AUTHORIZE: &str = "https://www.facebook.com/dialog/oauth";
pub const SITE: &str = "meta_ads";
/// Tier B adds paused management. Existing `ads_read` tokens keep working for
/// insights, but an operator must re-authenticate before a create is allowed.
/// `pages_show_list` lets the token discover the operator's Pages
/// (`/me/accounts` returns empty without it) and `pages_manage_ads` lets a
/// Page-backed `object_story_spec` creative act on the chosen Page — the
/// Tier B link-creative path is unusable without both.
pub const SCOPES: &str = "ads_read,ads_management,pages_show_list,pages_manage_ads";

use crate::ads::SYSTEM_USER_TOKEN_KIND;

pub struct MetaAds {
    pub(super) http: Http,
    pub(super) site: Site,
    pub(super) base: String,
    pub(super) graph_origin: String,
}

impl MetaAds {
    pub fn new() -> Result<Self, Error> {
        Self::with_origins(
            format!("https://{GRAPH_HOST}/{GRAPH_VERSION}"),
            GRAPH_ORIGIN,
        )
    }

    /// Test helper: httpmock publish base, e.g. `http://127.0.0.1:PORT/v26.0`.
    pub fn with_base(base: impl Into<String>) -> Result<Self, Error> {
        Self::with_origins(base, GRAPH_ORIGIN)
    }

    /// Test helper: separate versioned base and unversioned Graph origin
    /// (OAuth endpoints live on the origin, not under the version).
    pub fn with_origins(
        publish_base: impl Into<String>,
        graph_origin: impl Into<String>,
    ) -> Result<Self, Error> {
        Ok(Self {
            http: Http::new()?,
            site: Site::new(SITE),
            base: publish_base.into().trim_end_matches('/').to_string(),
            graph_origin: graph_origin.into().trim_end_matches('/').to_string(),
        })
    }

    /// Publisher + insights + paused-ads manager. Extra verbs stay off
    /// `Publisher` so a Threads-only build never compiles this surface.
    pub fn connector(self) -> Connector {
        let this = std::sync::Arc::new(self);
        Connector::from_publisher(this.clone())
            .insights(this.clone())
            .ads(this)
    }
}

#[async_trait]
impl Publisher for MetaAds {
    fn site(&self) -> &Site {
        &self.site
    }

    fn capabilities(&self) -> &[Capability] {
        &[
            Capability::ReadMetrics,
            Capability::ReadAdAccounts,
            Capability::ReadAdPreviews,
            Capability::ReadAdReviewStatus,
            Capability::ReadAdsInventory,
            Capability::CreatePausedAds,
            Capability::CreateAdCreative,
            Capability::ManageAdsLifecycle,
        ]
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::OAuth2AuthCode
    }

    /// Tier A is read-only; the capability check in `Client::publish` turns
    /// any publish away before it reaches here. This arm exists so the
    /// connector still refuses on its own if called directly.
    async fn publish(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _intent: Intent,
        _deadline: Deadline,
    ) -> Result<Outcome, Error> {
        Err(Error::InvalidPost {
            site: self.site.clone(),
            reason: "publish_unsupported".into(),
            limit: None,
        })
    }

    async fn whoami(&self, _app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error> {
        let token = access_token(creds)?;
        let deadline = Deadline::from_secs(30);
        whoami(&self.http, &self.base, token, deadline).await
    }

    async fn auth_start(&self, app: &AppConfig) -> Result<AuthStart, Error> {
        let oauth = require_oauth(app)?;
        let state = new_state()?;
        let url = authorize_url(
            AUTHORIZE,
            &oauth.client_id,
            &oauth.redirect_uri,
            SCOPES,
            &state,
        );
        Ok(AuthStart::Browser {
            authorize_url: url,
            state,
        })
    }

    async fn auth_finish(&self, app: &AppConfig, reply: AuthReply) -> Result<AccountCreds, Error> {
        let oauth = require_oauth(app)?;
        let raw = match reply {
            AuthReply::Pasted { code } => code,
            AuthReply::Redirect { url } => url,
            AuthReply::AppPassword { .. } => {
                return Err(Error::Auth {
                    site: self.site.clone(),
                    reason: "use_code".into(),
                });
            }
        };
        let code = extract_code(&raw)?;
        let deadline = Deadline::from_secs(30);
        let token_ep = format!("{}/oauth/access_token", self.graph_origin);
        let short = exchange_code(
            &self.http,
            &token_ep,
            &oauth.client_id,
            &oauth.client_secret,
            &oauth.redirect_uri,
            &code,
            deadline,
            &self.site,
        )
        .await?;
        let long = long_lived(
            &self.http,
            &self.graph_origin,
            &oauth.client_id,
            &oauth.client_secret,
            &short.access_token,
            deadline,
        )
        .await?;
        // Resolve the user and their first ad account before anything is
        // returned for vaulting: creds without an ad account cannot answer
        // a single insights query, so the gap surfaces at the door.
        let me = whoami(&self.http, &self.base, &long.access_token, deadline).await?;
        let account = first_ad_account(&self.http, &self.base, &long.access_token, deadline)
            .await?
            .ok_or_else(|| Error::Auth {
                site: self.site.clone(),
                reason: "no_ad_account".into(),
            })?;
        let mut creds = creds_from_long(&long, Some(me.id));
        if let AccountCreds::OAuth2 { extra, .. } = &mut creds {
            extra["ad_account_id"] = Value::String(account);
        }
        Ok(creds)
    }

    /// `fb_exchange_token` re-issue. Needs the app secret, so a stored
    /// long-lived token can only be refreshed while the app config exists.
    async fn refresh(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<AccountCreds, Error> {
        let oauth = require_oauth(app)?;
        let token = access_token(creds)?;
        let user_id = extra_string(creds, "user_id");
        let account = extra_string(creds, "ad_account_id");
        // System User tokens are not user OAuth credentials. Meta's
        // fb_exchange_token grant is the ~60-day user path; calling it here
        // would treat an unattended secret as a person token.
        if extra_string(creds, "token_kind").as_deref() == Some(SYSTEM_USER_TOKEN_KIND) {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "system_user_no_refresh".into(),
            });
        }
        // The caller's budget, not a private 30s (issue 024).
        let long = long_lived(
            &self.http,
            &self.graph_origin,
            &oauth.client_id,
            &oauth.client_secret,
            token,
            deadline,
        )
        .await?;
        let mut creds = creds_from_long(&long, user_id);
        if let (Some(account), AccountCreds::OAuth2 { extra, .. }) = (account, &mut creds) {
            extra["ad_account_id"] = Value::String(account);
        }
        Ok(creds)
    }
}
