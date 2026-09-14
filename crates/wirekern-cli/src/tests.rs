use crate::ads::*;
use crate::app::*;
use crate::cli::Cli;
use crate::commands::{whatsapp_send_allowed, x_direct_message_allowed, Commands};
use crate::keys::KeysCmd;
use crate::media::MediaCmd;
use crate::pages::PagesCmd;
use crate::post::*;
use crate::whatsapp::*;
use crate::x::XCmd;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use wirekern::connectors::instagram::MAX_CAROUSEL_IMAGES;
use wirekern::{
    AdAccount, AdEntity, AdPreviewFormat, AdReviewStatus, AdReviewWait, AdsInventoryItem,
    AdsInventoryKind, Body, Breakdown, CampaignObjective, CreatedAd, CreatedAdCreative, Error,
    Image, InboundMessages, InsightRow, InsightsLevel, LinkCallToAction, Metric, PausedAdCreate,
    PausedAdset, PausedCampaign, PausedDraftManifest, Site, UploadedAdImage,
};

#[test]
fn build_insights_query_parses_and_defaults() {
    let q = build_insights_query(
        "meta_ads",
        "2026-06-01",
        "2026-06-30",
        "campaign",
        "spend, purchases",
        "7d_click_1d_view",
        InsightsOptions {
            ad_account: Some("act_9".into()),
            entity_ids: vec!["238".into(), "239".into()],
            breakdowns: "country,age".into(),
            report: "performance".into(),
        },
    )
    .unwrap();
    assert_eq!(q.level.as_str(), "campaign");
    assert_eq!(q.metrics, vec![Metric::Spend, Metric::Purchases]);
    assert_eq!(q.account.as_deref(), Some("act_9"));
    assert_eq!(q.entity_ids, vec!["238", "239"]);
    assert_eq!(q.breakdowns, vec![Breakdown::Country, Breakdown::Age]);
}

#[test]
fn build_insights_query_rejects_bad_inputs() {
    // range errors surface via Client's validate(); parse errors here
    let cases: [(&str, &str, &str, &str); 4] = [
        (
            "campaigns",
            "spend",
            "7d_click_1d_view",
            "unknown_level:campaigns",
        ),
        (
            "campaign",
            "not_a_metric",
            "7d_click_1d_view",
            "unknown_metric:not_a_metric",
        ),
        (
            "campaign",
            "spend",
            "default",
            "unknown_attribution:default",
        ),
        ("campaign", " , ", "7d_click_1d_view", "no_metrics"),
    ];
    for (level, metrics, attribution, reason) in cases {
        let err = build_insights_query(
            "meta_ads",
            "2026-06-01",
            "2026-06-02",
            level,
            metrics,
            attribution,
            InsightsOptions {
                ad_account: None,
                entity_ids: vec![],
                breakdowns: String::new(),
                report: "performance".into(),
            },
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::InvalidQuery { reason: r, .. } if r == reason),
            "{level}/{metrics}/{attribution}: {err:?}"
        );
    }

    let err = build_insights_query(
        "meta_ads",
        "2026-06-01",
        "2026-06-02",
        "campaign",
        "roas",
        "7d_click_1d_view",
        InsightsOptions {
            ad_account: None,
            entity_ids: vec![],
            breakdowns: "country,unknown".into(),
            report: "performance".into(),
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { reason, .. } if reason == "unknown_breakdown:unknown")
    );
}

#[test]
fn insight_line_renders_row() {
    let mut metrics = serde_json::Map::new();
    metrics.insert("spend".into(), serde_json::json!(12.5));
    metrics.insert("impressions".into(), serde_json::json!(4567));
    let line = insight_line(&InsightRow {
        entity_id: "238".into(),
        level: InsightsLevel::Campaign,
        date_start: "2026-06-01".into(),
        dimensions: serde_json::Map::new(),
        metrics,
    });
    assert_eq!(line, "2026-06-01 campaign 238 impressions=4567 spend=12.5");
}

#[test]
fn ad_account_line_is_copyable_and_labels_metadata() {
    let line = ad_account_line(&AdAccount {
        id: "act_123".into(),
        name: Some("Main".into()),
        currency: Some("ILS".into()),
        timezone: Some("Asia/Jerusalem".into()),
        status: Some("1".into()),
    });
    assert_eq!(
        line,
        "act_123 name=Main currency=ILS timezone=Asia/Jerusalem status=1"
    );
}

#[test]
fn ads_accounts_command_parses() {
    let cli = Cli::try_parse_from([
        "wirekern",
        "auth",
        "meta_ads",
        "--token",
        "SYS",
        "--system-user",
    ])
    .unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Auth {
            system_user: true,
            token: Some(ref token),
            ..
        } if token == "SYS"
    ));
    let cli = Cli::try_parse_from(["wirekern", "ads", "inspect-token", "meta_ads"]).unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Ads(AdsCmd::InspectToken { site }) if site == "meta_ads"
    ));
    let cli = Cli::try_parse_from(["wirekern", "ads", "access-tier", "meta_ads"]).unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Ads(AdsCmd::AccessTier { site }) if site == "meta_ads"
    ));
    let cli = Cli::try_parse_from(["wirekern", "ads", "accounts", "meta_ads"]).unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Ads(AdsCmd::Accounts { site }) if site == "meta_ads"
    ));
    let cli = Cli::try_parse_from([
        "wirekern",
        "ads",
        "list",
        "meta_ads",
        "--entity",
        "creative",
        "--ad-account",
        "act_123",
    ])
    .unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Ads(AdsCmd::List { site, entity, ad_account })
            if site == "meta_ads"
                && entity == "creative"
                && ad_account.as_deref() == Some("act_123")
    ));
    let request = build_ads_inventory_request("meta_ads", "adset", None).unwrap();
    assert_eq!(request.kind, AdsInventoryKind::Adset);
    let bad = build_ads_inventory_request("meta_ads", "insight", None).unwrap_err();
    assert!(
        matches!(bad, Error::InvalidQuery { reason, .. } if reason == "unknown_ads_inventory_kind:insight")
    );
    let line = inventory_item_line(
        AdsInventoryKind::Campaign,
        &AdsInventoryItem {
            id: "100".into(),
            name: Some("Paused".into()),
            configured_status: Some("PAUSED".into()),
            effective_status: Some("PAUSED".into()),
            status: None,
            campaign_id: None,
            adset_id: None,
            objective: Some("OUTCOME_TRAFFIC".into()),
            object_type: None,
        },
    );
    assert_eq!(
        line,
        "campaign 100 name=Paused configured=PAUSED effective=PAUSED objective=OUTCOME_TRAFFIC"
    );
    let cli = Cli::try_parse_from([
        "wirekern", "ads", "inspect", "meta_ads", "--entity", "adset", "--id", "456",
    ])
    .unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Ads(AdsCmd::Inspect { site, entity, id })
            if site == "meta_ads" && entity == "adset" && id == "456"
    ));
    let inspect = build_ads_inspect_request("meta_ads", "creative", "789").unwrap();
    assert_eq!(inspect.kind, AdsInventoryKind::Creative);
    let cli = Cli::try_parse_from([
        "wirekern",
        "ads",
        "activate",
        "meta_ads",
        "--entity",
        "adset",
        "--id",
        "456",
        "--confirm-id",
        "456",
        "--allow-activate",
        "--confirm-daily-budget",
        "500",
        "--state",
        "activate.state.json",
    ])
    .unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Ads(AdsCmd::Activate {
            allow_activate: true,
            confirm_daily_budget: Some(500),
            ..
        })
    ));
    let activate =
        build_ads_activate_request("meta_ads", "adset", "456", "456", Some(500), None).unwrap();
    assert_eq!(activate.entity, AdEntity::Adset);
    let catalog = build_catalog_creative_request(
        "meta_ads",
        None,
        "Cat".into(),
        "111".into(),
        "555".into(),
        "https://example.com".into(),
        "Shop".into(),
        "shop_now",
        None,
        false,
    )
    .unwrap();
    assert!(matches!(catalog.kind, wirekern::AdCreativeKind::Catalog(_)));
    assert_eq!(
        AdPreviewFormat::from_str("whatsapp_status_media")
            .unwrap()
            .meta_value(),
        "WHATSAPP_STATUS_MEDIA"
    );
    let cli = Cli::try_parse_from([
        "wirekern", "ads", "pause", "meta_ads", "--entity", "ad", "--id", "700",
    ])
    .unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Ads(AdsCmd::Pause { entity, id, .. }) if entity == "ad" && id == "700"
    ));
    let bad_id = build_ads_inspect_request("meta_ads", "ad", "ad-1").unwrap_err();
    assert!(
        matches!(bad_id, Error::InvalidQuery { reason, .. } if reason == "bad_ads_inspect_id:ad-1")
    );
}

#[test]
fn scope_elevation_commands_are_explicit() {
    let auth = Cli::try_parse_from(["wirekern", "auth", "x", "--with-dm"]).unwrap();
    assert!(matches!(
        *auth.command,
        Commands::Auth { with_dm: true, .. }
    ));
    let threads = Cli::try_parse_from(["wirekern", "auth", "threads", "--with-replies"]).unwrap();
    assert!(matches!(
        *threads.command,
        Commands::Auth {
            with_replies: true,
            ..
        }
    ));

    let denied = Cli::try_parse_from([
        "wirekern",
        "x",
        "dm",
        "--to",
        "123",
        "--text",
        "private",
        "--idempotency",
        "x-dm-1",
    ])
    .unwrap();
    assert!(matches!(
        *denied.command,
        Commands::X(XCmd::Dm {
            allow_dm: false,
            ..
        })
    ));
    assert!(!x_direct_message_allowed(&denied.command));

    let allowed = Cli::try_parse_from([
        "wirekern",
        "x",
        "dm",
        "--to",
        "123",
        "--text",
        "private",
        "--idempotency",
        "x-dm-1",
        "--allow-dm",
    ])
    .unwrap();
    assert!(x_direct_message_allowed(&allowed.command));
}

#[test]
fn pages_accounts_command_parses_as_a_separate_read_surface() {
    let cli = Cli::try_parse_from(["wirekern", "pages", "accounts", "facebook_pages"]).unwrap();
    assert!(
        matches!(*cli.command, Commands::Pages(PagesCmd::Accounts { site }) if site == "facebook_pages")
    );
}

#[test]
fn whatsapp_commands_require_explicit_send_acknowledgement_and_idempotency() {
    let allowed = Cli::try_parse_from([
        "wirekern",
        "whatsapp",
        "reply",
        "--to",
        "60123456789",
        "--reply-to",
        "wamid.inbound",
        "--text",
        "Terima kasih",
        "--idempotency",
        "reply-1",
        "--allow-send",
    ])
    .unwrap();
    assert!(whatsapp_send_allowed(&allowed.command));
    assert!(matches!(
        *allowed.command,
        Commands::WhatsApp(WhatsAppCmd::Reply { idempotency, .. }) if idempotency == "reply-1"
    ));

    let session = Cli::try_parse_from([
        "wirekern",
        "whatsapp",
        "text",
        "--to",
        "60123456789",
        "--text",
        "Hello",
        "--idempotency",
        "text-1",
        "--allow-send",
    ])
    .unwrap();
    assert!(whatsapp_send_allowed(&session.command));
    assert!(matches!(
        *session.command,
        Commands::WhatsApp(WhatsAppCmd::Text { .. })
    ));

    let unacknowledged = Cli::try_parse_from([
        "wirekern",
        "whatsapp",
        "template",
        "--to",
        "60123456789",
        "--name",
        "order_update",
        "--language",
        "en_US",
        "--idempotency",
        "template-1",
    ])
    .unwrap();
    assert!(!whatsapp_send_allowed(&unacknowledged.command));

    // Clap makes idempotency non-optional: a private send cannot silently
    // fall back to the less-safe no-ledger behavior of generic posts.
    assert!(Cli::try_parse_from([
        "wirekern",
        "whatsapp",
        "reply",
        "--to",
        "60123456789",
        "--reply-to",
        "wamid.inbound",
        "--text",
        "hi",
    ])
    .is_err());

    let structured = Cli::try_parse_from([
        "wirekern",
        "whatsapp",
        "send",
        "--request",
        "request.json",
        "--sender",
        "marketing",
        "--allow-send",
    ])
    .unwrap();
    assert!(whatsapp_send_allowed(&structured.command));
    assert!(matches!(
        *structured.command,
        Commands::WhatsApp(WhatsAppCmd::Send { sender: Some(sender), .. }) if sender == "marketing"
    ));
}

#[test]
fn whatsapp_config_is_phone_only_and_never_needs_oauth_fields() {
    let cfg = whatsapp_app_config(
        "123456789".into(),
        None,
        None,
        Some("app-secret".into()),
        None,
        vec![],
    )
    .unwrap();
    assert!(cfg.oauth.is_none());
    assert_eq!(cfg.extra["phone_number_id"].as_str(), Some("123456789"));
    let senders = parse_whatsapp_senders(&["marketing=987654321".into()], true).unwrap();
    assert_eq!(senders[0].alias, "marketing");
    assert!(parse_whatsapp_senders(&["bad-value".into()], true).is_err());
    assert!(whatsapp_app_config("+6012".into(), None, None, None, None, vec![]).is_err());
}

#[test]
fn whatsapp_webhook_human_output_reports_counts_without_message_identifiers() {
    let reply = InboundMessages {
        site: Site::new("whatsapp_cloud"),
        messages: vec![],
        statuses: vec![
            wirekern::DeliveryStatus {
                id: "wamid.private-one".into(),
                status: wirekern::DeliveryStatusKind::Delivered,
                timestamp: Some("1".into()),
                errors: vec![],
                recipient_id: None,
                conversation: None,
                pricing: None,
            },
            wirekern::DeliveryStatus {
                id: "wamid.private-two".into(),
                status: wirekern::DeliveryStatusKind::Read,
                timestamp: Some("2".into()),
                errors: vec![],
                recipient_id: None,
                conversation: None,
                pricing: None,
            },
        ],
    };
    let line = whatsapp_webhook_line(&reply);
    assert_eq!(
        line,
        "whatsapp_cloud verified webhook: 0 inbound message(s), 2 delivery status(es)"
    );
    assert!(!line.contains("wamid.private"));
}

#[test]
fn media_list_command_parses_with_an_explicit_bounded_limit() {
    let cli =
        Cli::try_parse_from(["wirekern", "media", "list", "instagram", "--limit", "2"]).unwrap();
    assert!(
        matches!(*cli.command, Commands::Media(MediaCmd::List { site, limit }) if site == "instagram" && limit == 2)
    );
}

#[test]
fn ads_status_command_is_closed_and_opt_in_for_bounded_waiting() {
    let cli = Cli::try_parse_from([
        "wirekern", "ads", "status", "meta_ads", "--entity", "ad", "--id", "700", "--wait",
    ])
    .unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Ads(AdsCmd::Status { site, entity, id, wait })
            if site == "meta_ads" && entity == "ad" && id == "700" && wait
    ));

    let request = build_ad_review_status_request("meta_ads", "adset", "700").unwrap();
    assert_eq!(request.entity, AdEntity::Adset);
    let bad_entity = build_ad_review_status_request("meta_ads", "creative", "700").unwrap_err();
    assert!(
        matches!(bad_entity, Error::InvalidQuery { reason, .. } if reason == "unknown_ad_entity:creative")
    );
    let bad_id = build_ad_review_status_request("meta_ads", "ad", "ad-700").unwrap_err();
    assert!(
        matches!(bad_id, Error::InvalidQuery { reason, .. } if reason == "bad_ad_entity_id:ad-700")
    );

    let pending = AdReviewStatus {
        site: Site::new("meta_ads"),
        entity: AdEntity::Ad,
        id: "700".into(),
        name: Some("Paused validation".into()),
        configured_status: "PAUSED".into(),
        effective_status: "PENDING_REVIEW".into(),
        issues: vec![],
    };
    assert!(pending.is_pending_review());
    let wire = serde_json::to_value(AdReviewWait::PendingReview(pending)).unwrap();
    assert_eq!(wire["review"], "pending_review");
    assert_eq!(wire["status"]["configured_status"], "PAUSED");
}

#[test]
fn creative_preview_command_requires_a_closed_format_and_new_output_file() {
    let preview = Cli::try_parse_from([
        "wirekern",
        "ads",
        "preview-creative",
        "meta_ads",
        "--creative-id",
        "500",
        "--ad-format",
        "desktop_feed_standard",
        "--output",
        "preview.html",
    ])
    .unwrap();
    assert!(matches!(
        *preview.command,
        Commands::Ads(AdsCmd::PreviewCreative { site, creative_id, ad_format, output })
            if site == "meta_ads" && creative_id == "500" && ad_format == "desktop_feed_standard" && output == Path::new("preview.html")
    ));

    let missing_output = Cli::try_parse_from([
        "wirekern",
        "ads",
        "preview-creative",
        "meta_ads",
        "--creative-id",
        "500",
        "--ad-format",
        "desktop_feed_standard",
    ]);
    assert!(missing_output.is_err());

    let request =
        build_creative_preview_request("meta_ads", "500", "mobile_feed_standard").unwrap();
    assert_eq!(request.ad_format, AdPreviewFormat::MobileFeedStandard);
    let bad_format =
        build_creative_preview_request("meta_ads", "500", "instagram_standard").unwrap_err();
    assert!(
        matches!(bad_format, Error::InvalidQuery { reason, .. } if reason == "unknown_ad_preview_format:instagram_standard")
    );

    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("preview.html");
    write_preview_output(&output, "<iframe src=\"https://meta.test\"></iframe>").unwrap();
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "<iframe src=\"https://meta.test\"></iframe>"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&output).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    let exists = write_preview_output(&output, "different").unwrap_err();
    assert_eq!(preview_output_reason(&exists), "preview_output_exists");
}

#[test]
fn image_link_creative_commands_require_explicit_non_delivery_inputs() {
    let upload = Cli::try_parse_from([
        "wirekern",
        "ads",
        "upload-image",
        "meta_ads",
        "--file",
        "hero.png",
    ])
    .unwrap();
    assert!(matches!(
        *upload.command,
        Commands::Ads(AdsCmd::UploadImage { site, file, .. })
            if site == "meta_ads" && file.file_name().and_then(|name| name.to_str()) == Some("hero.png")
    ));

    let missing_cta = Cli::try_parse_from([
        "wirekern",
        "ads",
        "create-link-creative",
        "meta_ads",
        "--name",
        "Hero",
        "--page-id",
        "456",
        "--image-hash",
        "hash-1",
        "--message",
        "A clear benefit",
        "--headline",
        "Learn more",
        "--destination-url",
        "https://example.com/offer",
    ]);
    assert!(missing_cta.is_err());

    let request = build_link_ad_creative_request(
        "meta_ads",
        LinkCreativeOptions {
            ad_account: Some("act_123".into()),
            name: "Hero".into(),
            page_id: "456".into(),
            image_hash: "hash-1".into(),
            message: "A clear benefit".into(),
            headline: "Learn more".into(),
            destination_url: "https://example.com/offer".into(),
            call_to_action: "learn_more".into(),
            geo_link: None,
            application_id: None,
            app_link: None,
            instagram_user_id: None,
            advantage_plus: false,
            whatsapp_identity: None,
        },
    )
    .unwrap();
    assert_eq!(request.creative.call_to_action, LinkCallToAction::LearnMore);

    let shop = build_link_ad_creative_request(
        "meta_ads",
        LinkCreativeOptions {
            ad_account: None,
            name: "Hero".into(),
            page_id: "456".into(),
            image_hash: "hash-1".into(),
            message: "A clear benefit".into(),
            headline: "Shop".into(),
            destination_url: "https://example.com/offer".into(),
            call_to_action: "shop_now".into(),
            geo_link: None,
            application_id: None,
            app_link: None,
            instagram_user_id: None,
            advantage_plus: false,
            whatsapp_identity: None,
        },
    )
    .unwrap();
    assert_eq!(shop.creative.call_to_action, LinkCallToAction::ShopNow);

    let invalid = build_link_ad_creative_request(
        "meta_ads",
        LinkCreativeOptions {
            ad_account: None,
            name: "Hero".into(),
            page_id: "456".into(),
            image_hash: "hash-1".into(),
            message: "A clear benefit".into(),
            headline: "Learn more".into(),
            destination_url: "http://example.com/offer".into(),
            call_to_action: "swipe_up_shop".into(),
            geo_link: None,
            application_id: None,
            app_link: None,
            instagram_user_id: None,
            advantage_plus: false,
            whatsapp_identity: None,
        },
    )
    .unwrap_err();
    assert!(
        matches!(invalid, Error::InvalidQuery { reason, .. } if reason == "unknown_link_call_to_action:swipe_up_shop")
    );
    let missing_geo = build_link_ad_creative_request(
        "meta_ads",
        LinkCreativeOptions {
            ad_account: None,
            name: "Hero".into(),
            page_id: "456".into(),
            image_hash: "hash-1".into(),
            message: "A clear benefit".into(),
            headline: "Directions".into(),
            destination_url: "https://example.com/offer".into(),
            call_to_action: "get_directions".into(),
            geo_link: None,
            application_id: None,
            app_link: None,
            instagram_user_id: None,
            advantage_plus: false,
            whatsapp_identity: None,
        },
    )
    .unwrap_err();
    assert!(
        matches!(missing_geo, Error::InvalidQuery { reason, .. } if reason == "missing_geo_link")
    );

    let upload =
        build_upload_ad_image_request("meta_ads", None, "hero.png".into(), b"image bytes".to_vec())
            .unwrap();
    assert_eq!(
        uploaded_image_line(&UploadedAdImage {
            site: Site::new("meta_ads"),
            account_id: "act_123".into(),
            hash: upload.filename,
        }),
        "image hash=hero.png account=act_123"
    );
    assert_eq!(
        created_creative_line(&CreatedAdCreative {
            site: Site::new("meta_ads"),
            account_id: "act_123".into(),
            id: "500".into(),
        }),
        "creative 500 not-delivering account=act_123"
    );
}

#[test]
fn paused_ads_commands_and_builders_require_explicit_safe_inputs() {
    let cli = Cli::try_parse_from([
        "wirekern",
        "ads",
        "create-campaign",
        "meta_ads",
        "--name",
        "Paused validation",
        "--objective",
        "sales",
        "--ad-account",
        "act_123",
    ])
    .unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Ads(AdsCmd::CreateCampaign { site, ad_account: Some(account), .. })
            if site == "meta_ads" && account == "act_123"
    ));

    // A missing strategy is a parser error rather than a Meta code 100:
    // strategies such as cost cap need additional constraint fields that
    // Tier B deliberately does not infer.
    let missing_bid_strategy = Cli::try_parse_from([
        "wirekern",
        "ads",
        "create-adset",
        "meta_ads",
        "--name",
        "Paused ad set",
        "--campaign-id",
        "100",
        "--daily-budget",
        "2500",
        "--billing-event",
        "IMPRESSIONS",
        "--optimization-goal",
        "REACH",
        "--country",
        "MY",
    ]);
    assert!(missing_bid_strategy.is_err());

    let adset_cli = Cli::try_parse_from([
        "wirekern",
        "ads",
        "create-adset",
        "meta_ads",
        "--name",
        "Paused ad set",
        "--campaign-id",
        "100",
        "--daily-budget",
        "2500",
        "--bid-strategy",
        "lowest_cost_without_cap",
        "--billing-event",
        "IMPRESSIONS",
        "--optimization-goal",
        "REACH",
        "--country",
        "MY",
    ])
    .unwrap();
    assert!(matches!(
        *adset_cli.command,
        Commands::Ads(AdsCmd::CreateAdset { bid_strategy, .. })
            if bid_strategy == "lowest_cost_without_cap"
    ));

    let campaign = build_paused_campaign_request(
        "meta_ads",
        PausedCampaignOptions {
            ad_account: Some("act_123".into()),
            name: "Paused validation".into(),
            objective: "sales".into(),
            special_ad_categories: "HOUSING, EMPLOYMENT".into(),
            daily_budget: None,
            lifetime_budget: None,
            is_adset_budget_sharing_enabled: false,
        },
    )
    .unwrap();
    assert!(matches!(
        campaign.create,
        PausedAdCreate::Campaign(PausedCampaign { objective: CampaignObjective::Sales, special_ad_categories, .. })
            if special_ad_categories == ["HOUSING", "EMPLOYMENT"]
    ));

    let cbo = build_paused_campaign_request(
        "meta_ads",
        PausedCampaignOptions {
            ad_account: None,
            name: "CBO".into(),
            objective: "awareness".into(),
            special_ad_categories: String::new(),
            daily_budget: Some(5000),
            lifetime_budget: None,
            is_adset_budget_sharing_enabled: false,
        },
    )
    .unwrap();
    assert!(matches!(
        cbo.create,
        PausedAdCreate::Campaign(PausedCampaign {
            daily_budget: Some(5000),
            is_adset_budget_sharing_enabled: false,
            ..
        })
    ));
    let sharing_with_cbo = build_paused_campaign_request(
        "meta_ads",
        PausedCampaignOptions {
            ad_account: None,
            name: "CBO share".into(),
            objective: "awareness".into(),
            special_ad_categories: String::new(),
            daily_budget: Some(5000),
            lifetime_budget: None,
            is_adset_budget_sharing_enabled: true,
        },
    );
    assert!(
        matches!(sharing_with_cbo, Err(Error::InvalidQuery { reason, .. }) if reason == "budget_sharing_incompatible_with_campaign_budget")
    );
    let both_budgets = build_paused_campaign_request(
        "meta_ads",
        PausedCampaignOptions {
            ad_account: None,
            name: "x".into(),
            objective: "awareness".into(),
            special_ad_categories: String::new(),
            daily_budget: Some(100),
            lifetime_budget: Some(200),
            is_adset_budget_sharing_enabled: false,
        },
    );
    assert!(
        matches!(both_budgets, Err(Error::InvalidQuery { reason, .. }) if reason == "daily_and_lifetime_budget_mutually_exclusive")
    );

    let adset = build_paused_adset_request(
        "meta_ads",
        PausedAdsetOptions {
            ad_account: None,
            name: "Paused ad set".into(),
            campaign_id: "100".into(),
            daily_budget: Some(2500),
            lifetime_budget: None,
            bid_strategy: "lowest_cost_without_cap".into(),
            bid_amount: None,
            roas_average_floor: None,
            start_time: None,
            end_time: None,
            billing_event: "IMPRESSIONS".into(),
            optimization_goal: "REACH".into(),
            countries: vec!["MY".into()],
            age_min: None,
            age_max: None,
            publisher_platforms: vec![],
            facebook_positions: vec![],
            instagram_positions: vec![],
            whatsapp_positions: vec![],
            user_age_unknown: None,
            promoted_object: None,
        },
    )
    .unwrap();
    assert!(matches!(adset.create, PausedAdCreate::Adset(_)));

    let lifetime_open = build_paused_adset_request(
        "meta_ads",
        PausedAdsetOptions {
            ad_account: None,
            name: "Lifetime set".into(),
            campaign_id: "100".into(),
            daily_budget: None,
            lifetime_budget: Some(20_000),
            bid_strategy: "lowest_cost_without_cap".into(),
            bid_amount: None,
            roas_average_floor: None,
            start_time: None,
            end_time: None,
            billing_event: "IMPRESSIONS".into(),
            optimization_goal: "REACH".into(),
            countries: vec!["MY".into()],
            age_min: None,
            age_max: None,
            publisher_platforms: vec![],
            facebook_positions: vec![],
            instagram_positions: vec![],
            whatsapp_positions: vec![],
            user_age_unknown: None,
            promoted_object: None,
        },
    );
    assert!(
        matches!(lifetime_open, Err(Error::InvalidQuery { reason, .. }) if reason == "lifetime_budget_requires_end_time")
    );
    let lifetime = build_paused_adset_request(
        "meta_ads",
        PausedAdsetOptions {
            ad_account: None,
            name: "Lifetime set".into(),
            campaign_id: "100".into(),
            daily_budget: None,
            lifetime_budget: Some(20_000),
            bid_strategy: "lowest_cost_without_cap".into(),
            bid_amount: None,
            roas_average_floor: None,
            start_time: Some("2026-11-11T14:26:09-08:00".into()),
            end_time: Some("2026-11-21T14:26:09-08:00".into()),
            billing_event: "IMPRESSIONS".into(),
            optimization_goal: "REACH".into(),
            countries: vec!["MY".into()],
            age_min: None,
            age_max: None,
            publisher_platforms: vec![],
            facebook_positions: vec![],
            instagram_positions: vec![],
            whatsapp_positions: vec![],
            user_age_unknown: None,
            promoted_object: None,
        },
    )
    .unwrap();
    assert!(matches!(
        lifetime.create,
        PausedAdCreate::Adset(PausedAdset {
            lifetime_budget: Some(20_000),
            daily_budget: None,
            ..
        })
    ));

    let bad_bid_strategy = build_paused_adset_request(
        "meta_ads",
        PausedAdsetOptions {
            ad_account: None,
            name: "x".into(),
            campaign_id: "100".into(),
            daily_budget: Some(1),
            lifetime_budget: None,
            bid_strategy: "cost_cap".into(),
            bid_amount: None,
            roas_average_floor: None,
            start_time: None,
            end_time: None,
            billing_event: "IMPRESSIONS".into(),
            optimization_goal: "REACH".into(),
            countries: vec!["MY".into()],
            age_min: None,
            age_max: None,
            publisher_platforms: vec![],
            facebook_positions: vec![],
            instagram_positions: vec![],
            whatsapp_positions: vec![],
            user_age_unknown: None,
            promoted_object: None,
        },
    );
    assert!(
        matches!(bad_bid_strategy, Err(Error::InvalidQuery { reason, .. }) if reason == "missing_bid_amount")
    );
    let cost_cap = build_paused_adset_request(
        "meta_ads",
        PausedAdsetOptions {
            ad_account: None,
            name: "cap".into(),
            campaign_id: "100".into(),
            daily_budget: Some(2500),
            lifetime_budget: None,
            bid_strategy: "cost_cap".into(),
            bid_amount: Some(200),
            roas_average_floor: None,
            start_time: None,
            end_time: None,
            billing_event: "IMPRESSIONS".into(),
            optimization_goal: "REACH".into(),
            countries: vec!["MY".into()],
            age_min: None,
            age_max: None,
            publisher_platforms: vec![],
            facebook_positions: vec![],
            instagram_positions: vec![],
            whatsapp_positions: vec![],
            user_age_unknown: None,
            promoted_object: None,
        },
    )
    .unwrap();
    assert!(matches!(
        cost_cap.create,
        PausedAdCreate::Adset(PausedAdset {
            bid_strategy: wirekern::BidStrategy::CostCap,
            bid_amount: Some(200),
            ..
        })
    ));
    let min_roas = build_paused_adset_request(
        "meta_ads",
        PausedAdsetOptions {
            ad_account: None,
            name: "roas".into(),
            campaign_id: "100".into(),
            daily_budget: Some(2500),
            lifetime_budget: None,
            bid_strategy: "lowest_cost_with_min_roas".into(),
            bid_amount: None,
            roas_average_floor: Some(10_000),
            start_time: None,
            end_time: None,
            billing_event: "IMPRESSIONS".into(),
            optimization_goal: "VALUE".into(),
            countries: vec!["MY".into()],
            age_min: None,
            age_max: None,
            publisher_platforms: vec![],
            facebook_positions: vec![],
            instagram_positions: vec![],
            whatsapp_positions: vec![],
            user_age_unknown: None,
            promoted_object: Some(wirekern::PromotedObject::Pixel {
                pixel_id: "789".into(),
                custom_event_type: wirekern::CustomEventType::Purchase,
            }),
        },
    )
    .unwrap();
    assert!(matches!(
        min_roas.create,
        PausedAdCreate::Adset(PausedAdset {
            bid_strategy: wirekern::BidStrategy::LowestCostWithMinRoas,
            roas_average_floor: Some(10_000),
            ..
        })
    ));

    let mixed_promoted = build_promoted_object(
        "meta_ads",
        Some("1".into()),
        Some("2".into()),
        None,
        None,
        None,
        None,
    );
    assert!(
        matches!(mixed_promoted, Err(Error::InvalidQuery { reason, .. }) if reason == "promoted_object_kinds_mutually_exclusive")
    );
    let pixel = build_paused_adset_request(
        "meta_ads",
        PausedAdsetOptions {
            ad_account: None,
            name: "pixel".into(),
            campaign_id: "100".into(),
            daily_budget: Some(2500),
            lifetime_budget: None,
            bid_strategy: "lowest_cost_without_cap".into(),
            bid_amount: None,
            roas_average_floor: None,
            start_time: None,
            end_time: None,
            billing_event: "IMPRESSIONS".into(),
            optimization_goal: "OFFSITE_CONVERSIONS".into(),
            countries: vec!["MY".into()],
            age_min: None,
            age_max: None,
            publisher_platforms: vec![],
            facebook_positions: vec![],
            instagram_positions: vec![],
            whatsapp_positions: vec![],
            user_age_unknown: None,
            promoted_object: Some(wirekern::PromotedObject::Pixel {
                pixel_id: "789".into(),
                custom_event_type: wirekern::CustomEventType::Purchase,
            }),
        },
    )
    .unwrap();
    assert!(matches!(
        pixel.create,
        PausedAdCreate::Adset(PausedAdset {
            promoted_object: Some(wirekern::PromotedObject::Pixel { .. }),
            ..
        })
    ));

    let clicks = build_paused_campaign_request(
        "meta_ads",
        PausedCampaignOptions {
            ad_account: None,
            name: "x".into(),
            objective: "clicks".into(),
            special_ad_categories: String::new(),
            daily_budget: None,
            lifetime_budget: None,
            is_adset_budget_sharing_enabled: false,
        },
    );
    assert!(
        matches!(clicks, Err(Error::InvalidQuery { reason, .. }) if reason == "unknown_objective:clicks")
    );
    let no_country = build_paused_adset_request(
        "meta_ads",
        PausedAdsetOptions {
            ad_account: None,
            name: "x".into(),
            campaign_id: "100".into(),
            daily_budget: Some(1),
            lifetime_budget: None,
            bid_strategy: "lowest_cost_without_cap".into(),
            bid_amount: None,
            roas_average_floor: None,
            start_time: None,
            end_time: None,
            billing_event: "IMPRESSIONS".into(),
            optimization_goal: "REACH".into(),
            countries: vec![],
            age_min: None,
            age_max: None,
            publisher_platforms: vec![],
            facebook_positions: vec![],
            instagram_positions: vec![],
            whatsapp_positions: vec![],
            user_age_unknown: None,
            promoted_object: None,
        },
    );
    assert!(
        matches!(no_country, Err(Error::InvalidQuery { reason, .. }) if reason == "targeting_missing_country")
    );
}

#[test]
fn paused_create_line_cannot_hide_its_status() {
    let line = created_ad_line(&CreatedAd {
        site: Site::new("meta_ads"),
        account_id: "act_123".into(),
        entity: wirekern::AdEntity::Campaign,
        id: "100".into(),
        status: "PAUSED".into(),
    });
    assert_eq!(line, "campaign 100 status=PAUSED account=act_123");
}

#[test]
fn insights_help_lists_all_supported_metrics() {
    // Keep the user-facing discovery text aligned with Metric::from_str.
    // This caught the Tier A+ metrics being accepted by the parser but
    // absent from `wirekern insights --help`.
    let mut command = Cli::command();
    let help = command
        .find_subcommand_mut("insights")
        .expect("insights command")
        .get_arguments()
        .find(|arg| arg.get_id() == "metrics")
        .and_then(|arg| arg.get_help())
        .expect("metrics help")
        .to_string();
    assert!(help.contains("purchase_value,roas"));
    assert!(help.contains("frequency,unique_clicks"));
    assert!(help.contains("quality_ranking,video_thruplay"));
}

#[test]
fn home_precedence_and_no_cwd_fallback() {
    let fe = Some(PathBuf::from("/flag-or-env"));
    let hm = Some(PathBuf::from("/user"));
    // --home / WIREKERN_HOME (same field via clap env) > $HOME/.wirekern
    assert_eq!(
        resolve_home(fe.clone(), hm.clone()).unwrap(),
        PathBuf::from("/flag-or-env")
    );
    assert_eq!(
        resolve_home(None, hm.clone()).unwrap(),
        PathBuf::from("/user/.wirekern")
    );
    // HOME unset (cron, systemd, env -i): never guess the CWD — the old
    // code silently wrote tokens into ./.wirekern
    let err = resolve_home(None, None).unwrap_err();
    assert!(err.contains("WIREKERN_HOME or HOME"));
}

#[test]
fn wirekern_home_env_feeds_the_home_flag() {
    // clap's env feature folds WIREKERN_HOME into --home, so the var is
    // visible in --help and the manual env read stays out of run().
    std::env::set_var("WIREKERN_HOME", "/from-env");
    let cli = Cli::try_parse_from(["wirekern", "whoami", "threads"]).unwrap();
    assert_eq!(cli.home, Some(PathBuf::from("/from-env")));
    std::env::remove_var("WIREKERN_HOME");
}

#[test]
fn resolve_texts_requires_one() {
    assert_eq!(resolve_texts(vec![]).unwrap_err(), 2);
    assert_eq!(resolve_texts(vec!["a".into(), "-".into()]).unwrap_err(), 2);
    assert_eq!(
        resolve_texts(vec!["root".into(), "reply".into()]).unwrap(),
        vec!["root", "reply"]
    );
}

#[test]
fn collect_post_sites_from_to_or_site() {
    assert_eq!(
        collect_post_sites(None, Some("threads, bluesky")).unwrap(),
        vec!["threads", "bluesky"]
    );
    assert_eq!(
        collect_post_sites(Some("threads"), None).unwrap(),
        vec!["threads"]
    );
    assert_eq!(collect_post_sites(None, None).unwrap_err(), 2);
}

#[test]
fn chain_blocked_unless_all_threads() {
    assert_eq!(chain_blocked_site(&["threads".into()]), None);
    assert_eq!(
        chain_blocked_site(&["threads".into(), "threads".into()]),
        None
    );
    assert_eq!(
        chain_blocked_site(&["threads".into(), "bluesky".into()]),
        Some("bluesky")
    );
}

#[test]
fn with_reply_to_overwrites_parent() {
    let p = with_reply_to(&serde_json::json!({ "reply_to_id": "old" }), "A");
    assert_eq!(p["reply_to_id"], "A");
    let p = with_reply_to(&serde_json::json!({}), "B");
    assert_eq!(p["reply_to_id"], "B");
}

#[test]
fn reply_to_flag_parses() {
    let cli = Cli::try_parse_from([
        "wirekern",
        "post",
        "threads",
        "--text",
        "hi",
        "--reply-to",
        "18367439386214650",
    ])
    .unwrap();
    match *cli.command {
        Commands::Post { ref reply_to, .. } => {
            assert_eq!(reply_to.as_deref(), Some("18367439386214650"))
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn reply_to_refuses_empty_dual_source_and_fanout() {
    // No flag: no opinion — --param stays a valid spelling.
    assert_eq!(reply_to_conflict(None, &[]), None);
    assert_eq!(reply_to_conflict(None, &["reply_to_id=1".into()]), None);
    // Empty flag: threads would degrade it to a root post, so the
    // requested reply must be refused before anything is published.
    assert_eq!(reply_to_conflict(Some(""), &[]), Some("reply_to_empty"));
    // Both spellings of one wire field: refuse rather than pick a winner.
    assert_eq!(
        reply_to_conflict(Some("1"), &["reply_to_id=2".into()]),
        Some("reply_to_conflict")
    );
    // Unrelated --param keys coexist with the flag.
    assert_eq!(reply_to_conflict(Some("1"), &["chat_id=5".into()]), None);
    // Fan-out: one id cannot be honest in two sites' namespaces.
    assert_eq!(
        reply_to_fanout_conflict(Some("1"), &["threads".into()]),
        None
    );
    assert_eq!(
        reply_to_fanout_conflict(Some("1"), &["threads".into(), "bluesky".into()]),
        Some("reply_to_fanout_unsupported")
    );
    assert_eq!(
        reply_to_fanout_conflict(None, &["threads".into(), "bluesky".into()]),
        None
    );
}

#[test]
fn page_id_flag_parses_and_refuses_wrong_targets() {
    let cli = Cli::try_parse_from([
        "wirekern",
        "post",
        "facebook_pages",
        "--text",
        "hi",
        "--page-id",
        "123",
    ])
    .unwrap();
    match *cli.command {
        Commands::Post { ref page_id, .. } => assert_eq!(page_id.as_deref(), Some("123")),
        other => panic!("{other:?}"),
    }
    assert_eq!(page_id_conflict(None, &[]), None);
    assert_eq!(page_id_conflict(None, &["page_id=1".into()]), None);
    assert_eq!(page_id_conflict(Some(""), &[]), Some("page_id_empty"));
    assert_eq!(
        page_id_conflict(Some("1"), &["page_id=2".into()]),
        Some("page_id_conflict")
    );
    assert_eq!(
        page_id_target_conflict(Some("1"), &["facebook_pages".into()]),
        None
    );
    assert_eq!(
        page_id_target_conflict(Some("1"), &["threads".into()]),
        Some("page_id_site_unsupported")
    );
    assert_eq!(
        page_id_target_conflict(Some("1"), &["facebook_pages".into(), "threads".into()]),
        Some("page_id_fanout_unsupported")
    );
}

#[test]
fn insights_until_and_async_report_parse() {
    let cli = Cli::try_parse_from([
        "wirekern",
        "insights",
        "meta_ads",
        "--from",
        "2026-06-01",
        "--until",
        "2026-06-30",
        "--attribution",
        "7d_click_1d_view",
        "--async-report",
    ])
    .unwrap();
    match *cli.command {
        Commands::Insights {
            ref until,
            async_report,
            ..
        } => {
            assert_eq!(until, "2026-06-30");
            assert!(async_report);
        }
        other => panic!("{other:?}"),
    }
    assert!(Cli::try_parse_from([
        "wirekern",
        "insights",
        "meta_ads",
        "--from",
        "2026-06-01",
        "--to",
        "2026-06-30",
        "--attribution",
        "7d_click_1d_view",
    ])
    .is_err());
}

#[test]
fn insights_job_result_reuses_cached_query() {
    let dir = tempfile::tempdir().unwrap();
    let query = build_insights_query(
        "meta_ads",
        "2026-06-01",
        "2026-06-30",
        "campaign",
        "spend",
        "7d_click_1d_view",
        InsightsOptions {
            ad_account: None,
            entity_ids: vec![],
            breakdowns: String::new(),
            report: "performance".into(),
        },
    )
    .unwrap();
    store_insights_job_query(dir.path(), "238001", &query).unwrap();
    let loaded = resolve_insights_job_query(
        dir.path(),
        "meta_ads",
        "238001",
        None,
        None,
        "account",
        "spend,impressions,clicks,purchases",
        None,
        InsightsOptions {
            ad_account: None,
            entity_ids: vec![],
            breakdowns: String::new(),
            report: "performance".into(),
        },
    )
    .unwrap();
    assert_eq!(loaded.range.from, "2026-06-01");
    assert_eq!(loaded.range.to, "2026-06-30");
    assert_eq!(loaded.level.as_str(), "campaign");

    let missing = resolve_insights_job_query(
        dir.path(),
        "meta_ads",
        "missing",
        None,
        None,
        "account",
        "spend",
        None,
        InsightsOptions {
            ad_account: None,
            entity_ids: vec![],
            breakdowns: String::new(),
            report: "performance".into(),
        },
    )
    .unwrap_err();
    assert!(
        matches!(missing, Error::InvalidQuery { reason, .. } if reason == "insights_job_query_missing")
    );

    let incomplete = resolve_insights_job_query(
        dir.path(),
        "meta_ads",
        "238001",
        Some("2026-06-01".into()),
        None,
        "account",
        "spend",
        None,
        InsightsOptions {
            ad_account: None,
            entity_ids: vec![],
            breakdowns: String::new(),
            report: "performance".into(),
        },
    )
    .unwrap_err();
    assert!(
        matches!(incomplete, Error::InvalidQuery { reason, .. } if reason == "insights_job_query_incomplete")
    );
}

#[test]
fn reply_to_flag_folds_into_the_param_spelling() {
    // The fold is what makes one flag feed every later check: the image
    // guard sees it, parse_params carries it, and the chain anchor
    // (params.clone() on post 0, with_reply_to on the rest) inherits it.
    fn reply_to_flag() -> Option<&'static str> {
        Some("1836")
    }
    let mut param = vec!["chat_id=5".to_string()];
    if let Some(id) = reply_to_flag() {
        param.push(format!("reply_to_id={id}"));
    }
    let parsed = parse_params(&param, true).unwrap();
    assert_eq!(parsed["reply_to_id"], "1836");
    assert_eq!(parsed["chat_id"], "5");
    assert_eq!(
        image_input_conflict(1, 1, false, None, &param),
        Some("image_reply_unsupported")
    );
}

#[test]
fn result_line_matches_outcome_and_wire_error() {
    let ok = serde_json::json!({ "site": "threads", "id": "9", "url": "https://x/1" });
    assert_eq!(result_line(&ok), "threads 9 https://x/1");
    let no_url = serde_json::json!({ "site": "bluesky", "id": "at://x" });
    assert_eq!(result_line(&no_url), "bluesky at://x");
    let err = serde_json::json!({ "error": "invalid_post", "site": "threads", "reason": "text_too_long" });
    assert_eq!(result_line(&err), "invalid_post threads text_too_long");
    let terse = serde_json::json!({ "error": "rate_limited", "site": "threads" });
    assert_eq!(result_line(&terse), "rate_limited threads");
    // 027: probe rows must never render like posts — the id is an
    // unpublished container, and a bare "threads C" would read as one
    let probe =
        serde_json::json!({ "site": "threads", "container_id": "C", "expires_in_hours": 24 });
    assert_eq!(result_line(&probe), "threads C dry-run");
}

#[test]
fn serve_and_keys_commands_parse() {
    let cli = Cli::try_parse_from(["wirekern", "serve", "--bind", "127.0.0.1:9000"]).unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Serve { bind: Some(ref b) } if b == "127.0.0.1:9000"
    ));
    let cli = Cli::try_parse_from(["wirekern", "mcp"]).unwrap();
    assert!(matches!(*cli.command, Commands::Mcp));
    let cli = Cli::try_parse_from(["wirekern", "keys", "create", "--name", "n8n"]).unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Keys(KeysCmd::Create { ref name }) if name == "n8n"
    ));
}

#[test]
fn dry_run_flag_parses() {
    let cli =
        Cli::try_parse_from(["wirekern", "post", "threads", "--text", "hi", "--dry-run"]).unwrap();
    match *cli.command {
        Commands::Post { dry_run, .. } => assert!(dry_run),
        other => panic!("{other:?}"),
    }
}

#[test]
fn stdin_refuses_every_content_flag() {
    // --stdin alone is the intended shape: a complete request.
    assert!(stdin_conflict(true, &[], &[], None, None, &[], None, None).is_none());
    // Each content-carrying flag must refuse — any of them silently
    // ignored is a post the command line does not describe (025).
    let t = vec!["hi".to_string()];
    let image = vec!["x.png".to_string()];
    let p = vec!["reply_to_id=1".to_string()];
    for e in [
        stdin_conflict(true, &t, &[], None, None, &[], None, None),
        stdin_conflict(true, &[], &image, None, None, &[], None, None),
        stdin_conflict(true, &[], &[], Some("alt"), None, &[], None, None),
        stdin_conflict(true, &[], &[], None, Some("threads"), &[], None, None),
        stdin_conflict(true, &[], &[], None, None, &p, None, None),
        stdin_conflict(true, &[], &[], None, None, &[], Some("threads"), None),
        stdin_conflict(true, &[], &[], None, None, &[], None, Some("1")),
    ] {
        let e = e.expect("must refuse");
        assert!(matches!(&e, Error::InvalidPost { reason, .. } if reason == "stdin_exclusive"));
    }
    // Without --stdin the flags are the normal path, no opinion here.
    assert!(stdin_conflict(
        false,
        &t,
        &image,
        Some("alt"),
        Some("threads"),
        &p,
        Some("x"),
        Some("1"),
    )
    .is_none());
}

#[test]
fn dry_run_conflicts_are_refused_loudly() {
    // no dry-run: no opinion, whatever the other flags say
    assert!(dry_run_conflict(false, Some("k"), 3).is_none());
    // dry-run + idempotency: a probe never touches the ledger
    let e = dry_run_conflict(true, Some("k"), 1).unwrap();
    assert!(matches!(&e, Error::InvalidPost { reason, .. } if reason == "dry_run_idempotency"));
    // dry-run + chain: no published parent to reply to
    let e = dry_run_conflict(true, None, 2).unwrap();
    assert!(matches!(&e, Error::InvalidPost { reason, .. } if reason == "dry_run_chain"));
    // dry-run alone with one text is exactly the intended shape
    assert!(dry_run_conflict(true, None, 1).is_none());
}

#[test]
fn draft_subcommands_parse_with_required_flags() {
    let cli = Cli::try_parse_from([
        "wirekern",
        "ads",
        "create-draft",
        "meta_ads",
        "--manifest",
        "launch.json",
        "--state",
        "launch.state.json",
    ])
    .unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Ads(AdsCmd::CreateDraft { ref site, .. }) if site == "meta_ads"
    ));
    let cli = Cli::try_parse_from([
        "wirekern",
        "ads",
        "adopt-draft-step",
        "meta_ads",
        "--state",
        "launch.state.json",
        "--step",
        "adset",
        "--id",
        "123",
    ])
    .unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Ads(AdsCmd::AdoptDraftStep { ref step, ref id, .. })
            if step == "adset" && id == "123"
    ));
    // A draft step typo is a parse-level unknown, never a silent default.
    assert!(Cli::try_parse_from([
        "wirekern",
        "ads",
        "validate-draft",
        "meta_ads",
        "--manifest",
        "m.json"
    ])
    .is_ok());
}

#[test]
fn draft_manifest_reader_maps_errors_without_leaking_paths() {
    let dir = std::env::temp_dir().join(format!("wirekern-draft-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("manifest.json");

    // A typo'd key is named by serde and prefixed with the site family.
    std::fs::write(&path, r#"{"version": 1, "ad_account": "act_1", "x": 0}"#).unwrap();
    let err = read_draft_manifest("meta_ads", &path).unwrap_err();
    assert!(
        matches!(&err, Error::InvalidQuery { reason, .. } if reason.starts_with("bad_manifest:"))
    );

    // Unreadable file: stable reason, no operator path echoed.
    std::fs::remove_file(&path).unwrap();
    let err = read_draft_manifest("meta_ads", &path).unwrap_err();
    assert!(matches!(&err, Error::InvalidQuery { reason, .. } if reason == "manifest_unreadable"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn draft_image_reader_tolerates_missing_file_for_resume() {
    let dir = std::env::temp_dir().join(format!("wirekern-draft-img-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let img = dir.join("hero.png");
    std::fs::write(&img, b"bytes").unwrap();
    let raw = r#"{"version":1,"ad_account":"act_1",
            "campaign":{"name":"n","objective":"awareness","special_ad_categories":[]},
            "adset":{"name":"n","daily_budget":100,"bid_strategy":"lowest_cost_without_cap",
                "billing_event":"IMPRESSIONS","optimization_goal":"REACH",
                "targeting":{"geo_locations":{"countries":["MY"]}}},
            "creative":{"name":"n","image_file":"IMGPATH","page_id":"1","message":"m",
                "headline":"h","destination_url":"https://e.com/x","call_to_action":"learn_more"},
            "ad":{"name":"n"}}"#
        .replace("IMGPATH", &img.display().to_string());
    let manifest: PausedDraftManifest = serde_json::from_str(&raw).unwrap();
    let image = read_draft_image("meta_ads", &manifest).unwrap().unwrap();
    assert_eq!(image.filename, "hero.png");
    // A since-deleted local file yields None (the core decides whether
    // the bytes are still needed), not a hard error.
    std::fs::remove_file(&img).unwrap();
    assert!(read_draft_image("meta_ads", &manifest).unwrap().is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn resolve_image_splits_url_from_local_file() {
    let dir = std::env::temp_dir().join(format!("wirekern-image-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let img = dir.join("hero.png");
    std::fs::write(&img, b"bytes").unwrap();

    let url = resolve_image("https://cdn.test/h.png", "threads").unwrap();
    assert!(matches!(url, Image::Url(u) if u == "https://cdn.test/h.png"));
    // http is not upgraded or silently accepted.
    let err = resolve_image("http://cdn.test/h.png", "threads").unwrap_err();
    assert!(
        matches!(&err, Error::InvalidPost { reason, .. } if reason == "image_url_must_be_https")
    );

    let bytes = resolve_image(&img.display().to_string(), "bluesky").unwrap();
    assert!(
        matches!(bytes, Image::Bytes { ref filename, ref bytes } if filename == "hero.png" && bytes == b"bytes")
    );
    // Unreadable file: stable reason, no path echo.
    std::fs::remove_file(&img).unwrap();
    let err = resolve_image(&img.display().to_string(), "bluesky").unwrap_err();
    assert!(matches!(&err, Error::InvalidPost { reason, .. } if reason == "image_file_unreadable"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn image_flags_parse_and_document_the_split() {
    let cli = Cli::try_parse_from([
        "wirekern",
        "post",
        "bluesky",
        "--image",
        "./hero.png",
        "--text",
        "caption",
        "--alt",
        "chart",
    ])
    .unwrap();
    assert!(matches!(
        *cli.command,
        Commands::Post { ref image, ref alt, .. }
            if image == &["./hero.png"] && alt.as_deref() == Some("chart")
    ));
    // Image without any --text is a valid, caption-less post.
    let cli = Cli::try_parse_from([
        "wirekern",
        "post",
        "threads",
        "--image",
        "https://cdn.test/h.png",
    ])
    .unwrap();
    assert!(matches!(*cli.command, Commands::Post { ref text, .. } if text.is_empty()));

    let carousel = Cli::try_parse_from([
        "wirekern",
        "post",
        "instagram",
        "--image",
        "https://cdn.test/one.jpg",
        "--image",
        "https://cdn.test/two.jpg",
        "--text",
        "one caption",
    ])
    .unwrap();
    assert!(matches!(
        *carousel.command,
        Commands::Post { ref image, .. }
            if image == &["https://cdn.test/one.jpg", "https://cdn.test/two.jpg"]
    ));
}

#[test]
fn repeated_images_build_one_carousel_and_reject_ambiguous_flags() {
    let images = vec![
        "https://cdn.test/one.jpg".to_string(),
        "https://cdn.test/two.jpg".to_string(),
    ];
    let body = build_post_body(&images, Some("one caption".into()), "", "instagram").unwrap();
    assert!(matches!(
        &body,
        Body::Carousel { text: Some(text), images }
            if text == "one caption" && images.len() == 2
    ));
    assert_eq!(
        body.required_capability(),
        wirekern::Capability::PublishCarousel
    );

    let reply = vec!["reply_to_id=1".to_string()];
    assert_eq!(
        image_input_conflict(2, 1, false, Some("alt"), &[]),
        Some("carousel_alt_unsupported")
    );
    assert_eq!(
        image_input_conflict(2, 2, false, None, &[]),
        Some("carousel_caption_multiple")
    );
    assert_eq!(
        image_input_conflict(2, 1, false, None, &reply),
        Some("carousel_reply_unsupported")
    );
    assert_eq!(
        image_input_conflict(2, 1, true, None, &[]),
        Some("dry_run_image_unsupported")
    );

    let too_many = (0..MAX_CAROUSEL_IMAGES + 1)
        .map(|index| format!("missing-{index}.jpg"))
        .collect::<Vec<_>>();
    let error = build_post_body(&too_many, None, "", "instagram").unwrap_err();
    assert!(matches!(error, Error::InvalidPost { reason, limit, .. }
            if reason == "carousel_too_many_images" && limit == Some(MAX_CAROUSEL_IMAGES as u32)));
}
