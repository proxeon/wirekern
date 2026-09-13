//! `postkit ads` dispatch and spend-shaped policy opt-in.
use super::cmd::{AdsCmd, InsightsJobCmd};
use super::helpers::*;
use crate::app::fail;
use crate::output::{emit_raw, human_line};
use postkit::{AccountKey, Client, Deadline, DraftStep, FileDraftStore, RunPausedDraft, Site};
use std::path::Path;
use std::str::FromStr;

pub(crate) fn apply_lifecycle_policy(client: Client, command: &AdsCmd) -> Client {
    match command {
        AdsCmd::Activate {
            allow_activate: true,
            ..
        } => client.with_ads_policy(std::sync::Arc::new(postkit::AllowAdsActionPolicy::new(
            postkit::AdsAction::Activate,
        ))),
        AdsCmd::Archive {
            allow_archive: true,
            ..
        } => client.with_ads_policy(std::sync::Arc::new(postkit::AllowAdsActionPolicy::new(
            postkit::AdsAction::Archive,
        ))),
        AdsCmd::Delete {
            allow_delete: true, ..
        } => client.with_ads_policy(std::sync::Arc::new(postkit::AllowAdsActionPolicy::new(
            postkit::AdsAction::Delete,
        ))),
        AdsCmd::Duplicate {
            allow_duplicate: true,
            ..
        } => client.with_ads_policy(std::sync::Arc::new(postkit::AllowAdsActionPolicy::new(
            postkit::AdsAction::Duplicate,
        ))),
        AdsCmd::UpdateBudget {
            allow_budget_edit: true,
            ..
        }
        | AdsCmd::UpdateLifetimeBudget {
            allow_budget_edit: true,
            ..
        } => client.with_ads_policy(std::sync::Arc::new(postkit::AllowAdsActionPolicy::new(
            postkit::AdsAction::UpdateBudget,
        ))),
        AdsCmd::UpdateBid {
            allow_bid_edit: true,
            ..
        } => client.with_ads_policy(std::sync::Arc::new(postkit::AllowAdsActionPolicy::new(
            postkit::AdsAction::UpdateBid,
        ))),
        AdsCmd::UpdateSchedule {
            allow_schedule_edit: true,
            ..
        } => client.with_ads_policy(std::sync::Arc::new(postkit::AllowAdsActionPolicy::new(
            postkit::AdsAction::UpdateSchedule,
        ))),
        AdsCmd::UpdatePlacement {
            allow_placement_edit: true,
            ..
        } => client.with_ads_policy(std::sync::Arc::new(postkit::AllowAdsActionPolicy::new(
            postkit::AdsAction::UpdatePlacement,
        ))),
        AdsCmd::UpdateTargeting {
            allow_targeting_edit: true,
            ..
        } => client.with_ads_policy(std::sync::Arc::new(postkit::AllowAdsActionPolicy::new(
            postkit::AdsAction::UpdateTargeting,
        ))),
        AdsCmd::SwapCreative {
            allow_creative_swap: true,
            ..
        } => client.with_ads_policy(std::sync::Arc::new(postkit::AllowAdsActionPolicy::new(
            postkit::AdsAction::SwapCreative,
        ))),
        _ => client,
    }
}

pub(crate) async fn dispatch(
    client: Client,
    home: &Path,
    cmd: AdsCmd,
    json: bool,
    account: String,
    deadline: Deadline,
) -> Result<(), i32> {
    match cmd {
        AdsCmd::InsightsJob(InsightsJobCmd::Status { site, id }) => {
            let key = AccountKey::new(&site, &account);
            match client.insights_job(&key, &id, deadline).await {
                Ok(job) => {
                    if json {
                        emit_raw(&serde_json::to_value(&job).expect("json"));
                    } else {
                        human_line(format!(
                            "{} {} {} {}%",
                            job.site,
                            job.id,
                            format!("{:?}", job.status).to_ascii_lowercase(),
                            job.percent_complete
                        ));
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        AdsCmd::InsightsJob(InsightsJobCmd::Result {
            site,
            id,
            from,
            until,
            level,
            metrics,
            attribution,
            ad_account,
            entity_ids,
            breakdowns,
            report,
        }) => {
            let query = resolve_insights_job_query(
                home,
                &site,
                &id,
                from,
                until,
                &level,
                &metrics,
                attribution,
                InsightsOptions {
                    ad_account,
                    entity_ids,
                    breakdowns,
                    report,
                },
            )
            .map_err(|e| fail(&e, json))?;
            let key = AccountKey::new(&site, &account);
            match client.insights_job_result(&key, &id, query, deadline).await {
                Ok(reply) => emit_insights_reply(&reply, json),
                Err(e) => Err(fail(&e, json)),
            }
        }
        AdsCmd::InsightsJob(InsightsJobCmd::Cancel { site, id, yes }) => {
            if !yes {
                eprintln!("pass --yes to cancel insights job {id}");
                return Err(2);
            }
            let key = AccountKey::new(&site, &account);
            match client.cancel_insights_job(&key, &id, deadline).await {
                Ok(job) => {
                    if json {
                        emit_raw(&serde_json::to_value(&job).expect("json"));
                    } else {
                        human_line(format!("{} {} cancelled", job.site, job.id));
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        AdsCmd::InspectToken { site } => {
            let key = AccountKey::new(&site, &account);
            match client.inspect_ads_token(&key, deadline).await {
                Ok(inspection) => {
                    if json {
                        emit_raw(&serde_json::to_value(&inspection).expect("json"));
                    } else {
                        human_line(format!(
                            "{} {} valid={} type={}",
                            inspection.site,
                            inspection.token_kind.as_str(),
                            inspection.is_valid,
                            inspection.debug_type.as_deref().unwrap_or("-")
                        ));
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        AdsCmd::AccessTier { site } => {
            let key = AccountKey::new(&site, &account);
            match client.ads_access_tier(&key, deadline).await {
                Ok(tier) => {
                    if json {
                        emit_raw(&serde_json::to_value(&tier).expect("json"));
                    } else {
                        human_line(format!(
                            "{} {} {} ({})",
                            tier.site,
                            tier.tier.as_str(),
                            tier.raw.as_deref().unwrap_or("-"),
                            tier.dashboard
                        ));
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        AdsCmd::Accounts { site } => {
            let key = AccountKey::new(&site, &account);
            match client.ad_accounts(&key, deadline).await {
                Ok(reply) => {
                    if json {
                        emit_raw(&serde_json::to_value(&reply).expect("json"));
                    } else {
                        for ad_account in &reply.accounts {
                            human_line(ad_account_line(ad_account));
                        }
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        AdsCmd::List {
            site,
            entity,
            ad_account,
        } => {
            let request = build_ads_inventory_request(&site, &entity, ad_account)
                .map_err(|e| fail(&e, json))?;
            one_ads_inventory(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::Inspect { site, entity, id } => {
            let request =
                build_ads_inspect_request(&site, &entity, &id).map_err(|e| fail(&e, json))?;
            one_ads_inspect(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::Activate {
            site,
            entity,
            id,
            confirm_id,
            allow_activate: _,
            confirm_daily_budget,
            confirm_lifetime_budget,
            state,
        } => {
            let request = build_ads_activate_request(
                &site,
                &entity,
                &id,
                &confirm_id,
                confirm_daily_budget,
                confirm_lifetime_budget,
            )
            .map_err(|e| fail(&e, json))?;
            one_ads_activate(
                &client,
                &AccountKey::new(&site, &account),
                request,
                &state,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::Pause { site, entity, id } => {
            let request =
                build_ads_pause_request(&site, &entity, &id).map_err(|e| fail(&e, json))?;
            one_ads_pause(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::Archive {
            site,
            entity,
            id,
            confirm_id,
            allow_archive: _,
        } => {
            let request = build_ads_archive_request(&site, &entity, &id, &confirm_id)
                .map_err(|e| fail(&e, json))?;
            one_ads_archive(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::Delete {
            site,
            entity,
            id,
            confirm_id,
            allow_delete: _,
            confirm_delete,
        } => {
            let request =
                build_ads_delete_request(&site, &entity, &id, &confirm_id, confirm_delete)
                    .map_err(|e| fail(&e, json))?;
            one_ads_delete(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::Duplicate {
            site,
            entity,
            id,
            confirm_id,
            allow_duplicate: _,
            confirm_daily_budget,
            confirm_lifetime_budget,
        } => {
            let request = build_ads_duplicate_request(
                &site,
                &entity,
                &id,
                &confirm_id,
                confirm_daily_budget,
                confirm_lifetime_budget,
            )
            .map_err(|e| fail(&e, json))?;
            one_ads_duplicate(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::UpdateBudget {
            site,
            entity,
            id,
            confirm_id,
            allow_budget_edit: _,
            current_daily_budget,
            new_daily_budget,
            max_change_ratio,
        } => {
            let request = build_ads_budget_update_request(
                &site,
                &entity,
                &id,
                &confirm_id,
                current_daily_budget,
                new_daily_budget,
                max_change_ratio,
            )
            .map_err(|e| fail(&e, json))?;
            one_ads_budget_update(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::UpdateLifetimeBudget {
            site,
            entity,
            id,
            confirm_id,
            allow_budget_edit: _,
            current_lifetime_budget,
            new_lifetime_budget,
            max_change_ratio,
        } => {
            let request = build_ads_lifetime_budget_update_request(
                &site,
                &entity,
                &id,
                &confirm_id,
                current_lifetime_budget,
                new_lifetime_budget,
                max_change_ratio,
            )
            .map_err(|e| fail(&e, json))?;
            one_ads_lifetime_budget_update(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::UpdateBid {
            site,
            entity,
            id,
            confirm_id,
            allow_bid_edit: _,
            bid_strategy,
            bid_amount,
            roas_average_floor,
        } => {
            let request = build_ads_bid_update_request(
                &site,
                &entity,
                &id,
                &confirm_id,
                &bid_strategy,
                bid_amount,
                roas_average_floor,
            )
            .map_err(|e| fail(&e, json))?;
            one_ads_bid_update(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::UpdateSchedule {
            site,
            id,
            confirm_id,
            allow_schedule_edit: _,
            start_time,
            end_time,
        } => {
            let request =
                build_ads_schedule_update_request(&site, &id, &confirm_id, start_time, end_time)
                    .map_err(|e| fail(&e, json))?;
            one_ads_schedule_update(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::UpdatePlacement {
            site,
            id,
            confirm_id,
            allow_placement_edit: _,
            publisher_platform,
            facebook_position,
            instagram_position,
            whatsapp_position,
        } => {
            let request = build_ads_placement_update_request(
                &site,
                &id,
                &confirm_id,
                publisher_platform,
                facebook_position,
                instagram_position,
                whatsapp_position,
            )
            .map_err(|e| fail(&e, json))?;
            one_ads_placement_update(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::UpdateTargeting {
            site,
            id,
            confirm_id,
            allow_targeting_edit: _,
            targeting_file,
        } => {
            let request =
                build_ads_targeting_update_request(&site, &id, &confirm_id, &targeting_file)
                    .map_err(|e| fail(&e, json))?;
            one_ads_targeting_update(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::SwapCreative {
            site,
            id,
            confirm_id,
            creative_id,
            allow_creative_swap: _,
        } => {
            let request = build_ads_creative_swap_request(&site, &id, &confirm_id, &creative_id)
                .map_err(|e| fail(&e, json))?;
            one_ads_creative_swap(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::Status {
            site,
            entity,
            id,
            wait,
        } => {
            let request =
                build_ad_review_status_request(&site, &entity, &id).map_err(|e| fail(&e, json))?;
            one_ad_review_status(
                &client,
                &AccountKey::new(&site, &account),
                request,
                wait,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::PreviewCreative {
            site,
            creative_id,
            ad_format,
            output,
        } => {
            let request = build_creative_preview_request(&site, &creative_id, &ad_format)
                .map_err(|e| fail(&e, json))?;
            one_creative_preview(
                &client,
                &AccountKey::new(&site, &account),
                request,
                output,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::UploadImage {
            site,
            ad_account,
            file,
        } => {
            // The core library intentionally receives opaque bytes rather
            // than a host path. This is the sole filesystem boundary, and it
            // turns an unreadable local file into a stable field-level error
            // without leaking user or CI directory names.
            let filename = file
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .ok_or_else(|| fail(&ads_input_error(&site, "invalid_image_filename"), json))?;
            let bytes = std::fs::read(file)
                .map_err(|_| fail(&ads_input_error(&site, "image_file_unreadable"), json))?;
            let request = build_upload_ad_image_request(&site, ad_account, filename, bytes)
                .map_err(|e| fail(&e, json))?;
            one_image_upload(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::UploadVideo {
            site,
            ad_account,
            file,
            wait,
        } => {
            let filename = file
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .ok_or_else(|| fail(&ads_input_error(&site, "invalid_video_filename"), json))?;
            let bytes = std::fs::read(file)
                .map_err(|_| fail(&ads_input_error(&site, "video_file_unreadable"), json))?;
            let request = build_upload_ad_video_request(&site, ad_account, filename, bytes)
                .map_err(|e| fail(&e, json))?;
            one_video_upload(
                &client,
                &AccountKey::new(&site, &account),
                request,
                wait,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::VideoStatus { site, id, wait } => {
            one_video_status(
                &client,
                &AccountKey::new(&site, &account),
                &site,
                id,
                wait,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::CreateLinkCreative {
            site,
            ad_account,
            name,
            page_id,
            image_hash,
            message,
            headline,
            destination_url,
            call_to_action,
            geo_link,
            application_id,
            app_link,
            instagram_user_id,
            advantage_plus,
            whatsapp_identity_id,
            whatsapp_phone_number,
        } => {
            let request = build_link_ad_creative_request(
                &site,
                LinkCreativeOptions {
                    ad_account,
                    name,
                    page_id,
                    image_hash,
                    message,
                    headline,
                    destination_url,
                    call_to_action,
                    geo_link,
                    application_id,
                    app_link,
                    instagram_user_id,
                    advantage_plus,
                    whatsapp_identity: whatsapp_identity_id.map(|identity_id| {
                        postkit::WhatsAppStatusIdentity {
                            identity_id,
                            phone_number: whatsapp_phone_number,
                        }
                    }),
                },
            )
            .map_err(|e| fail(&e, json))?;
            one_link_creative(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::CreateVideoCreative {
            site,
            ad_account,
            name,
            page_id,
            video_id,
            image_hash,
            message,
            destination_url,
            call_to_action,
            geo_link,
            application_id,
            app_link,
            wait,
        } => {
            let request = build_video_ad_creative_request(
                &site,
                VideoCreativeOptions {
                    ad_account,
                    name,
                    page_id,
                    video_id,
                    image_hash,
                    message,
                    destination_url,
                    call_to_action,
                    geo_link,
                    application_id,
                    app_link,
                },
            )
            .map_err(|e| fail(&e, json))?;
            one_video_creative(
                &client,
                &AccountKey::new(&site, &account),
                request,
                wait,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::CreateCarouselCreative {
            site,
            ad_account,
            name,
            page_id,
            message,
            call_to_action,
            cards_file,
            instagram_user_id,
            advantage_plus,
        } => {
            let request = build_carousel_creative_request(
                &site,
                ad_account,
                name,
                page_id,
                message,
                &call_to_action,
                &cards_file,
                instagram_user_id,
                advantage_plus,
            )
            .map_err(|e| fail(&e, json))?;
            one_extra_creative(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::CreateCatalogCreative {
            site,
            ad_account,
            name,
            page_id,
            product_set_id,
            link,
            message,
            call_to_action,
            instagram_user_id,
            advantage_plus,
        } => {
            let request = build_catalog_creative_request(
                &site,
                ad_account,
                name,
                page_id,
                product_set_id,
                link,
                message,
                &call_to_action,
                instagram_user_id,
                advantage_plus,
            )
            .map_err(|e| fail(&e, json))?;
            one_extra_creative(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::CreateLeadFormCreative {
            site,
            ad_account,
            name,
            page_id,
            image_hash,
            message,
            headline,
            destination_url,
            lead_gen_form_id,
            call_to_action,
            instagram_user_id,
            advantage_plus,
        } => {
            let request = build_lead_form_creative_request(
                &site,
                ad_account,
                name,
                page_id,
                image_hash,
                message,
                headline,
                destination_url,
                lead_gen_form_id,
                &call_to_action,
                instagram_user_id,
                advantage_plus,
            )
            .map_err(|e| fail(&e, json))?;
            one_extra_creative(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::CreateAppInstallCreative {
            site,
            ad_account,
            name,
            page_id,
            image_hash,
            message,
            application_id,
            object_store_url,
            instagram_user_id,
            advantage_plus,
        } => {
            let request = build_app_install_creative_request(
                &site,
                ad_account,
                name,
                page_id,
                image_hash,
                message,
                application_id,
                object_store_url,
                instagram_user_id,
                advantage_plus,
            )
            .map_err(|e| fail(&e, json))?;
            one_extra_creative(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::CreateCampaign {
            site,
            ad_account,
            name,
            objective,
            special_ad_categories,
            daily_budget,
            lifetime_budget,
            adset_budget_sharing,
        } => {
            let request = build_paused_campaign_request(
                &site,
                PausedCampaignOptions {
                    ad_account,
                    name,
                    objective,
                    special_ad_categories,
                    daily_budget,
                    lifetime_budget,
                    is_adset_budget_sharing_enabled: adset_budget_sharing,
                },
            )
            .map_err(|e| fail(&e, json))?;
            one_paused_create(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::CreateAdset {
            site,
            ad_account,
            name,
            campaign_id,
            daily_budget,
            lifetime_budget,
            bid_strategy,
            bid_amount,
            roas_average_floor,
            start_time,
            end_time,
            billing_event,
            optimization_goal,
            countries,
            age_min,
            age_max,
            publisher_platforms,
            facebook_positions,
            instagram_positions,
            whatsapp_positions,
            user_age_unknown,
            promoted_page_id,
            promoted_pixel_id,
            custom_event_type,
            promoted_application_id,
            object_store_url,
            promoted_product_set_id,
        } => {
            let request = build_paused_adset_request(
                &site,
                PausedAdsetOptions {
                    ad_account,
                    name,
                    campaign_id,
                    daily_budget,
                    lifetime_budget,
                    bid_strategy,
                    bid_amount,
                    roas_average_floor,
                    start_time,
                    end_time,
                    billing_event,
                    optimization_goal,
                    countries,
                    age_min,
                    age_max,
                    publisher_platforms,
                    facebook_positions,
                    instagram_positions,
                    whatsapp_positions,
                    user_age_unknown,
                    promoted_object: build_promoted_object(
                        &site,
                        promoted_page_id,
                        promoted_pixel_id,
                        custom_event_type,
                        promoted_application_id,
                        object_store_url,
                        promoted_product_set_id,
                    )
                    .map_err(|e| fail(&e, json))?,
                },
            )
            .map_err(|e| fail(&e, json))?;
            one_paused_create(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::CreateAd {
            site,
            ad_account,
            name,
            adset_id,
            creative_id,
        } => {
            let request =
                build_paused_ad_request(&site, ad_account, &name, &adset_id, &creative_id)
                    .map_err(|e| fail(&e, json))?;
            one_paused_create(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        AdsCmd::ValidateDraft { site, manifest } => {
            let manifest = read_draft_manifest(&site, &manifest).map_err(|e| fail(&e, json))?;
            match client.validate_paused_draft(&Site::new(&site), &manifest) {
                Ok((account_id, fingerprint)) => {
                    if json {
                        emit_raw(&serde_json::json!({
                            "site": site,
                            "valid": true,
                            "account_id": account_id,
                            "fingerprint": fingerprint,
                        }));
                    } else {
                        human_line(format!("{site} manifest valid; {account_id}"));
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        AdsCmd::CreateDraft {
            site,
            manifest,
            state,
        } => {
            let manifest = read_draft_manifest(&site, &manifest).map_err(|e| fail(&e, json))?;
            let image = read_draft_image(&site, &manifest).map_err(|e| fail(&e, json))?;
            let key = AccountKey::new(&site, &account);
            let result = client
                .run_paused_draft(RunPausedDraft {
                    key: &key,
                    manifest: &manifest,
                    image: image.as_ref(),
                    store: &FileDraftStore,
                    state_path: &state,
                    resume: false,
                    deadline,
                })
                .await;
            match result {
                Ok(reply) => {
                    emit_draft_result(&reply, json);
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        AdsCmd::ResumeDraft {
            site,
            manifest,
            state,
        } => {
            let manifest = read_draft_manifest(&site, &manifest).map_err(|e| fail(&e, json))?;
            let image = read_draft_image(&site, &manifest).map_err(|e| fail(&e, json))?;
            let key = AccountKey::new(&site, &account);
            let result = client
                .run_paused_draft(RunPausedDraft {
                    key: &key,
                    manifest: &manifest,
                    image: image.as_ref(),
                    store: &FileDraftStore,
                    state_path: &state,
                    resume: true,
                    deadline,
                })
                .await;
            match result {
                Ok(reply) => {
                    emit_draft_result(&reply, json);
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        AdsCmd::StatusDraft { site, state, wait } => {
            let key = AccountKey::new(&site, &account);
            // `--wait` polls the read path only, bounded by --deadline; the
            // bounded-wait contract (last observed state, never a raw
            // timeout) lives in the Client and is tested there.
            let reply = if wait {
                client
                    .paused_draft_status_wait(
                        &key,
                        &FileDraftStore,
                        &state,
                        deadline,
                        std::time::Duration::from_secs(2),
                    )
                    .await
            } else {
                client
                    .paused_draft_status(&key, &FileDraftStore, &state, deadline)
                    .await
            }
            .map_err(|e| fail(&e, json))?;
            emit_draft_status(&reply, json);
            Ok(())
        }
        AdsCmd::AdoptDraftStep {
            site,
            state,
            step,
            id,
        } => {
            let step =
                DraftStep::from_str(&step).map_err(|e| fail(&ads_input_error(&site, e), json))?;
            let result = client
                .adopt_paused_draft_step(
                    &AccountKey::new(&site, &account),
                    &FileDraftStore,
                    &state,
                    step,
                    id,
                    deadline,
                )
                .await;
            match result {
                Ok(reply) => {
                    emit_draft_result(&reply, json);
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_insights(
    client: &Client,
    home: &Path,
    site: String,
    from: String,
    until: String,
    level: String,
    metrics: String,
    attribution: String,
    ad_account: Option<String>,
    entity_ids: Vec<String>,
    breakdowns: String,
    async_report: bool,
    report: String,
    json: bool,
    account: &str,
    deadline: Deadline,
) -> Result<(), i32> {
    let query = build_insights_query(
        &site,
        &from,
        &until,
        &level,
        &metrics,
        &attribution,
        InsightsOptions {
            ad_account,
            entity_ids,
            breakdowns,
            report,
        },
    )
    .map_err(|e| fail(&e, json))?;
    let key = AccountKey::new(&site, account);
    if async_report {
        return run_async_insights(client, home, &key, query, deadline, json).await;
    }
    match client.insights(&key, query, deadline).await {
        Ok(reply) => {
            if json {
                emit_raw(&serde_json::to_value(&reply).expect("json"));
            } else {
                human_line(format!(
                    "{} {} {}",
                    reply.site,
                    reply.account_id,
                    reply.currency.as_deref().unwrap_or("-")
                ));
                for row in &reply.rows {
                    human_line(insight_line(row));
                }
            }
            Ok(())
        }
        Err(e) => Err(fail(&e, json)),
    }
}
