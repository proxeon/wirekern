//! X API v2: narrow public text posts plus intentionally guarded 1:1 DMs.
//!
//! This is not an X Ads connector. Ads use separate access, contracts, and
//! safety controls, so the organic `/2/tweets` and DM APIs never inherit an
//! advertising capability merely because they share the X brand.

use crate::error::Error;
use crate::facets::XDirectMessages;
use crate::form::form;
use crate::http::Http;
use crate::oauth::{extract_code, new_state};
use crate::publisher::{AuthKind, AuthReply, AuthStart, AuthStartOptions, Publisher};
use crate::registry::Connector;
use crate::types::{
    AccountCreds, AppConfig, Body, Capability, Deadline, Intent, OAuthApp, OAuthPkceSession,
    Outcome, Site, WhoAmI,
};
use crate::x::{valid_x_id, XDirectMessageRequest};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const SITE: &str = "x";
pub const API_ORIGIN: &str = "https://api.x.com";
pub const AUTHORIZE: &str = "https://x.com/i/oauth2/authorize";
pub const TOKEN_ENDPOINT: &str = "https://api.x.com/2/oauth2/token";
/// Public post authorization is deliberately the baseline. Direct-message
/// scopes are added only by `wirekern auth x --with-dm`.
pub const POST_SCOPES: &str = "tweet.read tweet.write users.read offline.access";
pub const DM_SCOPES: &str = "dm.read dm.write";
const PKCE_TTL_SECS: u64 = 10 * 60;

/// An X API v2 connector. Origins are injectable only for deterministic mock
/// tests; production construction always uses X's documented HTTPS hosts.
pub struct X {
    http: Http,
    site: Site,
    api_origin: String,
    authorize_endpoint: String,
    token_endpoint: String,
}

impl X {
    pub fn new() -> Result<Self, Error> {
        Self::with_origins(API_ORIGIN, AUTHORIZE, TOKEN_ENDPOINT)
    }

    pub fn with_origins(
        api_origin: impl Into<String>,
        authorize_endpoint: impl Into<String>,
        token_endpoint: impl Into<String>,
    ) -> Result<Self, Error> {
        Ok(Self {
            http: Http::new()?,
            site: Site::new(SITE),
            api_origin: api_origin.into().trim_end_matches('/').to_string(),
            authorize_endpoint: authorize_endpoint.into(),
            token_endpoint: token_endpoint.into(),
        })
    }

    /// Register the publisher and the separate private-message facet. The
    /// latter is not a `Publisher` method, preventing a future social site
    /// from accidentally inheriting an X-specific DM power.
    pub fn connector(self) -> Connector {
        let this = Arc::new(self);
        Connector::from_publisher(this.clone()).x_direct_messages(this)
    }
}

#[async_trait]
impl Publisher for X {
    fn site(&self) -> &Site {
        &self.site
    }

    fn capabilities(&self) -> &[Capability] {
        // DMs are listed so capability discovery describes the compiled
        // surface, but Client policy still denies every DM by default.
        &[Capability::PublishText, Capability::SendDirectMessage]
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
        let Body::Text { text } = &intent.body else {
            return Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: "x_text_only".into(),
                limit: None,
            });
        };
        validate_text(text)?;
        let reply_to_id = parse_reply_to(&intent.params)?;
        let mut payload = json!({ "text": text });
        if let Some(reply_to_id) = reply_to_id {
            payload["reply"] = json!({ "in_reply_to_tweet_id": reply_to_id });
        }
        let value = self
            .send_json(
                self.http
                    .post(&format!("{}/2/tweets", self.api_origin))
                    .bearer_auth(access_token(creds)?)
                    .json(&payload),
                deadline,
            )
            .await?;
        let id = response_id(&value, "id")?;
        Ok(Outcome {
            site: self.site.clone(),
            // A post permalink is a convenience, not an assertion that the
            // username is immutable: `/i/web/status` is owner-independent.
            url: Some(format!("https://x.com/i/web/status/{id}")),
            id: Some(id),
            limits: None,
        })
    }

    async fn whoami(&self, _app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error> {
        let value = self
            .send_json(
                self.http
                    .get(&format!("{}/2/users/me", self.api_origin))
                    .bearer_auth(access_token(creds)?),
                Deadline::from_secs(30),
            )
            .await?;
        identity_from_value(&value)
    }

    async fn auth_start(&self, app: &AppConfig) -> Result<AuthStart, Error> {
        self.auth_start_with(app, &AuthStartOptions::default())
            .await
    }

    async fn auth_start_with(
        &self,
        app: &AppConfig,
        options: &AuthStartOptions,
    ) -> Result<AuthStart, Error> {
        let oauth = require_oauth(app)?;
        let wants_dm = requested_dm(options)?;
        let state = new_state()?;
        let verifier = new_verifier()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let scope = if wants_dm {
            format!("{POST_SCOPES} {DM_SCOPES}")
        } else {
            POST_SCOPES.to_string()
        };
        let query = form(&[
            ("response_type", "code"),
            ("client_id", oauth.client_id.as_str()),
            ("redirect_uri", oauth.redirect_uri.as_str()),
            ("scope", &scope),
            ("state", &state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        let separator = if self.authorize_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        Ok(AuthStart::Browser {
            authorize_url: format!("{}{}{}", self.authorize_endpoint, separator, query),
            state: state.clone(),
            pending_pkce: Some(OAuthPkceSession {
                state,
                code_verifier: verifier,
                expires_at: unix_now().saturating_add(PKCE_TTL_SECS),
            }),
        })
    }

    async fn auth_finish(&self, app: &AppConfig, reply: AuthReply) -> Result<AccountCreds, Error> {
        let oauth = require_oauth(app)?;
        let AuthReply::Pkce { code, verifier } = reply else {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "x_pkce_session_required".into(),
            });
        };
        let code = extract_code(&code)?;
        let token = self
            .token_request(&[
                ("grant_type", "authorization_code"),
                ("client_id", oauth.client_id.as_str()),
                ("redirect_uri", oauth.redirect_uri.as_str()),
                ("code", &code),
                ("code_verifier", &verifier),
            ])
            .await?;
        let identity = self
            .whoami_from_token(&token.access_token, Deadline::from_secs(30))
            .await?;
        Ok(creds_from_token(&token, &identity, None))
    }

    async fn refresh(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        _deadline: Deadline,
    ) -> Result<AccountCreds, Error> {
        let oauth = require_oauth(app)?;
        let AccountCreds::OAuth2 {
            refresh_token,
            extra,
            ..
        } = creds
        else {
            return Err(wrong_cred_kind());
        };
        let old_refresh = refresh_token
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::Auth {
                site: Site::new(SITE),
                reason: "no_refresh".into(),
            })?;
        let mut token = self
            .token_request(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", old_refresh),
                ("client_id", oauth.client_id.as_str()),
            ])
            .await?;
        // X can rotate a refresh token, but an omitted value is not a
        // command to erase the usable old one.
        if token.refresh_token.is_none() {
            token.refresh_token = Some(old_refresh.to_string());
        }
        let identity = stored_identity(extra)?;
        Ok(creds_from_token(&token, &identity, Some(extra)))
    }
}

#[async_trait]
impl XDirectMessages for X {
    async fn send_x_direct_message(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        request: &XDirectMessageRequest,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        request.validate().map_err(|reason| Error::InvalidPost {
            site: self.site.clone(),
            reason,
            limit: None,
        })?;
        let value = self
            .send_json(
                self.http
                    .post(&format!(
                        "{}/2/dm_conversations/with/{}/messages",
                        self.api_origin, request.recipient_id
                    ))
                    .bearer_auth(access_token(creds)?)
                    .json(&json!({ "text": request.text })),
                deadline,
            )
            .await?;
        let id = response_id(&value, "dm_event_id")?;
        Ok(Outcome {
            site: self.site.clone(),
            // DM event IDs are not public resources. Inventing a URL would
            // leak a private identifier into browser history without a valid
            // destination, so this remains intentionally absent.
            url: None,
            id: Some(id),
            limits: None,
        })
    }
}

impl X {
    async fn whoami_from_token(&self, token: &str, deadline: Deadline) -> Result<Identity, Error> {
        let value = self
            .send_json(
                self.http
                    .get(&format!("{}/2/users/me", self.api_origin))
                    .bearer_auth(token),
                deadline,
            )
            .await?;
        let identity = identity_from_value(&value)?;
        Ok(Identity {
            id: identity.id,
            username: identity.handle,
        })
    }

    async fn token_request(&self, pairs: &[(&str, &str)]) -> Result<Token, Error> {
        let response = self
            .http
            .send(
                self.http
                    .post(&self.token_endpoint)
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(form(pairs)),
                Deadline::from_secs(30),
                &self.site,
            )
            .await?;
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|_| Error::request_failed(&self.site))?;
        if !(200..300).contains(&status) {
            // Token endpoints frequently return a descriptive `error` field;
            // do not echo it because proxies and developers can reflect the
            // authorization code or verifier in those bodies.
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "token_exchange_failed".into(),
            });
        }
        token_from_value(&serde_json::from_str(&body).map_err(|_| Error::Platform {
            site: self.site.clone(),
            code: "bad_token_json".into(),
            message: "X token endpoint returned invalid JSON".into(),
        })?)
    }

    async fn send_json(
        &self,
        request: reqwest::RequestBuilder,
        deadline: Deadline,
    ) -> Result<Value, Error> {
        let response = self.http.send(request, deadline, &self.site).await?;
        let status = response.status().as_u16();
        let reset = response
            .headers()
            .get("x-rate-limit-reset")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let body = response
            .text()
            .await
            .map_err(|_| Error::request_failed(&self.site))?;
        if !(200..300).contains(&status) {
            return Err(map_x_error(status, reset, &body));
        }
        serde_json::from_str(&body).map_err(|_| Error::Platform {
            site: self.site.clone(),
            code: "bad_json".into(),
            message: "X API returned invalid JSON".into(),
        })
    }
}

/// Only the public `--reply-to` sugar is admitted. Quote posts, polls,
/// scheduling, audience controls and arbitrary API fields each carry their
/// own product/permission semantics and are intentionally not smuggled in.
fn parse_reply_to(params: &Value) -> Result<Option<String>, Error> {
    match params {
        Value::Null => Ok(None),
        Value::Object(object) if object.is_empty() => Ok(None),
        Value::Object(object) if object.len() == 1 && object.contains_key("reply_to_id") => {
            let id = object
                .get("reply_to_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if valid_x_id(id) {
                Ok(Some(id.to_string()))
            } else {
                Err(Error::InvalidPost {
                    site: Site::new(SITE),
                    reason: "x_reply_to_id_invalid".into(),
                    limit: None,
                })
            }
        }
        Value::Object(object) => Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: format!(
                "unsupported_param:{}",
                object.keys().next().expect("non-empty object has a key")
            ),
            limit: None,
        }),
        _ => Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "params_not_object".into(),
            limit: None,
        }),
    }
}

fn validate_text(text: &str) -> Result<(), Error> {
    if text.trim().is_empty() {
        return Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "text_empty".into(),
            limit: None,
        });
    }
    // X's weighted character counting changes around URLs, emoji and CJK.
    // A local scalar-count cap would reject valid posts or accept invalid
    // ones, so v1 validates only the invariant empty case and lets X apply
    // its current canonical weighted-length policy.
    Ok(())
}

fn requested_dm(options: &AuthStartOptions) -> Result<bool, Error> {
    let mut direct_messages = false;
    for feature in &options.requested_features {
        if feature == "direct_messages" && !direct_messages {
            direct_messages = true;
        } else {
            return Err(Error::Auth {
                site: Site::new(SITE),
                reason: "unsupported_auth_feature".into(),
            });
        }
    }
    Ok(direct_messages)
}

fn new_verifier() -> Result<String, Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Auth {
        site: Site::new(SITE),
        reason: "os_rng".into(),
    })?;
    // 32 random bytes encode to 43 unpadded URL-safe characters: exactly
    // RFC 7636's minimum verifier length, with no characters that need form
    // escaping or could be treated as shell syntax.
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn require_oauth(app: &AppConfig) -> Result<&OAuthApp, Error> {
    app.oauth.as_ref().ok_or_else(|| Error::Auth {
        site: Site::new(SITE),
        reason: "missing_app_config".into(),
    })
}

fn access_token(creds: &AccountCreds) -> Result<&str, Error> {
    match creds {
        AccountCreds::OAuth2 { access_token, .. } if !access_token.is_empty() => Ok(access_token),
        AccountCreds::OAuth2 { .. } => Err(Error::Auth {
            site: Site::new(SITE),
            reason: "missing_access_token".into(),
        }),
        _ => Err(wrong_cred_kind()),
    }
}

fn wrong_cred_kind() -> Error {
    Error::Auth {
        site: Site::new(SITE),
        reason: "wrong_cred_kind".into(),
    }
}

#[derive(Clone, Debug)]
struct Token {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    scope: Option<String>,
}

#[derive(Clone, Debug)]
struct Identity {
    id: String,
    username: Option<String>,
}

fn token_from_value(value: &Value) -> Result<Token, Error> {
    let access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "token_missing_access_token".into(),
        })?;
    Ok(Token {
        access_token,
        refresh_token: value
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        expires_in: value.get("expires_in").and_then(Value::as_u64),
        scope: value
            .get("scope")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    })
}

fn identity_from_value(value: &Value) -> Result<WhoAmI, Error> {
    let data = value.get("data").unwrap_or(&Value::Null);
    let id = data
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| valid_x_id(value))
        .map(str::to_owned)
        .ok_or_else(|| Error::Platform {
            site: Site::new(SITE),
            code: "missing_user_id".into(),
            message: "X API returned no valid user ID".into(),
        })?;
    let handle = data
        .get("username")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
        .map(str::to_owned);
    Ok(WhoAmI {
        site: Site::new(SITE),
        id,
        handle,
    })
}

fn stored_identity(extra: &Value) -> Result<Identity, Error> {
    let id = extra
        .get("user_id")
        .and_then(Value::as_str)
        .filter(|value| valid_x_id(value))
        .map(str::to_owned)
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "missing_user_id".into(),
        })?;
    Ok(Identity {
        id,
        username: extra
            .get("username")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    })
}

fn creds_from_token(
    token: &Token,
    identity: &Identity,
    previous_extra: Option<&Value>,
) -> AccountCreds {
    let now = unix_now();
    let scope = token.scope.clone().or_else(|| {
        previous_extra
            .and_then(|extra| extra.get("scopes"))
            .and_then(Value::as_array)
            .map(|scopes| {
                scopes
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
    });
    let mut extra = serde_json::Map::new();
    extra.insert("user_id".into(), Value::String(identity.id.clone()));
    if let Some(username) = &identity.username {
        extra.insert("username".into(), Value::String(username.clone()));
    }
    if let Some(scope) = scope {
        extra.insert(
            "scopes".into(),
            Value::Array(
                scope
                    .split_whitespace()
                    .map(|scope| Value::String(scope.into()))
                    .collect(),
            ),
        );
    }
    extra.insert("refreshed_at".into(), Value::from(now));
    if let Some(expires_in) = token.expires_in {
        extra.insert(
            "expires_at".into(),
            Value::from(now.saturating_add(expires_in)),
        );
    }
    AccountCreds::OAuth2 {
        access_token: token.access_token.clone(),
        refresh_token: token.refresh_token.clone(),
        extra: Value::Object(extra),
    }
}

fn response_id(value: &Value, field: &str) -> Result<String, Error> {
    value
        .get("data")
        .and_then(|data| data.get(field))
        .and_then(Value::as_str)
        .filter(|value| valid_x_id(value))
        .map(str::to_owned)
        .ok_or_else(|| Error::Platform {
            site: Site::new(SITE),
            code: "missing_response_id".into(),
            message: "X API returned no valid identifier".into(),
        })
}

fn map_x_error(http_status: u16, reset: Option<u64>, body: &str) -> Error {
    let site = Site::new(SITE);
    if http_status == 401 {
        return Error::Auth {
            site,
            reason: "token_expired".into(),
        };
    }
    if http_status == 429 {
        let retry_after = reset.and_then(|reset| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|now| Duration::from_secs(reset.saturating_sub(now.as_secs())))
        });
        return Error::RateLimited { site, retry_after };
    }
    if http_status >= 500 {
        return Error::Network {
            site,
            message: format!("http_{http_status}"),
        };
    }
    // X's structured error `type` is a stable operator label. Never expose
    // `detail`: it can repeat caller text, recipient IDs, or authorization
    // context supplied by a reverse proxy.
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = value
        .get("errors")
        .and_then(Value::as_array)
        .and_then(|errors| errors.first())
        .and_then(|error| error.get("type"))
        .and_then(Value::as_str)
        .filter(|value| {
            value.len() <= 80
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
        .unwrap_or("request_rejected")
        .to_string();
    Error::Platform {
        site,
        code,
        message: "X API rejected the request".into(),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn app() -> AppConfig {
        AppConfig {
            site: Site::new(SITE),
            oauth: Some(OAuthApp {
                client_id: "client-id".into(),
                client_secret: "unused-for-pkce".into(),
                redirect_uri: "https://example.test/callback".into(),
            }),
            extra: Value::Null,
        }
    }

    fn creds() -> AccountCreds {
        AccountCreds::OAuth2 {
            access_token: "access-token".into(),
            refresh_token: None,
            extra: json!({}),
        }
    }

    #[tokio::test]
    async fn auth_start_uses_s256_and_dm_scope_is_explicit() {
        let connector = X::new().unwrap();
        let AuthStart::Browser {
            authorize_url,
            pending_pkce,
            ..
        } = connector.auth_start(&app()).await.unwrap()
        else {
            panic!("browser")
        };
        assert!(authorize_url.contains("code_challenge_method=S256"));
        assert!(authorize_url.contains("tweet.write"));
        assert!(!authorize_url.contains("dm.write"));
        let session = pending_pkce.expect("PKCE session");
        assert!(!authorize_url.contains(&session.code_verifier));

        let AuthStart::Browser { authorize_url, .. } = connector
            .auth_start_with(
                &app(),
                &AuthStartOptions {
                    requested_features: vec!["direct_messages".into()],
                },
            )
            .await
            .unwrap()
        else {
            panic!("browser")
        };
        assert!(authorize_url.contains("dm.write"));
    }

    #[tokio::test]
    async fn posts_text_and_returns_owner_independent_permalink() {
        let server = MockServer::start();
        let post = server.mock(|when, then| {
            when.method(POST)
                .path("/2/tweets")
                .header("authorization", "Bearer access-token")
                .json_body(json!({"text":"hello"}));
            then.status(201).json_body(json!({"data":{"id":"123"}}));
        });
        let connector =
            X::with_origins(server.base_url(), server.base_url(), server.base_url()).unwrap();
        let out = connector
            .publish(
                &app(),
                &creds(),
                Intent {
                    site: Site::new(SITE),
                    body: Body::Text {
                        text: "hello".into(),
                    },
                    params: Value::Null,
                    idempotency_key: None,
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(out.url.as_deref(), Some("https://x.com/i/web/status/123"));
        assert_eq!(post.hits(), 1);
    }

    #[tokio::test]
    async fn dm_uses_private_endpoint_and_never_invents_a_url() {
        let server = MockServer::start();
        let dm = server.mock(|when, then| {
            when.method(POST)
                .path("/2/dm_conversations/with/456/messages")
                .header("authorization", "Bearer access-token")
                .json_body(json!({"text":"private"}));
            then.status(201)
                .json_body(json!({"data":{"dm_event_id":"789"}}));
        });
        let connector =
            X::with_origins(server.base_url(), server.base_url(), server.base_url()).unwrap();
        let out = connector
            .send_x_direct_message(
                &app(),
                &creds(),
                &XDirectMessageRequest {
                    recipient_id: "456".into(),
                    text: "private".into(),
                    idempotency_key: "dm-1".into(),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(out.id.as_deref(), Some("789"));
        assert_eq!(out.url, None);
        assert_eq!(dm.hits(), 1);
    }

    #[tokio::test]
    async fn pkce_exchange_uses_verifier_and_resolves_the_authorized_user() {
        let server = MockServer::start();
        let token = server.mock(|when, then| {
            when.method(POST)
                .path("/2/oauth2/token")
                .header("content-type", "application/x-www-form-urlencoded")
                .body_contains("grant_type=authorization_code")
                .body_contains("client_id=client-id")
                .body_contains("code=code-1")
                .body_contains("code_verifier=known-verifier");
            then.status(200).json_body(json!({
                "access_token":"new-access-token",
                "refresh_token":"refresh-token",
                "expires_in":7200,
                "scope":"tweet.read tweet.write users.read offline.access"
            }));
        });
        let me = server.mock(|when, then| {
            when.method(GET)
                .path("/2/users/me")
                .header("authorization", "Bearer new-access-token");
            then.status(200)
                .json_body(json!({"data":{"id":"123","username":"wirekern"}}));
        });
        let connector = X::with_origins(
            server.base_url(),
            server.base_url(),
            format!("{}/2/oauth2/token", server.base_url()),
        )
        .unwrap();
        let creds = connector
            .auth_finish(
                &app(),
                AuthReply::Pkce {
                    code: "https://example.test/callback?code=code-1&state=state".into(),
                    verifier: "known-verifier".into(),
                },
            )
            .await
            .unwrap();
        token.assert();
        me.assert();
        let AccountCreds::OAuth2 { extra, .. } = creds else {
            panic!("OAuth credential")
        };
        assert_eq!(extra["user_id"], "123");
        assert_eq!(extra["username"], "wirekern");
    }

    #[test]
    fn rejection_does_not_echo_private_response_detail() {
        let err = map_x_error(
            400,
            None,
            r#"{"errors":[{"type":"invalid_request","detail":"recipient 456 said private text"}]}"#,
        );
        let rendered = err.to_string();
        assert!(rendered.contains("invalid_request"));
        assert!(!rendered.contains("private text"));
        assert!(!rendered.contains("recipient 456"));
    }
}
