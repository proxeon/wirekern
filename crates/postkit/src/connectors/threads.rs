use crate::error::Error;
use crate::form::form;
use crate::http::Http;
use crate::oauth::{authorize_url, exchange_code, extract_code, new_state};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::types::{
    AccountCreds, AppConfig, Body, Capability, Deadline, Intent, OAuthApp, Outcome, Probe, Site,
    WhoAmI,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const GRAPH_HOST: &str = "graph.threads.net";
pub const GRAPH_VERSION: &str = "v1.0";
pub const GRAPH_ORIGIN: &str = "https://graph.threads.net";
pub const AUTHORIZE: &str = "https://threads.net/oauth/authorize";
pub const SITE: &str = "threads";
pub const SCOPES: &str = "threads_basic,threads_content_publish,threads_manage_replies";
/// Meta's Threads text limit in Meta's own counting units: "Text posts are
/// limited to 500 characters. Emojis are counted as the number of UTF-8
/// bytes" (developers.facebook.com/docs/threads/posts). See [`threads_len`].
pub const MAX_TEXT: usize = 500;

/// Pacing between reply-container retries while Meta's write path catches
/// up with the just-published parent (issue 020). Measured live: the read
/// path serves the parent's permalink at ~2s while reply creation still
/// returns `code 24` — until ~30s on 2026-09-08 but 12–15 min on
/// 2026-09-09, so the window varies with Meta-side load by more than an
/// order of magnitude. This is deliberately *not* a backoff schedule
/// guessing the window; completion is gated on the 24-to-success
/// transition itself — the deadline is the operator's bound, not ours.
pub const DEFAULT_REPLY_RETRY_DELAY: Duration = Duration::from_secs(2);

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
    /// Pacing between reply-container retries on Graph `code 24`. A
    /// sampling rate, not a window prediction — see `post_text`.
    reply_retry_delay: Duration,
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
            reply_retry_delay: DEFAULT_REPLY_RETRY_DELAY,
        })
    }

    /// Test helper: shrink the code-24 retry pacing so regression tests
    /// run in milliseconds instead of seconds.
    pub fn with_reply_retry_delay(mut self, delay: Duration) -> Self {
        self.reply_retry_delay = delay;
        self
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
        let reply_to = reply_to_id(&intent.params)?;
        post_text(
            &self.http,
            &self.base,
            token,
            &user_id,
            text,
            reply_to.as_deref(),
            deadline,
            self.reply_retry_delay,
        )
        .await
    }

    /// 027 create-only probe: everything a publish does except the step
    /// that makes it visible. One container-creation attempt, then stop —
    /// no `threads_publish`, and unlike the publish path, no `code 24`
    /// retry: the 2s loop exists to *complete* a publish against the 020
    /// propagation window, while a probe's job is to report the write
    /// path's answer *now*. One call, three operator states (validated
    /// live 2026-09-09): `container_id` → ready, a publish would go
    /// through; Graph `24` "resource does not exist" → a well-formed id
    /// the reply path cannot see yet (the 020 window — or a deleted or
    /// foreign post: wait or re-check, don't retry blindly); Graph `100`
    /// "not a valid threads_media ID" → the id itself is bad, fix the
    /// input. A root-shaped probe (no `reply_to_id`) exercises the same
    /// creation the smoke-test use case wants: credentials and app
    /// accepted end-to-end, nothing visible, container expires in 24h.
    async fn probe(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        intent: Intent,
        deadline: Deadline,
    ) -> Result<Probe, Error> {
        let Body::Text { text } = &intent.body;
        validate_text(text)?;
        let token = access_token(creds)?;
        let user_id = path_user_id(&intent, creds);
        let reply_to = reply_to_id(&intent.params)?;
        let container_id = create_text_container(
            &self.http,
            &self.base,
            token,
            &user_id,
            text,
            reply_to.as_deref(),
            deadline,
        )
        .await?;
        Ok(Probe {
            site: self.site.clone(),
            container_id,
            // Meta discards unpublished containers after 24h; postkit
            // never publishes one from a probe, so the expiry is the
            // probe's only cleanup.
            expires_in_hours: 24,
        })
    }

    async fn whoami(&self, _app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error> {
        let token = access_token(creds)?;
        whoami(&self.http, &self.base, token, Deadline::from_secs(30)).await
    }

    async fn auth_start(&self, app: &AppConfig) -> Result<AuthStart, Error> {
        let oauth = require_oauth(app)?;
        let state = new_state()?;
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

    async fn refresh(&self, _app: &AppConfig, creds: &AccountCreds) -> Result<AccountCreds, Error> {
        let token = access_token(creds)?;
        let user_id = extra_user_id(creds);
        let deadline = Deadline::from_secs(30);
        let q = form(&[("grant_type", "th_refresh_token"), ("access_token", token)]);
        let url = format!("{}/refresh_access_token?{q}", self.graph_origin);
        let resp = self
            .http
            .send(self.http.get(&url), deadline, &self.site)
            .await?;
        let body = read_json(resp, &self.site).await?;
        creds_from_long(&body, user_id)
    }
}

/// Count text the way Meta does: every character is 1 — CJK, Arabic,
/// combining marks included — except an emoji, which counts as its UTF-8
/// byte length (typically 4). The previous whole-string `str::len()` rule
/// charged every non-ASCII script 2–4x its real count and falsely rejected
/// valid posts (e.g. 400 Japanese characters = 1200 bytes, well under the
/// platform's 500).
///
/// ZWJ sequences (family emoji etc.) are approximated per codepoint: each
/// emoji member is charged its bytes, the joiner counts 1. The server's
/// exact rule for composed sequences is undocumented; worst case such a
/// post at the boundary is rejected by the platform and surfaces as its
/// error instead of ours.
pub fn threads_len(text: &str) -> usize {
    text.chars()
        .map(|c| {
            // emojis::get takes &str; encode_utf8 borrows a stack buffer
            // instead of allocating a String per character
            let mut buf = [0u8; 4];
            match emojis::get(c.encode_utf8(&mut buf)) {
                // an emoji burns as many slots as it has UTF-8 bytes
                Some(_) => c.len_utf8(),
                None => 1,
            }
        })
        .sum()
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
    if threads_len(text) > MAX_TEXT {
        return Err(Error::InvalidPost {
            site,
            reason: "text_too_long".into(),
            limit: Some(MAX_TEXT as u32),
        });
    }
    Ok(())
}

pub fn text_form_pairs<'a>(
    text: &'a str,
    reply_to: Option<&'a str>,
    access_token: &'a str,
) -> Vec<(&'a str, &'a str)> {
    let mut pairs = vec![("media_type", "TEXT"), ("text", text)];
    // Replies cannot use auto_publish_text; Meta's create-replies path is
    // container then POST /threads_publish.
    if reply_to.is_none() {
        pairs.push(("auto_publish_text", "true"));
    }
    if let Some(id) = reply_to {
        pairs.push(("reply_to_id", id));
    }
    pairs.push(("access_token", access_token));
    pairs
}

pub fn reply_to_id(params: &Value) -> Result<Option<String>, Error> {
    match params.get("reply_to_id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "reply_to_id".into(),
            limit: None,
        }),
    }
}

/// One container-creation attempt (`POST /{user}/threads`), shared by the
/// publish path (which then publishes the container) and the probe (which
/// stops here). Never sends `auto_publish_text`: that shortcut is only
/// valid for standalone text posts — replies must go container-then-
/// publish, and a probe must never publish at all.
async fn create_text_container(
    http: &Http,
    base: &str,
    access_token: &str,
    user_id: &str,
    text: &str,
    reply_to: Option<&str>,
    deadline: Deadline,
) -> Result<String, Error> {
    let site = Site::new(SITE);
    let url = format!("{}/{}/threads", base.trim_end_matches('/'), user_id);
    let pairs = text_form_pairs(text, reply_to, access_token);
    let req = http
        .post(&url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form(&pairs));
    let resp = http.send(req, deadline, &site).await?;
    let body = read_json(resp, &site).await?;
    json_id(&body, &site, "Graph create")
}

#[allow(clippy::too_many_arguments)]
pub async fn post_text(
    http: &Http,
    base: &str,
    access_token: &str,
    user_id: &str,
    text: &str,
    reply_to: Option<&str>,
    deadline: Deadline,
    reply_retry_delay: Duration,
) -> Result<Outcome, Error> {
    let site = Site::new(SITE);
    // Reply containers race Meta's propagation: the parent is readable
    // (permalink GET green) seconds before the write path accepts replies
    // to it, answering `code 24` ("resource does not exist") in the
    // meantime — issue 020, which measured acceptance anywhere from ~30s
    // to 12–15 min after the parent published, night by night. The reply
    // attempt itself is the only readiness signal that flips in lockstep
    // with acceptance, so retry the creation, gated on the code and the
    // deadline. Retrying is safe: a 24 is an application-level rejection
    // (parent lookup failed server-side), not a lost response — a retried
    // creation cannot double-post. Root posts never retry: a 24 there
    // means a genuinely bad target id.
    let mut last_24: Option<Error> = None;
    let created_id = loop {
        // Out of time mid-retry: the propagation 24 explains the stall
        // better than a bare timeout, so it is the error we surface.
        if deadline.check(&site).is_err() {
            if let Some(e) = last_24.take() {
                return Err(e);
            }
        }
        match create_text_container(http, base, access_token, user_id, text, reply_to, deadline)
            .await
        {
            Ok(id) => break id,
            // the same race one layer down: send's own expiry check can
            // fire between retries — still surface the 24, not the timeout
            Err(e @ Error::DeadlineExceeded { .. }) => match last_24.take() {
                Some(twenty_four) => return Err(twenty_four),
                None => return Err(e),
            },
            Err(e)
                if reply_to.is_some()
                    && matches!(&e, Error::Platform { ref code, .. } if code.as_str() == "24") =>
            {
                last_24 = Some(e);
                tokio::time::sleep(reply_retry_delay).await;
            }
            Err(e) => return Err(e),
        }
    };
    let id = if reply_to.is_some() {
        publish_container(http, base, user_id, &created_id, access_token, deadline).await?
    } else {
        created_id
    };

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

fn json_id(body: &Value, site: &Site, what: &str) -> Result<String, Error> {
    body.get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_id".into(),
            message: format!("{what} returned no id"),
        })
}

fn retry_container_publish(e: &Error) -> bool {
    match e {
        Error::Network { .. } => true,
        Error::Platform { message, .. } => {
            let m = message.to_ascii_lowercase();
            m.contains("not ready")
                || m.contains("in progress")
                || m.contains("try again")
                || m.contains("please wait")
        }
        _ => false,
    }
}

/// Container publish poll bound, independent of the deadline: a
/// misclassified permanent error (say "try again" in unrelated copy)
/// must fail in ~4s, not burn the whole 30s budget polling a dead
/// container. Whichever fires first — attempts or deadline — wins.
const MAX_CONTAINER_ATTEMPTS: usize = 10;
const CONTAINER_RETRY_DELAY_MS: u64 = 400;

async fn publish_container(
    http: &Http,
    base: &str,
    user_id: &str,
    creation_id: &str,
    access_token: &str,
    deadline: Deadline,
) -> Result<String, Error> {
    let site = Site::new(SITE);
    let url = format!("{}/{}/threads_publish", base.trim_end_matches('/'), user_id);
    let mut last: Option<Error> = None;
    for _ in 1..=MAX_CONTAINER_ATTEMPTS {
        if let Err(e) = deadline.check(&site) {
            return Err(last.unwrap_or(e));
        }
        let req = http
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form(&[
                ("creation_id", creation_id),
                ("access_token", access_token),
            ]));
        match http.send(req, deadline, &site).await {
            Ok(resp) => match read_json(resp, &site).await {
                Ok(body) => return json_id(&body, &site, "threads_publish"),
                Err(e) if retry_container_publish(&e) => last = Some(e),
                Err(e) => return Err(e),
            },
            Err(e) if retry_container_publish(&e) => last = Some(e),
            Err(e) => return Err(e),
        }
        if let Err(e) = deadline.check(&site) {
            return Err(last.unwrap_or(e));
        }
        tokio::time::sleep(Duration::from_millis(CONTAINER_RETRY_DELAY_MS)).await;
    }
    Err(last.unwrap_or_else(|| Error::Platform {
        site,
        code: "container_not_published".into(),
        message: format!("threads_publish not ready after {MAX_CONTAINER_ATTEMPTS} attempts"),
    }))
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

// Form encoding lives in crate::form, shared with oauth.rs — no local copy.

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

async fn long_lived(
    http: &Http,
    graph_origin: &str,
    client_secret: &str,
    short: &str,
    user_id: Option<String>,
    deadline: Deadline,
) -> Result<AccountCreds, Error> {
    let site = Site::new(SITE);
    let q = form(&[
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
    // Numeric codes are the primary classifier; the English-substring
    // fallbacks run ONLY when Meta sent no code at all. Heuristics that
    // carried equal weight with codes let any code-bearing error
    // reclassify whenever its copy happened to contain "quota" or
    // "expired" — and Meta rewords that copy over time. With no code the
    // strings are all we have (some Graph errors arrive message-only).
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
        // Meta often returns HTTP 500 for param errors (code 100) with a JSON body.
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
    use httpmock::prelude::*;
    use serde_json::json;
    use std::sync::Arc;

    fn token_creds() -> AccountCreds {
        AccountCreds::OAuth2 {
            access_token: "tok".into(),
            refresh_token: None,
            extra: json!({}),
        }
    }

    #[tokio::test]
    async fn whoami_redirect_is_an_error_not_a_follow() {
        // whoami is a token-bearing GET: a 3xx must surface as an error and
        // the host in Location must never receive the re-issued request.
        let attacker = MockServer::start();
        let sink = attacker.mock(|when, then| {
            when.method(GET).path("/sink");
            then.status(200).body("got it");
        });
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/me");
            then.status(302)
                .header("Location", format!("{}/sink", attacker.base_url()));
        });

        let t = Threads::with_base(server.base_url()).unwrap();
        let err = t.whoami(&empty_app(), &token_creds()).await.unwrap_err();
        // read_json maps the raw 302 body through the graph error mapper;
        // exact variant aside, it must be an error, not a follow.
        assert!(matches!(
            err,
            Error::Auth { .. } | Error::Platform { .. } | Error::Network { .. }
        ));
        assert_eq!(sink.hits(), 0);
    }

    #[tokio::test]
    async fn auth_start_state_is_csprng_hex() {
        let t = Threads::new().unwrap();
        let app = oauth_app();
        let (a, b) = match (t.auth_start(&app).await, t.auth_start(&app).await) {
            (
                Ok(AuthStart::Browser {
                    authorize_url: ua,
                    state: sa,
                }),
                Ok(AuthStart::Browser {
                    authorize_url: ub,
                    state: sb,
                }),
            ) => ((ua, sa), (ub, sb)),
            _ => panic!("auth_start should return Browser"),
        };
        // 128 bits of lowercase hex — the old time-nanos state was ~11 chars.
        for state in [&a.1, &b.1] {
            assert_eq!(state.len(), 32);
            assert!(state.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(state.chars().all(|c| !c.is_ascii_uppercase()));
        }
        assert_ne!(a.1, b.1);
        // the authorize URL echoes the state the CLI will verify on paste
        assert_eq!(
            crate::oauth::query_param(&a.0, "state").as_deref(),
            Some(&a.1[..])
        );
        assert_eq!(
            crate::oauth::query_param(&b.0, "state").as_deref(),
            Some(&b.1[..])
        );
    }

    fn text_intent(text: &str) -> Intent {
        Intent {
            site: Site::new(SITE),
            params: json!({}),
            body: Body::Text { text: text.into() },
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
    fn limit_is_metas_character_rule() {
        // Meta: 500 characters, emoji charged as their UTF-8 byte length.
        validate_text(&"a".repeat(500)).unwrap();
        let err = validate_text(&"a".repeat(501)).unwrap_err();
        assert!(
            matches!(err, Error::InvalidPost { reason, limit, .. } if reason == "text_too_long" && limit == Some(500))
        );
    }

    #[test]
    fn cjk_counts_one_per_character() {
        // The old byte rule rejected this: 400 chars = 1200 UTF-8 bytes.
        validate_text(&"漢".repeat(400)).unwrap();
        validate_text(&"漢".repeat(500)).unwrap();
        let err = validate_text(&"漢".repeat(501)).unwrap_err();
        assert!(matches!(err, Error::InvalidPost { reason, .. } if reason == "text_too_long"));
    }

    #[test]
    fn emoji_charged_as_utf8_bytes() {
        // 😀 is 4 bytes: eats four of the 500 slots.
        validate_text(&format!("{}😀", "a".repeat(496))).unwrap(); // 500
        let err = validate_text(&format!("{}😀", "a".repeat(497))).unwrap_err(); // 501
        assert!(matches!(err, Error::InvalidPost { reason, .. } if reason == "text_too_long"));
    }

    #[test]
    fn threads_len_matches_metas_rule() {
        // ascii + two CJK chars (1 each) + one emoji (4 bytes)
        assert_eq!(threads_len("abc漢字😀"), 9);
        assert_eq!(threads_len(&"漢".repeat(400)), 400);
        assert_eq!(threads_len("😀"), 4);
        // ZWJ family: each emoji member its bytes, joiners 1 — the
        // documented per-codepoint approximation.
        assert_eq!(threads_len("👨‍👩‍👧"), 4 + 1 + 4 + 1 + 4);
    }

    #[test]
    fn form_pairs_root_has_no_reply_to() {
        let pairs = text_form_pairs("hello", None, "tok");
        assert!(!pairs.iter().any(|(k, _)| *k == "reply_to_id"));
        assert!(pairs.contains(&("media_type", "TEXT")));
        assert!(pairs.contains(&("auto_publish_text", "true")));
        assert!(pairs.contains(&("text", "hello")));
    }

    #[test]
    fn form_pairs_includes_reply_to() {
        let pairs = text_form_pairs("reply", Some("17900"), "tok");
        assert!(pairs.contains(&("reply_to_id", "17900")));
        let keys: Vec<_> = pairs.iter().map(|(k, _)| *k).collect();
        assert!(!pairs.iter().any(|(k, _)| *k == "auto_publish_text"));
        assert_eq!(
            keys,
            vec!["media_type", "text", "reply_to_id", "access_token"]
        );
    }

    #[test]
    fn reply_to_id_missing_empty_or_string() {
        assert_eq!(reply_to_id(&json!({})).unwrap(), None);
        assert_eq!(reply_to_id(&json!({ "reply_to_id": null })).unwrap(), None);
        assert_eq!(reply_to_id(&json!({ "reply_to_id": "" })).unwrap(), None);
        assert_eq!(
            reply_to_id(&json!({ "reply_to_id": "17900" }))
                .unwrap()
                .as_deref(),
            Some("17900")
        );
        let err = reply_to_id(&json!({ "reply_to_id": 17900 })).unwrap_err();
        assert!(matches!(err, Error::InvalidPost { reason, .. } if reason == "reply_to_id"));
    }

    #[tokio::test]
    async fn reply_to_id_publish_ok() {
        let server = MockServer::start();
        let create = server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(200).json_body(json!({ "id": "C" }));
        });
        let publish = server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads_publish");
            then.status(200).json_body(json!({ "id": "B" }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url())).unwrap();
        let mut intent = text_intent("reply");
        intent.params = json!({ "reply_to_id": "A" });
        let out = t
            .publish(
                &empty_app(),
                &token_creds(),
                intent,
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        create.assert();
        publish.assert();
        assert_eq!(out.id.as_deref(), Some("B"));
    }

    #[tokio::test]
    async fn reply_to_id_non_string_no_http() {
        let server = MockServer::start();
        let create = server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(200).json_body(json!({ "id": "x" }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url())).unwrap();
        let mut intent = text_intent("reply");
        intent.params = json!({ "reply_to_id": true });
        let err = t
            .publish(
                &empty_app(),
                &token_creds(),
                intent,
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidPost { reason, .. } if reason == "reply_to_id"));
        create.assert_hits(0);
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
            then.status(200).json_body(
                json!({ "id": "17900", "permalink": "https://www.threads.net/@x/post/abc" }),
            );
        });
        let publish = server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads_publish");
            then.status(500);
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
        publish.assert_hits(0);
        assert_eq!(out.id.as_deref(), Some("17900"));
        assert_eq!(
            out.url.as_deref(),
            Some("https://www.threads.net/@x/post/abc")
        );
    }

    #[tokio::test]
    async fn reply_chain_root_then_reply() {
        let server = MockServer::start();
        let root_create = server.mock(|when, then| {
            when.method(POST)
                .path("/v1.0/me/threads")
                .x_www_form_urlencoded_key_exists("auto_publish_text");
            then.status(200).json_body(json!({ "id": "A" }));
        });
        let reply_create = server.mock(|when, then| {
            when.method(POST)
                .path("/v1.0/me/threads")
                .x_www_form_urlencoded_key_exists("reply_to_id");
            then.status(200).json_body(json!({ "id": "C" }));
        });
        let reply_publish = server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads_publish");
            then.status(200).json_body(json!({ "id": "B" }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url())).unwrap();
        let root = t
            .publish(
                &empty_app(),
                &token_creds(),
                text_intent("root"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(root.id.as_deref(), Some("A"));
        root_create.assert();
        reply_publish.assert_hits(0);
        let mut reply = text_intent("reply");
        reply.params = json!({ "reply_to_id": root.id.clone().unwrap() });
        let out = t
            .publish(&empty_app(), &token_creds(), reply, Deadline::from_secs(30))
            .await
            .unwrap();
        reply_create.assert();
        reply_publish.assert();
        assert_eq!(out.id.as_deref(), Some("B"));
    }

    #[test]
    fn graph_error_code_outranks_message_copy() {
        // A precise code must not be hijacked by English copy that happens
        // to contain the heuristic words — Meta rewords copy over time.
        let err = map_graph_error(
            400,
            r#"{"error":{"code":100,"message":"this token has expired; quota weirdness"}}"#,
        );
        assert!(matches!(err, Error::Platform { code, .. } if code == "100"));
        // No code at all: the substring fallback still classifies.
        let err = map_graph_error(
            400,
            r#"{"error":{"message":"Error validating access token: Session has expired"}}"#,
        );
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "token_expired"));
        let err = map_graph_error(400, r#"{"error":{"message":"rate limit reached"}}"#);
        assert!(matches!(err, Error::RateLimited { .. }));
    }

    #[test]
    fn rate_limit_codes_are_rate_limited() {
        // 17 / 32 / 613 were checked but untested before.
        for code in [4, 17, 32, 613] {
            let body = format!(r#"{{"error":{{"code":{code},"message":"generic"}}}}"#);
            let err = map_graph_error(400, &body);
            assert!(
                matches!(err, Error::RateLimited { .. }),
                "code {code} should be RateLimited, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn container_publish_gives_up_after_max_attempts() {
        // A permanently "not ready" container must fail after
        // MAX_CONTAINER_ATTEMPTS (≈4s), not burn the whole deadline.
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(200).json_body(json!({ "id": "17900" }));
        });
        let poll = server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads_publish");
            then.status(400)
                .json_body(json!({ "error": { "code": 100, "message": "media not ready" } }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url())).unwrap();
        let mut intent = text_intent("reply");
        intent.params = json!({ "reply_to_id": "1790" });
        let err = t
            .publish(
                &empty_app(),
                &token_creds(),
                intent,
                Deadline::from_secs(60),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Platform { .. }));
        assert_eq!(poll.hits(), MAX_CONTAINER_ATTEMPTS);
    }

    #[tokio::test]
    async fn reply_container_retries_code_24_until_accepted() {
        // httpmock has no call-limit API, so a spawned task swaps the 24
        // mock for a success mock mid-publish; the current-thread runtime
        // advances the swap timer while post_text awaits between retries.
        // The server is leaked so the Mock handle (Mock<'static>) can move
        // into the spawned task; the process reclaims it at test exit.
        let server: &'static MockServer = Box::leak(Box::new(MockServer::start()));
        let m24 = server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(400).json_body(json!({
                "error": { "code": 24, "message": "The requested resource does not exist" }
            }));
        });
        server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads_publish");
            then.status(200).json_body(json!({ "id": "18800" }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v1.0/18800");
            then.status(200)
                .json_body(json!({ "permalink": "https://example.test/18800" }));
        });
        let hits24 = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let hits = hits24.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(40)).await;
                hits.store(m24.hits(), std::sync::atomic::Ordering::SeqCst);
                m24.delete_async().await;
                server.mock(|when, then| {
                    when.method(POST).path("/v1.0/me/threads");
                    then.status(200).json_body(json!({ "id": "17900" }));
                });
            });
        }
        let t = Threads::with_base(format!("{}/v1.0", server.base_url()))
            .unwrap()
            .with_reply_retry_delay(Duration::from_millis(1));
        let mut intent = text_intent("reply");
        intent.params = json!({ "reply_to_id": "1790" });
        let out = t
            .publish(
                &empty_app(),
                &token_creds(),
                intent,
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(out.id.as_deref(), Some("18800"));
        assert!(
            hits24.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the 24 must have been sampled before the swap"
        );
    }

    #[tokio::test]
    async fn reply_container_24_past_deadline_surfaces_the_platform_error() {
        // Deadline exhausted mid-retry: the propagation 24 is the
        // informative error, not a bare timeout.
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(400).json_body(json!({
                "error": { "code": 24, "message": "The requested resource does not exist" }
            }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url()))
            .unwrap()
            .with_reply_retry_delay(Duration::from_millis(50));
        let mut intent = text_intent("reply");
        intent.params = json!({ "reply_to_id": "1790" });
        let err = t
            .publish(&empty_app(), &token_creds(), intent, Deadline::from_secs(1))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Platform { ref code, .. } if code == "24"));
    }

    #[tokio::test]
    async fn root_container_24_does_not_retry() {
        // A 24 on a root creation means a genuinely bad target id, not
        // propagation: it must surface immediately, once.
        let server = MockServer::start();
        let m = server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(400).json_body(json!({
                "error": { "code": 24, "message": "The requested resource does not exist" }
            }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url()))
            .unwrap()
            .with_reply_retry_delay(Duration::from_millis(1));
        let err = t
            .publish(
                &empty_app(),
                &token_creds(),
                text_intent("root"),
                Deadline::from_secs(5),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Platform { ref code, .. } if code == "24"));
        assert_eq!(m.hits(), 1);
    }

    // --- 027: create-only probe -------------------------------------

    #[tokio::test]
    async fn probe_creates_container_and_never_publishes() {
        let server = MockServer::start();
        let create = server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(200).json_body(json!({ "id": "C" }));
        });
        // the mock exists only to prove it is never touched: a probe that
        // publishes is not a probe
        let publish = server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads_publish");
            then.status(200).json_body(json!({ "id": "B" }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url())).unwrap();
        let mut intent = text_intent("probe me");
        intent.params = json!({ "reply_to_id": "A" });
        let p = t
            .probe(
                &empty_app(),
                &token_creds(),
                intent,
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        create.assert();
        publish.assert_hits(0);
        assert_eq!(p.container_id, "C");
        assert_eq!(p.expires_in_hours, 24);
    }

    #[tokio::test]
    async fn probe_answers_code_24_once_without_retrying() {
        // The publish path samples the 24 until Meta's write path catches
        // up (020); a probe's product is the *current* answer, so exactly
        // one attempt — a probe that retried would be a slow publish.
        let server = MockServer::start();
        let m = server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(400).json_body(json!({
                "error": { "code": 24, "message": "The requested resource does not exist" }
            }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url()))
            .unwrap()
            // a 1ms pacing makes an accidental retry loop finish fast —
            // and still fail the hits() assertion below
            .with_reply_retry_delay(Duration::from_millis(1));
        let mut intent = text_intent("reply");
        intent.params = json!({ "reply_to_id": "1790" });
        let err = t
            .probe(&empty_app(), &token_creds(), intent, Deadline::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Platform { ref code, .. } if code == "24"));
        assert_eq!(m.hits(), 1);
    }

    #[tokio::test]
    async fn multiline_text_posts_percent_encoded_newlines() {
        // Threads renders \n as line breaks, but a
        // application/x-www-form-urlencoded body may not carry them raw —
        // form() must emit %0A (validated live 2026-09-09: Meta accepted a
        // %0A-encoded multi-line post, 329/500 units). The mock only
        // matches when the escaped form arrives; a raw-newline body falls
        // through to an unmatched route and fails loudly.
        let server = MockServer::start();
        let create = server.mock(|when, then| {
            when.method(POST)
                .path("/v1.0/me/threads")
                .body_contains("text=Baris+pertama%0A%0ABaris+kedua");
            then.status(200).json_body(json!({ "id": "17900" }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v1.0/17900");
            then.status(200)
                .json_body(json!({ "permalink": "https://example.test/17900" }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url())).unwrap();
        let out = t
            .publish(
                &empty_app(),
                &token_creds(),
                text_intent("Baris pertama\n\nBaris kedua"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        create.assert();
        assert_eq!(out.id.as_deref(), Some("17900"));
    }

    #[tokio::test]
    async fn probe_rejects_invalid_text_before_http() {
        let server = MockServer::start();
        let create = server.mock(|when, then| {
            when.method(POST).path("/v1.0/me/threads");
            then.status(200).json_body(json!({ "id": "x" }));
        });
        let t = Threads::with_base(format!("{}/v1.0", server.base_url())).unwrap();
        let err = t
            .probe(
                &empty_app(),
                &token_creds(),
                text_intent(&"a".repeat(501)),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidPost { ref reason, .. } if reason == "text_too_long"));
        create.assert_hits(0);
    }

    #[test]
    fn retry_container_publish_rules() {
        let site = Site::new(SITE);
        assert!(retry_container_publish(&Error::Network {
            site: site.clone(),
            message: "http_500".into(),
        }));
        assert!(retry_container_publish(&Error::Platform {
            site: site.clone(),
            code: "1".into(),
            message: "media not ready".into(),
        }));
        assert!(!retry_container_publish(&Error::Auth {
            site,
            reason: "token_expired".into(),
        }));
    }

    #[test]
    fn graph_500_code_100_is_platform() {
        let err = map_graph_error(
            500,
            r#"{"error":{"message":"Param reply_to_id is not a valid threads_media ID","type":"THApiException","code":100}}"#,
        );
        assert!(
            matches!(err, Error::Platform { ref code, ref message, .. } if code == "100" && message.contains("reply_to_id"))
        );
    }

    #[test]
    fn graph_500_empty_is_network_http_500() {
        let err = map_graph_error(500, "");
        assert!(matches!(err, Error::Network { ref message, .. } if message == "http_500"));
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
            then.status(500)
                .json_body(json!({ "error": { "message": "down" } }));
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
                assert!(authorize_url
                    .contains("threads_basic%2Cthreads_content_publish%2Cthreads_manage_replies"));
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
        let t = Threads::with_origins(format!("{}/v1.0", server.base_url()), server.base_url())
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
        let t = Threads::with_origins(format!("{}/v1.0", server.base_url()), server.base_url())
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
