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
        validate_params(&intent.params)?;
        let (pds, identifier, secret) = app_password(creds)?;
        let pds = self.pds_override.as_deref().unwrap_or(pds);
        match &intent.body {
            Body::Text { text } => {
                validate_text(text)?;
                let sess = create_session(&self.http, pds, identifier, secret, deadline).await?;
                post_text(
                    &self.http,
                    pds,
                    &sess.access_jwt,
                    &sess.did,
                    Some(&sess.handle),
                    text,
                    deadline,
                )
                .await
            }
            Body::Image { text, image, alt } => {
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
                // extension map (the multipart part needs a content type;
                // sniffing bytes is out), then the lexicon's 2 MB cap.
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
                let blob = upload_blob(
                    &self.http,
                    pds,
                    &sess.access_jwt,
                    filename,
                    bytes,
                    mime,
                    deadline,
                )
                .await?;
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

/// Bluesky supports no `Intent.params` today. A dropped param is not
/// neutral: `--param reply_to_id=…` would be silently discarded and a
/// **root post** published instead of the intended reply — wrong public
/// output while reporting a success `Outcome`, the worst failure mode
/// this kernel can have. So any param is an error until the connector
/// actually implements it (replies via `reply.parent` will teach this
/// fn the key instead of rejecting it).
pub fn validate_params(params: &Value) -> Result<(), Error> {
    if let Some(k) = params.as_object().and_then(|o| o.keys().next()) {
        return Err(Error::InvalidPost {
            site: Site::new(SITE),
            reason: format!("unsupported_param:{k}"),
            limit: None,
        });
    }
    Ok(())
}

/// Closed extension → MIME map. The lexicon accepts any `image/*`, but the
/// multipart upload must name one content type and postkit does not sniff
/// bytes; a conservative closed set turns a typo'd extension into a local
/// `invalid_post` instead of a server-side guess. Extended deliberately,
/// like every closed set here.
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

/// `com.atproto.repo.uploadBlob`: one multipart file part carrying the
/// bytes and their derived content type. Returns the platform's blob
/// object verbatim (`{"$type":"blob","ref":{"$link":…},"mimeType":…,
/// "size":…}`) — the embed references it untouched.
async fn upload_blob(
    http: &Http,
    pds: &str,
    access_jwt: &str,
    filename: &str,
    bytes: &[u8],
    mime: &str,
    deadline: Deadline,
) -> Result<Value, Error> {
    let site = Site::new(SITE);
    let url = xrpc(pds, "com.atproto.repo.uploadBlob");
    let part = reqwest::multipart::Part::bytes(bytes.to_vec())
        .file_name(filename.to_string())
        .mime_str(mime)
        .map_err(|e| Error::Platform {
            site: site.clone(),
            code: "mime".into(),
            message: e.to_string(),
        })?;
    let form = reqwest::multipart::Form::new().part("file", part);
    let req = http
        .post(&url)
        .header("Authorization", format!("Bearer {access_jwt}"))
        .multipart(form);
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

pub async fn post_text(
    http: &Http,
    pds: &str,
    access_jwt: &str,
    did: &str,
    handle: Option<&str>,
    text: &str,
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
            "text": text,
            "createdAt": created,
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
    async fn params_rejected_before_http() {
        // reply_to_id used to be dropped and a root post published in its
        // place. The param must be refused before a session is even created.
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
        intent.params = json!({ "reply_to_id": "at://did:plc:abc/app.bsky.feed.post/3kx" });
        let err = t
            .publish(&empty_app(), &pw_creds(), intent, Deadline::from_secs(30))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidPost { reason, .. } if reason == "unsupported_param:reply_to_id")
        );
        assert_eq!(session.hits(), 0, "no HTTP before the param check");
    }

    #[test]
    fn empty_params_object_passes() {
        validate_params(&json!({})).unwrap();
        validate_params(&Value::Null).unwrap();
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
        if std::env::var("POSTKIT_LIVE").ok().as_deref() != Some("1") {
            return;
        }
        let ident = std::env::var("POSTKIT_BSKY_HANDLE").expect("POSTKIT_BSKY_HANDLE");
        let secret = std::env::var("POSTKIT_BSKY_APP_PASSWORD").expect("POSTKIT_BSKY_APP_PASSWORD");
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
                text_intent("postkit bluesky live"),
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

    fn blob_mock<'a>(server: &'a MockServer) -> httpmock::Mock<'a> {
        server.mock(|when, then| {
            when.method(POST).path("/xrpc/com.atproto.repo.uploadBlob");
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
        let blob = blob_mock(&server);
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
        blob_mock(&server);
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
