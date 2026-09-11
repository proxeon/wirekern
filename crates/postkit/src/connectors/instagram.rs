//! Instagram organic image publishing through Instagram Login.
//!
//! This connector deliberately does not reuse Facebook Page credentials. The
//! Instagram Login grant identifies one professional Instagram account, whose
//! stored `user_id` is the only v1 publish target.

use crate::error::Error;
use crate::facets::MediaReader;
use crate::form::form;
use crate::http::Http;
use crate::media::{MediaQuery, MediaReply, PublishedMedia};
use crate::oauth::{authorize_url, exchange_code, extract_code, new_state};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::registry::Connector;
use crate::types::{
    AccountCreds, AppConfig, Body, Capability, Deadline, Image, Intent, OAuthApp, Outcome, Site,
    WhoAmI,
};
use async_trait::async_trait;
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
/// A carousel needs at least two slides to differ from the existing image
/// post, and Meta's current carousel container accepts at most ten items.
pub const MIN_CAROUSEL_IMAGES: usize = 2;
pub const MAX_CAROUSEL_IMAGES: usize = 10;
/// Meta fetches a public image asynchronously. One second is responsive
/// enough for a CLI while avoiding a hot loop against the status endpoint.
pub const DEFAULT_CONTAINER_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub struct Instagram {
    http: Http,
    site: Site,
    base: String,
    graph_origin: String,
    token_origin: String,
    container_poll_interval: Duration,
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
            container_poll_interval: DEFAULT_CONTAINER_POLL_INTERVAL,
        })
    }

    /// Test helper: production checks once per second, while deterministic
    /// status-transition tests need no wall-clock-second sleeps.
    pub fn with_container_poll_interval(mut self, interval: Duration) -> Self {
        self.container_poll_interval = interval;
        self
    }

    pub fn connector(self) -> Connector {
        let this = std::sync::Arc::new(self);
        Connector::from_publisher(this.clone()).media(this)
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
        &[
            Capability::PublishImage,
            Capability::PublishCarousel,
            Capability::ReadMedia,
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
        validate_params(&intent.params)?;
        match &intent.body {
            Body::Image { text, image, .. } => {
                let image_url = public_image_url(image)?;
                if let Some(caption) = text {
                    validate_caption(caption)?;
                }
                // V1 intentionally ignores generic `alt`: no verified
                // Instagram Login wire field is sent until that accessibility
                // contract is implemented and live-validated rather than
                // guessed.
                let token = access_token(creds)?;
                let user_id = stored_user_id(creds)?;
                post_image(
                    &self.http,
                    &self.base,
                    ImagePost {
                        access_token: token,
                        user_id: &user_id,
                        image_url,
                        caption: text.as_deref(),
                    },
                    deadline,
                    self.container_poll_interval,
                )
                .await
            }
            Body::Carousel { text, images } => {
                let image_urls = carousel_image_urls(images)?;
                if let Some(caption) = text {
                    validate_caption(caption)?;
                }
                let token = access_token(creds)?;
                let user_id = stored_user_id(creds)?;
                post_carousel(
                    &self.http,
                    &self.base,
                    CarouselPost {
                        access_token: token,
                        user_id: &user_id,
                        image_urls: &image_urls,
                        caption: text.as_deref(),
                    },
                    deadline,
                    self.container_poll_interval,
                )
                .await
            }
            // `Client` normally refuses this through capabilities first. The
            // connector is still safe when embedded and called directly.
            Body::Text { .. } => Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: "image_required".into(),
                limit: None,
            }),
        }
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

#[async_trait]
impl MediaReader for Instagram {
    async fn media(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        query: &MediaQuery,
        deadline: Deadline,
    ) -> Result<MediaReply, Error> {
        // The credential's resolved user ID is intentionally the only read
        // target too. A read command must not become a side door for probing
        // arbitrary Instagram accounts by ID.
        let user_id = stored_user_id(creds)?;
        list_published_media(
            &self.http,
            &self.base,
            access_token(creds)?,
            &user_id,
            query,
            deadline,
        )
        .await
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

/// Instagram Login asks Meta to crawl each supplied image URL. Keeping this
/// check shared by single-image and carousel paths guarantees Postkit never
/// grows an implicit local-upload, fetch, or image-hosting capability.
fn public_image_url(image: &Image) -> Result<&str, Error> {
    let Image::Url(image_url) = image else {
        return Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "image_source_unsupported:bytes".into(),
            limit: None,
        });
    };
    image.validate().map_err(|reason| Error::InvalidPost {
        site: Site::new(SITE),
        reason,
        limit: None,
    })?;
    Ok(image_url)
}

/// Return the exact ordered URLs that become Meta carousel children. The
/// count guard is deliberately before credential lookup and child creation:
/// a malformed batch must not leave even invisible remote containers behind.
fn carousel_image_urls(images: &[Image]) -> Result<Vec<&str>, Error> {
    if images.len() < MIN_CAROUSEL_IMAGES {
        return Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "carousel_too_few_images".into(),
            limit: Some(MIN_CAROUSEL_IMAGES as u32),
        });
    }
    if images.len() > MAX_CAROUSEL_IMAGES {
        return Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: "carousel_too_many_images".into(),
            limit: Some(MAX_CAROUSEL_IMAGES as u32),
        });
    }
    images.iter().map(public_image_url).collect()
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

/// A child has no caption of its own. The parent is the only carousel
/// container that carries visible copy; putting it here would invite Meta to
/// reject the batch or create an ambiguous per-slide contract.
pub fn carousel_item_form_pairs<'a>(
    image_url: &'a str,
    access_token: &'a str,
) -> Vec<(&'a str, &'a str)> {
    vec![
        ("image_url", image_url),
        ("is_carousel_item", "true"),
        ("access_token", access_token),
    ]
}

/// The parent owns the ordered children and optional post caption. `children`
/// is one comma-separated form value because that is Meta's carousel grammar,
/// not a JSON array or a series of repeated fields.
pub fn carousel_parent_form_pairs<'a>(
    children: &'a str,
    caption: Option<&'a str>,
    access_token: &'a str,
) -> Vec<(&'a str, &'a str)> {
    let mut pairs = vec![("media_type", "CAROUSEL"), ("children", children)];
    if let Some(caption) = caption {
        pairs.push(("caption", caption));
    }
    pairs.push(("access_token", access_token));
    pairs
}

/// Validated inputs for the two-step media publish. Grouping the fields keeps
/// the network primitive honest: adding a media property is a named contract
/// change rather than another positional argument to a sensitive write path.
struct ImagePost<'a> {
    access_token: &'a str,
    user_id: &'a str,
    image_url: &'a str,
    caption: Option<&'a str>,
}

/// Validated carousel inputs. URLs are borrowed from `Body::Carousel`, so
/// this primitive cannot invent another target or source while it constructs
/// child containers in their caller-specified order.
struct CarouselPost<'a> {
    access_token: &'a str,
    user_id: &'a str,
    image_urls: &'a [&'a str],
    caption: Option<&'a str>,
}

async fn post_image(
    http: &Http,
    base: &str,
    post: ImagePost<'_>,
    deadline: Deadline,
    poll_interval: Duration,
) -> Result<Outcome, Error> {
    let container = create_media_container(
        http,
        base,
        post.user_id,
        form(&image_form_pairs(
            post.image_url,
            post.caption,
            post.access_token,
        )),
        "media create",
        deadline,
    )
    .await?;

    // `POST /media` only asks Meta to fetch/process the public image. The
    // live API can legitimately reject an immediate publish with code 9007
    // (not ready), so status is a read-only readiness gate before the one
    // externally visible `media_publish` write.
    wait_for_container_ready(
        http,
        base,
        post.access_token,
        &container,
        deadline,
        poll_interval,
    )
    .await?;

    publish_ready_container(
        http,
        base,
        post.access_token,
        post.user_id,
        &container,
        deadline,
    )
    .await
}

/// Build a carousel without ever turning its input into independent posts.
/// Each child reaches `FINISHED` before the parent is created, so one failed
/// or expired URL stops further work before there is any visible publish.
async fn post_carousel(
    http: &Http,
    base: &str,
    post: CarouselPost<'_>,
    deadline: Deadline,
    poll_interval: Duration,
) -> Result<Outcome, Error> {
    let mut children = Vec::with_capacity(post.image_urls.len());
    for image_url in post.image_urls {
        let child = create_media_container(
            http,
            base,
            post.user_id,
            form(&carousel_item_form_pairs(image_url, post.access_token)),
            "carousel child create",
            deadline,
        )
        .await?;
        wait_for_container_ready(
            http,
            base,
            post.access_token,
            &child,
            deadline,
            poll_interval,
        )
        .await?;
        children.push(child);
    }

    // The comma join is internal only. Returning child IDs would let a caller
    // mistake invisible, expiring containers for independently published media.
    let children = children.join(",");
    let parent = create_media_container(
        http,
        base,
        post.user_id,
        form(&carousel_parent_form_pairs(
            &children,
            post.caption,
            post.access_token,
        )),
        "carousel parent create",
        deadline,
    )
    .await?;
    wait_for_container_ready(
        http,
        base,
        post.access_token,
        &parent,
        deadline,
        poll_interval,
    )
    .await?;

    publish_ready_container(
        http,
        base,
        post.access_token,
        post.user_id,
        &parent,
        deadline,
    )
    .await
}

/// Create an invisible media container with the caller's already-reviewed
/// form body. The action label is only for a missing-ID diagnosis; raw server
/// bodies remain inside `read_json`'s credential-safe error boundary.
async fn create_media_container(
    http: &Http,
    base: &str,
    user_id: &str,
    body: String,
    action: &str,
    deadline: Deadline,
) -> Result<String, Error> {
    let site = Site::new(SITE);
    let create = http
        .post(&format!("{}/{user_id}/media", base.trim_end_matches('/')))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body);
    let response = http.send(create, deadline, &site).await?;
    json_id(&read_json(response, &site).await?, &site, action)
}

/// The one irreversible write shared by single images and carousel parents.
/// It is intentionally called only after its container's read-only readiness
/// gate has returned `FINISHED`.
async fn publish_ready_container(
    http: &Http,
    base: &str,
    access_token: &str,
    user_id: &str,
    container: &str,
    deadline: Deadline,
) -> Result<Outcome, Error> {
    let site = Site::new(SITE);
    let publish_url = format!("{}/{}/media_publish", base.trim_end_matches('/'), user_id);
    let publish = http
        .post(&publish_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form(&[
            ("creation_id", container),
            ("access_token", access_token),
        ]));
    // Never retry this write automatically. A response lost after Meta
    // accepts it is ambiguous, and a retry could make a second visible post.
    let response = http.send(publish, deadline, &site).await?;
    let id = json_id(&read_json(response, &site).await?, &site, "media publish")?;
    // Publishing has already succeeded. A permalink GET is useful output but
    // not part of the write's truth: a timeout, permission rollout, or a
    // malformed optional reply must never report a confirmed visible post as
    // failed or tempt Client into retrying `media_publish`.
    let url = published_permalink(http, base, access_token, &id, deadline)
        .await
        .ok()
        .flatten();
    Ok(Outcome {
        site,
        id: Some(id),
        url,
        limits: None,
    })
}

/// Read the first page of a credential owner's media, with an explicit
/// provider limit and no cursor follow-up. Keeping this as one GET gives the
/// generic `read.media` capability a hard ceiling even if Meta adds pagination
/// links to a future response.
async fn list_published_media(
    http: &Http,
    base: &str,
    access_token: &str,
    user_id: &str,
    query: &MediaQuery,
    deadline: Deadline,
) -> Result<MediaReply, Error> {
    query.validate().map_err(|reason| Error::InvalidQuery {
        site: Site::new(SITE),
        reason,
    })?;
    let site = Site::new(SITE);
    let limit = query.limit.to_string();
    let request_query = form(&[
        ("fields", "id,permalink,caption,media_type,timestamp"),
        ("limit", limit.as_str()),
        ("access_token", access_token),
    ]);
    let response = http
        .send(
            http.get(&format!(
                "{}/{user_id}/media?{request_query}",
                base.trim_end_matches('/')
            )),
            deadline,
            &site,
        )
        .await?;
    let body = read_json(response, &site).await?;
    let entries = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_media_data".into(),
            message: "media list returned no data array".into(),
        })?;

    // Honor the public Postkit bound even if a remote regression ignores our
    // `limit` query parameter. Retaining order preserves Meta's recency order.
    let media = entries
        .iter()
        .take(query.limit.into())
        .map(|entry| parse_published_media(entry, &site))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MediaReply { site, media })
}

/// Fetch only the link for a newly confirmed media ID. This narrow helper is
/// intentionally not a public arbitrary-ID lookup: it is used solely to
/// enrich the outcome of the preceding publish.
async fn published_permalink(
    http: &Http,
    base: &str,
    access_token: &str,
    media_id: &str,
    deadline: Deadline,
) -> Result<Option<String>, Error> {
    let site = Site::new(SITE);
    let request_query = form(&[("fields", "permalink"), ("access_token", access_token)]);
    let response = http
        .send(
            http.get(&format!(
                "{}/{media_id}?{request_query}",
                base.trim_end_matches('/')
            )),
            deadline,
            &site,
        )
        .await?;
    let body = read_json(response, &site).await?;
    Ok(nonempty_value_string(body.get("permalink")))
}

fn parse_published_media(entry: &Value, site: &Site) -> Result<PublishedMedia, Error> {
    let id = nonempty_value_string(entry.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_media_id".into(),
        message: "media list returned an item without id".into(),
    })?;
    Ok(PublishedMedia {
        id,
        permalink: nonempty_value_string(entry.get("permalink")),
        caption: value_string(entry.get("caption")),
        media_type: nonempty_value_string(entry.get("media_type")),
        timestamp: nonempty_value_string(entry.get("timestamp")),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContainerStatus {
    Finished,
    InProgress,
    Error,
    Expired,
    Published,
}

impl ContainerStatus {
    fn parse(body: &Value, site: &Site) -> Result<Self, Error> {
        let raw = body
            .get("status_code")
            .and_then(Value::as_str)
            .filter(|status| !status.is_empty())
            .ok_or_else(|| Error::Platform {
                site: site.clone(),
                code: "missing_container_status".into(),
                message: "media container returned no status_code".into(),
            })?;
        match raw {
            "FINISHED" => Ok(Self::Finished),
            "IN_PROGRESS" => Ok(Self::InProgress),
            "ERROR" => Ok(Self::Error),
            "EXPIRED" => Ok(Self::Expired),
            "PUBLISHED" => Ok(Self::Published),
            // A newly introduced status is not evidence that publishing is
            // safe. Refuse rather than treating an unknown string as ready.
            _ => Err(Error::Platform {
                site: site.clone(),
                code: "unknown_container_status".into(),
                message: "media container returned an unsupported status".into(),
            }),
        }
    }
}

async fn wait_for_container_ready(
    http: &Http,
    base: &str,
    access_token: &str,
    container_id: &str,
    deadline: Deadline,
    poll_interval: Duration,
) -> Result<(), Error> {
    let site = Site::new(SITE);
    let query = form(&[("fields", "status_code"), ("access_token", access_token)]);
    let url = format!("{}/{}?{query}", base.trim_end_matches('/'), container_id);
    loop {
        deadline.check(&site)?;
        let response = http.send(http.get(&url), deadline, &site).await?;
        match ContainerStatus::parse(&read_json(response, &site).await?, &site)? {
            ContainerStatus::Finished => return Ok(()),
            ContainerStatus::InProgress => {
                let remaining = deadline.remaining();
                if remaining.is_zero() {
                    return Err(Error::DeadlineExceeded { site });
                }
                // A zero test interval must not turn an embedding caller
                // into a busy loop. One final sleep to the deadline keeps the
                // operation bounded and leaves no hidden retry schedule.
                let delay = if poll_interval.is_zero() {
                    remaining
                } else {
                    poll_interval.min(remaining)
                };
                tokio::time::sleep(delay).await;
            }
            ContainerStatus::Error => {
                return Err(Error::Platform {
                    site,
                    code: "container_error".into(),
                    message: "media container processing failed".into(),
                })
            }
            ContainerStatus::Expired => {
                return Err(Error::Platform {
                    site,
                    code: "container_expired".into(),
                    message: "media container expired before publication".into(),
                })
            }
            ContainerStatus::Published => {
                // This caller did not publish it, so a separate actor did.
                // There is no returned media ID to safely dedupe against;
                // never issue a second publish just because it is visible.
                return Err(Error::Platform {
                    site,
                    code: "container_already_published".into(),
                    message: "media container was already published".into(),
                });
            }
        }
    }
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

/// IDs and URL-like fields must not turn an empty provider string into a
/// usable target. Captions intentionally use `value_string` directly: an
/// empty caption is still distinct from a field Meta omitted.
fn nonempty_value_string(value: Option<&Value>) -> Option<String> {
    value_string(value).filter(|value| !value.is_empty())
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
    use std::io::{Read, Write};
    use std::net::TcpListener;

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

    fn carousel_intent(caption: Option<&str>) -> Intent {
        Intent {
            site: Site::new(SITE),
            params: json!({}),
            body: Body::Carousel {
                text: caption.map(str::to_owned),
                images: vec![
                    Image::Url("https://cdn.example.test/slide-1.jpg".into()),
                    Image::Url("https://cdn.example.test/slide-2.jpg".into()),
                ],
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
        let ready = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/container-1")
                .query_param("fields", "status_code")
                .query_param("access_token", "long-token");
            then.status(200)
                .json_body(json!({ "status_code": "FINISHED" }));
        });
        let publish = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/178900/media_publish")
                .body_contains("creation_id=container-1")
                .body_contains("access_token=long-token");
            then.status(200).json_body(json!({ "id": "media-1" }));
        });
        let permalink = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/media-1")
                .query_param("fields", "permalink")
                .query_param("access_token", "long-token");
            then.status(200)
                .json_body(json!({ "permalink": "https://www.instagram.com/p/media-1/" }));
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
        ready.assert();
        publish.assert();
        permalink.assert();
        assert_eq!(outcome.id.as_deref(), Some("media-1"));
        assert_eq!(
            outcome.url.as_deref(),
            Some("https://www.instagram.com/p/media-1/")
        );
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
        let ready = server.mock(|when, then| {
            when.method(GET).path("/v26.0/container-1");
            then.status(200)
                .json_body(json!({ "status_code": "FINISHED" }));
        });
        let publish = server.mock(|when, then| {
            when.method(POST).path("/v26.0/178900/media_publish");
            then.status(200).json_body(json!({ "id": "media-1" }));
        });
        let permalink = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/media-1")
                .query_param("fields", "permalink");
            then.status(200)
                .json_body(json!({ "permalink": "https://www.instagram.com/p/media-1/" }));
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
        ready.assert();
        publish.assert();
        permalink.assert();
    }

    #[tokio::test]
    async fn image_publish_waits_for_finished_before_the_visible_write() {
        // httpmock deliberately has no response sequence primitive. This
        // local five-request script proves the real order: create,
        // IN_PROGRESS read, FINISHED read, one visible publish, then the
        // strictly follow-up permalink read.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for expected in 0..5 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0u8; 1024];
                    let read = stream.read(&mut chunk).unwrap();
                    assert!(read > 0, "request ended before HTTP headers");
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let path = std::str::from_utf8(&request)
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .and_then(|target| target.split('?').next())
                    .unwrap();
                let body = match (expected, path) {
                    (0, "/v26.0/178900/media") => r#"{"id":"container-1"}"#,
                    (1, "/v26.0/container-1") => r#"{"status_code":"IN_PROGRESS"}"#,
                    (2, "/v26.0/container-1") => r#"{"status_code":"FINISHED"}"#,
                    (3, "/v26.0/178900/media_publish") => r#"{"id":"media-1"}"#,
                    (4, "/v26.0/media-1") => {
                        r#"{"permalink":"https://www.instagram.com/p/media-1/"}"#
                    }
                    other => panic!("unexpected request {other:?}"),
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let connector = Instagram::with_base(format!("http://{address}/v26.0"))
            .unwrap()
            .with_container_poll_interval(Duration::from_millis(1));
        let outcome = connector
            .publish(
                &app(),
                &creds(),
                image_intent(Some("Wait for Meta")),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(outcome.id.as_deref(), Some("media-1"));
        assert_eq!(
            outcome.url.as_deref(),
            Some("https://www.instagram.com/p/media-1/")
        );
    }

    #[tokio::test]
    async fn carousel_publishes_ready_children_then_one_ready_parent() {
        let server = MockServer::start();
        // Child forms use no caption: only the parent owns visible carousel
        // copy. The distinct URLs make the test prove caller order is kept.
        let child_one = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/178900/media")
                .body_contains("image_url=https%3A%2F%2Fcdn.example.test%2Fslide-1.jpg")
                .body_contains("is_carousel_item=true")
                .body_contains("access_token=long-token");
            then.status(200).json_body(json!({ "id": "child-1" }));
        });
        let child_one_ready = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/child-1")
                .query_param("fields", "status_code");
            then.status(200)
                .json_body(json!({ "status_code": "FINISHED" }));
        });
        let child_two = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/178900/media")
                .body_contains("image_url=https%3A%2F%2Fcdn.example.test%2Fslide-2.jpg")
                .body_contains("is_carousel_item=true")
                .body_contains("access_token=long-token");
            then.status(200).json_body(json!({ "id": "child-2" }));
        });
        let child_two_ready = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/child-2")
                .query_param("fields", "status_code");
            then.status(200)
                .json_body(json!({ "status_code": "FINISHED" }));
        });
        let parent = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/178900/media")
                .body_contains("media_type=CAROUSEL")
                .body_contains("children=child-1%2Cchild-2")
                .body_contains("caption=Carousel+caption")
                .body_contains("access_token=long-token");
            then.status(200).json_body(json!({ "id": "parent-1" }));
        });
        let parent_ready = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/parent-1")
                .query_param("fields", "status_code");
            then.status(200)
                .json_body(json!({ "status_code": "FINISHED" }));
        });
        let publish = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/178900/media_publish")
                .body_contains("creation_id=parent-1")
                .body_contains("access_token=long-token");
            then.status(200).json_body(json!({ "id": "carousel-1" }));
        });
        let permalink = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/carousel-1")
                .query_param("fields", "permalink");
            then.status(200)
                .json_body(json!({ "permalink": "https://www.instagram.com/p/carousel-1/" }));
        });
        let connector = Instagram::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let outcome = connector
            .publish(
                &app(),
                &creds(),
                carousel_intent(Some("Carousel caption")),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        child_one.assert();
        child_one_ready.assert();
        child_two.assert();
        child_two_ready.assert();
        parent.assert();
        parent_ready.assert();
        publish.assert();
        permalink.assert();
        assert_eq!(outcome.id.as_deref(), Some("carousel-1"));
        assert_eq!(
            outcome.url.as_deref(),
            Some("https://www.instagram.com/p/carousel-1/")
        );

        let child_form = form(&carousel_item_form_pairs(
            "https://cdn.example.test/slide-1.jpg",
            "long-token",
        ));
        assert!(child_form.contains("is_carousel_item=true"));
        assert!(!child_form.contains("caption="));
        let parent_form = form(&carousel_parent_form_pairs(
            "child-1,child-2",
            Some("Carousel caption"),
            "long-token",
        ));
        assert!(parent_form.contains("media_type=CAROUSEL"));
        assert!(parent_form.contains("children=child-1%2Cchild-2"));
    }

    #[tokio::test]
    async fn carousel_child_failure_refuses_parent_and_visible_publish() {
        let server = MockServer::start();
        let child = server.mock(|when, then| {
            when.method(POST).path("/v26.0/178900/media");
            then.status(200).json_body(json!({ "id": "child-1" }));
        });
        let failed_child = server.mock(|when, then| {
            when.method(GET).path("/v26.0/child-1");
            then.status(200)
                .json_body(json!({ "status_code": "ERROR" }));
        });
        let never_parent = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/178900/media")
                .body_contains("media_type=CAROUSEL");
            then.status(200).json_body(json!({ "id": "parent-1" }));
        });
        let never_publish = server.mock(|when, then| {
            when.method(POST).path("/v26.0/178900/media_publish");
            then.status(200).json_body(json!({ "id": "carousel-1" }));
        });
        let connector = Instagram::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let error = connector
            .publish(
                &app(),
                &creds(),
                carousel_intent(None),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        child.assert();
        failed_child.assert();
        never_parent.assert_hits(0);
        never_publish.assert_hits(0);
        assert!(matches!(error, Error::Platform { code, .. } if code == "container_error"));
    }

    #[tokio::test]
    async fn carousel_parent_failure_refuses_visible_publish() {
        let server = MockServer::start();
        let child_one = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/178900/media")
                .body_contains("slide-1.jpg");
            then.status(200).json_body(json!({ "id": "child-1" }));
        });
        let child_one_ready = server.mock(|when, then| {
            when.method(GET).path("/v26.0/child-1");
            then.status(200)
                .json_body(json!({ "status_code": "FINISHED" }));
        });
        let child_two = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/178900/media")
                .body_contains("slide-2.jpg");
            then.status(200).json_body(json!({ "id": "child-2" }));
        });
        let child_two_ready = server.mock(|when, then| {
            when.method(GET).path("/v26.0/child-2");
            then.status(200)
                .json_body(json!({ "status_code": "FINISHED" }));
        });
        let parent = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/178900/media")
                .body_contains("media_type=CAROUSEL");
            then.status(200).json_body(json!({ "id": "parent-1" }));
        });
        let failed_parent = server.mock(|when, then| {
            when.method(GET).path("/v26.0/parent-1");
            then.status(200)
                .json_body(json!({ "status_code": "ERROR" }));
        });
        let never_publish = server.mock(|when, then| {
            when.method(POST).path("/v26.0/178900/media_publish");
            then.status(200).json_body(json!({ "id": "carousel-1" }));
        });
        let connector = Instagram::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let error = connector
            .publish(
                &app(),
                &creds(),
                carousel_intent(None),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        child_one.assert();
        child_one_ready.assert();
        child_two.assert();
        child_two_ready.assert();
        parent.assert();
        failed_parent.assert();
        never_publish.assert_hits(0);
        assert!(matches!(error, Error::Platform { code, .. } if code == "container_error"));
    }

    #[tokio::test]
    async fn media_list_is_bounded_and_preserves_meta_order() {
        let server = MockServer::start();
        let list = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/178900/media")
                .query_param("fields", "id,permalink,caption,media_type,timestamp")
                .query_param("limit", "2")
                .query_param("access_token", "long-token");
            then.status(200).json_body(json!({
                "data": [
                    {
                        "id": "newest",
                        "permalink": "https://www.instagram.com/p/newest/",
                        "caption": "Recent labelled test",
                        "media_type": "IMAGE",
                        "timestamp": "2026-09-10T12:00:00+0000"
                    },
                    { "id": "older", "media_type": "CAROUSEL_ALBUM" },
                    { "id": "ignored-beyond-limit" }
                ]
            }));
        });
        let connector = Instagram::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let reply = connector
            .media(
                &app(),
                &creds(),
                &MediaQuery { limit: 2 },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        list.assert();
        assert_eq!(reply.site, Site::new(SITE));
        assert_eq!(
            reply
                .media
                .iter()
                .map(|media| media.id.as_str())
                .collect::<Vec<_>>(),
            vec!["newest", "older"]
        );
        assert_eq!(
            reply.media[0].permalink.as_deref(),
            Some("https://www.instagram.com/p/newest/")
        );
        assert_eq!(reply.media[1].caption, None);
        assert_eq!(reply.media[1].media_type.as_deref(), Some("CAROUSEL_ALBUM"));
    }

    #[tokio::test]
    async fn media_list_refuses_a_malformed_item_after_the_single_get() {
        let server = MockServer::start();
        let list = server.mock(|when, then| {
            when.method(GET).path("/v26.0/178900/media");
            then.status(200)
                .json_body(json!({ "data": [{ "caption": "no id" }] }));
        });
        let connector = Instagram::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let error = connector
            .media(
                &app(),
                &creds(),
                &MediaQuery { limit: 1 },
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        list.assert();
        assert!(matches!(error, Error::Platform { code, .. } if code == "missing_media_id"));
    }

    #[tokio::test]
    async fn media_list_validates_the_limit_before_http() {
        let server = MockServer::start();
        let no_get = server.mock(|when, then| {
            when.method(GET);
            then.status(500);
        });
        let connector = Instagram::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let error = connector
            .media(
                &app(),
                &creds(),
                &MediaQuery { limit: 0 },
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        no_get.assert_hits(0);
        assert!(
            matches!(error, Error::InvalidQuery { reason, .. } if reason == "media_limit_out_of_range")
        );
    }

    #[tokio::test]
    async fn confirmed_publish_stays_successful_if_permalink_lookup_fails() {
        let server = MockServer::start();
        let create = server.mock(|when, then| {
            when.method(POST).path("/v26.0/178900/media");
            then.status(200).json_body(json!({ "id": "container-1" }));
        });
        let ready = server.mock(|when, then| {
            when.method(GET).path("/v26.0/container-1");
            then.status(200)
                .json_body(json!({ "status_code": "FINISHED" }));
        });
        let publish = server.mock(|when, then| {
            when.method(POST).path("/v26.0/178900/media_publish");
            then.status(200).json_body(json!({ "id": "media-1" }));
        });
        let failed_permalink = server.mock(|when, then| {
            when.method(GET).path("/v26.0/media-1");
            then.status(500).body("gateway unavailable");
        });
        let connector = Instagram::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let outcome = connector
            .publish(
                &app(),
                &creds(),
                image_intent(Some("Confirmed despite lookup")),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        create.assert();
        ready.assert();
        publish.assert();
        failed_permalink.assert();
        assert_eq!(outcome.id.as_deref(), Some("media-1"));
        assert_eq!(outcome.url, None);
    }

    #[tokio::test]
    async fn terminal_container_error_refuses_to_publish() {
        let server = MockServer::start();
        let create = server.mock(|when, then| {
            when.method(POST).path("/v26.0/178900/media");
            then.status(200).json_body(json!({ "id": "container-1" }));
        });
        let status = server.mock(|when, then| {
            when.method(GET).path("/v26.0/container-1");
            then.status(200)
                .json_body(json!({ "status_code": "ERROR" }));
        });
        let never_publish = server.mock(|when, then| {
            when.method(POST).path("/v26.0/178900/media_publish");
            then.status(200).json_body(json!({ "id": "media-1" }));
        });
        let connector = Instagram::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let error = connector
            .publish(
                &app(),
                &creds(),
                image_intent(None),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        create.assert();
        status.assert();
        never_publish.assert_hits(0);
        assert!(matches!(error, Error::Platform { code, .. } if code == "container_error"));
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
    async fn invalid_carousels_fail_before_any_container_create() {
        let server = MockServer::start();
        let no_write = server.mock(|when, then| {
            when.method(POST);
            then.status(500);
        });
        let connector = Instagram::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let too_few = Intent {
            site: Site::new(SITE),
            params: json!({}),
            body: Body::Carousel {
                text: None,
                images: vec![Image::Url("https://cdn.example.test/only.jpg".into())],
            },
            idempotency_key: None,
        };
        let too_many = Intent {
            site: Site::new(SITE),
            params: json!({}),
            body: Body::Carousel {
                text: None,
                images: (0..MAX_CAROUSEL_IMAGES + 1)
                    .map(|number| Image::Url(format!("https://cdn.example.test/{number}.jpg")))
                    .collect(),
            },
            idempotency_key: None,
        };
        let byte_source = Intent {
            site: Site::new(SITE),
            params: json!({}),
            body: Body::Carousel {
                text: None,
                images: vec![
                    Image::Url("https://cdn.example.test/first.jpg".into()),
                    Image::Bytes {
                        filename: "second.jpg".into(),
                        bytes: vec![1],
                    },
                ],
            },
            idempotency_key: None,
        };
        for (intent, reason, limit) in [
            (
                too_few,
                "carousel_too_few_images",
                MIN_CAROUSEL_IMAGES as u32,
            ),
            (
                too_many,
                "carousel_too_many_images",
                MAX_CAROUSEL_IMAGES as u32,
            ),
            (byte_source, "image_source_unsupported:bytes", 0),
        ] {
            let error = connector
                .publish(&app(), &creds(), intent, Deadline::from_secs(30))
                .await
                .unwrap_err();
            assert!(
                matches!(error, Error::InvalidPost { reason: actual, limit: actual_limit, .. }
                    if actual == reason && (limit == 0 || actual_limit == Some(limit)))
            );
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
