//! Local HTTP surface: same JSON as `--json`, Bearer `pk_live_` keys.
//!
//! Bind defaults to 127.0.0.1:8788. Auth dances stay on the CLI. This module
//! calls `Client` through `app::make_client` so serve cannot diverge from
//! the exec path.

use crate::app::{fail, make_client};
use crate::output::{emit_ok, emit_raw, human_line};
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use postkit::{
    AccountKey, Client, Deadline, Error, FileKeyStore, PostRequest, Site, Vault, WhatsAppMessage,
    WhatsAppSendRequest, WireError, KEY_PREFIX,
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

const DEFAULT_BIND: &str = "127.0.0.1:8788";
const DEFAULT_DEADLINE_SECS: u64 = 30;

#[derive(Clone)]
struct AppState {
    /// Deny-by-default: posts, reads, whoami. Cannot send WhatsApp.
    client: Arc<Client>,
    /// Allowing policy, used only by `POST /v1/whatsapp` after `allow_send`.
    whatsapp: Arc<Client>,
    keys: Arc<FileKeyStore>,
}

pub fn keys_create(home: &Path, name: &str, json: bool) -> Result<(), i32> {
    let store = FileKeyStore::new(home).map_err(|e| fail(&e, json))?;
    let created = store.create(name).map_err(|e| fail(&e, json))?;
    if json {
        emit_raw(&serde_json::json!({
            "name": created.name,
            "token": created.token,
        }));
    } else {
        eprintln!("shown once; store the hash only:");
        human_line(&created.token);
    }
    Ok(())
}

pub fn keys_list(home: &Path, json: bool) -> Result<(), i32> {
    let store = FileKeyStore::new(home).map_err(|e| fail(&e, json))?;
    let keys = store.list().map_err(|e| fail(&e, json))?;
    if json {
        emit_raw(&serde_json::json!({ "keys": keys }));
    } else {
        for k in keys {
            human_line(format!("{} {}", k.name, k.created_at));
        }
    }
    Ok(())
}

pub fn keys_revoke(home: &Path, name: &str, yes: bool, json: bool) -> Result<(), i32> {
    if !yes {
        eprintln!("pass --yes to revoke key {name}");
        return Err(2);
    }
    let store = FileKeyStore::new(home).map_err(|e| fail(&e, json))?;
    store.delete(name).map_err(|e| fail(&e, json))?;
    if json {
        emit_raw(&serde_json::json!({ "revoked": name }));
    } else {
        eprintln!("revoked {name}");
    }
    Ok(())
}

pub async fn run(home: &Path, bind: Option<&str>, json: bool) -> Result<(), i32> {
    let addr = parse_bind(bind).map_err(|e| fail(&e, json))?;
    let keys = FileKeyStore::new(home).map_err(|e| fail(&e, json))?;
    if keys.list().map_err(|e| fail(&e, json))?.is_empty() {
        let err = Error::Auth {
            site: Site::new(""),
            reason: "no_keys".into(),
        };
        eprintln!("create a key first: postkit keys create --name <n>");
        return Err(fail(&err, json));
    }
    let client = make_client(home, false).map_err(|e| fail(&e, json))?;
    // Built only for the WhatsApp route. The handler still requires
    // `allow_send: true` on each request — this client is not used by
    // `/v1/posts`, so a forgotten flag cannot send a private message.
    let whatsapp = make_client(home, true).map_err(|e| fail(&e, json))?;
    let app = router(Arc::new(client), Arc::new(whatsapp), Arc::new(keys));
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|_| {
        fail(
            &Error::Network {
                site: Site::new(""),
                message: "bind failed".into(),
            },
            json,
        )
    })?;
    if json {
        emit_ok(
            &serde_json::json!({ "bind": addr.to_string() }),
            true,
            String::new,
        );
    } else {
        eprintln!("listening on http://{addr}  (Authorization: Bearer {KEY_PREFIX}…)");
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|_| {
            fail(
                &Error::Network {
                    site: Site::new(""),
                    message: "server failed".into(),
                },
                json,
            )
        })
}

/// Refuse a missing/garbage bind string. Unspecified (0.0.0.0) is allowed
/// only as an explicit `--bind` — a key is not a firewall, but the operator
/// who passed the flag has opted in.
pub(crate) fn parse_bind(bind: Option<&str>) -> Result<SocketAddr, Error> {
    let spec = bind.unwrap_or(DEFAULT_BIND);
    spec.parse::<SocketAddr>().map_err(|_| Error::InvalidQuery {
        site: Site::new(""),
        reason: "invalid_bind".into(),
    })
}

fn router(client: Arc<Client>, whatsapp: Arc<Client>, keys: Arc<FileKeyStore>) -> Router {
    Router::new()
        .route("/v1/posts", post(posts))
        .route("/v1/whatsapp", post(whatsapp_send))
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/accounts", get(accounts))
        .route("/v1/whoami", get(whoami))
        .with_state(AppState {
            client,
            whatsapp,
            keys,
        })
}

#[derive(Deserialize)]
struct HttpWhatsAppSend {
    #[serde(default = "default_http_account")]
    account: String,
    /// Must be present and true. Omitted/false is the HTTP twin of a CLI
    /// invocation without `--allow-send`.
    #[serde(default)]
    allow_send: bool,
    #[serde(flatten)]
    request: WhatsAppSendRequest,
}

fn default_http_account() -> String {
    "default".into()
}

async fn whatsapp_send(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(e) = authorize(&state, &headers).await {
        return wire_response(e);
    }
    let mut req: HttpWhatsAppSend = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return wire_response(Error::InvalidPost {
                site: Site::new("whatsapp_cloud"),
                reason: format!("json:{e}"),
                limit: None,
            });
        }
    };
    if let Some(idem) = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
    {
        if req.request.idempotency_key.is_empty() {
            req.request.idempotency_key = idem.to_string();
        }
    }
    // Gate before the allowing client is used. A body without allow_send
    // must not reveal whether a WhatsApp account exists in the vault.
    if !req.allow_send {
        let action = match &req.request.message {
            WhatsAppMessage::Reply { .. } => "send_whatsapp_reply",
            WhatsAppMessage::Template { .. } => "send_whatsapp_template",
        };
        return wire_response(Error::PolicyDenied {
            site: Site::new("whatsapp_cloud"),
            action: action.into(),
            reason: "explicit_whatsapp_send_required".into(),
        });
    }
    let key = AccountKey::new("whatsapp_cloud", &req.account);
    match state
        .whatsapp
        .send_whatsapp(&key, req.request, deadline_from(&headers))
        .await
    {
        Ok(out) => (StatusCode::OK, Json(out)).into_response(),
        Err(e) => wire_response(e),
    }
}

async fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), Error> {
    let raw = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Error::Auth {
            site: Site::new(""),
            reason: "missing_key".into(),
        })?;
    // RFC 7235: the scheme is case-insensitive. n8n and some HTTP libs send
    // `bearer` rather than `Bearer`.
    let token = bearer_token(raw)?;
    state.keys.verify(token).map(|_| ())
}

pub(crate) fn bearer_token(header: &str) -> Result<&str, Error> {
    let header = header.trim();
    let Some((scheme, rest)) = header.split_once(char::is_whitespace) else {
        return Err(Error::Auth {
            site: Site::new(""),
            reason: "missing_key".into(),
        });
    };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(Error::Auth {
            site: Site::new(""),
            reason: "invalid_key".into(),
        });
    }
    let token = rest.trim();
    if token.is_empty() {
        return Err(Error::Auth {
            site: Site::new(""),
            reason: "missing_key".into(),
        });
    }
    Ok(token)
}

fn wire_response(err: Error) -> Response {
    let wire = WireError::from(&err);
    let status =
        StatusCode::from_u16(wire.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(wire)).into_response()
}

fn deadline_from(headers: &HeaderMap) -> Deadline {
    let secs = headers
        .get("x-postkit-deadline")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_DEADLINE_SECS);
    Deadline::from_secs(secs.max(1))
}

async fn posts(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(e) = authorize(&state, &headers).await {
        return wire_response(e);
    }
    let req: PostRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return wire_response(Error::InvalidPost {
                site: Site::new(""),
                reason: format!("json:{e}"),
                limit: None,
            });
        }
    };
    let (key, mut intent) = match req.into_key_intent() {
        Ok(v) => v,
        Err(e) => return wire_response(e),
    };
    // Header wins: HTTP's Idempotency-Key is the n8n contract. PostRequest
    // has no key field; CLI --stdin uses Intent after the surface compiles.
    if let Some(idem) = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
    {
        intent.idempotency_key = Some(idem.to_string());
    }
    match state
        .client
        .publish(&key, intent, deadline_from(&headers))
        .await
    {
        Ok(out) => (StatusCode::OK, Json(out)).into_response(),
        Err(e) => wire_response(e),
    }
}

async fn capabilities(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = authorize(&state, &headers).await {
        return wire_response(e);
    }
    Json(state.client.registry().capabilities_json()).into_response()
}

#[derive(Deserialize)]
struct SiteQuery {
    site: Option<String>,
    account: Option<String>,
}

async fn accounts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SiteQuery>,
) -> Response {
    if let Err(e) = authorize(&state, &headers).await {
        return wire_response(e);
    }
    let filter = q.site.as_deref().map(Site::new);
    let vault: &dyn Vault = state.client.vault();
    match vault.list(filter.as_ref()) {
        Ok(keys) => {
            let rows: Vec<_> = keys
                .iter()
                .map(|k| serde_json::json!({ "site": k.site, "name": k.name }))
                .collect();
            Json(serde_json::json!({ "accounts": rows })).into_response()
        }
        Err(e) => wire_response(e),
    }
}

async fn whoami(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SiteQuery>,
) -> Response {
    if let Err(e) = authorize(&state, &headers).await {
        return wire_response(e);
    }
    let Some(site) = q.site else {
        return wire_response(Error::InvalidQuery {
            site: Site::new(""),
            reason: "missing_site".into(),
        });
    };
    let account = q.account.unwrap_or_else(|| "default".into());
    let key = AccountKey::new(&site, &account);
    match state.client.whoami(&key).await {
        Ok(w) => (StatusCode::OK, Json(w)).into_response(),
        Err(e) => wire_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use postkit::{
        AccountCreds, AllowWhatsAppSendsPolicy, AuthKind, Capability, Connector, MemoryAppStore,
        MemoryVault, Outcome, Publisher, Registry, Vault, WhatsAppSender, WhoAmI,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tower::ServiceExt;

    struct WaMock {
        site: Site,
        sends: AtomicUsize,
    }

    #[async_trait]
    impl Publisher for WaMock {
        fn site(&self) -> &Site {
            &self.site
        }
        fn capabilities(&self) -> &[Capability] {
            &[Capability::SendReply, Capability::SendTemplate]
        }
        fn auth_kind(&self) -> AuthKind {
            AuthKind::StaticToken
        }
        async fn publish(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _intent: postkit::Intent,
            _deadline: Deadline,
        ) -> Result<Outcome, Error> {
            Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: "use_whatsapp_command".into(),
                limit: None,
            })
        }
        async fn whoami(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
        ) -> Result<WhoAmI, Error> {
            Ok(WhoAmI {
                site: self.site.clone(),
                id: "1".into(),
                handle: None,
            })
        }
    }

    #[async_trait]
    impl WhatsAppSender for WaMock {
        async fn send_whatsapp(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _request: &WhatsAppSendRequest,
            _deadline: Deadline,
        ) -> Result<Outcome, Error> {
            let n = self.sends.fetch_add(1, Ordering::SeqCst);
            Ok(Outcome {
                site: self.site.clone(),
                id: Some(format!("wamid-{n}")),
                url: None,
                limits: None,
            })
        }
    }

    fn test_router(tmp: &Path) -> (Router, String, Arc<WaMock>) {
        let keys = FileKeyStore::new(tmp).unwrap();
        let created = keys.create("n8n").unwrap();
        let mock = Arc::new(WaMock {
            site: Site::new("whatsapp_cloud"),
            sends: AtomicUsize::new(0),
        });
        let mut registry = Registry::new();
        registry.register_connector(Connector::from_publisher(mock.clone()).whatsapp(mock.clone()));
        let vault = Arc::new(MemoryVault::new());
        vault
            .put(
                &AccountKey::new("whatsapp_cloud", "default"),
                &AccountCreds::BotToken {
                    token: "system-user".into(),
                },
            )
            .unwrap();
        let apps = Arc::new(MemoryAppStore::new());
        let deny = Client::new(registry, vault.clone(), apps.clone());
        let mut registry_allow = Registry::new();
        registry_allow
            .register_connector(Connector::from_publisher(mock.clone()).whatsapp(mock.clone()));
        let allow = Client::new(registry_allow, vault, apps)
            .with_whatsapp_policy(Arc::new(AllowWhatsAppSendsPolicy));
        (
            router(Arc::new(deny), Arc::new(allow), Arc::new(keys)),
            created.token,
            mock,
        )
    }

    #[tokio::test]
    async fn missing_bearer_is_401() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, _, _) = test_router(tmp.path());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/capabilities")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn lowercase_bearer_and_wrong_key() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, token, _) = test_router(tmp.path());
        let ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/capabilities")
                    .header(AUTHORIZATION, format!("bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);

        let bad = app
            .oneshot(
                Request::builder()
                    .uri("/v1/capabilities")
                    .header(AUTHORIZATION, "Bearer pk_live_nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn valid_key_lists_capabilities() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, token, _) = test_router(tmp.path());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/capabilities")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_site_post_is_404_json_without_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, token, _) = test_router(tmp.path());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/posts")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"target":{"site":"nope"},"body":{"type":"text","text":"hi"}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"], "unknown_site");
        assert!(v.get("ok").is_none());
    }

    #[test]
    fn default_bind_is_loopback() {
        let addr = parse_bind(None).unwrap();
        assert!(addr.ip().is_loopback());
        assert_eq!(addr.port(), 8788);
        assert!(parse_bind(Some("not-an-addr")).is_err());
        let lan = parse_bind(Some("0.0.0.0:9999")).unwrap();
        assert!(lan.ip().is_unspecified());
        assert_eq!(lan.port(), 9999);
    }

    fn wa_body(allow_send: bool) -> String {
        format!(
            r#"{{"allow_send":{allow_send},"idempotency_key":"k1","message":{{"type":"reply","to":"60123456789","reply_to_message_id":"wamid.in","text":"hi"}}}}"#
        )
    }

    #[tokio::test]
    async fn whatsapp_without_allow_send_is_denied_before_connector() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, token, mock) = test_router(tmp.path());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/whatsapp")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(wa_body(false)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"], "policy_denied");
        assert_eq!(v["reason"], "explicit_whatsapp_send_required");
        assert_eq!(mock.sends.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn whatsapp_allow_send_reaches_connector() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, token, mock) = test_router(tmp.path());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/whatsapp")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(wa_body(true)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["id"], "wamid-0");
        assert!(v.get("ok").is_none());
        assert_eq!(mock.sends.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn posts_cannot_send_whatsapp() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, token, mock) = test_router(tmp.path());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/posts")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"target":{"site":"whatsapp_cloud"},"body":{"type":"text","text":"hi"}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // Production WhatsApp advertises send.*, not publish.text, so a
        // generic post is unsupported — never a private send.
        assert_eq!(v["error"], "unsupported");
        assert_eq!(mock.sends.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn bearer_scheme_is_case_insensitive() {
        assert_eq!(bearer_token("Bearer abc").unwrap(), "abc");
        assert_eq!(bearer_token("bearer abc").unwrap(), "abc");
        assert_eq!(bearer_token("  BEARER   abc  ").unwrap(), "abc");
        assert!(matches!(
            bearer_token("Basic abc"),
            Err(Error::Auth { reason, .. }) if reason == "invalid_key"
        ));
        assert!(matches!(
            bearer_token("Bearer"),
            Err(Error::Auth { reason, .. }) if reason == "missing_key"
        ));
    }
}
