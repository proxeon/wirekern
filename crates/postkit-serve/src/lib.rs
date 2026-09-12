//! Local HTTP surface for postkit. Same JSON as the CLI `--json` document.
//!
//! This crate is the listen/router process. CLI and a later MCP crate call
//! [`run`] / [`router`]; they must not copy the route table. Auth dances and
//! `keys create` stay on the CLI. Clients come from [`Client::from_home`] so
//! serve cannot register a different connector set than `postkit post`.

use axum::body::Bytes;
use axum::extract::{Path as PathParam, Query, State};
use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use postkit::{
    AccountKey, AdEntity, AdReviewStatusRequest, AdsInspectRequest, AdsInventoryKind,
    AdsInventoryRequest, AttributionWindow, Breakdown, Client, DateRange, Deadline, Error,
    FileKeyStore, InsightsLevel, InsightsQuery, Metric, PostRequest, Site, Vault, WhatsAppMessage,
    WhatsAppSendRequest, WireError, KEY_PREFIX,
};
use serde::Deserialize;
use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::str::FromStr;
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

pub async fn run(home: &Path, bind: Option<&str>, json: bool) -> Result<(), Error> {
    let addr = parse_bind(bind)?;
    let keys = FileKeyStore::new(home)?;
    if keys.list()?.is_empty() {
        eprintln!("create a key first: postkit keys create --name <n>");
        return Err(Error::Auth {
            site: Site::new(""),
            reason: "no_keys".into(),
        });
    }
    // Same connector set as `postkit post`. Deny-by-default client for
    // social routes; allowing client only for POST /v1/whatsapp.
    let client = Client::from_home(home, false)?;
    let whatsapp = Client::from_home(home, true)?;
    let app = router(Arc::new(client), Arc::new(whatsapp), Arc::new(keys));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|_| Error::Network {
            site: Site::new(""),
            message: "bind failed".into(),
        })?;
    if json {
        // One document, then this process is the server. Scripts must read
        // this JSON and not wait for exit; request results are HTTP bodies.
        println!(
            "{}",
            serde_json::to_string(&listen_document(addr)).expect("listen json")
        );
        let _ = std::io::stdout().flush();
    } else {
        eprintln!(
            "listening on http://{addr}  (Authorization: Bearer {KEY_PREFIX}…); process stays up until interrupt"
        );
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|_| Error::Network {
            site: Site::new(""),
            message: "server failed".into(),
        })
}

/// Refuse a missing/garbage bind string. Unspecified (0.0.0.0) is allowed
/// only as an explicit `--bind` — a key is not a firewall, but the operator
/// who passed the flag has opted in.
/// Machine-readable listen line. `listening: true` is the contract that
/// stdout will not get a second document; the process remains the server.
pub fn listen_document(addr: SocketAddr) -> serde_json::Value {
    serde_json::json!({
        "bind": addr.to_string(),
        "listening": true,
        "pid": std::process::id(),
    })
}

pub fn parse_bind(bind: Option<&str>) -> Result<SocketAddr, Error> {
    let spec = bind.unwrap_or(DEFAULT_BIND);
    spec.parse::<SocketAddr>().map_err(|_| Error::InvalidQuery {
        site: Site::new(""),
        reason: "invalid_bind".into(),
    })
}

pub fn router(client: Arc<Client>, whatsapp: Arc<Client>, keys: Arc<FileKeyStore>) -> Router {
    Router::new()
        .route("/v1/posts", post(posts))
        .route("/v1/whatsapp", post(whatsapp_send))
        .route("/v1/whatsapp/webhook", post(whatsapp_webhook))
        // Meta-facing callback: no pk_live_ key. GET is the verify-token
        // handshake; POST is HMAC + HTTP 200 ACK. Authenticated parse stays
        // on /v1/whatsapp/webhook for BYO receivers.
        .route(
            "/v1/whatsapp/callback",
            get(whatsapp_callback_get).post(whatsapp_callback_post),
        )
        .route("/v1/whatsapp/events/{wamid}", get(whatsapp_event_get))
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/accounts", get(accounts))
        .route("/v1/whoami", get(whoami))
        // Ads reads only. pk_live_ is not a spend key: there is no HTTP
        // activate, budget edit, or paused-create route.
        .route("/v1/insights", get(insights))
        .route("/v1/ads/accounts", get(ads_accounts))
        .route("/v1/ads/list", get(ads_list))
        .route("/v1/ads/inspect", get(ads_inspect))
        .route("/v1/ads/status", get(ads_status))
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
    /// A configured local alias. This is intentionally not a raw phone ID:
    /// the server may only route through senders the operator configured.
    #[serde(default)]
    sender: Option<String>,
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
            WhatsAppMessage::Text { .. } => "send_whatsapp_text",
            WhatsAppMessage::Template { .. } => "send_whatsapp_template",
            WhatsAppMessage::Image { .. }
            | WhatsAppMessage::Document { .. }
            | WhatsAppMessage::Audio { .. }
            | WhatsAppMessage::Video { .. }
            | WhatsAppMessage::Sticker { .. } => "send_whatsapp_media",
            WhatsAppMessage::Buttons { .. }
            | WhatsAppMessage::List { .. }
            | WhatsAppMessage::CtaUrl { .. }
            | WhatsAppMessage::LocationRequest { .. }
            | WhatsAppMessage::VoiceCall { .. }
            | WhatsAppMessage::AddressRequest { .. } => "send_whatsapp_interactive",
            WhatsAppMessage::Location { .. } => "send_whatsapp_location",
            WhatsAppMessage::Contacts { .. } => "send_whatsapp_contacts",
            WhatsAppMessage::Reaction { .. } => "send_whatsapp_reaction",
            WhatsAppMessage::MarkRead { .. } => "send_whatsapp_read",
            WhatsAppMessage::Typing { .. } => "send_whatsapp_typing",
            WhatsAppMessage::Catalog { .. }
            | WhatsAppMessage::Product { .. }
            | WhatsAppMessage::ProductList { .. }
            | WhatsAppMessage::OrderStatus { .. } => "send_whatsapp_catalog",
            WhatsAppMessage::Flow { .. } => "send_whatsapp_flow",
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
        .send_whatsapp_from(
            &key,
            req.sender.as_deref(),
            req.request,
            deadline_from(&headers),
        )
        .await
    {
        Ok(out) => (StatusCode::OK, Json(out)).into_response(),
        Err(e) => wire_response(e),
    }
}

async fn whatsapp_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WebhookQuery>,
    body: Bytes,
) -> Response {
    if let Err(e) = authorize(&state, &headers).await {
        return wire_response(e);
    }
    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let extras = status_extras_requested(&headers, query.status_extras);
    match state.client.parse_whatsapp_webhook(
        signature,
        &body,
        postkit::WebhookParseOptions {
            include_status_extras: extras,
        },
    ) {
        Ok(out) => (StatusCode::OK, Json(out)).into_response(),
        Err(e) => wire_response(e),
    }
}

#[derive(Deserialize, Default)]
struct HubChallenge {
    #[serde(rename = "hub.mode")]
    mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    challenge: Option<String>,
}

/// Meta webhook verification. Response body is the raw challenge string.
async fn whatsapp_callback_get(
    State(state): State<AppState>,
    Query(hub): Query<HubChallenge>,
) -> Response {
    match state.client.verify_whatsapp_callback_challenge(
        hub.mode.as_deref().unwrap_or(""),
        hub.verify_token.as_deref().unwrap_or(""),
        hub.challenge.as_deref().unwrap_or(""),
    ) {
        Ok(challenge) => (StatusCode::OK, challenge).into_response(),
        Err(_) => StatusCode::FORBIDDEN.into_response(),
    }
}

/// Meta event delivery. HMAC over the raw body; HTTP 200 ACK even when the
/// payload cannot be reduced, so Meta does not disable the callback. Invalid
/// signatures are 403.
async fn whatsapp_callback_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    match state.client.ingest_whatsapp_webhook(
        signature,
        &body,
        postkit::WebhookParseOptions {
            include_status_extras: true,
        },
    ) {
        Ok(_) => StatusCode::OK.into_response(),
        Err(Error::InvalidQuery { reason, .. }) if reason == "webhook_signature_invalid" => {
            StatusCode::FORBIDDEN.into_response()
        }
        Err(_) => StatusCode::OK.into_response(),
    }
}

async fn whatsapp_event_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    PathParam(wamid): PathParam<String>,
) -> Response {
    if let Err(e) = authorize(&state, &headers).await {
        return wire_response(e);
    }
    match state.client.whatsapp_ledger_get(&wamid) {
        Ok(Some(row)) => (StatusCode::OK, Json(row)).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
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

pub fn bearer_token(header: &str) -> Result<&str, Error> {
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

#[derive(Deserialize, Default)]
struct WebhookQuery {
    #[serde(default)]
    status_extras: bool,
}

fn status_extras_requested(headers: &HeaderMap, query: bool) -> bool {
    if query {
        return true;
    }
    headers
        .get("x-postkit-status-extras")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
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

#[derive(Deserialize, Default)]
struct AdsReadQuery {
    site: Option<String>,
    account: Option<String>,
    entity: Option<String>,
    id: Option<String>,
    ad_account: Option<String>,
    from: Option<String>,
    to: Option<String>,
    attribution: Option<String>,
    level: Option<String>,
    metrics: Option<String>,
    #[serde(default)]
    entity_id: Vec<String>,
    breakdowns: Option<String>,
    report: Option<String>,
}

fn ads_key(q: &AdsReadQuery) -> Result<AccountKey, Error> {
    let site = q.site.as_deref().unwrap_or("meta_ads");
    if site.is_empty() {
        return Err(Error::InvalidQuery {
            site: Site::new(""),
            reason: "missing_site".into(),
        });
    }
    Ok(AccountKey::new(
        site,
        q.account.as_deref().unwrap_or("default"),
    ))
}

async fn ads_accounts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AdsReadQuery>,
) -> Response {
    if let Err(e) = authorize(&state, &headers).await {
        return wire_response(e);
    }
    let key = match ads_key(&q) {
        Ok(key) => key,
        Err(e) => return wire_response(e),
    };
    match state
        .client
        .ad_accounts(&key, deadline_from(&headers))
        .await
    {
        Ok(reply) => (StatusCode::OK, Json(reply)).into_response(),
        Err(e) => wire_response(e),
    }
}

async fn ads_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AdsReadQuery>,
) -> Response {
    if let Err(e) = authorize(&state, &headers).await {
        return wire_response(e);
    }
    let key = match ads_key(&q) {
        Ok(key) => key,
        Err(e) => return wire_response(e),
    };
    let Some(entity) = q.entity.as_deref() else {
        return wire_response(Error::InvalidQuery {
            site: key.site.clone(),
            reason: "missing_entity".into(),
        });
    };
    let kind = match AdsInventoryKind::from_str(entity) {
        Ok(kind) => kind,
        Err(reason) => {
            return wire_response(Error::InvalidQuery {
                site: key.site.clone(),
                reason,
            })
        }
    };
    let request = AdsInventoryRequest {
        account: q.ad_account.clone(),
        kind,
    };
    match state
        .client
        .list_ads_inventory(&key, request, deadline_from(&headers))
        .await
    {
        Ok(reply) => (StatusCode::OK, Json(reply)).into_response(),
        Err(e) => wire_response(e),
    }
}

async fn ads_inspect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AdsReadQuery>,
) -> Response {
    if let Err(e) = authorize(&state, &headers).await {
        return wire_response(e);
    }
    let key = match ads_key(&q) {
        Ok(key) => key,
        Err(e) => return wire_response(e),
    };
    let (entity, id) = match (q.entity.as_deref(), q.id.as_deref()) {
        (Some(entity), Some(id)) => (entity, id),
        _ => {
            return wire_response(Error::InvalidQuery {
                site: key.site.clone(),
                reason: "missing_entity_or_id".into(),
            })
        }
    };
    let kind = match AdsInventoryKind::from_str(entity) {
        Ok(kind) => kind,
        Err(reason) => {
            return wire_response(Error::InvalidQuery {
                site: key.site.clone(),
                reason,
            })
        }
    };
    let request = AdsInspectRequest {
        kind,
        id: id.into(),
    };
    match state
        .client
        .inspect_ads_object(&key, request, deadline_from(&headers))
        .await
    {
        Ok(reply) => (StatusCode::OK, Json(reply)).into_response(),
        Err(e) => wire_response(e),
    }
}

async fn ads_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AdsReadQuery>,
) -> Response {
    if let Err(e) = authorize(&state, &headers).await {
        return wire_response(e);
    }
    let key = match ads_key(&q) {
        Ok(key) => key,
        Err(e) => return wire_response(e),
    };
    let (entity, id) = match (q.entity.as_deref(), q.id.as_deref()) {
        (Some(entity), Some(id)) => (entity, id),
        _ => {
            return wire_response(Error::InvalidQuery {
                site: key.site.clone(),
                reason: "missing_entity_or_id".into(),
            })
        }
    };
    let entity = match AdEntity::from_str(entity) {
        Ok(entity) => entity,
        Err(reason) => {
            return wire_response(Error::InvalidQuery {
                site: key.site.clone(),
                reason,
            })
        }
    };
    let request = AdReviewStatusRequest {
        entity,
        id: id.into(),
    };
    match state
        .client
        .ad_review_status(&key, request, deadline_from(&headers))
        .await
    {
        Ok(reply) => (StatusCode::OK, Json(reply)).into_response(),
        Err(e) => wire_response(e),
    }
}

async fn insights(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AdsReadQuery>,
) -> Response {
    if let Err(e) = authorize(&state, &headers).await {
        return wire_response(e);
    }
    let key = match ads_key(&q) {
        Ok(key) => key,
        Err(e) => return wire_response(e),
    };
    let query = match insights_from_query(&q) {
        Ok(query) => query,
        Err(e) => return wire_response(e),
    };
    match state
        .client
        .insights(&key, query, deadline_from(&headers))
        .await
    {
        Ok(reply) => (StatusCode::OK, Json(reply)).into_response(),
        Err(e) => wire_response(e),
    }
}

fn insights_from_query(q: &AdsReadQuery) -> Result<InsightsQuery, Error> {
    let site = q.site.as_deref().unwrap_or("meta_ads");
    let from = q.from.as_deref().ok_or_else(|| Error::InvalidQuery {
        site: Site::new(site),
        reason: "missing_from".into(),
    })?;
    let to = q.to.as_deref().ok_or_else(|| Error::InvalidQuery {
        site: Site::new(site),
        reason: "missing_to".into(),
    })?;
    let attribution = q
        .attribution
        .as_deref()
        .ok_or_else(|| Error::InvalidQuery {
            site: Site::new(site),
            reason: "missing_attribution".into(),
        })?;
    let invalid = |reason: String| Error::InvalidQuery {
        site: Site::new(site),
        reason,
    };
    let level: InsightsLevel = q
        .level
        .as_deref()
        .unwrap_or("account")
        .parse()
        .map_err(invalid)?;
    let attribution: AttributionWindow = attribution.parse().map_err(invalid)?;
    let metrics = match q.metrics.as_deref() {
        Some(raw) if !raw.trim().is_empty() => raw
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(|item| item.parse())
            .collect::<Result<Vec<Metric>, String>>()
            .map_err(invalid)?,
        _ => vec![
            Metric::Spend,
            Metric::Impressions,
            Metric::Clicks,
            Metric::Purchases,
        ],
    };
    let mut entity_ids = q.entity_id.clone();
    entity_ids.retain(|id| !id.is_empty());
    let breakdowns = match q.breakdowns.as_deref() {
        Some(raw) if !raw.trim().is_empty() => raw
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(|item| item.parse())
            .collect::<Result<Vec<Breakdown>, String>>()
            .map_err(invalid)?,
        _ => vec![],
    };
    Ok(InsightsQuery {
        level,
        metrics,
        range: DateRange {
            from: from.into(),
            to: to.into(),
        },
        attribution,
        account: q.ad_account.clone(),
        entity_ids,
        breakdowns,
        report: q
            .report
            .as_deref()
            .unwrap_or("performance")
            .parse()
            .map_err(invalid)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode};
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
        phone_ids: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Publisher for WaMock {
        fn site(&self) -> &Site {
            &self.site
        }
        fn capabilities(&self) -> &[Capability] {
            &[
                Capability::SendReply,
                Capability::SendText,
                Capability::SendTemplate,
                Capability::SendMedia,
            ]
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
            app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _request: &WhatsAppSendRequest,
            _deadline: Deadline,
        ) -> Result<Outcome, Error> {
            if let Some(phone) = app
                .extra
                .get("phone_number_id")
                .and_then(|value| value.as_str())
            {
                self.phone_ids.lock().expect("phone ids").push(phone.into());
            }
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
            phone_ids: std::sync::Mutex::new(Vec::new()),
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
        postkit::AppStore::put(
            &*apps,
            &postkit::AppConfig {
                site: Site::new("whatsapp_cloud"),
                oauth: None,
                extra: serde_json::json!({
                    "phone_number_id": "123456789",
                    "senders": [{ "alias": "marketing", "phone_number_id": "987654321" }],
                    "app_secret": "webhook-secret",
                    "verify_token": "verify-me",
                }),
            },
        )
        .unwrap();
        let ledger = Arc::new(postkit::MemoryWhatsAppLedger::new());
        let deny =
            Client::new(registry, vault.clone(), apps.clone()).with_whatsapp_ledger(ledger.clone());
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

    #[test]
    fn listen_document_says_the_process_stays_up() {
        let addr = parse_bind(None).unwrap();
        let doc = listen_document(addr);
        assert_eq!(doc["bind"], addr.to_string());
        assert_eq!(doc["listening"], true);
        assert_eq!(doc["pid"], std::process::id());
        assert!(doc.get("ok").is_none());
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
    async fn whatsapp_http_routes_a_configured_sender_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, token, mock) = test_router(tmp.path());
        let body = r#"{
            "allow_send": true,
            "sender": "marketing",
            "idempotency_key": "marketing-1",
            "message": {
                "type": "reply",
                "to": "60123456789",
                "reply_to_message_id": "wamid.in",
                "text": "hi"
            }
        }"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/whatsapp")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            *mock.phone_ids.lock().expect("phone ids"),
            vec!["987654321".to_string()]
        );
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

    fn webhook_sig(raw: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"webhook-secret").unwrap();
        mac.update(raw);
        let bytes = mac.finalize().into_bytes();
        format!(
            "sha256={}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        )
    }

    #[tokio::test]
    async fn http_webhook_parse_is_hmac_not_a_listener() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, token, _) = test_router(tmp.path());
        let raw = br#"{"object":"whatsapp_business_account","entry":[{"changes":[{"field":"messages","value":{"metadata":{"phone_number_id":"123456789"},"messages":[{"from":"1","id":"wamid.in","type":"text","text":{"body":"hi"}}]}}]}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/whatsapp/webhook")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("x-hub-signature-256", webhook_sig(raw))
                    .body(Body::from(raw.as_ref()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["messages"][0]["id"], "wamid.in");
        assert!(v.get("ok").is_none());
    }

    #[tokio::test]
    async fn meta_callback_get_echoes_challenge_without_bearer() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, _, _) = test_router(tmp.path());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/whatsapp/callback?hub.mode=subscribe&hub.verify_token=verify-me&hub.challenge=1158")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(&bytes[..], b"1158");
    }

    #[tokio::test]
    async fn meta_callback_post_acks_200_and_rejects_bad_hmac() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, token, _) = test_router(tmp.path());
        let raw = br#"{"object":"whatsapp_business_account","entry":[{"changes":[{"field":"messages","value":{"metadata":{"phone_number_id":"123456789"},"messages":[{"from":"1","id":"wamid.in","type":"text","text":{"body":"hi"}}]}}]}]}"#;
        let forbidden = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/whatsapp/callback")
                    .header("x-hub-signature-256", "sha256=00")
                    .body(Body::from(raw.as_ref()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        let ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/whatsapp/callback")
                    .header("x-hub-signature-256", webhook_sig(raw))
                    .body(Body::from(raw.as_ref()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let got = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/whatsapp/events/wamid.in")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(got.status(), StatusCode::OK);
    }

    #[test]
    fn status_extras_flag_accepts_header_or_query() {
        let mut headers = HeaderMap::new();
        assert!(!status_extras_requested(&headers, false));
        assert!(status_extras_requested(&headers, true));
        headers.insert("x-postkit-status-extras", "true".parse().unwrap());
        assert!(status_extras_requested(&headers, false));
    }

    #[test]
    fn serve_uses_the_kernel_file_client() {
        // CLI and serve must not each assemble a Registry. from_home is the
        // shared operator factory; this crate only wraps it in HTTP.
        let tmp = tempfile::tempdir().unwrap();
        let client = Client::from_home(tmp.path(), false).unwrap();
        assert!(client.registry().get(&Site::new("threads")).is_some());
        assert!(client
            .registry()
            .get(&Site::new("whatsapp_cloud"))
            .is_some());
    }

    #[tokio::test]
    async fn ads_read_routes_require_a_key_and_typed_query() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, token, _) = test_router(tmp.path());
        let unauth = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/ads/accounts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

        let missing_entity = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/ads/list?site=meta_ads")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_entity.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let missing_from = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/insights?to=2026-06-30&attribution=1d_click")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_from.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // pk_live_ is not a spend key: ads routes are GET-only.
        let post_list = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/ads/list?site=meta_ads&entity=campaign")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post_list.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    struct AdsReadMock {
        site: Site,
    }

    #[async_trait]
    impl Publisher for AdsReadMock {
        fn site(&self) -> &Site {
            &self.site
        }
        fn capabilities(&self) -> &[Capability] {
            &[
                Capability::ReadAdAccounts,
                Capability::ReadAdsInventory,
                Capability::ReadAdReviewStatus,
                Capability::ReadMetrics,
            ]
        }
        fn auth_kind(&self) -> AuthKind {
            AuthKind::OAuth2AuthCode
        }
        async fn publish(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _intent: postkit::Intent,
            _deadline: Deadline,
        ) -> Result<Outcome, Error> {
            Err(Error::UnsupportedCapability {
                site: self.site.clone(),
                need: Capability::PublishText,
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
    impl postkit::InsightsSource for AdsReadMock {
        async fn insights(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _query: &postkit::InsightsQuery,
            _deadline: Deadline,
        ) -> Result<postkit::InsightsReply, Error> {
            Ok(postkit::InsightsReply {
                site: self.site.clone(),
                account_id: "act_1".into(),
                currency: None,
                rows: vec![],
            })
        }
        async fn ad_accounts(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _deadline: Deadline,
        ) -> Result<postkit::AdAccountsReply, Error> {
            Ok(postkit::AdAccountsReply {
                site: self.site.clone(),
                accounts: vec![postkit::AdAccount {
                    id: "act_1".into(),
                    name: Some("Test".into()),
                    currency: None,
                    timezone: None,
                    status: None,
                }],
            })
        }
    }

    #[async_trait]
    impl postkit::AdsManager for AdsReadMock {
        async fn create_paused_ad(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _request: &postkit::CreatePausedAdRequest,
            _deadline: Deadline,
        ) -> Result<postkit::CreatedAd, Error> {
            Err(Error::UnsupportedCapability {
                site: self.site.clone(),
                need: Capability::CreatePausedAds,
            })
        }
        async fn upload_ad_image(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _request: &postkit::UploadAdImageRequest,
            _deadline: Deadline,
        ) -> Result<postkit::UploadedAdImage, Error> {
            Err(Error::UnsupportedCapability {
                site: self.site.clone(),
                need: Capability::CreatePausedAds,
            })
        }
        async fn upload_ad_video(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _request: &postkit::UploadAdVideoRequest,
            _deadline: Deadline,
        ) -> Result<postkit::UploadedAdVideo, Error> {
            Err(Error::UnsupportedCapability {
                site: self.site.clone(),
                need: Capability::CreatePausedAds,
            })
        }
        async fn ad_video_status(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _request: &postkit::AdVideoStatusRequest,
            _deadline: Deadline,
        ) -> Result<postkit::AdVideoStatus, Error> {
            Err(Error::UnsupportedCapability {
                site: self.site.clone(),
                need: Capability::CreatePausedAds,
            })
        }
        async fn create_link_ad_creative(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _request: &postkit::CreateLinkAdCreativeRequest,
            _deadline: Deadline,
        ) -> Result<postkit::CreatedAdCreative, Error> {
            Err(Error::UnsupportedCapability {
                site: self.site.clone(),
                need: Capability::CreateAdCreative,
            })
        }
        async fn create_video_ad_creative(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _request: &postkit::CreateVideoAdCreativeRequest,
            _deadline: Deadline,
        ) -> Result<postkit::CreatedAdCreative, Error> {
            Err(Error::UnsupportedCapability {
                site: self.site.clone(),
                need: Capability::CreateAdCreative,
            })
        }
        async fn create_ad_creative(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _request: &postkit::CreateAdCreativeRequest,
            _deadline: Deadline,
        ) -> Result<postkit::CreatedAdCreative, Error> {
            Err(Error::UnsupportedCapability {
                site: self.site.clone(),
                need: Capability::CreateAdCreative,
            })
        }
        async fn preview_ad_creative(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            _request: &postkit::CreativePreviewRequest,
            _deadline: Deadline,
        ) -> Result<postkit::CreativePreview, Error> {
            Err(Error::UnsupportedCapability {
                site: self.site.clone(),
                need: Capability::CreateAdCreative,
            })
        }
        async fn ad_review_status(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            request: &postkit::AdReviewStatusRequest,
            _deadline: Deadline,
        ) -> Result<postkit::AdReviewStatus, Error> {
            Ok(postkit::AdReviewStatus {
                site: self.site.clone(),
                entity: request.entity,
                id: request.id.clone(),
                name: Some("Paused".into()),
                configured_status: "PAUSED".into(),
                effective_status: "PAUSED".into(),
                issues: vec![],
            })
        }
        async fn list_ads_inventory(
            &self,
            _app: &postkit::AppConfig,
            _creds: &AccountCreds,
            request: &postkit::AdsInventoryRequest,
            _deadline: Deadline,
        ) -> Result<postkit::AdsInventoryReply, Error> {
            Ok(postkit::AdsInventoryReply {
                site: self.site.clone(),
                account_id: "act_1".into(),
                kind: request.kind,
                items: vec![],
            })
        }
    }

    fn ads_read_router(tmp: &Path) -> (Router, String) {
        let keys = FileKeyStore::new(tmp).unwrap();
        let created = keys.create("n8n").unwrap();
        let mock = Arc::new(AdsReadMock {
            site: Site::new("meta_ads"),
        });
        let mut registry = Registry::new();
        registry.register_connector(
            Connector::from_publisher(mock.clone())
                .ads(mock.clone())
                .insights(mock.clone()),
        );
        let vault = Arc::new(MemoryVault::new());
        vault
            .put(
                &AccountKey::new("meta_ads", "default"),
                &AccountCreds::OAuth2 {
                    access_token: "tok".into(),
                    refresh_token: None,
                    extra: serde_json::json!({}),
                },
            )
            .unwrap();
        let client = Arc::new(Client::new(
            registry,
            vault,
            Arc::new(MemoryAppStore::new()),
        ));
        (
            router(client.clone(), client, Arc::new(keys)),
            created.token,
        )
    }

    #[tokio::test]
    async fn ads_read_routes_succeed_with_a_mock_connector() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, token) = ads_read_router(tmp.path());
        let accounts = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/ads/accounts?site=meta_ads")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accounts.status(), StatusCode::OK);

        let listed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/ads/list?site=meta_ads&entity=campaign")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);

        let status = app
            .oneshot(
                Request::builder()
                    .uri("/v1/ads/status?site=meta_ads&entity=adset&id=456")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
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
