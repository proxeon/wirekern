//! Meta Ads connector — Tier A reads, Tier B paused creates, Tier C lifecycle.
//!
//! Creates still hard-code `status=PAUSED`. Activation and other spend-shaped
//! updates exist as explicit `AdsManager` methods; `Client` still calls
//! `policy.rs` before credentials. Auth reuses the Threads paste-code machinery
//! against the Facebook OAuth host; the long-lived exchange is Meta's
//! `fb_exchange_token` grant (~60 days).

use crate::ads::{
    AdReviewIssue, AdReviewStatus, AdReviewStatusRequest, AdsDuplicateReply, AdsDuplicateRequest,
    AdsInspectReply, AdsInspectRequest, AdsInventoryItem, AdsInventoryKind, AdsInventoryReply,
    AdsInventoryRequest, AdsStatusUpdateRequest, AdsTargetingReadback, AdsTokenInspection,
    AdsTokenKind, CreateLinkAdCreativeRequest, CreatePausedAdRequest, CreatedAd, CreatedAdCreative,
    CreativePreview, CreativePreviewRequest, MarketingApiAccessTier, MarketingApiAccessTierKind,
    PausedAdCreate, UploadAdImageRequest, UploadedAdImage, MARKETING_API_ACCESS_TIER_DASHBOARD,
    SYSTEM_USER_TOKEN_KIND,
};
use crate::error::Error;
use crate::facets::{AdsManager, InsightsSource};
use crate::form::form;
use crate::http::Http;
use crate::insights::{
    AdAccount, AdAccountsReply, AttributionWindow, InsightRow, InsightsJob, InsightsJobStatus,
    InsightsLevel, InsightsQuery, InsightsReply, Metric, MAX_INSIGHTS_RESULT_ROWS,
};
use crate::oauth::{authorize_url, exchange_code, extract_code, new_state};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::registry::Connector;
use crate::types::{
    AccountCreds, AppConfig, Capability, Deadline, Intent, OAuthApp, Outcome, Site, WhoAmI,
};
use async_trait::async_trait;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Daily rows over a ≤90-day range fit in one Graph page; this cap exists
/// so a runaway cursor loop fails loudly instead of paging forever.
const MAX_PAGES: usize = 50;

pub struct MetaAds {
    http: Http,
    site: Site,
    base: String,
    graph_origin: String,
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

#[async_trait]
impl InsightsSource for MetaAds {
    async fn insights(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        query: &InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, query.account.as_deref())?;
        let params = insights_form(query, token)?;
        let url = format!("{}/act_{}/insights?{}", self.base, account, params);
        let rows = fetch_insights_pages(&self.http, &self.site, &url, query, deadline).await?;
        let currency = account_currency(&self.http, &self.base, &account, token, deadline).await?;
        Ok(InsightsReply {
            site: self.site.clone(),
            account_id: format!("act_{account}"),
            currency,
            rows,
        })
    }

    async fn ad_accounts(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<AdAccountsReply, Error> {
        let token = access_token(creds)?;
        let accounts = list_ad_accounts(&self.http, &self.base, token, deadline).await?;
        Ok(AdAccountsReply {
            site: self.site.clone(),
            accounts,
        })
    }

    async fn start_insights_job(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        query: &InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, query.account.as_deref())?;
        let params = insights_form(query, token)?;
        let url = format!("{}/act_{}/insights", self.base, account);
        let resp = self
            .http
            .send(
                self.http
                    .post(&url)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(params),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(resp, &self.site).await?;
        let id = body
            .get("report_run_id")
            .or_else(|| body.get("id"))
            .and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "missing_report_run_id".into(),
                message: "insights job did not return report_run_id".into(),
            })?;
        Ok(InsightsJob {
            site: self.site.clone(),
            id,
            status: InsightsJobStatus::NotStarted,
            percent_complete: 0,
            error_code: None,
            error_message: None,
        })
    }

    async fn insights_job(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        job_id: &str,
        deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        let token = access_token(creds)?;
        read_insights_job(&self.http, &self.base, &self.site, token, job_id, deadline).await
    }

    async fn insights_job_result(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        job_id: &str,
        query: &InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
        let token = access_token(creds)?;
        validate_insights_job_id(job_id)?;
        let account = account_id(creds, query.account.as_deref())?;
        let q = form(&[("access_token", token)]);
        let url = format!("{}/{}/insights?{q}", self.base, job_id);
        let rows = fetch_insights_pages(&self.http, &self.site, &url, query, deadline).await?;
        let currency = account_currency(&self.http, &self.base, &account, token, deadline).await?;
        Ok(InsightsReply {
            site: self.site.clone(),
            account_id: format!("act_{account}"),
            currency,
            rows,
        })
    }

    async fn cancel_insights_job(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        job_id: &str,
        deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        let token = access_token(creds)?;
        validate_insights_job_id(job_id)?;
        let q = form(&[("access_token", token)]);
        let url = format!("{}/{job_id}?{q}", self.base);
        let resp = self
            .http
            .send(self.http.delete(&url), deadline, &self.site)
            .await?;
        // DELETE may return `{success:true}` or an empty 200. Do not require
        // an Ad Report Run document after cancel.
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|_| Error::request_failed(&self.site))?;
        if !status.is_success() {
            return Err(map_graph_error(status.as_u16(), &text));
        }
        Ok(InsightsJob {
            site: self.site.clone(),
            id: job_id.into(),
            status: InsightsJobStatus::Skipped,
            percent_complete: 0,
            error_code: None,
            error_message: None,
        })
    }
}

#[async_trait]
impl AdsManager for MetaAds {
    async fn create_paused_ad(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &CreatePausedAdRequest,
        deadline: Deadline,
    ) -> Result<CreatedAd, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        create_paused_ad(
            &self.http,
            &self.base,
            &self.site,
            &account,
            token,
            &request.create,
            deadline,
        )
        .await
    }

    async fn upload_ad_image(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &UploadAdImageRequest,
        deadline: Deadline,
    ) -> Result<UploadedAdImage, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        upload_ad_image(
            &self.http, &self.base, &self.site, &account, token, request, deadline,
        )
        .await
    }

    async fn upload_ad_video(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &crate::ads::UploadAdVideoRequest,
        deadline: Deadline,
    ) -> Result<crate::ads::UploadedAdVideo, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        upload_ad_video(
            &self.http, &self.base, &self.site, &account, token, request, deadline,
        )
        .await
    }

    async fn ad_video_status(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &crate::ads::AdVideoStatusRequest,
        deadline: Deadline,
    ) -> Result<crate::ads::AdVideoStatus, Error> {
        let token = access_token(creds)?;
        read_ad_video_status(&self.http, &self.base, &self.site, token, request, deadline).await
    }

    async fn create_link_ad_creative(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &CreateLinkAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        create_link_ad_creative(
            &self.http, &self.base, &self.site, &account, token, request, deadline,
        )
        .await
    }

    async fn create_video_ad_creative(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &crate::ads::CreateVideoAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        create_video_ad_creative(
            &self.http, &self.base, &self.site, &account, token, request, deadline,
        )
        .await
    }

    async fn create_ad_creative(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &crate::ads::CreateAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        create_typed_ad_creative(
            &self.http, &self.base, &self.site, &account, token, request, deadline,
        )
        .await
    }

    async fn preview_ad_creative(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &CreativePreviewRequest,
        deadline: Deadline,
    ) -> Result<CreativePreview, Error> {
        let token = access_token(creds)?;
        preview_ad_creative(&self.http, &self.base, &self.site, token, request, deadline).await
    }

    async fn bootstrap_system_user_token(
        &self,
        app: &AppConfig,
        token: &str,
        deadline: Deadline,
    ) -> Result<AccountCreds, Error> {
        let token = token.trim();
        if token.is_empty() {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "missing_token".into(),
            });
        }
        let oauth = require_oauth(app)?;
        // Classify before vault write so a user token cannot be stored as
        // an unattended secret (references item 3).
        let debug = debug_token(
            &self.http,
            &self.base,
            oauth,
            token,
            deadline,
            AdsTokenKind::SystemUser,
        )
        .await?;
        if !debug.is_valid {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "invalid_token".into(),
            });
        }
        refuse_non_system_user_debug(&self.site, debug.debug_type.as_deref(), debug.expires_at)?;
        if let Some(app_id) = debug.app_id.as_deref() {
            if app_id != oauth.client_id {
                return Err(Error::Auth {
                    site: self.site.clone(),
                    reason: "token_app_mismatch".into(),
                });
            }
        }
        let me = whoami(&self.http, &self.base, token, deadline).await?;
        let account = first_ad_account(&self.http, &self.base, token, deadline)
            .await?
            .ok_or_else(|| Error::Auth {
                site: self.site.clone(),
                reason: "no_ad_account".into(),
            })?;
        Ok(system_user_creds(token, &me.id, &account, debug.expires_at))
    }

    async fn inspect_access_token(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<AdsTokenInspection, Error> {
        let oauth = require_oauth(app)?;
        let token = access_token(creds)?;
        let kind = AdsTokenKind::from_vault_extra(extra_string(creds, "token_kind").as_deref());
        debug_token(&self.http, &self.base, oauth, token, deadline, kind).await
    }

    async fn marketing_api_access_tier(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<MarketingApiAccessTier, Error> {
        let token = access_token(creds)?;
        marketing_api_access_tier(&self.http, &self.base, token, deadline).await
    }

    async fn ad_review_status(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &AdReviewStatusRequest,
        deadline: Deadline,
    ) -> Result<AdReviewStatus, Error> {
        let token = access_token(creds)?;
        read_ad_review_status(&self.http, &self.base, &self.site, token, request, deadline).await
    }

    async fn list_ads_inventory(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &AdsInventoryRequest,
        deadline: Deadline,
    ) -> Result<AdsInventoryReply, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, request.account.as_deref())?;
        list_ads_inventory(
            &self.http,
            &self.base,
            &self.site,
            &account,
            token,
            request.kind,
            deadline,
        )
        .await
    }

    async fn inspect_ads_object(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &AdsInspectRequest,
        deadline: Deadline,
    ) -> Result<AdsInspectReply, Error> {
        let token = access_token(creds)?;
        inspect_ads_object(&self.http, &self.base, &self.site, token, request, deadline).await
    }

    async fn update_ad_status(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &AdsStatusUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdReviewStatus, Error> {
        let token = access_token(creds)?;
        update_ad_status(&self.http, &self.base, &self.site, token, request, deadline).await
    }

    async fn duplicate_ad_object(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &AdsDuplicateRequest,
        deadline: Deadline,
    ) -> Result<AdsDuplicateReply, Error> {
        let token = access_token(creds)?;
        duplicate_ad_object(&self.http, &self.base, &self.site, token, request, deadline).await
    }

    async fn post_ad_update(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        id: &str,
        fields: &[(String, String)],
        deadline: Deadline,
    ) -> Result<(), Error> {
        let token = access_token(creds)?;
        post_ad_update(
            &self.http, &self.base, &self.site, token, id, fields, deadline,
        )
        .await
    }

    async fn read_ad_targeting_json(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        id: &str,
        deadline: Deadline,
    ) -> Result<Value, Error> {
        let token = access_token(creds)?;
        read_ad_json_field(
            &self.http,
            &self.base,
            &self.site,
            token,
            id,
            "targeting",
            deadline,
        )
        .await
    }

    async fn read_special_ad_categories(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        id: &str,
        deadline: Deadline,
    ) -> Result<Vec<String>, Error> {
        let token = access_token(creds)?;
        let value = read_ad_json_field(
            &self.http,
            &self.base,
            &self.site,
            token,
            id,
            "special_ad_categories",
            deadline,
        )
        .await?;
        Ok(value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| nonempty_value_string(Some(item)))
                    .filter(|item| item != "NONE")
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// Submit the one intentionally narrow Tier B form. `status=PAUSED` lives in
/// this function rather than in a public request type, so neither CLI users
/// nor library callers have a way to turn a create into an active delivery.
async fn create_paused_ad(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    create: &PausedAdCreate,
    deadline: Deadline,
) -> Result<CreatedAd, Error> {
    let (path, entity, mut fields) = match create {
        PausedAdCreate::Campaign(campaign) => {
            let mut fields = vec![
                ("name", campaign.name.clone()),
                ("objective", campaign.objective.meta_value().into()),
                (
                    "special_ad_categories",
                    serde_json::to_string(&campaign.special_ad_categories)
                        .expect("Vec<String> serializes"),
                ),
                (
                    // Always explicit for ABO (v24.0+). `false` keeps
                    // independent ad-set budgets; `true` is Meta's up-to-20%
                    // ABO share and is refused locally with a campaign budget.
                    "is_adset_budget_sharing_enabled",
                    if campaign.is_adset_budget_sharing_enabled {
                        "true".into()
                    } else {
                        "false".into()
                    },
                ),
            ];
            // CBO: Meta's campaign create takes daily XOR lifetime. Sending
            // neither leaves budget on the ad set (Advantage campaign budget
            // off). Sending both is refused before HTTP.
            if let Some(daily) = campaign.daily_budget {
                fields.push(("daily_budget", daily.to_string()));
            }
            if let Some(lifetime) = campaign.lifetime_budget {
                fields.push(("lifetime_budget", lifetime.to_string()));
            }
            ("campaigns", crate::ads::AdEntity::Campaign, fields)
        }
        PausedAdCreate::Adset(adset) => {
            let mut fields = vec![
                ("name", adset.name.clone()),
                ("campaign_id", adset.campaign_id.clone()),
                ("bid_strategy", adset.bid_strategy.meta_value().into()),
                ("billing_event", adset.billing_event.meta_value().into()),
                (
                    "optimization_goal",
                    adset.optimization_goal.meta_value().into(),
                ),
                (
                    "targeting",
                    serde_json::to_string(&adset.targeting).expect("AdTargeting serializes"),
                ),
            ];
            // Ad-set daily XOR lifetime. Both omitted is a CBO child: the
            // parent campaign already posted the shared budget.
            if let Some(daily) = adset.daily_budget {
                fields.push(("daily_budget", daily.to_string()));
            }
            if let Some(lifetime) = adset.lifetime_budget {
                fields.push(("lifetime_budget", lifetime.to_string()));
            }
            // Cap strategies: Meta `bid_amount` in account minor units.
            // Min-ROAS: `bid_constraints.roas_average_floor` (10000 = 1.0);
            // Meta forbids combining this with `bid_amount`.
            if let Some(amount) = adset.bid_amount {
                fields.push(("bid_amount", amount.to_string()));
            }
            if let Some(floor) = adset.roas_average_floor {
                fields.push((
                    "bid_constraints",
                    serde_json::json!({ "roas_average_floor": floor }).to_string(),
                ));
            }
            // Ad-set schedule is Meta's delivery window, not a Postkit
            // calendar. Lifetime budget requires end_time (checked locally).
            if let Some(start) = &adset.start_time {
                fields.push(("start_time", start.clone()));
            }
            if let Some(end) = &adset.end_time {
                fields.push(("end_time", end.clone()));
            }
            if let Some(promoted) = &adset.promoted_object {
                fields.push(("promoted_object", promoted.meta_json().to_string()));
            }
            ("adsets", crate::ads::AdEntity::Adset, fields)
        }
        PausedAdCreate::Ad(ad) => (
            "ads",
            crate::ads::AdEntity::Ad,
            vec![
                ("name", ad.name.clone()),
                ("adset_id", ad.adset_id.clone()),
                (
                    "creative",
                    serde_json::json!({ "creative_id": ad.creative_id }).to_string(),
                ),
            ],
        ),
    };
    fields.push(("status", "PAUSED".into()));
    // The token goes in the form body rather than a URL query so proxy logs,
    // errors, and test output have fewer opportunities to expose it.
    fields.push(("access_token", token.into()));
    let pairs: Vec<(&str, &str)> = fields
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    let body = form(&pairs);
    let url = format!("{base}/act_{account}/{path}");
    let response = http
        .send(
            http.post(&url)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body),
            deadline,
            site,
        )
        .await?;
    let response = read_json(response, site).await?;
    let id = value_string(response.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_id".into(),
        message: "paused create returned no id".into(),
    })?;
    Ok(CreatedAd {
        site: site.clone(),
        account_id: format!("act_{account}"),
        entity,
        id,
        status: "PAUSED".into(),
    })
}

/// Upload an account image using Meta's multipart `filename` part. The
/// operator-supplied filename is validated as a basename before this point;
/// keeping it as multipart metadata lets Meta preserve media type inference
/// without leaking a local filesystem path into a request or error.
async fn upload_ad_image(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    request: &UploadAdImageRequest,
    deadline: Deadline,
) -> Result<UploadedAdImage, Error> {
    let form = reqwest::multipart::Form::new()
        .part(
            "filename",
            reqwest::multipart::Part::bytes(request.bytes.clone())
                .file_name(request.filename.clone()),
        )
        // Put the bearer in the multipart body for the same log-safety reason
        // as paused create forms: never place credentials in a request URL.
        .text("access_token", token.to_string());
    let url = format!("{base}/act_{account}/adimages");
    let response = http
        .send(http.post(&url).multipart(form), deadline, site)
        .await?;
    let response = read_json(response, site).await?;
    let hash = response
        .get("images")
        .and_then(Value::as_object)
        .and_then(|images| {
            images
                .values()
                .find_map(|image| value_string(image.get("hash")))
        })
        .or_else(|| value_string(response.get("hash")))
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_image_hash".into(),
            message: "image upload returned no hash".into(),
        })?;
    Ok(UploadedAdImage {
        site: site.clone(),
        account_id: format!("act_{account}"),
        hash,
    })
}

/// Multipart `source` upload to `/advideos`. Encoding is a later GET of
/// `status.video_status`; this call only returns the numeric video id.
async fn upload_ad_video(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    request: &crate::ads::UploadAdVideoRequest,
    deadline: Deadline,
) -> Result<crate::ads::UploadedAdVideo, Error> {
    let form = reqwest::multipart::Form::new()
        .part(
            "source",
            reqwest::multipart::Part::bytes(request.bytes.clone())
                .file_name(request.filename.clone()),
        )
        .text("access_token", token.to_string());
    let url = format!("{base}/act_{account}/advideos");
    let response = http
        .send(http.post(&url).multipart(form), deadline, site)
        .await?;
    let response = read_json(response, site).await?;
    let id = value_string(response.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_video_id".into(),
        message: "video upload returned no id".into(),
    })?;
    Ok(crate::ads::UploadedAdVideo {
        site: site.clone(),
        account_id: format!("act_{account}"),
        id,
    })
}

async fn read_ad_video_status(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    request: &crate::ads::AdVideoStatusRequest,
    deadline: Deadline,
) -> Result<crate::ads::AdVideoStatus, Error> {
    let params = form(&[("fields", "status"), ("access_token", token)]);
    let url = format!("{base}/{}?{params}", request.video_id);
    let response = http.send(http.get(&url), deadline, site).await?;
    let response = read_json(response, site).await?;
    let raw = response
        .get("status")
        .and_then(|status| status.get("video_status"))
        .and_then(Value::as_str)
        .or_else(|| response.get("status").and_then(Value::as_str));
    let raw = raw.ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_video_status".into(),
        message: "video status returned no video_status".into(),
    })?;
    Ok(crate::ads::AdVideoStatus {
        site: site.clone(),
        video_id: request.video_id.clone(),
        video_status: crate::ads::AdVideoStatusKind::from_meta(raw),
        raw: Some(raw.to_string()),
    })
}

fn attach_creative_identity(
    spec: &mut Value,
    instagram_user_id: Option<&str>,
    _advantage_plus: bool,
    _whatsapp_identity: Option<&crate::ads::WhatsAppStatusIdentity>,
) {
    if let Some(id) = instagram_user_id {
        spec["instagram_user_id"] = serde_json::Value::String(id.to_string());
    }
}

fn append_creative_extras(
    fields: &mut Vec<(&str, String)>,
    advantage_plus: bool,
    whatsapp_identity: Option<&crate::ads::WhatsAppStatusIdentity>,
) {
    if advantage_plus {
        fields.push((
            "degrees_of_freedom_spec",
            serde_json::json!({
                "creative_features_spec": {
                    "standard_enhancements": { "enroll_status": "OPT_IN" }
                }
            })
            .to_string(),
        ));
    }
    if let Some(ident) = whatsapp_identity {
        fields.push(("wamo_whatsapp_identity_spec", ident.meta_json().to_string()));
    }
}

/// Create an unpublished Page-backed image-link creative. CTA `value` is
/// built from the typed extra fields; video, carousel, and Instagram shapes
/// have their own typed contracts.
async fn create_link_ad_creative(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    request: &CreateLinkAdCreativeRequest,
    deadline: Deadline,
) -> Result<CreatedAdCreative, Error> {
    let creative = &request.creative;
    let mut spec = serde_json::json!({
        "page_id": creative.page_id,
        "link_data": {
            "image_hash": creative.image_hash,
            "link": creative.destination_url,
            "message": creative.message,
            "name": creative.headline,
            "call_to_action": {
                "type": creative.call_to_action.meta_value(),
                "value": crate::ads::link_cta_value_json(creative),
            },
        },
    });
    attach_creative_identity(
        &mut spec,
        creative.instagram_user_id.as_deref(),
        creative.advantage_plus,
        creative.whatsapp_identity.as_ref(),
    );
    let object_story_spec = spec.to_string();
    let mut fields = vec![
        ("name", creative.name.clone()),
        ("object_story_spec", object_story_spec),
        ("access_token", token.to_string()),
    ];
    append_creative_extras(
        &mut fields,
        creative.advantage_plus,
        creative.whatsapp_identity.as_ref(),
    );
    let body = form(
        &fields
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect::<Vec<_>>(),
    );
    let url = format!("{base}/act_{account}/adcreatives");
    let response = http
        .send(
            http.post(&url)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body),
            deadline,
            site,
        )
        .await?;
    let response = read_json(response, site).await?;
    let id = value_string(response.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_creative_id".into(),
        message: "creative create returned no id".into(),
    })?;
    Ok(CreatedAdCreative {
        site: site.clone(),
        account_id: format!("act_{account}"),
        id,
    })
}

async fn create_video_ad_creative(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    request: &crate::ads::CreateVideoAdCreativeRequest,
    deadline: Deadline,
) -> Result<CreatedAdCreative, Error> {
    let creative = &request.creative;
    let link_cta = crate::ads::LinkAdCreative {
        name: creative.name.clone(),
        page_id: creative.page_id.clone(),
        image_hash: creative.image_hash.clone(),
        message: creative.message.clone(),
        headline: String::new(),
        destination_url: creative.destination_url.clone(),
        call_to_action: creative.call_to_action,
        geo_link: creative.geo_link.clone(),
        application_id: creative.application_id.clone(),
        app_link: creative.app_link.clone(),
        instagram_user_id: creative.instagram_user_id.clone(),
        advantage_plus: creative.advantage_plus,
        whatsapp_identity: creative.whatsapp_identity.clone(),
    };
    let mut spec = serde_json::json!({
        "page_id": creative.page_id,
        "video_data": {
            "video_id": creative.video_id,
            "image_hash": creative.image_hash,
            "message": creative.message,
            "call_to_action": {
                "type": creative.call_to_action.meta_value(),
                "value": crate::ads::link_cta_value_json(&link_cta),
            },
        },
    });
    attach_creative_identity(
        &mut spec,
        creative.instagram_user_id.as_deref(),
        creative.advantage_plus,
        creative.whatsapp_identity.as_ref(),
    );
    let object_story_spec = spec.to_string();
    let mut fields = vec![
        ("name", creative.name.clone()),
        ("object_story_spec", object_story_spec),
        ("access_token", token.to_string()),
    ];
    append_creative_extras(
        &mut fields,
        creative.advantage_plus,
        creative.whatsapp_identity.as_ref(),
    );
    let body = form(
        &fields
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect::<Vec<_>>(),
    );
    let url = format!("{base}/act_{account}/adcreatives");
    let response = http
        .send(
            http.post(&url)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body),
            deadline,
            site,
        )
        .await?;
    let response = read_json(response, site).await?;
    let id = value_string(response.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_creative_id".into(),
        message: "creative create returned no id".into(),
    })?;
    Ok(CreatedAdCreative {
        site: site.clone(),
        account_id: format!("act_{account}"),
        id,
    })
}

async fn create_typed_ad_creative(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    request: &crate::ads::CreateAdCreativeRequest,
    deadline: Deadline,
) -> Result<CreatedAdCreative, Error> {
    use crate::ads::AdCreativeKind;
    let (name, spec, advantage_plus, wa, product_set_id) = match &request.kind {
        AdCreativeKind::Carousel(c) => {
            let cards: Vec<_> = c
                .cards
                .iter()
                .map(|card| {
                    serde_json::json!({
                        "image_hash": card.image_hash,
                        "link": card.link,
                        "name": card.name,
                    })
                })
                .collect();
            let mut spec = serde_json::json!({
                "page_id": c.page_id,
                "link_data": {
                    "message": c.message,
                    "link": c.cards.first().map(|card| card.link.as_str()).unwrap_or_default(),
                    "child_attachments": cards,
                    "call_to_action": { "type": c.call_to_action.meta_value() },
                },
            });
            attach_creative_identity(
                &mut spec,
                c.instagram_user_id.as_deref(),
                c.advantage_plus,
                c.whatsapp_identity.as_ref(),
            );
            (
                c.name.clone(),
                spec,
                c.advantage_plus,
                c.whatsapp_identity.clone(),
                None,
            )
        }
        AdCreativeKind::Catalog(c) => {
            let mut spec = serde_json::json!({
                "page_id": c.page_id,
                "template_data": {
                    "link": c.link,
                    "message": c.message,
                    "call_to_action": { "type": c.call_to_action.meta_value() },
                },
            });
            attach_creative_identity(
                &mut spec,
                c.instagram_user_id.as_deref(),
                c.advantage_plus,
                c.whatsapp_identity.as_ref(),
            );
            (
                c.name.clone(),
                spec,
                c.advantage_plus,
                c.whatsapp_identity.clone(),
                Some(c.product_set_id.clone()),
            )
        }
        AdCreativeKind::LeadForm(c) => {
            let mut spec = serde_json::json!({
                "page_id": c.page_id,
                "link_data": {
                    "image_hash": c.image_hash,
                    "link": c.destination_url,
                    "message": c.message,
                    "name": c.headline,
                    "call_to_action": {
                        "type": c.call_to_action.meta_value(),
                        "value": {
                            "link": c.destination_url,
                            "lead_gen_form_id": c.lead_gen_form_id,
                        },
                    },
                },
            });
            attach_creative_identity(
                &mut spec,
                c.instagram_user_id.as_deref(),
                c.advantage_plus,
                c.whatsapp_identity.as_ref(),
            );
            (
                c.name.clone(),
                spec,
                c.advantage_plus,
                c.whatsapp_identity.clone(),
                None,
            )
        }
        AdCreativeKind::AppInstall(c) => {
            let mut spec = serde_json::json!({
                "page_id": c.page_id,
                "link_data": {
                    "image_hash": c.image_hash,
                    "link": c.object_store_url,
                    "message": c.message,
                    "call_to_action": {
                        "type": "INSTALL_MOBILE_APP",
                        "value": {
                            "application": c.application_id,
                            "link": c.object_store_url,
                        },
                    },
                },
            });
            attach_creative_identity(
                &mut spec,
                c.instagram_user_id.as_deref(),
                c.advantage_plus,
                c.whatsapp_identity.as_ref(),
            );
            (
                c.name.clone(),
                spec,
                c.advantage_plus,
                c.whatsapp_identity.clone(),
                None,
            )
        }
    };
    let object_story_spec = spec.to_string();
    let mut fields = vec![
        ("name", name),
        ("object_story_spec", object_story_spec),
        ("access_token", token.to_string()),
    ];
    if let Some(product_set_id) = product_set_id {
        // Advantage+ catalog ads: product_set_id is a creative field, not
        // nested under template_data.
        fields.push(("product_set_id", product_set_id));
    }
    append_creative_extras(&mut fields, advantage_plus, wa.as_ref());
    let body = form(
        &fields
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect::<Vec<_>>(),
    );
    let url = format!("{base}/act_{account}/adcreatives");
    let response = http
        .send(
            http.post(&url)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body),
            deadline,
            site,
        )
        .await?;
    let response = read_json(response, site).await?;
    let id = value_string(response.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_creative_id".into(),
        message: "creative create returned no id".into(),
    })?;
    Ok(CreatedAdCreative {
        site: site.clone(),
        account_id: format!("act_{account}"),
        id,
    })
}

/// Ask Meta to render an already-stored creative in one reviewed placement.
/// This is a Graph read edge, not `generatepreviews`: no campaign, ad set, or
/// final ad is created, and the body is kept opaque until the CLI writes it to
/// the operator-selected preview file.
async fn preview_ad_creative(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    request: &CreativePreviewRequest,
    deadline: Deadline,
) -> Result<CreativePreview, Error> {
    // Graph accepts user tokens on this read edge as a query parameter. The
    // shared HTTP layer deliberately redacts request URLs from transport
    // errors, preventing this credential from reaching terminal output.
    let params = form(&[
        ("ad_format", request.ad_format.meta_value()),
        ("access_token", token),
    ]);
    let url = format!("{base}/{}/previews?{params}", request.creative_id);
    let response = http.send(http.get(&url), deadline, site).await?;
    let response = read_json(response, site).await?;
    let body = response
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| value_string(item.get("body")))
        .filter(|body| !body.trim().is_empty())
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_preview_body".into(),
            message: "creative preview returned no body".into(),
        })?;
    Ok(CreativePreview {
        site: site.clone(),
        creative_id: request.creative_id.clone(),
        ad_format: request.ad_format,
        body,
    })
}

/// Read Meta's lifecycle fields for one existing campaign, ad set, or ad.
/// This endpoint intentionally has no account path: all three Graph objects
/// are addressed by their globally unique IDs, and the selected credential
/// still authorizes the read. Keeping this to GET prevents review inspection
/// from ever changing a paused draft's delivery or billing state.
async fn read_ad_review_status(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    request: &AdReviewStatusRequest,
    deadline: Deadline,
) -> Result<AdReviewStatus, Error> {
    let params = form(&[
        // `issues_info` carries Meta's concrete review/configuration errors;
        // without it operators see only a status and must switch tools to
        // learn why a draft cannot settle.
        (
            "fields",
            "id,name,configured_status,effective_status,issues_info",
        ),
        ("access_token", token),
    ]);
    let url = format!("{base}/{}?{params}", request.id);
    let response = http.send(http.get(&url), deadline, site).await?;
    let response = read_json(response, site).await?;
    let configured_status =
        nonempty_value_string(response.get("configured_status")).ok_or_else(|| {
            Error::Platform {
                site: site.clone(),
                code: "missing_configured_status".into(),
                message: "ad status returned no configured status".into(),
            }
        })?;
    let effective_status =
        nonempty_value_string(response.get("effective_status")).ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_effective_status".into(),
            message: "ad status returned no effective status".into(),
        })?;
    let issues = response
        .get("issues_info")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(review_issue_from).collect())
        .unwrap_or_default();
    Ok(AdReviewStatus {
        site: site.clone(),
        entity: request.entity,
        // Echo the validated request ID instead of trusting an optional Graph
        // response field: a malformed or partial reply cannot make a status
        // line appear to describe a different object.
        id: request.id.clone(),
        name: nonempty_value_string(response.get("name")),
        configured_status,
        effective_status,
        issues,
    })
}

/// Preserve Meta's review vocabulary without making the connector depend on
/// a particular issue subtype. Graph has used numeric and string error codes
/// across fields, so `value_string` normalizes both while absent fields remain
/// absent in Postkit's stable JSON response.
fn review_issue_from(value: &Value) -> AdReviewIssue {
    AdReviewIssue {
        code: nonempty_value_string(value.get("error_code")),
        summary: nonempty_value_string(value.get("error_summary")),
        message: nonempty_value_string(value.get("error_message")),
        level: nonempty_value_string(value.get("level")),
    }
}

/// Meta's documented campaign default already omits archived/deleted. The
/// example `["ACTIVE","PAUSED"]` would hide paused drafts still in
/// `IN_PROCESS` / `WITH_ISSUES`. Ad set and ad edges do not promise that
/// default, so every delivery kind sends the same live-ish list.
const LIVE_EFFECTIVE_STATUS: &str = "[\"ACTIVE\",\"PAUSED\",\"IN_PROCESS\",\"WITH_ISSUES\",\"PENDING_REVIEW\",\"DISAPPROVED\",\"PREAPPROVED\",\"PENDING_BILLING_INFO\",\"CAMPAIGN_PAUSED\",\"ADSET_PAUSED\"]";
const INVENTORY_PAGE_LIMIT: &str = "25";

/// Page one account-scoped inventory edge. Follows only Meta's opaque
/// `paging.next`, caps at `MAX_PAGES`, then sorts by id so CLI/MCP order
/// does not follow cursor arrival.
async fn list_ads_inventory(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    kind: AdsInventoryKind,
    deadline: Deadline,
) -> Result<AdsInventoryReply, Error> {
    let fields = inventory_list_fields(kind);
    let mut pairs = vec![
        ("fields", fields),
        ("limit", INVENTORY_PAGE_LIMIT),
        ("access_token", token),
    ];
    // Creatives: the adcreatives edge documents no parameters. Filter
    // DELETED locally after the GET instead of sending `effective_status`.
    if kind != AdsInventoryKind::Creative {
        pairs.push(("effective_status", LIVE_EFFECTIVE_STATUS));
    }
    let q = form(&pairs);
    let mut next = Some(format!("{base}/act_{account}/{}?{q}", kind.graph_edge()));
    let mut pages = 0usize;
    let mut items = Vec::new();
    while let Some(url) = next {
        deadline.check(site)?;
        pages += 1;
        if pages > MAX_PAGES {
            return Err(Error::Platform {
                site: site.clone(),
                code: "paging_exceeded".into(),
                message: format!("ads inventory paging exceeded {MAX_PAGES} pages"),
            });
        }
        let resp = http.send(http.get(&url), deadline, site).await?;
        let body = read_json(resp, site).await?;
        if let Some(data) = body.get("data").and_then(|data| data.as_array()) {
            for object in data {
                if let Some(item) = inventory_item_from(kind, object)? {
                    items.push(item);
                }
            }
        }
        next = body
            .get("paging")
            .and_then(|paging| paging.get("next"))
            .and_then(|next| next.as_str())
            .map(str::to_owned);
    }
    items.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(AdsInventoryReply {
        site: site.clone(),
        account_id: format!("act_{account}"),
        kind,
        items,
    })
}

/// POST one documented `status` on a globally unique object, then GET the
/// review fields. Token stays in the form body, same as paused creates.
async fn update_ad_status(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    request: &AdsStatusUpdateRequest,
    deadline: Deadline,
) -> Result<AdReviewStatus, Error> {
    let status = request.status.meta_value();
    let body = form(&[("status", status), ("access_token", token)]);
    let url = format!("{base}/{}", request.id);
    let response = http
        .send(
            http.post(&url)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body),
            deadline,
            site,
        )
        .await?;
    let response = read_json(response, site).await?;
    // Meta documents `{success: true}` for status updates. A false or
    // missing success is not a delivery claim.
    let success = response
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Err(Error::Platform {
            site: site.clone(),
            code: "status_update_unconfirmed".into(),
            message: "ad status update returned no success".into(),
        });
    }
    read_ad_review_status(
        http,
        base,
        site,
        token,
        &AdReviewStatusRequest {
            entity: request.entity,
            id: request.id.clone(),
        },
        deadline,
    )
    .await
}

/// Copy with Meta's documented default `status_option=PAUSED`. Postkit
/// never sends ACTIVE or INHERITED_FROM_SOURCE.
async fn duplicate_ad_object(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    request: &AdsDuplicateRequest,
    deadline: Deadline,
) -> Result<AdsDuplicateReply, Error> {
    let body = form(&[("status_option", "PAUSED"), ("access_token", token)]);
    let url = format!("{base}/{}/copies", request.id);
    let response = http
        .send(
            http.post(&url)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body),
            deadline,
            site,
        )
        .await?;
    let response = read_json(response, site).await?;
    let copied_id = nonempty_value_string(response.get("copied_campaign_id"))
        .or_else(|| nonempty_value_string(response.get("copied_adset_id")))
        .or_else(|| nonempty_value_string(response.get("copied_ad_id")))
        .or_else(|| nonempty_value_string(response.get("copied_adgroup_id")))
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_copied_id".into(),
            message: "ad copy returned no copied id".into(),
        })?;
    Ok(AdsDuplicateReply {
        site: site.clone(),
        entity: request.entity,
        source_id: request.id.clone(),
        copied_id,
        status: "PAUSED".into(),
    })
}

async fn post_ad_update(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    id: &str,
    fields: &[(String, String)],
    deadline: Deadline,
) -> Result<(), Error> {
    let mut pairs: Vec<(&str, &str)> = fields
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    pairs.push(("access_token", token));
    let body = form(&pairs);
    let url = format!("{base}/{id}");
    let response = http
        .send(
            http.post(&url)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body),
            deadline,
            site,
        )
        .await?;
    let response = read_json(response, site).await?;
    if !response
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(Error::Platform {
            site: site.clone(),
            code: "update_unconfirmed".into(),
            message: "ad object update returned no success".into(),
        });
    }
    Ok(())
}

async fn read_ad_json_field(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    id: &str,
    field: &str,
    deadline: Deadline,
) -> Result<Value, Error> {
    let params = form(&[("fields", field), ("access_token", token)]);
    let url = format!("{base}/{id}?{params}");
    let response = http.send(http.get(&url), deadline, site).await?;
    let response = read_json(response, site).await?;
    Ok(response.get(field).cloned().unwrap_or(Value::Null))
}

fn inventory_list_fields(kind: AdsInventoryKind) -> &'static str {
    match kind {
        AdsInventoryKind::Campaign => "id,name,configured_status,effective_status,objective",
        AdsInventoryKind::Adset => "id,name,campaign_id,configured_status,effective_status",
        AdsInventoryKind::Ad => "id,name,adset_id,campaign_id,configured_status,effective_status",
        AdsInventoryKind::Creative => "id,name,status,object_type",
    }
}

fn inventory_item_from(
    kind: AdsInventoryKind,
    value: &Value,
) -> Result<Option<AdsInventoryItem>, Error> {
    let id = nonempty_value_string(value.get("id")).ok_or_else(|| Error::Platform {
        site: Site::new(SITE),
        code: "missing_inventory_id".into(),
        message: "ads inventory object returned no id".into(),
    })?;
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(Error::Platform {
            site: Site::new(SITE),
            code: "bad_inventory_id".into(),
            message: "ads inventory object returned a non-numeric id".into(),
        });
    }
    if kind == AdsInventoryKind::Creative {
        let status = nonempty_value_string(value.get("status"));
        // The creative edge has no status filter. A deleted library entry
        // is not inventory of something an operator could later activate.
        if status.as_deref() == Some("DELETED") {
            return Ok(None);
        }
        return Ok(Some(AdsInventoryItem {
            id,
            name: nonempty_value_string(value.get("name")),
            configured_status: None,
            effective_status: None,
            status,
            campaign_id: None,
            adset_id: None,
            objective: None,
            object_type: nonempty_value_string(value.get("object_type")),
        }));
    }
    Ok(Some(AdsInventoryItem {
        id,
        name: nonempty_value_string(value.get("name")),
        configured_status: nonempty_value_string(value.get("configured_status")),
        effective_status: nonempty_value_string(value.get("effective_status")),
        status: None,
        campaign_id: nonempty_value_string(value.get("campaign_id")),
        adset_id: nonempty_value_string(value.get("adset_id")),
        objective: nonempty_value_string(value.get("objective")),
        object_type: None,
    }))
}

/// Read one object's spend-shaped fields. Campaign/ad set carry budget and
/// bid; targeting and destination live on the ad set (and sometimes the ad);
/// Page and click destination live on the creative.
async fn inspect_ads_object(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    request: &AdsInspectRequest,
    deadline: Deadline,
) -> Result<AdsInspectReply, Error> {
    let params = form(&[
        ("fields", inspect_fields(request.kind)),
        ("access_token", token),
    ]);
    let url = format!("{base}/{}?{params}", request.id);
    let response = http.send(http.get(&url), deadline, site).await?;
    let response = read_json(response, site).await?;
    inspect_reply_from(site, request, &response)
}

fn inspect_fields(kind: AdsInventoryKind) -> &'static str {
    match kind {
        AdsInventoryKind::Campaign => {
            "id,name,configured_status,effective_status,daily_budget,lifetime_budget,bid_strategy,objective"
        }
        AdsInventoryKind::Adset => {
            "id,name,campaign_id,configured_status,effective_status,daily_budget,lifetime_budget,bid_strategy,bid_amount,bid_constraints,targeting,promoted_object,destination_type"
        }
        AdsInventoryKind::Ad => {
            "id,name,adset_id,campaign_id,configured_status,effective_status,targeting,creative{id,name,object_story_spec,actor_id,object_url,link_url,call_to_action_type,product_set_id,instagram_user_id,wamo_whatsapp_identity_spec}"
        }
        AdsInventoryKind::Creative => {
            "id,name,status,object_story_spec,actor_id,object_url,link_url,call_to_action_type,product_set_id,instagram_user_id,wamo_whatsapp_identity_spec"
        }
    }
}

fn inspect_reply_from(
    site: &Site,
    request: &AdsInspectRequest,
    value: &Value,
) -> Result<AdsInspectReply, Error> {
    let creative = value.get("creative");
    let story = value
        .get("object_story_spec")
        .or_else(|| creative.and_then(|creative| creative.get("object_story_spec")));
    let product_set_id = nonempty_value_string(value.get("product_set_id")).or_else(|| {
        nonempty_value_string(creative.and_then(|creative| creative.get("product_set_id")))
    });
    let destination = destination_from(value, story).or_else(|| product_set_id.clone());
    Ok(AdsInspectReply {
        site: site.clone(),
        kind: request.kind,
        id: request.id.clone(),
        name: nonempty_value_string(value.get("name")),
        configured_status: nonempty_value_string(value.get("configured_status")),
        effective_status: nonempty_value_string(value.get("effective_status")),
        status: nonempty_value_string(value.get("status")),
        daily_budget: nonempty_value_string(value.get("daily_budget")),
        lifetime_budget: nonempty_value_string(value.get("lifetime_budget")),
        bid_strategy: nonempty_value_string(value.get("bid_strategy")),
        bid_amount: nonempty_value_string(value.get("bid_amount")),
        roas_average_floor: nonempty_value_string(
            value
                .get("bid_constraints")
                .and_then(|constraints| constraints.get("roas_average_floor")),
        ),
        targeting: targeting_readback(value.get("targeting")),
        page_id: page_id_from(value, story),
        destination,
        destination_type: nonempty_value_string(value.get("destination_type")),
        call_to_action_type: nonempty_value_string(value.get("call_to_action_type"))
            .or_else(|| {
                nonempty_value_string(
                    creative.and_then(|creative| creative.get("call_to_action_type")),
                )
            })
            .or_else(|| {
                nonempty_value_string(
                    story
                        .and_then(|story| story.get("link_data"))
                        .or_else(|| story.and_then(|story| story.get("video_data")))
                        .or_else(|| story.and_then(|story| story.get("template_data")))
                        .and_then(|data| data.get("call_to_action"))
                        .and_then(|cta| cta.get("type")),
                )
            }),
        product_set_id,
        instagram_user_id: nonempty_value_string(value.get("instagram_user_id"))
            .or_else(|| {
                nonempty_value_string(story.and_then(|story| story.get("instagram_user_id")))
            })
            .or_else(|| {
                nonempty_value_string(
                    creative.and_then(|creative| creative.get("instagram_user_id")),
                )
            }),
        whatsapp_identity_id: whatsapp_identity_id(value)
            .or_else(|| whatsapp_identity_id(creative.unwrap_or(&Value::Null))),
        campaign_id: nonempty_value_string(value.get("campaign_id")),
        adset_id: nonempty_value_string(value.get("adset_id")),
        creative_id: nonempty_value_string(
            value
                .get("creative")
                .and_then(|creative| creative.get("id")),
        ),
        objective: nonempty_value_string(value.get("objective")),
    })
}

fn whatsapp_identity_id(value: &Value) -> Option<String> {
    nonempty_value_string(
        value
            .get("wamo_whatsapp_identity_spec")
            .and_then(|spec| spec.get("wamo_whatsapp_identity_id")),
    )
}

fn page_id_from(value: &Value, story: Option<&Value>) -> Option<String> {
    nonempty_value_string(story.and_then(|story| story.get("page_id")))
        .or_else(|| nonempty_value_string(value.get("actor_id")))
        .or_else(|| {
            nonempty_value_string(
                value
                    .get("promoted_object")
                    .and_then(|object| object.get("page_id")),
            )
        })
        .or_else(|| {
            nonempty_value_string(
                value
                    .get("creative")
                    .and_then(|creative| creative.get("actor_id")),
            )
        })
}

fn destination_from(value: &Value, story: Option<&Value>) -> Option<String> {
    story_destination(story)
        .or_else(|| nonempty_value_string(value.get("link_url")))
        .or_else(|| nonempty_value_string(value.get("object_url")))
        .or_else(|| {
            story_destination(
                value
                    .get("creative")
                    .and_then(|creative| creative.get("object_story_spec")),
            )
            .or_else(|| {
                nonempty_value_string(
                    value
                        .get("creative")
                        .and_then(|creative| creative.get("link_url")),
                )
            })
            .or_else(|| {
                nonempty_value_string(
                    value
                        .get("creative")
                        .and_then(|creative| creative.get("object_url")),
                )
            })
        })
}

fn story_destination(story: Option<&Value>) -> Option<String> {
    let story = story?;
    for key in ["link_data", "video_data", "template_data"] {
        let Some(data) = story.get(key) else {
            continue;
        };
        if let Some(link) = nonempty_value_string(data.get("link")) {
            return Some(link);
        }
        let cta_value = data.get("call_to_action").and_then(|cta| cta.get("value"));
        // Website CTAs store `value.link`. Page CTAs store `value.page`.
        // WhatsApp Message stores `value.app_destination`. Get Directions
        // stores `value.geo_link` (or HTTPS/fbgeo in `link`).
        for key in ["link", "geo_link", "page", "app_destination"] {
            if let Some(dest) = nonempty_value_string(cta_value.and_then(|value| value.get(key))) {
                return Some(dest);
            }
        }
    }
    None
}

fn targeting_readback(value: Option<&Value>) -> Option<AdsTargetingReadback> {
    let targeting = value?;
    if !targeting.is_object() {
        return None;
    }
    let countries = targeting
        .get("geo_locations")
        .and_then(|geo| geo.get("countries"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| nonempty_value_string(Some(item)))
                .collect()
        })
        .unwrap_or_default();
    let readback = AdsTargetingReadback {
        countries,
        age_min: targeting
            .get("age_min")
            .and_then(Value::as_u64)
            .and_then(|n| u8::try_from(n).ok()),
        age_max: targeting
            .get("age_max")
            .and_then(Value::as_u64)
            .and_then(|n| u8::try_from(n).ok()),
        publisher_platforms: string_list(targeting.get("publisher_platforms")),
        facebook_positions: string_list(targeting.get("facebook_positions")),
        instagram_positions: string_list(targeting.get("instagram_positions")),
        whatsapp_positions: string_list(targeting.get("whatsapp_positions")),
        user_age_unknown: targeting.get("user_age_unknown").and_then(Value::as_bool),
    };
    if readback.is_empty() {
        None
    } else {
        Some(readback)
    }
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| nonempty_value_string(Some(item)))
                .collect()
        })
        .unwrap_or_default()
}

/// Map a metric to its Graph insights field name; `None` for metrics that
/// are derived (purchases ← actions).
fn meta_field(m: Metric) -> Option<&'static str> {
    match m {
        Metric::Spend => Some("spend"),
        Metric::Impressions => Some("impressions"),
        Metric::Clicks => Some("clicks"),
        Metric::Reach => Some("reach"),
        Metric::Ctr => Some("ctr"),
        Metric::Cpc => Some("cpc"),
        Metric::Cpm => Some("cpm"),
        Metric::Purchases => None,
        Metric::PurchaseValue | Metric::Roas => None,
        Metric::Frequency => Some("frequency"),
        Metric::UniqueClicks => Some("unique_clicks"),
        Metric::InlineLinkClicks => Some("inline_link_clicks"),
        Metric::InlineLinkClickCtr => Some("inline_link_click_ctr"),
        Metric::QualityRanking => Some("quality_ranking"),
        Metric::VideoThruplay => Some("video_thruplay_watched_actions"),
    }
}

/// Graph's `action_attribution_windows` wants an array of atomic windows
/// (`["7d_click","1d_view"]`); the combined `7d_click_1d_view` is only the
/// Ads Manager display name for that preset and is rejected with code 100.
fn attribution_param(a: AttributionWindow) -> &'static str {
    a.graph_windows()
}

fn insights_form(query: &InsightsQuery, token: &str) -> Result<String, Error> {
    let mut fields: std::collections::BTreeSet<&str> = query
        .metrics
        .iter()
        .copied()
        .filter_map(meta_field)
        .collect();
    if query.metrics.contains(&Metric::Purchases) {
        fields.insert("actions");
    }
    if query
        .metrics
        .iter()
        .any(|metric| matches!(metric, Metric::PurchaseValue | Metric::Roas))
    {
        fields.insert("action_values");
    }
    if query.metrics.contains(&Metric::Roas) {
        fields.insert("spend");
    }
    let fields: Vec<&str> = fields.into_iter().collect();
    let field_list = fields.join(",");
    let range = format!(
        "{{\"since\":\"{}\",\"until\":\"{}\"}}",
        query.range.from, query.range.to
    );
    let filter = entity_filter(query)?;
    let breakdowns = query
        .breakdowns
        .iter()
        .map(|breakdown| breakdown.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let mut pairs = vec![
        ("level", query.level.as_str()),
        ("fields", field_list.as_str()),
        ("time_range", range.as_str()),
        ("time_increment", "1"),
        (
            "action_attribution_windows",
            attribution_param(query.attribution),
        ),
        ("access_token", token),
    ];
    if let Some(filter) = filter.as_deref() {
        pairs.push(("filtering", filter));
    }
    if !breakdowns.is_empty() {
        pairs.push(("breakdowns", breakdowns.as_str()));
    }
    Ok(form(&pairs))
}

fn validate_insights_job_id(id: &str) -> Result<(), Error> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(Error::InvalidQuery {
            site: Site::new(SITE),
            reason: "bad_insights_job_id".into(),
        });
    }
    Ok(())
}

async fn fetch_insights_pages(
    http: &Http,
    site: &Site,
    start_url: &str,
    query: &InsightsQuery,
    deadline: Deadline,
) -> Result<Vec<InsightRow>, Error> {
    let mut rows: Vec<InsightRow> = Vec::new();
    let mut next = Some(start_url.to_string());
    let mut pages = 0usize;
    while let Some(url) = next {
        deadline.check(site)?;
        pages += 1;
        if pages > MAX_PAGES {
            return Err(Error::Platform {
                site: site.clone(),
                code: "paging_exceeded".into(),
                message: format!("insights paging exceeded {MAX_PAGES} pages"),
            });
        }
        let resp = http.send(http.get(&url), deadline, site).await?;
        let body = read_json(resp, site).await?;
        if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
            for item in data {
                rows.push(row_from(item, query));
                if rows.len() > MAX_INSIGHTS_RESULT_ROWS {
                    return Err(Error::InvalidQuery {
                        site: site.clone(),
                        reason: format!("insights_row_cap:{MAX_INSIGHTS_RESULT_ROWS}"),
                    });
                }
            }
        }
        next = body
            .get("paging")
            .and_then(|p| p.get("next"))
            .and_then(|n| n.as_str())
            .map(str::to_string);
    }
    rows.sort_by(|a, b| {
        (
            &a.entity_id,
            &a.date_start,
            serde_json::to_string(&a.dimensions).unwrap_or_default(),
        )
            .cmp(&(
                &b.entity_id,
                &b.date_start,
                serde_json::to_string(&b.dimensions).unwrap_or_default(),
            ))
    });
    Ok(rows)
}

async fn read_insights_job(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    job_id: &str,
    deadline: Deadline,
) -> Result<InsightsJob, Error> {
    validate_insights_job_id(job_id)?;
    let q = form(&[
        (
            "fields",
            "async_status,async_percent_completion,error_code,error_message,error_user_msg",
        ),
        ("access_token", token),
    ]);
    let url = format!("{base}/{job_id}?{q}");
    let resp = http.send(http.get(&url), deadline, site).await?;
    let body = read_json(resp, site).await?;
    let status = InsightsJobStatus::from_meta(
        body.get("async_status")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    let percent = body
        .get("async_percent_completion")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_f64().map(|n| n as u64))
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0)
        .min(100) as u8;
    let error_message = body
        .get("error_user_msg")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| body.get("error_message").and_then(|v| v.as_str()))
        .map(str::to_string);
    Ok(InsightsJob {
        site: site.clone(),
        id: job_id.into(),
        status,
        percent_complete: percent,
        error_code: body
            .get("error_code")
            .map(|v| {
                v.as_i64()
                    .map(|n| n.to_string())
                    .or_else(|| v.as_str().map(str::to_string))
                    .unwrap_or_default()
            })
            .filter(|s| !s.is_empty()),
        error_message,
    })
}

/// Graph returns numerics as JSON *strings* ("12.34", "12345") — parse
/// either shape, preserving int-ness for count metrics.
fn number(v: &Value) -> Option<Value> {
    if let Some(s) = v.as_str() {
        return s
            .parse::<u64>()
            .map(Value::from)
            .ok()
            .or_else(|| s.parse::<f64>().ok().map(Value::from));
    }
    if v.is_number() {
        return Some(v.clone());
    }
    None
}

/// Sum the action rows that mean "purchase". Meta's event taxonomy has
/// several purchase-ish action_types; the two below cover API and pixel.
fn is_purchase_action(kind: &str) -> bool {
    kind == "purchase" || kind == "offsite_conversion.fb_pixel_purchase"
}

fn purchases_of(item: &Value) -> Value {
    let mut total: u64 = 0;
    if let Some(actions) = item.get("actions").and_then(|a| a.as_array()) {
        for a in actions {
            let kind = a.get("action_type").and_then(|t| t.as_str()).unwrap_or("");
            if is_purchase_action(kind) {
                total += a
                    .get("value")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .or_else(|| a.get("value").and_then(|v| v.as_u64()))
                    .unwrap_or(0);
            }
        }
    }
    Value::from(total)
}

/// Sum the monetary purchase events that correspond to `purchases_of`.
/// `None` means Graph omitted `action_values`; zero is a meaningful result
/// when Graph supplied the array but it contained no purchase event.
fn purchase_value_of(item: &Value) -> Option<Value> {
    let values = item.get("action_values")?.as_array()?;
    let mut total = 0.0f64;
    for value in values {
        let kind = value
            .get("action_type")
            .and_then(|kind| kind.as_str())
            .unwrap_or("");
        if is_purchase_action(kind) {
            total += number(value.get("value").unwrap_or(&Value::Null))?.as_f64()?;
        }
    }
    Some(Value::from(total))
}

/// ROAS is a per-row derived value, not a Meta field. Null protects callers
/// from treating absent attribution data or a zero denominator as a real 0x.
fn video_thruplay_of(item: &Value) -> Value {
    let mut total: u64 = 0;
    if let Some(actions) = item
        .get("video_thruplay_watched_actions")
        .and_then(|a| a.as_array())
    {
        for action in actions {
            total += action
                .get("value")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .or_else(|| action.get("value").and_then(|v| v.as_u64()))
                .unwrap_or(0);
        }
    }
    Value::from(total)
}

fn roas_of(item: &Value) -> Option<Value> {
    let spend = number(item.get("spend").unwrap_or(&Value::Null))?.as_f64()?;
    if spend == 0.0 {
        return None;
    }
    let value = purchase_value_of(item)?.as_f64()?;
    Some(Value::from(value / spend))
}

/// Translate generic entity IDs into Meta's structured filtering grammar.
/// Accounts are already selected by the `/act_<id>/insights` path, so an
/// account-level entity filter would be misleading and is rejected early.
fn entity_filter(query: &InsightsQuery) -> Result<Option<String>, Error> {
    if query.entity_ids.is_empty() {
        return Ok(None);
    }
    let field = match query.level {
        InsightsLevel::Campaign => "campaign.id",
        InsightsLevel::Adset => "adset.id",
        InsightsLevel::Ad => "ad.id",
        InsightsLevel::Account => {
            return Err(Error::InvalidQuery {
                site: Site::new(SITE),
                reason: "entity_filter_unsupported:account".into(),
            });
        }
    };
    let mut ids = std::collections::BTreeSet::new();
    for id in &query.entity_ids {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::InvalidQuery {
                site: Site::new(SITE),
                reason: format!("bad_entity_id:{id}"),
            });
        }
        ids.insert(id);
    }
    Ok(Some(
        serde_json::json!([{
            "field": field,
            "operator": "IN",
            "value": ids.into_iter().collect::<Vec<_>>(),
        }])
        .to_string(),
    ))
}

fn row_from(item: &Value, query: &InsightsQuery) -> InsightRow {
    let entity_id = item
        .get(query.level.id_field())
        .and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .unwrap_or_default();
    let mut metrics = serde_json::Map::new();
    for m in &query.metrics {
        let value = match m {
            Metric::Purchases => purchases_of(item),
            Metric::PurchaseValue => purchase_value_of(item).unwrap_or(Value::Null),
            Metric::Roas => roas_of(item).unwrap_or(Value::Null),
            Metric::VideoThruplay => video_thruplay_of(item),
            Metric::QualityRanking => item
                .get("quality_ranking")
                .and_then(|v| v.as_str())
                .map(Value::from)
                .unwrap_or(Value::Null),
            _ => number(item.get(m.as_str()).unwrap_or(&Value::Null)).unwrap_or(Value::Null),
        };
        metrics.insert(m.as_str().into(), value);
    }
    let mut dimensions = serde_json::Map::new();
    for breakdown in &query.breakdowns {
        dimensions.insert(
            breakdown.as_str().into(),
            item.get(breakdown.as_str()).cloned().unwrap_or(Value::Null),
        );
    }
    InsightRow {
        entity_id,
        level: query.level,
        date_start: item
            .get("date_start")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string(),
        dimensions,
        metrics,
    }
}

fn require_oauth(app: &AppConfig) -> Result<&OAuthApp, Error> {
    app.oauth.as_ref().ok_or_else(|| Error::Auth {
        site: Site::new(SITE),
        reason: "missing_app_config".into(),
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Meta's long-lived exchange: `fb_exchange_token` grant, ~60-day token.
/// The grant requires both `client_id` and `client_secret` — Graph answers
/// `101: Missing client_id parameter` otherwise.
async fn long_lived(
    http: &Http,
    graph_origin: &str,
    client_id: &str,
    client_secret: &str,
    token: &str,
    deadline: Deadline,
) -> Result<TokenLong, Error> {
    let site = Site::new(SITE);
    let q = form(&[
        ("grant_type", "fb_exchange_token"),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("fb_exchange_token", token),
    ]);
    let url = format!("{graph_origin}/oauth/access_token?{q}");
    let resp = http.send(http.get(&url), deadline, &site).await?;
    let body = read_json(resp, &site).await?;
    Ok(TokenLong {
        access_token: body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Auth {
                site: site.clone(),
                reason: "missing_access_token".into(),
            })?
            .to_string(),
        expires_in: body.get("expires_in").and_then(|v| v.as_u64()),
    })
}

struct TokenLong {
    access_token: String,
    expires_in: Option<u64>,
}

fn creds_from_long(long: &TokenLong, user_id: Option<String>) -> AccountCreds {
    let mut extra = serde_json::Map::new();
    if let Some(id) = user_id {
        extra.insert("user_id".into(), Value::String(id));
    }
    extra.insert("token_kind".into(), Value::String("user_oauth".into()));
    extra.insert("refreshed_at".into(), Value::from(unix_now()));
    if let Some(exp) = long.expires_in {
        extra.insert(
            "expires_at".into(),
            Value::from(unix_now().saturating_add(exp)),
        );
    }
    AccountCreds::OAuth2 {
        access_token: long.access_token.clone(),
        refresh_token: None,
        extra: Value::Object(extra),
    }
}

async fn whoami(http: &Http, base: &str, token: &str, deadline: Deadline) -> Result<WhoAmI, Error> {
    let site = Site::new(SITE);
    let q = form(&[("fields", "id,name"), ("access_token", token)]);
    let url = format!("{base}/me?{q}");
    let resp = http.send(http.get(&url), deadline, &site).await?;
    let body = read_json(resp, &site).await?;
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_id".into(),
            message: "whoami returned no id".into(),
        })?
        .to_string();
    Ok(WhoAmI {
        site,
        id,
        handle: body.get("name").and_then(|v| v.as_str()).map(String::from),
    })
}

/// First ad account visible to the token, as `act_<account_id>`. This is kept
/// only for existing auth compatibility; `ads accounts` lets new operators
/// discover IDs and select them explicitly with `insights --ad-account`.
async fn first_ad_account(
    http: &Http,
    base: &str,
    token: &str,
    deadline: Deadline,
) -> Result<Option<String>, Error> {
    Ok(list_ad_accounts(http, base, token, deadline)
        .await?
        .into_iter()
        .next()
        .map(|account| account.id))
}

/// Page through every account visible to the credential. The same deadline
/// and cap as insights prevent account discovery from becoming an unbounded
/// read if Graph returns a malformed cursor cycle.
async fn list_ad_accounts(
    http: &Http,
    base: &str,
    token: &str,
    deadline: Deadline,
) -> Result<Vec<AdAccount>, Error> {
    let site = Site::new(SITE);
    let q = form(&[
        (
            "fields",
            "account_id,name,currency,timezone_name,account_status",
        ),
        ("limit", "100"),
        ("access_token", token),
    ]);
    let mut next = Some(format!("{base}/me/adaccounts?{q}"));
    let mut pages = 0usize;
    let mut accounts = Vec::new();
    while let Some(url) = next {
        deadline.check(&site)?;
        pages += 1;
        if pages > MAX_PAGES {
            return Err(Error::Platform {
                site: site.clone(),
                code: "paging_exceeded".into(),
                message: format!("ad account paging exceeded {MAX_PAGES} pages"),
            });
        }
        let resp = http.send(http.get(&url), deadline, &site).await?;
        let body = read_json(resp, &site).await?;
        if let Some(data) = body.get("data").and_then(|data| data.as_array()) {
            for account in data {
                accounts.push(ad_account_from(account)?);
            }
        }
        next = body
            .get("paging")
            .and_then(|paging| paging.get("next"))
            .and_then(|next| next.as_str())
            .map(str::to_owned);
    }
    accounts.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(accounts)
}

fn value_string(value: Option<&Value>) -> Option<String> {
    value.and_then(|value| {
        value
            .as_str()
            .map(str::to_owned)
            .or_else(|| value.as_i64().map(|value| value.to_string()))
            .or_else(|| value.as_u64().map(|value| value.to_string()))
    })
}

/// Status and issue strings that contain only whitespace are semantically
/// missing; treating them as real values would falsely imply Meta answered a
/// review question when it did not.
fn nonempty_value_string(value: Option<&Value>) -> Option<String> {
    value_string(value).filter(|value| !value.trim().is_empty())
}

fn ad_account_from(value: &Value) -> Result<AdAccount, Error> {
    let raw = value_string(value.get("account_id")).ok_or_else(|| Error::Platform {
        site: Site::new(SITE),
        code: "missing_account_id".into(),
        message: "ad account returned no account_id".into(),
    })?;
    let digits = raw.strip_prefix("act_").unwrap_or(&raw);
    if digits.is_empty() || !digits.chars().all(|digit| digit.is_ascii_digit()) {
        return Err(Error::Platform {
            site: Site::new(SITE),
            code: "bad_account_id".into(),
            message: "ad account returned an invalid account_id".into(),
        });
    }
    Ok(AdAccount {
        id: format!("act_{digits}"),
        name: value_string(value.get("name")),
        currency: value_string(value.get("currency")),
        timezone: value_string(value.get("timezone_name")),
        status: value_string(value.get("account_status")),
    })
}

async fn account_currency(
    http: &Http,
    base: &str,
    account: &str,
    token: &str,
    deadline: Deadline,
) -> Result<Option<String>, Error> {
    let site = Site::new(SITE);
    let q = form(&[("fields", "currency"), ("access_token", token)]);
    let url = format!("{base}/act_{account}?{q}");
    let resp = http.send(http.get(&url), deadline, &site).await?;
    let body = read_json(resp, &site).await?;
    Ok(body
        .get("currency")
        .and_then(|c| c.as_str())
        .map(String::from))
}

fn access_token(creds: &AccountCreds) -> Result<&str, Error> {
    match creds {
        AccountCreds::OAuth2 { access_token, .. } => Ok(access_token),
        _ => Err(Error::Auth {
            site: Site::new(SITE),
            reason: "wrong_cred_kind".into(),
        }),
    }
}

fn extra_string(creds: &AccountCreds, key: &str) -> Option<String> {
    let AccountCreds::OAuth2 { extra, .. } = creds else {
        return None;
    };
    extra.get(key).and_then(|v| v.as_str()).map(String::from)
}

fn app_access_token(oauth: &OAuthApp) -> String {
    format!("{}|{}", oauth.client_id, oauth.client_secret)
}

/// System User tokens are often labelled `USER` by `/debug_token` (the
/// official field table does not even document `type`). A person OAuth token
/// still has a non-zero `expires_at` (~60 days). Never-expiring USER tokens
/// are accepted; PAGE/APP tokens are not a Marketing API system user.
fn refuse_non_system_user_debug(
    site: &Site,
    debug_type: Option<&str>,
    expires_at: Option<u64>,
) -> Result<(), Error> {
    let kind = debug_type.map(|value| value.to_ascii_uppercase());
    match kind.as_deref() {
        Some("SYSTEM_USER") => Ok(()),
        Some("PAGE") | Some("APP") => Err(Error::Auth {
            site: site.clone(),
            reason: format!("wrong_token_type:{}", kind.as_deref().unwrap_or("")),
        }),
        Some("USER") | None => {
            if expires_at.is_none() {
                Ok(())
            } else {
                Err(Error::Auth {
                    site: site.clone(),
                    reason: "user_token_not_system_user".into(),
                })
            }
        }
        Some(other) => Err(Error::Auth {
            site: site.clone(),
            reason: format!("wrong_token_type:{other}"),
        }),
    }
}

fn system_user_creds(
    token: &str,
    user_id: &str,
    ad_account_id: &str,
    expires_at: Option<u64>,
) -> AccountCreds {
    let mut extra = serde_json::Map::new();
    extra.insert("user_id".into(), Value::String(user_id.into()));
    extra.insert("ad_account_id".into(), Value::String(ad_account_id.into()));
    extra.insert(
        "token_kind".into(),
        Value::String(SYSTEM_USER_TOKEN_KIND.into()),
    );
    if let Some(expires_at) = expires_at {
        extra.insert("expires_at".into(), Value::from(expires_at));
    }
    AccountCreds::OAuth2 {
        access_token: token.into(),
        refresh_token: None,
        extra: Value::Object(extra),
    }
}

/// `GET /debug_token`. The app access token authenticates the inspect call;
/// `input_token` is the vault credential. Neither value is copied into errors.
async fn debug_token(
    http: &Http,
    base: &str,
    oauth: &OAuthApp,
    input_token: &str,
    deadline: Deadline,
    vault_kind: AdsTokenKind,
) -> Result<AdsTokenInspection, Error> {
    let site = Site::new(SITE);
    let app_token = app_access_token(oauth);
    let q = form(&[("input_token", input_token), ("access_token", &app_token)]);
    let url = format!("{base}/debug_token?{q}");
    let resp = http.send(http.get(&url), deadline, &site).await?;
    let body = read_json(resp, &site).await?;
    let data = body.get("data").cloned().unwrap_or(Value::Null);
    Ok(AdsTokenInspection {
        site,
        token_kind: vault_kind,
        debug_type: data
            .get("type")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        is_valid: data
            .get("is_valid")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        expires_at: unix_field(&data, "expires_at"),
        data_access_expires_at: unix_field(&data, "data_access_expires_at"),
        scopes: data
            .get("scopes")
            .and_then(|v| v.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        user_id: value_string(data.get("user_id")),
        app_id: value_string(data.get("app_id")),
        application: data
            .get("application")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

fn unix_field(data: &Value, key: &str) -> Option<u64> {
    let value = data.get(key)?;
    let n = value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))?;
    (n != 0).then_some(n)
}

fn access_tier_from_headers(headers: &reqwest::header::HeaderMap) -> Option<String> {
    const NAMES: &[&str] = &[
        "x-fb-ads-insights-throttle",
        "x-ad-account-usage",
        "x-business-use-case-usage",
    ];
    for name in NAMES {
        let Some(value) = headers.get(*name).and_then(|value| value.to_str().ok()) else {
            continue;
        };
        if let Ok(json) = serde_json::from_str::<Value>(value) {
            if let Some(tier) = json_ads_api_access_tier(&json) {
                return Some(tier);
            }
        }
    }
    None
}

fn json_ads_api_access_tier(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(tier) = map.get("ads_api_access_tier").and_then(|v| v.as_str()) {
                return Some(tier.to_string());
            }
            for nested in map.values() {
                if let Some(tier) = json_ads_api_access_tier(nested) {
                    return Some(tier);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(json_ads_api_access_tier),
        _ => None,
    }
}

async fn marketing_api_access_tier(
    http: &Http,
    base: &str,
    token: &str,
    deadline: Deadline,
) -> Result<MarketingApiAccessTier, Error> {
    let site = Site::new(SITE);
    let q = form(&[
        ("fields", "account_id"),
        ("limit", "1"),
        ("access_token", token),
    ]);
    let url = format!("{base}/me/adaccounts?{q}");
    let resp = http.send(http.get(&url), deadline, &site).await?;
    let raw = access_tier_from_headers(resp.headers());
    // Consume the body so a 4xx still maps through the usual Graph errors.
    let _ = read_json(resp, &site).await?;
    let tier = raw
        .as_deref()
        .map(MarketingApiAccessTierKind::from_header)
        .unwrap_or(MarketingApiAccessTierKind::Unknown);
    Ok(MarketingApiAccessTier {
        site,
        tier,
        raw,
        source: if tier == MarketingApiAccessTierKind::Unknown {
            "dashboard".into()
        } else {
            "response_header".into()
        },
        dashboard: MARKETING_API_ACCESS_TIER_DASHBOARD.into(),
    })
}

/// Resolve the ad account digits: query override (accepts `123` or
/// `act_123`) outranks the one stored at auth time.
fn account_id(creds: &AccountCreds, override_: Option<&str>) -> Result<String, Error> {
    let raw = override_
        .map(str::to_string)
        .or_else(|| extra_string(creds, "ad_account_id"))
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "no_ad_account".into(),
        })?;
    let digits = raw.strip_prefix("act_").unwrap_or(&raw);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(Error::InvalidQuery {
            site: Site::new(SITE),
            reason: format!("bad_ad_account:{raw}"),
        });
    }
    Ok(digits.to_string())
}

async fn read_json(resp: reqwest::Response, site: &Site) -> Result<Value, Error> {
    let status = resp.status();
    // Meta Ads uses token-bearing query URLs. Redact a response-read failure
    // for the same reason that Http::send redacts transport failures.
    let text = resp.text().await.map_err(|_| Error::request_failed(site))?;
    if !status.is_success() {
        return Err(map_graph_error(status.as_u16(), &text));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| Error::Platform {
        site: site.clone(),
        code: "bad_json".into(),
        message: e.to_string(),
    })?;
    if v.get("error").is_some() {
        return Err(map_graph_error(status.as_u16(), &text));
    }
    Ok(v)
}

/// Same discipline as the Threads mapper (019): numeric codes are the
/// primary classifier; English-substring fallbacks run only when Meta sent
/// no code at all. Codes here are the Marketing-API set.
fn map_graph_error(http_status: u16, body: &str) -> Error {
    let site = Site::new(SITE);
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let err = v.get("error");
    let code = err
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_i64())
        .unwrap_or(0);
    let subcode = err
        .and_then(|e| e.get("error_subcode"))
        .and_then(|c| c.as_i64())
        .unwrap_or(0);
    // Meta's generic `message` is often only "Invalid parameter". Prefer
    // `error_user_msg`, and when `error_user_title` is present prepend it
    // so the dialog title can change operator guidance.
    let user_msg = err
        .and_then(|e| e.get("error_user_msg"))
        .and_then(|m| m.as_str())
        .filter(|message| !message.trim().is_empty());
    let user_title = err
        .and_then(|e| e.get("error_user_title"))
        .and_then(|m| m.as_str())
        .filter(|title| !title.trim().is_empty());
    let message = match (user_title, user_msg) {
        (Some(title), Some(msg)) => format!("{title}: {msg}"),
        (None, Some(msg)) => msg.to_string(),
        (Some(title), None) => title.to_string(),
        (None, None) => err
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .or_else(|| v.get("error_message").and_then(|m| m.as_str()))
            .unwrap_or(body)
            .to_string(),
    };
    let lower = message.to_ascii_lowercase();
    // 80004 is the Marketing API ads-management rate limit; 341 is Graph's
    // application-limit / throttling code. Both are retryable waits, not
    // validation failures.
    let (auth_hit, rate_hit) = if code != 0 {
        (
            matches!(code, 190 | 102),
            matches!(code, 4 | 17 | 32 | 341 | 613 | 80004),
        )
    } else {
        (
            lower.contains("validating access token")
                || lower.contains("expired")
                || lower.contains("invalid oauth"),
            lower.contains("quota")
                || lower.contains("rate limit")
                || lower.contains("publishing limit"),
        )
    };
    if auth_hit {
        // Checkpoint / install / unconfirmed cannot be cleared by
        // `fb_exchange_token`. Only treat expired/invalid-token subcodes
        // (and a bare 190/102) as refreshable `token_expired`.
        let reason = match subcode {
            458 => "app_not_installed",
            459 => "user_checkpointed",
            464 => "unconfirmed_user",
            _ => "token_expired",
        };
        return Error::Auth {
            site,
            reason: reason.into(),
        };
    }
    if rate_hit {
        return Error::RateLimited {
            site,
            retry_after: None,
        };
    }
    if http_status >= 500 {
        if err.is_some() && code != 0 {
            return Error::Platform {
                site,
                code: code.to_string(),
                message: message.to_string(),
            };
        }
        return Error::Network {
            site,
            message: if message.is_empty() {
                format!("http_{http_status}")
            } else {
                message.to_string()
            },
        };
    }
    Error::Platform {
        site,
        code: code.to_string(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ads::{
        AdEntity, AdPreviewFormat, AdReviewStatusRequest, AdsConfiguredStatus, AdsInspectRequest,
        AdsInventoryKind, AdsInventoryRequest, AdsStatusUpdateRequest, CampaignObjective,
        CreateLinkAdCreativeRequest, CreativePreviewRequest, LinkAdCreative, LinkCallToAction,
        PausedAd, PausedAdset, PausedCampaign, UploadAdImageRequest,
    };
    use crate::facets::{AdsManager, InsightsSource};
    use crate::insights::{AttributionWindow, InsightsLevel, InsightsQuery, Metric};
    use httpmock::prelude::*;
    use serde_json::json;

    fn token_creds(account: &str) -> AccountCreds {
        AccountCreds::OAuth2 {
            access_token: "tok".into(),
            refresh_token: None,
            extra: json!({ "ad_account_id": account }),
        }
    }

    fn empty_app() -> AppConfig {
        AppConfig {
            site: Site::new(SITE),
            oauth: None,
            extra: json!({}),
        }
    }

    fn oauth_app() -> AppConfig {
        AppConfig {
            site: Site::new(SITE),
            oauth: Some(OAuthApp {
                client_id: "id".into(),
                client_secret: "sec".into(),
                redirect_uri: "https://localhost/callback".into(),
            }),
            extra: json!({}),
        }
    }

    fn query() -> InsightsQuery {
        InsightsQuery {
            level: InsightsLevel::Campaign,
            metrics: vec![Metric::Spend, Metric::Impressions, Metric::Purchases],
            range: crate::insights::DateRange {
                from: "2026-06-01".into(),
                to: "2026-06-02".into(),
            },
            attribution: AttributionWindow::SevenDayClickOneDayView,
            account: None,
            entity_ids: vec![],
            breakdowns: vec![],
            report: crate::insights::InsightsReportKind::Performance,
        }
    }

    fn mock_account_currency(server: &MockServer, currency: &str) {
        server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/act_123")
                .query_param("fields", "currency");
            then.status(200).json_body(json!({ "currency": currency }));
        });
    }

    #[test]
    fn attribution_param_maps_every_preset_to_the_wire_array() {
        // regression (fcba1bd): Graph code-100 rejects the combined display
        // name; every preset must serialize as an array of atomic windows
        assert_eq!(
            attribution_param(AttributionWindow::SevenDayClickOneDayView),
            r#"["7d_click","1d_view"]"#
        );
        assert_eq!(
            attribution_param(AttributionWindow::OneDayClick),
            r#"["1d_click"]"#
        );
        assert_eq!(
            attribution_param(AttributionWindow::OneDayView),
            r#"["1d_view"]"#
        );
        for a in [
            AttributionWindow::SevenDayClickOneDayView,
            AttributionWindow::OneDayClick,
            AttributionWindow::OneDayView,
        ] {
            let p = attribution_param(a);
            assert!(p.starts_with('['), "not an array: {p}");
            assert!(!p.contains("7d_click_1d_view"), "display name leaked: {p}");
        }
    }

    #[test]
    fn graph_error_codes_classify() {
        // code-first: copy containing "expired"/"quota" cannot hijack code 100
        let err = map_graph_error(
            400,
            r#"{"error":{"code":100,"message":"token expired; quota weirdness"}}"#,
        );
        assert!(matches!(err, Error::Platform { code, .. } if code == "100"));
        let err = map_graph_error(
            400,
            r#"{"error":{"code":190,"message":"Error validating access token"}}"#,
        );
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "token_expired"));
        for code in [4, 17, 32, 341, 613, 80004] {
            let body = format!(r#"{{"error":{{"code":{code},"message":"x"}}}}"#);
            assert!(matches!(
                map_graph_error(400, &body),
                Error::RateLimited { .. }
            ));
        }
        // no code at all: the substring fallback still classifies
        let err = map_graph_error(400, r#"{"error":{"message":"rate limit reached"}}"#);
        assert!(matches!(err, Error::RateLimited { .. }));
        let err = map_graph_error(500, "");
        assert!(matches!(err, Error::Network { ref message, .. } if message == "http_500"));
    }

    #[test]
    fn graph_error_prefers_meta_operator_message_over_generic_summary() {
        // Regression for the paused-ad validation: code 100's generic
        // "Invalid parameter" hid Meta's actionable billing requirement.
        let err = map_graph_error(
            400,
            r#"{"error":{"code":100,"message":"Invalid parameter","error_user_msg":"Update payment method: add a valid payment method."}}"#,
        );
        assert!(
            matches!(err, Error::Platform { code, message, .. } if code == "100" && message == "Update payment method: add a valid payment method.")
        );

        // Empty optional detail must fall back to the regular Meta message.
        let err = map_graph_error(
            400,
            r#"{"error":{"code":100,"message":"Invalid parameter","error_user_msg":"   "}}"#,
        );
        assert!(matches!(err, Error::Platform { message, .. } if message == "Invalid parameter"));

        let err = map_graph_error(
            400,
            r#"{"error":{"code":100,"message":"Invalid parameter","error_user_title":"Payment needed","error_user_msg":"Add a valid payment method."}}"#,
        );
        assert!(
            matches!(err, Error::Platform { message, .. } if message == "Payment needed: Add a valid payment method.")
        );
    }

    #[test]
    fn marketing_error_shapes_that_change_retry_or_guidance() {
        // One fixture per Marketing/Graph shape that changes Postkit's
        // retry decision or operator text. Codes from Meta's error
        // reference and Graph error-handling tables (v26.0).
        #[allow(clippy::type_complexity)]
        let cases: &[(&str, fn(&Error) -> bool)] = &[
            (
                r#"{"error":{"code":80004,"message":"There have been too many calls to this ad-account"}}"#,
                |err| matches!(err, Error::RateLimited { .. }),
            ),
            (
                r#"{"error":{"code":341,"message":"Application limit reached"}}"#,
                |err| matches!(err, Error::RateLimited { .. }),
            ),
            (
                r#"{"error":{"code":102,"message":"API session"}}"#,
                |err| matches!(err, Error::Auth { reason, .. } if reason == "token_expired"),
            ),
            (
                r#"{"error":{"code":190,"error_subcode":463,"message":"Error validating access token"}}"#,
                |err| matches!(err, Error::Auth { reason, .. } if reason == "token_expired"),
            ),
            (
                r#"{"error":{"code":190,"error_subcode":467,"message":"Invalid OAuth 2.0 Access Token"}}"#,
                |err| matches!(err, Error::Auth { reason, .. } if reason == "token_expired"),
            ),
            (
                r#"{"error":{"code":190,"error_subcode":459,"message":"Error validating access token","error_user_title":"Confirm your identity"}}"#,
                |err| matches!(err, Error::Auth { reason, .. } if reason == "user_checkpointed"),
            ),
            (
                r#"{"error":{"code":190,"error_subcode":458,"message":"Error validating access token"}}"#,
                |err| matches!(err, Error::Auth { reason, .. } if reason == "app_not_installed"),
            ),
            (
                r#"{"error":{"code":190,"error_subcode":464,"message":"Error validating access token"}}"#,
                |err| matches!(err, Error::Auth { reason, .. } if reason == "unconfirmed_user"),
            ),
            (
                r#"{"error":{"code":100,"message":"Invalid parameter","error_user_title":"Budget too low","error_user_msg":"Increase the daily budget."}}"#,
                |err| matches!(err, Error::Platform { code, message, .. } if code == "100" && message == "Budget too low: Increase the daily budget."),
            ),
        ];
        for (body, check) in cases {
            let err = map_graph_error(400, body);
            assert!(check(&err), "shape {body} classified as {err:?}");
        }
    }

    #[tokio::test]
    async fn auth_start_url_shape() {
        let t = MetaAds::new().unwrap();
        match t.auth_start(&oauth_app()).await.unwrap() {
            AuthStart::Browser {
                authorize_url,
                state,
            } => {
                assert!(authorize_url.starts_with("https://www.facebook.com/dialog/oauth?"));
                assert_eq!(
                    crate::oauth::query_param(&authorize_url, "scope").as_deref(),
                    Some(SCOPES)
                );
                // Regression (d984e73): asserted against literals, not the
                // SCOPES constant — without pages_show_list the token cannot
                // see any Page, and without pages_manage_ads a Page-backed
                // creative cannot act on one; Tier B's creative step was
                // unreachable until both were added.
                let scope = crate::oauth::query_param(&authorize_url, "scope").unwrap();
                for required in [
                    "ads_read",
                    "ads_management",
                    "pages_show_list",
                    "pages_manage_ads",
                ] {
                    assert!(
                        scope.split(',').any(|s| s == required),
                        "scope lost {required}"
                    );
                }
                assert!(authorize_url.contains("response_type=code"));
                assert_eq!(
                    crate::oauth::query_param(&authorize_url, "state").as_deref(),
                    Some(&state[..])
                );
            }
            other => panic!("{other:?}"),
        }
        let err = t.auth_start(&empty_app()).await.unwrap_err();
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "missing_app_config"));
    }

    #[tokio::test]
    async fn paused_creates_use_only_paused_forms_and_correct_edges() {
        let server = MockServer::start();
        let campaign = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/act_123/campaigns")
                .body_contains("objective=OUTCOME_SALES")
                .body_contains("status=PAUSED")
                .body_contains("special_ad_categories=%5B%5D")
                // Mirrors Meta's required campaign choice for an ad-set
                // budget. A missing flag reaches the API as code 100 rather
                // than a locally actionable error.
                .body_contains("is_adset_budget_sharing_enabled=false");
            then.status(200).json_body(json!({ "id": "100" }));
        });
        let adset = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/act_123/adsets")
                .body_contains("campaign_id=100")
                .body_contains("daily_budget=2500")
                .body_contains("bid_strategy=LOWEST_COST_WITHOUT_CAP")
                .body_contains("targeting=%7B")
                .body_contains("status=PAUSED");
            then.status(200).json_body(json!({ "id": "200" }));
        });
        let ad = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/act_123/ads")
                .body_contains("adset_id=200")
                .body_contains("creative=%7B%22creative_id%22%3A%22300%22%7D")
                .body_contains("status=PAUSED");
            then.status(200).json_body(json!({ "id": "400" }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let creds = token_creds("123");

        let campaign_out = connector
            .create_paused_ad(
                &empty_app(),
                &creds,
                &CreatePausedAdRequest {
                    account: None,
                    create: PausedAdCreate::Campaign(PausedCampaign {
                        name: "paused campaign".into(),
                        objective: CampaignObjective::Sales,
                        special_ad_categories: vec![],
                        daily_budget: None,
                        lifetime_budget: None,
                        is_adset_budget_sharing_enabled: false,
                    }),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        let adset_out = connector
            .create_paused_ad(
                &empty_app(),
                &creds,
                &CreatePausedAdRequest {
                    account: None,
                    create: PausedAdCreate::Adset(PausedAdset {
                        name: "paused ad set".into(),
                        campaign_id: "100".into(),
                        daily_budget: Some(2500),
                        lifetime_budget: None,
                        bid_strategy: crate::ads::BidStrategy::LowestCostWithoutCap,
                        bid_amount: None,
                        roas_average_floor: None,
                        billing_event: crate::ads::BillingEvent::Impressions,
                        optimization_goal: crate::ads::OptimizationGoal::Reach,
                        targeting: crate::ads::AdTargeting {
                            geo_locations: crate::ads::GeoLocations {
                                countries: vec!["MY".into()],
                            },
                            age_min: None,
                            age_max: None,
                            publisher_platforms: vec![],
                            facebook_positions: vec![],
                            instagram_positions: vec![],
                            whatsapp_positions: vec![],
                            user_age_unknown: None,
                        },
                        start_time: None,
                        end_time: None,
                        promoted_object: None,
                    }),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        let ad_out = connector
            .create_paused_ad(
                &empty_app(),
                &creds,
                &CreatePausedAdRequest {
                    account: None,
                    create: PausedAdCreate::Ad(PausedAd {
                        name: "paused ad".into(),
                        adset_id: "200".into(),
                        creative_id: "300".into(),
                    }),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();

        campaign.assert();
        adset.assert();
        ad.assert();
        assert_eq!(campaign_out.id, "100");
        assert_eq!(adset_out.entity, crate::ads::AdEntity::Adset);
        assert_eq!(ad_out.status, "PAUSED");
    }

    #[tokio::test]
    async fn cbo_and_lifetime_budgets_are_posted_as_form_fields() {
        let server = MockServer::start();
        let campaign = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/act_123/campaigns")
                .body_contains("daily_budget=5000")
                .body_contains("is_adset_budget_sharing_enabled=false")
                .body_contains("status=PAUSED");
            then.status(200).json_body(json!({ "id": "100" }));
        });
        let adset = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/act_123/adsets")
                .body_contains("lifetime_budget=20000")
                .body_contains("end_time=2026-11-21T14%3A26%3A09-08%3A00")
                .body_contains("status=PAUSED");
            then.status(200).json_body(json!({ "id": "200" }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let creds = token_creds("123");

        connector
            .create_paused_ad(
                &empty_app(),
                &creds,
                &CreatePausedAdRequest {
                    account: None,
                    create: PausedAdCreate::Campaign(PausedCampaign {
                        name: "cbo campaign".into(),
                        objective: CampaignObjective::Awareness,
                        special_ad_categories: vec![],
                        daily_budget: Some(5000),
                        lifetime_budget: None,
                        is_adset_budget_sharing_enabled: false,
                    }),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        connector
            .create_paused_ad(
                &empty_app(),
                &creds,
                &CreatePausedAdRequest {
                    account: None,
                    create: PausedAdCreate::Adset(PausedAdset {
                        name: "lifetime ad set".into(),
                        campaign_id: "100".into(),
                        daily_budget: None,
                        lifetime_budget: Some(20_000),
                        bid_strategy: crate::ads::BidStrategy::LowestCostWithoutCap,
                        bid_amount: None,
                        roas_average_floor: None,
                        billing_event: crate::ads::BillingEvent::Impressions,
                        optimization_goal: crate::ads::OptimizationGoal::Reach,
                        targeting: crate::ads::AdTargeting {
                            geo_locations: crate::ads::GeoLocations {
                                countries: vec!["MY".into()],
                            },
                            age_min: None,
                            age_max: None,
                            publisher_platforms: vec![],
                            facebook_positions: vec![],
                            instagram_positions: vec![],
                            whatsapp_positions: vec![],
                            user_age_unknown: None,
                        },
                        start_time: Some("2026-11-11T14:26:09-08:00".into()),
                        end_time: Some("2026-11-21T14:26:09-08:00".into()),
                        promoted_object: None,
                    }),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();

        campaign.assert();
        adset.assert();
    }

    #[tokio::test]
    async fn cap_and_min_roas_constraints_are_posted() {
        let server = MockServer::start();
        let cap = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/act_123/adsets")
                .body_contains("bid_strategy=COST_CAP")
                .body_contains("bid_amount=200")
                .body_contains("status=PAUSED");
            then.status(200).json_body(json!({ "id": "201" }));
        });
        let roas = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/act_123/adsets")
                .body_contains("bid_strategy=LOWEST_COST_WITH_MIN_ROAS")
                .body_contains("roas_average_floor")
                .body_contains("10000")
                .body_contains("status=PAUSED");
            then.status(200).json_body(json!({ "id": "202" }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let creds = token_creds("123");
        let targeting = crate::ads::AdTargeting {
            geo_locations: crate::ads::GeoLocations {
                countries: vec!["MY".into()],
            },
            age_min: None,
            age_max: None,
            publisher_platforms: vec![],
            facebook_positions: vec![],
            instagram_positions: vec![],
            whatsapp_positions: vec![],
            user_age_unknown: None,
        };

        connector
            .create_paused_ad(
                &empty_app(),
                &creds,
                &CreatePausedAdRequest {
                    account: None,
                    create: PausedAdCreate::Adset(PausedAdset {
                        name: "cap".into(),
                        campaign_id: "100".into(),
                        daily_budget: Some(2500),
                        lifetime_budget: None,
                        bid_strategy: crate::ads::BidStrategy::CostCap,
                        bid_amount: Some(200),
                        roas_average_floor: None,
                        billing_event: crate::ads::BillingEvent::Impressions,
                        optimization_goal: crate::ads::OptimizationGoal::Reach,
                        targeting: targeting.clone(),
                        start_time: None,
                        end_time: None,
                        promoted_object: None,
                    }),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        connector
            .create_paused_ad(
                &empty_app(),
                &creds,
                &CreatePausedAdRequest {
                    account: None,
                    create: PausedAdCreate::Adset(PausedAdset {
                        name: "roas".into(),
                        campaign_id: "100".into(),
                        daily_budget: Some(2500),
                        lifetime_budget: None,
                        bid_strategy: crate::ads::BidStrategy::LowestCostWithMinRoas,
                        bid_amount: None,
                        roas_average_floor: Some(10_000),
                        billing_event: crate::ads::BillingEvent::Impressions,
                        optimization_goal: crate::ads::OptimizationGoal::Value,
                        targeting,
                        start_time: None,
                        end_time: None,
                        promoted_object: Some(crate::ads::PromotedObject::Pixel {
                            pixel_id: "789".into(),
                            custom_event_type: crate::ads::CustomEventType::Purchase,
                        }),
                    }),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();

        cap.assert();
        roas.assert();
    }

    #[tokio::test]
    async fn promoted_object_is_posted_without_kind_tag() {
        let server = MockServer::start();
        let adset = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/act_123/adsets")
                .body_contains("promoted_object=")
                .body_contains("pixel_id")
                .body_contains("PURCHASE")
                .body_contains("status=PAUSED");
            then.status(200).json_body(json!({ "id": "203" }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        connector
            .create_paused_ad(
                &empty_app(),
                &token_creds("123"),
                &CreatePausedAdRequest {
                    account: None,
                    create: PausedAdCreate::Adset(PausedAdset {
                        name: "pixel set".into(),
                        campaign_id: "100".into(),
                        daily_budget: Some(2500),
                        lifetime_budget: None,
                        bid_strategy: crate::ads::BidStrategy::LowestCostWithoutCap,
                        bid_amount: None,
                        roas_average_floor: None,
                        billing_event: crate::ads::BillingEvent::Impressions,
                        optimization_goal: crate::ads::OptimizationGoal::OffsiteConversions,
                        targeting: crate::ads::AdTargeting {
                            geo_locations: crate::ads::GeoLocations {
                                countries: vec!["MY".into()],
                            },
                            age_min: None,
                            age_max: None,
                            publisher_platforms: vec![],
                            facebook_positions: vec![],
                            instagram_positions: vec![],
                            whatsapp_positions: vec![],
                            user_age_unknown: None,
                        },
                        start_time: None,
                        end_time: None,
                        promoted_object: Some(crate::ads::PromotedObject::Pixel {
                            pixel_id: "789".into(),
                            custom_event_type: crate::ads::CustomEventType::Purchase,
                        }),
                    }),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        adset.assert();
    }

    #[tokio::test]
    async fn image_link_creative_uploads_media_then_posts_reviewable_story_spec() {
        let server = MockServer::start();
        let image = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/act_123/adimages")
                // `filename` is a multipart field containing raw selected
                // bytes; an account image upload never creates an ad.
                .body_contains("name=\"filename\"; filename=\"hero.png\"")
                .body_contains("not-a-real-png");
            then.status(200).json_body(json!({
                "images": { "hero.png": { "hash": "hash-1" } }
            }));
        });
        let creative = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/act_123/adcreatives")
                .body_contains("name=Hero+link")
                .body_contains("object_story_spec=%7B")
                .body_contains("%22page_id%22%3A%22456%22")
                .body_contains("%22image_hash%22%3A%22hash-1%22")
                .body_contains("https%3A%2F%2Fexample.com%2Foffer")
                .body_contains("LEARN_MORE");
            then.status(200).json_body(json!({ "id": "500" }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let creds = token_creds("123");

        let uploaded = connector
            .upload_ad_image(
                &empty_app(),
                &creds,
                &UploadAdImageRequest {
                    account: None,
                    filename: "hero.png".into(),
                    bytes: b"not-a-real-png".to_vec(),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        let created = connector
            .create_link_ad_creative(
                &empty_app(),
                &creds,
                &CreateLinkAdCreativeRequest {
                    account: None,
                    creative: LinkAdCreative {
                        name: "Hero link".into(),
                        page_id: "456".into(),
                        image_hash: uploaded.hash,
                        message: "A clear benefit".into(),
                        headline: "Learn more".into(),
                        destination_url: "https://example.com/offer".into(),
                        call_to_action: LinkCallToAction::LearnMore,
                        geo_link: None,
                        application_id: None,
                        app_link: None,
                        instagram_user_id: None,
                        advantage_plus: false,
                        whatsapp_identity: None,
                    },
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();

        image.assert();
        creative.assert();
        assert_eq!(created.id, "500");
        assert_eq!(created.account_id, "act_123");
    }

    #[tokio::test]
    async fn video_upload_posts_source_and_status_reads_video_status() {
        let server = MockServer::start();
        let upload = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/act_123/advideos")
                .body_contains("name=\"source\"; filename=\"hero.mp4\"");
            then.status(200).json_body(json!({ "id": "9001" }));
        });
        let status = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/9001")
                .query_param("fields", "status");
            then.status(200).json_body(json!({
                "id": "9001",
                "status": { "video_status": "processing" }
            }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let creds = token_creds("123");
        let uploaded = connector
            .upload_ad_video(
                &empty_app(),
                &creds,
                &crate::ads::UploadAdVideoRequest {
                    account: None,
                    filename: "hero.mp4".into(),
                    bytes: b"not-a-real-mp4".to_vec(),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        let status_out = connector
            .ad_video_status(
                &empty_app(),
                &creds,
                &crate::ads::AdVideoStatusRequest {
                    video_id: uploaded.id.clone(),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        upload.assert();
        status.assert();
        assert_eq!(uploaded.id, "9001");
        assert_eq!(
            status_out.video_status,
            crate::ads::AdVideoStatusKind::Processing
        );
    }

    #[tokio::test]
    async fn video_creative_posts_video_data_story_spec() {
        let server = MockServer::start();
        let creative = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/act_123/adcreatives")
                .body_contains("video_id")
                .body_contains("9001");
            then.status(200).json_body(json!({ "id": "501" }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let created = connector
            .create_video_ad_creative(
                &empty_app(),
                &token_creds("123"),
                &crate::ads::CreateVideoAdCreativeRequest {
                    account: None,
                    creative: crate::ads::VideoAdCreative {
                        name: "Hero video".into(),
                        page_id: "456".into(),
                        video_id: "9001".into(),
                        image_hash: "hash-1".into(),
                        message: "Watch".into(),
                        destination_url: "https://example.com/offer".into(),
                        call_to_action: LinkCallToAction::LearnMore,
                        geo_link: None,
                        application_id: None,
                        app_link: None,
                        instagram_user_id: None,
                        advantage_plus: false,
                        whatsapp_identity: None,
                    },
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        creative.assert();
        assert_eq!(created.id, "501");
    }

    #[tokio::test]
    async fn catalog_creative_posts_product_set_id_as_a_creative_field() {
        let server = MockServer::start();
        let creative = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/act_123/adcreatives")
                .body_contains("product_set_id=88")
                .body_contains("template_data");
            then.status(200).json_body(json!({ "id": "502" }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let created = connector
            .create_ad_creative(
                &empty_app(),
                &token_creds("123"),
                &crate::ads::CreateAdCreativeRequest {
                    account: None,
                    kind: crate::ads::AdCreativeKind::Catalog(crate::ads::CatalogAdCreative {
                        name: "Catalog".into(),
                        page_id: "456".into(),
                        product_set_id: "88".into(),
                        link: "https://example.com/shop".into(),
                        message: "Shop".into(),
                        call_to_action: LinkCallToAction::ShopNow,
                        instagram_user_id: None,
                        advantage_plus: false,
                        whatsapp_identity: None,
                    }),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        creative.assert();
        assert_eq!(created.id, "502");
    }

    #[tokio::test]
    async fn creative_preview_reads_one_closed_format_and_keeps_the_body_opaque() {
        let server = MockServer::start();
        let preview = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/500/previews")
                .query_param("ad_format", "MOBILE_FEED_STANDARD")
                .query_param("access_token", "tok");
            then.status(200)
                .json_body(json!({ "data": [{ "body": "<iframe src=\"https://meta.test/preview\"></iframe>" }] }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let response = connector
            .preview_ad_creative(
                &empty_app(),
                &token_creds("123"),
                &CreativePreviewRequest {
                    creative_id: "500".into(),
                    ad_format: AdPreviewFormat::MobileFeedStandard,
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        preview.assert();
        assert_eq!(response.creative_id, "500");
        assert_eq!(response.ad_format, AdPreviewFormat::MobileFeedStandard);
        assert!(response.body.contains("iframe"));

        let missing_body = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/501/previews")
                .query_param("ad_format", "DESKTOP_FEED_STANDARD");
            then.status(200).json_body(json!({ "data": [{}] }));
        });
        let err = connector
            .preview_ad_creative(
                &empty_app(),
                &token_creds("123"),
                &CreativePreviewRequest {
                    creative_id: "501".into(),
                    ad_format: AdPreviewFormat::DesktopFeedStandard,
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        missing_body.assert();
        assert!(
            matches!(err, Error::Platform { code, message, .. } if code == "missing_preview_body" && message == "creative preview returned no body")
        );
    }

    #[tokio::test]
    async fn review_status_reads_only_lifecycle_fields_and_preserves_meta_issues() {
        let server = MockServer::start();
        let status = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/700")
                .query_param(
                    "fields",
                    "id,name,configured_status,effective_status,issues_info",
                )
                .query_param("access_token", "tok");
            then.status(200).json_body(json!({
                "id": "700",
                "name": "Paused validation ad",
                "configured_status": "PAUSED",
                "effective_status": "PENDING_REVIEW",
                "issues_info": [{
                    "error_code": 100,
                    "error_summary": "Review pending",
                    "error_message": "Meta is reviewing this ad.",
                    "level": "WARNING"
                }]
            }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let response = connector
            .ad_review_status(
                &empty_app(),
                &token_creds("123"),
                &AdReviewStatusRequest {
                    entity: AdEntity::Ad,
                    id: "700".into(),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();

        status.assert();
        assert_eq!(response.configured_status, "PAUSED");
        assert_eq!(response.effective_status, "PENDING_REVIEW");
        assert!(response.is_pending_review());
        assert_eq!(response.issues.len(), 1);
        assert_eq!(response.issues[0].code.as_deref(), Some("100"));
        assert_eq!(
            response.issues[0].message.as_deref(),
            Some("Meta is reviewing this ad.")
        );

        let missing_status = server.mock(|when, then| {
            when.method(GET).path("/v26.0/701");
            then.status(200).json_body(json!({ "id": "701" }));
        });
        let err = connector
            .ad_review_status(
                &empty_app(),
                &token_creds("123"),
                &AdReviewStatusRequest {
                    entity: AdEntity::Campaign,
                    id: "701".into(),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        missing_status.assert();
        assert!(matches!(err, Error::Platform { code, .. } if code == "missing_configured_status"));
    }

    #[tokio::test]
    async fn ads_inventory_pages_sorts_by_id_and_omits_deleted_creatives() {
        let server = MockServer::start();
        let base = server.base_url();
        let next_base = base.clone();
        let second = server.mock(move |when, then| {
            when.method(GET)
                .path("/v26.0/act_123/campaigns")
                .query_param("after", "next");
            then.status(200).json_body(json!({
                "data": [{
                    "id": "100",
                    "name": "First",
                    "configured_status": "PAUSED",
                    "effective_status": "PAUSED",
                    "objective": "OUTCOME_TRAFFIC"
                }]
            }));
        });
        server.mock(move |when, then| {
            when.method(GET)
                .path("/v26.0/act_123/campaigns")
                .query_param(
                    "fields",
                    "id,name,configured_status,effective_status,objective",
                )
                .query_param("limit", "25")
                .query_param("effective_status", LIVE_EFFECTIVE_STATUS);
            then.status(200).json_body(json!({
                "data": [{
                    "id": "200",
                    "name": "Second",
                    "configured_status": "PAUSED",
                    "effective_status": "IN_PROCESS"
                }],
                "paging": { "next": format!("{next_base}/v26.0/act_123/campaigns?after=next") }
            }));
        });
        let connector = MetaAds::with_base(format!("{base}/v26.0")).unwrap();
        let reply = connector
            .list_ads_inventory(
                &empty_app(),
                &token_creds("act_123"),
                &AdsInventoryRequest {
                    account: None,
                    kind: AdsInventoryKind::Campaign,
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();

        second.assert();
        assert_eq!(reply.account_id, "act_123");
        assert_eq!(reply.kind, AdsInventoryKind::Campaign);
        assert_eq!(reply.items.len(), 2);
        assert_eq!(reply.items[0].id, "100");
        assert_eq!(reply.items[0].name.as_deref(), Some("First"));
        assert_eq!(reply.items[1].id, "200");

        let creatives = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/act_123/adcreatives")
                .query_param("fields", "id,name,status,object_type")
                .query_param("limit", "25");
            then.status(200).json_body(json!({
                "data": [
                    { "id": "9", "name": "Gone", "status": "DELETED", "object_type": "SHARE" },
                    { "id": "8", "name": "Hero", "status": "ACTIVE", "object_type": "SHARE" }
                ]
            }));
        });
        let creative_reply = connector
            .list_ads_inventory(
                &empty_app(),
                &token_creds("act_123"),
                &AdsInventoryRequest {
                    account: None,
                    kind: AdsInventoryKind::Creative,
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        creatives.assert();
        assert_eq!(creative_reply.items.len(), 1);
        assert_eq!(creative_reply.items[0].id, "8");
        assert_eq!(creative_reply.items[0].status.as_deref(), Some("ACTIVE"));
        assert!(creative_reply.items[0].configured_status.is_none());
    }

    #[tokio::test]
    async fn ads_inventory_paging_is_capped() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/act_123/adsets");
            let base = server.base_url();
            then.status(200).json_body(json!({
                "data": [],
                "paging": { "next": format!("{base}/v26.0/act_123/adsets?after=x") }
            }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let err = connector
            .list_ads_inventory(
                &empty_app(),
                &token_creds("act_123"),
                &AdsInventoryRequest {
                    account: None,
                    kind: AdsInventoryKind::Adset,
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Platform { ref code, .. } if code == "paging_exceeded"));
    }

    #[tokio::test]
    async fn ads_inventory_refuses_a_non_numeric_account_before_http() {
        let server = MockServer::start();
        let sink = server.mock(|when, then| {
            when.method(GET).path("/v26.0/act_nope/campaigns");
            then.status(200).json_body(json!({ "data": [] }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let err = connector
            .list_ads_inventory(
                &empty_app(),
                &token_creds("act_123"),
                &AdsInventoryRequest {
                    account: Some("nope".into()),
                    kind: AdsInventoryKind::Campaign,
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidQuery { reason, .. } if reason == "bad_ad_account:nope")
        );
        assert_eq!(sink.hits(), 0);
    }

    #[tokio::test]
    async fn ads_inspect_reads_budget_bid_targeting_page_and_destination() {
        let server = MockServer::start();
        let adset = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/456")
                .query_param(
                    "fields",
                    "id,name,campaign_id,configured_status,effective_status,daily_budget,lifetime_budget,bid_strategy,bid_amount,bid_constraints,targeting,promoted_object,destination_type",
                );
            then.status(200).json_body(json!({
                "id": "456",
                "name": "Paused set",
                "campaign_id": "100",
                "configured_status": "PAUSED",
                "effective_status": "PAUSED",
                "daily_budget": "500",
                "bid_strategy": "LOWEST_COST_WITHOUT_CAP",
                "bid_amount": 2,
                "destination_type": "WEBSITE",
                "promoted_object": { "page_id": "111" },
                "targeting": {
                    "geo_locations": { "countries": ["MY"] },
                    "age_min": 18,
                    "age_max": 65,
                    "publisher_platforms": ["facebook"],
                    "flexible_spec": [{ "interests": [{ "id": "1" }] }]
                }
            }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let reply = connector
            .inspect_ads_object(
                &empty_app(),
                &token_creds("act_123"),
                &AdsInspectRequest {
                    kind: AdsInventoryKind::Adset,
                    id: "456".into(),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        adset.assert();
        assert_eq!(reply.daily_budget.as_deref(), Some("500"));
        assert_eq!(
            reply.bid_strategy.as_deref(),
            Some("LOWEST_COST_WITHOUT_CAP")
        );
        assert_eq!(reply.page_id.as_deref(), Some("111"));
        assert_eq!(reply.destination_type.as_deref(), Some("WEBSITE"));
        let targeting = reply.targeting.expect("subset");
        assert_eq!(targeting.countries, ["MY"]);
        assert_eq!(targeting.age_min, Some(18));
        assert_eq!(targeting.publisher_platforms, ["facebook"]);

        let creative = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/789")
                .query_param(
                    "fields",
                    "id,name,status,object_story_spec,actor_id,object_url,link_url,call_to_action_type,product_set_id,instagram_user_id,wamo_whatsapp_identity_spec",
                );
            then.status(200).json_body(json!({
                "id": "789",
                "name": "Hero",
                "status": "ACTIVE",
                "object_story_spec": {
                    "page_id": "111",
                    "link_data": {
                        "link": "https://example.com/offer",
                        "call_to_action": { "type": "LEARN_MORE", "value": { "link": "https://example.com/offer" } }
                    }
                }
            }));
        });
        let creative_reply = connector
            .inspect_ads_object(
                &empty_app(),
                &token_creds("act_123"),
                &AdsInspectRequest {
                    kind: AdsInventoryKind::Creative,
                    id: "789".into(),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        creative.assert();
        assert_eq!(creative_reply.page_id.as_deref(), Some("111"));
        assert_eq!(
            creative_reply.destination.as_deref(),
            Some("https://example.com/offer")
        );
        assert!(creative_reply.daily_budget.is_none());
        assert_eq!(
            creative_reply.call_to_action_type.as_deref(),
            Some("LEARN_MORE")
        );
    }

    #[tokio::test]
    async fn ads_inspect_maps_catalog_min_roas_identity_and_non_link_destinations() {
        let server = MockServer::start();
        let adset = server.mock(|when, then| {
            when.method(GET).path("/v26.0/456");
            then.status(200).json_body(json!({
                "id": "456",
                "bid_strategy": "LOWEST_COST_WITH_MIN_ROAS",
                "bid_constraints": { "roas_average_floor": 15000 }
            }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let reply = connector
            .inspect_ads_object(
                &empty_app(),
                &token_creds("act_123"),
                &AdsInspectRequest {
                    kind: AdsInventoryKind::Adset,
                    id: "456".into(),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        adset.assert();
        assert_eq!(reply.roas_average_floor.as_deref(), Some("15000"));

        let catalog = server.mock(|when, then| {
            when.method(GET).path("/v26.0/800");
            then.status(200).json_body(json!({
                "id": "800",
                "status": "ACTIVE",
                "product_set_id": "555",
                "instagram_user_id": "222",
                "wamo_whatsapp_identity_spec": { "wamo_whatsapp_identity_id": "333" },
                "object_story_spec": {
                    "page_id": "111",
                    "template_data": { "name": "Catalog" }
                }
            }));
        });
        let catalog_reply = connector
            .inspect_ads_object(
                &empty_app(),
                &token_creds("act_123"),
                &AdsInspectRequest {
                    kind: AdsInventoryKind::Creative,
                    id: "800".into(),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        catalog.assert();
        assert_eq!(catalog_reply.product_set_id.as_deref(), Some("555"));
        assert_eq!(catalog_reply.destination.as_deref(), Some("555"));
        assert_eq!(catalog_reply.instagram_user_id.as_deref(), Some("222"));
        assert_eq!(catalog_reply.whatsapp_identity_id.as_deref(), Some("333"));

        let whatsapp = server.mock(|when, then| {
            when.method(GET).path("/v26.0/801");
            then.status(200).json_body(json!({
                "id": "801",
                "call_to_action_type": "WHATSAPP_MESSAGE",
                "object_story_spec": {
                    "page_id": "111",
                    "link_data": {
                        "call_to_action": {
                            "type": "WHATSAPP_MESSAGE",
                            "value": { "app_destination": "whatsapp" }
                        }
                    }
                }
            }));
        });
        let whatsapp_reply = connector
            .inspect_ads_object(
                &empty_app(),
                &token_creds("act_123"),
                &AdsInspectRequest {
                    kind: AdsInventoryKind::Creative,
                    id: "801".into(),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        whatsapp.assert();
        assert_eq!(whatsapp_reply.destination.as_deref(), Some("whatsapp"));
        assert_eq!(
            whatsapp_reply.call_to_action_type.as_deref(),
            Some("WHATSAPP_MESSAGE")
        );

        let page_cta = server.mock(|when, then| {
            when.method(GET).path("/v26.0/802");
            then.status(200).json_body(json!({
                "id": "802",
                "call_to_action_type": "LIKE_PAGE",
                "object_story_spec": {
                    "video_data": {
                        "call_to_action": {
                            "type": "LIKE_PAGE",
                            "value": { "page": "111" }
                        }
                    }
                }
            }));
        });
        let page_reply = connector
            .inspect_ads_object(
                &empty_app(),
                &token_creds("act_123"),
                &AdsInspectRequest {
                    kind: AdsInventoryKind::Creative,
                    id: "802".into(),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        page_cta.assert();
        assert_eq!(page_reply.destination.as_deref(), Some("111"));
    }

    #[tokio::test]
    async fn ads_inventory_creatives_do_not_send_effective_status() {
        let server = MockServer::start();
        let forbidden = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/act_123/adcreatives")
                .query_param_exists("effective_status");
            then.status(400).json_body(json!({
                "error": { "code": 100, "message": "effective_status is not a parameter" }
            }));
        });
        let ok = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/act_123/adcreatives")
                .query_param("fields", "id,name,status,object_type")
                .query_param("limit", "25");
            then.status(200).json_body(json!({
                "data": [{ "id": "8", "name": "Hero", "status": "ACTIVE" }]
            }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let reply = connector
            .list_ads_inventory(
                &empty_app(),
                &token_creds("act_123"),
                &AdsInventoryRequest {
                    account: None,
                    kind: AdsInventoryKind::Creative,
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        ok.assert();
        assert_eq!(forbidden.hits(), 0);
        assert_eq!(reply.items[0].id, "8");
    }

    #[tokio::test]
    async fn ads_status_update_posts_active_then_reads_review() {
        let server = MockServer::start();
        let post = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/456")
                .body_contains("status=ACTIVE");
            then.status(200).json_body(json!({ "success": true }));
        });
        let get = server.mock(|when, then| {
            when.method(GET).path("/v26.0/456").query_param(
                "fields",
                "id,name,configured_status,effective_status,issues_info",
            );
            then.status(200).json_body(json!({
                "id": "456",
                "name": "Paused set",
                "configured_status": "ACTIVE",
                "effective_status": "ACTIVE"
            }));
        });
        let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let status = connector
            .update_ad_status(
                &empty_app(),
                &token_creds("act_123"),
                &AdsStatusUpdateRequest {
                    entity: AdEntity::Adset,
                    id: "456".into(),
                    status: AdsConfiguredStatus::Active,
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        post.assert();
        get.assert();
        assert_eq!(status.configured_status, "ACTIVE");
        assert_eq!(status.effective_status, "ACTIVE");
    }

    #[tokio::test]
    async fn auth_finish_exchanges_and_resolves_ad_account() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/oauth/access_token");
            then.status(200)
                .json_body(json!({ "access_token": "SHORT" }));
        });
        server.mock(|when, then| {
            when.method(GET)
                .path("/oauth/access_token")
                .query_param("grant_type", "fb_exchange_token")
                .query_param("client_id", "id")
                .query_param("client_secret", "sec");
            then.status(200).json_body(json!({
                "access_token": "LONG",
                "token_type": "bearer",
                "expires_in": 5_184_000
            }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/me");
            then.status(200)
                .json_body(json!({ "id": "1000", "name": "Akmal" }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/me/adaccounts");
            then.status(200).json_body(json!({
                "data": [ { "account_id": "123", "name": "Main" } ]
            }));
        });
        let t = MetaAds::with_origins(format!("{}/v26.0", server.base_url()), server.base_url())
            .unwrap();
        let creds = t
            .auth_finish(
                &oauth_app(),
                AuthReply::Pasted {
                    code: "AQBx".into(),
                },
            )
            .await
            .unwrap();
        match creds {
            AccountCreds::OAuth2 {
                access_token,
                extra,
                ..
            } => {
                assert_eq!(access_token, "LONG");
                assert_eq!(extra.get("user_id").and_then(|v| v.as_str()), Some("1000"));
                assert_eq!(
                    extra.get("ad_account_id").and_then(|v| v.as_str()),
                    Some("act_123")
                );
                assert!(extra.get("expires_at").and_then(|v| v.as_u64()).is_some());
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn auth_finish_without_ad_account_fails_at_the_door() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/oauth/access_token");
            then.status(200).json_body(json!({ "access_token": "S" }));
        });
        server.mock(|when, then| {
            when.method(GET)
                .path("/oauth/access_token")
                .query_param("grant_type", "fb_exchange_token")
                .query_param("client_id", "id")
                .query_param("client_secret", "sec");
            then.status(200)
                .json_body(json!({ "access_token": "L", "expires_in": 100 }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/me");
            then.status(200)
                .json_body(json!({ "id": "1000", "name": "Akmal" }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/me/adaccounts");
            then.status(200).json_body(json!({ "data": [] }));
        });
        let t = MetaAds::with_origins(format!("{}/v26.0", server.base_url()), server.base_url())
            .unwrap();
        let err = t
            .auth_finish(
                &oauth_app(),
                AuthReply::Pasted {
                    code: "AQBx".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "no_ad_account"));
    }

    #[tokio::test]
    async fn insights_query_shape_and_row_mapping() {
        let server = MockServer::start();
        mock_account_currency(&server, "MYR");
        let insights = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/act_123/insights")
                .query_param("level", "campaign")
                .query_param("time_increment", "1")
                .query_param(
                    "time_range",
                    r#"{"since":"2026-06-01","until":"2026-06-02"}"#,
                )
                .query_param("action_attribution_windows", r#"["7d_click","1d_view"]"#)
                .query_param("fields", "actions,impressions,spend");
            then.status(200).json_body(json!({
                "data": [ {
                    "date_start": "2026-06-01",
                    "campaign_id": "238001",
                    "spend": "12.34",
                    "impressions": "4567",
                    "actions": [
                        { "action_type": "purchase", "value": "2" },
                        { "action_type": "landing_page_view", "value": "31" }
                    ]
                } ]
            }));
        });
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let reply = t
            .insights(
                &empty_app(),
                &token_creds("act_123"),
                &query(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        insights.assert();
        assert_eq!(reply.account_id, "act_123");
        assert_eq!(reply.currency.as_deref(), Some("MYR"));
        assert_eq!(reply.rows.len(), 1);
        let row = &reply.rows[0];
        assert_eq!(row.entity_id, "238001");
        assert_eq!(row.date_start, "2026-06-01");
        // Graph string numerics parse preserving int-ness; purchases sums
        // only the purchase-ish action rows
        assert_eq!(
            row.metrics.get("spend").and_then(|v| v.as_f64()),
            Some(12.34)
        );
        assert_eq!(
            row.metrics.get("impressions").and_then(|v| v.as_u64()),
            Some(4567)
        );
        assert_eq!(
            row.metrics.get("purchases").and_then(|v| v.as_u64()),
            Some(2)
        );
    }

    #[tokio::test]
    async fn insights_filters_breaks_down_and_derives_purchase_value_and_roas() {
        let server = MockServer::start();
        mock_account_currency(&server, "ILS");
        let insights = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/act_123/insights")
                .query_param("fields", "action_values,spend")
                .query_param(
                    "filtering",
                    r#"[{"field":"campaign.id","operator":"IN","value":["100","200"]}]"#,
                )
                .query_param("breakdowns", "country,publisher_platform");
            then.status(200).json_body(json!({
                "data": [ {
                    "date_start": "2026-06-01",
                    "campaign_id": "100",
                    "country": "IL",
                    "publisher_platform": "facebook",
                    "spend": "25.00",
                    "action_values": [
                        { "action_type": "purchase", "value": "100.00" },
                        { "action_type": "landing_page_view", "value": "999" }
                    ]
                } ]
            }));
        });
        let mut q = query();
        q.metrics = vec![Metric::PurchaseValue, Metric::Roas];
        q.entity_ids = vec!["200".into(), "100".into(), "100".into()];
        q.breakdowns = vec![
            crate::insights::Breakdown::Country,
            crate::insights::Breakdown::PublisherPlatform,
        ];
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let reply = t
            .insights(
                &empty_app(),
                &token_creds("act_123"),
                &q,
                Deadline::from_secs(30),
            )
            .await
            .unwrap();

        insights.assert();
        assert_eq!(reply.currency.as_deref(), Some("ILS"));
        let row = &reply.rows[0];
        assert_eq!(
            row.metrics.get("purchase_value").and_then(Value::as_f64),
            Some(100.0)
        );
        assert_eq!(row.metrics.get("roas").and_then(Value::as_f64), Some(4.0));
        assert_eq!(
            row.dimensions.get("country").and_then(Value::as_str),
            Some("IL")
        );
        assert_eq!(
            row.dimensions
                .get("publisher_platform")
                .and_then(Value::as_str),
            Some("facebook")
        );
    }

    #[test]
    fn roas_is_null_when_spend_is_zero_or_action_values_are_absent() {
        let mut q = query();
        q.metrics = vec![Metric::Roas];
        let zero_spend = row_from(
            &json!({
                "date_start": "2026-06-01",
                "campaign_id": "100",
                "spend": "0",
                "action_values": [{ "action_type": "purchase", "value": "100" }]
            }),
            &q,
        );
        assert!(zero_spend.metrics["roas"].is_null());
        let absent_values = row_from(
            &json!({
                "date_start": "2026-06-01",
                "campaign_id": "100",
                "spend": "10"
            }),
            &q,
        );
        assert!(absent_values.metrics["roas"].is_null());
    }

    #[tokio::test]
    async fn invalid_entity_filter_stops_before_http() {
        let server = MockServer::start();
        let sink = server.mock(|when, then| {
            when.method(GET).path("/v26.0/act_123/insights");
            then.status(200).json_body(json!({ "data": [] }));
        });
        let mut q = query();
        q.entity_ids = vec!["../not-an-id".into()];
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let err = t
            .insights(
                &empty_app(),
                &token_creds("act_123"),
                &q,
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidQuery { reason, .. } if reason == "bad_entity_id:../not-an-id")
        );
        assert_eq!(sink.hits(), 0);
    }

    #[tokio::test]
    async fn ad_accounts_pages_maps_metadata_and_sorts_by_canonical_id() {
        let server = MockServer::start();
        let base = server.base_url();
        let next_base = base.clone();
        let next_page = server.mock(move |when, then| {
            when.method(GET)
                .path("/v26.0/me/adaccounts")
                .query_param("after", "next");
            then.status(200).json_body(json!({
                "data": [{
                    "account_id": "123",
                    "name": "Primary",
                    "currency": "ILS",
                    "timezone_name": "Asia/Jerusalem",
                    "account_status": 1
                }]
            }));
        });
        server.mock(move |when, then| {
            when.method(GET).path("/v26.0/me/adaccounts").query_param(
                "fields",
                "account_id,name,currency,timezone_name,account_status",
            );
            then.status(200).json_body(json!({
                "data": [{ "account_id": "999", "name": "Secondary" }],
                "paging": { "next": format!("{next_base}/v26.0/me/adaccounts?after=next") }
            }));
        });
        let t = MetaAds::with_base(format!("{base}/v26.0")).unwrap();
        let reply = t
            .ad_accounts(
                &empty_app(),
                &token_creds("act_123"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();

        next_page.assert();
        assert_eq!(reply.site.as_str(), SITE);
        assert_eq!(reply.accounts.len(), 2);
        assert_eq!(reply.accounts[0].id, "act_123");
        assert_eq!(reply.accounts[0].name.as_deref(), Some("Primary"));
        assert_eq!(reply.accounts[0].currency.as_deref(), Some("ILS"));
        assert_eq!(
            reply.accounts[0].timezone.as_deref(),
            Some("Asia/Jerusalem")
        );
        assert_eq!(reply.accounts[0].status.as_deref(), Some("1"));
        assert_eq!(reply.accounts[1].id, "act_999");
    }

    #[tokio::test]
    async fn currency_failure_is_not_silently_reported_as_unknown() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/act_123/insights");
            then.status(200).json_body(json!({ "data": [] }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/act_123");
            then.status(500).body("");
        });
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let err = t
            .insights(
                &empty_app(),
                &token_creds("act_123"),
                &query(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Network { .. }));
    }

    #[tokio::test]
    async fn account_override_selects_the_act_id() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/act_999")
                .query_param("fields", "currency");
            then.status(200).json_body(json!({ "currency": "MYR" }));
        });
        let insights = server.mock(|when, then| {
            when.method(GET).path("/v26.0/act_999/insights");
            then.status(200).json_body(json!({ "data": [] }));
        });
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let mut q = query();
        q.account = Some("act_999".into());
        let reply = t
            .insights(
                &empty_app(),
                &token_creds("act_123"),
                &q,
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        insights.assert();
        assert_eq!(reply.account_id, "act_999");
        // empty data is an empty reply, not an error
        assert!(reply.rows.is_empty());
    }

    #[tokio::test]
    async fn bad_ad_account_is_invalid_query() {
        let t = MetaAds::new().unwrap();
        let mut q = query();
        q.account = Some("../escape".into());
        let err = t
            .insights(
                &empty_app(),
                &token_creds("act_123"),
                &q,
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidQuery { reason, .. } if reason == "bad_ad_account:../escape")
        );
    }

    #[tokio::test]
    async fn missing_ad_account_in_creds_is_auth() {
        let t = MetaAds::new().unwrap();
        let err = t
            .insights(
                &empty_app(),
                &AccountCreds::OAuth2 {
                    access_token: "tok".into(),
                    refresh_token: None,
                    extra: json!({}),
                },
                &query(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "no_ad_account"));
    }

    #[tokio::test]
    async fn insights_follows_paging_until_exhausted() {
        let server = MockServer::start();
        mock_account_currency(&server, "MYR");
        let base = server.base_url();
        let base2 = base.clone();
        // created before the fallback so the after-param page matches this
        // one; httpmock resolves multiple matches in creation order
        let page1 = server.mock(move |when, then| {
            when.method(GET)
                .path("/v26.0/act_123/insights")
                .query_param_exists("after");
            then.status(200).json_body(json!({
                "data": [ { "date_start": "2026-06-02", "campaign_id": "238001", "spend": "2.00" } ]
            }));
        });
        server.mock(move |when, then| {
            when.method(GET).path("/v26.0/act_123/insights");
            then.status(200).json_body(json!({
                "data": [ { "date_start": "2026-06-01", "campaign_id": "238001", "spend": "1.00" } ],
                "paging": { "cursors": { "after": "CUR" },
                            "next": format!("{base2}/v26.0/act_123/insights?after=CUR&level=campaign") }
            }));
        });
        let t = MetaAds::with_base(format!("{base}/v26.0")).unwrap();
        let reply = t
            .insights(
                &empty_app(),
                &token_creds("act_123"),
                &query(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        page1.assert();
        assert_eq!(reply.rows.len(), 2);
        // deterministic order regardless of page arrival
        assert_eq!(reply.rows[0].date_start, "2026-06-01");
        assert_eq!(reply.rows[1].date_start, "2026-06-02");
    }

    #[tokio::test]
    async fn insights_paging_is_capped() {
        // A self-referential paging.next (every page promises another)
        // must end in a loud Platform error, not an infinite loop.
        let server = MockServer::start();
        mock_account_currency(&server, "MYR");
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/act_123/insights");
            let base = server.base_url();
            then.status(200).json_body(json!({
                "data": [],
                "paging": { "next": format!("{base}/v26.0/act_123/insights?after=x") }
            }));
        });
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let err = t
            .insights(
                &empty_app(),
                &token_creds("act_123"),
                &query(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Platform { ref code, .. } if code == "paging_exceeded"));
    }

    #[tokio::test]
    async fn exhausted_deadline_surfaces_timeout_before_http() {
        let server = MockServer::start();
        let sink = server.mock(|when, then| {
            when.method(GET).path("/v26.0/act_123/insights");
            then.status(200).json_body(json!({ "data": [] }));
        });
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let err = t
            .insights(
                &empty_app(),
                &token_creds("act_123"),
                &query(),
                Deadline::from_secs(0),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::DeadlineExceeded { .. }));
        assert_eq!(sink.hits(), 0);
    }

    #[tokio::test]
    async fn code_190_is_token_expired() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/act_123/insights");
            then.status(400).json_body(json!({
                "error": { "code": 190, "message": "Error validating access token: Session has expired" }
            }));
        });
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let err = t
            .insights(
                &empty_app(),
                &token_creds("act_123"),
                &query(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "token_expired"));
    }

    #[tokio::test]
    async fn refresh_reissues_via_fb_exchange_token_preserving_state() {
        let server = MockServer::start();
        let m = server.mock(|when, then| {
            when.method(GET)
                .path("/oauth/access_token")
                .query_param("grant_type", "fb_exchange_token")
                .query_param("client_id", "id")
                .query_param("client_secret", "sec")
                .query_param("fb_exchange_token", "tok");
            then.status(200).json_body(json!({
                "access_token": "NEW", "expires_in": 5_184_000
            }));
        });
        let t = MetaAds::with_origins(format!("{}/v26.0", server.base_url()), server.base_url())
            .unwrap();
        let new = t
            .refresh(
                &oauth_app(),
                &token_creds("act_123"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        m.assert();
        match new {
            AccountCreds::OAuth2 {
                access_token,
                extra,
                ..
            } => {
                assert_eq!(access_token, "NEW");
                // refresh must not lose the ad account the auth flow stored
                assert_eq!(
                    extra.get("ad_account_id").and_then(|v| v.as_str()),
                    Some("act_123")
                );
            }
            other => panic!("{other:?}"),
        }
        // refresh requires the app config (client_secret); its absence is
        // a door error, not a mid-flight one
        let err = t
            .refresh(
                &empty_app(),
                &token_creds("act_123"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "missing_app_config"));
    }

    #[tokio::test]
    async fn whoami_maps_id_and_name() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/me");
            then.status(200)
                .json_body(json!({ "id": "1000", "name": "Akmal" }));
        });
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let me = t
            .whoami(&empty_app(), &token_creds("act_123"))
            .await
            .unwrap();
        assert_eq!(me.id, "1000");
        assert_eq!(me.handle.as_deref(), Some("Akmal"));
    }

    #[tokio::test]
    async fn publish_refuses_without_touching_the_network() {
        let t = MetaAds::new().unwrap();
        let intent = Intent {
            site: Site::new(SITE),
            params: json!({}),
            body: crate::types::Body::Text { text: "hi".into() },
            idempotency_key: None,
        };
        let err = t
            .publish(
                &empty_app(),
                &token_creds("act_123"),
                intent,
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidPost { reason, .. } if reason == "publish_unsupported")
        );
    }

    #[tokio::test]
    async fn insights_async_job_start_status_result_and_cancel() {
        let server = MockServer::start();
        mock_account_currency(&server, "MYR");
        let start = server.mock(|when, then| {
            when.method(POST).path("/v26.0/act_123/insights");
            then.status(200)
                .json_body(json!({ "report_run_id": "999" }));
        });
        let status = server.mock(|when, then| {
            when.method(GET).path("/v26.0/999");
            then.status(200).json_body(json!({
                "async_status": "Job Completed",
                "async_percent_completion": 100
            }));
        });
        let result = server.mock(|when, then| {
            when.method(GET).path("/v26.0/999/insights");
            then.status(200).json_body(json!({
                "data": [{
                    "date_start": "2026-06-01",
                    "campaign_id": "1",
                    "spend": "1.00"
                }]
            }));
        });
        let cancel = server.mock(|when, then| {
            when.method(DELETE).path("/v26.0/999");
            then.status(200).body("");
        });
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let q = query();
        let job = t
            .start_insights_job(
                &empty_app(),
                &token_creds("123"),
                &q,
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(job.id, "999");
        let st = t
            .insights_job(
                &empty_app(),
                &token_creds("123"),
                "999",
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(st.status, InsightsJobStatus::Completed);
        let reply = t
            .insights_job_result(
                &empty_app(),
                &token_creds("123"),
                "999",
                &q,
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(reply.rows.len(), 1);
        t.cancel_insights_job(
            &empty_app(),
            &token_creds("123"),
            "999",
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
        start.assert();
        status.assert();
        result.assert();
        cancel.assert();
        let bad = t
            .insights_job(
                &empty_app(),
                &token_creds("123"),
                "not-a-job",
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(bad, Error::InvalidQuery { reason, .. } if reason == "bad_insights_job_id")
        );
    }

    #[test]
    fn extra_metrics_map_graph_fields_and_definitions() {
        assert_eq!(meta_field(Metric::Frequency), Some("frequency"));
        assert_eq!(
            meta_field(Metric::VideoThruplay),
            Some("video_thruplay_watched_actions")
        );
        assert_eq!(meta_field(Metric::QualityRanking), Some("quality_ranking"));
        let row = row_from(
            &json!({
                "campaign_id": "1",
                "date_start": "2026-06-01",
                "frequency": "1.4",
                "unique_clicks": "9",
                "inline_link_clicks": "4",
                "inline_link_click_ctr": "0.02",
                "quality_ranking": "ABOVE_AVERAGE",
                "video_thruplay_watched_actions": [{ "action_type": "video_view", "value": "3" }]
            }),
            &InsightsQuery {
                level: InsightsLevel::Campaign,
                metrics: vec![
                    Metric::Frequency,
                    Metric::UniqueClicks,
                    Metric::InlineLinkClicks,
                    Metric::InlineLinkClickCtr,
                    Metric::QualityRanking,
                    Metric::VideoThruplay,
                ],
                range: crate::insights::DateRange {
                    from: "2026-06-01".into(),
                    to: "2026-06-01".into(),
                },
                attribution: AttributionWindow::OneDayClick,
                account: None,
                entity_ids: vec![],
                breakdowns: vec![],
                report: crate::insights::InsightsReportKind::Performance,
            },
        );
        assert_eq!(row.metrics["frequency"], json!(1.4));
        assert_eq!(row.metrics["unique_clicks"], json!(9));
        assert_eq!(row.metrics["quality_ranking"], json!("ABOVE_AVERAGE"));
        assert_eq!(row.metrics["video_thruplay"], json!(3));
    }

    #[test]
    fn level_id_fields() {
        assert_eq!(InsightsLevel::Account.id_field(), "account_id");
        assert_eq!(InsightsLevel::Campaign.id_field(), "campaign_id");
        assert_eq!(InsightsLevel::Adset.id_field(), "adset_id");
        assert_eq!(InsightsLevel::Ad.id_field(), "ad_id");
    }

    #[test]
    fn access_tier_header_maps_limited_and_full() {
        assert_eq!(
            MarketingApiAccessTierKind::from_header("standard_access"),
            MarketingApiAccessTierKind::Full
        );
        assert_eq!(
            MarketingApiAccessTierKind::from_header("development_access"),
            MarketingApiAccessTierKind::Limited
        );
        assert_eq!(
            MarketingApiAccessTierKind::from_header("limited_access"),
            MarketingApiAccessTierKind::Limited
        );
        assert_eq!(
            MarketingApiAccessTierKind::from_header("nope"),
            MarketingApiAccessTierKind::Unknown
        );
    }

    #[test]
    fn system_user_debug_refuses_a_user_token() {
        let site = Site::new(SITE);
        let err =
            refuse_non_system_user_debug(&site, Some("USER"), Some(1_800_000_000)).unwrap_err();
        assert!(
            matches!(err, Error::Auth { reason, .. } if reason == "user_token_not_system_user")
        );
        assert!(refuse_non_system_user_debug(&site, Some("USER"), None).is_ok());
        assert!(refuse_non_system_user_debug(&site, Some("SYSTEM_USER"), None).is_ok());
        assert!(refuse_non_system_user_debug(&site, None, None).is_ok());
        let err = refuse_non_system_user_debug(&site, Some("PAGE"), None).unwrap_err();
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "wrong_token_type:PAGE"));
    }

    #[test]
    fn access_tier_headers_require_json() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-fb-ads-insights-throttle",
            "not-json ads_api_access_tier".parse().unwrap(),
        );
        assert_eq!(access_tier_from_headers(&headers), None);
        headers.insert(
            "x-fb-ads-insights-throttle",
            r#"{"ads_api_access_tier":"development_access"}"#.parse().unwrap(),
        );
        assert_eq!(
            access_tier_from_headers(&headers).as_deref(),
            Some("development_access")
        );
    }

    #[tokio::test]
    async fn system_user_bootstrap_stores_kind_and_ad_account() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/debug_token");
            then.status(200).json_body(json!({
                "data": {
                    "app_id": "id",
                    "type": "SYSTEM_USER",
                    "is_valid": true,
                    "expires_at": 0,
                    "scopes": ["ads_management", "ads_read"],
                    "user_id": "55"
                }
            }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/me");
            then.status(200)
                .json_body(json!({ "id": "55", "name": "Postkit Bot" }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/me/adaccounts");
            then.status(200).json_body(json!({
                "data": [{ "account_id": "123", "name": "Test", "currency": "MYR" }]
            }));
        });
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let creds = t
            .bootstrap_system_user_token(&oauth_app(), "SYS", Deadline::from_secs(30))
            .await
            .unwrap();
        match &creds {
            AccountCreds::OAuth2 {
                access_token,
                extra,
                ..
            } => {
                assert_eq!(access_token, "SYS");
                assert_eq!(
                    extra.get("token_kind").and_then(|v| v.as_str()),
                    Some(SYSTEM_USER_TOKEN_KIND)
                );
                assert_eq!(extra.get("user_id").and_then(|v| v.as_str()), Some("55"));
                assert_eq!(
                    extra.get("ad_account_id").and_then(|v| v.as_str()),
                    Some("act_123")
                );
            }
            other => panic!("{other:?}"),
        }
        let err = t
            .refresh(&oauth_app(), &creds, Deadline::from_secs(30))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "system_user_no_refresh"));
        assert!(!crate::refresh_is_due(&creds));
    }

    #[tokio::test]
    async fn system_user_bootstrap_rejects_a_user_oauth_token() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/debug_token");
            then.status(200).json_body(json!({
                "data": {
                    "type": "USER",
                    "is_valid": true,
                    "expires_at": 1_800_000_000,
                    "user_id": "1"
                }
            }));
        });
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let err = t
            .bootstrap_system_user_token(&oauth_app(), "EAA", Deadline::from_secs(30))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Auth { reason, .. } if reason == "user_token_not_system_user")
        );
    }

    #[tokio::test]
    async fn system_user_bootstrap_accepts_never_expiring_user_type() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/debug_token");
            then.status(200).json_body(json!({
                "data": {
                    "app_id": "id",
                    "type": "USER",
                    "is_valid": true,
                    "expires_at": 0,
                    "user_id": 55
                }
            }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/me");
            then.status(200)
                .json_body(json!({ "id": "55", "name": "Bot" }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/me/adaccounts");
            then.status(200)
                .json_body(json!({ "data": [{ "account_id": "9" }] }));
        });
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let creds = t
            .bootstrap_system_user_token(&oauth_app(), "SYS", Deadline::from_secs(30))
            .await
            .unwrap();
        match &creds {
            AccountCreds::OAuth2 { extra, .. } => {
                assert_eq!(
                    extra.get("token_kind").and_then(|v| v.as_str()),
                    Some(SYSTEM_USER_TOKEN_KIND)
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn inspect_token_omits_the_secret_and_maps_debug_fields() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/debug_token");
            then.status(200).json_body(json!({
                "data": {
                    "app_id": "id",
                    "application": "Postkit",
                    "type": "SYSTEM_USER",
                    "is_valid": true,
                    "expires_at": 0,
                    "data_access_expires_at": 1_800_000_000,
                    "scopes": ["ads_read"],
                    "user_id": "55"
                }
            }));
        });
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let creds = AccountCreds::OAuth2 {
            access_token: "secret-token-value".into(),
            refresh_token: None,
            extra: json!({ "token_kind": SYSTEM_USER_TOKEN_KIND, "ad_account_id": "act_123" }),
        };
        let inspection = t
            .inspect_access_token(&oauth_app(), &creds, Deadline::from_secs(30))
            .await
            .unwrap();
        let encoded = serde_json::to_string(&inspection).unwrap();
        assert!(!encoded.contains("secret-token-value"));
        assert!(!encoded.contains("sec"));
        assert_eq!(inspection.token_kind, AdsTokenKind::SystemUser);
        assert_eq!(inspection.debug_type.as_deref(), Some("SYSTEM_USER"));
        assert!(inspection.is_valid);
        assert_eq!(inspection.expires_at, None);
        assert_eq!(inspection.scopes, vec!["ads_read"]);
    }

    #[tokio::test]
    async fn access_tier_reads_ads_api_access_tier_header() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/me/adaccounts");
            then.status(200)
                .header(
                    "X-FB-Ads-Insights-Throttle",
                    r#"{"app_id_util_pct":1,"ads_api_access_tier":"standard_access"}"#,
                )
                .json_body(json!({ "data": [{ "account_id": "123" }] }));
        });
        let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let reply = t
            .marketing_api_access_tier(&oauth_app(), &token_creds("123"), Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(reply.tier, MarketingApiAccessTierKind::Full);
        assert_eq!(reply.raw.as_deref(), Some("standard_access"));
        assert_eq!(reply.source, "response_header");
        assert!(reply.dashboard.contains("Marketing API Access Tier"));
    }
}
