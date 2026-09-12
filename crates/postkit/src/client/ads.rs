//! Ads routing: paused creates, creatives, inventory, inspect, lifecycle.
use super::ads_inner::{
    activate_ad_inner, confirm_budget_echo, confirmed_status_inner, merge_string_list,
    pause_ad_inner, post_then_inspect,
};
use super::{empty_app, Client, REVIEW_POLL_INTERVAL};
#[cfg(feature = "meta-ads")]
use crate::ads::AdReviewWait;
use crate::ads::{
    AdReviewStatus, AdReviewStatusRequest, AdsActivateRequest, AdsArchiveRequest,
    AdsBidUpdateRequest, AdsBudgetUpdateRequest, AdsConfiguredStatus, AdsCreativeSwapRequest,
    AdsDeleteRequest, AdsDuplicateReply, AdsDuplicateRequest, AdsEditOutcome, AdsInspectReply,
    AdsInspectRequest, AdsInventoryKind, AdsInventoryReply, AdsInventoryRequest,
    AdsLifecycleOutcome, AdsLifetimeBudgetUpdateRequest, AdsPauseRequest,
    AdsPlacementUpdateRequest, AdsScheduleUpdateRequest, AdsTargetingDiff,
    AdsTargetingUpdateRequest, AdsTokenInspection, CreateLinkAdCreativeRequest,
    CreatePausedAdRequest, CreatedAd, CreatedAdCreative, CreativePreview, CreativePreviewRequest,
    MarketingApiAccessTier, UploadAdImageRequest, UploadedAdImage, ARCHIVE_RECONCILE_GUIDANCE,
    DELETE_RECONCILE_GUIDANCE,
};
use crate::error::Error;
use crate::policy::AdsAction;
use crate::types::{AccountKey, Capability, Deadline, WhoAmI};

impl Client {
    pub async fn create_paused_ad(
        &self,
        key: &AccountKey,
        request: CreatePausedAdRequest,
        deadline: Deadline,
    ) -> Result<CreatedAd, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::for_paused_create(&request.create))?;
        self.require_capability(&key.site, Capability::CreatePausedAds)?;
        let ads = self.ads_manager(&key.site, Capability::CreatePausedAds)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move { ads.create_paused_ad(&app, &creds, &request, deadline).await })
        })
        .await
    }

    /// Upload an image only after local validation and policy approval. An
    /// upload has no delivery status, but it is still a remote asset write and
    /// must not reach credentials or HTTP when a stricter policy refuses it.
    pub async fn upload_ad_image(
        &self,
        key: &AccountKey,
        request: UploadAdImageRequest,
        deadline: Deadline,
    ) -> Result<UploadedAdImage, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UploadAdImage)?;
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move { ads.upload_ad_image(&app, &creds, &request, deadline).await })
        })
        .await
    }

    /// Upload a video after local validation and policy approval. Encoding
    /// is a later status poll; this write only stores the asset.
    pub async fn upload_ad_video(
        &self,
        key: &AccountKey,
        request: crate::ads::UploadAdVideoRequest,
        deadline: Deadline,
    ) -> Result<crate::ads::UploadedAdVideo, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UploadAdVideo)?;
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move { ads.upload_ad_video(&app, &creds, &request, deadline).await })
        })
        .await
    }

    /// One GET of Meta `status.video_status`. Encoding is not delivery.
    pub async fn ad_video_status(
        &self,
        key: &AccountKey,
        request: crate::ads::AdVideoStatusRequest,
        deadline: Deadline,
    ) -> Result<crate::ads::AdVideoStatus, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move { ads.ad_video_status(&app, &creds, &request, deadline).await })
        })
        .await
    }

    /// Poll every 2s until ready/error or the deadline. Pending is success.
    #[cfg(feature = "meta-ads")]
    pub async fn wait_for_ad_video(
        &self,
        key: &AccountKey,
        request: crate::ads::AdVideoStatusRequest,
        deadline: Deadline,
    ) -> Result<crate::ads::AdVideoWait, Error> {
        self.wait_for_ad_video_with_interval(key, request, deadline, REVIEW_POLL_INTERVAL)
            .await
    }

    #[cfg(feature = "meta-ads")]
    pub(crate) async fn wait_for_ad_video_with_interval(
        &self,
        key: &AccountKey,
        request: crate::ads::AdVideoStatusRequest,
        deadline: Deadline,
        poll_interval: std::time::Duration,
    ) -> Result<crate::ads::AdVideoWait, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        let (publisher, app, mut creds) = self.prepare_creds(key, deadline, true).await?;
        let mut retried_expired_token = false;
        loop {
            let status = match ads.ad_video_status(&app, &creds, &request, deadline).await {
                Err(e) if !retried_expired_token => {
                    creds = self
                        .recover_expired(&*publisher, &app, key, creds, deadline, e)
                        .await?;
                    retried_expired_token = true;
                    continue;
                }
                other => other,
            }?;
            match status.video_status {
                crate::ads::AdVideoStatusKind::Ready => {
                    return Ok(crate::ads::AdVideoWait::Ready(status));
                }
                crate::ads::AdVideoStatusKind::Error => {
                    return Ok(crate::ads::AdVideoWait::Error(status));
                }
                _ => {}
            }
            let remaining = deadline.remaining();
            if remaining.is_zero() {
                return Ok(crate::ads::AdVideoWait::Pending(status));
            }
            let delay = if poll_interval.is_zero() {
                remaining
            } else {
                poll_interval.min(remaining)
            };
            tokio::time::sleep(delay).await;
        }
    }

    /// Create a Page-backed image-link creative behind the same validation,
    /// policy, capability, and refresh ordering as every other Tier B write.
    /// The returned creative cannot deliver until a separate paused ad uses it.
    pub async fn create_link_ad_creative(
        &self,
        key: &AccountKey,
        request: CreateLinkAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::CreateLinkAdCreative)?;
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                ads.create_link_ad_creative(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
    }

    /// Page-backed video creative. The video must already exist; this does
    /// not wait for encoding.
    pub async fn create_video_ad_creative(
        &self,
        key: &AccountKey,
        request: crate::ads::CreateVideoAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::CreateLinkAdCreative)?;
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                ads.create_video_ad_creative(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
    }

    pub async fn create_ad_creative(
        &self,
        key: &AccountKey,
        request: crate::ads::CreateAdCreativeRequest,
        deadline: Deadline,
    ) -> Result<CreatedAdCreative, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::CreateLinkAdCreative)?;
        self.require_capability(&key.site, Capability::CreateAdCreative)?;
        let ads = self.ads_manager(&key.site, Capability::CreateAdCreative)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                ads.create_ad_creative(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
    }

    /// Read the platform's rendering of an existing creative. Unlike the
    /// creative/upload methods above this has no policy decision: it is a
    /// GET-only review operation and cannot affect delivery, budget, billing,
    /// or the creative itself.
    pub async fn preview_ad_creative(
        &self,
        key: &AccountKey,
        request: CreativePreviewRequest,
        deadline: Deadline,
    ) -> Result<CreativePreview, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadAdPreviews)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdPreviews)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                ads.preview_ad_creative(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
    }

    /// List one kind of advertising object in a selected account. GET-only
    /// and outside `AdsPolicy`: paging through paused drafts cannot activate
    /// them or change a budget.
    pub async fn list_ads_inventory(
        &self,
        key: &AccountKey,
        request: AdsInventoryRequest,
        deadline: Deadline,
    ) -> Result<AdsInventoryReply, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadAdsInventory)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdsInventory)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                ads.list_ads_inventory(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
    }

    /// Read budget, bid, targeting, Page, and destination on one known
    /// object. GET-only and outside `AdsPolicy`: this is the pre-activate
    /// confirmation surface, not an edit.
    pub async fn inspect_ads_object(
        &self,
        key: &AccountKey,
        request: AdsInspectRequest,
        deadline: Deadline,
    ) -> Result<AdsInspectReply, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadAdsInventory)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdsInventory)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                ads.inspect_ads_object(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
    }

    /// Confirmed PAUSED → ACTIVE. Policy, confirmation, and review preflight
    /// all run before the vault. A network/deadline failure after the POST
    /// leaves is `reconciliation_required`, never a second activate.
    pub async fn activate_ad(
        &self,
        key: &AccountKey,
        request: AdsActivateRequest,
        deadline: Deadline,
    ) -> Result<AdsLifecycleOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy.authorize(&key.site, AdsAction::Activate)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(
                async move { activate_ad_inner(&*ads, &app, &creds, &request, deadline).await },
            )
        })
        .await
    }

    /// Emergency `ACTIVE` → `PAUSED`. Allowed by the default policy because
    /// it cannot start spend. Already-paused is idempotent; archived/deleted
    /// objects refuse rather than guessing.
    pub async fn pause_ad(
        &self,
        key: &AccountKey,
        request: AdsPauseRequest,
        deadline: Deadline,
    ) -> Result<AdsLifecycleOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy.authorize(&key.site, AdsAction::Pause)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move { pause_ad_inner(&*ads, &app, &creds, &request, deadline).await })
        })
        .await
    }

    /// Archive a known object. Default policy denies; `--confirm-id` must
    /// match. Deleted objects cannot be archived.
    pub async fn archive_ad(
        &self,
        key: &AccountKey,
        request: AdsArchiveRequest,
        deadline: Deadline,
    ) -> Result<AdsLifecycleOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy.authorize(&key.site, AdsAction::Archive)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                confirmed_status_inner(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    AdsConfiguredStatus::Archived,
                    &["DELETED"],
                    ARCHIVE_RECONCILE_GUIDANCE,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    /// Delete a known object. Irreversible to live. Default policy denies.
    pub async fn delete_ad(
        &self,
        key: &AccountKey,
        request: AdsDeleteRequest,
        deadline: Deadline,
    ) -> Result<AdsLifecycleOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy.authorize(&key.site, AdsAction::Delete)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                confirmed_status_inner(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    AdsConfiguredStatus::Deleted,
                    &[],
                    DELETE_RECONCILE_GUIDANCE,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    /// Copy an object as PAUSED. Default policy denies. Never inherits ACTIVE.
    pub async fn duplicate_ad(
        &self,
        key: &AccountKey,
        request: AdsDuplicateRequest,
        deadline: Deadline,
    ) -> Result<AdsDuplicateReply, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy.authorize(&key.site, AdsAction::Duplicate)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                let inspect = ads
                    .inspect_ads_object(
                        &app,
                        &creds,
                        &AdsInspectRequest {
                            kind: AdsInventoryKind::from_entity(request.entity),
                            id: request.id.clone(),
                        },
                        deadline,
                    )
                    .await?;
                confirm_budget_echo(
                    &inspect,
                    request.confirm_daily_budget,
                    request.confirm_lifetime_budget,
                )
                .map_err(|reason| Error::InvalidQuery {
                    site: inspect.site.clone(),
                    reason,
                })?;
                ads.duplicate_ad_object(&app, &creds, &request, deadline)
                    .await
            })
        })
        .await
    }

    pub async fn update_ad_budget(
        &self,
        key: &AccountKey,
        request: AdsBudgetUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UpdateBudget)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                let inspect = ads
                    .inspect_ads_object(
                        &app,
                        &creds,
                        &AdsInspectRequest {
                            kind: AdsInventoryKind::from_entity(request.entity),
                            id: request.id.clone(),
                        },
                        deadline,
                    )
                    .await?;
                if inspect.daily_budget.is_none() {
                    return Err(Error::InvalidQuery {
                        site: inspect.site.clone(),
                        reason: "budget_not_on_object".into(),
                    });
                }
                let current = inspect
                    .daily_budget
                    .as_deref()
                    .and_then(|raw| raw.parse::<u64>().ok());
                if current != Some(request.current_daily_budget) {
                    return Err(Error::InvalidQuery {
                        site: inspect.site.clone(),
                        reason: "current_daily_budget_mismatch".into(),
                    });
                }
                post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    &[("daily_budget".into(), request.new_daily_budget.to_string())],
                    None,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    /// Same confirmation and max-change guard as daily, posting
    /// `lifetime_budget`. Campaign/ad set only.
    pub async fn update_ad_lifetime_budget(
        &self,
        key: &AccountKey,
        request: AdsLifetimeBudgetUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UpdateBudget)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                let inspect = ads
                    .inspect_ads_object(
                        &app,
                        &creds,
                        &AdsInspectRequest {
                            kind: AdsInventoryKind::from_entity(request.entity),
                            id: request.id.clone(),
                        },
                        deadline,
                    )
                    .await?;
                if inspect.lifetime_budget.is_none() {
                    return Err(Error::InvalidQuery {
                        site: inspect.site.clone(),
                        reason: "budget_not_on_object".into(),
                    });
                }
                let current = inspect
                    .lifetime_budget
                    .as_deref()
                    .and_then(|raw| raw.parse::<u64>().ok());
                if current != Some(request.current_lifetime_budget) {
                    return Err(Error::InvalidQuery {
                        site: inspect.site.clone(),
                        reason: "current_lifetime_budget_mismatch".into(),
                    });
                }
                post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    &[(
                        "lifetime_budget".into(),
                        request.new_lifetime_budget.to_string(),
                    )],
                    None,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    pub async fn update_ad_bid(
        &self,
        key: &AccountKey,
        request: AdsBidUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy.authorize(&key.site, AdsAction::UpdateBid)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                // Plan: GET current strategy before POST so a typo cannot
                // mutate an object we have not read.
                ads.inspect_ads_object(
                    &app,
                    &creds,
                    &AdsInspectRequest {
                        kind: AdsInventoryKind::from_entity(request.entity),
                        id: request.id.clone(),
                    },
                    deadline,
                )
                .await?;
                let mut fields = vec![(
                    "bid_strategy".into(),
                    request.bid_strategy.meta_value().into(),
                )];
                if let Some(amount) = request.bid_amount {
                    fields.push(("bid_amount".into(), amount.to_string()));
                }
                if let Some(floor) = request.roas_average_floor {
                    fields.push((
                        "bid_constraints".into(),
                        serde_json::json!({ "roas_average_floor": floor }).to_string(),
                    ));
                }
                post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    &fields,
                    None,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    pub async fn update_ad_schedule(
        &self,
        key: &AccountKey,
        request: AdsScheduleUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UpdateSchedule)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                let mut fields = Vec::new();
                if let Some(start) = &request.start_time {
                    fields.push(("start_time".into(), start.clone()));
                }
                if let Some(end) = &request.end_time {
                    fields.push(("end_time".into(), end.clone()));
                }
                post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    &fields,
                    None,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    pub async fn update_ad_placement(
        &self,
        key: &AccountKey,
        request: AdsPlacementUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UpdatePlacement)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                let mut targeting = ads
                    .read_ad_targeting_json(&app, &creds, &request.id, deadline)
                    .await?;
                if !targeting.is_object() {
                    targeting = serde_json::json!({});
                }
                merge_string_list(
                    &mut targeting,
                    "publisher_platforms",
                    request
                        .publisher_platforms
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                merge_string_list(
                    &mut targeting,
                    "facebook_positions",
                    request
                        .facebook_positions
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                merge_string_list(
                    &mut targeting,
                    "instagram_positions",
                    request
                        .instagram_positions
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                merge_string_list(
                    &mut targeting,
                    "whatsapp_positions",
                    request
                        .whatsapp_positions
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    &[("targeting".into(), targeting.to_string())],
                    None,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    pub async fn update_ad_targeting(
        &self,
        key: &AccountKey,
        request: AdsTargetingUpdateRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::UpdateTargeting)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                let inspect = ads
                    .inspect_ads_object(
                        &app,
                        &creds,
                        &AdsInspectRequest {
                            kind: AdsInventoryKind::Adset,
                            id: request.id.clone(),
                        },
                        deadline,
                    )
                    .await?;
                let campaign_id =
                    inspect
                        .campaign_id
                        .as_deref()
                        .ok_or_else(|| Error::InvalidQuery {
                            site: inspect.site.clone(),
                            reason: "missing_campaign_id".into(),
                        })?;
                let categories = ads
                    .read_special_ad_categories(&app, &creds, campaign_id, deadline)
                    .await?;
                if !categories.is_empty() {
                    return Err(Error::InvalidQuery {
                        site: inspect.site.clone(),
                        reason: "special_ad_category_contract".into(),
                    });
                }
                let before = inspect.targeting.clone().unwrap_or_default();
                let mut targeting = ads
                    .read_ad_targeting_json(&app, &creds, &request.id, deadline)
                    .await?;
                if !targeting.is_object() {
                    targeting = serde_json::json!({});
                }
                targeting["geo_locations"] = serde_json::to_value(&request.targeting.geo_locations)
                    .unwrap_or(serde_json::Value::Null);
                if let Some(min) = request.targeting.age_min {
                    targeting["age_min"] = serde_json::json!(min);
                }
                if let Some(max) = request.targeting.age_max {
                    targeting["age_max"] = serde_json::json!(max);
                }
                if let Some(unknown) = request.targeting.user_age_unknown {
                    targeting["user_age_unknown"] = serde_json::json!(unknown);
                }
                merge_string_list(
                    &mut targeting,
                    "publisher_platforms",
                    request
                        .targeting
                        .publisher_platforms
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                merge_string_list(
                    &mut targeting,
                    "facebook_positions",
                    request
                        .targeting
                        .facebook_positions
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                merge_string_list(
                    &mut targeting,
                    "instagram_positions",
                    request
                        .targeting
                        .instagram_positions
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                merge_string_list(
                    &mut targeting,
                    "whatsapp_positions",
                    request
                        .targeting
                        .whatsapp_positions
                        .iter()
                        .map(|p| p.as_str().to_string())
                        .collect(),
                );
                let outcome = post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    request.entity,
                    &request.id,
                    &[("targeting".into(), targeting.to_string())],
                    None,
                    deadline,
                )
                .await?;
                Ok(match outcome {
                    AdsEditOutcome::Applied { inspect, .. } => {
                        let after = inspect.targeting.clone().unwrap_or_default();
                        AdsEditOutcome::Applied {
                            targeting_diff: Some(AdsTargetingDiff { before, after }),
                            inspect,
                        }
                    }
                    other => other,
                })
            })
        })
        .await
    }

    pub async fn swap_ad_creative(
        &self,
        key: &AccountKey,
        request: AdsCreativeSwapRequest,
        deadline: Deadline,
    ) -> Result<AdsEditOutcome, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.ads_policy
            .authorize(&key.site, AdsAction::SwapCreative)?;
        self.require_capability(&key.site, Capability::ManageAdsLifecycle)?;
        let ads = self.ads_manager(&key.site, Capability::ManageAdsLifecycle)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move {
                post_then_inspect(
                    &*ads,
                    &app,
                    &creds,
                    crate::ads::AdEntity::Ad,
                    &request.id,
                    &[(
                        "creative".into(),
                        serde_json::json!({ "creative_id": request.creative_id }).to_string(),
                    )],
                    None,
                    deadline,
                )
                .await
            })
        })
        .await
    }

    /// Read one ad object's configured and effective state once. This is a
    /// GET-only operation, so it bypasses `AdsPolicy`: inspecting a Meta
    /// review cannot activate an object, alter a budget, or affect billing.
    pub async fn ad_review_status(
        &self,
        key: &AccountKey,
        request: AdReviewStatusRequest,
        deadline: Deadline,
    ) -> Result<AdReviewStatus, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadAdReviewStatus)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdReviewStatus)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            let request = request.clone();
            Box::pin(async move { ads.ad_review_status(&app, &creds, &request, deadline).await })
        })
        .await
    }

    /// Explicit System User bootstrap. Verifies the token and refuses a
    /// user OAuth token stored as an unattended secret.
    pub async fn put_ads_system_user_token(
        &self,
        key: &AccountKey,
        token: &str,
        deadline: Deadline,
    ) -> Result<WhoAmI, Error> {
        self.require_capability(&key.site, Capability::ReadAdAccounts)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdAccounts)?;
        let publisher = self.publisher(&key.site)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = ads
            .bootstrap_system_user_token(&app, token, deadline)
            .await?;
        self.vault.put(key, &creds)?;
        publisher.whoami(&app, &creds).await
    }

    /// `GET /debug_token` metadata for the stored credential. Never returns
    /// the token or app secret.
    pub async fn inspect_ads_token(
        &self,
        key: &AccountKey,
        deadline: Deadline,
    ) -> Result<AdsTokenInspection, Error> {
        self.require_capability(&key.site, Capability::ReadAdAccounts)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdAccounts)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            Box::pin(async move { ads.inspect_access_token(&app, &creds, deadline).await })
        })
        .await
    }

    /// Operator-facing Marketing API Access Tier. Header mapping is a hint;
    /// Meta's App Dashboard remains authoritative.
    pub async fn ads_access_tier(
        &self,
        key: &AccountKey,
        deadline: Deadline,
    ) -> Result<MarketingApiAccessTier, Error> {
        self.require_capability(&key.site, Capability::ReadAdAccounts)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdAccounts)?;
        self.with_creds(key, deadline, move |app, creds| {
            let ads = ads.clone();
            Box::pin(async move { ads.marketing_api_access_tier(&app, &creds, deadline).await })
        })
        .await
    }

    /// Poll review state only until `deadline`. The `PendingReview` reply is
    /// successful but explicit: it preserves the final known state and tells
    /// a script to retry later without recasting normal Meta review latency as
    /// a network timeout. This method exists only with the Meta connector,
    /// where Tokio's timer dependency is already part of the feature.
    #[cfg(feature = "meta-ads")]
    pub async fn wait_for_ad_review(
        &self,
        key: &AccountKey,
        request: AdReviewStatusRequest,
        deadline: Deadline,
    ) -> Result<AdReviewWait, Error> {
        self.wait_for_ad_review_with_interval(key, request, deadline, REVIEW_POLL_INTERVAL)
            .await
    }

    /// The interval-bearing helper keeps the production interval conservative
    /// while letting the deterministic client test prove the bounded polling
    /// behavior without sleeping for seconds.
    #[cfg(feature = "meta-ads")]
    pub(crate) async fn wait_for_ad_review_with_interval(
        &self,
        key: &AccountKey,
        request: AdReviewStatusRequest,
        deadline: Deadline,
        poll_interval: std::time::Duration,
    ) -> Result<AdReviewWait, Error> {
        request.validate().map_err(|reason| Error::InvalidQuery {
            site: key.site.clone(),
            reason,
        })?;
        self.require_capability(&key.site, Capability::ReadAdReviewStatus)?;
        let ads = self.ads_manager(&key.site, Capability::ReadAdReviewStatus)?;
        let (publisher, app, mut creds) = self.prepare_creds(key, deadline, true).await?;
        let mut retried_expired_token = false;

        loop {
            let status = match ads.ad_review_status(&app, &creds, &request, deadline).await {
                Err(e) if !retried_expired_token => {
                    // Match every other Client read: an expired token gets
                    // one refresh and one retry, never an unbounded refresh
                    // loop hidden inside a status poller.
                    creds = self
                        .recover_expired(&*publisher, &app, key, creds, deadline, e)
                        .await?;
                    retried_expired_token = true;
                    continue;
                }
                other => other,
            }?;
            if !status.is_pending_review() {
                return Ok(AdReviewWait::Settled(status));
            }

            let remaining = deadline.remaining();
            if remaining.is_zero() {
                return Ok(AdReviewWait::PendingReview(status));
            }
            // A zero supplied interval would otherwise busy-spin in an
            // embedding program. Sleeping the remaining deadline makes it a
            // single bounded observation instead.
            let delay = if poll_interval.is_zero() {
                remaining
            } else {
                poll_interval.min(remaining)
            };
            tokio::time::sleep(delay).await;
        }
    }
}
