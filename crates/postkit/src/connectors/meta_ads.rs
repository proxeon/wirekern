//! Meta Ads connector — Tier A: read-only insights (026 §5).
//!
//! One-way surface: `read.metrics` only. No verb in this module can spend.
//! Management (paused-first creates) and activation (policy-gated) are
//! Tier B/C and deliberately absent. Auth reuses the Threads paste-code
//! machinery against the Facebook OAuth host; the long-lived exchange is
//! Meta's `fb_exchange_token` grant (~60 days).

use crate::error::Error;
use crate::form::form;
use crate::http::Http;
use crate::insights::{AttributionWindow, InsightRow, InsightsQuery, InsightsReply, Metric};
use crate::oauth::{authorize_url, exchange_code, extract_code, new_state};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
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
/// Tier A scope. `ads_management` joins only with Tier B (026 §5).
pub const SCOPES: &str = "ads_read";

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
}

#[async_trait]
impl Publisher for MetaAds {
    fn site(&self) -> &Site {
        &self.site
    }

    fn capabilities(&self) -> &[Capability] {
        &[Capability::ReadMetrics]
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
    async fn refresh(&self, app: &AppConfig, creds: &AccountCreds) -> Result<AccountCreds, Error> {
        let oauth = require_oauth(app)?;
        let token = access_token(creds)?;
        let user_id = extra_string(creds, "user_id");
        let account = extra_string(creds, "ad_account_id");
        let deadline = Deadline::from_secs(30);
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

    async fn insights(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        query: &InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, query.account.as_deref())?;
        let mut fields: Vec<&str> = query
            .metrics
            .iter()
            .copied()
            .filter_map(meta_field)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        // purchases is derived from the actions breakdown
        if query.metrics.contains(&Metric::Purchases) {
            fields.push("actions");
        }
        let range = format!(
            "{{\"since\":\"{}\",\"until\":\"{}\"}}",
            query.range.from, query.range.to
        );
        let params = form(&[
            ("level", query.level.as_str()),
            ("fields", &fields.join(",")),
            ("time_range", &range),
            ("time_increment", "1"),
            (
                "action_attribution_windows",
                attribution_param(query.attribution),
            ),
            ("access_token", token),
        ]);
        let mut rows: Vec<InsightRow> = Vec::new();
        let mut next = Some(format!("{}/act_{}/insights?{}", self.base, account, params));
        let mut pages = 0usize;
        while let Some(url) = next {
            deadline.check(&self.site)?;
            pages += 1;
            if pages > MAX_PAGES {
                return Err(Error::Platform {
                    site: self.site.clone(),
                    code: "paging_exceeded".into(),
                    message: format!("insights paging exceeded {MAX_PAGES} pages"),
                });
            }
            let resp = self
                .http
                .send(self.http.get(&url), deadline, &self.site)
                .await?;
            let body = read_json(resp, &self.site).await?;
            if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
                for item in data {
                    rows.push(row_from(item, query));
                }
            }
            next = body
                .get("paging")
                .and_then(|p| p.get("next"))
                .and_then(|n| n.as_str())
                .map(str::to_string);
        }
        // Deterministic reply bytes: same query always yields rows in the
        // same order regardless of how Graph paginated them.
        rows.sort_by(|a, b| (&a.entity_id, &a.date_start).cmp(&(&b.entity_id, &b.date_start)));
        let currency = account_currency(&self.http, &self.base, &account, token, deadline)
            .await
            .unwrap_or(None);
        Ok(InsightsReply {
            site: self.site.clone(),
            account_id: format!("act_{account}"),
            currency,
            rows,
        })
    }
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
    }
}

/// Graph's `action_attribution_windows` wants an array of atomic windows
/// (`["7d_click","1d_view"]`); the combined `7d_click_1d_view` is only the
/// Ads Manager display name for that preset and is rejected with code 100.
fn attribution_param(a: AttributionWindow) -> &'static str {
    match a {
        AttributionWindow::SevenDayClickOneDayView => r#"["7d_click","1d_view"]"#,
        AttributionWindow::OneDayClick => r#"["1d_click"]"#,
        AttributionWindow::OneDayView => r#"["1d_view"]"#,
    }
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
fn purchases_of(item: &Value) -> Value {
    let mut total: u64 = 0;
    if let Some(actions) = item.get("actions").and_then(|a| a.as_array()) {
        for a in actions {
            let kind = a.get("action_type").and_then(|t| t.as_str()).unwrap_or("");
            if kind == "purchase" || kind == "offsite_conversion.fb_pixel_purchase" {
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
        let value = if matches!(m, Metric::Purchases) {
            purchases_of(item)
        } else {
            number(item.get(m.as_str()).unwrap_or(&Value::Null)).unwrap_or(Value::Null)
        };
        metrics.insert(m.as_str().into(), value);
    }
    InsightRow {
        entity_id,
        level: query.level,
        date_start: item
            .get("date_start")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string(),
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

/// First ad account visible to the token, as `act_<account_id>`.
async fn first_ad_account(
    http: &Http,
    base: &str,
    token: &str,
    deadline: Deadline,
) -> Result<Option<String>, Error> {
    let site = Site::new(SITE);
    let q = form(&[
        ("fields", "account_id"),
        ("limit", "100"),
        ("access_token", token),
    ]);
    let url = format!("{base}/me/adaccounts?{q}");
    let resp = http.send(http.get(&url), deadline, &site).await?;
    let body = read_json(resp, &site).await?;
    Ok(body
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())
        .and_then(|acct| acct.get("account_id"))
        .and_then(|v| v.as_str())
        .map(|id| format!("act_{id}")))
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
    let message = err
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .or_else(|| v.get("error_message").and_then(|m| m.as_str()))
        .unwrap_or(body);
    let lower = message.to_ascii_lowercase();
    let (auth_hit, rate_hit) = if code != 0 {
        (code == 190, matches!(code, 4 | 17 | 32 | 613))
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
        return Error::Auth {
            site,
            reason: "token_expired".into(),
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
    use crate::insights::{AttributionWindow, InsightsLevel};
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
        for code in [4, 17, 32, 613] {
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

    #[tokio::test]
    async fn auth_start_url_shape() {
        let t = MetaAds::new().unwrap();
        match t.auth_start(&oauth_app()).await.unwrap() {
            AuthStart::Browser {
                authorize_url,
                state,
            } => {
                assert!(authorize_url.starts_with("https://www.facebook.com/dialog/oauth?"));
                assert!(authorize_url.contains("scope=ads_read"));
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
                .query_param("fields", "impressions,spend,actions");
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
    async fn account_override_selects_the_act_id() {
        let server = MockServer::start();
        mock_account_currency(&server, "MYR");
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
            .refresh(&oauth_app(), &token_creds("act_123"))
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
            .refresh(&empty_app(), &token_creds("act_123"))
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

    #[test]
    fn level_id_fields() {
        assert_eq!(InsightsLevel::Account.id_field(), "account_id");
        assert_eq!(InsightsLevel::Campaign.id_field(), "campaign_id");
        assert_eq!(InsightsLevel::Adset.id_field(), "adset_id");
        assert_eq!(InsightsLevel::Ad.id_field(), "ad_id");
    }
}
