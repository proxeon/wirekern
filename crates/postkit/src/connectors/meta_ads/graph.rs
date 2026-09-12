//! Shared Graph HTTP helpers for the Meta Ads connector.
use crate::error::Error;
use crate::form::form;
use crate::http::Http;
use crate::types::{AccountCreds, Deadline, Site};
use serde_json::Value;

use super::SITE;

/// Daily rows over a ≤90-day range fit in one Graph page; this cap exists
/// so a runaway cursor loop fails loudly instead of paging forever.
pub(super) const MAX_PAGES: usize = 50;

pub(super) fn value_string(value: Option<&Value>) -> Option<String> {
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
pub(super) fn nonempty_value_string(value: Option<&Value>) -> Option<String> {
    value_string(value).filter(|value| !value.trim().is_empty())
}

pub(super) fn access_token(creds: &AccountCreds) -> Result<&str, Error> {
    match creds {
        AccountCreds::OAuth2 { access_token, .. } => Ok(access_token),
        _ => Err(Error::Auth {
            site: Site::new(SITE),
            reason: "wrong_cred_kind".into(),
        }),
    }
}

pub(super) fn extra_string(creds: &AccountCreds, key: &str) -> Option<String> {
    let AccountCreds::OAuth2 { extra, .. } = creds else {
        return None;
    };
    extra.get(key).and_then(|v| v.as_str()).map(String::from)
}

pub(super) fn account_id(creds: &AccountCreds, override_: Option<&str>) -> Result<String, Error> {
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

pub(super) async fn read_json(resp: reqwest::Response, site: &Site) -> Result<Value, Error> {
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
pub(super) fn map_graph_error(http_status: u16, body: &str) -> Error {
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
    // 10/200 are permission denials: not refreshable. 368 is Graph
    // "temporarily blocked" — wait, same family as rate_limited.
    if matches!(code, 10 | 200) {
        return Error::Auth {
            site,
            reason: "permission".into(),
        };
    }
    let (auth_hit, rate_hit) = if code != 0 {
        (
            matches!(code, 190 | 102),
            matches!(code, 4 | 17 | 32 | 341 | 368 | 613 | 80004),
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

pub(super) async fn read_ad_json_field(
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
