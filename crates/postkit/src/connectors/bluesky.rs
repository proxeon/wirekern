use crate::error::Error;
use crate::http::Http;
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::types::{
    AccountCreds, AppConfig, Body, Capability, Deadline, Intent, Outcome, Site, WhoAmI,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use unicode_segmentation::UnicodeSegmentation;

pub const SITE: &str = "bluesky";
pub const DEFAULT_PDS: &str = "https://bsky.social";
pub const MAX_GRAPHEMES: usize = 300;

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
        &[Capability::PublishText]
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::AppPassword
    }

    async fn auth_start(&self, _app: &AppConfig) -> Result<AuthStart, Error> {
        Ok(AuthStart::PasteInstructions {
            hint: "app password (not account password); identifier is --account (handle)"
                .into(),
        })
    }

    async fn auth_finish(
        &self,
        _app: &AppConfig,
        reply: AuthReply,
    ) -> Result<AccountCreds, Error> {
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
        let _ = create_session(&self.http, &pds, &identifier, &secret, Deadline::from_secs(30))
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
        let Body::Text { text } = &intent.body;
        validate_text(text)?;
        let (pds, identifier, secret) = app_password(creds)?;
        let pds = self.pds_override.as_deref().unwrap_or(pds);
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

    async fn whoami(&self, _app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error> {
        let (pds, identifier, secret) = app_password(creds)?;
        let pds = self.pds_override.as_deref().unwrap_or(pds);
        let sess = create_session(&self.http, pds, identifier, secret, Deadline::from_secs(30))
            .await?;
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
            let pds = pds.as_deref().filter(|s| !s.is_empty()).unwrap_or(DEFAULT_PDS);
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
    let text = resp.text().await.map_err(|e| Error::Network {
        site: site.clone(),
        message: e.to_string(),
    })?;
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
    let message = v
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or(body);
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
}
