//! Ads Client routing, policy preflight, inventory, and lifecycle tests.
use super::mock::*;
use crate::ads::{
    AdEntity, AdPreviewFormat, AdReviewStatus, AdReviewStatusRequest, AdsActivateRequest,
    AdsArchiveRequest, AdsInspectRequest, AdsInventoryKind, AdsInventoryRequest,
    AdsLifecycleOutcome, AdsPauseRequest, CreateLinkAdCreativeRequest, CreativePreviewRequest,
    UploadAdImageRequest,
};
use crate::apps::{AppStore, MemoryAppStore};
use crate::client::Client;
use crate::error::Error;
use crate::policy::{AdsAction, AllowAdsActionPolicy};
use crate::registry::Registry;
use crate::types::{AccountCreds, AccountKey, AppConfig, Capability, Deadline, Site};
use crate::vault::{MemoryVault, Vault};
use std::sync::Arc;
/// Tier B follows the same capability routing discipline as reads, with an
/// extra policy gate before it can touch a credential or issue a write.
#[tokio::test]
async fn client_paused_create_routes_and_refuses_before_vault_access() {
    let (client, key) = setup(MockPub::paused_ads("meta_ads"));
    let created = client
        .create_paused_ad(
            &key,
            paused_campaign_request("draft"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(created.entity.as_str(), "campaign");
    assert_eq!(created.status, "PAUSED");

    let (no_management, key) = setup(MockPub::text("meta_ads"));
    let err = no_management
        .create_paused_ad(
            &key,
            paused_campaign_request("draft"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::CreatePausedAds)
    );

    // Deliberately leave the vault empty. Policy denial must win over an
    // `unknown_account` error, proving the gate is before credential access.
    let mut registry = Registry::new();
    register_mock(&mut registry, Arc::new(MockPub::paused_ads("meta_ads")));
    let denied = Client::new(
        registry,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    )
    .with_ads_policy(Arc::new(DenyAds))
    .create_paused_ad(
        &AccountKey::new("meta_ads", "default"),
        paused_campaign_request("draft"),
        Deadline::from_secs(30),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(denied, Error::PolicyDenied { action, reason, .. } if action == "create_paused_campaign" && reason == "test_denied")
    );

    // Validation is also local: a malformed draft cannot reach vault lookup
    // or HTTP, even under the normal paused-only policy.
    let empty_vault = Client::new(
        Registry::new(),
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let err = empty_vault
        .create_paused_ad(
            &AccountKey::new("meta_ads", "default"),
            paused_campaign_request("   "),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidQuery { reason, .. } if reason == "missing_name"));
}

/// Creative assets use their own capability and policy labels, while keeping
/// the same "validate and authorize before vault" invariant as paused ads.
#[tokio::test]
async fn client_creative_assets_route_and_refuse_before_vault_access() {
    let (client, key) = setup(MockPub::creative_assets("meta_ads"));
    let uploaded = client
        .upload_ad_image(
            &key,
            UploadAdImageRequest {
                account: None,
                filename: "hero.png".into(),
                bytes: b"image bytes".to_vec(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(uploaded.hash, "image-0");

    let created = client
        .create_link_ad_creative(
            &key,
            CreateLinkAdCreativeRequest {
                account: None,
                creative: crate::ads::LinkAdCreative {
                    name: "Hero".into(),
                    page_id: "456".into(),
                    image_hash: uploaded.hash,
                    message: "A clear benefit".into(),
                    headline: "Learn more".into(),
                    destination_url: "https://example.com/offer".into(),
                    call_to_action: crate::ads::LinkCallToAction::LearnMore,
                    geo_link: None,
                    application_id: None,
                    app_link: None,
                    instagram_user_id: None,
                    advantage_plus: false,
                    whatsapp_identity: None,
                },
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(created.id, "creative-0");

    let (no_creative_capability, key) = setup(MockPub::text("meta_ads"));
    let err = no_creative_capability
        .upload_ad_image(
            &key,
            UploadAdImageRequest {
                account: None,
                filename: "hero.png".into(),
                bytes: b"image bytes".to_vec(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::CreateAdCreative)
    );

    // The vault is empty on purpose. A stricter policy must reject before
    // token lookup, proving an asset write cannot cause a credential side
    // effect when an embedding application disallows it.
    let mut registry = Registry::new();
    register_mock(
        &mut registry,
        Arc::new(MockPub::creative_assets("meta_ads")),
    );
    let denied = Client::new(
        registry,
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    )
    .with_ads_policy(Arc::new(DenyAds))
    .upload_ad_image(
        &AccountKey::new("meta_ads", "default"),
        UploadAdImageRequest {
            account: None,
            filename: "hero.png".into(),
            bytes: b"image bytes".to_vec(),
        },
        Deadline::from_secs(30),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(denied, Error::PolicyDenied { action, reason, .. } if action == "upload_ad_image" && reason == "test_denied")
    );

    // Validation also wins before registry/vault lookup, which avoids local
    // filesystem details and network behavior hiding a bad destination URL.
    let empty_vault = Client::new(
        Registry::new(),
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let err = empty_vault
        .create_link_ad_creative(
            &AccountKey::new("meta_ads", "default"),
            CreateLinkAdCreativeRequest {
                account: None,
                creative: crate::ads::LinkAdCreative {
                    name: "Hero".into(),
                    page_id: "456".into(),
                    image_hash: "hash".into(),
                    message: "Copy".into(),
                    headline: "Headline".into(),
                    destination_url: "http://example.com".into(),
                    call_to_action: crate::ads::LinkCallToAction::LearnMore,
                    geo_link: None,
                    application_id: None,
                    app_link: None,
                    instagram_user_id: None,
                    advantage_plus: false,
                    whatsapp_identity: None,
                },
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { reason, .. } if reason == "destination_url_must_be_https")
    );
}

/// Previewing is a separately declared read: it neither reuses a creative
/// write capability nor touches `AdsPolicy`, because a GET cannot change an
/// auction, draft, budget, or payment state.
#[tokio::test]
async fn client_creative_preview_routes_validates_and_retries_expired_tokens() {
    let mut preview_connector = MockPub::creative_previews("meta_ads");
    preview_connector.fail_auth_once = true;
    let (client, key) = setup(preview_connector);
    let preview = client
        .preview_ad_creative(
            &key,
            CreativePreviewRequest {
                creative_id: "123".into(),
                ad_format: AdPreviewFormat::DesktopFeedStandard,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(preview.creative_id, "123");
    assert_eq!(preview.ad_format, AdPreviewFormat::DesktopFeedStandard);
    assert!(preview.body.contains("data-preview=\"1\""));

    let (no_preview_capability, key) = setup(MockPub::text("meta_ads"));
    let err = no_preview_capability
        .preview_ad_creative(
            &key,
            CreativePreviewRequest {
                creative_id: "123".into(),
                ad_format: AdPreviewFormat::MobileFeedStandard,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadAdPreviews)
    );

    // Request validation is the first operation. An invalid creative ID must
    // not reveal whether a vault alias exists or attempt a connector read.
    let empty = Client::new(
        Registry::new(),
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let err = empty
        .preview_ad_creative(
            &AccountKey::new("meta_ads", "default"),
            CreativePreviewRequest {
                creative_id: "bad-id".into(),
                ad_format: AdPreviewFormat::DesktopFeedStandard,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { reason, .. } if reason == "bad_creative_id:bad-id")
    );
}

/// Review status is a separately declared, GET-only capability. It validates
/// before vault access and represents Meta's transitional state explicitly;
/// `PENDING_REVIEW` is never mistaken for an active delivery request.
#[tokio::test]
async fn client_ad_review_status_routes_validates_and_checks_capability() {
    let (client, key) = setup(MockPub::review_statuses("meta_ads", 1));
    let status = client
        .ad_review_status(
            &key,
            AdReviewStatusRequest {
                entity: AdEntity::Ad,
                id: "123".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert!(status.is_pending_review());
    assert_eq!(status.configured_status, "PAUSED");
    assert_eq!(status.issues[0].summary.as_deref(), Some("Review pending"));

    let (no_status_capability, key) = setup(MockPub::text("meta_ads"));
    let err = no_status_capability
        .ad_review_status(
            &key,
            AdReviewStatusRequest {
                entity: AdEntity::Adset,
                id: "123".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadAdReviewStatus)
    );

    let empty = Client::new(
        Registry::new(),
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let err = empty
        .ad_review_status(
            &AccountKey::new("meta_ads", "default"),
            AdReviewStatusRequest {
                entity: AdEntity::Campaign,
                id: "bad-id".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { reason, .. } if reason == "bad_ad_entity_id:bad-id")
    );
}

/// Inventory is a separately declared GET-only capability. It validates the
/// account override before vault access so a typo cannot become a Graph call.
#[tokio::test]
async fn client_list_ads_inventory_routes_validates_and_checks_capability() {
    let (client, key) = setup(MockPub::ads_inventory("meta_ads"));
    let reply = client
        .list_ads_inventory(
            &key,
            AdsInventoryRequest {
                account: None,
                kind: AdsInventoryKind::Campaign,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(reply.kind, AdsInventoryKind::Campaign);
    assert_eq!(reply.items[0].id, "100");
    assert_eq!(reply.items[0].configured_status.as_deref(), Some("PAUSED"));

    let (no_inventory, key) = setup(MockPub::text("meta_ads"));
    let err = no_inventory
        .list_ads_inventory(
            &key,
            AdsInventoryRequest {
                account: None,
                kind: AdsInventoryKind::Ad,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadAdsInventory)
    );

    let empty = Client::new(
        Registry::new(),
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let err = empty
        .list_ads_inventory(
            &AccountKey::new("meta_ads", "default"),
            AdsInventoryRequest {
                account: Some("nope".into()),
                kind: AdsInventoryKind::Creative,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidQuery { reason, .. } if reason == "bad_ad_account:nope"));
}

/// Inspect is the same GET-only capability as inventory. A non-numeric ID
/// fails before vault access so a typo cannot become a Graph call.
#[tokio::test]
async fn client_inspect_ads_object_routes_validates_and_checks_capability() {
    let (client, key) = setup(MockPub::ads_inventory("meta_ads"));
    let reply = client
        .inspect_ads_object(
            &key,
            AdsInspectRequest {
                kind: AdsInventoryKind::Adset,
                id: "456".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(reply.daily_budget.as_deref(), Some("500"));
    assert_eq!(reply.page_id.as_deref(), Some("111"));
    assert_eq!(
        reply.targeting.as_ref().unwrap().countries,
        ["MY".to_string()]
    );

    let (no_inventory, key) = setup(MockPub::text("meta_ads"));
    let err = no_inventory
        .inspect_ads_object(
            &key,
            AdsInspectRequest {
                kind: AdsInventoryKind::Ad,
                id: "1".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadAdsInventory)
    );

    let empty = Client::new(
        Registry::new(),
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let err = empty
        .inspect_ads_object(
            &AccountKey::new("meta_ads", "default"),
            AdsInspectRequest {
                kind: AdsInventoryKind::Creative,
                id: "creative-1".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { reason, .. } if reason == "bad_ads_inspect_id:creative-1")
    );
}

/// Default policy refuses activate before the vault. Confirmation and
/// review preflight also fail locally so a typo cannot POST status=ACTIVE.
#[tokio::test]
async fn client_activate_ad_policy_preflight_and_opt_in() {
    let (client, key) = setup(MockPub::ads_lifecycle("meta_ads"));
    let denied = client
        .activate_ad(&key, activate_request(), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(
        matches!(denied, Error::PolicyDenied { action, reason, .. } if action == "activate" && reason == "paused_only")
    );

    let empty = Client::new(
        Registry::new(),
        Arc::new(MemoryVault::new()),
        Arc::new(MemoryAppStore::new()),
    );
    let mismatch = empty
        .activate_ad(
            &AccountKey::new("meta_ads", "default"),
            AdsActivateRequest {
                confirm_id: "999".into(),
                ..activate_request()
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(mismatch, Error::InvalidQuery { reason, .. } if reason == "confirm_id_mismatch")
    );

    let mut registry = Registry::new();
    register_mock(&mut registry, Arc::new(MockPub::ads_lifecycle("meta_ads")));
    let vault = Arc::new(MemoryVault::new());
    let apps = Arc::new(MemoryAppStore::new());
    let key = AccountKey::new("meta_ads", "default");
    apps.put(&AppConfig {
        site: Site::new("meta_ads"),
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
    let allowed = Client::new(registry, vault, apps)
        .with_ads_policy(Arc::new(AllowAdsActionPolicy::new(AdsAction::Activate)));
    let (lifetime_client, lifetime_key) =
        setup(MockPub::ads_lifecycle_with_lifetime_budget("meta_ads"));
    let lifetime_client =
        lifetime_client.with_ads_policy(Arc::new(AllowAdsActionPolicy::new(AdsAction::Activate)));
    let err = lifetime_client
        .activate_ad(
            &lifetime_key,
            AdsActivateRequest {
                entity: AdEntity::Adset,
                id: "456".into(),
                confirm_id: "456".into(),
                confirm_daily_budget: None,
                confirm_lifetime_budget: None,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { reason, .. } if reason == "confirm_lifetime_budget_mismatch")
    );
    let outcome = allowed
        .activate_ad(&key, activate_request(), Deadline::from_secs(30))
        .await
        .unwrap();
    match outcome {
        AdsLifecycleOutcome::Applied { status } => {
            assert_eq!(status.configured_status, "ACTIVE");
            assert_eq!(status.id, "456");
        }
        other => panic!("expected applied, got {other:?}"),
    }

    let pending = {
        let mut mock = MockPub::ads_lifecycle("meta_ads");
        mock.review_status_pending_reads = 1;
        mock
    };
    let (client, key) = setup(pending);
    let client = client.with_ads_policy(Arc::new(AllowAdsActionPolicy::new(AdsAction::Activate)));
    let err = client
        .activate_ad(&key, activate_request(), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidQuery { reason, .. } if reason == "review_unresolved"));
}

/// Pause is the emergency valve: default policy allows it, already-paused
/// is idempotent, and a missing capability still fails closed.
#[tokio::test]
async fn client_pause_ad_is_allowed_and_idempotent_when_already_paused() {
    let (client, key) = setup(MockPub::ads_lifecycle("meta_ads"));
    let outcome = client
        .pause_ad(
            &key,
            AdsPauseRequest {
                entity: AdEntity::Adset,
                id: "456".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    match outcome {
        AdsLifecycleOutcome::Applied { status } => {
            assert_eq!(status.configured_status, "PAUSED");
        }
        other => panic!("expected applied, got {other:?}"),
    }

    let (no_cap, key) = setup(MockPub::text("meta_ads"));
    let err = no_cap
        .pause_ad(
            &key,
            AdsPauseRequest {
                entity: AdEntity::Ad,
                id: "1".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ManageAdsLifecycle)
    );
}

#[tokio::test]
async fn client_archive_ad_is_denied_by_default_and_opt_in() {
    let (client, key) = setup(MockPub::ads_lifecycle("meta_ads"));
    let request = AdsArchiveRequest {
        entity: AdEntity::Adset,
        id: "456".into(),
        confirm_id: "456".into(),
    };
    let denied = client
        .archive_ad(&key, request.clone(), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(
        matches!(denied, Error::PolicyDenied { action, reason, .. } if action == "archive" && reason == "paused_only")
    );
    let (client, key) = setup(MockPub::ads_lifecycle("meta_ads"));
    let allowed = client.with_ads_policy(Arc::new(AllowAdsActionPolicy::new(AdsAction::Archive)));
    let outcome = allowed
        .archive_ad(&key, request, Deadline::from_secs(30))
        .await
        .unwrap();
    assert!(matches!(outcome, AdsLifecycleOutcome::Applied { .. }));
}

#[tokio::test]
async fn client_delete_and_duplicate_are_denied_until_opt_in() {
    let (client, key) = setup(MockPub::ads_lifecycle("meta_ads"));
    let delete = crate::ads::AdsDeleteRequest {
        entity: AdEntity::Ad,
        id: "456".into(),
        confirm_id: "456".into(),
        confirm_delete: true,
    };
    let denied = client
        .delete_ad(&key, delete.clone(), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(
        matches!(denied, Error::PolicyDenied { action, reason, .. } if action == "delete" && reason == "paused_only")
    );
    let (client, key) = setup(MockPub::ads_lifecycle("meta_ads"));
    let allowed = client.with_ads_policy(Arc::new(AllowAdsActionPolicy::new(AdsAction::Delete)));
    assert!(matches!(
        allowed
            .delete_ad(&key, delete, Deadline::from_secs(30))
            .await
            .unwrap(),
        AdsLifecycleOutcome::Applied { .. }
    ));

    let dup = crate::ads::AdsDuplicateRequest {
        entity: AdEntity::Campaign,
        id: "100".into(),
        confirm_id: "100".into(),
        confirm_daily_budget: Some(500),
        confirm_lifetime_budget: None,
    };
    let (client, key) = setup(MockPub::ads_lifecycle("meta_ads"));
    let denied = client
        .duplicate_ad(&key, dup.clone(), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(
        matches!(denied, Error::PolicyDenied { action, reason, .. } if action == "duplicate" && reason == "paused_only")
    );
    let (client, key) = setup(MockPub::ads_lifecycle("meta_ads"));
    let allowed = client.with_ads_policy(Arc::new(AllowAdsActionPolicy::new(AdsAction::Duplicate)));
    let copied = allowed
        .duplicate_ad(&key, dup, Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(copied.copied_id, "999");
    assert_eq!(copied.status, "PAUSED");
}

#[tokio::test]
async fn client_typed_edits_require_policy_and_guards() {
    let request = crate::ads::AdsBudgetUpdateRequest {
        entity: AdEntity::Adset,
        id: "456".into(),
        confirm_id: "456".into(),
        current_daily_budget: 500,
        new_daily_budget: 550,
        max_change_ratio: 0.2,
    };
    let (client, key) = setup(MockPub::ads_lifecycle("meta_ads"));
    let denied = client
        .update_ad_budget(&key, request.clone(), Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(
        matches!(denied, Error::PolicyDenied { action, reason, .. } if action == "update_budget" && reason == "paused_only")
    );
    let (client, key) = setup(MockPub::ads_lifecycle("meta_ads"));
    let allowed =
        client.with_ads_policy(Arc::new(AllowAdsActionPolicy::new(AdsAction::UpdateBudget)));
    let outcome = allowed
        .update_ad_budget(&key, request, Deadline::from_secs(30))
        .await
        .unwrap();
    match outcome {
        crate::ads::AdsEditOutcome::Applied { inspect, .. } => {
            assert_eq!(inspect.daily_budget.as_deref(), Some("500"));
        }
        other => panic!("expected applied, got {other:?}"),
    }

    let too_big = crate::ads::AdsBudgetUpdateRequest {
        entity: AdEntity::Adset,
        id: "456".into(),
        confirm_id: "456".into(),
        current_daily_budget: 500,
        new_daily_budget: 800,
        max_change_ratio: 0.2,
    };
    assert_eq!(
        too_big.validate().unwrap_err(),
        "budget_change_exceeds_guard"
    );

    let swap = crate::ads::AdsCreativeSwapRequest {
        id: "456".into(),
        confirm_id: "456".into(),
        creative_id: "789".into(),
    };
    let (client, key) = setup(MockPub::ads_lifecycle("meta_ads"));
    let allowed =
        client.with_ads_policy(Arc::new(AllowAdsActionPolicy::new(AdsAction::SwapCreative)));
    let outcome = allowed
        .swap_ad_creative(&key, swap, Deadline::from_secs(30))
        .await
        .unwrap();
    match outcome {
        crate::ads::AdsEditOutcome::Applied { inspect, .. } => {
            assert_eq!(inspect.id, "456");
        }
        other => panic!("expected applied, got {other:?}"),
    }

    let ad_budget = crate::ads::AdsBudgetUpdateRequest {
        entity: AdEntity::Ad,
        id: "456".into(),
        confirm_id: "456".into(),
        current_daily_budget: 500,
        new_daily_budget: 550,
        max_change_ratio: 0.2,
    };
    assert_eq!(ad_budget.validate().unwrap_err(), "budget_not_on_object");

    let lifetime = crate::ads::AdsLifetimeBudgetUpdateRequest {
        entity: AdEntity::Adset,
        id: "456".into(),
        confirm_id: "456".into(),
        current_lifetime_budget: 10000,
        new_lifetime_budget: 11000,
        max_change_ratio: 0.2,
    };
    // Lifetime edits must read a coherent lifetime-budget object, not the
    // daily-budget fixture used by the neighbouring edit tests.
    let (client, key) = setup(MockPub::ads_lifecycle_with_lifetime_budget("meta_ads"));
    let allowed =
        client.with_ads_policy(Arc::new(AllowAdsActionPolicy::new(AdsAction::UpdateBudget)));
    let outcome = allowed
        .update_ad_lifetime_budget(&key, lifetime, Deadline::from_secs(30))
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        crate::ads::AdsEditOutcome::Applied { .. }
    ));
}

/// A poller is useful only if it stops on the platform's final state. The
/// short test interval is private to the Client test; production uses the
/// conservative two-second interval and always honors the global deadline.
#[cfg(feature = "meta-ads")]
#[tokio::test]
async fn client_ad_review_wait_polls_pending_status_until_settled() {
    let (client, key) = setup(MockPub::review_statuses("meta_ads", 1));
    let result = client
        .wait_for_ad_review_with_interval(
            &key,
            AdReviewStatusRequest {
                entity: AdEntity::Ad,
                id: "123".into(),
            },
            Deadline::from_secs(1),
            std::time::Duration::from_millis(1),
        )
        .await
        .unwrap();
    assert!(matches!(
        result,
        crate::ads::AdReviewWait::Settled(AdReviewStatus { effective_status, .. })
            if effective_status == "PAUSED"
    ));
}
