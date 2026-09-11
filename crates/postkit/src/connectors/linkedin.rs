//! LinkedIn member text publishing through the versioned Posts API.
//!
//! This connector deliberately owns only one narrow capability: the
//! authenticated member publishes one public organic text post. LinkedIn's
//! organization, media, comments, analytics, and sponsored-content APIs need
//! distinct permissions and payload contracts, so none are represented as
//! generic `Intent.params` escape hatches here.

use crate::error::Error;
use crate::form::form;
use crate::http::Http;
use crate::oauth::{authorize_url, exchange_code, extract_code, new_state};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::types::{
    AccountCreds, AppConfig, Body, Capability, Deadline, Intent, OAuthApp, Outcome, Site, WhoAmI,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

pub const API_ORIGIN: &str = "https://api.linkedin.com";
pub const AUTHORIZE: &str = "https://www.linkedin.com/oauth/v2/authorization";
pub const TOKEN_ENDPOINT: &str = "https://www.linkedin.com/oauth/v2/accessToken";
const FEED_UPDATE_ORIGIN: &str = "https://www.linkedin.com/feed/update";
pub const SITE: &str = "linkedin";
/// Pin the documented Marketing API version. A version change is a reviewed
/// wire change: the header is asserted by mock tests rather than inherited
/// from a dependency or the calendar at runtime.
pub const LINKEDIN_VERSION: &str = "202608";
pub const RESTLI_PROTOCOL_VERSION: &str = "2.0.0";
/// `openid profile` is required to resolve the authenticated member through
/// OIDC UserInfo; `w_member_social` grants the one v1 write capability.
pub const SCOPES: &str = "openid profile w_member_social";
/// LinkedIn's Posts schema limits text commentary to 3,000 characters. Count
/// Unicode scalar values, not UTF-8 bytes, so valid non-ASCII commentary is
/// not rejected early merely because it has a larger byte representation.
pub const MAX_COMMENTARY: usize = 3_000;

pub struct LinkedIn {
    http: Http,
    site: Site,
    api_origin: String,
    token_endpoint: String,
}

impl LinkedIn {
    pub fn new() -> Result<Self, Error> {
        Self::with_origins(API_ORIGIN, TOKEN_ENDPOINT)
    }

    /// Test helper: LinkedIn uses a fixed API origin for both UserInfo and
    /// Posts, while OAuth code/refresh exchange uses a separate origin.
    /// Keeping both injectable makes the exact live wire contract testable
    /// locally without a fake DNS name or real credentials.
    pub fn with_origins(
        api_origin: impl Into<String>,
        token_endpoint: impl Into<String>,
    ) -> Result<Self, Error> {
        Ok(Self {
            http: Http::new()?,
            site: Site::new(SITE),
            api_origin: api_origin.into().trim_end_matches('/').to_string(),
            token_endpoint: token_endpoint.into(),
        })
    }
}

#[async_trait]
impl Publisher for LinkedIn {
    fn site(&self) -> &Site {
        &self.site
    }

    fn capabilities(&self) -> &[Capability] {
        // The Posts API can do much more, but advertising any of those
        // operations before their permission/media/review contracts exist
        // would let generic CLI input promise a post we cannot faithfully
        // construct.
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
        // Validate every caller-controlled field before loading a credential
        // or making a write. A malformed target must never turn into a post
        // authored by the default LinkedIn member.
        validate_params(&intent.params)?;
        let Body::Text { text } = &intent.body else {
            return Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: match intent.body {
                    Body::Image { .. } => "image_unsupported",
                    Body::Carousel { .. } => "carousel_unsupported",
                    Body::Text { .. } => unreachable!("matched above"),
                }
                .into(),
                limit: None,
            });
        };
        validate_text(text)?;
        let member_id = stored_member_id(creds)?;
        post_text(
            &self.http,
            &self.api_origin,
            access_token(creds)?,
            &member_id,
            text,
            deadline,
        )
        .await
    }

    async fn whoami(&self, _app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error> {
        // Read UserInfo instead of returning the stored ID blindly. This is
        // both a token validation step and the modern self-serve identity
        // source; legacy `/v2/me` needs profile permissions new apps should
        // not assume they have.
        let identity = userinfo(
            &self.http,
            &self.api_origin,
            access_token(creds)?,
            Deadline::from_secs(30),
        )
        .await?;
        Ok(WhoAmI {
            site: self.site.clone(),
            id: identity.member_id,
            handle: identity.name,
        })
    }

    async fn auth_start(&self, app: &AppConfig) -> Result<AuthStart, Error> {
        let oauth = require_oauth(app)?;
        let state = new_state()?;
        Ok(AuthStart::Browser {
            authorize_url: authorize_url(
                AUTHORIZE,
                &oauth.client_id,
                &oauth.redirect_uri,
                SCOPES,
                &state,
            ),
            state,
        })
    }

    async fn auth_finish(&self, app: &AppConfig, reply: AuthReply) -> Result<AccountCreds, Error> {
        let oauth = require_oauth(app)?;
        let raw = match reply {
            AuthReply::Pasted { code } | AuthReply::Redirect { url: code } => code,
            AuthReply::AppPassword { .. } => {
                return Err(Error::Auth {
                    site: self.site.clone(),
                    reason: "use_code".into(),
                })
            }
        };
        let deadline = Deadline::from_secs(30);
        let response = exchange_code(
            &self.http,
            &self.token_endpoint,
            &oauth.client_id,
            &oauth.client_secret,
            &oauth.redirect_uri,
            &extract_code(&raw)?,
            deadline,
            &self.site,
        )
        .await?;
        let token = token_from_response(&response.raw)?;
        let identity =
            userinfo(&self.http, &self.api_origin, &token.access_token, deadline).await?;
        Ok(creds_from_token(&token, &identity))
    }

    async fn refresh(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<AccountCreds, Error> {
        let oauth = require_oauth(app)?;
        let AccountCreds::OAuth2 {
            refresh_token,
            extra,
            ..
        } = creds
        else {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "wrong_cred_kind".into(),
            });
        };
        let refresh_token = refresh_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| Error::Auth {
                site: self.site.clone(),
                reason: "no_refresh".into(),
            })?;
        let request = self
            .http
            .post(&self.token_endpoint)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", oauth.client_id.as_str()),
                ("client_secret", oauth.client_secret.as_str()),
            ]));
        let response = self.http.send(request, deadline, &self.site).await?;
        let mut token = token_from_response(&read_json(response, &self.site).await?)?;
        // LinkedIn may omit refresh_token on a rotation response. Keep the
        // previous opaque token in that case; replacing it with None would
        // silently make the next proactive-refresh path impossible.
        if token.refresh_token.is_none() {
            token.refresh_token = Some(refresh_token.to_string());
        }
        let identity = Identity {
            member_id: stored_member_id(creds)?,
            name: extra.get("name").and_then(Value::as_str).map(str::to_owned),
        };
        Ok(creds_from_token(&token, &identity))
    }
}

/// LinkedIn v1 accepts no target parameter: the stored OAuth identity is the
/// only author. An arbitrary organization or author URN would be a different
/// permission and authority model, not a harmless caller convenience.
fn validate_params(params: &Value) -> Result<(), Error> {
    match params {
        Value::Null => Ok(()),
        Value::Object(object) if object.is_empty() => Ok(()),
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

pub fn validate_text(text: &str) -> Result<(), Error> {
    if text.trim().is_empty() {
        return Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "text_empty".into(),
            limit: None,
        });
    }
    if text.chars().count() > MAX_COMMENTARY {
        return Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "text_too_long".into(),
            limit: Some(MAX_COMMENTARY as u32),
        });
    }
    Ok(())
}

/// Closed payload for the current member-only, public organic write. Keeping
/// this a builder rather than caller-provided JSON prevents a general `post`
/// command from accidentally creating a targeted, dark, or organization post.
fn text_post_payload(member_id: &str, text: &str) -> Value {
    json!({
        "author": format!("urn:li:person:{member_id}"),
        "commentary": text,
        "visibility": "PUBLIC",
        "distribution": {
            "feedDistribution": "MAIN_FEED",
            "targetEntities": [],
            "thirdPartyDistributionChannels": [],
        },
        "lifecycleState": "PUBLISHED",
        "isReshareDisabledByAuthor": false,
    })
}

async fn post_text(
    http: &Http,
    api_origin: &str,
    access_token: &str,
    member_id: &str,
    text: &str,
    deadline: Deadline,
) -> Result<Outcome, Error> {
    let site = Site::new(SITE);
    let request = http
        .post(&format!("{}/rest/posts", api_origin.trim_end_matches('/')))
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .header("Linkedin-Version", LINKEDIN_VERSION)
        .header("X-Restli-Protocol-Version", RESTLI_PROTOCOL_VERSION)
        .json(&text_post_payload(member_id, text));
    // This is the only visible write. Do not retry it: a lost response after
    // LinkedIn accepted the post is ambiguous and another POST could create a
    // duplicate public update. Client idempotency only replays known success.
    let response = http.send(request, deadline, &site).await?;
    let id = post_id_from_response(response, &site).await?;
    Ok(Outcome {
        site,
        // LinkedIn returns the created post URN in `x-restli-id`. Its feed
        // route is deterministic for the two post-URN types LinkedIn emits,
        // so callers can open the accepted post without reverse-engineering a
        // link from the opaque identifier themselves.
        url: linkedin_feed_url(&id),
        id: Some(id),
        limits: None,
    })
}

fn linkedin_feed_url(post_urn: &str) -> Option<String> {
    // Do not construct a URL from an arbitrary response header. Restrict the
    // convenience URL to LinkedIn's documented post URN families and numeric
    // identifiers, leaving an unexpected future format safely URL-less.
    let numeric_id = post_urn
        .strip_prefix("urn:li:share:")
        .or_else(|| post_urn.strip_prefix("urn:li:ugcPost:"))?;
    if numeric_id.is_empty() || !numeric_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(format!("{FEED_UPDATE_ORIGIN}/{post_urn}/"))
}

async fn post_id_from_response(response: reqwest::Response, site: &Site) -> Result<String, Error> {
    let status = response.status();
    let id = response
        .headers()
        .get("x-restli-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    // Read the body even though the documented 201 is empty. It releases the
    // connection, and a non-201 structured body can still become a safe,
    // concise operator error instead of a raw diagnostic echo.
    let body = response
        .text()
        .await
        .map_err(|_| Error::request_failed(site))?;
    if status.as_u16() != 201 {
        return Err(map_linkedin_error(status.as_u16(), &body));
    }
    id.ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_post_id".into(),
        message: "LinkedIn post creation returned no x-restli-id header".into(),
    })
}

#[derive(Clone)]
struct Token {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

fn token_from_response(body: &Value) -> Result<Token, Error> {
    let access_token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "missing_access_token".into(),
        })?
        .to_owned();
    Ok(Token {
        access_token,
        refresh_token: body
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_owned),
        expires_in: body.get("expires_in").and_then(Value::as_u64),
    })
}

#[derive(Clone)]
struct Identity {
    member_id: String,
    name: Option<String>,
}

async fn userinfo(
    http: &Http,
    api_origin: &str,
    access_token: &str,
    deadline: Deadline,
) -> Result<Identity, Error> {
    let site = Site::new(SITE);
    let response = http
        .send(
            http.get(&format!("{}/v2/userinfo", api_origin.trim_end_matches('/')))
                .bearer_auth(access_token),
            deadline,
            &site,
        )
        .await?;
    let body = read_json(response, &site).await?;
    let member_id = body
        .get("sub")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty() && !id.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_member_id".into(),
            message: "LinkedIn UserInfo returned no sub".into(),
        })?;
    Ok(Identity {
        member_id,
        name: body
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .map(str::to_owned),
    })
}

fn creds_from_token(token: &Token, identity: &Identity) -> AccountCreds {
    let now = unix_now();
    let mut extra = serde_json::Map::new();
    extra.insert(
        "member_id".into(),
        Value::String(identity.member_id.clone()),
    );
    if let Some(name) = &identity.name {
        extra.insert("name".into(), Value::String(name.clone()));
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

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
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
        _ => Err(Error::Auth {
            site: Site::new(SITE),
            reason: "wrong_cred_kind".into(),
        }),
    }
}

fn stored_member_id(creds: &AccountCreds) -> Result<String, Error> {
    let AccountCreds::OAuth2 { extra, .. } = creds else {
        return Err(Error::Auth {
            site: Site::new(SITE),
            reason: "wrong_cred_kind".into(),
        });
    };
    // OAuth completion stores `member_id`. The generic `auth --token`
    // bootstrap predates LinkedIn and records every OAuth identity under
    // `user_id`; accept that one compatibility alias so a verified raw token
    // remains usable, while never allowing a runtime target override.
    extra
        .get("member_id")
        .or_else(|| extra.get("user_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty() && !id.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "missing_member_id".into(),
        })
}

async fn read_json(response: reqwest::Response, site: &Site) -> Result<Value, Error> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|_| Error::request_failed(site))?;
    if !status.is_success() {
        return Err(map_linkedin_error(status.as_u16(), &body));
    }
    serde_json::from_str(&body).map_err(|error| Error::Platform {
        site: site.clone(),
        code: "bad_json".into(),
        message: error.to_string(),
    })
}

/// LinkedIn errors use a small structured envelope (`serviceErrorCode`,
/// `message`, `status`). Preserve that explicit operator message when present
/// but never fall back to arbitrary raw text: proxies can reflect request data
/// and bearer credentials must not become diagnostics.
fn map_linkedin_error(http_status: u16, body: &str) -> Error {
    let site = Site::new(SITE);
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if http_status == 401 {
        return Error::Auth {
            site,
            reason: "token_expired".into(),
        };
    }
    if http_status == 429 {
        return Error::RateLimited {
            site,
            retry_after: None,
        };
    }
    if http_status >= 500 {
        return Error::Network {
            site,
            message: format!("http_{http_status}"),
        };
    }
    let code = value
        .get("serviceErrorCode")
        .and_then(Value::as_i64)
        .map(|code| code.to_string())
        .unwrap_or_else(|| format!("http_{http_status}"));
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("LinkedIn request failed");
    Error::Platform {
        site,
        code,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use serde_json::json;

    fn app() -> AppConfig {
        AppConfig {
            site: Site::new(SITE),
            oauth: Some(OAuthApp {
                client_id: "client-id".into(),
                client_secret: "client-secret".into(),
                redirect_uri: "https://example.test/callback".into(),
            }),
            extra: json!({}),
        }
    }

    fn creds() -> AccountCreds {
        AccountCreds::OAuth2 {
            access_token: "member-token".into(),
            refresh_token: Some("refresh-token".into()),
            extra: json!({ "member_id": "member-123", "name": "Akmal" }),
        }
    }

    fn text_intent(text: &str) -> Intent {
        Intent {
            site: Site::new(SITE),
            params: json!({}),
            body: Body::Text { text: text.into() },
            idempotency_key: None,
        }
    }

    #[tokio::test]
    async fn auth_start_requests_only_member_publish_and_oidc_scopes() {
        let connector = LinkedIn::new().unwrap();
        let AuthStart::Browser {
            authorize_url,
            state,
        } = connector.auth_start(&app()).await.unwrap()
        else {
            panic!("expected browser authorization");
        };
        assert_eq!(
            crate::oauth::query_param(&authorize_url, "scope").as_deref(),
            Some(SCOPES)
        );
        assert_eq!(
            crate::oauth::query_param(&authorize_url, "state").as_deref(),
            Some(state.as_str())
        );
        assert!(!SCOPES.split_whitespace().any(|scope| {
            matches!(
                scope,
                "r_liteprofile" | "w_organization_social" | "r_member_social"
            )
        }));
    }

    #[tokio::test]
    async fn auth_finish_exchanges_code_and_stores_oidc_member_identity() {
        let server = MockServer::start();
        let token = server.mock(|when, then| {
            when.method(POST)
                .path("/oauth/accessToken")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body_contains("grant_type=authorization_code")
                .body_contains("code=code-1")
                .body_contains("client_secret=client-secret");
            then.status(200).json_body(json!({
                "access_token": "member-token",
                "refresh_token": "refresh-token",
                "expires_in": 5_184_000,
            }));
        });
        let userinfo = server.mock(|when, then| {
            when.method(GET)
                .path("/v2/userinfo")
                .header("authorization", "Bearer member-token");
            then.status(200)
                .json_body(json!({ "sub": "member-123", "name": "Akmal" }));
        });
        let connector = LinkedIn::with_origins(
            server.base_url(),
            format!("{}/oauth/accessToken", server.base_url()),
        )
        .unwrap();
        let result = connector
            .auth_finish(
                &app(),
                AuthReply::Pasted {
                    code: "code-1".into(),
                },
            )
            .await
            .unwrap();
        token.assert();
        userinfo.assert();
        let AccountCreds::OAuth2 {
            access_token,
            refresh_token,
            extra,
        } = result
        else {
            panic!("expected OAuth credential");
        };
        assert_eq!(access_token, "member-token");
        assert_eq!(refresh_token.as_deref(), Some("refresh-token"));
        assert_eq!(extra["member_id"], "member-123");
        assert_eq!(extra["name"], "Akmal");
        assert!(extra["expires_at"].as_u64().is_some());
    }

    #[tokio::test]
    async fn post_uses_closed_current_member_payload_and_returns_post_url() {
        let server = MockServer::start();
        let post = server.mock(|when, then| {
            when.method(POST)
                .path("/rest/posts")
                .header("authorization", "Bearer member-token")
                .header("linkedin-version", LINKEDIN_VERSION)
                .header("x-restli-protocol-version", RESTLI_PROTOCOL_VERSION)
                .header("content-type", "application/json")
                .json_body(json!({
                    "author": "urn:li:person:member-123",
                    "commentary": "Hello LinkedIn",
                    "visibility": "PUBLIC",
                    "distribution": {
                        "feedDistribution": "MAIN_FEED",
                        "targetEntities": [],
                        "thirdPartyDistributionChannels": [],
                    },
                    "lifecycleState": "PUBLISHED",
                    "isReshareDisabledByAuthor": false,
                }));
            then.status(201)
                .header("x-restli-id", "urn:li:share:123")
                .body("");
        });
        let connector =
            LinkedIn::with_origins(server.base_url(), "https://unused.test/token").unwrap();
        let outcome = connector
            .publish(
                &app(),
                &creds(),
                text_intent("Hello LinkedIn"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        post.assert();
        assert_eq!(outcome.id.as_deref(), Some("urn:li:share:123"));
        assert_eq!(
            outcome.url.as_deref(),
            Some("https://www.linkedin.com/feed/update/urn:li:share:123/")
        );
    }

    #[test]
    fn feed_url_accepts_only_known_numeric_linkedin_post_urns() {
        assert_eq!(
            linkedin_feed_url("urn:li:share:7504108828771401729").as_deref(),
            Some("https://www.linkedin.com/feed/update/urn:li:share:7504108828771401729/")
        );
        assert_eq!(
            linkedin_feed_url("urn:li:ugcPost:7504108828771401729").as_deref(),
            Some("https://www.linkedin.com/feed/update/urn:li:ugcPost:7504108828771401729/")
        );
        assert!(linkedin_feed_url("urn:li:organization:123").is_none());
        assert!(linkedin_feed_url("urn:li:share:not-a-number").is_none());
    }

    #[tokio::test]
    async fn whoami_reads_current_oidc_identity_and_not_legacy_me() {
        let server = MockServer::start();
        let userinfo = server.mock(|when, then| {
            when.method(GET)
                .path("/v2/userinfo")
                .header("authorization", "Bearer member-token");
            then.status(200)
                .json_body(json!({ "sub": "member-now", "name": "Current Name" }));
        });
        let connector =
            LinkedIn::with_origins(server.base_url(), "https://unused.test/token").unwrap();
        let who = connector.whoami(&app(), &creds()).await.unwrap();
        userinfo.assert();
        assert_eq!(who.id, "member-now");
        assert_eq!(who.handle.as_deref(), Some("Current Name"));
    }

    #[tokio::test]
    async fn refresh_preserves_identity_and_refresh_token_when_rotation_omits_it() {
        let server = MockServer::start();
        let refresh = server.mock(|when, then| {
            when.method(POST)
                .path("/oauth/accessToken")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body_contains("grant_type=refresh_token")
                .body_contains("refresh_token=refresh-token")
                .body_contains("client_id=client-id")
                .body_contains("client_secret=client-secret");
            then.status(200).json_body(json!({
                "access_token": "renewed-token",
                "expires_in": 7_200,
            }));
        });
        let connector = LinkedIn::with_origins(
            "https://unused.test/api",
            format!("{}/oauth/accessToken", server.base_url()),
        )
        .unwrap();
        let refreshed = connector
            .refresh(&app(), &creds(), Deadline::from_secs(30))
            .await
            .unwrap();
        refresh.assert();
        let AccountCreds::OAuth2 {
            access_token,
            refresh_token,
            extra,
        } = refreshed
        else {
            panic!("expected OAuth credential");
        };
        assert_eq!(access_token, "renewed-token");
        assert_eq!(refresh_token.as_deref(), Some("refresh-token"));
        assert_eq!(extra["member_id"], "member-123");
        assert_eq!(extra["name"], "Akmal");
    }

    #[tokio::test]
    async fn invalid_input_refuses_before_the_visible_write() {
        let server = MockServer::start();
        let post = server.mock(|when, then| {
            when.method(POST).path("/rest/posts");
            then.status(201).header("x-restli-id", "must-not-exist");
        });
        let connector =
            LinkedIn::with_origins(server.base_url(), "https://unused.test/token").unwrap();
        let mut unknown_param = text_intent("valid text");
        unknown_param.params = json!({ "author": "urn:li:organization:forbidden" });
        let err = connector
            .publish(&app(), &creds(), unknown_param, Deadline::from_secs(30))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidPost { reason, .. } if reason == "unsupported_param:author")
        );

        let err = connector
            .publish(
                &app(),
                &creds(),
                text_intent(" \n "),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidPost { reason, .. } if reason == "text_empty"));

        let err = connector
            .publish(
                &app(),
                &creds(),
                Intent {
                    site: Site::new(SITE),
                    params: json!({}),
                    body: Body::Image {
                        text: None,
                        image: crate::types::Image::Url(
                            "https://cdn.example.test/image.png".into(),
                        ),
                        alt: String::new(),
                    },
                    idempotency_key: None,
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidPost { reason, .. } if reason == "image_unsupported"));
        post.assert_hits(0);
    }

    #[tokio::test]
    async fn missing_restli_id_is_not_reported_as_anonymous_success() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/rest/posts");
            then.status(201).body("");
        });
        let connector =
            LinkedIn::with_origins(server.base_url(), "https://unused.test/token").unwrap();
        let err = connector
            .publish(
                &app(),
                &creds(),
                text_intent("Hello"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Platform { code, .. } if code == "missing_post_id"));
    }

    #[test]
    fn validation_counts_unicode_scalars_and_raw_token_bootstrap_id_is_compatible() {
        assert!(validate_text(&"é".repeat(MAX_COMMENTARY)).is_ok());
        assert!(matches!(
            validate_text(&"x".repeat(MAX_COMMENTARY + 1)),
            Err(Error::InvalidPost { reason, limit: Some(limit), .. })
                if reason == "text_too_long" && limit == MAX_COMMENTARY as u32
        ));
        let bootstrap = AccountCreds::OAuth2 {
            access_token: "token".into(),
            refresh_token: None,
            extra: json!({ "user_id": "bootstrap-member" }),
        };
        assert_eq!(stored_member_id(&bootstrap).unwrap(), "bootstrap-member");
    }

    #[test]
    fn linked_in_errors_are_classified_and_never_echo_raw_body() {
        assert!(matches!(
            map_linkedin_error(401, r#"{"message":"expired"}"#),
            Error::Auth { reason, .. } if reason == "token_expired"
        ));
        assert!(matches!(
            map_linkedin_error(429, "over quota"),
            Error::RateLimited { .. }
        ));
        let err = map_linkedin_error(400, "token-must-not-leak");
        assert!(matches!(
            err,
            Error::Platform { ref code, ref message, .. }
                if code == "http_400" && message == "LinkedIn request failed"
        ));
        let err = map_linkedin_error(
            403,
            r#"{"serviceErrorCode":100,"message":"Not enough permissions"}"#,
        );
        assert!(matches!(
            err,
            Error::Platform { ref code, ref message, .. }
                if code == "100" && message == "Not enough permissions"
        ));
    }
}
