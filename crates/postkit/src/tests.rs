use crate::apps::{AppStore, MemoryAppStore};
use crate::client::Client;
use crate::error::Error;
use crate::insights::{
    AttributionWindow, InsightRow, InsightsLevel, InsightsQuery, InsightsReply, Metric,
};
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::registry::Registry;
use crate::types::{
    AccountCreds, AccountKey, AppConfig, Body, Capability, Deadline, Intent, Outcome, Probe, Site,
    WhoAmI,
};
use crate::vault::{MemoryVault, Vault};
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct MockPub {
    site: Site,
    caps: Vec<Capability>,
    auth_kind: AuthKind,
    fail_auth_once: bool,
    fail_publish: bool,
    whoami_fails: bool,
    refresh_network_err: bool,
    refresh_dead_session: bool,
    publishes: AtomicUsize,
    probes: AtomicUsize,
}

impl MockPub {
    fn text(site: &str) -> Self {
        Self {
            site: Site::new(site),
            caps: vec![Capability::PublishText],
            auth_kind: AuthKind::OAuth2AuthCode,
            fail_auth_once: false,
            fail_publish: false,
            whoami_fails: false,
            refresh_network_err: false,
            refresh_dead_session: false,
            publishes: AtomicUsize::new(0),
            probes: AtomicUsize::new(0),
        }
    }

    fn metrics(site: &str) -> Self {
        Self {
            caps: vec![Capability::ReadMetrics],
            ..Self::text(site)
        }
    }
}

#[async_trait]
impl Publisher for MockPub {
    fn site(&self) -> &Site {
        &self.site
    }
    fn capabilities(&self) -> &[Capability] {
        &self.caps
    }
    fn auth_kind(&self) -> AuthKind {
        self.auth_kind
    }

    async fn publish(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        intent: Intent,
        _deadline: Deadline,
    ) -> Result<Outcome, Error> {
        let n = self.publishes.fetch_add(1, Ordering::SeqCst);
        if self.fail_publish {
            return Err(Error::Platform {
                site: self.site.clone(),
                code: "boom".into(),
                message: "mock failure".into(),
            });
        }
        if self.fail_auth_once && n == 0 {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "token_expired".into(),
            });
        }
        let Body::Text { text } = intent.body;
        Ok(Outcome {
            site: intent.site,
            id: Some(format!("id-{text}")),
            url: Some(format!("https://example.test/{text}")),
            limits: None,
        })
    }

    async fn probe(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        intent: Intent,
        _deadline: Deadline,
    ) -> Result<Probe, Error> {
        let n = self.probes.fetch_add(1, Ordering::SeqCst);
        // same reactive-expiry behavior as publish, so Client::probe's
        // refresh mapping is exercised by the same fail_auth_once switch
        if self.fail_auth_once && n == 0 {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "token_expired".into(),
            });
        }
        let Body::Text { text } = intent.body;
        Ok(Probe {
            site: intent.site,
            container_id: format!("container-{text}"),
            expires_in_hours: 24,
        })
    }

    async fn whoami(&self, _app: &AppConfig, _creds: &AccountCreds) -> Result<WhoAmI, Error> {
        if self.whoami_fails {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "invalid_token".into(),
            });
        }
        Ok(WhoAmI {
            site: self.site.clone(),
            id: "user-1".into(),
            handle: Some("tester".into()),
        })
    }

    async fn insights(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        query: &InsightsQuery,
        _deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
        let mut metrics = serde_json::Map::new();
        metrics.insert("spend".into(), serde_json::json!(10.0));
        Ok(InsightsReply {
            site: self.site.clone(),
            account_id: "act_1".into(),
            currency: Some("MYR".into()),
            rows: vec![InsightRow {
                entity_id: "1".into(),
                level: query.level,
                date_start: query.range.from.clone(),
                metrics,
            }],
        })
    }

    async fn auth_start(&self, _app: &AppConfig) -> Result<AuthStart, Error> {
        Ok(AuthStart::PasteInstructions {
            hint: "app password".into(),
        })
    }

    async fn auth_finish(&self, _app: &AppConfig, reply: AuthReply) -> Result<AccountCreds, Error> {
        match reply {
            AuthReply::AppPassword {
                identifier,
                secret,
                pds,
            } => Ok(AccountCreds::AppPassword {
                identifier,
                secret,
                pds,
            }),
            _ => Err(Error::Auth {
                site: self.site.clone(),
                reason: "unsupported_auth".into(),
            }),
        }
    }

    async fn refresh(&self, _app: &AppConfig, creds: &AccountCreds) -> Result<AccountCreds, Error> {
        if self.refresh_network_err {
            return Err(Error::Network {
                site: self.site.clone(),
                message: "mock refresh outage".into(),
            });
        }
        if self.refresh_dead_session {
            return Err(Error::Auth {
                site: self.site.clone(),
                reason: "session_expired".into(),
            });
        }
        match creds {
            AccountCreds::OAuth2 { extra, .. } => Ok(AccountCreds::OAuth2 {
                access_token: "refreshed".into(),
                refresh_token: None,
                extra: extra.clone(),
            }),
            other => Ok(other.clone()),
        }
    }
}

fn setup(p: MockPub) -> (Client, AccountKey) {
    let mut reg = Registry::new();
    let site = p.site.clone();
    reg.register(Arc::new(p));
    let vault = Arc::new(MemoryVault::new());
    let apps = Arc::new(MemoryAppStore::new());
    let key = AccountKey::new(site.as_str(), "default");
    apps.put(&AppConfig {
        site: site.clone(),
        oauth: None,
        extra: serde_json::json!({}),
    })
    .unwrap();
    vault
        .put(
            &key,
            &AccountCreds::OAuth2 {
                access_token: "tok".into(),
                refresh_token: None,
                extra: serde_json::json!({}),
            },
        )
        .unwrap();
    (Client::new(reg, vault, apps), key)
}

fn intent(site: &str, text: &str) -> Intent {
    Intent {
        site: Site::new(site),
        params: serde_json::json!({}),
        body: Body::Text { text: text.into() },
        idempotency_key: None,
    }
}

/// Client whose stored token expires in an hour — inside the 7-day window,
/// so every publish attempts a proactive refresh first.
fn setup_expiring(p: MockPub) -> (Client, AccountKey) {
    let mut reg = Registry::new();
    let site = p.site.clone();
    reg.register(Arc::new(p));
    let vault = Arc::new(MemoryVault::new());
    let apps = Arc::new(MemoryAppStore::new());
    let key = AccountKey::new(site.as_str(), "default");
    apps.put(&AppConfig {
        site: site.clone(),
        oauth: None,
        extra: serde_json::json!({}),
    })
    .unwrap();
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    vault
        .put(
            &key,
            &AccountCreds::OAuth2 {
                access_token: "tok".into(),
                refresh_token: None,
                extra: serde_json::json!({ "expires_at": expires_at, "refreshed_at": 0 }),
            },
        )
        .unwrap();
    (Client::new(reg, vault, apps), key)
}

fn intent_with_idem(site: &str, text: &str, idem: &str) -> Intent {
    Intent {
        idempotency_key: Some(idem.into()),
        ..intent(site, text)
    }
}

#[tokio::test]
async fn unknown_site() {
    let (c, key) = setup(MockPub::text("threads"));
    let err = c
        .publish(&key, intent("bluesky", "hi"), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidPost { reason, .. } if reason == "site_mismatch"));
}

#[tokio::test]
async fn unknown_site_registry() {
    let (c, _) = setup(MockPub::text("threads"));
    let key = AccountKey::new("bluesky", "default");
    let err = c
        .publish(&key, intent("bluesky", "hi"), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::UnknownSite(s) if s.as_str() == "bluesky"));
}

#[tokio::test]
async fn unknown_account() {
    let (c, _) = setup(MockPub::text("threads"));
    let key = AccountKey::new("threads", "work");
    let err = c
        .publish(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::UnknownAccount(k) if k.name == "work"));
}

#[tokio::test]
async fn publish_without_app_config() {
    let mut reg = Registry::new();
    reg.register(Arc::new(MockPub::text("threads")));
    let vault = Arc::new(MemoryVault::new());
    let apps = Arc::new(MemoryAppStore::new());
    let key = AccountKey::new("threads", "default");
    vault
        .put(
            &key,
            &AccountCreds::OAuth2 {
                access_token: "tok".into(),
                refresh_token: None,
                extra: serde_json::json!({}),
            },
        )
        .unwrap();
    let c = Client::new(reg, vault, apps);
    let out = c
        .publish(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(out.id.as_deref(), Some("id-hi"));
}

#[tokio::test]
async fn happy_publish() {
    let (c, key) = setup(MockPub::text("threads"));
    let out = c
        .publish(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(out.id.as_deref(), Some("id-hi"));
    assert!(out.url.is_some());
}

#[tokio::test]
async fn retries_once_on_token_expired() {
    let mut p = MockPub::text("threads");
    p.fail_auth_once = true;
    let (c, key) = setup(p);
    let out = c
        .publish(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(out.id.as_deref(), Some("id-hi"));
}

#[tokio::test]
async fn auth_start_without_app_config() {
    let mut reg = Registry::new();
    reg.register(Arc::new(MockPub::text("bluesky")));
    let c = Client::new(
        reg,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let start = c.auth_start(&Site::new("bluesky")).await.unwrap();
    assert!(matches!(start, AuthStart::PasteInstructions { .. }));
}

#[tokio::test]
async fn auth_finish_without_app_config() {
    let mut reg = Registry::new();
    reg.register(Arc::new(MockPub::text("bluesky")));
    let vault = Arc::new(MemoryVault::new());
    let c = Client::new(reg, vault.clone(), Arc::new(MemoryAppStore::new()));
    let key = AccountKey::new("bluesky", "you.bsky.social");
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
    assert_eq!(me.handle.as_deref(), Some("tester"));
    match vault.get(&key).unwrap() {
        AccountCreds::AppPassword { identifier, .. } => {
            assert_eq!(identifier, "you.bsky.social");
        }
        other => panic!("{other:?}"),
    }
}

/// Implements `Publisher` without overriding `probe` — the shape every
/// connector that has no create/publish split (Bluesky's createRecord is
/// atomic) keeps forever. Exercises the trait's *default* refusal.
struct Bare {
    caps: Vec<Capability>,
}

#[async_trait]
impl Publisher for Bare {
    fn site(&self) -> &Site {
        static SITE: std::sync::OnceLock<Site> = std::sync::OnceLock::new();
        SITE.get_or_init(|| Site::new("bluesky"))
    }
    fn capabilities(&self) -> &[Capability] {
        &self.caps
    }
    fn auth_kind(&self) -> AuthKind {
        AuthKind::AppPassword
    }
    async fn publish(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _intent: Intent,
        _deadline: Deadline,
    ) -> Result<Outcome, Error> {
        unreachable!("not under test")
    }
    async fn whoami(&self, _app: &AppConfig, _creds: &AccountCreds) -> Result<WhoAmI, Error> {
        Ok(WhoAmI {
            site: Site::new("bluesky"),
            id: "user-1".into(),
            handle: Some("tester".into()),
        })
    }
}

/// 027: the default `probe` must refuse — never fall through to a real
/// publish — on sites with no creation/publication split.
#[tokio::test]
async fn default_probe_refuses_instead_of_publishing() {
    let mut reg = Registry::new();
    reg.register(Arc::new(Bare {
        caps: vec![Capability::PublishText],
    }));
    let vault = Arc::new(MemoryVault::new());
    let key = AccountKey::new("bluesky", "default");
    vault
        .put(
            &key,
            &AccountCreds::AppPassword {
                identifier: "you.bsky.social".into(),
                secret: "xxxx".into(),
                pds: None,
            },
        )
        .unwrap();
    let c = Client::new(reg, vault, Arc::new(MemoryAppStore::new()));
    let err = c
        .probe(&key, intent("bluesky", "hi"), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidPost { ref reason, .. } if reason == "dry_run_unsupported")
    );
}

/// 027: a probe runs the capability gate exactly like a publish.
#[tokio::test]
async fn probe_checks_capability() {
    let mut reg = Registry::new();
    reg.register(Arc::new(Bare { caps: vec![] }));
    let key = AccountKey::new("bluesky", "default");
    let c = Client::new(
        reg,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let err = c
        .probe(&key, intent("bluesky", "hi"), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::UnsupportedCapability { .. }));
}

/// 027: probes never read or write the idempotency ledger. A stored
/// publish outcome must not silence a probe, and a probe must not make a
/// later publish "succeed" by replaying the probe's result.
#[tokio::test]
async fn probe_neither_reads_nor_writes_the_idempotency_ledger() {
    let mut reg = Registry::new();
    reg.register(Arc::new(MockPub::text("threads")));
    let vault = Arc::new(MemoryVault::new());
    let key = AccountKey::new("threads", "default");
    vault
        .put(
            &key,
            &AccountCreds::OAuth2 {
                access_token: "tok".into(),
                refresh_token: None,
                extra: serde_json::json!({}),
            },
        )
        .unwrap();
    // an old completed publish under the same idempotency key
    let seeded = Outcome {
        site: Site::new("threads"),
        id: Some("old-post".into()),
        url: Some("https://example.test/old".into()),
        limits: None,
    };
    vault.put_outcome(&key, "k", &seeded).unwrap();
    let c = Client::new(reg, vault.clone(), Arc::new(MemoryAppStore::new()));

    // read side: the probe ignores the ledger and answers from the platform
    let probe = c
        .probe(
            &key,
            intent_with_idem("threads", "hi", "k"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(probe.container_id, "container-hi");

    // write side: the ledger still replays the *old publish*, not the probe
    let replay = c
        .publish(
            &key,
            intent_with_idem("threads", "hi", "k"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(replay.id.as_deref(), Some("old-post"));
}

/// 027: the reactive token_expired → refresh → retry mapping applies to
/// probes too — a refreshable token must not read as "broken".
#[tokio::test]
async fn probe_retries_once_on_token_expired() {
    let mut p = MockPub::text("threads");
    p.fail_auth_once = true;
    let (c, key) = setup(p);
    let probe = c
        .probe(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(probe.container_id, "container-hi");
}

#[tokio::test]
async fn put_token_then_whoami() {
    let mut reg = Registry::new();
    reg.register(Arc::new(MockPub::text("threads")));
    let c = Client::new(
        reg,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let key = AccountKey::new("threads", "default");
    let me = c.put_token(&key, "THQVJ").await.unwrap();
    assert_eq!(me.id, "user-1");
}

#[tokio::test]
async fn put_token_persists_whoami_id() {
    // The id that whoami already fetches lands in extra — same shape as
    // the OAuth path — so both auth flows publish against /{user_id}/….
    let mut reg = Registry::new();
    reg.register(Arc::new(MockPub::text("threads")));
    let vault = Arc::new(MemoryVault::new());
    let c = Client::new(reg, vault.clone(), Arc::new(MemoryAppStore::new()));
    let key = AccountKey::new("threads", "default");
    let me = c.put_token(&key, "THQVJ").await.unwrap();
    assert_eq!(me.id, "user-1");
    match vault.get(&key).unwrap() {
        AccountCreds::OAuth2 { extra, .. } => {
            assert_eq!(
                extra.get("user_id").and_then(|v| v.as_str()),
                Some("user-1")
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn put_token_rejects_bad_token_before_vault_write() {
    // whoami verifies before the store: an invalid token never lands in
    // the vault to shadow the next publish.
    let mut p = MockPub::text("threads");
    p.whoami_fails = true;
    let mut reg = Registry::new();
    reg.register(Arc::new(p));
    let vault = Arc::new(MemoryVault::new());
    let c = Client::new(reg, vault.clone(), Arc::new(MemoryAppStore::new()));
    let key = AccountKey::new("threads", "default");
    let err = c.put_token(&key, "bogus").await.unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "invalid_token"));
    assert!(
        matches!(vault.get(&key), Err(Error::UnknownAccount(_))),
        "an unverified token must not be stored"
    );
}

#[tokio::test]
async fn put_token_refused_for_app_password_sites() {
    // `auth bluesky --token x` used to store OAuth2 creds that publish
    // rejected much later; the guard must refuse at the door and leave the
    // vault untouched.
    let mut mock = MockPub::text("bluesky");
    mock.auth_kind = AuthKind::AppPassword;
    let mut reg = Registry::new();
    reg.register(Arc::new(mock));
    let vault = Arc::new(MemoryVault::new());
    let c = Client::new(reg, vault.clone(), Arc::new(MemoryAppStore::new()));
    let key = AccountKey::new("bluesky", "you.bsky.social");
    let err = c.put_token(&key, "whatever").await.unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "token_bootstrap_unsupported"));
    assert!(
        matches!(vault.get(&key), Err(Error::UnknownAccount(_))),
        "no creds may be written on refusal"
    );
}

#[tokio::test]
async fn idempotency_retry_returns_stored_outcome() {
    // Same key twice: exactly one connector publish; the retry replays
    // the stored Outcome without HTTP.
    let mock = Arc::new(MockPub::text("threads"));
    let mut reg = Registry::new();
    reg.register(mock.clone());
    let vault = Arc::new(MemoryVault::new());
    let apps = Arc::new(MemoryAppStore::new());
    let key = AccountKey::new("threads", "default");
    apps.put(&AppConfig {
        site: Site::new("threads"),
        oauth: None,
        extra: serde_json::json!({}),
    })
    .unwrap();
    vault
        .put(
            &key,
            &AccountCreds::OAuth2 {
                access_token: "tok".into(),
                refresh_token: None,
                extra: serde_json::json!({}),
            },
        )
        .unwrap();
    let c = Client::new(reg, vault, apps);
    let d = Deadline::from_secs(30);

    let out1 = c
        .publish(&key, intent_with_idem("threads", "hi", "k"), d)
        .await
        .unwrap();
    // retry with the same key — even with different text — must not republish
    let out2 = c
        .publish(&key, intent_with_idem("threads", "CHANGED", "k"), d)
        .await
        .unwrap();
    assert_eq!(out1.id, out2.id);
    assert_eq!(mock.publishes.load(Ordering::SeqCst), 1);

    // a different key posts again; no key, no dedupe
    c.publish(&key, intent_with_idem("threads", "hi", "k2"), d)
        .await
        .unwrap();
    c.publish(&key, intent("threads", "plain"), d)
        .await
        .unwrap();
    assert_eq!(mock.publishes.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn failed_publish_is_not_recorded() {
    // A failed attempt must stay retryable: nothing enters the ledger.
    let mut p = MockPub::text("threads");
    p.fail_publish = true;
    let (c, key) = setup(p);
    let err = c
        .publish(
            &key,
            intent_with_idem("threads", "hi", "k"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Platform { .. }));
    assert!(
        c.vault().get_outcome(&key, "k").unwrap().is_none(),
        "ledger must stay empty after a failure"
    );
}

#[tokio::test]
async fn transient_refresh_failure_still_publishes() {
    // Proactive refresh is an optimization: the stored token is still
    // valid for another hour, so a network failure on the refresh endpoint
    // must degrade to publishing with the current token, not abort it.
    let mut p = MockPub::text("threads");
    p.refresh_network_err = true;
    let (c, key) = setup_expiring(p);
    let out = c
        .publish(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(out.id.as_deref(), Some("id-hi"));
}

#[tokio::test]
async fn dead_session_refresh_fails_fast() {
    // A refresh rejected by the platform means the session is dead;
    // failing fast with the auth error beats dying later inside publish.
    let mut p = MockPub::text("threads");
    p.refresh_dead_session = true;
    let (c, key) = setup_expiring(p);
    let err = c
        .publish(&key, intent("threads", "hi"), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "session_expired"));
}

#[tokio::test]
async fn wire_error_no_ok_field() {
    let err = Error::InvalidPost {
        site: Site::new("threads"),
        reason: "text_too_long".into(),
        limit: Some(500),
    };
    let v = serde_json::to_value(crate::WireError::from(&err)).unwrap();
    assert_eq!(v["error"], "invalid_post");
    assert!(v.get("ok").is_none());
}

#[tokio::test]
async fn secrets_debug_redacted() {
    let creds = AccountCreds::OAuth2 {
        access_token: "secret-token".into(),
        refresh_token: Some("r".into()),
        extra: serde_json::json!({}),
    };
    let d = format!("{creds:?}");
    assert!(!d.contains("secret-token"));
    assert!(d.contains("[redacted]"));
}

fn insights_query(from: &str, to: &str) -> InsightsQuery {
    InsightsQuery {
        level: InsightsLevel::Campaign,
        metrics: vec![Metric::Spend],
        range: crate::insights::DateRange {
            from: from.into(),
            to: to.into(),
        },
        attribution: AttributionWindow::SevenDayClickOneDayView,
        account: None,
    }
}

/// 026 read seam: Client routes the query to the connector, and the
/// capability gate turns a publish-only site away before any HTTP.
#[tokio::test]
async fn client_insights_routes_and_checks_capability() {
    let (c, key) = setup(MockPub::metrics("meta_ads"));
    let reply = c
        .insights(
            &key,
            insights_query("2026-06-01", "2026-06-02"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(reply.account_id, "act_1");
    assert_eq!(reply.rows.len(), 1);

    // a site without read.metrics is refused before reaching the connector
    let (text_only, key) = setup(MockPub::text("meta_ads"));
    let err = text_only
        .insights(
            &key,
            insights_query("2026-06-01", "2026-06-02"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadMetrics)
    );
}

/// The trait's default `insights` must refuse — the same honesty the
/// default `probe` keeps: a connector that never implemented reads cannot
/// let one slip through as something else.
#[tokio::test]
async fn default_insights_refuses() {
    let mut reg = Registry::new();
    reg.register(Arc::new(Bare {
        caps: vec![Capability::PublishText],
    }));
    let vault = Arc::new(MemoryVault::new());
    let apps = Arc::new(MemoryAppStore::new());
    let key = AccountKey::new("bluesky", "default");
    vault
        .put(
            &key,
            &AccountCreds::AppPassword {
                identifier: "you".into(),
                secret: "x".into(),
                pds: None,
            },
        )
        .unwrap();
    let c = Client::new(reg, vault, apps);
    let err = c
        .insights(
            &key,
            insights_query("2026-06-01", "2026-06-02"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadMetrics)
    );
}

/// Range bounds are enforced in the kernel, not just the CLI: a 91-day
/// query is invalid no matter which caller built it.
#[tokio::test]
async fn insights_range_validated_before_any_routing() {
    let (c, key) = setup(MockPub::metrics("meta_ads"));
    let err = c
        .insights(
            &key,
            insights_query("2026-01-01", "2026-04-01"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidQuery { reason, .. } if reason == "range_too_long:91"));
}
