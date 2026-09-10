//! Facebook Pages organic publishing through the Graph API.
//!
//! This is intentionally not part of `meta_ads`: a user token discovers
//! Pages, a transient Page token publishes to one selected Page, and neither
//! Page selection nor Page credentials belong to an advertising account.

use crate::error::Error;
use crate::form::form;
use crate::http::Http;
use crate::oauth::{authorize_url, exchange_code, extract_code, new_state};
use crate::pages::{PageAccount, PagesReply};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::types::{
    AccountCreds, AppConfig, Body, Capability, Deadline, Image, Intent, OAuthApp, Outcome, Site,
    WhoAmI,
};
use async_trait::async_trait;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub const GRAPH_HOST: &str = "graph.facebook.com";
/// Pinned deliberately, matching the supported Graph surface at release.
pub const GRAPH_VERSION: &str = "v26.0";
pub const GRAPH_ORIGIN: &str = "https://graph.facebook.com";
pub const AUTHORIZE: &str = "https://www.facebook.com/dialog/oauth";
pub const SITE: &str = "facebook_pages";
/// A Page publisher needs discovery, the write verb, and the read scope that
/// makes Page membership/task information available. New scope grants require
/// one new authorization; an older Meta Ads token is not silently upgraded.
pub const SCOPES: &str = "pages_show_list,pages_manage_posts,pages_read_engagement";

/// Graph cursors are server-provided URLs. Cap the sequence and require that
/// each next URL stays under the connector's own versioned Graph base, so a
/// malicious response cannot turn Page discovery into an arbitrary fetch.
const MAX_PAGES: usize = 50;

pub struct FacebookPages {
    http: Http,
    site: Site,
    base: String,
    graph_origin: String,
}

impl FacebookPages {
    pub fn new() -> Result<Self, Error> {
        Self::with_origins(
            format!("https://{GRAPH_HOST}/{GRAPH_VERSION}"),
            GRAPH_ORIGIN,
        )
    }

    /// Test helper for a local versioned Graph base.
    pub fn with_base(base: impl Into<String>) -> Result<Self, Error> {
        Self::with_origins(base, GRAPH_ORIGIN)
    }

    /// Test helper: Graph OAuth endpoints are unversioned, while Page calls
    /// use a pinned versioned base.
    pub fn with_origins(
        base: impl Into<String>,
        graph_origin: impl Into<String>,
    ) -> Result<Self, Error> {
        Ok(Self {
            http: Http::new()?,
            site: Site::new(SITE),
            base: base.into().trim_end_matches('/').to_string(),
            graph_origin: graph_origin.into().trim_end_matches('/').to_string(),
        })
    }
}

#[async_trait]
impl Publisher for FacebookPages {
    fn site(&self) -> &Site {
        &self.site
    }

    fn capabilities(&self) -> &[Capability] {
        &[
            Capability::ReadPages,
            Capability::PublishText,
            Capability::PublishImage,
        ]
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
        // Parse every caller-controlled field before even looking at the
        // vault credential. This makes invalid input a pure, no-network
        // refusal and prevents an accidental Page post to a default target.
        let params = FacebookPagesParams::parse(&intent.params)?;
        validate_body(&intent.body)?;
        let user_token = access_token(creds)?;
        let page = resolve_page(
            &self.http,
            &self.base,
            user_token,
            &params.page_id,
            deadline,
        )
        .await?;

        match &intent.body {
            Body::Text { text } => {
                publish_text(
                    &self.http,
                    &self.base,
                    &self.site,
                    &page.id,
                    &page.access_token,
                    text,
                    deadline,
                )
                .await
            }
            Body::Image { text, image, alt } => {
                publish_image(
                    &self.http,
                    &self.base,
                    &self.site,
                    &page.id,
                    &page.access_token,
                    text.as_deref(),
                    image,
                    alt,
                    deadline,
                )
                .await
            }
            // `validate_body` returns before this branch can be reached, but
            // spelling it out keeps a future validation refactor from
            // accidentally turning a carousel into multiple visible Photos.
            Body::Carousel { .. } => Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: "carousel_unsupported".into(),
                limit: None,
            }),
        }
    }

    async fn whoami(&self, _app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error> {
        let token = access_token(creds)?;
        whoami(&self.http, &self.base, token, Deadline::from_secs(30)).await
    }

    async fn pages(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<PagesReply, Error> {
        let token = access_token(creds)?;
        let pages = list_pages(&self.http, &self.base, token, deadline).await?;
        Ok(PagesReply {
            site: self.site.clone(),
            // The public reply deliberately crosses a one-way boundary:
            // internal Page tokens are discarded instead of serialised.
            pages: pages.into_iter().map(|page| page.public()).collect(),
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
        let short = exchange_code(
            &self.http,
            &format!("{}/oauth/access_token", self.graph_origin),
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
            &oauth.client_id,
            &oauth.client_secret,
            &short.access_token,
            deadline,
        )
        .await?;
        // Authentication identifies the user but does not require a Page
        // today. Page membership can be granted later, and discovery should
        // be the operation that reports it rather than making OAuth fail.
        let me = whoami(&self.http, &self.base, &long.access_token, deadline).await?;
        Ok(creds_from_long(&long, Some(me.id)))
    }

    async fn refresh(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<AccountCreds, Error> {
        let oauth = require_oauth(app)?;
        let long = long_lived(
            &self.http,
            &self.graph_origin,
            &oauth.client_id,
            &oauth.client_secret,
            access_token(creds)?,
            deadline,
        )
        .await?;
        Ok(creds_from_long(&long, extra_string(creds, "user_id")))
    }
}

struct FacebookPagesParams {
    page_id: String,
}

impl FacebookPagesParams {
    fn parse(params: &Value) -> Result<Self, Error> {
        let obj = params.as_object().ok_or_else(|| Error::InvalidPost {
            site: Site::new(SITE),
            reason: "params_must_be_object".into(),
            limit: None,
        })?;
        for key in obj.keys() {
            if key != "page_id" {
                return Err(Error::InvalidPost {
                    site: Site::new(SITE),
                    reason: format!("unsupported_param:{key}"),
                    limit: None,
                });
            }
        }
        let page_id = obj
            .get("page_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.chars().all(|ch| ch.is_ascii_digit()))
            .ok_or_else(|| Error::InvalidPost {
                site: Site::new(SITE),
                reason: "bad_page_id".into(),
                limit: None,
            })?;
        Ok(Self {
            page_id: page_id.to_string(),
        })
    }
}

fn validate_body(body: &Body) -> Result<(), Error> {
    match body {
        Body::Text { text } if text.trim().is_empty() => Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "text_empty".into(),
            limit: None,
        }),
        Body::Text { .. } => Ok(()),
        // A Page photo endpoint consumes multipart bytes. Treating a remote
        // URL as a local fetch would introduce an unreviewed SSRF/hosting
        // capability into the publishing kernel, even though the URL itself
        // is syntactically valid.
        Body::Image {
            image: Image::Url(_),
            ..
        } => Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "image_source_unsupported:url".into(),
            limit: None,
        }),
        Body::Image {
            image: image @ Image::Bytes { .. },
            ..
        } => image.validate().map_err(|reason| Error::InvalidPost {
            site: Site::new(SITE),
            reason,
            limit: None,
        }),
        // Pages' photos endpoint is intentionally one local multipart image
        // per post in v1. A caller must not get multiple independent public
        // Page posts just because it supplied a carousel body.
        Body::Carousel { .. } => Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "carousel_unsupported".into(),
            limit: None,
        }),
    }
}

struct PageWithToken {
    id: String,
    name: Option<String>,
    tasks: Vec<String>,
    // Never derive Debug/Serialize here: this value is a short-lived Page
    // bearer capability and must remain inside the connector request path.
    access_token: Option<String>,
}

impl PageWithToken {
    fn public(&self) -> PageAccount {
        PageAccount {
            id: self.id.clone(),
            name: self.name.clone(),
            tasks: self.tasks.clone(),
        }
    }
}

async fn resolve_page(
    http: &Http,
    base: &str,
    user_token: &str,
    page_id: &str,
    deadline: Deadline,
) -> Result<ResolvedPage, Error> {
    let page = list_pages(http, base, user_token, deadline)
        .await?
        .into_iter()
        .find(|page| page.id == page_id)
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "page_not_available".into(),
        })?;
    let access_token = page
        .access_token
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "page_access_token_missing".into(),
        })?;
    Ok(ResolvedPage {
        id: page.id,
        access_token,
    })
}

struct ResolvedPage {
    id: String,
    access_token: String,
}

async fn list_pages(
    http: &Http,
    base: &str,
    user_token: &str,
    deadline: Deadline,
) -> Result<Vec<PageWithToken>, Error> {
    let site = Site::new(SITE);
    let query = form(&[
        ("fields", "id,name,tasks,access_token"),
        ("limit", "100"),
        ("access_token", user_token),
    ]);
    let mut next = Some(format!("{base}/me/accounts?{query}"));
    let mut requests = 0usize;
    let mut pages = Vec::new();
    while let Some(url) = next {
        if !is_graph_cursor(base, &url) {
            return Err(Error::Platform {
                site: site.clone(),
                code: "invalid_page_cursor".into(),
                message: "Page discovery returned a cursor outside Graph".into(),
            });
        }
        deadline.check(&site)?;
        requests += 1;
        if requests > MAX_PAGES {
            return Err(Error::Platform {
                site: site.clone(),
                code: "paging_exceeded".into(),
                message: format!("Page discovery exceeded {MAX_PAGES} pages"),
            });
        }
        let response = http.send(http.get(&url), deadline, &site).await?;
        let body = read_json(response, &site).await?;
        if let Some(data) = body.get("data").and_then(Value::as_array) {
            for value in data {
                pages.push(page_from(value)?);
            }
        }
        next = body
            .get("paging")
            .and_then(|paging| paging.get("next"))
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    // Discovery output is designed for scripts. Stable sorting prevents an
    // unrelated Facebook ordering change from appearing as a state change.
    pages.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(pages)
}

fn is_graph_cursor(base: &str, url: &str) -> bool {
    url.starts_with(base) && url[base.len()..].starts_with('/')
}

fn page_from(value: &Value) -> Result<PageWithToken, Error> {
    let id = value_string(value.get("id"))
        .filter(|id| !id.is_empty() && id.chars().all(|character| character.is_ascii_digit()))
        .ok_or_else(|| Error::Platform {
            site: Site::new(SITE),
            code: "missing_page_id".into(),
            message: "Page discovery returned no valid id".into(),
        })?;
    let tasks = value
        .get("tasks")
        .and_then(Value::as_array)
        .map(|tasks| {
            tasks
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(PageWithToken {
        id,
        name: value_string(value.get("name")).filter(|name| !name.trim().is_empty()),
        tasks,
        access_token: value_string(value.get("access_token")),
    })
}

async fn publish_text(
    http: &Http,
    base: &str,
    site: &Site,
    page_id: &str,
    page_token: &str,
    text: &str,
    deadline: Deadline,
) -> Result<Outcome, Error> {
    // Form bodies keep the credential out of URLs, which reduces accidental
    // leakage via request logs, proxy diagnostics, or redirected locations.
    let body = form(&[("message", text), ("access_token", page_token)]);
    let response = http
        .send(
            http.post(&format!("{base}/{page_id}/feed"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body),
            deadline,
            site,
        )
        .await?;
    outcome_from(read_json(response, site).await?, site, "text post")
}

#[allow(clippy::too_many_arguments)]
async fn publish_image(
    http: &Http,
    base: &str,
    site: &Site,
    page_id: &str,
    page_token: &str,
    caption: Option<&str>,
    image: &Image,
    alt: &str,
    deadline: Deadline,
) -> Result<Outcome, Error> {
    let Image::Bytes { filename, bytes } = image else {
        // `validate_body` catches this before credentials/HTTP. Retain the
        // direct-call guard because Publisher is public and can be called
        // without Client's routing path.
        return Err(Error::InvalidPost {
            site: site.clone(),
            reason: "image_source_unsupported:url".into(),
            limit: None,
        });
    };
    let mut form = reqwest::multipart::Form::new()
        .part(
            "source",
            reqwest::multipart::Part::bytes(bytes.clone()).file_name(filename.clone()),
        )
        .text("published", "true")
        .text("access_token", page_token.to_string());
    if let Some(caption) = caption.filter(|caption| !caption.is_empty()) {
        form = form.text("caption", caption.to_string());
    }
    if !alt.is_empty() {
        form = form.text("alt_text_custom", alt.to_string());
    }
    let response = http
        .send(
            http.post(&format!("{base}/{page_id}/photos"))
                .multipart(form),
            deadline,
            site,
        )
        .await?;
    let body = read_json(response, site).await?;
    // The photo endpoint can return both fields; `post_id` identifies the
    // visible Page post while `id` can identify only the media object.
    let id = value_string(body.get("post_id"))
        .or_else(|| value_string(body.get("id")))
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_id".into(),
            message: "image post returned no id".into(),
        })?;
    Ok(Outcome {
        site: site.clone(),
        id: Some(id),
        url: None,
        limits: None,
    })
}

fn outcome_from(body: Value, site: &Site, action: &str) -> Result<Outcome, Error> {
    let id = value_string(body.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_id".into(),
        message: format!("{action} returned no id"),
    })?;
    Ok(Outcome {
        site: site.clone(),
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

fn extra_string(creds: &AccountCreds, key: &str) -> Option<String> {
    let AccountCreds::OAuth2 { extra, .. } = creds else {
        return None;
    };
    extra.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

struct TokenLong {
    access_token: String,
    expires_in: Option<u64>,
}

async fn long_lived(
    http: &Http,
    graph_origin: &str,
    client_id: &str,
    client_secret: &str,
    token: &str,
    deadline: Deadline,
) -> Result<TokenLong, Error> {
    let site = Site::new(SITE);
    let query = form(&[
        ("grant_type", "fb_exchange_token"),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("fb_exchange_token", token),
    ]);
    let response = http
        .send(
            http.get(&format!("{graph_origin}/oauth/access_token?{query}")),
            deadline,
            &site,
        )
        .await?;
    let body = read_json(response, &site).await?;
    Ok(TokenLong {
        access_token: body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Auth {
                site: site.clone(),
                reason: "missing_access_token".into(),
            })?
            .to_string(),
        expires_in: body.get("expires_in").and_then(Value::as_u64),
    })
}

fn creds_from_long(long: &TokenLong, user_id: Option<String>) -> AccountCreds {
    let mut extra = serde_json::Map::new();
    if let Some(user_id) = user_id {
        extra.insert("user_id".into(), Value::String(user_id));
    }
    extra.insert("refreshed_at".into(), Value::from(unix_now()));
    if let Some(expires_in) = long.expires_in {
        extra.insert(
            "expires_at".into(),
            Value::from(unix_now().saturating_add(expires_in)),
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
    let query = form(&[("fields", "id,name"), ("access_token", token)]);
    let response = http
        .send(http.get(&format!("{base}/me?{query}")), deadline, &site)
        .await?;
    let body = read_json(response, &site).await?;
    let id = value_string(body.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_id".into(),
        message: "whoami returned no id".into(),
    })?;
    Ok(WhoAmI {
        site,
        id,
        handle: value_string(body.get("name")),
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
    let text = response
        .text()
        .await
        .map_err(|_| Error::request_failed(site))?;
    if !status.is_success() {
        return Err(map_graph_error(status.as_u16(), &text));
    }
    let value: Value = serde_json::from_str(&text).map_err(|error| Error::Platform {
        site: site.clone(),
        code: "bad_json".into(),
        message: error.to_string(),
    })?;
    if value.get("error").is_some() {
        return Err(map_graph_error(status.as_u16(), &text));
    }
    Ok(value)
}

fn map_graph_error(status: u16, body: &str) -> Error {
    let site = Site::new(SITE);
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let error = value.get("error");
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    // Keep Meta's operator action (`error_user_msg`) when available, but
    // never fall back to an arbitrary raw response body: a malformed proxy
    // body must not become a future bearer-token disclosure in CLI output.
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
        .unwrap_or("Graph request failed");
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
    if status >= 500 && (error.is_none() || code == 0) {
        return Error::Network {
            site,
            message: if message == "Graph request failed" {
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

    fn creds() -> AccountCreds {
        AccountCreds::OAuth2 {
            access_token: "user-token".into(),
            refresh_token: None,
            extra: json!({ "user_id": "11" }),
        }
    }

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

    #[tokio::test]
    async fn auth_start_requests_only_page_scopes() {
        let connector = FacebookPages::new().unwrap();
        let AuthStart::Browser { authorize_url, .. } = connector.auth_start(&app()).await.unwrap()
        else {
            panic!("expected browser authorization");
        };
        assert_eq!(
            crate::oauth::query_param(&authorize_url, "scope").as_deref(),
            Some(SCOPES)
        );
        assert!(!SCOPES.split(',').any(|scope| scope.starts_with("ads_")));
    }

    #[tokio::test]
    async fn auth_finish_stores_only_the_long_lived_user_token() {
        let server = MockServer::start();
        let short = server.mock(|when, then| {
            when.method(POST)
                .path("/oauth/access_token")
                .body_contains("code=code-1");
            then.status(200)
                .json_body(json!({ "access_token": "short-token" }));
        });
        let long = server.mock(|when, then| {
            when.method(GET)
                .path("/oauth/access_token")
                .query_param("grant_type", "fb_exchange_token")
                .query_param("fb_exchange_token", "short-token");
            then.status(200)
                .json_body(json!({ "access_token": "long-user-token", "expires_in": 3600 }));
        });
        let me = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/me")
                .query_param("access_token", "long-user-token");
            then.status(200)
                .json_body(json!({ "id": "11", "name": "Person" }));
        });
        let connector =
            FacebookPages::with_origins(format!("{}/v26.0", server.base_url()), server.base_url())
                .unwrap();
        let stored = connector
            .auth_finish(
                &app(),
                AuthReply::Pasted {
                    code: "code-1".into(),
                },
            )
            .await
            .unwrap();
        short.assert();
        long.assert();
        me.assert();
        let json = serde_json::to_string(&stored).unwrap();
        assert!(json.contains("long-user-token"));
        assert!(!json.contains("page-token"));
        assert_eq!(extra_string(&stored, "user_id").as_deref(), Some("11"));
    }

    #[tokio::test]
    async fn discovery_paginates_sorts_and_never_serializes_page_tokens() {
        let server = MockServer::start();
        let next = format!("{}/v26.0/me/accounts?after=cursor", server.base_url());
        let first = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/me/accounts")
                .query_param("fields", "id,name,tasks,access_token")
                .query_param("access_token", "user-token");
            then.status(200).json_body(json!({
                "data": [{ "id": "20", "name": "Z Page", "tasks": ["ADVERTISE"], "access_token": "page-token-z" }],
                "paging": { "next": next }
            }));
        });
        let second = server.mock(|when, then| {
            when.method(GET).path("/v26.0/me/accounts").query_param("after", "cursor");
            then.status(200).json_body(json!({
                "data": [{ "id": "10", "name": "A Page", "tasks": ["CREATE_CONTENT"], "access_token": "page-token-a" }]
            }));
        });
        let connector = FacebookPages::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let reply = connector
            .pages(&app(), &creds(), Deadline::from_secs(30))
            .await
            .unwrap();
        first.assert();
        second.assert();
        assert_eq!(
            reply
                .pages
                .iter()
                .map(|page| page.id.as_str())
                .collect::<Vec<_>>(),
            vec!["10", "20"]
        );
        let public = serde_json::to_string(&reply).unwrap();
        assert!(!public.contains("page-token-a"));
        assert!(!public.contains("page-token-z"));
    }

    #[tokio::test]
    async fn text_publish_resolves_a_page_token_then_uses_it_only_in_the_post_body() {
        let server = MockServer::start();
        let pages = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/me/accounts")
                .query_param("access_token", "user-token");
            then.status(200).json_body(json!({
                "data": [{ "id": "123", "access_token": "page-token" }]
            }));
        });
        let post = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/123/feed")
                .body_contains("message=Hello+Page")
                .body_contains("access_token=page-token");
            then.status(200).json_body(json!({ "id": "123_456" }));
        });
        let connector = FacebookPages::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let outcome = connector
            .publish(
                &app(),
                &creds(),
                Intent {
                    site: Site::new(SITE),
                    params: json!({ "page_id": "123" }),
                    body: Body::Text {
                        text: "Hello Page".into(),
                    },
                    idempotency_key: None,
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        pages.assert();
        post.assert();
        assert_eq!(outcome.id.as_deref(), Some("123_456"));
    }

    #[tokio::test]
    async fn image_publish_is_multipart_and_prefers_visible_post_id() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/v26.0/me/accounts");
            then.status(200).json_body(json!({
                "data": [{ "id": "123", "access_token": "page-token" }]
            }));
        });
        let post = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/123/photos")
                .body_contains("name=\"source\"; filename=\"announcement.png\"")
                .body_contains("image-bytes")
                .body_contains("name=\"caption\"")
                .body_contains("Page caption")
                .body_contains("name=\"alt_text_custom\"")
                .body_contains("A test image")
                .body_contains("name=\"published\"")
                .body_contains("true")
                .body_contains("page-token");
            then.status(200)
                .json_body(json!({ "id": "media-1", "post_id": "123_789" }));
        });
        let connector = FacebookPages::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let outcome = connector
            .publish(
                &app(),
                &creds(),
                Intent {
                    site: Site::new(SITE),
                    params: json!({ "page_id": "123" }),
                    body: Body::Image {
                        text: Some("Page caption".into()),
                        image: Image::Bytes {
                            filename: "announcement.png".into(),
                            bytes: b"image-bytes".to_vec(),
                        },
                        alt: "A test image".into(),
                    },
                    idempotency_key: None,
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        post.assert();
        assert_eq!(outcome.id.as_deref(), Some("123_789"));
    }

    #[tokio::test]
    async fn invalid_publish_inputs_fail_before_any_page_lookup() {
        let server = MockServer::start();
        let no_lookup = server.mock(|when, then| {
            when.method(GET).path("/v26.0/me/accounts");
            then.status(500);
        });
        let connector = FacebookPages::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        for (params, body, reason) in [
            (json!({}), Body::Text { text: "x".into() }, "bad_page_id"),
            (
                json!({ "page_id": "12", "reply_to_id": "1" }),
                Body::Text { text: "x".into() },
                "unsupported_param:reply_to_id",
            ),
            (
                json!({ "page_id": "12" }),
                Body::Text { text: "   ".into() },
                "text_empty",
            ),
            (
                json!({ "page_id": "12" }),
                Body::Image {
                    text: None,
                    image: Image::Url("https://example.test/image.png".into()),
                    alt: String::new(),
                },
                "image_source_unsupported:url",
            ),
            (
                json!({ "page_id": "12" }),
                Body::Image {
                    text: None,
                    image: Image::Bytes {
                        filename: "empty.png".into(),
                        bytes: vec![],
                    },
                    alt: String::new(),
                },
                "image_file_empty",
            ),
        ] {
            let error = connector
                .publish(
                    &app(),
                    &creds(),
                    Intent {
                        site: Site::new(SITE),
                        params,
                        body,
                        idempotency_key: None,
                    },
                    Deadline::from_secs(30),
                )
                .await
                .unwrap_err();
            assert!(matches!(error, Error::InvalidPost { reason: actual, .. } if actual == reason));
        }
        // A local failure must not touch the mock server at all.
        no_lookup.assert_hits(0);
    }

    #[test]
    fn graph_error_prefers_operator_message_and_never_uses_raw_body() {
        let error = map_graph_error(
            400,
            r#"{"error":{"code":100,"message":"Invalid parameter","error_user_msg":"Choose a Page you can manage."}}"#,
        );
        assert!(
            matches!(error, Error::Platform { code, message, .. } if code == "100" && message == "Choose a Page you can manage.")
        );
        let secret = "page-token-must-not-leak";
        let error = map_graph_error(500, secret);
        assert!(!error.to_string().contains(secret));
    }

    #[test]
    fn external_cursors_are_refused() {
        assert!(is_graph_cursor(
            "https://graph.facebook.com/v26.0",
            "https://graph.facebook.com/v26.0/me/accounts?after=x"
        ));
        assert!(!is_graph_cursor(
            "https://graph.facebook.com/v26.0",
            "https://attacker.test/me/accounts"
        ));
    }
}
