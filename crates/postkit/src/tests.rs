use crate::apps::{AppStore, MemoryAppStore};
use crate::client::Client;
use crate::error::Error;
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::registry::Registry;
use crate::types::{
    AccountCreds, AccountKey, AppConfig, Body, Capability, Deadline, Intent, Outcome, Site, WhoAmI,
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
    refresh_network_err: bool,
    refresh_dead_session: bool,
    publishes: AtomicUsize,
}

impl MockPub {
    fn text(site: &str) -> Self {
        Self {
            site: Site::new(site),
            caps: vec![Capability::PublishText],
            auth_kind: AuthKind::OAuth2AuthCode,
            fail_auth_once: false,
            fail_publish: false,
            refresh_network_err: false,
            refresh_dead_session: false,
            publishes: AtomicUsize::new(0),
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

    async fn whoami(&self, _app: &AppConfig, _creds: &AccountCreds) -> Result<WhoAmI, Error> {
        Ok(WhoAmI {
            site: self.site.clone(),
            id: "user-1".into(),
            handle: Some("tester".into()),
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
