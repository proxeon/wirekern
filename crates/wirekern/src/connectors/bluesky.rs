use crate::error::Error;
use crate::http::Http;
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::types::{
    AccountCreds, AppConfig, Body, Capability, Deadline, Image, Intent, Outcome, Site, WhoAmI,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use unicode_segmentation::UnicodeSegmentation;

pub const SITE: &str = "bluesky";
pub const DEFAULT_PDS: &str = "https://bsky.social";
pub const MAX_GRAPHEMES: usize = 300;

/// Per-image blob cap from the `app.bsky.embed.images` lexicon
/// (`maxSize: 2000000` — raised from the former 1 MB). Enforced locally so
/// an oversized file dies as `invalid_post` exit 2 before any session or
/// upload, not as a lexicon rejection after the bytes crossed the wire.
pub const MAX_IMAGE_BYTES: usize = 2_000_000;

pub struct Bluesky {
    http: Http,
    site: Site,
    pds_override: Option<String>,
}

impl Bluesky {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            http: Http::new()?,
            site: Site::new(SITE),
            pds_override: None,
        })
    }

    /// Test helper: httpmock origin, e.g. `http://127.0.0.1:PORT`.
    pub fn with_pds(pds: impl Into<String>) -> Result<Self, Error> {
        Ok(Self {
            http: Http::new()?,
            site: Site::new(SITE),
            pds_override: Some(pds.into().trim_end_matches('/').to_string()),
        })
    }
}

#[async_trait]
impl Publisher for Bluesky {
    fn site(&self) -> &Site {
        &self.site
    }

    fn capabilities(&self) -> &[Capability] {
        &[Capability::PublishText, Capability::PublishImage]
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::AppPassword
    }

    async fn auth_start(&self, _app: &AppConfig) -> Result<AuthStart, Error> {
        Ok(AuthStart::PasteInstructions {
            hint: "app password (not account password); identifier is --account (handle)".into(),
        })
    }

    async fn auth_finish(&self, _app: &AppConfig, reply: AuthReply) -> Result<AccountCreds, Error> {
        let AuthReply::AppPassword {
            identifier,
            secret,
            pds,
        } = reply
        else {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "use_password".into(),
            });
        };
        let identifier = identifier.trim().to_string();
        if identifier.is_empty() || identifier == "default" {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "identifier_required".into(),
            });
        }
        let pds = pds
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_PDS.into());
        let pds = self
            .pds_override
            .clone()
            .unwrap_or_else(|| pds.trim_end_matches('/').to_string());
        // Prove the app password works; do not vault JWTs.
        let _ = create_session(
            &self.http,
            &pds,
            &identifier,
            &secret,
            Deadline::from_secs(30),
        )
        .await?;
        Ok(AccountCreds::AppPassword {
            identifier,
            secret,
            pds: Some(pds),
        })
    }

    async fn publish(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        intent: Intent,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        // Reject before any HTTP: an unsupported param must error here,
        // never become a published post of the wrong shape.
        let params = parse_params(&intent.params)?;
        let (pds, identifier, secret) = app_password(creds)?;
        let pds = self.pds_override.as_deref().unwrap_or(pds);
        match &intent.body {
            Body::Text { text } => {
                validate_text(text)?;
                let sess = create_session(&self.http, pds, identifier, secret, deadline).await?;
                // The reply's strongRefs need the session's token, so the
                // parent lookup happens after createSession — the malformed-
                // URI case inside it still fires before any write.
                let reply = match params.reply_to_id.as_deref() {
                    Some(uri) => Some(
                        resolve_reply_refs(&self.http, pds, &sess.access_jwt, uri, deadline)
                            .await?,
                    ),
                    None => None,
                };
                post_text(
                    &self.http,
                    pds,
                    &sess.access_jwt,
                    &sess.did,
                    Some(&sess.handle),
                    text,
                    reply.as_ref(),
                    deadline,
                )
                .await
            }
            Body::Image { text, image, alt } => {
                // Image replies stay refused at the connector too (the CLI
                // refuses earlier; this guards serve-mode callers): the
                // embed-with-reply wire is untaught, and a dropped reply
                // param would publish a root image post instead.
                if params.reply_to_id.is_some() {
                    return Err(Error::InvalidPost {
                        site: self.site.clone(),
                        reason: "image_reply_unsupported".into(),
                        limit: None,
                    });
                }
                // Bluesky uploads bytes as blobs; a URL source is refused
                // at the door — the kernel never fetches operator URLs
                // (plans/001/015 D1).
                let Image::Bytes { filename, bytes } = image else {
                    return Err(Error::InvalidPost {
                        site: self.site.clone(),
                        reason: "image_source_unsupported:url".into(),
                        limit: None,
                    });
                };
                image.validate().map_err(|reason| Error::InvalidPost {
                    site: self.site.clone(),
                    reason,
                    limit: None,
                })?;
                // All local checks before the session: mime from the closed
                // extension map (the uploadBlob Content-Type header must
                // name it; sniffing bytes is out), then the lexicon's 2 MB cap.
                let mime = image_mime(filename)?;
                if bytes.len() > MAX_IMAGE_BYTES {
                    return Err(Error::InvalidPost {
                        site: self.site.clone(),
                        reason: "image_too_large".into(),
                        limit: Some(MAX_IMAGE_BYTES as u32),
                    });
                }
                if let Some(caption) = text.as_deref() {
                    validate_text(caption)?;
                }
                let sess = create_session(&self.http, pds, identifier, secret, deadline).await?;
                let blob =
                    upload_blob(&self.http, pds, &sess.access_jwt, bytes, mime, deadline).await?;
                post_image_embed(
                    &self.http,
                    pds,
                    &sess.access_jwt,
                    &sess.did,
                    Some(&sess.handle),
                    text.as_deref(),
                    &blob,
                    alt,
                    deadline,
                )
                .await
            }
            // One image embed is the only reviewed Bluesky image shape.
            // Refuse a carousel explicitly instead of creating several
            // separate records or pretending a multi-image embed is known.
            Body::Carousel { .. } => Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: "carousel_unsupported".into(),
                limit: None,
            }),
        }
    }

    async fn whoami(&self, _app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error> {
        let (pds, identifier, secret) = app_password(creds)?;
        let pds = self.pds_override.as_deref().unwrap_or(pds);
        let sess =
            create_session(&self.http, pds, identifier, secret, Deadline::from_secs(30)).await?;
        Ok(WhoAmI {
            site: self.site.clone(),
            id: sess.did,
            handle: Some(sess.handle),
        })
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
    let n = text.graphemes(true).count();
    if n > MAX_GRAPHEMES {
        return Err(Error::InvalidPost {
            site,
            reason: "text_too_long".into(),
            limit: Some(MAX_GRAPHEMES as u32),
        });
    }
    Ok(())
}

/// Bluesky's only `Intent.params` key today is `reply_to_id`, holding the
/// parent post's `at://` URI (the same string `Outcome.id` returns, so a
/// reply can target a post this kernel just published). Unknown keys stay
/// errors: a dropped param is not neutral — `--param chat_id=…` silently
/// discarded is a post of the wrong shape reported as success, the worst
/// failure mode this kernel can have.
#[derive(Default)]
struct BlueskyParams {
    reply_to_id: Option<String>,
}

fn parse_params(params: &Value) -> Result<BlueskyParams, Error> {
    if params.is_null() {
        return Ok(BlueskyParams::default());
    }
    let object = params.as_object().ok_or_else(|| Error::InvalidPost {
        site: Site::new(SITE),
        reason: "params_not_object".into(),
        limit: None,
    })?;
    for key in object.keys() {
        if key != "reply_to_id" {
            return Err(Error::InvalidPost {
                site: Site::new(SITE),
                reason: format!("unsupported_param:{key}"),
                limit: None,
            });
        }
    }
    let reply_to_id = match object.get("reply_to_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.is_empty() => {
            // Shape-check here, not after the session: a malformed at-URI
            // dies before any HTTP at all, including createSession.
            parse_at_uri(s)?;
            Some(s.clone())
        }
        // An empty or non-string reply target would degrade to a root post
        // downstream; refuse it here, before credentials are touched.
        Some(_) => {
            return Err(Error::InvalidPost {
                site: Site::new(SITE),
                reason: "reply_to_id".into(),
                limit: None,
            });
        }
    };
    Ok(BlueskyParams { reply_to_id })
}

/// A `strongRef` (lexicon `com.atproto.repo.strongRef`): the `{uri, cid}`
/// pair every record reference carries. The cid is the part wirekern cannot
/// know from the at-URI alone, which is why replies resolve the parent via
/// `getRecord` before creating the reply.
pub struct StrongRef {
    pub uri: String,
    pub cid: String,
}

impl StrongRef {
    fn from_json(v: &Value) -> Result<Self, Error> {
        let uri = v
            .get("uri")
            .and_then(|x| x.as_str())
            .ok_or_else(|| Error::Platform {
                site: Site::new(SITE),
                code: "missing_uri".into(),
                message: "strongRef has no uri".into(),
            })?;
        let cid = v
            .get("cid")
            .and_then(|x| x.as_str())
            .ok_or_else(|| Error::Platform {
                site: Site::new(SITE),
                code: "missing_cid".into(),
                message: "strongRef has no cid".into(),
            })?;
        Ok(Self {
            uri: uri.to_string(),
            cid: cid.to_string(),
        })
    }

    fn to_json(&self) -> Value {
        json!({ "uri": self.uri, "cid": self.cid })
    }
}

/// `reply.root` + `reply.parent` as the post lexicon wants them: the thread
/// root (which a nested reply inherits from its parent — replying to a
/// reply must not start a new thread) and the immediate parent.
pub struct ReplyRefs {
    pub root: StrongRef,
    pub parent: StrongRef,
}

/// The three segments of an at-URI (`at://<authority>/<collection>/<rkey>`).
/// The authority may be a DID or a handle; `getRecord` accepts either.
struct AtUri {
    authority: String,
    collection: String,
    rkey: String,
}

fn parse_at_uri(uri: &str) -> Result<AtUri, Error> {
    let bad = || Error::InvalidPost {
        site: Site::new(SITE),
        reason: "reply_to_id".into(),
        limit: None,
    };
    let rest = uri.strip_prefix("at://").ok_or_else(bad)?;
    let mut parts = rest.split('/');
    let authority = parts.next().filter(|s| !s.is_empty()).ok_or_else(bad)?;
    let collection = parts.next().filter(|s| !s.is_empty()).ok_or_else(bad)?;
    let rkey = parts.next().filter(|s| !s.is_empty()).ok_or_else(bad)?;
    if parts.next().is_some() {
        return Err(bad());
    }
    // A reply's parent must be a post; pointing at another collection
    // (a like, a follow) would publish a malformed record the PDS rejects.
    if collection != "app.bsky.feed.post" {
        return Err(bad());
    }
    Ok(AtUri {
        authority: authority.to_string(),
        collection: collection.to_string(),
        rkey: rkey.to_string(),
    })
}

/// Resolve the parent of a reply: one `com.atproto.repo.getRecord` on the
/// session's PDS yields the parent's strongRef, and — if the parent is
/// itself a reply — the thread root it already carries, so a reply-to-reply
/// lands in the right thread with a single lookup. (Limitation: the PDS
/// only serves repos it hosts or caches; a parent on a foreign PDS that it
/// cannot resolve surfaces as a lookup failure rather than a wrong-thread
/// reply.)
async fn resolve_reply_refs(
    http: &Http,
    pds: &str,
    access_jwt: &str,
    parent_uri: &str,
    deadline: Deadline,
) -> Result<ReplyRefs, Error> {
    let site = Site::new(SITE);
    let at = parse_at_uri(parent_uri)?;
    let url = xrpc(pds, "com.atproto.repo.getRecord");
    let req = http
        .get(&url)
        .query(&[
            ("repo", at.authority.as_str()),
            ("collection", at.collection.as_str()),
            ("rkey", at.rkey.as_str()),
        ])
        .header("Authorization", format!("Bearer {access_jwt}"));
    let resp = http.send(req, deadline, &site).await?;
    let body = read_json(resp, &site).await?;
    // Trust the record's own uri over the operator's input: handles and
    // DIDs both work in getRecord, and the strongRef should carry the
    // canonical DID form the PDS returns.
    let parent = StrongRef {
        uri: body
            .get("uri")
            .and_then(|v| v.as_str())
            .unwrap_or(parent_uri)
            .to_string(),
        cid: body
            .get("cid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Platform {
                site: site.clone(),
                code: "missing_cid".into(),
                message: "getRecord returned no cid".into(),
            })?
            .to_string(),
    };
    let root = body
        .pointer("/value/reply/root")
        .map(StrongRef::from_json)
        .transpose()?
        .unwrap_or(StrongRef {
            uri: parent.uri.clone(),
            cid: parent.cid.clone(),
        });
    Ok(ReplyRefs { root, parent })
}

/// Closed extension → MIME map. The lexicon accepts any `image/*`, but the
/// uploadBlob request must name one content type in its `Content-Type`
/// header and wirekern does not sniff bytes; a conservative closed set turns
/// a typo'd extension into a local `invalid_post` instead of a server-side
/// guess. Extended deliberately, like every closed set here.
pub fn image_mime(filename: &str) -> Result<&'static str, Error> {
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Ok("image/png"),
        "jpg" | "jpeg" => Ok("image/jpeg"),
        "gif" => Ok("image/gif"),
        "webp" => Ok("image/webp"),
        _ => Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: format!("unsupported_image_type:{ext}"),
            limit: None,
        }),
    }
}

/// `com.atproto.repo.uploadBlob`: the body is the **raw** image bytes and
/// the `Content-Type` header names their type. The lexicon's input encoding
/// is `*/*` and the PDS records the request's Content-Type as the blob's
/// `mimeType` — wrapping the bytes in multipart (wirekern's first attempt)
/// uploads fine but poisons the blob with `multipart/form-data`, which
/// `createRecord` then rejects against the post lexicon's `image/*`
/// (live 2026-09-10: `Expected "image/*" (got "multipart/form-data") at
/// $.record.embed.images[0].image.mimeType`). Returns the platform's blob
/// object verbatim (`{"$type":"blob","ref":{"$link":…},"mimeType":…,
/// "size":…}`) — the embed references it untouched.
async fn upload_blob(
    http: &Http,
    pds: &str,
    access_jwt: &str,
    bytes: &[u8],
    mime: &str,
    deadline: Deadline,
) -> Result<Value, Error> {
    let site = Site::new(SITE);
    let url = xrpc(pds, "com.atproto.repo.uploadBlob");
    let req = http
        .post(&url)
        .header("Content-Type", mime)
        .header("Authorization", format!("Bearer {access_jwt}"))
        .body(bytes.to_vec());
    let resp = http.send(req, deadline, &site).await?;
    let body = read_json(resp, &site).await?;
    body.get("blob").cloned().ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_blob".into(),
        message: "uploadBlob returned no blob".into(),
    })
}

/// One image post: `createRecord` with an `app.bsky.embed.images` embed
/// referencing the uploaded blob. The lexicon requires `alt` (empty string
/// allowed); a caption obeys the 300-grapheme rule, and no caption sends
/// `text: ""` — the post *is* the image.
#[allow(clippy::too_many_arguments)] // one post's fixed wire inputs
pub async fn post_image_embed(
    http: &Http,
    pds: &str,
    access_jwt: &str,
    did: &str,
    handle: Option<&str>,
    text: Option<&str>,
    blob: &Value,
    alt: &str,
    deadline: Deadline,
) -> Result<Outcome, Error> {
    let site = Site::new(SITE);
    let created = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| Error::Platform {
            site: site.clone(),
            code: "time".into(),
            message: e.to_string(),
        })?;
    let url = xrpc(pds, "com.atproto.repo.createRecord");
    let body = json!({
        "repo": did,
        "collection": "app.bsky.feed.post",
        "record": {
            "$type": "app.bsky.feed.post",
            "text": text.unwrap_or(""),
            "createdAt": created,
            "embed": {
                "$type": "app.bsky.embed.images",
                "images": [
                    { "image": blob, "alt": alt }
                ]
            }
        }
    });
    let req = http
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {access_jwt}"))
        .body(body.to_string());
    let resp = http.send(req, deadline, &site).await?;
    let v = read_json(resp, &site).await?;
    let uri = v
        .get("uri")
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_uri".into(),
            message: "createRecord returned no uri".into(),
        })?;
    let url_out = handle.and_then(|h| public_url(h, uri));
    Ok(Outcome {
        site,
        id: Some(uri.to_string()),
        url: url_out,
        limits: None,
    })
}

#[allow(clippy::too_many_arguments)] // one post's fixed wire inputs
pub async fn post_text(
    http: &Http,
    pds: &str,
    access_jwt: &str,
    did: &str,
    handle: Option<&str>,
    text: &str,
    reply: Option<&ReplyRefs>,
    deadline: Deadline,
) -> Result<Outcome, Error> {
    let site = Site::new(SITE);
    let created = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| Error::Platform {
            site: site.clone(),
            code: "time".into(),
            message: e.to_string(),
        })?;
    let url = xrpc(pds, "com.atproto.repo.createRecord");
    // `reply` is only present on replies: root posts carry no reply field,
    // and a self-referential one would be an invalid record.
    let reply_json = reply.map(|r| {
        json!({
            "root": r.root.to_json(),
            "parent": r.parent.to_json(),
        })
    });
    let mut record = json!({
        "$type": "app.bsky.feed.post",
        "text": text,
        "createdAt": created,
    });
    if let Some(r) = reply_json {
        record["reply"] = r;
    }
    let body = json!({
        "repo": did,
        "collection": "app.bsky.feed.post",
        "record": record
    });
    let req = http
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {access_jwt}"))
        .body(body.to_string());
    let resp = http.send(req, deadline, &site).await?;
    let v = read_json(resp, &site).await?;
    let uri = v
        .get("uri")
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_uri".into(),
            message: "createRecord returned no uri".into(),
        })?;
    let url_out = handle.and_then(|h| public_url(h, uri));
    Ok(Outcome {
        site,
        id: Some(uri.to_string()),
        url: url_out,
        limits: None,
    })
}

struct Session {
    did: String,
    handle: String,
    access_jwt: String,
}

async fn create_session(
    http: &Http,
    pds: &str,
    identifier: &str,
    password: &str,
    deadline: Deadline,
) -> Result<Session, Error> {
    let site = Site::new(SITE);
    let url = xrpc(pds, "com.atproto.server.createSession");
    let body = json!({ "identifier": identifier, "password": password });
    let req = http
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body.to_string());
    let resp = http.send(req, deadline, &site).await?;
    let v = read_json(resp, &site).await?;
    let did = v
        .get("did")
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::Auth {
            site: site.clone(),
            reason: "missing_did".into(),
        })?
        .to_string();
    let handle = v
        .get("handle")
        .and_then(|x| x.as_str())
        .unwrap_or(identifier)
        .to_string();
    let access_jwt = v
        .get("accessJwt")
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::Auth {
            site: site.clone(),
            reason: "missing_jwt".into(),
        })?
        .to_string();
    Ok(Session {
        did,
        handle,
        access_jwt,
    })
}

fn app_password(creds: &AccountCreds) -> Result<(&str, &str, &str), Error> {
    match creds {
        AccountCreds::AppPassword {
            identifier,
            secret,
            pds,
        } => {
            let pds = pds
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_PDS);
            Ok((pds, identifier, secret))
        }
        _ => Err(Error::Auth {
            site: Site::new(SITE),
            reason: "wrong_cred_kind".into(),
        }),
    }
}

fn xrpc(pds: &str, nsid: &str) -> String {
    format!("{}/xrpc/{nsid}", pds.trim_end_matches('/'))
}

fn public_url(handle: &str, at_uri: &str) -> Option<String> {
    let rkey = at_uri.rsplit('/').next()?;
    Some(format!("https://bsky.app/profile/{handle}/post/{rkey}"))
}

async fn read_json(resp: reqwest::Response, site: &Site) -> Result<Value, Error> {
    let status = resp.status();
    // Keep this shared response-read pattern URL-safe. The PDS host is
    // caller supplied and future connectors may use URL credentials.
    let text = resp.text().await.map_err(|_| Error::request_failed(site))?;
    if !status.is_success() {
        return Err(map_xrpc_error(status.as_u16(), &text));
    }
    serde_json::from_str(&text).map_err(|e| Error::Platform {
        site: site.clone(),
        code: "bad_json".into(),
        message: e.to_string(),
    })
}

fn map_xrpc_error(http_status: u16, body: &str) -> Error {
    let site = Site::new(SITE);
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let err = v.get("error").and_then(|e| e.as_str()).unwrap_or("");
    let message = v.get("message").and_then(|m| m.as_str()).unwrap_or(body);
    if http_status == 401
        || err == "AuthenticationRequired"
        || err == "ExpiredToken"
        || err == "InvalidToken"
    {
        return Error::Auth {
            site,
            reason: if err.is_empty() {
                "bad_password".into()
            } else {
                err.to_string()
            },
        };
    }
    if http_status == 429 || err == "RateLimitExceeded" {
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
        code: if err.is_empty() {
            http_status.to_string()
        } else {
            err.to_string()
        },
        message: message.to_string(),
    }
}

#[cfg(all(test, not(feature = "oauth")))]
#[test]
fn bluesky_feature_excludes_oauth() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::MemoryAppStore;
    use crate::client::Client;
    use crate::publisher::AuthReply;
    use crate::registry::Registry;
    use crate::types::AccountKey;
    use crate::vault::{MemoryVault, Vault};
    use httpmock::prelude::*;
    use serde_json::json;
    use std::sync::Arc;

    fn pw_creds() -> AccountCreds {
        AccountCreds::AppPassword {
            identifier: "you.bsky.social".into(),
            secret: "xxxx-xxxx".into(),
            pds: None,
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

    fn empty_app() -> AppConfig {
        AppConfig {
            site: Site::new(SITE),
            oauth: None,
            extra: json!({}),
        }
    }

    fn session_ok(server: &MockServer) {
        server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.server.createSession");
            then.status(200).json_body(json!({
                "did": "did:plc:abc",
                "handle": "you.bsky.social",
                "accessJwt": "jwt",
                "refreshJwt": "rjwt"
            }));
        });
    }

    #[test]
    fn empty_text_no_http() {
        let err = validate_text("  ").unwrap_err();
        assert!(matches!(err, Error::InvalidPost { reason, .. } if reason == "empty"));
    }

    #[tokio::test]
    async fn unsupported_params_rejected_before_http() {
        // reply_to_id is taught; every other key stays an error before a
        // session is even created — a dropped param would publish a
        // differently-shaped post while reporting success.
        let server = MockServer::start();
        let session = server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.server.createSession");
            then.status(200).json_body(json!({
                "did": "did:plc:abc",
                "handle": "you.bsky.social",
                "accessJwt": "jwt",
                "refreshJwt": "rjwt"
            }));
        });
        let t = Bluesky::with_pds(server.base_url()).unwrap();
        let mut intent = text_intent("hi");
        intent.params = json!({ "chat_id": "-100" });
        let err = t
            .publish(&empty_app(), &pw_creds(), intent, Deadline::from_secs(30))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidPost { reason, .. } if reason == "unsupported_param:chat_id")
        );
        assert_eq!(session.hits(), 0, "no HTTP before the param check");
    }

    #[test]
    fn params_parse_reply_key_only() {
        let p = parse_params(&json!({})).unwrap();
        assert!(p.reply_to_id.is_none());
        assert!(parse_params(&Value::Null).unwrap().reply_to_id.is_none());
        assert!(parse_params(&json!({ "reply_to_id": null }))
            .unwrap()
            .reply_to_id
            .is_none());
        let p = parse_params(&json!({ "reply_to_id": "at://did:plc:abc/app.bsky.feed.post/3kx" }))
            .unwrap();
        assert_eq!(
            p.reply_to_id.as_deref(),
            Some("at://did:plc:abc/app.bsky.feed.post/3kx")
        );
        // Empty, non-string, unknown keys, and malformed at-URIs all refuse.
        assert!(parse_params(&json!({ "reply_to_id": "" })).is_err());
        assert!(parse_params(&json!({ "reply_to_id": 5 })).is_err());
        assert!(parse_params(&json!({ "user_id": "x" })).is_err());
        // A bsky.app web URL is not an at-URI; a reply into a non-post
        // collection would build an invalid record.
        assert!(
            parse_params(&json!({ "reply_to_id": "https://bsky.app/profile/you/post/3kx" }))
                .is_err()
        );
        assert!(
            parse_params(&json!({ "reply_to_id": "at://did:plc:abc/app.bsky.feed.like/3kx" }))
                .is_err()
        );
    }

    #[tokio::test]
    async fn malformed_reply_uri_refused_before_session() {
        // The at-URI shape check runs in parse_params, so a bad target dies
        // before even createSession — no HTTP at all.
        let server = MockServer::start();
        let session = server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.server.createSession");
            then.status(200).json_body(json!({
                "did": "did:plc:abc",
                "handle": "you.bsky.social",
                "accessJwt": "jwt"
            }));
        });
        let t = Bluesky::with_pds(server.base_url()).unwrap();
        for bad in [
            "https://bsky.app/profile/you.bsky.social/post/3kx",
            "at://did:plc:abc/app.bsky.feed.like/3kx",
            "at:///app.bsky.feed.post/3kx",
        ] {
            let mut intent = text_intent("hi");
            intent.params = json!({ "reply_to_id": bad });
            let err = t
                .publish(&empty_app(), &pw_creds(), intent, Deadline::from_secs(30))
                .await
                .unwrap_err();
            assert!(
                matches!(err, Error::InvalidPost { reason, .. } if reason == "reply_to_id"),
                "expected reply_to_id refusal for {bad}"
            );
        }
        assert_eq!(session.hits(), 0, "no HTTP before the URI check");
    }

    #[tokio::test]
    async fn reply_resolves_parent_strongref_and_pins_reply_wire() {
        // The reply wire: one getRecord for the parent's cid, then
        // createRecord carrying reply.root == reply.parent (a direct reply
        // to a root post roots the thread at that post).
        let server = MockServer::start();
        session_mock(&server);
        let lookup = server.mock(|when, then| {
            when.method(GET)
                .path("/xrpc/com.atproto.repo.getRecord")
                .query_param("repo", "did:plc:abc")
                .query_param("collection", "app.bsky.feed.post")
                .query_param("rkey", "3kx");
            then.status(200).json_body(json!({
                "uri": "at://did:plc:abc/app.bsky.feed.post/3kx",
                "cid": "parentcid",
                "value": {
                    "$type": "app.bsky.feed.post",
                    "text": "parent",
                    "createdAt": "2026-09-10T00:00:00Z"
                }
            }));
        });
        let record = server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.repo.createRecord")
                .body_contains("\"reply\":{")
                // serde_json emits object keys sorted: cid before uri.
                .body_contains("\"root\":{\"cid\":\"parentcid\",\"uri\":\"at://did:plc:abc/app.bsky.feed.post/3kx\"}")
                .body_contains("\"parent\":{\"cid\":\"parentcid\",\"uri\":\"at://did:plc:abc/app.bsky.feed.post/3kx\"}");
            then.status(200).json_body(json!({
                "uri": "at://did:plc:abc/app.bsky.feed.post/3ky",
                "cid": "y"
            }));
        });
        let b = Bluesky::with_pds(server.base_url()).unwrap();
        let mut intent = text_intent("a reply");
        intent.params = json!({ "reply_to_id": "at://did:plc:abc/app.bsky.feed.post/3kx" });
        let out = b
            .publish(&empty_app(), &pw_creds(), intent, Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(
            out.id.as_deref(),
            Some("at://did:plc:abc/app.bsky.feed.post/3ky")
        );
        assert_eq!(
            out.url.as_deref(),
            Some("https://bsky.app/profile/you.bsky.social/post/3ky")
        );
        lookup.assert();
        record.assert();
    }

    #[tokio::test]
    async fn reply_to_reply_inherits_thread_root() {
        // A parent that is itself a reply already carries reply.root; the
        // new reply must root at the thread root, not the parent, or it
        // would start a new thread.
        let server = MockServer::start();
        session_mock(&server);
        server.mock(|when, then| {
            when.method(GET).path("/xrpc/com.atproto.repo.getRecord");
            then.status(200).json_body(json!({
                "uri": "at://did:plc:abc/app.bsky.feed.post/3kx",
                "cid": "parentcid",
                "value": {
                    "$type": "app.bsky.feed.post",
                    "text": "a reply itself",
                    "reply": {
                        "root": {
                            "uri": "at://did:plc:abc/app.bsky.feed.post/3kw",
                            "cid": "rootcid"
                        },
                        "parent": {
                            "uri": "at://did:plc:abc/app.bsky.feed.post/3kw",
                            "cid": "rootcid"
                        }
                    }
                }
            }));
        });
        let record = server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.repo.createRecord")
                .body_contains("\"root\":{\"cid\":\"rootcid\",\"uri\":\"at://did:plc:abc/app.bsky.feed.post/3kw\"}")
                .body_contains("\"parent\":{\"cid\":\"parentcid\",\"uri\":\"at://did:plc:abc/app.bsky.feed.post/3kx\"}");
            then.status(200).json_body(json!({ "uri": "at://x", "cid": "y" }));
        });
        let b = Bluesky::with_pds(server.base_url()).unwrap();
        let mut intent = text_intent("nested");
        intent.params = json!({ "reply_to_id": "at://did:plc:abc/app.bsky.feed.post/3kx" });
        b.publish(&empty_app(), &pw_creds(), intent, Deadline::from_secs(30))
            .await
            .unwrap();
        record.assert();
    }

    #[tokio::test]
    async fn image_with_reply_param_refused_before_http() {
        // Parity with threads: the image-with-reply wire is untaught, so
        // the param must not be dropped into a root image post.
        let server = MockServer::start();
        let session = server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.server.createSession");
            then.status(200)
                .json_body(json!({ "accessJwt": "jwt", "did": "d", "handle": "h" }));
        });
        let b = Bluesky::with_pds(server.base_url()).unwrap();
        let mut intent = image_intent(
            Image::Bytes {
                filename: "hero.png".into(),
                bytes: b"png".to_vec(),
            },
            None,
            "",
        );
        intent.params = json!({ "reply_to_id": "at://did:plc:abc/app.bsky.feed.post/3kx" });
        let err = b
            .publish(&empty_app(), &pw_creds(), intent, Deadline::from_secs(30))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidPost { reason, .. } if reason == "image_reply_unsupported")
        );
        assert_eq!(session.hits(), 0);
    }

    #[test]
    fn too_long_is_graphemes() {
        let s = "a".repeat(301);
        let err = validate_text(&s).unwrap_err();
        assert!(
            matches!(err, Error::InvalidPost { reason, limit, .. } if reason == "text_too_long" && limit == Some(300))
        );
        let ok = "é".repeat(300);
        assert_eq!(ok.graphemes(true).count(), 300);
        validate_text(&ok).unwrap();
    }

    #[tokio::test]
    async fn client_auth_finish_without_app_file() {
        let server = MockServer::start();
        session_ok(&server);
        session_ok(&server);
        let mut reg = Registry::new();
        reg.register(Arc::new(Bluesky::with_pds(server.base_url()).unwrap()));
        let vault = Arc::new(MemoryVault::new());
        let c = Client::new(reg, vault.clone(), Arc::new(MemoryAppStore::new()));
        let key = AccountKey::new(SITE, "you.bsky.social");
        let me = c
            .auth_finish(
                &key,
                AuthReply::AppPassword {
                    identifier: "you.bsky.social".into(),
                    secret: "xxxx-xxxx".into(),
                    pds: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(me.handle.as_deref(), Some("you.bsky.social"));
        match vault.get(&key).unwrap() {
            AccountCreds::AppPassword { identifier, .. } => {
                assert_eq!(identifier, "you.bsky.social");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn auth_finish_stores_app_password() {
        let server = MockServer::start();
        session_ok(&server);
        let t = Bluesky::with_pds(server.base_url()).unwrap();
        let creds = t
            .auth_finish(
                &empty_app(),
                AuthReply::AppPassword {
                    identifier: "you.bsky.social".into(),
                    secret: "xxxx-xxxx".into(),
                    pds: None,
                },
            )
            .await
            .unwrap();
        match creds {
            AccountCreds::AppPassword {
                identifier,
                secret,
                pds,
            } => {
                assert_eq!(identifier, "you.bsky.social");
                assert_eq!(secret, "xxxx-xxxx");
                assert!(pds.unwrap().starts_with("http://"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn auth_finish_rejects_default_identifier() {
        let t = Bluesky::new().unwrap();
        let err = t
            .auth_finish(
                &empty_app(),
                AuthReply::AppPassword {
                    identifier: "default".into(),
                    secret: "x".into(),
                    pds: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Auth { reason, .. } if reason == "identifier_required"));
    }

    #[tokio::test]
    async fn happy_post() {
        let server = MockServer::start();
        session_ok(&server);
        server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.repo.createRecord");
            then.status(200).json_body(json!({
                "uri": "at://did:plc:abc/app.bsky.feed.post/xyz",
                "cid": "bafy"
            }));
        });
        let t = Bluesky::with_pds(server.base_url()).unwrap();
        let out = t
            .publish(
                &empty_app(),
                &pw_creds(),
                text_intent("hello"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(
            out.id.as_deref(),
            Some("at://did:plc:abc/app.bsky.feed.post/xyz")
        );
        assert_eq!(
            out.url.as_deref(),
            Some("https://bsky.app/profile/you.bsky.social/post/xyz")
        );
    }

    #[tokio::test]
    async fn multiline_text_round_trips_and_counts_newlines_as_graphemes() {
        // Bluesky text rides inside JSON, so serde escapes \n natively
        // (backslash-n in the raw body); the grapheme limit counts each
        // newline as 1. Locked so a future pre-encoding or stripping of
        // newlines cannot land silently.
        let server = MockServer::start();
        session_ok(&server);
        let record = server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.repo.createRecord")
                .body_contains("Baris pertama\\n\\nBaris kedua");
            then.status(200).json_body(json!({
                "uri": "at://did:plc:abc/app.bsky.feed.post/xyz",
                "cid": "bafy"
            }));
        });
        let t = Bluesky::with_pds(server.base_url()).unwrap();
        let out = t
            .publish(
                &empty_app(),
                &pw_creds(),
                text_intent("Baris pertama\n\nBaris kedua"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        record.assert();
        assert_eq!(
            out.id.as_deref(),
            Some("at://did:plc:abc/app.bsky.feed.post/xyz")
        );
        // 298 letters + 2 newlines = 300 passes; one more of either fails
        validate_text(&format!("{}\n\n", "a".repeat(298))).unwrap();
        let err = validate_text(&format!("{}\n\n", "a".repeat(299))).unwrap_err();
        assert!(matches!(err, Error::InvalidPost { reason, .. } if reason == "text_too_long"));
    }

    #[tokio::test]
    async fn session_401_is_auth() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.server.createSession");
            then.status(401).json_body(json!({
                "error": "AuthenticationRequired",
                "message": "Invalid identifier or password"
            }));
        });
        let t = Bluesky::with_pds(server.base_url()).unwrap();
        let err = t
            .publish(
                &empty_app(),
                &pw_creds(),
                text_intent("hello"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Auth { .. }));
    }

    #[tokio::test]
    async fn other_400_is_platform() {
        let server = MockServer::start();
        session_ok(&server);
        server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.repo.createRecord");
            then.status(400).json_body(json!({
                "error": "InvalidRequest",
                "message": "nope"
            }));
        });
        let t = Bluesky::with_pds(server.base_url()).unwrap();
        let err = t
            .publish(
                &empty_app(),
                &pw_creds(),
                text_intent("hello"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Platform { code, .. } if code == "InvalidRequest"));
    }

    #[tokio::test]
    #[ignore]
    async fn live_post() {
        if std::env::var("WIREKERN_LIVE").ok().as_deref() != Some("1") {
            return;
        }
        let ident = std::env::var("WIREKERN_BSKY_HANDLE").expect("WIREKERN_BSKY_HANDLE");
        let secret =
            std::env::var("WIREKERN_BSKY_APP_PASSWORD").expect("WIREKERN_BSKY_APP_PASSWORD");
        let t = Bluesky::new().unwrap();
        let creds = AccountCreds::AppPassword {
            identifier: ident,
            secret,
            pds: None,
        };
        let out = t
            .publish(
                &empty_app(),
                &creds,
                text_intent("wirekern bluesky live"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert!(out.id.as_deref().unwrap_or("").starts_with("at://"));
    }

    fn image_intent(image: Image, text: Option<&str>, alt: &str) -> Intent {
        Intent {
            site: Site::new(SITE),
            params: json!({}),
            body: Body::Image {
                text: text.map(str::to_string),
                image,
                alt: alt.into(),
            },
            idempotency_key: None,
        }
    }

    fn session_mock(server: &MockServer) {
        server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.server.createSession");
            then.status(200).json_body(json!({
                "accessJwt": "jwt",
                "did": "did:plc:abc",
                "handle": "you.bsky.social"
            }));
        });
    }

    /// Wire pin for the live 2026-09-10 bug: uploadBlob must carry the raw
    /// bytes as the body and the image's type in the Content-Type header.
    /// `body` is an exact match, so it also proves no multipart envelope is
    /// sent — which is what poisoned the blob's mimeType before this pin.
    /// (httpmock's exact matcher is string-typed; image test bytes are ASCII.)
    fn blob_mock<'a>(server: &'a MockServer, body: &str, mime: &str) -> httpmock::Mock<'a> {
        server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.repo.uploadBlob")
                .header("Content-Type", mime)
                .body(body);
            then.status(200).json_body(json!({
                "blob": {
                    "$type": "blob",
                    "ref": { "$link": "bafkabc" },
                    "mimeType": "image/png",
                    "size": 1234
                }
            }));
        })
    }

    #[tokio::test]
    async fn image_post_uploads_blob_then_embeds_it_with_alt() {
        let server = MockServer::start();
        session_mock(&server);
        let blob = blob_mock(&server, "png-bytes", "image/png");
        let record = server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.repo.createRecord")
                .body_contains("app.bsky.embed.images")
                .body_contains("bafkabc")
                .body_contains("chart of spend");
            then.status(200).json_body(json!({
                "uri": "at://did:plc:abc/app.bsky.feed.post/3k",
                "cid": "x"
            }));
        });
        let b = Bluesky::with_pds(server.base_url()).unwrap();
        let image = Image::Bytes {
            filename: "hero.png".into(),
            bytes: b"png-bytes".to_vec(),
        };
        let out = b
            .publish(
                &empty_app(),
                &pw_creds(),
                image_intent(image, Some("Caption"), "chart of spend"),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(
            out.id.as_deref(),
            Some("at://did:plc:abc/app.bsky.feed.post/3k")
        );
        assert_eq!(
            out.url.as_deref(),
            Some("https://bsky.app/profile/you.bsky.social/post/3k")
        );
        blob.assert();
        record.assert();
    }

    #[tokio::test]
    async fn image_post_without_caption_sends_empty_text() {
        let server = MockServer::start();
        session_mock(&server);
        let blob = blob_mock(&server, "jpg", "image/jpeg");
        let record = server.mock(|when, then| {
            when.method(POST)
                .path("/xrpc/com.atproto.repo.createRecord")
                .body_contains("\"text\":\"\"");
            then.status(200)
                .json_body(json!({ "uri": "at://x", "cid": "y" }));
        });
        let b = Bluesky::with_pds(server.base_url()).unwrap();
        let image = Image::Bytes {
            filename: "hero.jpg".into(),
            bytes: b"jpg".to_vec(),
        };
        b.publish(
            &empty_app(),
            &pw_creds(),
            image_intent(image, None, ""),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
        record.assert();
        blob.assert();
    }

    #[tokio::test]
    async fn image_refusals_happen_before_any_http() {
        let b = Bluesky::with_pds("http://127.0.0.1:1").unwrap();
        // URL source: Bluesky has no URL ingestion and the kernel never fetches.
        let err = b
            .publish(
                &empty_app(),
                &pw_creds(),
                image_intent(Image::Url("https://e.com/x.png".into()), None, ""),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::InvalidPost { reason, .. } if reason == "image_source_unsupported:url")
        );
        // Closed extension map: no sniffing, no guessing.
        let err = b
            .publish(
                &empty_app(),
                &pw_creds(),
                image_intent(
                    Image::Bytes {
                        filename: "hero.tiff".into(),
                        bytes: b"x".to_vec(),
                    },
                    None,
                    "",
                ),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::InvalidPost { reason, .. } if reason == "unsupported_image_type:tiff")
        );
        // Lexicon cap (2,000,000 bytes) enforced locally, limit surfaced.
        let err = b
            .publish(
                &empty_app(),
                &pw_creds(),
                image_intent(
                    Image::Bytes {
                        filename: "hero.png".into(),
                        bytes: vec![0u8; MAX_IMAGE_BYTES + 1],
                    },
                    None,
                    "",
                ),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::InvalidPost { reason, limit: Some(l), .. } if reason == "image_too_large" && *l == 2_000_000)
        );
        // A caption still obeys the 300-grapheme rule.
        let long = "x".repeat(301);
        let err = b
            .publish(
                &empty_app(),
                &pw_creds(),
                image_intent(
                    Image::Bytes {
                        filename: "hero.png".into(),
                        bytes: b"x".to_vec(),
                    },
                    Some(&long),
                    "",
                ),
                Deadline::from_secs(30),
            )
            .await
            .unwrap_err();
        assert!(matches!(&err, Error::InvalidPost { reason, .. } if reason == "text_too_long"));
    }

    #[test]
    fn mime_map_is_closed_and_lowercase() {
        assert_eq!(image_mime("hero.png").unwrap(), "image/png");
        assert_eq!(image_mime("HERO.JPG").unwrap(), "image/jpeg");
        assert_eq!(image_mime("hero.webp").unwrap(), "image/webp");
        assert!(image_mime("noext").is_err());
    }
}
