//! Instagram organic image publishing through Instagram Login.
//!
//! This connector deliberately does not reuse Facebook Page credentials. The
//! Instagram Login grant identifies one professional Instagram account, whose
//! stored `user_id` is the only v1 publish target.

use crate::error::Error;
use crate::form::form;
use crate::http::Http;
use crate::oauth::{authorize_url, exchange_code, extract_code, new_state};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::types::{
    AccountCreds, AppConfig, Body, Capability, Deadline, Image, Intent, OAuthApp, Outcome, Site,
    WhoAmI,
};
use async_trait::async_trait;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub const GRAPH_HOST: &str = "graph.instagram.com";
/// Keep the Graph API version explicit. A version bump is a reviewed wire
/// change, not an incidental dependency update.
pub const GRAPH_VERSION: &str = "v26.0";
pub const GRAPH_ORIGIN: &str = "https://graph.instagram.com";
pub const TOKEN_ORIGIN: &str = "https://api.instagram.com";
pub const AUTHORIZE: &str = "https://www.instagram.com/oauth/authorize";
pub const SITE: &str = "instagram";
/// Instagram Login scopes for account identity plus feed-image publication.
/// Adding an unrelated permission is deliberately a separate auth change.
pub const SCOPES: &str = "instagram_business_basic,instagram_business_content_publish";
/// Instagram captions are limited to 2,200 characters. Rust's `chars()`
/// counts Unicode scalar values, avoiding a byte-based rejection of valid
/// non-ASCII captions.
pub const MAX_CAPTION: usize = 2_200;

pub struct Instagram {
    http: Http,
    site: Site,
    base: String,
    graph_origin: String,
    token_origin: String,
}

impl Instagram {
    pub fn new() -> Result<Self, Error> {
        Self::with_origins(
            format!("https://{GRAPH_HOST}/{GRAPH_VERSION}"),
            GRAPH_ORIGIN,
            TOKEN_ORIGIN,
        )
    }

    /// Test helper for a local versioned media API base.
    pub fn with_base(base: impl Into<String>) -> Result<Self, Error> {
        Self::with_origins(base, GRAPH_ORIGIN, TOKEN_ORIGIN)
    }

    /// Test helper: token exchange endpoints are not versioned while media
    /// endpoints are. Keeping both injectable makes exact wire tests local.
    pub fn with_origins(
        base: impl Into<String>,
        graph_origin: impl Into<String>,
        token_origin: impl Into<String>,
    ) -> Result<Self, Error> {
        Ok(Self {
            http: Http::new()?,
            site: Site::new(SITE),
            base: base.into().trim_end_matches('/').to_string(),
            graph_origin: graph_origin.into().trim_end_matches('/').to_string(),
            token_origin: token_origin.into().trim_end_matches('/').to_string(),
        })
    }
}

#[async_trait]
impl Publisher for Instagram {
    fn site(&self) -> &Site {
        &self.site
    }

    fn capabilities(&self) -> &[Capability] {
        // Feed publishing has no text-only media type. Advertising a text
        // capability here would make `post instagram --text` look valid even
        // though there is no truthful Graph request to send.
        &[Capability::PublishImage]
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
        validate_params(&intent.params)?;
        let Body::Image { text, image, .. } = &intent.body else {
            // `Client` normally refuses this through capabilities first. The
            // connector is still safe when embedded and called directly.
            return Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: "image_required".into(),
                limit: None,
            });
        };
        let Image::Url(image_url) = image else {
            // Instagram fetches `image_url` itself. Postkit never becomes an
            // image host or URL fetcher, which keeps SSRF-shaped I/O outside
            // the publishing kernel.
            return Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: "image_source_unsupported:bytes".into(),
                limit: None,
            });
        };
        image.validate().map_err(|reason| Error::InvalidPost {
            site: self.site.clone(),
            reason,
            limit: None,
        })?;
        if let Some(caption) = text {
            validate_caption(caption)?;
        }

        // V1 intentionally ignores generic `alt`: no verified Instagram
        // Login wire field is sent until that accessibility contract is
        // implemented and live-validated rather than guessed.
        let token = access_token(creds)?;
        let user_id = stored_user_id(creds)?;
        post_image(
            &self.http,
            &self.base,
            token,
            &user_id,
            image_url,
            text.as_deref(),
            deadline,
        )
        .await
    }

    async fn whoami(&self, _app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error> {
        whoami(
            &self.http,
            &self.base,
            access_token(creds)?,
            Deadline::from_secs(30),
        )
        .await
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
        let short = exchange_code(
            &self.http,
            &format!("{}/oauth/access_token", self.token_origin),
            &oauth.client_id,
            &oauth.client_secret,
            &oauth.redirect_uri,
            &extract_code(&raw)?,
            deadline,
            &self.site,
        )
        .await?;
        let long = long_lived(
            &self.http,
            &self.graph_origin,
            &oauth.client_secret,
            &short.access_token,
            deadline,
        )
        .await?;
        // Token exchange may return `user_id`, but `/me` is authoritative for
        // the long-lived token we are about to store and gives us its handle.
        let me = whoami(&self.http, &self.base, &long.access_token, deadline).await?;
        Ok(creds_from_token(&long, me.id))
    }

    async fn refresh(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<AccountCreds, Error> {
        let token = access_token(creds)?;
        let user_id = stored_user_id(creds)?;
        // Instagram's long-lived-token refresh does not require client
        // credentials. Preserve the known target instead of discovering it
        // again, so refresh remains one bounded token request.
        let query = form(&[("grant_type", "ig_refresh_token"), ("access_token", token)]);
        let response = self
            .http
            .send(
                self.http.get(&format!(
                    "{}/refresh_access_token?{query}",
                    self.graph_origin
                )),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        let refreshed = token_from_response(&body)?;
        Ok(creds_from_token(&refreshed, user_id))
    }
}

/// No runtime target parameter exists in this single-account connector. A
/// future multi-account surface must first add typed discovery and selection;
/// accepting a free-form ID now would turn a credential into a broad writer.
fn validate_params(params: &Value) -> Result<(), Error> {
    match params {
        Value::Null => Ok(()),
        Value::Object(object) if object.is_empty() => Ok(()),
        Value::Object(object) => {
            let key = object.keys().next().expect("non-empty object has key");
            Err(Error::InvalidPost {
                site: Site::new(SITE),
                reason: format!("unsupported_param:{key}"),
                limit: None,
            })
        }
        _ => Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "params_not_object".into(),
            limit: None,
        }),
    }
}

pub fn validate_caption(caption: &str) -> Result<(), Error> {
    if caption.trim().is_empty() {
        return Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "caption_empty".into(),
            limit: None,
        });
    }
    if caption.chars().count() > MAX_CAPTION {
        return Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "caption_too_long".into(),
            limit: Some(MAX_CAPTION as u32),
        });
    }
    Ok(())
}

/// The create-container body. `alt` is intentionally absent; see `publish`.
pub fn image_form_pairs<'a>(
    image_url: &'a str,
    caption: Option<&'a str>,
    access_token: &'a str,
) -> Vec<(&'a str, &'a str)> {
    let mut pairs = vec![("image_url", image_url)];
    if let Some(caption) = caption {
        pairs.push(("caption", caption));
    }
    pairs.push(("access_token", access_token));
    pairs
}

async fn post_image(
    http: &Http,
    base: &str,
    access_token: &str,
    user_id: &str,
    image_url: &str,
    caption: Option<&str>,
    deadline: Deadline,
) -> Result<Outcome, Error> {
    let site = Site::new(SITE);
    let create_url = format!("{}/{user_id}/media", base.trim_end_matches('/'));
    let create = http
        .post(&create_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form(&image_form_pairs(image_url, caption, access_token)));
    let response = http.send(create, deadline, &site).await?;
    let container = json_id(&read_json(response, &site).await?, &site, "media create")?;

    let publish_url = format!("{}/{user_id}/media_publish", base.trim_end_matches('/'));
    let publish = http
        .post(&publish_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form(&[
            ("creation_id", container.as_str()),
            ("access_token", access_token),
        ]));
    // Never retry this write automatically. A response lost after Meta
    // accepts it is ambiguous, and a retry could make a second visible post.
    let response = http.send(publish, deadline, &site).await?;
    let id = json_id(&read_json(response, &site).await?, &site, "media publish")?;
    Ok(Outcome {
        site,
        id: Some(id),
        url: None,
        limits: None,
    })
}

fn require_oauth(app: &AppConfig) -> Result<&OAuthApp, Error> {
    app.oauth.as_ref().ok_or_else(|| Error::Auth {
        site: Site::new(SITE),
        reason: "missing_app_config".into(),
    })
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

fn stored_user_id(creds: &AccountCreds) -> Result<String, Error> {
    let AccountCreds::OAuth2 { extra, .. } = creds else {
        return Err(Error::Auth {
            site: Site::new(SITE),
            reason: "wrong_cred_kind".into(),
        });
    };
    value_string(extra.get("user_id")).ok_or_else(|| Error::Auth {
        site: Site::new(SITE),
        reason: "missing_user_id".into(),
    })
}

#[derive(Clone)]
struct Token {
    access_token: String,
    expires_in: Option<u64>,
}

async fn long_lived(
    http: &Http,
    graph_origin: &str,
    client_secret: &str,
    short_token: &str,
    deadline: Deadline,
) -> Result<Token, Error> {
    let site = Site::new(SITE);
    let query = form(&[
        ("grant_type", "ig_exchange_token"),
        ("client_secret", client_secret),
        ("access_token", short_token),
    ]);
    let response = http
        .send(
            http.get(&format!("{graph_origin}/access_token?{query}")),
            deadline,
            &site,
        )
        .await?;
    token_from_response(&read_json(response, &site).await?)
}

fn token_from_response(body: &Value) -> Result<Token, Error> {
    let site = Site::new(SITE);
    let access_token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| Error::Auth {
            site: site.clone(),
            reason: "missing_access_token".into(),
        })?
        .to_string();
    Ok(Token {
        access_token,
        expires_in: body.get("expires_in").and_then(Value::as_u64),
    })
}

fn creds_from_token(token: &Token, user_id: String) -> AccountCreds {
    let now = unix_now();
    let mut extra = serde_json::Map::new();
    extra.insert("user_id".into(), Value::String(user_id));
    extra.insert("refreshed_at".into(), Value::from(now));
    if let Some(expires_in) = token.expires_in {
        extra.insert(
            "expires_at".into(),
            Value::from(now.saturating_add(expires_in)),
        );
    }
    AccountCreds::OAuth2 {
        access_token: token.access_token.clone(),
        refresh_token: None,
        extra: Value::Object(extra),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

async fn whoami(http: &Http, base: &str, token: &str, deadline: Deadline) -> Result<WhoAmI, Error> {
    let site = Site::new(SITE);
    let query = form(&[("fields", "user_id,username"), ("access_token", token)]);
    let response = http
        .send(
            http.get(&format!("{}/me?{query}", base.trim_end_matches('/'))),
            deadline,
            &site,
        )
        .await?;
    let body = read_json(response, &site).await?;
    // New Instagram Login replies name this field `user_id`. Accept `id` as
    // a compatibility fallback without changing which endpoint we call.
    let id = value_string(body.get("user_id"))
        .or_else(|| value_string(body.get("id")))
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_id".into(),
            message: "whoami returned no user_id".into(),
        })?;
    Ok(WhoAmI {
        site,
        id,
        handle: value_string(body.get("username")),
    })
}

fn json_id(body: &Value, site: &Site, action: &str) -> Result<String, Error> {
    value_string(body.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_id".into(),
        message: format!("{action} returned no id"),
    })
}

fn value_string(value: Option<&Value>) -> Option<String> {
    value.and_then(|value| {
        value
            .as_str()
            .map(str::to_owned)
            .or_else(|| value.as_u64().map(|number| number.to_string()))
            .or_else(|| value.as_i64().map(|number| number.to_string()))
    })
}

async fn read_json(response: reqwest::Response, site: &Site) -> Result<Value, Error> {
    let status = response.status();
    // A body-read failure may include credential-bearing request details in a
    // reqwest diagnostic, so retain only Postkit's generic network message.
    let text = response
        .text()
        .await
        .map_err(|_| Error::request_failed(site))?;
    if !status.is_success() {
        return Err(map_instagram_error(status.as_u16(), &text));
    }
    let value: Value = serde_json::from_str(&text).map_err(|error| Error::Platform {
        site: site.clone(),
        code: "bad_json".into(),
        message: error.to_string(),
    })?;
    if value.get("error").is_some() || value.get("error_message").is_some() {
        return Err(map_instagram_error(status.as_u16(), &text));
    }
    Ok(value)
}

fn map_instagram_error(status: u16, body: &str) -> Error {
    let site = Site::new(SITE);
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let error = value.get("error");
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_i64)
        .or_else(|| value.get("code").and_then(Value::as_i64))
        .unwrap_or(0);
    // Do not use `body` as a fallback. Gateway and proxy responses can echo
    // token-bearing URLs; Meta's structured user message is all operators
    // need to correct a normal request.
    let message = error
        .and_then(|error| error.get("error_user_msg"))
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .or_else(|| {
            error
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .or_else(|| value.get("error_message").and_then(Value::as_str))
        .unwrap_or("Instagram request failed");
    if code == 190 {
        return Error::Auth {
            site,
            reason: "token_expired".into(),
        };
    }
    if matches!(code, 4 | 17 | 32 | 613) {
        return Error::RateLimited {
            site,
            retry_after: None,
        };
    }
    if status >= 500 && code == 0 {
        return Error::Network {
            site,
            message: if message == "Instagram request failed" {
                format!("http_{status}")
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
            access_token: "long-token".into(),
            refresh_token: None,
            extra: json!({ "user_id": "178900" }),
        }
    }

    fn image_intent(caption: Option<&str>) -> Intent {
        Intent {
            site: Site::new(SITE),
            params: json!({}),
            body: Body::Image {
                text: caption.map(str::to_owned),
                image: Image::Url("https://cdn.example.test/photo.jpg".into()),
                alt: "A deliberately ignored v1 alt field".into(),
            },
            idempotency_key: None,
        }
    }

    #[tokio::test]
    async fn auth_start_requests_only_instagram_publish_scopes() {
        let connector = Instagram::new().unwrap();
        let AuthStart::Browser {
            authorize_url,
            state,
        } = connector.auth_start(&app()).await.unwrap()
        else {
            panic!("expected browser authorization");
        };
        assert!(authorize_url.starts_with(AUTHORIZE));
        assert_eq!(
            crate::oauth::query_param(&authorize_url, "scope").as_deref(),
            Some(SCOPES)
        );
        assert_eq!(
            crate::oauth::query_param(&authorize_url, "state").as_deref(),
            Some(state.as_str())
        );
        assert!(!SCOPES
            .split(',')
            .any(|scope| scope.starts_with("pages_") || scope.starts_with("ads_")));
    }

    #[tokio::test]
    async fn auth_finish_exchanges_and_stores_long_lived_token_and_user_id() {
        let server = MockServer::start();
        let short = server.mock(|when, then| {
            when.method(POST)
                .path("/oauth/access_token")
                .body_contains("client_id=client-id")
                .body_contains("code=code-1");
            then.status(200)
                .json_body(json!({ "access_token": "short-token" }));
        });
        let long = server.mock(|when, then| {
            when.method(GET)
                .path("/access_token")
                .query_param("grant_type", "ig_exchange_token")
                .query_param("client_secret", "client-secret")
                .query_param("access_token", "short-token");
            then.status(200)
                .json_body(json!({ "access_token": "long-token", "expires_in": 5_184_000 }));
        });
        let me = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/me")
                .query_param("fields", "user_id,username")
                .query_param("access_token", "long-token");
            then.status(200)
                .json_body(json!({ "user_id": "178900", "username": "postkit_test" }));
        });
        let connector = Instagram::with_origins(
            format!("{}/v26.0", server.base_url()),
            server.base_url(),
            server.base_url(),
        )
        .unwrap();
        let stored = connector
            .auth_finish(
                &app(),
                AuthReply::Redirect {
                    url: "https://example.test/callback?code=code-1&state=state".into(),
                },
            )
            .await
            .unwrap();
        short.assert();
        long.assert();
        me.assert();
        assert!(
            matches!(stored, AccountCreds::OAuth2 { ref access_token, ref refresh_token, .. } if access_token == "long-token" && refresh_token.is_none())
        );
        assert_eq!(stored_user_id(&stored).unwrap(), "178900");
    }

    #[tokio::test]
    async fn refresh_uses_ig_refresh_and_preserves_user_target() {
        let server = MockServer::start();
        let refresh = server.mock(|when, then| {
            when.method(GET)
                .path("/refresh_access_token")
                .query_param("grant_type", "ig_refresh_token")
                .query_param("access_token", "long-token");
            then.status(200)
                .json_body(json!({ "access_token": "renewed-token", "expires_in": 5_184_000 }));
        });
        let connector = Instagram::with_origins(
            format!("{}/v26.0", server.base_url()),
            server.base_url(),
            server.base_url(),
        )
        .unwrap();
        let refreshed = connector
            .refresh(&app(), &creds(), Deadline::from_secs(30))
            .await
            .unwrap();
        refresh.assert();
        assert!(
            matches!(refreshed, AccountCreds::OAuth2 { ref access_token, .. } if access_token == "renewed-token")
        );
        assert_eq!(stored_user_id(&refreshed).unwrap(), "178900");
    }

    #[tokio::test]
    async fn image_publish_posts_container_then_media_without_alt() {
        let server = MockServer::start();
        let create = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/178900/media")
                .body_contains("image_url=https%3A%2F%2Fcdn.example.test%2Fphoto.jpg")
                .body_contains("caption=Hello+Instagram")
                .body_contains("access_token=long-token");
            then.status(200).json_body(json!({ "id": "container-1" }));
        });
        let publish = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/178900/media_publish")
                .body_contains("creation_id=container-1")
                .body_contains("access_token=long-token");
            then.status(200).json_body(json!({ "id": "media-1" }));
        });
        let connector = Instagram::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let outcome = connector
            .publish(
                &app(),
                &creds(),
                image_intent(Some("Hello Instagram")),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        create.assert();
        publish.assert();
        assert_eq!(outcome.id.as_deref(), Some("media-1"));
        assert!(!form(&image_form_pairs(
            "https://cdn.example.test/photo.jpg",
            Some("Hello Instagram"),
            "long-token"
        ))
        .contains("alt"));
    }

    #[tokio::test]
    async fn absent_caption_is_omitted_from_container_form() {
        let server = MockServer::start();
        // Test the exact encoder input too: httpmock can assert that a body
        // contains a field, but deliberately has no negative body matcher.
        assert!(!form(&image_form_pairs(
            "https://cdn.example.test/photo.jpg",
            None,
            "long-token"
        ))
        .contains("caption="));
        let create = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/178900/media")
                .body_contains("image_url=https%3A%2F%2Fcdn.example.test%2Fphoto.jpg")
                .body_contains("access_token=long-token");
            then.status(200).json_body(json!({ "id": "container-1" }));
        });
        let publish = server.mock(|when, then| {
            when.method(POST).path("/v26.0/178900/media_publish");
            then.status(200).json_body(json!({ "id": "media-1" }));
        });
        let connector = Instagram::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        connector
            .publish(
                &app(),
                &creds(),
                image_intent(None),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        create.assert();
        publish.assert();
    }

    #[tokio::test]
    async fn invalid_inputs_fail_before_any_http() {
        let server = MockServer::start();
        let no_write = server.mock(|when, then| {
            when.method(POST);
            then.status(500);
        });
        let connector = Instagram::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let invalid = [
            (
                Intent {
                    site: Site::new(SITE),
                    params: json!({}),
                    body: Body::Text {
                        text: "not valid".into(),
                    },
                    idempotency_key: None,
                },
                "image_required",
            ),
            (
                Intent {
                    site: Site::new(SITE),
                    params: json!({}),
                    body: Body::Image {
                        text: None,
                        image: Image::Bytes {
                            filename: "photo.jpg".into(),
                            bytes: vec![1],
                        },
                        alt: String::new(),
                    },
                    idempotency_key: None,
                },
                "image_source_unsupported:bytes",
            ),
            (
                Intent {
                    site: Site::new(SITE),
                    params: json!({}),
                    body: Body::Image {
                        text: None,
                        image: Image::Url("http://cdn.example.test/photo.jpg".into()),
                        alt: String::new(),
                    },
                    idempotency_key: None,
                },
                "image_url_must_be_https",
            ),
            (
                Intent {
                    site: Site::new(SITE),
                    params: json!({ "user_id": "other" }),
                    body: image_intent(None).body,
                    idempotency_key: None,
                },
                "unsupported_param:user_id",
            ),
            (image_intent(Some("  ")), "caption_empty"),
            (
                image_intent(Some(&"a".repeat(MAX_CAPTION + 1))),
                "caption_too_long",
            ),
        ];
        for (intent, reason) in invalid {
            let error = connector
                .publish(&app(), &creds(), intent, Deadline::from_secs(30))
                .await
                .unwrap_err();
            assert!(matches!(error, Error::InvalidPost { reason: actual, .. } if actual == reason));
        }
        no_write.assert_hits(0);
    }

    #[tokio::test]
    async fn whoami_accepts_user_id_and_username() {
        let server = MockServer::start();
        let me = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/me")
                .query_param("fields", "user_id,username")
                .query_param("access_token", "long-token");
            then.status(200)
                .json_body(json!({ "user_id": 178900, "username": "postkit_test" }));
        });
        let connector = Instagram::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let me_out = connector.whoami(&app(), &creds()).await.unwrap();
        me.assert();
        assert_eq!(me_out.id, "178900");
        assert_eq!(me_out.handle.as_deref(), Some("postkit_test"));
    }

    #[test]
    fn error_mapping_prefers_operator_message_and_redacts_raw_body() {
        let error = map_instagram_error(
            400,
            r#"{"error":{"code":100,"message":"Invalid parameter","error_user_msg":"Use a professional Instagram account."}}"#,
        );
        assert!(
            matches!(error, Error::Platform { code, message, .. } if code == "100" && message == "Use a professional Instagram account.")
        );
        let secret = "long-token-must-not-leak";
        assert!(!map_instagram_error(500, secret)
            .to_string()
            .contains(secret));
    }

    #[test]
    fn caption_limit_counts_unicode_scalars_not_utf8_bytes() {
        assert!(validate_caption(&"界".repeat(MAX_CAPTION)).is_ok());
        assert!(
            matches!(validate_caption(&"界".repeat(MAX_CAPTION + 1)), Err(Error::InvalidPost { reason, limit: Some(limit), .. }) if reason == "caption_too_long" && limit == MAX_CAPTION as u32)
        );
    }
}
