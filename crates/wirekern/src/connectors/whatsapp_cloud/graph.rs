//! Shared Graph HTTP, paging, token, and ID helpers.
use crate::error::Error;
use crate::types::{AccountCreds, AppConfig, Site};
use crate::whatsapp::WhatsAppPageQuery;
use serde_json::Value;

use super::SITE;

pub(super) fn extra_digits(app: &AppConfig, key: &str) -> Option<String> {
    app.extra
        .get(key)
        .and_then(value_string)
        .filter(|id| !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_digit()))
}

/// Graph returns the next page cursor under `paging.cursors.after`. Expose
/// only that opaque continuation token, never the `paging.next` URL, which
/// is an implementation detail that can carry unrelated query parameters.
pub(super) fn graph_after(body: &Value) -> Option<String> {
    body.pointer("/paging/cursors/after")
        .and_then(value_string)
        .filter(|cursor| !cursor.is_empty() && cursor.len() <= 1_024)
}

pub(super) fn page_query_error(query: &WhatsAppPageQuery, site: &Site) -> Result<(), Error> {
    query.validate().map_err(|reason| Error::InvalidQuery {
        site: site.clone(),
        reason,
    })
}

/// Append one bounded opaque page query to an existing Graph edge URL. Both
/// values are percent encoded; cursors are data, never fragments of a URL.
pub(super) fn graph_page_url(mut url: String, query: &WhatsAppPageQuery) -> String {
    if let Some(limit) = query.limit {
        url.push_str(&format!("&limit={limit}"));
    }
    if let Some(after) = &query.after {
        url.push_str("&after=");
        url.push_str(&percent_encode(after));
    }
    url
}

/// Graph query values can contain cursor punctuation. Encode them locally
/// rather than trusting a caller-provided cursor/name to stay inside one
/// parameter (the `fields` portion above is Wirekern-owned static text).
pub(super) fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub(super) fn waba_id(app: &AppConfig) -> Result<String, Error> {
    app.extra
        .get("waba_id")
        .and_then(value_string)
        .filter(|id| {
            !id.is_empty() && id.len() <= 32 && id.bytes().all(|byte| byte.is_ascii_digit())
        })
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "missing_waba_id".into(),
        })
}

pub(super) fn validate_graph_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 32 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err("template_id_invalid".into());
    }
    Ok(())
}

pub(super) fn configured_phone_ids(app: &AppConfig) -> Result<Vec<String>, Error> {
    let mut ids = Vec::new();
    if let Ok(primary) = phone_number_id(app) {
        ids.push(primary);
    }
    if let Some(senders) = app.extra.get("senders").and_then(Value::as_array) {
        for sender in senders {
            if let Some(id) = sender
                .get("phone_number_id")
                .and_then(value_string)
                .filter(|id| {
                    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_digit())
                })
            {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    if ids.is_empty() {
        return Err(Error::Auth {
            site: Site::new(SITE),
            reason: "missing_phone_number_id".into(),
        });
    }
    Ok(ids)
}

pub(super) fn phone_number_id(app: &AppConfig) -> Result<String, Error> {
    app.extra
        .get("phone_number_id")
        .and_then(value_string)
        .filter(|id| {
            !id.is_empty() && id.len() <= 32 && id.bytes().all(|byte| byte.is_ascii_digit())
        })
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "missing_phone_number_id".into(),
        })
}

pub(super) fn access_token(creds: &AccountCreds) -> Result<&str, Error> {
    match creds {
        AccountCreds::BotToken { token } if !token.is_empty() => Ok(token),
        _ => Err(Error::Auth {
            site: Site::new(SITE),
            reason: "credential_kind".into(),
        }),
    }
}

pub(super) fn value_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|number| number.to_string()))
        .or_else(|| value.as_i64().map(|number| number.to_string()))
}

pub(super) async fn read_json(response: reqwest::Response, site: &Site) -> Result<Value, Error> {
    let status = response.status();
    let text = response
        .text()
        .await
        // A reqwest body diagnostic can carry credential-bearing request
        // details, so its content never enters a public Wirekern error.
        .map_err(|_| Error::request_failed(site))?;
    if !status.is_success() {
        return Err(map_graph_error(status.as_u16(), &text));
    }
    let value: Value = serde_json::from_str(&text).map_err(|_| Error::Platform {
        site: site.clone(),
        code: "bad_json".into(),
        message: "WhatsApp returned invalid JSON".into(),
    })?;
    if value.get("error").is_some() {
        return Err(map_graph_error(status.as_u16(), &text));
    }
    Ok(value)
}

pub(super) fn map_graph_error(status: u16, body: &str) -> Error {
    let site = Site::new(SITE);
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let error = value.get("error");
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    // Never fall back to an arbitrary raw body: reverse proxies can echo
    // Authorization headers or operator input. Meta's structured message is
    // the only useful and bounded diagnostics channel.
    let message = error
        .and_then(|error| error.get("error_user_msg"))
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .or_else(|| {
            error
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .unwrap_or("WhatsApp request failed");
    if code == 190 || status == 401 {
        return Error::Auth {
            site,
            reason: "token_invalid".into(),
        };
    }
    if matches!(code, 4 | 17 | 32 | 613) || status == 429 {
        return Error::RateLimited {
            site,
            retry_after: None,
        };
    }
    if status >= 500 && code == 0 {
        return Error::Network {
            site,
            message: format!("http_{status}"),
        };
    }
    Error::Platform {
        site,
        code: code.to_string(),
        message: message.to_string(),
    }
}
