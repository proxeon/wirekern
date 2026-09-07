use crate::error::Error;
use crate::http::Http;
use crate::oauth::{authorize_url, exchange_code, extract_code};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::types::{
    AccountCreds, AppConfig, Body, Capability, Deadline, Intent, OAuthApp, Outcome, Site, WhoAmI,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub const GRAPH_HOST: &str = "graph.threads.net";
pub const GRAPH_VERSION: &str = "v1.0";
pub const GRAPH_ORIGIN: &str = "https://graph.threads.net";
pub const AUTHORIZE: &str = "https://threads.net/oauth/authorize";
pub const SITE: &str = "threads";
pub const SCOPES: &str = "threads_basic,threads_content_publish";
pub const MAX_TEXT_BYTES: usize = 500;

fn default_base() -> String {
    format!("https://{GRAPH_HOST}/{GRAPH_VERSION}")
}

#[derive(Debug, Default, Deserialize)]
pub struct ThreadsParams {
    pub user_id: Option<String>,
}

pub struct Threads {
    http: Http,
    site: Site,
    base: String,
    graph_origin: String,
}

impl Threads {
    pub fn new() -> Result<Self, Error> {
        Self::with_origins(default_base(), GRAPH_ORIGIN)
    }

    /// Test helper: httpmock publish base, e.g. `http://127.0.0.1:PORT/v1.0`.
    pub fn with_base(base: impl Into<String>) -> Result<Self, Error> {
        Self::with_origins(base, GRAPH_ORIGIN)
    }

    /// Test helper: separate publish `/v1.0` base and unversioned Graph origin.
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
impl Publisher for Threads {
    fn site(&self) -> &Site {
        &self.site
    }

    fn capabilities(&self) -> &[Capability] {
        &[Capability::PublishText]
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::OAuth2AuthCode
    }

    async fn publish(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        intent: Intent,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        let Body::Text { text } = &intent.body;
        validate_text(text)?;
        let token = access_token(creds)?;
        let user_id = path_user_id(&intent, creds);
        post_text(&self.http, &self.base, token, &user_id, text, deadline).await
    }

    async fn whoami(&self, _app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error> {
        let token = access_token(creds)?;
        whoami(&self.http, &self.base, token, Deadline::from_secs(30)).await
    }

    async fn auth_start(&self, app: &AppConfig) -> Result<AuthStart, Error> {
        let oauth = require_oauth(app)?;
        let state = new_state();
        let authorize_url = authorize_url(
            AUTHORIZE,
            &oauth.client_id,
            &oauth.redirect_uri,
            SCOPES,
            &state,
        );
        Ok(AuthStart::Browser {
            authorize_url,
            state,
        })
    }

    async fn auth_finish(
        &self,
        app: &AppConfig,
        reply: AuthReply,
    ) -> Result<AccountCreds, Error> {
        let oauth = require_oauth(app)?;
        let raw = match reply {
            AuthReply::Pasted { code } => code,
            AuthReply::Redirect { url } => url,
        };
        let code = extract_code(&raw)?;
        let deadline = Deadline::from_secs(30);
        let site = self.site.clone();
        let token_ep = format!("{}/oauth/access_token", self.graph_origin);
        let short = exchange_code(
            &self.http,
            &token_ep,
            &oauth.client_id,
            &oauth.client_secret,
            &oauth.redirect_uri,
            &code,
            deadline,
            &site,
        )
        .await?;
        long_lived(
            &self.http,
            &self.graph_origin,
            &oauth.client_secret,
            &short.access_token,
            short.user_id,
            deadline,
        )
        .await
    }

    async fn refresh(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
    ) -> Result<AccountCreds, Error> {
        let token = access_token(creds)?;
        let user_id = extra_user_id(creds);
        let deadline = Deadline::from_secs(30);
        let q = crate::oauth::form(&[
            ("grant_type", "th_refresh_token"),
            ("access_token", token),
        ]);
        let url = format!("{}/refresh_access_token?{q}", self.graph_origin);
        let resp = self
            .http
            .send(self.http.get(&url), deadline, &self.site)
            .await?;
        let body = read_json(resp, &self.site).await?;
        creds_from_long(&body, user_id)
    }
}

pub fn validate_text(text: &str) -> Result<(), Error> {
    let site = Site::new(SITE);
    if text.trim().is_empty() {
        return Err(Error::InvalidPost {
            site,
            reason: "empty".into(),
            limit: None,
        });
    }
    if text.as_bytes().len() > MAX_TEXT_BYTES {
        return Err(Error::InvalidPost {
            site,
            reason: "text_too_long".into(),
            limit: Some(MAX_TEXT_BYTES as u32),
        });
    }
    Ok(())
}

pub async fn post_text(
    http: &Http,
    base: &str,
    access_token: &str,
    user_id: &str,
    text: &str,
    deadline: Deadline,
) -> Result<Outcome, Error> {
    let site = Site::new(SITE);
    let url = format!("{}/{}/threads", base.trim_end_matches('/'), user_id);
    let req = http.post(&url).header(
        "Content-Type",
        "application/x-www-form-urlencoded",
    ).body(form(&[
        ("media_type", "TEXT"),
        ("text", text),
        ("auto_publish_text", "true"),
        ("access_token", access_token),
    ]));
    let resp = http.send(req, deadline, &site).await?;
    let created = read_json(resp, &site).await?;
    let id = created
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_id".into(),
            message: "Graph create returned no id".into(),
        })?
        .to_string();

    let mut url_out = None;
    let get_url = format!(
        "{}/{}?fields=id,permalink&access_token={}",
        base.trim_end_matches('/'),
        id,
        access_token
    );
    if let Ok(resp) = http.send(http.get(&get_url), deadline, &site).await {
        if let Ok(body) = read_json(resp, &site).await {
            url_out = body
                .get("permalink")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
    }

    Ok(Outcome {
        site,
        id: Some(id),
        url: url_out,
        limits: None,
    })
}

async fn whoami(
    http: &Http,
    base: &str,
    access_token: &str,
    deadline: Deadline,
) -> Result<WhoAmI, Error> {
    let site = Site::new(SITE);
    let url = format!(
        "{}/me?fields=id,username&access_token={}",
        base.trim_end_matches('/'),
        access_token
    );
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
    let handle = body
        .get("username")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok(WhoAmI { site, id, handle })
}

fn form(pairs: &[(&str, &str)]) -> String {
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
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

fn require_oauth(app: &AppConfig) -> Result<&OAuthApp, Error> {
    app.oauth.as_ref().ok_or_else(|| Error::Auth {
        site: Site::new(SITE),
        reason: "missing_app_config".into(),
    })
}

fn new_state() -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{n:x}")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn long_lived(
    http: &Http,
    graph_origin: &str,
    client_secret: &str,
    short: &str,
    user_id: Option<String>,
    deadline: Deadline,
) -> Result<AccountCreds, Error> {
    let site = Site::new(SITE);
    let q = crate::oauth::form(&[
        ("grant_type", "th_exchange_token"),
        ("client_secret", client_secret),
        ("access_token", short),
    ]);
    let url = format!("{graph_origin}/access_token?{q}");
    let resp = http.send(http.get(&url), deadline, &site).await?;
    let body = read_json(resp, &site).await?;
    creds_from_long(&body, user_id)
}

fn creds_from_long(body: &Value, user_id: Option<String>) -> Result<AccountCreds, Error> {
    let site = Site::new(SITE);
    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Auth {
            site: site.clone(),
            reason: "missing_access_token".into(),
        })?
        .to_string();
    let now = unix_now();
    let mut extra = serde_json::Map::new();
    if let Some(id) = user_id.or_else(|| {
        body.get("user_id").and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
    }) {
        extra.insert("user_id".into(), Value::String(id));
    }
    extra.insert("refreshed_at".into(), json_u64(now));
    if let Some(exp) = body.get("expires_in").and_then(|v| v.as_u64()) {
        extra.insert("expires_at".into(), json_u64(now.saturating_add(exp)));
    }
    Ok(AccountCreds::OAuth2 {
        access_token,
        refresh_token: None,
        extra: Value::Object(extra),
    })
}

fn json_u64(n: u64) -> Value {
    Value::Number(n.into())
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

fn path_user_id(intent: &Intent, creds: &AccountCreds) -> String {
    let params: ThreadsParams = serde_json::from_value(intent.params.clone()).unwrap_or_default();
    if let Some(id) = params.user_id.filter(|s| !s.is_empty()) {
        return id;
    }
    extra_user_id(creds).unwrap_or_else(|| "me".into())
}

fn extra_user_id(creds: &AccountCreds) -> Option<String> {
    let AccountCreds::OAuth2 { extra, .. } = creds else {
        return None;
    };
    extra.get("user_id").and_then(|v| {
        v.as_str()
            .map(|s| s.to_string())
            .or_else(|| v.as_u64().map(|n| n.to_string()))
    })
}

async fn read_json(resp: reqwest::Response, site: &Site) -> Result<Value, Error> {
    let status = resp.status();
    let text = resp.text().await.map_err(|e| Error::Network {
        site: site.clone(),
        message: e.to_string(),
    })?;
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

    if code == 190
        || lower.contains("validating access token")
        || lower.contains("expired")
        || lower.contains("invalid oauth")
    {
        return Error::Auth {
            site,
            reason: "token_expired".into(),
        };
    }
    if code == 4
        || code == 17
        || code == 32
        || code == 613
        || lower.contains("quota")
        || lower.contains("rate limit")
        || lower.contains("publishing limit")
    {
        return Error::RateLimited {
            site,
            retry_after: None,
        };
    }
    if http_status >= 500 {
        return Error::Network {
            site,
            message: message.to_string(),
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
    use httpmock::prelude::*;
    use serde_json::json;

    fn token_creds() -> AccountCreds {
        AccountCreds::OAuth2 {
            access_token: "tok".into(),
            refresh_token: None,
            extra: json!({}),
        }
    }

    fn text_intent(text: &str) -> Intent {
        Intent {
            site: Site::new(SITE),
            params: json!({}),
            body: Body::Text {
                text: text.into(),
            },
            idempotency_key: None,
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

    #[test]
    fn empty_text_no_http() {
        let err = validate_text("   ").unwrap_err();
        assert!(matches!(err, Error::InvalidPost { reason, .. } if reason == "empty"));
    }

    #[test]
    fn too_long_is_bytes() {
        let s = "a".repeat(501);
        let err = validate_text(&s).unwrap_err();
        assert!(
            matches!(err, Error::InvalidPost { reason, limit, .. } if reason == "text_too_long" && limit == Some(500))
        );
        let ok = "é".repeat(250);
        assert_eq!(ok.as_bytes().len(), 500);
        validate_text(&ok).unwrap();
    }

    #[tokio::test]
    async fn happy_post_and_permalink() {
        let server = MockServer::start();
        let create = server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(200).json_body(json!({ "id": "17900" }));
        });
        let get = server.mock(|when, then| {
            when.method(GET).path("/v1.0/17900");
            then.status(200)
                .json_body(json!({ "id": "17900", "permalink": "https://www.threads.net/@x/post/abc" }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url())).unwrap();
        let out = t
            .publish(
                &empty_app(),
                &token_creds(),
                text_intent("hello"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        create.assert();
        get.assert();
        assert_eq!(out.id.as_deref(), Some("17900"));
        assert_eq!(
            out.url.as_deref(),
            Some("https://www.threads.net/@x/post/abc")
        );
    }

    #[tokio::test]
    async fn permalink_fail_still_ok() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(200).json_body(json!({ "id": "17900" }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v1.0/17900");
            then.status(500).json_body(json!({ "error": { "message": "down" } }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url())).unwrap();
        let out = t
            .publish(
                &empty_app(),
                &token_creds(),
                text_intent("hello"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(out.id.as_deref(), Some("17900"));
        assert!(out.url.is_none());
    }

    #[tokio::test]
    async fn code_190_is_token_expired() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(400).json_body(json!({
                "error": { "code": 190, "message": "Error validating access token", "type": "OAuthException" }
            }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url())).unwrap();
        let err = t
            .publish(
                &empty_app(),
                &token_creds(),
                text_intent("hello"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "token_expired"));
    }

    #[tokio::test]
    async fn quota_is_rate_limited() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(400).json_body(json!({
                "error": { "code": 4, "message": "publishing limit reached" }
            }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url())).unwrap();
        let err = t
            .publish(
                &empty_app(),
                &token_creds(),
                text_intent("hello"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::RateLimited { .. }));
    }

    #[tokio::test]
    async fn other_400_is_platform() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(400).json_body(json!({
                "error": { "code": 100, "message": "weird" }
            }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url())).unwrap();
        let err = t
            .publish(
                &empty_app(),
                &token_creds(),
                text_intent("hello"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Platform { code, .. } if code == "100"));
    }

    #[tokio::test]
    async fn auth_start_needs_app() {
        let t = Threads::new().unwrap();
        let err = t.auth_start(&empty_app()).await.unwrap_err();
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "missing_app_config"));
    }

    #[tokio::test]
    async fn auth_start_url_shape() {
        let t = Threads::new().unwrap();
        match t.auth_start(&oauth_app()).await.unwrap() {
            AuthStart::Browser {
                authorize_url,
                state,
            } => {
                assert!(authorize_url.contains("https://threads.net/oauth/authorize"));
                assert!(authorize_url.contains("threads_basic%2Cthreads_content_publish"));
                assert!(authorize_url.contains("response_type=code"));
                assert!(!state.is_empty());
                assert!(authorize_url.contains(&format!("state={state}")));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn auth_finish_exchanges_to_long_lived() {
        let server = MockServer::start();
        let short = server.mock(|when, then| {
            when.method(POST).path("/oauth/access_token");
            then.status(200).json_body(json!({
                "access_token": "SSS",
                "user_id": 1784
            }));
        });
        let long = server.mock(|when, then| {
            when.method(GET)
                .path("/access_token")
                .query_param("grant_type", "th_exchange_token");
            then.status(200).json_body(json!({
                "access_token": "LLL",
                "token_type": "bearer",
                "expires_in": 5183944
            }));
        });
        let t = Threads::with_origins(
            format!("{}/v1.0", server.base_url()),
            server.base_url(),
        )
        .unwrap();
        let creds = t
            .auth_finish(
                &oauth_app(),
                AuthReply::Pasted {
                    code: "AQBx#_".into(),
                },
            )
            .await
            .unwrap();
        short.assert();
        long.assert();
        match creds {
            AccountCreds::OAuth2 {
                access_token,
                refresh_token,
                extra,
            } => {
                assert_eq!(access_token, "LLL");
                assert!(refresh_token.is_none());
                assert_ne!(access_token, "SSS");
                assert_eq!(extra.get("user_id").and_then(|v| v.as_str()), Some("1784"));
                assert!(extra.get("expires_at").and_then(|v| v.as_u64()).is_some());
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn refresh_hits_th_refresh_token() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/refresh_access_token")
                .query_param("grant_type", "th_refresh_token");
            then.status(200).json_body(json!({
                "access_token": "NEW",
                "expires_in": 5183944
            }));
        });
        let t = Threads::with_origins(
            format!("{}/v1.0", server.base_url()),
            server.base_url(),
        )
        .unwrap();
        let old = AccountCreds::OAuth2 {
            access_token: "OLD".into(),
            refresh_token: None,
            extra: json!({ "user_id": "1784" }),
        };
        let new = t.refresh(&oauth_app(), &old).await.unwrap();
        match new {
            AccountCreds::OAuth2 {
                access_token,
                extra,
                ..
            } => {
                assert_eq!(access_token, "NEW");
                assert_eq!(extra.get("user_id").and_then(|v| v.as_str()), Some("1784"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    #[ignore]
    async fn live_post() {
        if std::env::var("POSTKIT_LIVE").ok().as_deref() != Some("1") {
            return;
        }
        let token = std::env::var("POSTKIT_THREADS_TOKEN").expect("POSTKIT_THREADS_TOKEN");
        let t = Threads::new().unwrap();
        let creds = AccountCreds::OAuth2 {
            access_token: token,
            refresh_token: None,
            extra: json!({}),
        };
        let out = t
            .publish(
                &empty_app(),
                &creds,
                text_intent("postkit live test"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert!(out.id.is_some());
    }
}
