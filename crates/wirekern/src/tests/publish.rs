//! Publish, probe, token, idempotency, and error-shape Client tests.
use super::mock::*;
use crate::apps::{AppStore, MemoryAppStore};
use crate::client::Client;
use crate::error::Error;
use crate::media::MediaQuery;
use crate::publisher::{AuthKind, AuthReply, AuthStart};
use crate::registry::Registry;
use crate::types::{AccountCreds, AccountKey, AppConfig, Capability, Deadline, Outcome, Site};
use crate::vault::{MemoryVault, Vault};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// 024: the reactive token-expiry retry refreshes under the caller's
/// deadline, not a private 30s.
#[tokio::test]
async fn reactive_refresh_shares_the_publish_deadline() {
    let (c, key, mock) = setup_refresh_probe(
        MockPub {
            fail_auth_once: true,
            ..MockPub::text("threads")
        },
        false,
    );
    let d = Deadline::from_secs(30);
    c.publish(&key, intent("threads", "hi"), d).await.unwrap();
    let got = mock.refresh_deadline.lock().unwrap();
    assert_eq!(
        got.unwrap().0,
        d.0,
        "refresh must receive the caller's budget"
    );
}

/// 024: the proactive pre-publish refresh runs under the caller's deadline
/// too — `--deadline` bounds refresh plus publish end-to-end.
#[tokio::test]
async fn proactive_refresh_shares_the_publish_deadline() {
    let (c, key, mock) = setup_refresh_probe(MockPub::text("threads"), true);
    let d = Deadline::from_secs(30);
    c.publish(&key, intent("threads", "hi"), d).await.unwrap();
    let got = mock.refresh_deadline.lock().unwrap();
    assert_eq!(
        got.unwrap().0,
        d.0,
        "refresh must receive the caller's budget"
    );
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
    register_mock(&mut reg, Arc::new(MockPub::text("threads")));
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
    register_mock(&mut reg, Arc::new(MockPub::text("bluesky")));
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
    register_mock(&mut reg, Arc::new(MockPub::text("bluesky")));
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

/// Page discovery is a facet, not a default method on Publisher. A
/// publisher-only registration cannot list Pages even if a caller skips
/// the capability check in their own code — Client fail-closes on the
/// missing PageDirectory slot.
#[tokio::test]
async fn default_pages_refuses_without_an_explicit_connector_method() {
    let mut reg = Registry::new();
    reg.register(Arc::new(Bare {
        caps: vec![Capability::ReadPages],
    }));
    let vault = Arc::new(MemoryVault::new());
    let key = AccountKey::new("bluesky", "default");
    vault
        .put(
            &key,
            &AccountCreds::BotToken {
                token: "not-used".into(),
            },
        )
        .unwrap();
    let client = Client::new(reg, vault, Arc::new(MemoryAppStore::new()));
    let error = client
        .pages(&key, Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::UnsupportedCapability { need, .. } if need == Capability::ReadPages
    ));
}

/// Media reads are a facet. Advertising `read.media` without attaching
/// MediaReader is the same as not implementing it.
#[tokio::test]
async fn default_media_refuses_without_an_explicit_connector_method() {
    let mut reg = Registry::new();
    reg.register(Arc::new(Bare {
        caps: vec![Capability::ReadMedia],
    }));
    let vault = Arc::new(MemoryVault::new());
    let key = AccountKey::new("bluesky", "default");
    vault
        .put(
            &key,
            &AccountCreds::BotToken {
                token: "not-used".into(),
            },
        )
        .unwrap();
    let client = Client::new(reg, vault, Arc::new(MemoryAppStore::new()));
    let error = client
        .media(&key, MediaQuery::default(), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::UnsupportedCapability { need, .. } if need == Capability::ReadMedia
    ));
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
    register_mock(&mut reg, Arc::new(MockPub::text("threads")));
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
        account: None,
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
    register_mock(&mut reg, Arc::new(MockPub::text("threads")));
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
    register_mock(&mut reg, Arc::new(MockPub::text("threads")));
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
    register_mock(&mut reg, Arc::new(p));
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
    register_mock(&mut reg, Arc::new(mock));
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

/// 023: while one publish under a key is in flight, a second caller with
/// the same key gets a distinct transient error and the connector is hit
/// exactly once. The gate holds the first publish inside the claim window
/// so the test does not depend on wall-clock races.
#[tokio::test]
async fn concurrent_same_key_publishes_once() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
    let mut mock = MockPub::text("threads");
    mock.publish_started = std::sync::Mutex::new(Some(started_tx));
    mock.publish_gate = std::sync::Mutex::new(Some(gate_rx));
    let mock = Arc::new(mock);
    let mut reg = Registry::new();
    register_mock(&mut reg, mock.clone());
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
    let c = Arc::new(Client::new(
        reg,
        vault.clone(),
        Arc::new(MemoryAppStore::new()),
    ));

    let first = {
        let c = c.clone();
        let key = key.clone();
        tokio::spawn(async move {
            c.publish(
                &key,
                intent_with_idem("threads", "hi", "k"),
                Deadline::from_secs(30),
            )
            .await
        })
    };
    // Wait until the first publish is inside the connector (claim held).
    started_rx.await.unwrap();
    // Second caller with the same key: refused while the first is in
    // flight, transient enough to retry (wire exit 4), never a duplicate.
    let err = c
        .publish(
            &key,
            intent_with_idem("threads", "hi", "k"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::IdempotencyInFlight { key: k, .. } if k == "k"),
        "got {err:?}"
    );
    assert_eq!(err.exit_code(), 4);
    assert_eq!(mock.publishes.load(Ordering::SeqCst), 1);
    // Release the gate: the first completes, records, and releases; a
    // retry with the same key now replays the stored outcome.
    gate_tx.send(()).unwrap();
    let out = first.await.unwrap().unwrap();
    assert_eq!(out.id.as_deref(), Some("id-hi"));
    let retry = c
        .publish(
            &key,
            intent_with_idem("threads", "hi", "k"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(retry.id, out.id);
    assert_eq!(mock.publishes.load(Ordering::SeqCst), 1);
}

/// 023: a failed attempt must release the claim — the key stays retryable.
#[tokio::test]
async fn failed_publish_releases_the_claim() {
    let mock = Arc::new(MockPub {
        fail_publish: true,
        ..MockPub::text("threads")
    });
    let mut reg = Registry::new();
    register_mock(&mut reg, mock.clone());
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
    let c = Client::new(reg, vault, Arc::new(MemoryAppStore::new()));
    let d = Deadline::from_secs(30);

    let err = c
        .publish(&key, intent_with_idem("threads", "hi", "k"), d)
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Platform { ref code, .. } if code == "boom"),
        "got {err:?}"
    );
    // A leaked claim would answer IdempotencyInFlight here; the retry
    // must reach the connector again.
    let err2 = c
        .publish(&key, intent_with_idem("threads", "hi", "k"), d)
        .await
        .unwrap_err();
    assert!(
        matches!(err2, Error::Platform { ref code, .. } if code == "boom"),
        "claim leaked: {err2:?}"
    );
    assert_eq!(mock.publishes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn idempotency_retry_returns_stored_outcome() {
    // Same key twice: exactly one connector publish; the retry replays
    // the stored Outcome without HTTP.
    let mock = Arc::new(MockPub::text("threads"));
    let mut reg = Registry::new();
    register_mock(&mut reg, mock.clone());
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
    // account echo: stamped on publish, echoed by the ledger replay
    assert_eq!(out1.account.as_deref(), Some("default"));
    assert_eq!(out2.account.as_deref(), Some("default"));
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

#[test]
fn wire_error_http_status_matches_the_serve_table() {
    use crate::WireError;
    assert_eq!(
        WireError::UnknownSite {
            site: Site::new("x")
        }
        .http_status(),
        404
    );
    assert_eq!(
        WireError::Auth {
            site: Site::new("x"),
            reason: "invalid_key".into()
        }
        .http_status(),
        401
    );
    assert_eq!(
        WireError::InvalidPost {
            site: Site::new("x"),
            reason: "text_too_long".into(),
            limit: Some(500)
        }
        .http_status(),
        422
    );
    assert_eq!(
        WireError::RateLimited {
            site: Site::new("x"),
            retry_after: None
        }
        .http_status(),
        429
    );
    assert_eq!(
        WireError::Platform {
            site: Site::new("x"),
            code: "100".into(),
            message: "no".into()
        }
        .http_status(),
        502
    );
    assert_eq!(
        WireError::Timeout {
            site: Site::new("x")
        }
        .http_status(),
        503
    );
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

#[test]
fn app_config_debug_keeps_connector_secret_extensions_opaque() {
    let config = AppConfig {
        site: Site::new("whatsapp_cloud"),
        oauth: None,
        extra: serde_json::json!({ "app_secret": "webhook-secret" }),
    };
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("webhook-secret"));
    assert!(rendered.contains("[opaque]"));
}
