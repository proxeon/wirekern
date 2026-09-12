use super::*;
use crate::types::Site;
use std::str::FromStr;

#[test]
fn objective_is_closed_and_maps_to_meta_outcomes() {
    assert_eq!(
        BillingEvent::from_str("impressions").unwrap().meta_value(),
        "IMPRESSIONS"
    );
    assert_eq!(
        OptimizationGoal::from_str("REACH").unwrap().meta_value(),
        "REACH"
    );
    assert!(billing_event_allowed(
        OptimizationGoal::LinkClicks,
        BillingEvent::LinkClicks
    ));
    assert!(!billing_event_allowed(
        OptimizationGoal::Reach,
        BillingEvent::LinkClicks
    ));
    assert!(supported_adset_pairing(
        CampaignObjective::Awareness,
        OptimizationGoal::Reach,
        BillingEvent::Impressions
    ));
    assert!(!supported_adset_pairing(
        CampaignObjective::Awareness,
        OptimizationGoal::LinkClicks,
        BillingEvent::Impressions
    ));
    assert_eq!(
        CampaignObjective::from_str("sales").unwrap().meta_value(),
        "OUTCOME_SALES"
    );
    assert_eq!(AdEntity::from_str("adset").unwrap(), AdEntity::Adset);
    assert_eq!(
        AdEntity::from_str("creative").unwrap_err(),
        "unknown_ad_entity:creative"
    );
    assert_eq!(
        CampaignObjective::from_str("clicks").unwrap_err(),
        "unknown_objective:clicks"
    );
}

#[test]
fn bid_strategy_is_closed_and_maps_to_meta_wire_value() {
    assert_eq!(
        BidStrategy::from_str("lowest_cost_without_cap")
            .unwrap()
            .meta_value(),
        "LOWEST_COST_WITHOUT_CAP"
    );
    assert_eq!(
        BidStrategy::from_str("cost_cap").unwrap().meta_value(),
        "COST_CAP"
    );
    assert_eq!(
        BidStrategy::from_str("lowest_cost_with_bid_cap")
            .unwrap()
            .meta_value(),
        "LOWEST_COST_WITH_BID_CAP"
    );
    assert_eq!(
        BidStrategy::from_str("lowest_cost_with_min_roas")
            .unwrap()
            .meta_value(),
        "LOWEST_COST_WITH_MIN_ROAS"
    );
    assert_eq!(
        BidStrategy::from_str("target_cost").unwrap_err(),
        "unknown_bid_strategy:target_cost"
    );
}

#[test]
fn image_link_creative_contract_is_closed_and_validates_every_input() {
    assert_eq!(
        LinkCallToAction::from_str("learn_more")
            .unwrap()
            .meta_value(),
        "LEARN_MORE"
    );
    assert_eq!(
        LinkCallToAction::from_str("shop_now").unwrap().meta_value(),
        "SHOP_NOW"
    );
    assert_eq!(
        LinkCallToAction::from_str("swipe_up_shop").unwrap_err(),
        "unknown_link_call_to_action:swipe_up_shop"
    );
    assert_eq!(
        AdPreviewFormat::from_str("desktop_feed_standard")
            .unwrap()
            .meta_value(),
        "DESKTOP_FEED_STANDARD"
    );
    assert_eq!(
        AdPreviewFormat::from_str("instagram_standard").unwrap_err(),
        "unknown_ad_preview_format:instagram_standard"
    );
    assert_eq!(
        AdPreviewFormat::from_str("whatsapp_status_media")
            .unwrap()
            .meta_value(),
        "WHATSAPP_STATUS_MEDIA"
    );

    let valid_image = UploadAdImageRequest {
        account: Some("act_123".into()),
        filename: "hero.png".into(),
        bytes: b"image bytes".to_vec(),
    };
    assert!(valid_image.validate().is_ok());
    let invalid_filename = UploadAdImageRequest {
        filename: "private/hero.png".into(),
        ..valid_image.clone()
    };
    assert_eq!(
        invalid_filename.validate().unwrap_err(),
        "invalid_image_filename"
    );
    let empty_image = UploadAdImageRequest {
        bytes: vec![],
        ..valid_image
    };
    assert_eq!(empty_image.validate().unwrap_err(), "image_file_empty");

    let valid_video = UploadAdVideoRequest {
        account: Some("act_123".into()),
        filename: "hero.mp4".into(),
        bytes: b"video bytes".to_vec(),
    };
    assert!(valid_video.validate().is_ok());
    assert_eq!(
        UploadAdVideoRequest {
            filename: "private/hero.mp4".into(),
            ..valid_video.clone()
        }
        .validate()
        .unwrap_err(),
        "invalid_video_filename"
    );
    assert_eq!(
        UploadAdVideoRequest {
            bytes: vec![],
            ..valid_video
        }
        .validate()
        .unwrap_err(),
        "video_file_empty"
    );

    assert!(CreativePreviewRequest {
        creative_id: "789".into(),
        ad_format: AdPreviewFormat::MobileFeedStandard,
    }
    .validate()
    .is_ok());
    assert_eq!(
        CreativePreviewRequest {
            creative_id: "not-an-id".into(),
            ad_format: AdPreviewFormat::DesktopFeedStandard,
        }
        .validate()
        .unwrap_err(),
        "bad_creative_id:not-an-id"
    );
    assert!(AdReviewStatusRequest {
        entity: AdEntity::Ad,
        id: "789".into(),
    }
    .validate()
    .is_ok());
    assert_eq!(
        AdReviewStatusRequest {
            entity: AdEntity::Campaign,
            id: "campaign-789".into(),
        }
        .validate()
        .unwrap_err(),
        "bad_ad_entity_id:campaign-789"
    );
    assert_eq!(
        AdsInventoryKind::from_str("creative").unwrap(),
        AdsInventoryKind::Creative
    );
    assert_eq!(AdsInventoryKind::Adset.graph_edge(), "adsets");
    assert_eq!(
        AdsInventoryKind::from_str("campaigns").unwrap_err(),
        "unknown_ads_inventory_kind:campaigns"
    );
    assert!(AdsInventoryRequest {
        account: Some("act_123".into()),
        kind: AdsInventoryKind::Ad,
    }
    .validate()
    .is_ok());
    assert_eq!(
        AdsInventoryRequest {
            account: Some("nope".into()),
            kind: AdsInventoryKind::Campaign,
        }
        .validate()
        .unwrap_err(),
        "bad_ad_account:nope"
    );
    assert!(AdsInspectRequest {
        kind: AdsInventoryKind::Adset,
        id: "456".into(),
    }
    .validate()
    .is_ok());
    assert_eq!(
        AdsInspectRequest {
            kind: AdsInventoryKind::Campaign,
            id: "campaign-1".into(),
        }
        .validate()
        .unwrap_err(),
        "bad_ads_inspect_id:campaign-1"
    );
    assert!(AdsActivateRequest {
        entity: AdEntity::Adset,
        id: "456".into(),
        confirm_id: "456".into(),
        confirm_daily_budget: Some(500),
        confirm_lifetime_budget: None,
    }
    .validate()
    .is_ok());
    assert_eq!(
        AdsActivateRequest {
            entity: AdEntity::Adset,
            id: "456".into(),
            confirm_id: "999".into(),
            confirm_daily_budget: None,
            confirm_lifetime_budget: None,
        }
        .validate()
        .unwrap_err(),
        "confirm_id_mismatch"
    );
    assert_eq!(AdsConfiguredStatus::Active.meta_value(), "ACTIVE");
    // The raw iframe is intentionally available in-process for a caller
    // to write to a file, but its derived JSON form must never become a
    // surprise terminal/log payload.
    let serialized = serde_json::to_value(CreativePreview {
        site: Site::new("meta_ads"),
        creative_id: "789".into(),
        ad_format: AdPreviewFormat::DesktopFeedStandard,
        body: "<iframe secret-ish-preview-url>".into(),
    })
    .unwrap();
    assert!(serialized.get("body").is_none());
    assert!(!format!(
        "{:?}",
        CreativePreview {
            site: Site::new("meta_ads"),
            creative_id: "789".into(),
            ad_format: AdPreviewFormat::DesktopFeedStandard,
            body: "<iframe secret-ish-preview-url>".into(),
        }
    )
    .contains("secret-ish-preview-url"));

    let valid_creative = CreateLinkAdCreativeRequest {
        account: Some("123".into()),
        creative: LinkAdCreative {
            name: "Hero link".into(),
            page_id: "456".into(),
            image_hash: "hash-1".into(),
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
    };
    assert!(valid_creative.validate().is_ok());
    let insecure_url = CreateLinkAdCreativeRequest {
        creative: LinkAdCreative {
            destination_url: "http://example.com".into(),
            ..valid_creative.creative.clone()
        },
        ..valid_creative.clone()
    };
    assert_eq!(
        insecure_url.validate().unwrap_err(),
        "destination_url_must_be_https"
    );
    let missing_host = CreateLinkAdCreativeRequest {
        creative: LinkAdCreative {
            destination_url: "https:///offer".into(),
            ..valid_creative.creative.clone()
        },
        ..valid_creative.clone()
    };
    assert_eq!(
        missing_host.validate().unwrap_err(),
        "destination_url_must_be_https"
    );
    let bad_page = CreateLinkAdCreativeRequest {
        creative: LinkAdCreative {
            page_id: "page-456".into(),
            ..valid_creative.creative.clone()
        },
        ..valid_creative.clone()
    };
    assert_eq!(bad_page.validate().unwrap_err(), "bad_page_id:page-456");

    let missing_geo = CreateLinkAdCreativeRequest {
        creative: LinkAdCreative {
            call_to_action: LinkCallToAction::GetDirections,
            ..valid_creative.creative.clone()
        },
        ..valid_creative.clone()
    };
    assert_eq!(missing_geo.validate().unwrap_err(), "missing_geo_link");
    let directions = CreateLinkAdCreativeRequest {
        creative: LinkAdCreative {
            call_to_action: LinkCallToAction::GetDirections,
            geo_link: Some("fbgeo://37.48,-122.15,\"1601 Willow Rd\"".into()),
            ..valid_creative.creative.clone()
        },
        ..valid_creative.clone()
    };
    assert!(directions.validate().is_ok());
    assert_eq!(
        link_cta_value_json(&directions.creative)["link"].as_str(),
        Some("fbgeo://37.48,-122.15,\"1601 Willow Rd\"")
    );

    let missing_app = CreateLinkAdCreativeRequest {
        creative: LinkAdCreative {
            call_to_action: LinkCallToAction::InstallApp,
            ..valid_creative.creative.clone()
        },
        ..valid_creative.clone()
    };
    assert_eq!(
        missing_app.validate().unwrap_err(),
        "missing_application_id"
    );
    let install = CreateLinkAdCreativeRequest {
        creative: LinkAdCreative {
            call_to_action: LinkCallToAction::InstallApp,
            application_id: Some("1".into()),
            app_link: Some("https://apps.apple.com/app/id1".into()),
            ..valid_creative.creative.clone()
        },
        ..valid_creative.clone()
    };
    assert!(install.validate().is_ok());
    let leftover_geo = CreateLinkAdCreativeRequest {
        creative: LinkAdCreative {
            geo_link: Some("fbgeo://1,2".into()),
            ..valid_creative.creative.clone()
        },
        ..valid_creative.clone()
    };
    assert_eq!(
        leftover_geo.validate().unwrap_err(),
        "geo_link_without_get_directions"
    );

    let video = CreateVideoAdCreativeRequest {
        account: Some("123".into()),
        creative: VideoAdCreative {
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
    };
    assert!(video.validate().is_ok());
    assert_eq!(
        CreateVideoAdCreativeRequest {
            creative: VideoAdCreative {
                video_id: "vid".into(),
                ..video.creative.clone()
            },
            ..video
        }
        .validate()
        .unwrap_err(),
        "bad_video_id:vid"
    );
    assert_eq!(
        link_cta_value_json(&valid_creative.creative),
        serde_json::json!({ "link": "https://example.com/offer" })
    );
    let like_page = LinkAdCreative {
        call_to_action: LinkCallToAction::LikePage,
        ..valid_creative.creative.clone()
    };
    assert_eq!(
        link_cta_value_json(&like_page),
        serde_json::json!({ "page": "456" })
    );
    let wa = LinkAdCreative {
        call_to_action: LinkCallToAction::WhatsAppMessage,
        ..valid_creative.creative.clone()
    };
    assert_eq!(
        link_cta_value_json(&wa),
        serde_json::json!({ "app_destination": "whatsapp" })
    );
}

#[test]
fn extra_creative_formats_and_whatsapp_status_validate() {
    let card = |n: &str| CarouselCard {
        image_hash: format!("h{n}"),
        link: format!("https://example.com/{n}"),
        name: format!("Card {n}"),
    };
    let too_few = CreateAdCreativeRequest {
        account: None,
        kind: AdCreativeKind::Carousel(CarouselAdCreative {
            name: "C".into(),
            page_id: "1".into(),
            message: "m".into(),
            call_to_action: LinkCallToAction::ShopNow,
            cards: vec![card("1")],
            instagram_user_id: None,
            advantage_plus: false,
            whatsapp_identity: None,
        }),
    };
    assert_eq!(
        too_few.validate().unwrap_err(),
        "carousel_cards_out_of_range"
    );
    let ok = CreateAdCreativeRequest {
        account: None,
        kind: AdCreativeKind::Carousel(CarouselAdCreative {
            cards: vec![card("1"), card("2")],
            ..match too_few.kind {
                AdCreativeKind::Carousel(c) => c,
                _ => unreachable!(),
            }
        }),
    };
    assert!(ok.validate().is_ok());

    let mut targeting = sample_targeting();
    targeting.whatsapp_positions = vec![WhatsAppPosition::Status];
    assert_eq!(
        targeting.validate().unwrap_err(),
        "whatsapp_positions_without_whatsapp_platform"
    );
    targeting.publisher_platforms = vec![PublisherPlatform::Whatsapp];
    assert_eq!(
        targeting.validate().unwrap_err(),
        "whatsapp_status_requires_instagram_story"
    );
    targeting.publisher_platforms = vec![PublisherPlatform::Whatsapp, PublisherPlatform::Instagram];
    targeting.instagram_positions = vec![InstagramPosition::Story];
    assert_eq!(
        targeting.validate().unwrap_err(),
        "whatsapp_status_requires_user_age_unknown"
    );
    targeting.user_age_unknown = Some(false);
    assert!(targeting.validate().is_ok());

    let ident = WhatsAppStatusIdentity {
        identity_id: "9".into(),
        phone_number: None,
    };
    let status_carousel = CreateAdCreativeRequest {
        account: None,
        kind: AdCreativeKind::Carousel(CarouselAdCreative {
            whatsapp_identity: Some(ident.clone()),
            ..match ok.kind {
                AdCreativeKind::Carousel(c) => c,
                _ => unreachable!(),
            }
        }),
    };
    assert_eq!(
        status_carousel.validate().unwrap_err(),
        "whatsapp_status_unsupported_creative_format"
    );
}

#[test]
fn paused_create_validation_rejects_invalid_shapes() {
    let bad_budget = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            name: "Test".into(),
            campaign_id: "12".into(),
            daily_budget: Some(0),
            lifetime_budget: None,
            bid_strategy: BidStrategy::LowestCostWithoutCap,
            bid_amount: None,
            roas_average_floor: None,
            billing_event: BillingEvent::Impressions,
            optimization_goal: OptimizationGoal::Reach,
            targeting: AdTargeting {
                geo_locations: GeoLocations {
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
    };
    assert_eq!(
        bad_budget.validate().unwrap_err(),
        "budget_must_be_positive"
    );

    let bad_targeting = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            name: "Test".into(),
            campaign_id: "12".into(),
            daily_budget: Some(100),
            lifetime_budget: None,
            bid_strategy: BidStrategy::LowestCostWithoutCap,
            bid_amount: None,
            roas_average_floor: None,
            billing_event: BillingEvent::Impressions,
            optimization_goal: OptimizationGoal::Reach,
            targeting: AdTargeting {
                geo_locations: GeoLocations { countries: vec![] },
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
    };
    assert_eq!(
        bad_targeting.validate().unwrap_err(),
        "targeting_missing_country"
    );
    assert_eq!(
        AdTargeting {
            geo_locations: GeoLocations {
                countries: vec!["my".into()],
            },
            age_min: None,
            age_max: None,
            publisher_platforms: vec![],
            facebook_positions: vec![],
            instagram_positions: vec![],
            whatsapp_positions: vec![],
            user_age_unknown: None,
        }
        .validate()
        .unwrap_err(),
        "bad_country_code:my"
    );
}

fn sample_targeting() -> AdTargeting {
    AdTargeting {
        geo_locations: GeoLocations {
            countries: vec!["MY".into()],
        },
        age_min: None,
        age_max: None,
        publisher_platforms: vec![],
        facebook_positions: vec![],
        instagram_positions: vec![],
        whatsapp_positions: vec![],
        user_age_unknown: None,
    }
}

fn sample_adset() -> PausedAdset {
    PausedAdset {
        name: "Test".into(),
        campaign_id: "12".into(),
        daily_budget: Some(100),
        lifetime_budget: None,
        bid_strategy: BidStrategy::LowestCostWithoutCap,
        bid_amount: None,
        roas_average_floor: None,
        billing_event: BillingEvent::Impressions,
        optimization_goal: OptimizationGoal::Reach,
        targeting: sample_targeting(),
        start_time: None,
        end_time: None,
        promoted_object: None,
    }
}

fn sample_campaign() -> PausedCampaign {
    PausedCampaign {
        name: "Test".into(),
        objective: CampaignObjective::Awareness,
        special_ad_categories: vec![],
        daily_budget: None,
        lifetime_budget: None,
        is_adset_budget_sharing_enabled: false,
    }
}

#[test]
fn budget_xor_and_campaign_sharing_are_local() {
    let both = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Campaign(PausedCampaign {
            daily_budget: Some(100),
            lifetime_budget: Some(200),
            ..sample_campaign()
        }),
    };
    assert_eq!(
        both.validate().unwrap_err(),
        "daily_and_lifetime_budget_mutually_exclusive"
    );

    let sharing = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Campaign(PausedCampaign {
            is_adset_budget_sharing_enabled: true,
            ..sample_campaign()
        }),
    };
    assert!(sharing.validate().is_ok());

    let cbo_and_sharing = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Campaign(PausedCampaign {
            daily_budget: Some(5000),
            is_adset_budget_sharing_enabled: true,
            ..sample_campaign()
        }),
    };
    assert_eq!(
        cbo_and_sharing.validate().unwrap_err(),
        "budget_sharing_incompatible_with_campaign_budget"
    );

    let cbo = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Campaign(PausedCampaign {
            daily_budget: Some(5000),
            ..sample_campaign()
        }),
    };
    assert!(cbo.validate().is_ok());

    let adset_both = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            daily_budget: Some(100),
            lifetime_budget: Some(200),
            ..sample_adset()
        }),
    };
    assert_eq!(
        adset_both.validate().unwrap_err(),
        "daily_and_lifetime_budget_mutually_exclusive"
    );

    // CBO child: the campaign holds the budget, so the ad set may omit
    // both fields. Meta still requires one level to have a budget.
    let cbo_child = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            daily_budget: None,
            lifetime_budget: None,
            ..sample_adset()
        }),
    };
    assert!(cbo_child.validate().is_ok());

    let lifetime = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            daily_budget: None,
            lifetime_budget: Some(20_000),
            ..sample_adset()
        }),
    };
    assert_eq!(
        lifetime.validate().unwrap_err(),
        "lifetime_budget_requires_end_time"
    );
    let lifetime_scheduled = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            daily_budget: None,
            lifetime_budget: Some(20_000),
            start_time: Some("2026-11-11T14:26:09-08:00".into()),
            end_time: Some("2026-11-21T14:26:09-08:00".into()),
            ..sample_adset()
        }),
    };
    assert!(lifetime_scheduled.validate().is_ok());
}

#[test]
fn adset_schedule_is_rfc3339_and_ordered() {
    assert!(parse_adset_datetime("start_time", "2026-11-11T14:25:17-08:00").is_ok());
    // Meta curl examples omit the colon in the offset.
    assert!(parse_adset_datetime("start_time", "2026-11-11T14:25:17-0800").is_ok());
    assert!(parse_adset_datetime("start_time", "2026-11-11 14:25:17-08:00").is_ok());
    assert_eq!(
        parse_adset_datetime("start_time", "next tuesday").unwrap_err(),
        "bad_start_time:next tuesday"
    );

    let inverted = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            start_time: Some("2026-11-21T14:26:09-08:00".into()),
            end_time: Some("2026-11-11T14:26:09-08:00".into()),
            ..sample_adset()
        }),
    };
    assert_eq!(
        inverted.validate().unwrap_err(),
        "end_time_not_after_start_time"
    );
}

#[test]
fn bid_strategies_require_their_constraint_fields() {
    let missing_cap = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            bid_strategy: BidStrategy::CostCap,
            ..sample_adset()
        }),
    };
    assert_eq!(missing_cap.validate().unwrap_err(), "missing_bid_amount");

    let cap = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            bid_strategy: BidStrategy::CostCap,
            bid_amount: Some(200),
            ..sample_adset()
        }),
    };
    assert!(cap.validate().is_ok());

    let bid_cap = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            bid_strategy: BidStrategy::LowestCostWithBidCap,
            bid_amount: Some(300),
            ..sample_adset()
        }),
    };
    assert!(bid_cap.validate().is_ok());

    let leftover_amount = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            bid_amount: Some(200),
            ..sample_adset()
        }),
    };
    assert_eq!(
        leftover_amount.validate().unwrap_err(),
        "bid_amount_without_cap_strategy"
    );

    let missing_roas = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            bid_strategy: BidStrategy::LowestCostWithMinRoas,
            ..sample_adset()
        }),
    };
    assert_eq!(
        missing_roas.validate().unwrap_err(),
        "missing_roas_average_floor"
    );

    let min_roas_wrong_goal = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            bid_strategy: BidStrategy::LowestCostWithMinRoas,
            roas_average_floor: Some(15_000),
            ..sample_adset()
        }),
    };
    assert_eq!(
        min_roas_wrong_goal.validate().unwrap_err(),
        "min_roas_requires_value_goal"
    );

    let min_roas = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            bid_strategy: BidStrategy::LowestCostWithMinRoas,
            roas_average_floor: Some(15_000),
            optimization_goal: OptimizationGoal::Value,
            promoted_object: Some(PromotedObject::Pixel {
                pixel_id: "789".into(),
                custom_event_type: CustomEventType::Purchase,
            }),
            ..sample_adset()
        }),
    };
    assert!(min_roas.validate().is_ok());

    let floor_too_small = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            bid_strategy: BidStrategy::LowestCostWithMinRoas,
            roas_average_floor: Some(50),
            optimization_goal: OptimizationGoal::Value,
            ..sample_adset()
        }),
    };
    assert_eq!(
        floor_too_small.validate().unwrap_err(),
        "roas_average_floor_out_of_range"
    );

    let both = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            bid_strategy: BidStrategy::LowestCostWithMinRoas,
            bid_amount: Some(200),
            roas_average_floor: Some(10_000),
            ..sample_adset()
        }),
    };
    assert_eq!(
        both.validate().unwrap_err(),
        "bid_amount_without_cap_strategy"
    );
}

#[test]
fn promoted_object_is_required_for_conversion_goals() {
    let missing = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            optimization_goal: OptimizationGoal::OffsiteConversions,
            billing_event: BillingEvent::Impressions,
            ..sample_adset()
        }),
    };
    assert_eq!(
        missing.validate().unwrap_err(),
        "promoted_object_required:offsite_conversions"
    );

    let wrong_kind = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            optimization_goal: OptimizationGoal::OffsiteConversions,
            promoted_object: Some(PromotedObject::Page {
                page_id: "456".into(),
            }),
            ..sample_adset()
        }),
    };
    assert_eq!(
        wrong_kind.validate().unwrap_err(),
        "promoted_object_kind_mismatch:offsite_conversions:page"
    );

    let pixel = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            optimization_goal: OptimizationGoal::OffsiteConversions,
            promoted_object: Some(PromotedObject::Pixel {
                pixel_id: "789".into(),
                custom_event_type: CustomEventType::Purchase,
            }),
            ..sample_adset()
        }),
    };
    assert!(pixel.validate().is_ok());
    assert_eq!(
        PromotedObject::Pixel {
            pixel_id: "789".into(),
            custom_event_type: CustomEventType::Purchase,
        }
        .meta_json(),
        serde_json::json!({"pixel_id":"789","custom_event_type":"PURCHASE"})
    );

    let bad_id = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            optimization_goal: OptimizationGoal::PageLikes,
            promoted_object: Some(PromotedObject::Page {
                page_id: "page-x".into(),
            }),
            ..sample_adset()
        }),
    };
    assert_eq!(bad_id.validate().unwrap_err(), "bad_page_id:page-x");

    let app = CreatePausedAdRequest {
        account: Some("123".into()),
        create: PausedAdCreate::Adset(PausedAdset {
            optimization_goal: OptimizationGoal::AppInstalls,
            promoted_object: Some(PromotedObject::App {
                application_id: "1".into(),
                object_store_url: "https://apps.apple.com/app/id1".into(),
            }),
            ..sample_adset()
        }),
    };
    assert!(app.validate().is_ok());
}
