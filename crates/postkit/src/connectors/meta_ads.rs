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
use crate::insights::{
    AdAccount, AdAccountsReply, AttributionWindow, InsightRow, InsightsLevel, InsightsQuery,
    InsightsReply, Metric,
};
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
        &[Capability::ReadMetrics, Capability::ReadAdAccounts]
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
        let mut fields: std::collections::BTreeSet<&str> = query
            .metrics
            .iter()
            .copied()
            .filter_map(meta_field)
            .collect();
        // Purchases and purchase value are reductions of Meta's action
        // arrays; ROAS needs both a purchase value and spend even if the
        // operator requested only the derived metric.
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
            ("fields", &field_list),
            ("time_range", &range),
            ("time_increment", "1"),
            (
                "action_attribution_windows",
                attribution_param(query.attribution),
            ),
            ("access_token", token),
        ];
        // Graph accepts these as JSON / comma-separated data parameters. The
        // values come from typed query fields and `form` percent-encodes them;
        // no caller input is interpolated into a URL expression.
        if let Some(filter) = filter.as_deref() {
            pairs.push(("filtering", filter));
        }
        if !breakdowns.is_empty() {
            pairs.push(("breakdowns", &breakdowns));
        }
        let params = form(&pairs);
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
        // A money report without a known currency is ambiguous. The previous
        // best-effort lookup hid a failed account request as `currency: null`.
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
            entity_ids: vec![],
            breakdowns: vec![],
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
