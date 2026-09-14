//! Small, reviewable Threads scheduling application built on Wirekern.
//!
//! This is intentionally a separate product surface from the local
//! `wirekern-serve` API. It owns browser sessions, scheduled-post state,
//! deletion callbacks, and the user-facing consent screen; Wirekern remains
//! the narrow OAuth and publish kernel beneath it.

use axum::extract::{Form, Path as PathParam, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use getrandom::fill as random_fill;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wirekern::{
    AccountKey, AuthReply, AuthStart, AuthStartOptions, Body, Client, Deadline, Error, Intent, Site,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

type HmacSha256 = Hmac<Sha256>;

const SITE: &str = "threads";
const SESSION_COOKIE: &str = "wirekern_threads_session";
const OAUTH_COOKIE: &str = "wirekern_threads_oauth";
const SESSION_TTL_SECS: u64 = 30 * 24 * 60 * 60;
const PENDING_TTL_SECS: u64 = 15 * 60;
const CONFIRMATION_TTL_SECS: u64 = 30 * 24 * 60 * 60;
const MAX_SCHEDULE_AHEAD_SECS: u64 = 366 * 24 * 60 * 60;
const WORKER_TICK: Duration = Duration::from_secs(5);

/// Run the reviewable Threads application.
///
/// `public_url` must be the public HTTPS origin of the app, such as
/// `https://threads.example.com`. Set `WIREKERN_THREADS_REDIRECT_URI` to its
/// exact `/auth/threads/callback` URL; this strict match prevents a deploy
/// from accidentally using a test redirect or a different OAuth client.
pub async fn run_threads_app(
    data_dir: PathBuf,
    public_url: String,
    bind: SocketAddr,
) -> Result<(), Error> {
    let config = ReviewConfig::from_environment(public_url, data_dir.clone())?;
    let store = FileStore::open(&data_dir)?;
    store.recover_interrupted_jobs()?;

    // This uses the normal file vault, isolated under the app data directory.
    // The Threads App secret stays in process environment, never in HTML,
    // cookies, or the browser bundle.
    let client = Arc::new(Client::from_home(data_dir.join("wirekern"), false)?);
    let state = ReviewState {
        client,
        store,
        config,
    };

    tokio::spawn(schedule_worker(state.clone()));
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| Error::Network {
            site: Site::new(SITE),
            message: "threads_app_bind_failed".into(),
        })?;
    eprintln!(
        "Threads review app listening on http://{bind}; terminate HTTPS at your reverse proxy"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|_| Error::Network {
            site: Site::new(SITE),
            message: "threads_app_server_failed".into(),
        })
}

pub fn router(state: ReviewState) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/connect/threads", get(connect_threads))
        .route("/auth/threads/callback", get(threads_callback))
        .route("/posts", post(schedule_post))
        .route("/posts/{id}/cancel", post(cancel_post))
        .route("/disconnect", post(disconnect))
        .route("/privacy", get(privacy))
        .route("/deauthorize", post(deauthorize))
        .route("/data-deletion", post(data_deletion))
        .route("/data-deletion/{code}", get(data_deletion_status))
        .route("/healthz", get(healthz))
        .with_state(state)
}

#[derive(Clone)]
pub struct ReviewState {
    client: Arc<Client>,
    store: FileStore,
    config: ReviewConfig,
}

/// Private deployment configuration. It intentionally does not implement
/// `Debug`: the app secret must never be included in a diagnostic dump.
#[derive(Clone)]
struct ReviewConfig {
    public_url: String,
    data_dir: PathBuf,
    support_email: String,
    app_secret: String,
}

impl ReviewConfig {
    fn from_environment(public_url: String, data_dir: PathBuf) -> Result<Self, Error> {
        let public_url = normalize_public_url(&public_url)?;
        let expected_redirect = format!("{public_url}/auth/threads/callback");
        let redirect = nonempty_env("WIREKERN_THREADS_REDIRECT_URI")?;
        if redirect != expected_redirect {
            return Err(Error::Auth {
                site: Site::new(SITE),
                reason: "threads_app_redirect_uri_mismatch".into(),
            });
        }
        // Read both values here so startup fails before accepting a browser
        // request. Client::from_home reads the same variables when OAuth
        // begins, but an early check gives operators one actionable failure.
        let _ = nonempty_env("WIREKERN_THREADS_CLIENT_ID")?;
        let app_secret = nonempty_env("WIREKERN_THREADS_CLIENT_SECRET")?;
        let support_email = support_email()?;
        Ok(Self {
            public_url,
            data_dir,
            support_email,
            app_secret,
        })
    }
}

fn nonempty_env(name: &str) -> Result<String, Error> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: format!("missing_{name}"),
        })
}

fn support_email() -> Result<String, Error> {
    let email = nonempty_env("WIREKERN_THREADS_SUPPORT_EMAIL")?;
    is_valid_support_email(&email)
        .then_some(email)
        .ok_or_else(|| invalid_config("invalid_threads_support_email"))
}

fn is_valid_support_email(email: &str) -> bool {
    email.bytes().filter(|byte| *byte == b'@').count() == 1
        && !email.starts_with('@')
        && !email.ends_with('@')
        && !email.chars().any(char::is_whitespace)
}

fn normalize_public_url(value: &str) -> Result<String, Error> {
    let origin = value.trim().trim_end_matches('/');
    let Some(authority) = origin.strip_prefix("https://") else {
        return Err(invalid_config("public_url_must_be_https_origin"));
    };
    if authority.is_empty()
        || authority.contains(['/', '?', '#', '@'])
        || authority.chars().any(char::is_whitespace)
    {
        return Err(invalid_config("public_url_must_be_https_origin"));
    }
    Ok(origin.to_string())
}

fn invalid_config(reason: &str) -> Error {
    Error::InvalidQuery {
        site: Site::new(SITE),
        reason: reason.into(),
    }
}

#[derive(Clone)]
struct FileStore {
    path: Arc<PathBuf>,
    inner: Arc<Mutex<PersistentState>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistentState {
    #[serde(default)]
    pending: BTreeMap<String, PendingOAuth>,
    #[serde(default)]
    sessions: BTreeMap<String, BrowserSession>,
    #[serde(default)]
    accounts: BTreeMap<String, ConnectedAccount>,
    #[serde(default)]
    posts: BTreeMap<String, ScheduledPost>,
    #[serde(default)]
    deletions: BTreeMap<String, DeletionConfirmation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingOAuth {
    account: String,
    browser_nonce: String,
    expires_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BrowserSession {
    account: String,
    csrf: String,
    expires_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ConnectedAccount {
    threads_user_id: String,
    username: String,
    connected_at: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PublishStatus {
    Scheduled,
    Publishing,
    Published,
    Failed,
    Cancelled,
    NeedsReview,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ScheduledPost {
    id: String,
    account: String,
    text: String,
    scheduled_at: u64,
    approved_at: u64,
    status: PublishStatus,
    attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DeletionConfirmation {
    user_id: String,
    completed_at: u64,
    expires_at: u64,
}

impl FileStore {
    fn open(root: &FsPath) -> Result<Self, Error> {
        ensure_private_dir(root)?;
        let path = root.join("threads-review-state.json");
        let state = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                PersistentState::default()
            }
            Err(error) => return Err(error.into()),
        };
        let store = Self {
            path: Arc::new(path),
            inner: Arc::new(Mutex::new(state)),
        };
        // Create an owner-only state file at first startup rather than
        // leaving scheduled post text in a process-default-permission file.
        store.save_current()?;
        Ok(store)
    }

    fn save_current(&self) -> Result<(), Error> {
        let state = self.inner.lock().expect("threads review state");
        atomic_write_private(&self.path, &serde_json::to_vec_pretty(&*state)?)
    }

    fn change<T>(&self, f: impl FnOnce(&mut PersistentState) -> T) -> Result<T, Error> {
        let mut state = self.inner.lock().expect("threads review state");
        prune_expired(&mut state, unix_now());
        let out = f(&mut state);
        atomic_write_private(&self.path, &serde_json::to_vec_pretty(&*state)?)?;
        Ok(out)
    }

    fn add_pending(
        &self,
        state_token: String,
        account: String,
        browser_nonce: String,
    ) -> Result<(), Error> {
        let now = unix_now();
        self.change(|state| {
            state.pending.insert(
                state_token,
                PendingOAuth {
                    account,
                    browser_nonce,
                    expires_at: now + PENDING_TTL_SECS,
                },
            );
        })
    }

    fn take_pending(&self, state_token: &str) -> Result<Option<PendingOAuth>, Error> {
        let now = unix_now();
        self.change(|state| {
            state
                .pending
                .remove(state_token)
                .filter(|pending| pending.expires_at >= now)
        })
    }

    fn complete_connection(
        &self,
        account: String,
        threads_user_id: String,
        username: String,
    ) -> Result<Vec<String>, Error> {
        let now = unix_now();
        self.change(|state| {
            // Reconnecting the same Threads account replaces the old local
            // token alias and scheduled content. One account must never have
            // two silent schedulers behind it.
            let stale: Vec<String> = state
                .accounts
                .iter()
                .filter(|(alias, value)| {
                    *alias != &account && value.threads_user_id == threads_user_id
                })
                .map(|(alias, _)| alias.clone())
                .collect();
            for alias in &stale {
                state.accounts.remove(alias);
                state.posts.retain(|_, post| post.account != *alias);
                state
                    .sessions
                    .retain(|_, session| session.account != *alias);
            }
            state.accounts.insert(
                account,
                ConnectedAccount {
                    threads_user_id,
                    username,
                    connected_at: now,
                },
            );
            stale
        })
    }

    fn create_session(&self, account: String) -> Result<(String, BrowserSession), Error> {
        let token = random_token()?;
        let csrf = random_token()?;
        let session = BrowserSession {
            account,
            csrf,
            expires_at: unix_now() + SESSION_TTL_SECS,
        };
        self.change(|state| {
            state.sessions.insert(token.clone(), session.clone());
        })?;
        Ok((token, session))
    }

    fn session(&self, token: &str) -> Option<BrowserSession> {
        let now = unix_now();
        self.inner
            .lock()
            .expect("threads review state")
            .sessions
            .get(token)
            .filter(|session| session.expires_at >= now)
            .cloned()
    }

    fn dashboard(&self, account: &str) -> Option<(ConnectedAccount, Vec<ScheduledPost>)> {
        let state = self.inner.lock().expect("threads review state");
        let connected = state.accounts.get(account)?.clone();
        let mut posts: Vec<ScheduledPost> = state
            .posts
            .values()
            .filter(|post| post.account == account)
            .cloned()
            .collect();
        posts.sort_by_key(|post| std::cmp::Reverse(post.scheduled_at));
        Some((connected, posts))
    }

    fn schedule(&self, post: ScheduledPost) -> Result<(), Error> {
        self.change(|state| {
            state.posts.insert(post.id.clone(), post);
        })
    }

    fn cancel(&self, account: &str, id: &str) -> Result<bool, Error> {
        self.change(|state| {
            let Some(post) = state.posts.get_mut(id) else {
                return false;
            };
            if post.account != account || post.status != PublishStatus::Scheduled {
                return false;
            }
            post.status = PublishStatus::Cancelled;
            true
        })
    }

    fn disconnect_account(&self, account: &str) -> Result<bool, Error> {
        self.change(|state| {
            let existed = state.accounts.remove(account).is_some();
            state.posts.retain(|_, post| post.account != account);
            state
                .sessions
                .retain(|_, session| session.account != account);
            existed
        })
    }

    fn delete_by_user(&self, threads_user_id: &str, code: String) -> Result<Vec<String>, Error> {
        let now = unix_now();
        self.change(|state| {
            let aliases: Vec<String> = state
                .accounts
                .iter()
                .filter(|(_, account)| account.threads_user_id == threads_user_id)
                .map(|(alias, _)| alias.clone())
                .collect();
            for alias in &aliases {
                state.accounts.remove(alias);
                state.posts.retain(|_, post| post.account != *alias);
                state
                    .sessions
                    .retain(|_, session| session.account != *alias);
            }
            state.deletions.insert(
                code,
                DeletionConfirmation {
                    user_id: threads_user_id.into(),
                    completed_at: now,
                    expires_at: now + CONFIRMATION_TTL_SECS,
                },
            );
            aliases
        })
    }

    fn deletion_confirmation(&self, code: &str) -> bool {
        let now = unix_now();
        self.inner
            .lock()
            .expect("threads review state")
            .deletions
            .get(code)
            .is_some_and(|confirmation| confirmation.expires_at >= now)
    }

    fn claim_due(&self, now: u64) -> Result<Vec<ScheduledPost>, Error> {
        self.change(|state| {
            let ids: Vec<String> = state
                .posts
                .iter()
                .filter(|(_, post)| {
                    post.status == PublishStatus::Scheduled && post.scheduled_at <= now
                })
                .map(|(id, _)| id.clone())
                .collect();
            let mut due = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(post) = state.posts.get_mut(&id) {
                    post.status = PublishStatus::Publishing;
                    post.attempts = post.attempts.saturating_add(1);
                    due.push(post.clone());
                }
            }
            due
        })
    }

    fn finish_post(&self, id: &str, result: Result<Option<String>, &Error>) -> Result<(), Error> {
        self.change(|state| {
            let Some(post) = state.posts.get_mut(id) else {
                return;
            };
            if post.status != PublishStatus::Publishing {
                return;
            }
            match result {
                Ok(url) => {
                    post.status = PublishStatus::Published;
                    post.outcome_url = url;
                    post.error = None;
                }
                Err(error) => {
                    // Never auto-retry an uncertain public write. A network
                    // loss after Meta accepted the post is indistinguishable
                    // from a failure, so blindly retrying can double-post.
                    post.status = PublishStatus::Failed;
                    post.error = Some(safe_publish_error(error).into());
                }
            }
        })
    }

    fn recover_interrupted_jobs(&self) -> Result<(), Error> {
        self.change(|state| {
            for post in state.posts.values_mut() {
                if post.status == PublishStatus::Publishing {
                    post.status = PublishStatus::NeedsReview;
                    post.error = Some(
                        "The service stopped while the publish outcome was unknown. It was not retried automatically to avoid a duplicate post."
                            .into(),
                    );
                }
            }
        })
    }
}

fn prune_expired(state: &mut PersistentState, now: u64) {
    state.pending.retain(|_, pending| pending.expires_at >= now);
    state
        .sessions
        .retain(|_, session| session.expires_at >= now);
    state
        .deletions
        .retain(|_, confirmation| confirmation.expires_at >= now);
}

fn ensure_private_dir(path: &FsPath) -> Result<(), Error> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn atomic_write_private(path: &FsPath, bytes: &[u8]) -> Result<(), Error> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_config("state_path_parent"))?;
    ensure_private_dir(parent)?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    #[cfg(unix)]
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    #[cfg(not(unix))]
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(tmp, path)?;
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn random_token() -> Result<String, Error> {
    let mut bytes = [0u8; 24];
    random_fill(&mut bytes).map_err(|_| Error::Network {
        site: Site::new(SITE),
        message: "secure_random_unavailable".into(),
    })?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

async fn home(
    State(state): State<ReviewState>,
    headers: HeaderMap,
    Query(query): Query<HomeQuery>,
) -> Response {
    let Some(session) = session_from_headers(&state, &headers) else {
        return html_response(
            StatusCode::OK,
            "Threads publisher",
            &landing_body(query.error.is_some()),
        );
    };
    let Some((account, posts)) = state.store.dashboard(&session.account) else {
        return html_response(StatusCode::OK, "Threads publisher", &landing_body(false));
    };
    html_response(
        StatusCode::OK,
        "Threads publisher",
        &dashboard_body(&session, &account, &posts, query.error.is_some()),
    )
}

#[derive(Deserialize)]
struct HomeQuery {
    error: Option<String>,
}

async fn connect_threads(State(state): State<ReviewState>) -> Response {
    let alias = match random_token() {
        Ok(token) => format!("threads-{token}"),
        Err(_) => {
            return html_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Unavailable",
                &retry_body(),
            )
        }
    };
    let key = AccountKey::new(SITE, &alias);
    let browser_nonce = match random_token() {
        Ok(token) => token,
        Err(_) => {
            return html_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Unavailable",
                &retry_body(),
            )
        }
    };
    let start = match state
        .client
        .auth_start_for(&key, AuthStartOptions::default())
        .await
    {
        Ok(start) => start,
        Err(_) => {
            return html_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Unavailable",
                &retry_body(),
            )
        }
    };
    let AuthStart::Browser {
        authorize_url,
        state: oauth_state,
        ..
    } = start
    else {
        return html_response(StatusCode::BAD_GATEWAY, "Connection error", &retry_body());
    };
    if state
        .store
        .add_pending(oauth_state, alias, browser_nonce.clone())
        .is_err()
    {
        return html_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Unavailable",
            &retry_body(),
        );
    }
    response_with_cookies(
        Redirect::temporary(&authorize_url).into_response(),
        &[oauth_cookie(&browser_nonce)],
    )
}

#[derive(Deserialize)]
struct OAuthCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn threads_callback(
    State(app): State<ReviewState>,
    headers: HeaderMap,
    Query(callback): Query<OAuthCallback>,
) -> Response {
    let Some(oauth_state) = callback.state.as_deref() else {
        return redirect_with_cookies("/?error=connection", &[expired_oauth_cookie()]);
    };
    let pending = match app.store.take_pending(oauth_state) {
        Ok(Some(pending)) => pending,
        _ => return redirect_with_cookies("/?error=connection", &[expired_oauth_cookie()]),
    };
    if cookie_value(&headers, OAUTH_COOKIE).as_deref() != Some(pending.browser_nonce.as_str()) {
        return redirect_with_cookies("/?error=connection", &[expired_oauth_cookie()]);
    }
    if callback.error.is_some() {
        return redirect_with_cookies("/?error=connection", &[expired_oauth_cookie()]);
    }
    let Some(code) = callback.code else {
        return redirect_with_cookies("/?error=connection", &[expired_oauth_cookie()]);
    };
    let key = AccountKey::new(SITE, &pending.account);
    let identity = match app
        .client
        .auth_finish(&key, AuthReply::Pasted { code })
        .await
    {
        Ok(identity) => identity,
        Err(_) => return redirect_with_cookies("/?error=connection", &[expired_oauth_cookie()]),
    };
    let username = identity
        .handle
        .unwrap_or_else(|| "Connected Threads account".into());
    let stale = match app
        .store
        .complete_connection(pending.account.clone(), identity.id, username)
    {
        Ok(stale) => stale,
        Err(_) => {
            let _ = app.client.vault().delete(&key);
            return redirect_with_cookies("/?error=connection", &[expired_oauth_cookie()]);
        }
    };
    for alias in stale {
        erase_account_data(&app, &alias);
    }
    let (session_token, _) = match app.store.create_session(pending.account) {
        Ok(session) => session,
        Err(_) => return redirect_with_cookies("/?error=connection", &[expired_oauth_cookie()]),
    };
    redirect_with_cookies(
        "/",
        &[session_cookie(&session_token), expired_oauth_cookie()],
    )
}

#[derive(Deserialize)]
struct ScheduleForm {
    csrf: String,
    text: String,
    scheduled_at: String,
    approved: Option<String>,
}

async fn schedule_post(
    State(app): State<ReviewState>,
    headers: HeaderMap,
    Form(form): Form<ScheduleForm>,
) -> Response {
    let Some(session) = session_from_headers(&app, &headers) else {
        return redirect_with_cookie("/", None);
    };
    if form.csrf != session.csrf || form.approved.as_deref() != Some("on") {
        return html_response(StatusCode::FORBIDDEN, "Request rejected", &csrf_body());
    }
    let text = form.text.trim().to_string();
    let now = unix_now();
    let scheduled_at = form.scheduled_at.parse::<u64>().ok();
    let Some(scheduled_at) = scheduled_at else {
        return redirect_with_cookie("/?error=schedule", None);
    };
    if text.is_empty()
        || scheduled_at > now.saturating_add(MAX_SCHEDULE_AHEAD_SECS)
        || scheduled_at.saturating_add(60) < now
    {
        return redirect_with_cookie("/?error=schedule", None);
    }
    let id = match random_token() {
        Ok(id) => id,
        Err(_) => {
            return html_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Unavailable",
                &retry_body(),
            )
        }
    };
    let job = ScheduledPost {
        id,
        account: session.account,
        text,
        scheduled_at: scheduled_at.max(now),
        approved_at: now,
        status: PublishStatus::Scheduled,
        attempts: 0,
        outcome_url: None,
        error: None,
    };
    match app.store.schedule(job) {
        Ok(()) => redirect_with_cookie("/", None),
        Err(_) => html_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Unavailable",
            &retry_body(),
        ),
    }
}

#[derive(Deserialize)]
struct CsrfForm {
    csrf: String,
}

async fn cancel_post(
    State(app): State<ReviewState>,
    headers: HeaderMap,
    PathParam(id): PathParam<String>,
    Form(form): Form<CsrfForm>,
) -> Response {
    let Some(session) = session_from_headers(&app, &headers) else {
        return redirect_with_cookie("/", None);
    };
    if form.csrf != session.csrf {
        return html_response(StatusCode::FORBIDDEN, "Request rejected", &csrf_body());
    }
    let _ = app.store.cancel(&session.account, &id);
    redirect_with_cookie("/", None)
}

async fn disconnect(
    State(app): State<ReviewState>,
    headers: HeaderMap,
    Form(form): Form<CsrfForm>,
) -> Response {
    let Some(session) = session_from_headers(&app, &headers) else {
        return redirect_with_cookie("/", Some(&expired_session_cookie()));
    };
    if form.csrf != session.csrf {
        return html_response(StatusCode::FORBIDDEN, "Request rejected", &csrf_body());
    }
    let _ = app.store.disconnect_account(&session.account);
    erase_account_data(&app, &session.account);
    redirect_with_cookie("/", Some(&expired_session_cookie()))
}

async fn privacy(State(app): State<ReviewState>) -> Response {
    html_response(
        StatusCode::OK,
        "Privacy policy",
        &privacy_body(&app.config.public_url, &app.config.support_email),
    )
}

async fn deauthorize(
    State(app): State<ReviewState>,
    Form(form): Form<DeletionRequest>,
) -> Response {
    let Ok(user_id) = verify_signed_request(&form.signed_request, &app.config.app_secret) else {
        return secure_response(
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid_signed_request" })),
            )
                .into_response(),
        );
    };
    let code = match random_token() {
        Ok(code) => code,
        Err(_) => {
            return secure_response(
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({ "error": "temporary_failure" })),
                )
                    .into_response(),
            )
        }
    };
    let aliases = match app.store.delete_by_user(&user_id, code) {
        Ok(aliases) => aliases,
        Err(_) => {
            return secure_response(
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({ "error": "temporary_failure" })),
                )
                    .into_response(),
            )
        }
    };
    for alias in aliases {
        erase_account_data(&app, &alias);
    }
    secure_response((StatusCode::OK, Json(json!({ "success": true }))).into_response())
}

#[derive(Deserialize)]
struct DeletionRequest {
    signed_request: String,
}

async fn data_deletion(
    State(app): State<ReviewState>,
    Form(form): Form<DeletionRequest>,
) -> Response {
    let Ok(user_id) = verify_signed_request(&form.signed_request, &app.config.app_secret) else {
        return secure_response(
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid_signed_request" })),
            )
                .into_response(),
        );
    };
    let code = match random_token() {
        Ok(code) => code,
        Err(_) => {
            return secure_response(
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({ "error": "temporary_failure" })),
                )
                    .into_response(),
            )
        }
    };
    let aliases = match app.store.delete_by_user(&user_id, code.clone()) {
        Ok(aliases) => aliases,
        Err(_) => {
            return secure_response(
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({ "error": "temporary_failure" })),
                )
                    .into_response(),
            )
        }
    };
    for alias in aliases {
        erase_account_data(&app, &alias);
    }
    let url = format!("{}/data-deletion/{code}", app.config.public_url);
    secure_response(
        (
            StatusCode::OK,
            Json(json!({ "url": url, "confirmation_code": code })),
        )
            .into_response(),
    )
}

async fn data_deletion_status(
    State(app): State<ReviewState>,
    PathParam(code): PathParam<String>,
) -> Response {
    if !app.store.deletion_confirmation(&code) {
        return html_response(StatusCode::NOT_FOUND, "Not found", &not_found_body());
    }
    html_response(
        StatusCode::OK,
        "Deletion complete",
        "<main><h1>Deletion complete</h1><p>All server-held connection credentials, scheduled posts, and browser sessions for this account have been deleted.</p></main>",
    )
}

async fn healthz() -> Response {
    secure_response((StatusCode::OK, Json(json!({ "ok": true }))).into_response())
}

fn session_from_headers(app: &ReviewState, headers: &HeaderMap) -> Option<BrowserSession> {
    let token = cookie_value(headers, SESSION_COOKIE)?;
    app.store.session(&token)
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|item| {
        let (key, value) = item.trim().split_once('=')?;
        (key == name && !value.is_empty()).then(|| value.to_string())
    })
}

fn session_cookie(value: &str) -> String {
    format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={SESSION_TTL_SECS}"
    )
}

fn oauth_cookie(value: &str) -> String {
    format!(
        "{OAUTH_COOKIE}={value}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={PENDING_TTL_SECS}"
    )
}

fn expired_session_cookie() -> String {
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0")
}

fn expired_oauth_cookie() -> String {
    format!("{OAUTH_COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0")
}

fn redirect_with_cookie(path: &str, cookie: Option<&str>) -> Response {
    let cookies: Vec<String> = cookie.into_iter().map(str::to_string).collect();
    redirect_with_cookies(path, &cookies)
}

fn redirect_with_cookies(path: &str, cookies: &[String]) -> Response {
    let response = Redirect::to(path).into_response();
    response_with_cookies(response, cookies)
}

fn response_with_cookies(mut response: Response, cookies: &[String]) -> Response {
    for cookie in cookies {
        if let Ok(value) = HeaderValue::from_str(cookie) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    secure_response(response)
}

fn html_response(status: StatusCode, title: &str, body: &str) -> Response {
    secure_response((status, Html(page(title, body))).into_response())
}

fn secure_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'",
        ),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{}</style></head><body>{body}</body></html>",
        escape_html(title),
        STYLE
    )
}

const STYLE: &str = "body{margin:0;background:#101014;color:#f5f3f8;font:16px system-ui,sans-serif}main{max-width:780px;margin:48px auto;padding:0 24px}a{color:#d3b6ff}h1{font-size:32px;margin-bottom:8px}.muted{color:#bcb6c6}.card{background:#1b1a20;border:1px solid #34313b;border-radius:14px;padding:20px;margin:22px 0}.button,button{display:inline-block;background:#f2e9ff;color:#211a2b;border:0;border-radius:9px;padding:11px 15px;font-weight:700;cursor:pointer}button.secondary{background:#332f3a;color:#f5f3f8}.danger{background:#562838!important;color:#fff!important}textarea,input{box-sizing:border-box;width:100%;border-radius:8px;border:1px solid #524d5a;background:#111015;color:#fff;padding:10px;font:inherit;margin:7px 0 14px}.check{display:flex;gap:9px;align-items:flex-start;margin:5px 0 16px}.check input{width:auto;margin:4px 0}.job{padding:14px 0;border-top:1px solid #34313b}.status{font-size:13px;border-radius:999px;background:#34313b;padding:3px 8px;margin-left:7px}.error{color:#ffbbb9}.row{display:flex;align-items:center;justify-content:space-between;gap:12px;flex-wrap:wrap}.small{font-size:13px}.text{white-space:pre-wrap}.notice{background:#43334b;padding:10px 12px;border-radius:8px}";

fn landing_body(show_error: bool) -> String {
    let notice = show_error.then_some(
        "<p class=\"notice\">The connection or schedule request could not be completed. Please try again.</p>"
    );
    format!(
        "<main><p class=\"muted\">Official Threads publishing</p><h1>Publish posts you approve, on the schedule you choose.</h1>{}<div class=\"card\"><p>Connect your own Threads account. This app only publishes text you create or explicitly approve to that same account.</p><a class=\"button\" href=\"/connect/threads\">Connect Threads</a></div><p class=\"small\"><a href=\"/privacy\">Privacy and data deletion</a></p></main>",
        notice.unwrap_or_default()
    )
}

fn dashboard_body(
    session: &BrowserSession,
    account: &ConnectedAccount,
    posts: &[ScheduledPost],
    show_error: bool,
) -> String {
    let notice = show_error.then_some(
        "<p class=\"notice\">That request could not be completed. Check the form and try again.</p>"
    );
    let jobs: String = if posts.is_empty() {
        "<p class=\"muted\">No scheduled posts yet.</p>".into()
    } else {
        posts
            .iter()
            .map(|post| job_body(post, &session.csrf))
            .collect()
    };
    format!(
        "<main><div class=\"row\"><div><p class=\"muted\">Connected account</p><h1>@{}</h1></div><form method=\"post\" action=\"/disconnect\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"danger\" type=\"submit\">Disconnect and delete data</button></form></div>{}<section class=\"card\"><h2>Schedule a text post</h2><p class=\"muted\">Nothing is published until the date and time you select.</p><form id=\"schedule-form\" method=\"post\" action=\"/posts\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><label for=\"post-text\">Post text</label><textarea id=\"post-text\" name=\"text\" maxlength=\"500\" required></textarea><label for=\"scheduled-local\">Publish time</label><input id=\"scheduled-local\" type=\"datetime-local\" required><input id=\"scheduled-at\" type=\"hidden\" name=\"scheduled_at\"><label class=\"check\"><input type=\"checkbox\" name=\"approved\" required>I approve publishing this exact text from my connected Threads account at the selected time.</label><button type=\"submit\">Approve and schedule</button></form></section><section class=\"card\"><h2>Scheduled posts</h2>{jobs}</section><p class=\"small\"><a href=\"/privacy\">Privacy and data deletion</a></p></main><script>{}</script>",
        escape_html(&account.username),
        escape_html(&session.csrf),
        notice.unwrap_or_default(),
        escape_html(&session.csrf),
        DASHBOARD_SCRIPT
    )
}

fn job_body(post: &ScheduledPost, csrf: &str) -> String {
    let status = match post.status {
        PublishStatus::Scheduled => "Scheduled",
        PublishStatus::Publishing => "Publishing",
        PublishStatus::Published => "Published",
        PublishStatus::Failed => "Failed",
        PublishStatus::Cancelled => "Cancelled",
        PublishStatus::NeedsReview => "Needs review",
    };
    let outcome = post.outcome_url.as_deref().map_or_else(String::new, |url| {
        format!("<p class=\"small\"><a href=\"{}\" rel=\"noopener\" target=\"_blank\">View published post</a></p>", escape_html(url))
    });
    let error = post.error.as_deref().map_or_else(String::new, |message| {
        format!("<p class=\"small error\">{}</p>", escape_html(message))
    });
    let cancel = (post.status == PublishStatus::Scheduled).then(|| {
        format!("<form method=\"post\" action=\"/posts/{}/cancel\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"secondary\" type=\"submit\">Cancel</button></form>", escape_html(&post.id), escape_html(csrf))
    });
    format!(
        "<article class=\"job\"><div class=\"row\"><div><strong class=\"when\" data-unix=\"{}\">{}</strong><span class=\"status\">{status}</span></div>{}</div><p class=\"text\">{}</p>{error}{outcome}</article>",
        post.scheduled_at,
        post.scheduled_at,
        cancel.unwrap_or_default(),
        escape_html(&post.text),
    )
}

const DASHBOARD_SCRIPT: &str = "const f=document.getElementById('schedule-form');const d=document.getElementById('scheduled-local');const h=document.getElementById('scheduled-at');if(d){const n=new Date(Date.now()+60000);n.setMinutes(n.getMinutes()-n.getTimezoneOffset());d.value=n.toISOString().slice(0,16);f.addEventListener('submit',()=>{h.value=String(Math.floor(new Date(d.value).getTime()/1000));});}document.querySelectorAll('.when').forEach(e=>{e.textContent=new Date(Number(e.dataset.unix)*1000).toLocaleString();});";

fn privacy_body(public_url: &str, support_email: &str) -> String {
    format!(
        "<main><h1>Privacy policy</h1><div class=\"card\"><p>This application lets an account owner connect their Threads account and schedule text they have explicitly approved.</p><h2>Data we process</h2><p>We store the Threads account ID and username, an access token, browser-session data, scheduled post text, selected publish times, and the published post URL when available.</p><h2>Why</h2><p>We use that data only to identify the connected account, display scheduled posts, and publish approved text to that same account at its selected time. We do not sell data, build advertising profiles, read replies, followers, or insights, or publish to another account.</p><h2>Retention and deletion</h2><p>Connection data and scheduled posts remain only until the account owner disconnects. Disconnecting deletes server-held credentials, sessions, and scheduled content. Meta data-deletion requests are accepted at <code>{}/data-deletion</code>. The account owner can also revoke access in Threads settings.</p><h2>Security</h2><p>Credentials and scheduler state are stored in owner-only server files. The Threads App secret stays server-side and is never delivered to a browser.</p><h2>Contact</h2><p>For privacy or account-deletion support, contact <a href=\"mailto:{}\">{}</a>.</p></div></main>",
        escape_html(public_url),
        escape_html(support_email),
        escape_html(support_email),
    )
}

fn retry_body() -> String {
    "<main><h1>Temporarily unavailable</h1><p>Please return to the app and try again.</p></main>"
        .into()
}

fn csrf_body() -> String {
    "<main><h1>Request rejected</h1><p>Refresh the page and try again.</p></main>".into()
}

fn not_found_body() -> String {
    "<main><h1>Not found</h1><p>This deletion confirmation is unavailable.</p></main>".into()
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

async fn schedule_worker(app: ReviewState) {
    let mut interval = tokio::time::interval(WORKER_TICK);
    loop {
        interval.tick().await;
        let due = match app.store.claim_due(unix_now()) {
            Ok(due) => due,
            Err(_) => continue,
        };
        for job in due {
            let key = AccountKey::new(SITE, &job.account);
            let intent = Intent {
                site: Site::new(SITE),
                params: json!({}),
                body: Body::Text {
                    text: job.text.clone(),
                },
                idempotency_key: Some(format!("threads-job-{}", job.id)),
            };
            let result = app
                .client
                .publish(&key, intent, Deadline::from_secs(30))
                .await;
            match &result {
                Ok(outcome) => {
                    let _ = app.store.finish_post(&job.id, Ok(outcome.url.clone()));
                }
                Err(error) => {
                    let _ = app.store.finish_post(&job.id, Err(error));
                }
            }
        }
    }
}

fn safe_publish_error(error: &Error) -> &'static str {
    match error {
        Error::Auth { .. } => "Threads access needs to be reconnected.",
        Error::RateLimited { .. } => "Threads rate-limited this post. It was not retried automatically.",
        Error::Network { .. } | Error::DeadlineExceeded { .. } => {
            "The publish outcome was uncertain. It was not retried automatically to avoid a duplicate post."
        }
        Error::InvalidPost { .. } => "Threads rejected this post. Edit it and schedule a new post.",
        _ => "Threads could not publish this post. It was not retried automatically.",
    }
}

fn erase_account_data(app: &ReviewState, alias: &str) {
    let key = AccountKey::new(SITE, alias);
    let _ = app.client.vault().delete(&key);
    // FileVault's trait can remove credentials but deliberately has no broad
    // destructive API. These are the exact app-owned, alias-scoped remnants
    // of an account; never resolve a user-controlled path here.
    let home = app.config.data_dir.join("wirekern");
    let account = home
        .join("accounts")
        .join(SITE)
        .join(format!("{alias}.json"));
    let session = home
        .join("oauth-sessions")
        .join(SITE)
        .join(format!("{alias}.json"));
    let outcomes = home.join("idempotency").join(SITE).join(alias);
    let _ = fs::remove_file(account);
    let _ = fs::remove_file(session);
    let _ = fs::remove_dir_all(outcomes);
}

fn verify_signed_request(signed_request: &str, app_secret: &str) -> Result<String, ()> {
    let (signature, payload) = signed_request.split_once('.').ok_or(())?;
    let signature = decode_base64url(signature)?;
    let payload_bytes = decode_base64url(payload)?;
    let mut mac = HmacSha256::new_from_slice(app_secret.as_bytes()).map_err(|_| ())?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&signature).map_err(|_| ())?;
    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).map_err(|_| ())?;
    let user_id = payload.get("user_id").ok_or(())?;
    match user_id {
        serde_json::Value::String(value) if !value.is_empty() => Ok(value.clone()),
        serde_json::Value::Number(value) => Ok(value.to_string()),
        _ => Err(()),
    }
}

fn decode_base64url(value: &str) -> Result<Vec<u8>, ()> {
    URL_SAFE_NO_PAD
        .decode(value)
        .or_else(|_| URL_SAFE.decode(value))
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_url_must_be_a_clean_https_origin() {
        assert_eq!(
            normalize_public_url("https://threads.example.com/").unwrap(),
            "https://threads.example.com"
        );
        assert!(normalize_public_url("http://threads.example.com").is_err());
        assert!(normalize_public_url("https://threads.example.com/path").is_err());
    }

    #[test]
    fn privacy_contact_must_look_like_an_email() {
        assert!(is_valid_support_email("support@example.com"));
        assert!(!is_valid_support_email("support example.com"));
        assert!(!is_valid_support_email("support@@example.com"));
    }

    #[test]
    fn signed_deletion_request_requires_a_valid_hmac() {
        let secret = "review-app-secret";
        let payload = URL_SAFE_NO_PAD.encode(br#"{"user_id":"1784"}"#);
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        assert_eq!(
            verify_signed_request(&format!("{signature}.{payload}"), secret).unwrap(),
            "1784"
        );
        assert!(verify_signed_request(&format!("{signature}.{payload}"), "wrong").is_err());
    }

    #[test]
    fn user_content_is_html_escaped() {
        assert_eq!(
            escape_html("<script>&\"'"),
            "&lt;script&gt;&amp;&quot;&#x27;"
        );
    }
}
