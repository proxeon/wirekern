//! OAuth, token debug, System User, and Marketing API access-tier.
use crate::ads::{
    AdsTokenInspection, AdsTokenKind, MarketingApiAccessTier, MarketingApiAccessTierKind,
    MARKETING_API_ACCESS_TIER_DASHBOARD, SYSTEM_USER_TOKEN_KIND,
};
use crate::error::Error;
use crate::form::form;
use crate::http::Http;
use crate::types::{AccountCreds, AppConfig, Deadline, OAuthApp, Site, WhoAmI};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

use super::graph::{read_json, value_string};
use super::SITE;

pub(super) fn require_oauth(app: &AppConfig) -> Result<&OAuthApp, Error> {
    app.oauth.as_ref().ok_or_else(|| Error::Auth {
        site: Site::new(SITE),
        reason: "missing_app_config".into(),
    })
}

pub(super) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Meta's long-lived exchange: `fb_exchange_token` grant, ~60-day token.
/// The grant requires both `client_id` and `client_secret` — Graph answers
/// `101: Missing client_id parameter` otherwise.
pub(super) async fn long_lived(
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

pub(super) struct TokenLong {
    pub(super) access_token: String,
    pub(super) expires_in: Option<u64>,
}

pub(super) fn creds_from_long(long: &TokenLong, user_id: Option<String>) -> AccountCreds {
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

pub(super) async fn whoami(
    http: &Http,
    base: &str,
    token: &str,
    deadline: Deadline,
) -> Result<WhoAmI, Error> {
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

pub(super) fn app_access_token(oauth: &OAuthApp) -> String {
    format!("{}|{}", oauth.client_id, oauth.client_secret)
}

/// System User tokens are often labelled `USER` by `/debug_token` (the
/// official field table does not even document `type`). A person OAuth token
/// still has a non-zero `expires_at` (~60 days). Never-expiring USER tokens
/// are accepted; PAGE/APP tokens are not a Marketing API system user.
pub(super) fn refuse_non_system_user_debug(
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

pub(super) fn system_user_creds(
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
pub(super) async fn debug_token(
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

pub(super) fn unix_field(data: &Value, key: &str) -> Option<u64> {
    let value = data.get(key)?;
    let n = value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))?;
    (n != 0).then_some(n)
}

pub(super) fn access_tier_from_headers(headers: &reqwest::header::HeaderMap) -> Option<String> {
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

pub(super) fn json_ads_api_access_tier(value: &Value) -> Option<String> {
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

pub(super) async fn marketing_api_access_tier(
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
