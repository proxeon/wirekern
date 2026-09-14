//! Pages, media, and image-body Client tests.
use super::mock::*;
use crate::apps::MemoryAppStore;
use crate::client::Client;
use crate::error::Error;
use crate::media::MediaQuery;
use crate::registry::Registry;
use crate::types::{AccountKey, Capability, Deadline, Intent, Site};
use crate::vault::MemoryVault;
use std::sync::Arc;
/// Remote ad-account discovery is a separately gated read. This prevents a
/// connector from accidentally treating local vault aliases as account IDs.
#[tokio::test]
async fn client_ad_accounts_routes_and_checks_capability() {
    let (c, key) = setup(MockPub::ad_accounts("meta_ads"));
    let reply = c.ad_accounts(&key, Deadline::from_secs(30)).await.unwrap();
    assert_eq!(reply.accounts[0].id, "act_1");
    assert_eq!(reply.accounts[0].name.as_deref(), Some("Main"));

    let (text_only, key) = setup(MockPub::text("meta_ads"));
    let err = text_only
        .ad_accounts(&key, Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadAdAccounts)
    );
}

/// Page discovery is its own capability: a social publisher does not gain an
/// account-listing read merely by supporting a text body. The mock expires on
/// its first read so this also proves the Client's one refresh/retry rule.
#[tokio::test]
async fn client_pages_routes_retries_expired_token_and_checks_capability() {
    let mut publisher = MockPub::pages("facebook_pages");
    publisher.fail_auth_once = true;
    let (client, key) = setup(publisher);
    let reply = client.pages(&key, Deadline::from_secs(30)).await.unwrap();
    assert_eq!(reply.pages[0].id, "10");
    assert_eq!(reply.pages[0].tasks, vec!["CREATE_CONTENT"]);

    // No credential is installed here. `ReadPages` must refuse at the
    // capability gate first, rather than leaking an unrelated
    // `unknown_account` and making the caller provision a token for a site
    // that cannot list Pages anyway.
    let mut registry = Registry::new();
    register_mock(&mut registry, Arc::new(MockPub::text("facebook_pages")));
    let client = Client::new(
        registry,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let key = AccountKey::new("facebook_pages", "default");
    let error = client
        .pages(&key, Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::UnsupportedCapability { need, .. } if need == Capability::ReadPages
    ));
}

/// Published-media reads share the read-only refresh rule while keeping their
/// own capability, so a connector cannot expose profile content by accident.
#[tokio::test]
async fn client_media_routes_retries_expired_token_and_checks_bounds() {
    let mut publisher = MockPub::media("instagram");
    publisher.fail_auth_once = true;
    let (client, key) = setup(publisher);
    let reply = client
        .media(&key, MediaQuery { limit: 2 }, Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(reply.media[0].id, "recent-2");
    assert_eq!(
        reply.media[0].permalink.as_deref(),
        Some("https://example.test/recent")
    );

    // Limit validation comes before even the capability lookup/vault read;
    // an embedding caller receives the same safe local error as the CLI.
    let mut registry = Registry::new();
    register_mock(&mut registry, Arc::new(MockPub::text("instagram")));
    let client = Client::new(
        registry,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let key = AccountKey::new("instagram", "default");
    let invalid = client
        .media(&key, MediaQuery { limit: 0 }, Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(
        matches!(invalid, Error::InvalidQuery { reason, .. } if reason == "media_limit_out_of_range")
    );

    // A valid request to a write-only connector refuses before it demands a
    // credential, exactly like the other remote discovery commands.
    let unsupported = client
        .media(&key, MediaQuery { limit: 1 }, Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(
        unsupported,
        Error::UnsupportedCapability { need, .. } if need == Capability::ReadMedia
    ));
}

#[test]
fn image_form_validation_is_local_and_stable() {
    use crate::types::Image;
    let ok = Image::Bytes {
        filename: "hero.png".into(),
        bytes: b"x".to_vec(),
    };
    ok.validate().unwrap();
    assert_eq!(
        Image::Bytes {
            filename: "a/hero.png".into(),
            bytes: b"x".to_vec()
        }
        .validate()
        .unwrap_err(),
        "invalid_image_filename"
    );
    assert_eq!(
        Image::Bytes {
            filename: "hero.png".into(),
            bytes: vec![]
        }
        .validate()
        .unwrap_err(),
        "image_file_empty"
    );
    assert_eq!(
        Image::Url("http://cdn.test/h.png".into())
            .validate()
            .unwrap_err(),
        "image_url_must_be_https"
    );
    assert_eq!(
        Image::Url("https:///no-authority".into())
            .validate()
            .unwrap_err(),
        "image_url_must_be_https"
    );
    Image::Url("https://cdn.test/h.png".into())
        .validate()
        .unwrap();
}

#[tokio::test]
async fn client_routes_image_bodies_by_capability() {
    // A text-only publisher must refuse an image body at the capability
    // check, before the vault is read — the same door every body faces.
    let (c, key) = setup(MockPub::text("threads"));
    let intent = Intent {
        site: Site::new("threads"),
        params: serde_json::json!({}),
        body: crate::types::Body::Image {
            text: None,
            image: crate::types::Image::Url("https://cdn.test/h.png".into()),
            alt: String::new(),
        },
        idempotency_key: None,
    };
    let err = c
        .publish(&key, intent, Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        Error::UnsupportedCapability {
            need: crate::types::Capability::PublishImage,
            ..
        }
    ));

    // A carousel gets its own capability. Reusing `publish.image` here would
    // let a connector see a multi-container body it never opted in to handle.
    let (c, key) = setup(MockPub::text("instagram"));
    let intent = Intent {
        site: Site::new("instagram"),
        params: serde_json::json!({}),
        body: crate::types::Body::Carousel {
            text: None,
            images: vec![
                crate::types::Image::Url("https://cdn.test/1.jpg".into()),
                crate::types::Image::Url("https://cdn.test/2.jpg".into()),
            ],
        },
        idempotency_key: None,
    };
    let err = c
        .publish(&key, intent, Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        Error::UnsupportedCapability {
            need: crate::types::Capability::PublishCarousel,
            ..
        }
    ));
}
