use crate::error::Error;
use crate::http::Http;
use crate::types::{Deadline, Site};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct TokenResponse {
    pub access_token: String,
    pub user_id: Option<String>,
    pub expires_in: Option<u64>,
    pub raw: Value,
}

/// RFC 6749 authorize URL. `scope` is already joined. No Graph knowledge.
pub fn authorize_url(
    authorize_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    state: &str,
) -> String {
    let base = authorize_endpoint.trim_end_matches('/');
    let q = form(&[
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", scope),
        ("response_type", "code"),
        ("state", state),
    ]);
    if base.contains('?') {
        format!("{base}&{q}")
    } else {
        format!("{base}?{q}")
    }
}

/// Pull `code` from a pasted redirect URL or a raw code. Strips Meta `#_`.
pub fn extract_code(pasted: &str) -> Result<String, Error> {
    let s = pasted.trim();
    if s.is_empty() {
        return Err(Error::Auth {
            site: Site::new(""),
            reason: "missing_code".into(),
        });
    }
    let looks_url = s.contains("://") || s.starts_with("http");
    let code = if looks_url {
        let without_frag = s.split('#').next().unwrap_or(s);
        query_param(without_frag, "code").ok_or_else(|| Error::Auth {
            site: Site::new(""),
            reason: "missing_code".into(),
        })?
    } else {
        s.to_string()
    };
    let code = code.trim().trim_end_matches("#_").trim().to_string();
    if code.is_empty() {
        return Err(Error::Auth {
            site: Site::new(""),
            reason: "missing_code".into(),
        });
    }
    Ok(code)
}

pub fn query_param(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    for part in q.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            if k == key {
                return Some(form_decode(v));
            }
        }
    }
    None
}

pub async fn exchange_code(
    http: &Http,
    token_endpoint: &str,
    client_id: &str,
    client_secret: &str,
    redirect_uri: &str,
    code: &str,
    deadline: Deadline,
    site: &Site,
) -> Result<TokenResponse, Error> {
    let req = http
        .post(token_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri),
            ("code", code),
        ]));
    let resp = http.send(req, deadline, site).await?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| Error::Network {
        site: site.clone(),
        message: e.to_string(),
    })?;
    let raw: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !status.is_success() || raw.get("error").is_some() {
        let msg = raw
            .get("error_message")
            .or_else(|| raw.get("error").and_then(|e| e.get("message")))
            .and_then(|v| v.as_str())
            .unwrap_or(text.as_str());
        return Err(Error::Auth {
            site: site.clone(),
            reason: msg.to_string(),
        });
    }
    let access_token = raw
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Auth {
            site: site.clone(),
            reason: "missing_access_token".into(),
        })?
        .to_string();
    let user_id = raw.get("user_id").and_then(|v| {
        v.as_str()
            .map(|s| s.to_string())
            .or_else(|| v.as_u64().map(|n| n.to_string()))
    });
    let expires_in = raw.get("expires_in").and_then(|v| v.as_u64());
    Ok(TokenResponse {
        access_token,
        user_id,
        expires_in,
        raw,
    })
}

pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    let mut s = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            s.push('&');
        }
        s.push_str(&form_encode(k));
        s.push('=');
        s.push_str(&form_encode(v));
    }
    s
}

fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn form_decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_shape() {
        let u = authorize_url(
            "https://threads.net/oauth/authorize",
            "99",
            "https://localhost/callback",
            "threads_basic,threads_content_publish",
            "st",
        );
        assert!(u.starts_with("https://threads.net/oauth/authorize?"));
        assert!(u.contains("response_type=code"));
        assert!(u.contains("client_id=99"));
        assert!(u.contains("redirect_uri=https%3A%2F%2Flocalhost%2Fcallback"));
        assert!(u.contains("scope=threads_basic%2Cthreads_content_publish"));
        assert!(u.contains("state=st"));
    }

    #[test]
    fn extract_code_from_url_and_hash() {
        assert_eq!(
            extract_code("https://localhost/callback?code=AQBx#_").unwrap(),
            "AQBx"
        );
        assert_eq!(extract_code("AQBx").unwrap(), "AQBx");
        assert_eq!(
            extract_code("https://localhost/callback?code=AQBx-hBs#_").unwrap(),
            "AQBx-hBs"
        );
        assert!(matches!(
            extract_code("   "),
            Err(Error::Auth { reason, .. }) if reason == "missing_code"
        ));
    }

    #[tokio::test]
    async fn exchange_code_posts_rfc6749_form() {
        use crate::http::Http;
        use httpmock::prelude::*;
        use serde_json::json;

        let server = MockServer::start();
        let m = server.mock(|when, then| {
            when.method(POST).path("/oauth/access_token");
            then.status(200).json_body(json!({
                "access_token": "SHORT",
                "user_id": 1784
            }));
        });
        let http = Http::new().unwrap();
        let tok = exchange_code(
            &http,
            &format!("{}/oauth/access_token", server.base_url()),
            "id",
            "sec",
            "https://localhost/callback",
            "AQBx",
            Deadline::from_secs(30),
            &Site::new("test"),
        )
        .await
        .unwrap();
        m.assert();
        assert_eq!(tok.access_token, "SHORT");
        assert_eq!(tok.user_id.as_deref(), Some("1784"));
    }
}
