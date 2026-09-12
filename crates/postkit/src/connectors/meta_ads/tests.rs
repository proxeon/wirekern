//! Connector tests. They call `pub(super)` helpers via sibling modules.
use super::auth::{access_tier_from_headers, refuse_non_system_user_debug};
use super::graph::map_graph_error;
use super::insights::{attribution_param, meta_field, row_from};
use super::inventory::LIVE_EFFECTIVE_STATUS;
use super::*;
use crate::ads::{
    AdEntity, AdPreviewFormat, AdReviewStatusRequest, AdsConfiguredStatus, AdsInspectRequest,
    AdsInventoryKind, AdsInventoryRequest, AdsStatusUpdateRequest, AdsTokenKind, CampaignObjective,
    CreateLinkAdCreativeRequest, CreatePausedAdRequest, CreativePreviewRequest, LinkAdCreative,
    LinkCallToAction, MarketingApiAccessTierKind, PausedAd, PausedAdCreate, PausedAdset,
    PausedCampaign, UploadAdImageRequest,
};
use crate::facets::{AdsManager, InsightsSource};
use crate::insights::{AttributionWindow, InsightsJobStatus, InsightsLevel, InsightsQuery, Metric};
use crate::types::OAuthApp;
use httpmock::prelude::*;
use serde_json::json;

fn token_creds(account: &str) -> AccountCreds {
    AccountCreds::OAuth2 {
        access_token: "tok".into(),
        refresh_token: None,
        extra: json!({ "ad_account_id": account }),
    }
}

fn empty_app() -> AppConfig {
    AppConfig {
        site: Site::new(SITE),
        oauth: None,
        extra: json!({}),
    }
}

fn oauth_app() -> AppConfig {
    AppConfig {
        site: Site::new(SITE),
        oauth: Some(OAuthApp {
            client_id: "id".into(),
            client_secret: "sec".into(),
            redirect_uri: "https://localhost/callback".into(),
        }),
        extra: json!({}),
    }
}

fn query() -> InsightsQuery {
    InsightsQuery {
        level: InsightsLevel::Campaign,
        metrics: vec![Metric::Spend, Metric::Impressions, Metric::Purchases],
        range: crate::insights::DateRange {
            from: "2026-06-01".into(),
            to: "2026-06-02".into(),
        },
        attribution: AttributionWindow::SevenDayClickOneDayView,
        account: None,
        entity_ids: vec![],
        breakdowns: vec![],
        report: crate::insights::InsightsReportKind::Performance,
    }
}

fn mock_account_currency(server: &MockServer, currency: &str) {
    server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/act_123")
            .query_param("fields", "currency");
        then.status(200).json_body(json!({ "currency": currency }));
    });
}

#[test]
fn attribution_param_maps_every_preset_to_the_wire_array() {
    // regression (fcba1bd): Graph code-100 rejects the combined display
    // name; every preset must serialize as an array of atomic windows
    assert_eq!(
        attribution_param(AttributionWindow::SevenDayClickOneDayView),
        r#"["7d_click","1d_view"]"#
    );
    assert_eq!(
        attribution_param(AttributionWindow::OneDayClick),
        r#"["1d_click"]"#
    );
    assert_eq!(
        attribution_param(AttributionWindow::OneDayView),
        r#"["1d_view"]"#
    );
    for a in [
        AttributionWindow::SevenDayClickOneDayView,
        AttributionWindow::OneDayClick,
        AttributionWindow::OneDayView,
    ] {
        let p = attribution_param(a);
        assert!(p.starts_with('['), "not an array: {p}");
        assert!(!p.contains("7d_click_1d_view"), "display name leaked: {p}");
    }
}

#[test]
fn graph_error_codes_classify() {
    // code-first: copy containing "expired"/"quota" cannot hijack code 100
    let err = map_graph_error(
        400,
        r#"{"error":{"code":100,"message":"token expired; quota weirdness"}}"#,
    );
    assert!(matches!(err, Error::Platform { code, .. } if code == "100"));
    let err = map_graph_error(
        400,
        r#"{"error":{"code":190,"message":"Error validating access token"}}"#,
    );
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "token_expired"));
    for code in [4, 17, 32, 341, 613, 80004] {
        let body = format!(r#"{{"error":{{"code":{code},"message":"x"}}}}"#);
        assert!(matches!(
            map_graph_error(400, &body),
            Error::RateLimited { .. }
        ));
    }
    // no code at all: the substring fallback still classifies
    let err = map_graph_error(400, r#"{"error":{"message":"rate limit reached"}}"#);
    assert!(matches!(err, Error::RateLimited { .. }));
    let err = map_graph_error(500, "");
    assert!(matches!(err, Error::Network { ref message, .. } if message == "http_500"));
}

#[test]
fn graph_error_prefers_meta_operator_message_over_generic_summary() {
    // Regression for the paused-ad validation: code 100's generic
    // "Invalid parameter" hid Meta's actionable billing requirement.
    let err = map_graph_error(
        400,
        r#"{"error":{"code":100,"message":"Invalid parameter","error_user_msg":"Update payment method: add a valid payment method."}}"#,
    );
    assert!(
        matches!(err, Error::Platform { code, message, .. } if code == "100" && message == "Update payment method: add a valid payment method.")
    );

    // Empty optional detail must fall back to the regular Meta message.
    let err = map_graph_error(
        400,
        r#"{"error":{"code":100,"message":"Invalid parameter","error_user_msg":"   "}}"#,
    );
    assert!(matches!(err, Error::Platform { message, .. } if message == "Invalid parameter"));

    let err = map_graph_error(
        400,
        r#"{"error":{"code":100,"message":"Invalid parameter","error_user_title":"Payment needed","error_user_msg":"Add a valid payment method."}}"#,
    );
    assert!(
        matches!(err, Error::Platform { message, .. } if message == "Payment needed: Add a valid payment method.")
    );
}

#[test]
fn marketing_error_shapes_that_change_retry_or_guidance() {
    // One fixture per Marketing/Graph shape that changes Postkit's
    // retry decision or operator text. Codes from Meta's error
    // reference and Graph error-handling tables (v26.0).
    #[allow(clippy::type_complexity)]
    let cases: &[(&str, fn(&Error) -> bool)] = &[
        (
            r#"{"error":{"code":10,"message":"Permission denied"}}"#,
            |err| matches!(err, Error::Auth { reason, .. } if reason == "permission"),
        ),
        (
            r#"{"error":{"code":200,"message":"Permissions error"}}"#,
            |err| matches!(err, Error::Auth { reason, .. } if reason == "permission"),
        ),
        (
            r#"{"error":{"code":368,"message":"Temporarily blocked for policies"}}"#,
            |err| matches!(err, Error::RateLimited { .. }),
        ),
        (
            r#"{"error":{"code":80004,"message":"There have been too many calls to this ad-account"}}"#,
            |err| matches!(err, Error::RateLimited { .. }),
        ),
        (
            r#"{"error":{"code":341,"message":"Application limit reached"}}"#,
            |err| matches!(err, Error::RateLimited { .. }),
        ),
        (
            r#"{"error":{"code":102,"message":"API session"}}"#,
            |err| matches!(err, Error::Auth { reason, .. } if reason == "token_expired"),
        ),
        (
            r#"{"error":{"code":190,"error_subcode":463,"message":"Error validating access token"}}"#,
            |err| matches!(err, Error::Auth { reason, .. } if reason == "token_expired"),
        ),
        (
            r#"{"error":{"code":190,"error_subcode":467,"message":"Invalid OAuth 2.0 Access Token"}}"#,
            |err| matches!(err, Error::Auth { reason, .. } if reason == "token_expired"),
        ),
        (
            r#"{"error":{"code":190,"error_subcode":459,"message":"Error validating access token","error_user_title":"Confirm your identity"}}"#,
            |err| matches!(err, Error::Auth { reason, .. } if reason == "user_checkpointed"),
        ),
        (
            r#"{"error":{"code":190,"error_subcode":458,"message":"Error validating access token"}}"#,
            |err| matches!(err, Error::Auth { reason, .. } if reason == "app_not_installed"),
        ),
        (
            r#"{"error":{"code":190,"error_subcode":464,"message":"Error validating access token"}}"#,
            |err| matches!(err, Error::Auth { reason, .. } if reason == "unconfirmed_user"),
        ),
        (
            r#"{"error":{"code":100,"message":"Invalid parameter","error_user_title":"Budget too low","error_user_msg":"Increase the daily budget."}}"#,
            |err| matches!(err, Error::Platform { code, message, .. } if code == "100" && message == "Budget too low: Increase the daily budget."),
        ),
    ];
    for (body, check) in cases {
        let err = map_graph_error(400, body);
        assert!(check(&err), "shape {body} classified as {err:?}");
    }
}

#[tokio::test]
async fn auth_start_url_shape() {
    let t = MetaAds::new().unwrap();
    match t.auth_start(&oauth_app()).await.unwrap() {
        AuthStart::Browser {
            authorize_url,
            state,
        } => {
            assert!(authorize_url.starts_with("https://www.facebook.com/dialog/oauth?"));
            assert_eq!(
                crate::oauth::query_param(&authorize_url, "scope").as_deref(),
                Some(SCOPES)
            );
            // Regression (d984e73): asserted against literals, not the
            // SCOPES constant — without pages_show_list the token cannot
            // see any Page, and without pages_manage_ads a Page-backed
            // creative cannot act on one; Tier B's creative step was
            // unreachable until both were added.
            let scope = crate::oauth::query_param(&authorize_url, "scope").unwrap();
            for required in [
                "ads_read",
                "ads_management",
                "pages_show_list",
                "pages_manage_ads",
            ] {
                assert!(
                    scope.split(',').any(|s| s == required),
                    "scope lost {required}"
                );
            }
            assert!(authorize_url.contains("response_type=code"));
            assert_eq!(
                crate::oauth::query_param(&authorize_url, "state").as_deref(),
                Some(&state[..])
            );
        }
        other => panic!("{other:?}"),
    }
    let err = t.auth_start(&empty_app()).await.unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "missing_app_config"));
}

#[tokio::test]
async fn paused_creates_use_only_paused_forms_and_correct_edges() {
    let server = MockServer::start();
    let campaign = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/act_123/campaigns")
            .body_contains("objective=OUTCOME_SALES")
            .body_contains("status=PAUSED")
            .body_contains("special_ad_categories=%5B%5D")
            // Mirrors Meta's required campaign choice for an ad-set
            // budget. A missing flag reaches the API as code 100 rather
            // than a locally actionable error.
            .body_contains("is_adset_budget_sharing_enabled=false");
        then.status(200).json_body(json!({ "id": "100" }));
    });
    let adset = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/act_123/adsets")
            .body_contains("campaign_id=100")
            .body_contains("daily_budget=2500")
            .body_contains("bid_strategy=LOWEST_COST_WITHOUT_CAP")
            .body_contains("targeting=%7B")
            .body_contains("status=PAUSED");
        then.status(200).json_body(json!({ "id": "200" }));
    });
    let ad = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/act_123/ads")
            .body_contains("adset_id=200")
            .body_contains("creative=%7B%22creative_id%22%3A%22300%22%7D")
            .body_contains("status=PAUSED");
        then.status(200).json_body(json!({ "id": "400" }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let creds = token_creds("123");

    let campaign_out = connector
        .create_paused_ad(
            &empty_app(),
            &creds,
            &CreatePausedAdRequest {
                account: None,
                create: PausedAdCreate::Campaign(PausedCampaign {
                    name: "paused campaign".into(),
                    objective: CampaignObjective::Sales,
                    special_ad_categories: vec![],
                    daily_budget: None,
                    lifetime_budget: None,
                    is_adset_budget_sharing_enabled: false,
                }),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    let adset_out = connector
        .create_paused_ad(
            &empty_app(),
            &creds,
            &CreatePausedAdRequest {
                account: None,
                create: PausedAdCreate::Adset(PausedAdset {
                    name: "paused ad set".into(),
                    campaign_id: "100".into(),
                    daily_budget: Some(2500),
                    lifetime_budget: None,
                    bid_strategy: crate::ads::BidStrategy::LowestCostWithoutCap,
                    bid_amount: None,
                    roas_average_floor: None,
                    billing_event: crate::ads::BillingEvent::Impressions,
                    optimization_goal: crate::ads::OptimizationGoal::Reach,
                    targeting: crate::ads::AdTargeting {
                        geo_locations: crate::ads::GeoLocations {
                            countries: vec!["MY".into()],
                        },
                        age_min: None,
                        age_max: None,
                        publisher_platforms: vec![],
                        facebook_positions: vec![],
                        instagram_positions: vec![],
                        whatsapp_positions: vec![],
                        user_age_unknown: None,
                    },
                    start_time: None,
                    end_time: None,
                    promoted_object: None,
                }),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    let ad_out = connector
        .create_paused_ad(
            &empty_app(),
            &creds,
            &CreatePausedAdRequest {
                account: None,
                create: PausedAdCreate::Ad(PausedAd {
                    name: "paused ad".into(),
                    adset_id: "200".into(),
                    creative_id: "300".into(),
                }),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();

    campaign.assert();
    adset.assert();
    ad.assert();
    assert_eq!(campaign_out.id, "100");
    assert_eq!(adset_out.entity, crate::ads::AdEntity::Adset);
    assert_eq!(ad_out.status, "PAUSED");
}

#[tokio::test]
async fn cbo_and_lifetime_budgets_are_posted_as_form_fields() {
    let server = MockServer::start();
    let campaign = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/act_123/campaigns")
            .body_contains("daily_budget=5000")
            .body_contains("is_adset_budget_sharing_enabled=false")
            .body_contains("status=PAUSED");
        then.status(200).json_body(json!({ "id": "100" }));
    });
    let adset = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/act_123/adsets")
            .body_contains("lifetime_budget=20000")
            .body_contains("end_time=2026-11-21T14%3A26%3A09-08%3A00")
            .body_contains("status=PAUSED");
        then.status(200).json_body(json!({ "id": "200" }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let creds = token_creds("123");

    connector
        .create_paused_ad(
            &empty_app(),
            &creds,
            &CreatePausedAdRequest {
                account: None,
                create: PausedAdCreate::Campaign(PausedCampaign {
                    name: "cbo campaign".into(),
                    objective: CampaignObjective::Awareness,
                    special_ad_categories: vec![],
                    daily_budget: Some(5000),
                    lifetime_budget: None,
                    is_adset_budget_sharing_enabled: false,
                }),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    connector
        .create_paused_ad(
            &empty_app(),
            &creds,
            &CreatePausedAdRequest {
                account: None,
                create: PausedAdCreate::Adset(PausedAdset {
                    name: "lifetime ad set".into(),
                    campaign_id: "100".into(),
                    daily_budget: None,
                    lifetime_budget: Some(20_000),
                    bid_strategy: crate::ads::BidStrategy::LowestCostWithoutCap,
                    bid_amount: None,
                    roas_average_floor: None,
                    billing_event: crate::ads::BillingEvent::Impressions,
                    optimization_goal: crate::ads::OptimizationGoal::Reach,
                    targeting: crate::ads::AdTargeting {
                        geo_locations: crate::ads::GeoLocations {
                            countries: vec!["MY".into()],
                        },
                        age_min: None,
                        age_max: None,
                        publisher_platforms: vec![],
                        facebook_positions: vec![],
                        instagram_positions: vec![],
                        whatsapp_positions: vec![],
                        user_age_unknown: None,
                    },
                    start_time: Some("2026-11-11T14:26:09-08:00".into()),
                    end_time: Some("2026-11-21T14:26:09-08:00".into()),
                    promoted_object: None,
                }),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();

    campaign.assert();
    adset.assert();
}

#[tokio::test]
async fn cap_and_min_roas_constraints_are_posted() {
    let server = MockServer::start();
    let cap = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/act_123/adsets")
            .body_contains("bid_strategy=COST_CAP")
            .body_contains("bid_amount=200")
            .body_contains("status=PAUSED");
        then.status(200).json_body(json!({ "id": "201" }));
    });
    let roas = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/act_123/adsets")
            .body_contains("bid_strategy=LOWEST_COST_WITH_MIN_ROAS")
            .body_contains("roas_average_floor")
            .body_contains("10000")
            .body_contains("status=PAUSED");
        then.status(200).json_body(json!({ "id": "202" }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let creds = token_creds("123");
    let targeting = crate::ads::AdTargeting {
        geo_locations: crate::ads::GeoLocations {
            countries: vec!["MY".into()],
        },
        age_min: None,
        age_max: None,
        publisher_platforms: vec![],
        facebook_positions: vec![],
        instagram_positions: vec![],
        whatsapp_positions: vec![],
        user_age_unknown: None,
    };

    connector
        .create_paused_ad(
            &empty_app(),
            &creds,
            &CreatePausedAdRequest {
                account: None,
                create: PausedAdCreate::Adset(PausedAdset {
                    name: "cap".into(),
                    campaign_id: "100".into(),
                    daily_budget: Some(2500),
                    lifetime_budget: None,
                    bid_strategy: crate::ads::BidStrategy::CostCap,
                    bid_amount: Some(200),
                    roas_average_floor: None,
                    billing_event: crate::ads::BillingEvent::Impressions,
                    optimization_goal: crate::ads::OptimizationGoal::Reach,
                    targeting: targeting.clone(),
                    start_time: None,
                    end_time: None,
                    promoted_object: None,
                }),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    connector
        .create_paused_ad(
            &empty_app(),
            &creds,
            &CreatePausedAdRequest {
                account: None,
                create: PausedAdCreate::Adset(PausedAdset {
                    name: "roas".into(),
                    campaign_id: "100".into(),
                    daily_budget: Some(2500),
                    lifetime_budget: None,
                    bid_strategy: crate::ads::BidStrategy::LowestCostWithMinRoas,
                    bid_amount: None,
                    roas_average_floor: Some(10_000),
                    billing_event: crate::ads::BillingEvent::Impressions,
                    optimization_goal: crate::ads::OptimizationGoal::Value,
                    targeting,
                    start_time: None,
                    end_time: None,
                    promoted_object: Some(crate::ads::PromotedObject::Pixel {
                        pixel_id: "789".into(),
                        custom_event_type: crate::ads::CustomEventType::Purchase,
                    }),
                }),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();

    cap.assert();
    roas.assert();
}

#[tokio::test]
async fn promoted_object_is_posted_without_kind_tag() {
    let server = MockServer::start();
    let adset = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/act_123/adsets")
            .body_contains("promoted_object=")
            .body_contains("pixel_id")
            .body_contains("PURCHASE")
            .body_contains("status=PAUSED");
        then.status(200).json_body(json!({ "id": "203" }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    connector
        .create_paused_ad(
            &empty_app(),
            &token_creds("123"),
            &CreatePausedAdRequest {
                account: None,
                create: PausedAdCreate::Adset(PausedAdset {
                    name: "pixel set".into(),
                    campaign_id: "100".into(),
                    daily_budget: Some(2500),
                    lifetime_budget: None,
                    bid_strategy: crate::ads::BidStrategy::LowestCostWithoutCap,
                    bid_amount: None,
                    roas_average_floor: None,
                    billing_event: crate::ads::BillingEvent::Impressions,
                    optimization_goal: crate::ads::OptimizationGoal::OffsiteConversions,
                    targeting: crate::ads::AdTargeting {
                        geo_locations: crate::ads::GeoLocations {
                            countries: vec!["MY".into()],
                        },
                        age_min: None,
                        age_max: None,
                        publisher_platforms: vec![],
                        facebook_positions: vec![],
                        instagram_positions: vec![],
                        whatsapp_positions: vec![],
                        user_age_unknown: None,
                    },
                    start_time: None,
                    end_time: None,
                    promoted_object: Some(crate::ads::PromotedObject::Pixel {
                        pixel_id: "789".into(),
                        custom_event_type: crate::ads::CustomEventType::Purchase,
                    }),
                }),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    adset.assert();
}

#[tokio::test]
async fn image_link_creative_uploads_media_then_posts_reviewable_story_spec() {
    let server = MockServer::start();
    let image = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/act_123/adimages")
            // `filename` is a multipart field containing raw selected
            // bytes; an account image upload never creates an ad.
            .body_contains("name=\"filename\"; filename=\"hero.png\"")
            .body_contains("not-a-real-png");
        then.status(200).json_body(json!({
            "images": { "hero.png": { "hash": "hash-1" } }
        }));
    });
    let creative = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/act_123/adcreatives")
            .body_contains("name=Hero+link")
            .body_contains("object_story_spec=%7B")
            .body_contains("%22page_id%22%3A%22456%22")
            .body_contains("%22image_hash%22%3A%22hash-1%22")
            .body_contains("https%3A%2F%2Fexample.com%2Foffer")
            .body_contains("LEARN_MORE");
        then.status(200).json_body(json!({ "id": "500" }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let creds = token_creds("123");

    let uploaded = connector
        .upload_ad_image(
            &empty_app(),
            &creds,
            &UploadAdImageRequest {
                account: None,
                filename: "hero.png".into(),
                bytes: b"not-a-real-png".to_vec(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    let created = connector
        .create_link_ad_creative(
            &empty_app(),
            &creds,
            &CreateLinkAdCreativeRequest {
                account: None,
                creative: LinkAdCreative {
                    name: "Hero link".into(),
                    page_id: "456".into(),
                    image_hash: uploaded.hash,
                    message: "A clear benefit".into(),
                    headline: "Learn more".into(),
                    destination_url: "https://example.com/offer".into(),
                    call_to_action: LinkCallToAction::LearnMore,
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

    image.assert();
    creative.assert();
    assert_eq!(created.id, "500");
    assert_eq!(created.account_id, "act_123");
}

#[tokio::test]
async fn video_upload_posts_source_and_status_reads_video_status() {
    let server = MockServer::start();
    let upload = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/act_123/advideos")
            .body_contains("name=\"source\"; filename=\"hero.mp4\"");
        then.status(200).json_body(json!({ "id": "9001" }));
    });
    let status = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/9001")
            .query_param("fields", "status");
        then.status(200).json_body(json!({
            "id": "9001",
            "status": { "video_status": "processing" }
        }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let creds = token_creds("123");
    let uploaded = connector
        .upload_ad_video(
            &empty_app(),
            &creds,
            &crate::ads::UploadAdVideoRequest {
                account: None,
                filename: "hero.mp4".into(),
                bytes: b"not-a-real-mp4".to_vec(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    let status_out = connector
        .ad_video_status(
            &empty_app(),
            &creds,
            &crate::ads::AdVideoStatusRequest {
                video_id: uploaded.id.clone(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    upload.assert();
    status.assert();
    assert_eq!(uploaded.id, "9001");
    assert_eq!(
        status_out.video_status,
        crate::ads::AdVideoStatusKind::Processing
    );
}

#[tokio::test]
async fn video_creative_posts_video_data_story_spec() {
    let server = MockServer::start();
    let creative = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/act_123/adcreatives")
            .body_contains("video_id")
            .body_contains("9001");
        then.status(200).json_body(json!({ "id": "501" }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let created = connector
        .create_video_ad_creative(
            &empty_app(),
            &token_creds("123"),
            &crate::ads::CreateVideoAdCreativeRequest {
                account: None,
                creative: crate::ads::VideoAdCreative {
                    name: "Hero video".into(),
                    page_id: "456".into(),
                    video_id: "9001".into(),
                    image_hash: "hash-1".into(),
                    message: "Watch".into(),
                    destination_url: "https://example.com/offer".into(),
                    call_to_action: LinkCallToAction::LearnMore,
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
    creative.assert();
    assert_eq!(created.id, "501");
}

#[tokio::test]
async fn catalog_creative_posts_product_set_id_as_a_creative_field() {
    let server = MockServer::start();
    let creative = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/act_123/adcreatives")
            .body_contains("product_set_id=88")
            .body_contains("template_data");
        then.status(200).json_body(json!({ "id": "502" }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let created = connector
        .create_ad_creative(
            &empty_app(),
            &token_creds("123"),
            &crate::ads::CreateAdCreativeRequest {
                account: None,
                kind: crate::ads::AdCreativeKind::Catalog(crate::ads::CatalogAdCreative {
                    name: "Catalog".into(),
                    page_id: "456".into(),
                    product_set_id: "88".into(),
                    link: "https://example.com/shop".into(),
                    message: "Shop".into(),
                    call_to_action: LinkCallToAction::ShopNow,
                    instagram_user_id: None,
                    advantage_plus: false,
                    whatsapp_identity: None,
                }),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    creative.assert();
    assert_eq!(created.id, "502");
}

#[tokio::test]
async fn creative_preview_reads_one_closed_format_and_keeps_the_body_opaque() {
    let server = MockServer::start();
    let preview = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/500/previews")
            .query_param("ad_format", "MOBILE_FEED_STANDARD")
            .query_param("access_token", "tok");
        then.status(200).json_body(
            json!({ "data": [{ "body": "<iframe src=\"https://meta.test/preview\"></iframe>" }] }),
        );
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let response = connector
        .preview_ad_creative(
            &empty_app(),
            &token_creds("123"),
            &CreativePreviewRequest {
                creative_id: "500".into(),
                ad_format: AdPreviewFormat::MobileFeedStandard,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    preview.assert();
    assert_eq!(response.creative_id, "500");
    assert_eq!(response.ad_format, AdPreviewFormat::MobileFeedStandard);
    assert!(response.body.contains("iframe"));

    let missing_body = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/501/previews")
            .query_param("ad_format", "DESKTOP_FEED_STANDARD");
        then.status(200).json_body(json!({ "data": [{}] }));
    });
    let err = connector
        .preview_ad_creative(
            &empty_app(),
            &token_creds("123"),
            &CreativePreviewRequest {
                creative_id: "501".into(),
                ad_format: AdPreviewFormat::DesktopFeedStandard,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    missing_body.assert();
    assert!(
        matches!(err, Error::Platform { code, message, .. } if code == "missing_preview_body" && message == "creative preview returned no body")
    );
}

#[tokio::test]
async fn review_status_reads_only_lifecycle_fields_and_preserves_meta_issues() {
    let server = MockServer::start();
    let status = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/700")
            .query_param(
                "fields",
                "id,name,configured_status,effective_status,issues_info",
            )
            .query_param("access_token", "tok");
        then.status(200).json_body(json!({
            "id": "700",
            "name": "Paused validation ad",
            "configured_status": "PAUSED",
            "effective_status": "PENDING_REVIEW",
            "issues_info": [{
                "error_code": 100,
                "error_summary": "Review pending",
                "error_message": "Meta is reviewing this ad.",
                "level": "WARNING"
            }]
        }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let response = connector
        .ad_review_status(
            &empty_app(),
            &token_creds("123"),
            &AdReviewStatusRequest {
                entity: AdEntity::Ad,
                id: "700".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();

    status.assert();
    assert_eq!(response.configured_status, "PAUSED");
    assert_eq!(response.effective_status, "PENDING_REVIEW");
    assert!(response.is_pending_review());
    assert_eq!(response.issues.len(), 1);
    assert_eq!(response.issues[0].code.as_deref(), Some("100"));
    assert_eq!(
        response.issues[0].message.as_deref(),
        Some("Meta is reviewing this ad.")
    );

    let missing_status = server.mock(|when, then| {
        when.method(GET).path("/v26.0/701");
        then.status(200).json_body(json!({ "id": "701" }));
    });
    let err = connector
        .ad_review_status(
            &empty_app(),
            &token_creds("123"),
            &AdReviewStatusRequest {
                entity: AdEntity::Campaign,
                id: "701".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    missing_status.assert();
    assert!(matches!(err, Error::Platform { code, .. } if code == "missing_configured_status"));
}

#[tokio::test]
async fn ads_inventory_pages_sorts_by_id_and_omits_deleted_creatives() {
    let server = MockServer::start();
    let base = server.base_url();
    let next_base = base.clone();
    let second = server.mock(move |when, then| {
        when.method(GET)
            .path("/v26.0/act_123/campaigns")
            .query_param("after", "next");
        then.status(200).json_body(json!({
            "data": [{
                "id": "100",
                "name": "First",
                "configured_status": "PAUSED",
                "effective_status": "PAUSED",
                "objective": "OUTCOME_TRAFFIC"
            }]
        }));
    });
    server.mock(move |when, then| {
        when.method(GET)
            .path("/v26.0/act_123/campaigns")
            .query_param(
                "fields",
                "id,name,configured_status,effective_status,objective",
            )
            .query_param("limit", "25")
            .query_param("effective_status", LIVE_EFFECTIVE_STATUS);
        then.status(200).json_body(json!({
            "data": [{
                "id": "200",
                "name": "Second",
                "configured_status": "PAUSED",
                "effective_status": "IN_PROCESS"
            }],
            "paging": { "next": format!("{next_base}/v26.0/act_123/campaigns?after=next") }
        }));
    });
    let connector = MetaAds::with_base(format!("{base}/v26.0")).unwrap();
    let reply = connector
        .list_ads_inventory(
            &empty_app(),
            &token_creds("act_123"),
            &AdsInventoryRequest {
                account: None,
                kind: AdsInventoryKind::Campaign,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();

    second.assert();
    assert_eq!(reply.account_id, "act_123");
    assert_eq!(reply.kind, AdsInventoryKind::Campaign);
    assert_eq!(reply.items.len(), 2);
    assert_eq!(reply.items[0].id, "100");
    assert_eq!(reply.items[0].name.as_deref(), Some("First"));
    assert_eq!(reply.items[1].id, "200");

    let creatives = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/act_123/adcreatives")
            .query_param("fields", "id,name,status,object_type")
            .query_param("limit", "25");
        then.status(200).json_body(json!({
            "data": [
                { "id": "9", "name": "Gone", "status": "DELETED", "object_type": "SHARE" },
                { "id": "8", "name": "Hero", "status": "ACTIVE", "object_type": "SHARE" }
            ]
        }));
    });
    let creative_reply = connector
        .list_ads_inventory(
            &empty_app(),
            &token_creds("act_123"),
            &AdsInventoryRequest {
                account: None,
                kind: AdsInventoryKind::Creative,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    creatives.assert();
    assert_eq!(creative_reply.items.len(), 1);
    assert_eq!(creative_reply.items[0].id, "8");
    assert_eq!(creative_reply.items[0].status.as_deref(), Some("ACTIVE"));
    assert!(creative_reply.items[0].configured_status.is_none());
}

#[tokio::test]
async fn ads_inventory_paging_is_capped() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/act_123/adsets");
        let base = server.base_url();
        then.status(200).json_body(json!({
            "data": [],
            "paging": { "next": format!("{base}/v26.0/act_123/adsets?after=x") }
        }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let err = connector
        .list_ads_inventory(
            &empty_app(),
            &token_creds("act_123"),
            &AdsInventoryRequest {
                account: None,
                kind: AdsInventoryKind::Adset,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Platform { ref code, .. } if code == "paging_exceeded"));
}

#[tokio::test]
async fn ads_inventory_refuses_a_non_numeric_account_before_http() {
    let server = MockServer::start();
    let sink = server.mock(|when, then| {
        when.method(GET).path("/v26.0/act_nope/campaigns");
        then.status(200).json_body(json!({ "data": [] }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let err = connector
        .list_ads_inventory(
            &empty_app(),
            &token_creds("act_123"),
            &AdsInventoryRequest {
                account: Some("nope".into()),
                kind: AdsInventoryKind::Campaign,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidQuery { reason, .. } if reason == "bad_ad_account:nope"));
    assert_eq!(sink.hits(), 0);
}

#[tokio::test]
async fn ads_inspect_reads_budget_bid_targeting_page_and_destination() {
    let server = MockServer::start();
    let adset = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/456")
            .query_param(
                "fields",
                "id,name,campaign_id,configured_status,effective_status,daily_budget,lifetime_budget,bid_strategy,bid_amount,bid_constraints,targeting,promoted_object,destination_type",
            );
        then.status(200).json_body(json!({
            "id": "456",
            "name": "Paused set",
            "campaign_id": "100",
            "configured_status": "PAUSED",
            "effective_status": "PAUSED",
            "daily_budget": "500",
            "bid_strategy": "LOWEST_COST_WITHOUT_CAP",
            "bid_amount": 2,
            "destination_type": "WEBSITE",
            "promoted_object": { "page_id": "111" },
            "targeting": {
                "geo_locations": { "countries": ["MY"] },
                "age_min": 18,
                "age_max": 65,
                "publisher_platforms": ["facebook"],
                "flexible_spec": [{ "interests": [{ "id": "1" }] }]
            }
        }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let reply = connector
        .inspect_ads_object(
            &empty_app(),
            &token_creds("act_123"),
            &AdsInspectRequest {
                kind: AdsInventoryKind::Adset,
                id: "456".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    adset.assert();
    assert_eq!(reply.daily_budget.as_deref(), Some("500"));
    assert_eq!(
        reply.bid_strategy.as_deref(),
        Some("LOWEST_COST_WITHOUT_CAP")
    );
    assert_eq!(reply.page_id.as_deref(), Some("111"));
    assert_eq!(reply.destination_type.as_deref(), Some("WEBSITE"));
    let targeting = reply.targeting.expect("subset");
    assert_eq!(targeting.countries, ["MY"]);
    assert_eq!(targeting.age_min, Some(18));
    assert_eq!(targeting.publisher_platforms, ["facebook"]);

    let creative = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/789")
            .query_param(
                "fields",
                "id,name,status,object_story_spec,actor_id,object_url,link_url,call_to_action_type,product_set_id,instagram_user_id,wamo_whatsapp_identity_spec",
            );
        then.status(200).json_body(json!({
            "id": "789",
            "name": "Hero",
            "status": "ACTIVE",
            "object_story_spec": {
                "page_id": "111",
                "link_data": {
                    "link": "https://example.com/offer",
                    "call_to_action": { "type": "LEARN_MORE", "value": { "link": "https://example.com/offer" } }
                }
            }
        }));
    });
    let creative_reply = connector
        .inspect_ads_object(
            &empty_app(),
            &token_creds("act_123"),
            &AdsInspectRequest {
                kind: AdsInventoryKind::Creative,
                id: "789".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    creative.assert();
    assert_eq!(creative_reply.page_id.as_deref(), Some("111"));
    assert_eq!(
        creative_reply.destination.as_deref(),
        Some("https://example.com/offer")
    );
    assert!(creative_reply.daily_budget.is_none());
    assert_eq!(
        creative_reply.call_to_action_type.as_deref(),
        Some("LEARN_MORE")
    );
}

#[tokio::test]
async fn ads_inspect_maps_catalog_min_roas_identity_and_non_link_destinations() {
    let server = MockServer::start();
    let adset = server.mock(|when, then| {
        when.method(GET).path("/v26.0/456");
        then.status(200).json_body(json!({
            "id": "456",
            "bid_strategy": "LOWEST_COST_WITH_MIN_ROAS",
            "bid_constraints": { "roas_average_floor": 15000 }
        }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let reply = connector
        .inspect_ads_object(
            &empty_app(),
            &token_creds("act_123"),
            &AdsInspectRequest {
                kind: AdsInventoryKind::Adset,
                id: "456".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    adset.assert();
    assert_eq!(reply.roas_average_floor.as_deref(), Some("15000"));

    let catalog = server.mock(|when, then| {
        when.method(GET).path("/v26.0/800");
        then.status(200).json_body(json!({
            "id": "800",
            "status": "ACTIVE",
            "product_set_id": "555",
            "instagram_user_id": "222",
            "wamo_whatsapp_identity_spec": { "wamo_whatsapp_identity_id": "333" },
            "object_story_spec": {
                "page_id": "111",
                "template_data": { "name": "Catalog" }
            }
        }));
    });
    let catalog_reply = connector
        .inspect_ads_object(
            &empty_app(),
            &token_creds("act_123"),
            &AdsInspectRequest {
                kind: AdsInventoryKind::Creative,
                id: "800".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    catalog.assert();
    assert_eq!(catalog_reply.product_set_id.as_deref(), Some("555"));
    assert_eq!(catalog_reply.destination.as_deref(), Some("555"));
    assert_eq!(catalog_reply.instagram_user_id.as_deref(), Some("222"));
    assert_eq!(catalog_reply.whatsapp_identity_id.as_deref(), Some("333"));

    let whatsapp = server.mock(|when, then| {
        when.method(GET).path("/v26.0/801");
        then.status(200).json_body(json!({
            "id": "801",
            "call_to_action_type": "WHATSAPP_MESSAGE",
            "object_story_spec": {
                "page_id": "111",
                "link_data": {
                    "call_to_action": {
                        "type": "WHATSAPP_MESSAGE",
                        "value": { "app_destination": "whatsapp" }
                    }
                }
            }
        }));
    });
    let whatsapp_reply = connector
        .inspect_ads_object(
            &empty_app(),
            &token_creds("act_123"),
            &AdsInspectRequest {
                kind: AdsInventoryKind::Creative,
                id: "801".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    whatsapp.assert();
    assert_eq!(whatsapp_reply.destination.as_deref(), Some("whatsapp"));
    assert_eq!(
        whatsapp_reply.call_to_action_type.as_deref(),
        Some("WHATSAPP_MESSAGE")
    );

    let page_cta = server.mock(|when, then| {
        when.method(GET).path("/v26.0/802");
        then.status(200).json_body(json!({
            "id": "802",
            "call_to_action_type": "LIKE_PAGE",
            "object_story_spec": {
                "video_data": {
                    "call_to_action": {
                        "type": "LIKE_PAGE",
                        "value": { "page": "111" }
                    }
                }
            }
        }));
    });
    let page_reply = connector
        .inspect_ads_object(
            &empty_app(),
            &token_creds("act_123"),
            &AdsInspectRequest {
                kind: AdsInventoryKind::Creative,
                id: "802".into(),
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    page_cta.assert();
    assert_eq!(page_reply.destination.as_deref(), Some("111"));
}

#[tokio::test]
async fn ads_inventory_creatives_do_not_send_effective_status() {
    let server = MockServer::start();
    let forbidden = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/act_123/adcreatives")
            .query_param_exists("effective_status");
        then.status(400).json_body(json!({
            "error": { "code": 100, "message": "effective_status is not a parameter" }
        }));
    });
    let ok = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/act_123/adcreatives")
            .query_param("fields", "id,name,status,object_type")
            .query_param("limit", "25");
        then.status(200).json_body(json!({
            "data": [{ "id": "8", "name": "Hero", "status": "ACTIVE" }]
        }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let reply = connector
        .list_ads_inventory(
            &empty_app(),
            &token_creds("act_123"),
            &AdsInventoryRequest {
                account: None,
                kind: AdsInventoryKind::Creative,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    ok.assert();
    assert_eq!(forbidden.hits(), 0);
    assert_eq!(reply.items[0].id, "8");
}

#[tokio::test]
async fn ads_status_update_posts_active_then_reads_review() {
    let server = MockServer::start();
    let post = server.mock(|when, then| {
        when.method(POST)
            .path("/v26.0/456")
            .body_contains("status=ACTIVE");
        then.status(200).json_body(json!({ "success": true }));
    });
    let get = server.mock(|when, then| {
        when.method(GET).path("/v26.0/456").query_param(
            "fields",
            "id,name,configured_status,effective_status,issues_info",
        );
        then.status(200).json_body(json!({
            "id": "456",
            "name": "Paused set",
            "configured_status": "ACTIVE",
            "effective_status": "ACTIVE"
        }));
    });
    let connector = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let status = connector
        .update_ad_status(
            &empty_app(),
            &token_creds("act_123"),
            &AdsStatusUpdateRequest {
                entity: AdEntity::Adset,
                id: "456".into(),
                status: AdsConfiguredStatus::Active,
            },
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    post.assert();
    get.assert();
    assert_eq!(status.configured_status, "ACTIVE");
    assert_eq!(status.effective_status, "ACTIVE");
}

#[tokio::test]
async fn auth_finish_exchanges_and_resolves_ad_account() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/oauth/access_token");
        then.status(200)
            .json_body(json!({ "access_token": "SHORT" }));
    });
    server.mock(|when, then| {
        when.method(GET)
            .path("/oauth/access_token")
            .query_param("grant_type", "fb_exchange_token")
            .query_param("client_id", "id")
            .query_param("client_secret", "sec");
        then.status(200).json_body(json!({
            "access_token": "LONG",
            "token_type": "bearer",
            "expires_in": 5_184_000
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/me");
        then.status(200)
            .json_body(json!({ "id": "1000", "name": "Akmal" }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/me/adaccounts");
        then.status(200).json_body(json!({
            "data": [ { "account_id": "123", "name": "Main" } ]
        }));
    });
    let t =
        MetaAds::with_origins(format!("{}/v26.0", server.base_url()), server.base_url()).unwrap();
    let creds = t
        .auth_finish(
            &oauth_app(),
            AuthReply::Pasted {
                code: "AQBx".into(),
            },
        )
        .await
        .unwrap();
    match creds {
        AccountCreds::OAuth2 {
            access_token,
            extra,
            ..
        } => {
            assert_eq!(access_token, "LONG");
            assert_eq!(extra.get("user_id").and_then(|v| v.as_str()), Some("1000"));
            assert_eq!(
                extra.get("ad_account_id").and_then(|v| v.as_str()),
                Some("act_123")
            );
            assert!(extra.get("expires_at").and_then(|v| v.as_u64()).is_some());
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn auth_finish_without_ad_account_fails_at_the_door() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/oauth/access_token");
        then.status(200).json_body(json!({ "access_token": "S" }));
    });
    server.mock(|when, then| {
        when.method(GET)
            .path("/oauth/access_token")
            .query_param("grant_type", "fb_exchange_token")
            .query_param("client_id", "id")
            .query_param("client_secret", "sec");
        then.status(200)
            .json_body(json!({ "access_token": "L", "expires_in": 100 }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/me");
        then.status(200)
            .json_body(json!({ "id": "1000", "name": "Akmal" }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/me/adaccounts");
        then.status(200).json_body(json!({ "data": [] }));
    });
    let t =
        MetaAds::with_origins(format!("{}/v26.0", server.base_url()), server.base_url()).unwrap();
    let err = t
        .auth_finish(
            &oauth_app(),
            AuthReply::Pasted {
                code: "AQBx".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "no_ad_account"));
}

#[tokio::test]
async fn insights_query_shape_and_row_mapping() {
    let server = MockServer::start();
    mock_account_currency(&server, "MYR");
    let insights = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/act_123/insights")
            .query_param("level", "campaign")
            .query_param("time_increment", "1")
            .query_param(
                "time_range",
                r#"{"since":"2026-06-01","until":"2026-06-02"}"#,
            )
            .query_param("action_attribution_windows", r#"["7d_click","1d_view"]"#)
            .query_param("fields", "actions,impressions,spend");
        then.status(200).json_body(json!({
            "data": [ {
                "date_start": "2026-06-01",
                "campaign_id": "238001",
                "spend": "12.34",
                "impressions": "4567",
                "actions": [
                    { "action_type": "purchase", "value": "2" },
                    { "action_type": "landing_page_view", "value": "31" }
                ]
            } ]
        }));
    });
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let reply = t
        .insights(
            &empty_app(),
            &token_creds("act_123"),
            &query(),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    insights.assert();
    assert_eq!(reply.account_id, "act_123");
    assert_eq!(reply.currency.as_deref(), Some("MYR"));
    assert_eq!(reply.rows.len(), 1);
    let row = &reply.rows[0];
    assert_eq!(row.entity_id, "238001");
    assert_eq!(row.date_start, "2026-06-01");
    // Graph string numerics parse preserving int-ness; purchases sums
    // only the purchase-ish action rows
    assert_eq!(
        row.metrics.get("spend").and_then(|v| v.as_f64()),
        Some(12.34)
    );
    assert_eq!(
        row.metrics.get("impressions").and_then(|v| v.as_u64()),
        Some(4567)
    );
    assert_eq!(
        row.metrics.get("purchases").and_then(|v| v.as_u64()),
        Some(2)
    );
}

#[tokio::test]
async fn insights_filters_breaks_down_and_derives_purchase_value_and_roas() {
    let server = MockServer::start();
    mock_account_currency(&server, "ILS");
    let insights = server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/act_123/insights")
            .query_param("fields", "action_values,spend")
            .query_param(
                "filtering",
                r#"[{"field":"campaign.id","operator":"IN","value":["100","200"]}]"#,
            )
            .query_param("breakdowns", "country,publisher_platform");
        then.status(200).json_body(json!({
            "data": [ {
                "date_start": "2026-06-01",
                "campaign_id": "100",
                "country": "IL",
                "publisher_platform": "facebook",
                "spend": "25.00",
                "action_values": [
                    { "action_type": "purchase", "value": "100.00" },
                    { "action_type": "landing_page_view", "value": "999" }
                ]
            } ]
        }));
    });
    let mut q = query();
    q.metrics = vec![Metric::PurchaseValue, Metric::Roas];
    q.entity_ids = vec!["200".into(), "100".into(), "100".into()];
    q.breakdowns = vec![
        crate::insights::Breakdown::Country,
        crate::insights::Breakdown::PublisherPlatform,
    ];
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let reply = t
        .insights(
            &empty_app(),
            &token_creds("act_123"),
            &q,
            Deadline::from_secs(30),
        )
        .await
        .unwrap();

    insights.assert();
    assert_eq!(reply.currency.as_deref(), Some("ILS"));
    let row = &reply.rows[0];
    assert_eq!(
        row.metrics.get("purchase_value").and_then(Value::as_f64),
        Some(100.0)
    );
    assert_eq!(row.metrics.get("roas").and_then(Value::as_f64), Some(4.0));
    assert_eq!(
        row.dimensions.get("country").and_then(Value::as_str),
        Some("IL")
    );
    assert_eq!(
        row.dimensions
            .get("publisher_platform")
            .and_then(Value::as_str),
        Some("facebook")
    );
}

#[test]
fn roas_is_null_when_spend_is_zero_or_action_values_are_absent() {
    let mut q = query();
    q.metrics = vec![Metric::Roas];
    let zero_spend = row_from(
        &json!({
            "date_start": "2026-06-01",
            "campaign_id": "100",
            "spend": "0",
            "action_values": [{ "action_type": "purchase", "value": "100" }]
        }),
        &q,
    );
    assert!(zero_spend.metrics["roas"].is_null());
    let absent_values = row_from(
        &json!({
            "date_start": "2026-06-01",
            "campaign_id": "100",
            "spend": "10"
        }),
        &q,
    );
    assert!(absent_values.metrics["roas"].is_null());
}

#[tokio::test]
async fn invalid_entity_filter_stops_before_http() {
    let server = MockServer::start();
    let sink = server.mock(|when, then| {
        when.method(GET).path("/v26.0/act_123/insights");
        then.status(200).json_body(json!({ "data": [] }));
    });
    let mut q = query();
    q.entity_ids = vec!["../not-an-id".into()];
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let err = t
        .insights(
            &empty_app(),
            &token_creds("act_123"),
            &q,
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { reason, .. } if reason == "bad_entity_id:../not-an-id")
    );
    assert_eq!(sink.hits(), 0);
}

#[tokio::test]
async fn ad_accounts_pages_maps_metadata_and_sorts_by_canonical_id() {
    let server = MockServer::start();
    let base = server.base_url();
    let next_base = base.clone();
    let next_page = server.mock(move |when, then| {
        when.method(GET)
            .path("/v26.0/me/adaccounts")
            .query_param("after", "next");
        then.status(200).json_body(json!({
            "data": [{
                "account_id": "123",
                "name": "Primary",
                "currency": "ILS",
                "timezone_name": "Asia/Jerusalem",
                "account_status": 1
            }]
        }));
    });
    server.mock(move |when, then| {
        when.method(GET).path("/v26.0/me/adaccounts").query_param(
            "fields",
            "account_id,name,currency,timezone_name,account_status",
        );
        then.status(200).json_body(json!({
            "data": [{ "account_id": "999", "name": "Secondary" }],
            "paging": { "next": format!("{next_base}/v26.0/me/adaccounts?after=next") }
        }));
    });
    let t = MetaAds::with_base(format!("{base}/v26.0")).unwrap();
    let reply = t
        .ad_accounts(
            &empty_app(),
            &token_creds("act_123"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();

    next_page.assert();
    assert_eq!(reply.site.as_str(), SITE);
    assert_eq!(reply.accounts.len(), 2);
    assert_eq!(reply.accounts[0].id, "act_123");
    assert_eq!(reply.accounts[0].name.as_deref(), Some("Primary"));
    assert_eq!(reply.accounts[0].currency.as_deref(), Some("ILS"));
    assert_eq!(
        reply.accounts[0].timezone.as_deref(),
        Some("Asia/Jerusalem")
    );
    assert_eq!(reply.accounts[0].status.as_deref(), Some("1"));
    assert_eq!(reply.accounts[1].id, "act_999");
}

#[tokio::test]
async fn currency_failure_is_not_silently_reported_as_unknown() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/act_123/insights");
        then.status(200).json_body(json!({ "data": [] }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/act_123");
        then.status(500).body("");
    });
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let err = t
        .insights(
            &empty_app(),
            &token_creds("act_123"),
            &query(),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Network { .. }));
}

#[tokio::test]
async fn account_override_selects_the_act_id() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET)
            .path("/v26.0/act_999")
            .query_param("fields", "currency");
        then.status(200).json_body(json!({ "currency": "MYR" }));
    });
    let insights = server.mock(|when, then| {
        when.method(GET).path("/v26.0/act_999/insights");
        then.status(200).json_body(json!({ "data": [] }));
    });
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let mut q = query();
    q.account = Some("act_999".into());
    let reply = t
        .insights(
            &empty_app(),
            &token_creds("act_123"),
            &q,
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    insights.assert();
    assert_eq!(reply.account_id, "act_999");
    // empty data is an empty reply, not an error
    assert!(reply.rows.is_empty());
}

#[tokio::test]
async fn bad_ad_account_is_invalid_query() {
    let t = MetaAds::new().unwrap();
    let mut q = query();
    q.account = Some("../escape".into());
    let err = t
        .insights(
            &empty_app(),
            &token_creds("act_123"),
            &q,
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { reason, .. } if reason == "bad_ad_account:../escape")
    );
}

#[tokio::test]
async fn missing_ad_account_in_creds_is_auth() {
    let t = MetaAds::new().unwrap();
    let err = t
        .insights(
            &empty_app(),
            &AccountCreds::OAuth2 {
                access_token: "tok".into(),
                refresh_token: None,
                extra: json!({}),
            },
            &query(),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "no_ad_account"));
}

#[tokio::test]
async fn insights_follows_paging_until_exhausted() {
    let server = MockServer::start();
    mock_account_currency(&server, "MYR");
    let base = server.base_url();
    let base2 = base.clone();
    // created before the fallback so the after-param page matches this
    // one; httpmock resolves multiple matches in creation order
    let page1 = server.mock(move |when, then| {
        when.method(GET)
            .path("/v26.0/act_123/insights")
            .query_param_exists("after");
        then.status(200).json_body(json!({
            "data": [ { "date_start": "2026-06-02", "campaign_id": "238001", "spend": "2.00" } ]
        }));
    });
    server.mock(move |when, then| {
        when.method(GET).path("/v26.0/act_123/insights");
        then.status(200).json_body(json!({
            "data": [ { "date_start": "2026-06-01", "campaign_id": "238001", "spend": "1.00" } ],
            "paging": { "cursors": { "after": "CUR" },
                        "next": format!("{base2}/v26.0/act_123/insights?after=CUR&level=campaign") }
        }));
    });
    let t = MetaAds::with_base(format!("{base}/v26.0")).unwrap();
    let reply = t
        .insights(
            &empty_app(),
            &token_creds("act_123"),
            &query(),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    page1.assert();
    assert_eq!(reply.rows.len(), 2);
    // deterministic order regardless of page arrival
    assert_eq!(reply.rows[0].date_start, "2026-06-01");
    assert_eq!(reply.rows[1].date_start, "2026-06-02");
}

#[tokio::test]
async fn insights_paging_is_capped() {
    // A self-referential paging.next (every page promises another)
    // must end in a loud Platform error, not an infinite loop.
    let server = MockServer::start();
    mock_account_currency(&server, "MYR");
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/act_123/insights");
        let base = server.base_url();
        then.status(200).json_body(json!({
            "data": [],
            "paging": { "next": format!("{base}/v26.0/act_123/insights?after=x") }
        }));
    });
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let err = t
        .insights(
            &empty_app(),
            &token_creds("act_123"),
            &query(),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Platform { ref code, .. } if code == "paging_exceeded"));
}

#[tokio::test]
async fn exhausted_deadline_surfaces_timeout_before_http() {
    let server = MockServer::start();
    let sink = server.mock(|when, then| {
        when.method(GET).path("/v26.0/act_123/insights");
        then.status(200).json_body(json!({ "data": [] }));
    });
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let err = t
        .insights(
            &empty_app(),
            &token_creds("act_123"),
            &query(),
            Deadline::from_secs(0),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::DeadlineExceeded { .. }));
    assert_eq!(sink.hits(), 0);
}

#[tokio::test]
async fn code_190_is_token_expired() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/act_123/insights");
        then.status(400).json_body(json!({
            "error": { "code": 190, "message": "Error validating access token: Session has expired" }
        }));
    });
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let err = t
        .insights(
            &empty_app(),
            &token_creds("act_123"),
            &query(),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "token_expired"));
}

#[tokio::test]
async fn refresh_reissues_via_fb_exchange_token_preserving_state() {
    let server = MockServer::start();
    let m = server.mock(|when, then| {
        when.method(GET)
            .path("/oauth/access_token")
            .query_param("grant_type", "fb_exchange_token")
            .query_param("client_id", "id")
            .query_param("client_secret", "sec")
            .query_param("fb_exchange_token", "tok");
        then.status(200).json_body(json!({
            "access_token": "NEW", "expires_in": 5_184_000
        }));
    });
    let t =
        MetaAds::with_origins(format!("{}/v26.0", server.base_url()), server.base_url()).unwrap();
    let new = t
        .refresh(
            &oauth_app(),
            &token_creds("act_123"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    m.assert();
    match new {
        AccountCreds::OAuth2 {
            access_token,
            extra,
            ..
        } => {
            assert_eq!(access_token, "NEW");
            // refresh must not lose the ad account the auth flow stored
            assert_eq!(
                extra.get("ad_account_id").and_then(|v| v.as_str()),
                Some("act_123")
            );
        }
        other => panic!("{other:?}"),
    }
    // refresh requires the app config (client_secret); its absence is
    // a door error, not a mid-flight one
    let err = t
        .refresh(
            &empty_app(),
            &token_creds("act_123"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "missing_app_config"));
}

#[tokio::test]
async fn whoami_maps_id_and_name() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/me");
        then.status(200)
            .json_body(json!({ "id": "1000", "name": "Akmal" }));
    });
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let me = t
        .whoami(&empty_app(), &token_creds("act_123"))
        .await
        .unwrap();
    assert_eq!(me.id, "1000");
    assert_eq!(me.handle.as_deref(), Some("Akmal"));
}

#[tokio::test]
async fn publish_refuses_without_touching_the_network() {
    let t = MetaAds::new().unwrap();
    let intent = Intent {
        site: Site::new(SITE),
        params: json!({}),
        body: crate::types::Body::Text { text: "hi".into() },
        idempotency_key: None,
    };
    let err = t
        .publish(
            &empty_app(),
            &token_creds("act_123"),
            intent,
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidPost { reason, .. } if reason == "publish_unsupported"));
}

#[tokio::test]
async fn insights_async_job_start_status_result_and_cancel() {
    let server = MockServer::start();
    mock_account_currency(&server, "MYR");
    let start = server.mock(|when, then| {
        when.method(POST).path("/v26.0/act_123/insights");
        then.status(200)
            .json_body(json!({ "report_run_id": "999" }));
    });
    let status = server.mock(|when, then| {
        when.method(GET).path("/v26.0/999");
        then.status(200).json_body(json!({
            "async_status": "Job Completed",
            "async_percent_completion": 100
        }));
    });
    let result = server.mock(|when, then| {
        when.method(GET).path("/v26.0/999/insights");
        then.status(200).json_body(json!({
            "data": [{
                "date_start": "2026-06-01",
                "campaign_id": "1",
                "spend": "1.00"
            }]
        }));
    });
    let cancel = server.mock(|when, then| {
        when.method(DELETE).path("/v26.0/999");
        then.status(200).body("");
    });
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let q = query();
    let job = t
        .start_insights_job(
            &empty_app(),
            &token_creds("123"),
            &q,
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(job.id, "999");
    let st = t
        .insights_job(
            &empty_app(),
            &token_creds("123"),
            "999",
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(st.status, InsightsJobStatus::Completed);
    let reply = t
        .insights_job_result(
            &empty_app(),
            &token_creds("123"),
            "999",
            &q,
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(reply.rows.len(), 1);
    t.cancel_insights_job(
        &empty_app(),
        &token_creds("123"),
        "999",
        Deadline::from_secs(30),
    )
    .await
    .unwrap();
    start.assert();
    status.assert();
    result.assert();
    cancel.assert();
    let bad = t
        .insights_job(
            &empty_app(),
            &token_creds("123"),
            "not-a-job",
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(bad, Error::InvalidQuery { reason, .. } if reason == "bad_insights_job_id"));
}

#[test]
fn extra_metrics_map_graph_fields_and_definitions() {
    assert_eq!(meta_field(Metric::Frequency), Some("frequency"));
    assert_eq!(
        meta_field(Metric::VideoThruplay),
        Some("video_thruplay_watched_actions")
    );
    assert_eq!(meta_field(Metric::QualityRanking), Some("quality_ranking"));
    let row = row_from(
        &json!({
            "campaign_id": "1",
            "date_start": "2026-06-01",
            "frequency": "1.4",
            "unique_clicks": "9",
            "inline_link_clicks": "4",
            "inline_link_click_ctr": "0.02",
            "quality_ranking": "ABOVE_AVERAGE",
            "video_thruplay_watched_actions": [{ "action_type": "video_view", "value": "3" }]
        }),
        &InsightsQuery {
            level: InsightsLevel::Campaign,
            metrics: vec![
                Metric::Frequency,
                Metric::UniqueClicks,
                Metric::InlineLinkClicks,
                Metric::InlineLinkClickCtr,
                Metric::QualityRanking,
                Metric::VideoThruplay,
            ],
            range: crate::insights::DateRange {
                from: "2026-06-01".into(),
                to: "2026-06-01".into(),
            },
            attribution: AttributionWindow::OneDayClick,
            account: None,
            entity_ids: vec![],
            breakdowns: vec![],
            report: crate::insights::InsightsReportKind::Performance,
        },
    );
    assert_eq!(row.metrics["frequency"], json!(1.4));
    assert_eq!(row.metrics["unique_clicks"], json!(9));
    assert_eq!(row.metrics["quality_ranking"], json!("ABOVE_AVERAGE"));
    assert_eq!(row.metrics["video_thruplay"], json!(3));
}

#[test]
fn level_id_fields() {
    assert_eq!(InsightsLevel::Account.id_field(), "account_id");
    assert_eq!(InsightsLevel::Campaign.id_field(), "campaign_id");
    assert_eq!(InsightsLevel::Adset.id_field(), "adset_id");
    assert_eq!(InsightsLevel::Ad.id_field(), "ad_id");
}

#[test]
fn access_tier_header_maps_limited_and_full() {
    assert_eq!(
        MarketingApiAccessTierKind::from_header("standard_access"),
        MarketingApiAccessTierKind::Full
    );
    assert_eq!(
        MarketingApiAccessTierKind::from_header("development_access"),
        MarketingApiAccessTierKind::Limited
    );
    assert_eq!(
        MarketingApiAccessTierKind::from_header("limited_access"),
        MarketingApiAccessTierKind::Limited
    );
    assert_eq!(
        MarketingApiAccessTierKind::from_header("nope"),
        MarketingApiAccessTierKind::Unknown
    );
}

#[test]
fn system_user_debug_refuses_a_user_token() {
    let site = Site::new(SITE);
    let err = refuse_non_system_user_debug(&site, Some("USER"), Some(1_800_000_000)).unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "user_token_not_system_user"));
    assert!(refuse_non_system_user_debug(&site, Some("USER"), None).is_ok());
    assert!(refuse_non_system_user_debug(&site, Some("SYSTEM_USER"), None).is_ok());
    assert!(refuse_non_system_user_debug(&site, None, None).is_ok());
    let err = refuse_non_system_user_debug(&site, Some("PAGE"), None).unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "wrong_token_type:PAGE"));
}

#[test]
fn access_tier_headers_require_json() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-fb-ads-insights-throttle",
        "not-json ads_api_access_tier".parse().unwrap(),
    );
    assert_eq!(access_tier_from_headers(&headers), None);
    headers.insert(
        "x-fb-ads-insights-throttle",
        r#"{"ads_api_access_tier":"development_access"}"#.parse().unwrap(),
    );
    assert_eq!(
        access_tier_from_headers(&headers).as_deref(),
        Some("development_access")
    );
}

#[tokio::test]
async fn system_user_bootstrap_stores_kind_and_ad_account() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/debug_token");
        then.status(200).json_body(json!({
            "data": {
                "app_id": "id",
                "type": "SYSTEM_USER",
                "is_valid": true,
                "expires_at": 0,
                "scopes": ["ads_management", "ads_read"],
                "user_id": "55"
            }
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/me");
        then.status(200)
            .json_body(json!({ "id": "55", "name": "Postkit Bot" }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/me/adaccounts");
        then.status(200).json_body(json!({
            "data": [{ "account_id": "123", "name": "Test", "currency": "MYR" }]
        }));
    });
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let creds = t
        .bootstrap_system_user_token(&oauth_app(), "SYS", Deadline::from_secs(30))
        .await
        .unwrap();
    match &creds {
        AccountCreds::OAuth2 {
            access_token,
            extra,
            ..
        } => {
            assert_eq!(access_token, "SYS");
            assert_eq!(
                extra.get("token_kind").and_then(|v| v.as_str()),
                Some(SYSTEM_USER_TOKEN_KIND)
            );
            assert_eq!(extra.get("user_id").and_then(|v| v.as_str()), Some("55"));
            assert_eq!(
                extra.get("ad_account_id").and_then(|v| v.as_str()),
                Some("act_123")
            );
        }
        other => panic!("{other:?}"),
    }
    let err = t
        .refresh(&oauth_app(), &creds, Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "system_user_no_refresh"));
    assert!(!crate::refresh_is_due(&creds));
}

#[tokio::test]
async fn system_user_bootstrap_rejects_a_user_oauth_token() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/debug_token");
        then.status(200).json_body(json!({
            "data": {
                "type": "USER",
                "is_valid": true,
                "expires_at": 1_800_000_000,
                "user_id": "1"
            }
        }));
    });
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let err = t
        .bootstrap_system_user_token(&oauth_app(), "EAA", Deadline::from_secs(30))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Auth { reason, .. } if reason == "user_token_not_system_user"));
}

#[tokio::test]
async fn system_user_bootstrap_accepts_never_expiring_user_type() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/debug_token");
        then.status(200).json_body(json!({
            "data": {
                "app_id": "id",
                "type": "USER",
                "is_valid": true,
                "expires_at": 0,
                "user_id": 55
            }
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/me");
        then.status(200)
            .json_body(json!({ "id": "55", "name": "Bot" }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/me/adaccounts");
        then.status(200)
            .json_body(json!({ "data": [{ "account_id": "9" }] }));
    });
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let creds = t
        .bootstrap_system_user_token(&oauth_app(), "SYS", Deadline::from_secs(30))
        .await
        .unwrap();
    match &creds {
        AccountCreds::OAuth2 { extra, .. } => {
            assert_eq!(
                extra.get("token_kind").and_then(|v| v.as_str()),
                Some(SYSTEM_USER_TOKEN_KIND)
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn inspect_token_omits_the_secret_and_maps_debug_fields() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/debug_token");
        then.status(200).json_body(json!({
            "data": {
                "app_id": "id",
                "application": "Postkit",
                "type": "SYSTEM_USER",
                "is_valid": true,
                "expires_at": 0,
                "data_access_expires_at": 1_800_000_000,
                "scopes": ["ads_read"],
                "user_id": "55"
            }
        }));
    });
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let creds = AccountCreds::OAuth2 {
        access_token: "secret-token-value".into(),
        refresh_token: None,
        extra: json!({ "token_kind": SYSTEM_USER_TOKEN_KIND, "ad_account_id": "act_123" }),
    };
    let inspection = t
        .inspect_access_token(&oauth_app(), &creds, Deadline::from_secs(30))
        .await
        .unwrap();
    let encoded = serde_json::to_string(&inspection).unwrap();
    assert!(!encoded.contains("secret-token-value"));
    assert!(!encoded.contains("sec"));
    assert_eq!(inspection.token_kind, AdsTokenKind::SystemUser);
    assert_eq!(inspection.debug_type.as_deref(), Some("SYSTEM_USER"));
    assert!(inspection.is_valid);
    assert_eq!(inspection.expires_at, None);
    assert_eq!(inspection.scopes, vec!["ads_read"]);
}

#[tokio::test]
async fn access_tier_reads_ads_api_access_tier_header() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v26.0/me/adaccounts");
        then.status(200)
            .header(
                "X-FB-Ads-Insights-Throttle",
                r#"{"app_id_util_pct":1,"ads_api_access_tier":"standard_access"}"#,
            )
            .json_body(json!({ "data": [{ "account_id": "123" }] }));
    });
    let t = MetaAds::with_base(format!("{}/v26.0", server.base_url())).unwrap();
    let reply = t
        .marketing_api_access_tier(&oauth_app(), &token_creds("123"), Deadline::from_secs(30))
        .await
        .unwrap();
    assert_eq!(reply.tier, MarketingApiAccessTierKind::Full);
    assert_eq!(reply.raw.as_deref(), Some("standard_access"));
    assert_eq!(reply.source, "response_header");
    assert!(reply.dashboard.contains("Marketing API Access Tier"));
}
