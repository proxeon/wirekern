//! WhatsApp Cloud Client policy, idempotency, and live-read tests.
use super::mock::*;
use crate::apps::{AppStore, MemoryAppStore};
use crate::client::Client;
use crate::error::Error;
use crate::policy::AllowWhatsAppSendsPolicy;
use crate::registry::Registry;
use crate::types::{AccountCreds, AccountKey, AppConfig, Capability, Deadline, Site};
use crate::vault::{MemoryVault, Vault};
use crate::whatsapp::{WhatsAppMessage, WhatsAppSendRequest};
use std::sync::atomic::Ordering;
use std::sync::Arc;
#[cfg(feature = "whatsapp-cloud")]
#[test]
fn parse_whatsapp_webhook_reads_app_store_not_a_listener() {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut registry = Registry::new();
    register_mock(&mut registry, Arc::new(MockPub::whatsapp("whatsapp_cloud")));
    let apps = Arc::new(MemoryAppStore::new());
    apps.put(&AppConfig {
        site: Site::new("whatsapp_cloud"),
        oauth: None,
        extra: serde_json::json!({
            "phone_number_id": "123456789",
            "app_secret": "webhook-secret",
        }),
    })
    .unwrap();
    let client = Client::new(registry, Arc::new(MemoryVault::new()), apps);
    let raw = br#"{"object":"whatsapp_business_account","entry":[{"changes":[{"field":"messages","value":{"metadata":{"phone_number_id":"123456789"},"messages":[]}}]}]}"#;
    let mut mac = Hmac::<Sha256>::new_from_slice(b"webhook-secret").unwrap();
    mac.update(raw);
    let sig = format!(
        "sha256={}",
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let reply = client
        .parse_whatsapp_webhook(&sig, raw, crate::whatsapp::WebhookParseOptions::default())
        .unwrap();
    assert!(reply.messages.is_empty());
}

/// A signed payload that this build cannot model is hash-audited as before
/// and, only when the caller attached the explicit replay store, encrypted
/// for a future parser upgrade. Retrying before that upgrade must retain it.
#[cfg(feature = "whatsapp-cloud")]
#[test]
fn signed_unmodeled_webhook_is_captured_for_safe_replay() {
    use crate::{
        MemoryWhatsAppLedger, MemoryWhatsAppReplayableDeadLetters, WhatsAppReplayableDeadLetters,
    };
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut registry = Registry::new();
    register_mock(&mut registry, Arc::new(MockPub::whatsapp("whatsapp_cloud")));
    let apps = Arc::new(MemoryAppStore::new());
    apps.put(&AppConfig {
        site: Site::new("whatsapp_cloud"),
        oauth: None,
        extra: serde_json::json!({
            "phone_number_id": "123456789",
            "app_secret": "webhook-secret",
        }),
    })
    .unwrap();
    let ledger = Arc::new(MemoryWhatsAppLedger::new());
    let replay = Arc::new(MemoryWhatsAppReplayableDeadLetters::new());
    let client = Client::new(registry, Arc::new(MemoryVault::new()), apps)
        .with_whatsapp_ledger(ledger.clone())
        .with_whatsapp_replay_dead_letters(replay.clone());
    let raw = br#"{"object":"whatsapp_business_account","entry":[{"changes":[{"field":"messages","value":{"metadata":{"phone_number_id":"123456789"},"statuses":[{"id":"wamid.unknown","status":"future_state"}]}}]}]}"#;
    let mut mac = Hmac::<Sha256>::new_from_slice(b"webhook-secret").unwrap();
    mac.update(raw);
    let signature = format!(
        "sha256={}",
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );

    let error = client
        .ingest_whatsapp_webhook(
            &signature,
            raw,
            crate::whatsapp::WebhookParseOptions::default(),
        )
        .unwrap_err();
    assert!(
        matches!(error, Error::InvalidQuery { reason, .. } if reason == "webhook_status_unsupported")
    );
    assert_eq!(ledger.dead_letters().len(), 1);
    let saved = client.list_whatsapp_replay_dead_letters().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].reason, "webhook_status_unsupported");
    assert!(!serde_json::to_string(&saved)
        .unwrap()
        .contains("future_state"));

    // No parser upgrade occurred, so retrying fails and leaves the encrypted
    // event available for a deliberate retry after the upgrade.
    assert!(client
        .replay_whatsapp_dead_letter(
            &saved[0].id,
            crate::whatsapp::WebhookParseOptions::default(),
        )
        .is_err());
    assert_eq!(client.list_whatsapp_replay_dead_letters().unwrap().len(), 1);

    // A parser upgrade is represented here by a valid signed callback placed
    // in the explicit replay store. Successful replay reduces it and removes
    // only that ciphertext; the still-unsupported event remains for later.
    let valid = br#"{"object":"whatsapp_business_account","entry":[{"changes":[{"field":"messages","value":{"metadata":{"phone_number_id":"123456789"},"messages":[]}}]}]}"#;
    let mut mac = Hmac::<Sha256>::new_from_slice(b"webhook-secret").unwrap();
    mac.update(valid);
    let valid_signature = format!(
        "sha256={}",
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let valid_entry = replay
        .capture("webhook_parser_upgraded", &valid_signature, valid)
        .unwrap();
    let parsed = client
        .replay_whatsapp_dead_letter(
            &valid_entry.id,
            crate::whatsapp::WebhookParseOptions::default(),
        )
        .unwrap();
    assert!(parsed.messages.is_empty());
    assert_eq!(client.list_whatsapp_replay_dead_letters().unwrap().len(), 1);

    // The parser rejects oversized bodies before it can prove the signature.
    // Even a syntactically valid HMAC header must not turn that input into a
    // durable archive or permit a storage-exhaustion route.
    let oversized = vec![b'x'; crate::connectors::whatsapp_cloud::MAX_WEBHOOK_BYTES + 1];
    let mut mac = Hmac::<Sha256>::new_from_slice(b"webhook-secret").unwrap();
    mac.update(&oversized);
    let oversized_signature = format!(
        "sha256={}",
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let error = client
        .ingest_whatsapp_webhook(
            &oversized_signature,
            &oversized,
            crate::whatsapp::WebhookParseOptions::default(),
        )
        .unwrap_err();
    assert!(
        matches!(error, Error::InvalidQuery { reason, .. } if reason == "webhook_body_too_large")
    );
    assert_eq!(ledger.dead_letters().len(), 1);
    assert_eq!(client.list_whatsapp_replay_dead_letters().unwrap().len(), 1);
}

#[cfg(feature = "whatsapp-cloud")]
fn whatsapp_request(key: &str) -> WhatsAppSendRequest {
    WhatsAppSendRequest {
        message: WhatsAppMessage::Reply {
            to: "60123456789".into(),
            reply_to_message_id: "wamid.inbound".into(),
            text: "Terima kasih".into(),
            preview_url: false,
        },
        idempotency_key: key.into(),
        recipient_type: crate::whatsapp::RecipientType::Individual,
    }
}

/// A static System User token must still be verified before it is saved, but
/// unlike OAuth it has no refresh token or connector target hidden in `extra`.
#[cfg(feature = "whatsapp-cloud")]
#[tokio::test]
async fn static_token_bootstrap_uses_redacted_bot_token_shape() {
    let mut registry = Registry::new();
    register_mock(&mut registry, Arc::new(MockPub::whatsapp("whatsapp_cloud")));
    let vault = Arc::new(MemoryVault::new());
    let client = Client::new(registry, vault.clone(), Arc::new(MemoryAppStore::new()));
    let key = AccountKey::new("whatsapp_cloud", "default");
    client.put_token(&key, "system-user-token").await.unwrap();
    assert!(
        matches!(vault.get(&key).unwrap(), AccountCreds::BotToken { token } if token == "system-user-token")
    );
}

/// Policy is checked before the vault: a caller missing `--allow-send` learns
/// the safe remediation without revealing whether an account exists.
#[cfg(feature = "whatsapp-cloud")]
#[tokio::test]
async fn whatsapp_default_policy_denies_before_vault_or_connector() {
    let publisher = Arc::new(MockPub::whatsapp("whatsapp_cloud"));
    let mut registry = Registry::new();
    register_mock(&mut registry, publisher.clone());
    let client = Client::new(
        registry,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let error = client
        .send_whatsapp(
            &AccountKey::new("whatsapp_cloud", "default"),
            whatsapp_request("reply-1"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::PolicyDenied { action, reason, .. }
        if action == "send_whatsapp_reply" && reason == "explicit_whatsapp_send_required"));
    assert_eq!(publisher.whatsapp_sends.load(Ordering::SeqCst), 0);
}

/// The explicit policy permits the typed send and shares Client's atomic
/// outcome ledger: retrying a confirmed private message never calls Meta a
/// second time with the same key.
#[cfg(feature = "whatsapp-cloud")]
#[tokio::test]
async fn whatsapp_allowed_send_replays_confirmed_idempotency_outcome() {
    let publisher = Arc::new(MockPub::whatsapp("whatsapp_cloud"));
    let mut registry = Registry::new();
    register_mock(&mut registry, publisher.clone());
    let vault = Arc::new(MemoryVault::new());
    let client = Client::new(registry, vault.clone(), Arc::new(MemoryAppStore::new()))
        .with_whatsapp_policy(Arc::new(AllowWhatsAppSendsPolicy));
    let key = AccountKey::new("whatsapp_cloud", "default");
    vault
        .put(
            &key,
            &AccountCreds::BotToken {
                token: "system-user-token".into(),
            },
        )
        .unwrap();
    let first = client
        .send_whatsapp(&key, whatsapp_request("reply-1"), Deadline::from_secs(30))
        .await
        .unwrap();
    let replay = client
        .send_whatsapp(&key, whatsapp_request("reply-1"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(first.id, replay.id);
    assert_eq!(publisher.whatsapp_sends.load(Ordering::SeqCst), 1);
}

/// Selecting a configured sender changes both the connector's phone ID and
/// the local idempotency namespace. The same human key can therefore be used
/// once per business number without a wrong-sender replay.
#[cfg(feature = "whatsapp-cloud")]
#[tokio::test]
async fn whatsapp_sender_alias_routes_and_scopes_idempotency() {
    let publisher = Arc::new(MockPub::whatsapp("whatsapp_cloud"));
    let mut registry = Registry::new();
    register_mock(&mut registry, publisher.clone());
    let vault = Arc::new(MemoryVault::new());
    let apps = Arc::new(MemoryAppStore::new());
    apps.put(&AppConfig {
        site: Site::new("whatsapp_cloud"),
        oauth: None,
        extra: serde_json::json!({
            "phone_number_id": "111111111",
            "senders": [{ "alias": "marketing", "phone_number_id": "222222222" }],
        }),
    })
    .unwrap();
    let client = Client::new(registry, vault.clone(), apps)
        .with_whatsapp_policy(Arc::new(AllowWhatsAppSendsPolicy));
    let key = AccountKey::new("whatsapp_cloud", "default");
    vault
        .put(
            &key,
            &AccountCreds::BotToken {
                token: "system-user-token".into(),
            },
        )
        .unwrap();

    let marketing = client
        .send_whatsapp_from(
            &key,
            Some("marketing"),
            whatsapp_request("shared-key"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    let primary = client
        .send_whatsapp(
            &key,
            whatsapp_request("shared-key"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    let replay = client
        .send_whatsapp_from(
            &key,
            Some("marketing"),
            whatsapp_request("shared-key"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();

    assert_ne!(marketing.id, primary.id);
    assert_eq!(marketing.id, replay.id);
    assert_eq!(publisher.whatsapp_sends.load(Ordering::SeqCst), 2);
    assert_eq!(
        *publisher.whatsapp_phone_ids.lock().expect("phone ids"),
        vec!["222222222".to_string(), "111111111".to_string()]
    );
}

/// A bounded fan-out is still made of normal private sends. Regression for a
/// batch-local limiter that accepted item one then returned RateLimited for
/// item two instead of waiting for the next Cloud API pacing slot.
#[cfg(feature = "whatsapp-cloud")]
#[tokio::test]
async fn whatsapp_many_paces_every_item_and_returns_every_outcome() {
    let publisher = Arc::new(MockPub::whatsapp("whatsapp_cloud"));
    let mut registry = Registry::new();
    register_mock(&mut registry, publisher.clone());
    let vault = Arc::new(MemoryVault::new());
    let client = Client::new(registry, vault.clone(), Arc::new(MemoryAppStore::new()))
        .with_whatsapp_policy(Arc::new(AllowWhatsAppSendsPolicy));
    let key = AccountKey::new("whatsapp_cloud", "default");
    vault
        .put(
            &key,
            &AccountCreds::BotToken {
                token: "system-user-token".into(),
            },
        )
        .unwrap();

    let outcomes = client
        .send_whatsapp_many(
            &key,
            vec![whatsapp_request("batch-1"), whatsapp_request("batch-2")],
            Deadline::from_secs(30),
        )
        .await
        .unwrap();

    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0].id.as_deref(), Some("wamid-0"));
    assert_eq!(outcomes[1].id.as_deref(), Some("wamid-1"));
    assert_eq!(publisher.whatsapp_sends.load(Ordering::SeqCst), 2);
}

#[cfg(feature = "whatsapp-cloud")]
#[tokio::test]
async fn whatsapp_missing_vault_account_releases_its_idempotency_claim() {
    let publisher = Arc::new(MockPub::whatsapp("whatsapp_cloud"));
    let mut registry = Registry::new();
    register_mock(&mut registry, publisher.clone());
    let vault = Arc::new(MemoryVault::new());
    let client = Client::new(registry, vault.clone(), Arc::new(MemoryAppStore::new()))
        .with_whatsapp_policy(Arc::new(AllowWhatsAppSendsPolicy));
    let key = AccountKey::new("whatsapp_cloud", "default");
    let first = client
        .send_whatsapp(&key, whatsapp_request("reply-1"), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(first, Error::UnknownAccount(_)));
    vault
        .put(
            &key,
            &AccountCreds::BotToken {
                token: "system-user-token".into(),
            },
        )
        .unwrap();
    // A leaked claim would return IdempotencyInFlight here instead of making
    // the first valid send after the credential repair.
    client
        .send_whatsapp(&key, whatsapp_request("reply-1"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(publisher.whatsapp_sends.load(Ordering::SeqCst), 1);
}

/// Ads and WhatsApp policies are independent: installing one must not
/// reset the other. A library user can deny ads activation while allowing
/// a typed WhatsApp send.
#[cfg(feature = "whatsapp-cloud")]
#[tokio::test]
async fn ads_and_whatsapp_policies_compose() {
    let mut mock = MockPub::whatsapp("meta_ads");
    mock.caps = vec![
        Capability::SendReply,
        Capability::SendTemplate,
        Capability::CreatePausedAds,
    ];
    let publisher = Arc::new(mock);
    let mut registry = Registry::new();
    register_mock(&mut registry, publisher.clone());
    let vault = Arc::new(MemoryVault::new());
    let client = Client::new(registry, vault.clone(), Arc::new(MemoryAppStore::new()))
        .with_ads_policy(Arc::new(DenyAds))
        .with_whatsapp_policy(Arc::new(AllowWhatsAppSendsPolicy));
    let key = AccountKey::new("meta_ads", "default");
    vault
        .put(
            &key,
            &AccountCreds::BotToken {
                token: "system-user-token".into(),
            },
        )
        .unwrap();
    client
        .send_whatsapp(&key, whatsapp_request("compose-1"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(publisher.whatsapp_sends.load(Ordering::SeqCst), 1);
    let denied = client
        .create_paused_ad(
            &key,
            paused_campaign_request("draft"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(denied, Error::PolicyDenied { action, reason, .. } if action == "create_paused_campaign" && reason == "test_denied")
    );
    assert_eq!(publisher.paused_creates.load(Ordering::SeqCst), 0);
}

/// Opt-in smoke contract for a deliberately configured local WhatsApp test
/// account. It calls only read endpoints, remains ignored in normal CI, and
/// does not become a hidden real-send test when credentials happen to exist.
#[cfg(all(feature = "whatsapp-cloud", feature = "vault-file"))]
#[tokio::test]
#[ignore = "requires WIREKERN_LIVE_WHATSAPP=1 and a local configured Cloud API test account"]
async fn live_whatsapp_cloud_reads() {
    assert_eq!(
        std::env::var("WIREKERN_LIVE_WHATSAPP").as_deref(),
        Ok("1"),
        "set WIREKERN_LIVE_WHATSAPP=1 to explicitly authorize live Graph reads"
    );
    let home = std::env::var_os("WIREKERN_HOME")
        .map(std::path::PathBuf::from)
        .expect("set WIREKERN_HOME to the local Wirekern test vault");
    let client = Client::from_home(&home, false).expect("build file-backed test client");
    let key = AccountKey::new("whatsapp_cloud", "default");
    let deadline = Deadline::from_secs(30);

    client.whoami(&key).await.expect("phone whoami contract");
    client
        .list_whatsapp_templates(
            &key,
            crate::whatsapp::WhatsAppTemplateQuery {
                limit: Some(1),
                ..Default::default()
            },
            deadline,
        )
        .await
        .expect("template page contract");
    client
        .list_whatsapp_flows(
            &key,
            crate::whatsapp::WhatsAppPageQuery {
                limit: Some(1),
                ..Default::default()
            },
            deadline,
        )
        .await
        .expect("Flow page contract");
    client
        .list_whatsapp_wabas(
            &key,
            crate::whatsapp::WhatsAppPageQuery {
                limit: Some(1),
                ..Default::default()
            },
            deadline,
        )
        .await
        .expect("WABA page contract");
    client
        .list_whatsapp_phone_numbers(
            &key,
            crate::whatsapp::WhatsAppPageQuery {
                limit: Some(1),
                ..Default::default()
            },
            deadline,
        )
        .await
        .expect("phone page contract");
    client
        .whatsapp_phone_health(&key, deadline)
        .await
        .expect("phone health contract");

    // System-user lookup is owned by the Business Portfolio, which is
    // optional for a single-WABA config. Only call its edge when it is
    // explicitly configured; a missing optional identifier is not a failed
    // Cloud API contract.
    let config = client.apps().get(&Site::new("whatsapp_cloud")).unwrap();
    if config
        .extra
        .get("business_id")
        .and_then(|value| value.as_str())
        .is_some()
    {
        client
            .list_whatsapp_system_users(
                &key,
                crate::whatsapp::WhatsAppPageQuery {
                    limit: Some(1),
                    ..Default::default()
                },
                deadline,
            )
            .await
            .expect("system-user page contract");
    }
    if let Ok(media_id) = std::env::var("WIREKERN_LIVE_WHATSAPP_MEDIA_ID") {
        client
            .whatsapp_media_metadata(&key, &media_id, deadline)
            .await
            .expect("media metadata contract");
    }
}

/// Opt-in live mutation contract for the isolated media lifecycle. It never
/// addresses a customer: the test uploads an operator-provided fixture and
/// deletes that exact media ID before it returns. Keep it separate from the
/// read suite because Graph credentials alone must never turn CI into writes.
#[cfg(all(feature = "whatsapp-cloud", feature = "vault-file"))]
#[tokio::test]
#[ignore = "requires WIREKERN_LIVE_WHATSAPP_WRITE_TESTS=1 and WIREKERN_LIVE_WHATSAPP_MEDIA_FILE"]
async fn live_whatsapp_cloud_media_upload_delete() {
    assert_eq!(
        std::env::var("WIREKERN_LIVE_WHATSAPP_WRITE_TESTS").as_deref(),
        Ok("1"),
        "set WIREKERN_LIVE_WHATSAPP_WRITE_TESTS=1 to explicitly authorize the upload/delete contract"
    );
    let home = std::env::var_os("WIREKERN_HOME")
        .map(std::path::PathBuf::from)
        .expect("set WIREKERN_HOME to the local Wirekern test vault");
    let media_file = std::env::var_os("WIREKERN_LIVE_WHATSAPP_MEDIA_FILE")
        .map(std::path::PathBuf::from)
        .expect("set WIREKERN_LIVE_WHATSAPP_MEDIA_FILE to a disposable supported media fixture");
    let mime_type =
        std::env::var("WIREKERN_LIVE_WHATSAPP_MEDIA_MIME").unwrap_or_else(|_| "image/png".into());
    let bytes = std::fs::read(&media_file).expect("read disposable media fixture");
    let filename = media_file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("wirekern-live-media")
        .to_string();
    let client = Client::from_home(&home, false).expect("build file-backed test client");
    let key = AccountKey::new("whatsapp_cloud", "default");
    let uploaded = client
        .upload_whatsapp_media(
            &key,
            crate::whatsapp::WhatsAppMediaUpload {
                bytes,
                mime_type,
                filename,
            },
            Deadline::from_secs(30),
        )
        .await
        .expect("media upload contract");
    client
        .delete_whatsapp_media(&key, &uploaded.id, Deadline::from_secs(30))
        .await
        .expect("delete uploaded contract fixture");
}
