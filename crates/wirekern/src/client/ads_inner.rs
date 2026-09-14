//! Confirmed ads POST helpers shared by Client lifecycle methods.
use crate::ads::{
    AdReviewStatusRequest, AdsActivateRequest, AdsConfiguredStatus, AdsEditOutcome,
    AdsInspectReply, AdsInspectRequest, AdsInventoryKind, AdsLifecycleOutcome, AdsPauseRequest,
    AdsStatusUpdateRequest, AdsTargetingDiff, ACTIVATE_RECONCILE_GUIDANCE, EDIT_RECONCILE_GUIDANCE,
    PAUSE_RECONCILE_GUIDANCE,
};
use crate::error::Error;
use crate::facets::AdsManager;
use crate::types::{AccountCreds, AppConfig, Deadline};

pub(super) async fn activate_ad_inner(
    ads: &dyn AdsManager,
    app: &AppConfig,
    creds: &AccountCreds,
    request: &AdsActivateRequest,
    deadline: Deadline,
) -> Result<AdsLifecycleOutcome, Error> {
    let review = ads
        .ad_review_status(
            app,
            creds,
            &AdReviewStatusRequest {
                entity: request.entity,
                id: request.id.clone(),
            },
            deadline,
        )
        .await?;
    // ARCHIVED stays refused. Meta can restore with status=ACTIVE; Wirekern
    // does not treat archive as a pause we can reverse through activate.
    if review.configured_status != "PAUSED" {
        return Err(Error::InvalidQuery {
            site: review.site.clone(),
            reason: format!("not_paused:{}", review.configured_status),
        });
    }
    if review.is_pending_review() {
        return Err(Error::InvalidQuery {
            site: review.site.clone(),
            reason: "review_unresolved".into(),
        });
    }
    if !review.issues.is_empty() {
        return Err(Error::InvalidQuery {
            site: review.site.clone(),
            reason: "review_issues".into(),
        });
    }
    let inspect = ads
        .inspect_ads_object(
            app,
            creds,
            &AdsInspectRequest {
                kind: AdsInventoryKind::from_entity(request.entity),
                id: request.id.clone(),
            },
            deadline,
        )
        .await?;
    confirm_activate_budget(&inspect, request).map_err(|reason| Error::InvalidQuery {
        site: inspect.site.clone(),
        reason,
    })?;
    match ads
        .update_ad_status(
            app,
            creds,
            &AdsStatusUpdateRequest {
                entity: request.entity,
                id: request.id.clone(),
                status: AdsConfiguredStatus::Active,
            },
            deadline,
        )
        .await
    {
        Ok(status) => Ok(AdsLifecycleOutcome::Applied { status }),
        Err(Error::Network { .. } | Error::DeadlineExceeded { .. }) => {
            Ok(AdsLifecycleOutcome::ReconciliationRequired {
                entity: request.entity,
                id: request.id.clone(),
                guidance: ACTIVATE_RECONCILE_GUIDANCE.into(),
            })
        }
        Err(error) => Err(error),
    }
}

pub(super) fn merge_string_list(targeting: &mut serde_json::Value, key: &str, values: Vec<String>) {
    if values.is_empty() {
        return;
    }
    targeting[key] = serde_json::json!(values);
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn post_then_inspect(
    ads: &dyn AdsManager,
    app: &AppConfig,
    creds: &AccountCreds,
    entity: crate::ads::AdEntity,
    id: &str,
    fields: &[(String, String)],
    targeting_diff: Option<AdsTargetingDiff>,
    deadline: Deadline,
) -> Result<AdsEditOutcome, Error> {
    match ads.post_ad_update(app, creds, id, fields, deadline).await {
        Ok(()) => match ads
            .inspect_ads_object(
                app,
                creds,
                &AdsInspectRequest {
                    kind: AdsInventoryKind::from_entity(entity),
                    id: id.into(),
                },
                deadline,
            )
            .await
        {
            Ok(inspect) => Ok(AdsEditOutcome::Applied {
                inspect: Box::new(inspect),
                targeting_diff,
            }),
            Err(Error::Network { .. } | Error::DeadlineExceeded { .. }) => {
                Ok(AdsEditOutcome::ReconciliationRequired {
                    entity,
                    id: id.into(),
                    guidance: EDIT_RECONCILE_GUIDANCE.into(),
                })
            }
            Err(error) => Err(error),
        },
        Err(Error::Network { .. } | Error::DeadlineExceeded { .. }) => {
            Ok(AdsEditOutcome::ReconciliationRequired {
                entity,
                id: id.into(),
                guidance: EDIT_RECONCILE_GUIDANCE.into(),
            })
        }
        Err(error) => Err(error),
    }
}

pub(super) fn confirm_budget_echo(
    inspect: &AdsInspectReply,
    confirm_daily_budget: Option<u64>,
    confirm_lifetime_budget: Option<u64>,
) -> Result<(), String> {
    let daily = inspect
        .daily_budget
        .as_deref()
        .map(|raw| raw.parse::<u64>())
        .transpose()
        .map_err(|_| "bad_inspect_daily_budget".to_string())?;
    let lifetime = inspect
        .lifetime_budget
        .as_deref()
        .map(|raw| raw.parse::<u64>())
        .transpose()
        .map_err(|_| "bad_inspect_lifetime_budget".to_string())?;
    if daily.is_some() && confirm_daily_budget != daily {
        return Err("confirm_daily_budget_mismatch".into());
    }
    if lifetime.is_some() && confirm_lifetime_budget != lifetime {
        return Err("confirm_lifetime_budget_mismatch".into());
    }
    if daily.is_none() && confirm_daily_budget.is_some() {
        return Err("confirm_daily_budget_not_on_object".into());
    }
    if lifetime.is_none() && confirm_lifetime_budget.is_some() {
        return Err("confirm_lifetime_budget_not_on_object".into());
    }
    Ok(())
}

pub(super) fn confirm_activate_budget(
    inspect: &AdsInspectReply,
    request: &AdsActivateRequest,
) -> Result<(), String> {
    confirm_budget_echo(
        inspect,
        request.confirm_daily_budget,
        request.confirm_lifetime_budget,
    )
}

pub(super) async fn pause_ad_inner(
    ads: &dyn AdsManager,
    app: &AppConfig,
    creds: &AccountCreds,
    request: &AdsPauseRequest,
    deadline: Deadline,
) -> Result<AdsLifecycleOutcome, Error> {
    let review = ads
        .ad_review_status(
            app,
            creds,
            &AdReviewStatusRequest {
                entity: request.entity,
                id: request.id.clone(),
            },
            deadline,
        )
        .await?;
    match review.configured_status.as_str() {
        "ARCHIVED" | "DELETED" => {
            return Err(Error::InvalidQuery {
                site: review.site.clone(),
                reason: format!("not_pausable:{}", review.configured_status),
            });
        }
        "PAUSED" => return Ok(AdsLifecycleOutcome::Applied { status: review }),
        _ => {}
    }
    match ads
        .update_ad_status(
            app,
            creds,
            &AdsStatusUpdateRequest {
                entity: request.entity,
                id: request.id.clone(),
                status: AdsConfiguredStatus::Paused,
            },
            deadline,
        )
        .await
    {
        Ok(status) => Ok(AdsLifecycleOutcome::Applied { status }),
        Err(Error::Network { .. } | Error::DeadlineExceeded { .. }) => {
            Ok(AdsLifecycleOutcome::ReconciliationRequired {
                entity: request.entity,
                id: request.id.clone(),
                guidance: PAUSE_RECONCILE_GUIDANCE.into(),
            })
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn confirmed_status_inner(
    ads: &dyn AdsManager,
    app: &AppConfig,
    creds: &AccountCreds,
    entity: crate::ads::AdEntity,
    id: &str,
    status: AdsConfiguredStatus,
    refuse: &[&str],
    guidance: &str,
    deadline: Deadline,
) -> Result<AdsLifecycleOutcome, Error> {
    let review = ads
        .ad_review_status(
            app,
            creds,
            &AdReviewStatusRequest {
                entity,
                id: id.into(),
            },
            deadline,
        )
        .await?;
    if refuse
        .iter()
        .any(|value| review.configured_status.eq_ignore_ascii_case(value))
    {
        return Err(Error::InvalidQuery {
            site: review.site.clone(),
            reason: format!("not_{}:{}", status.as_str(), review.configured_status),
        });
    }
    if review
        .configured_status
        .eq_ignore_ascii_case(status.meta_value())
    {
        return Ok(AdsLifecycleOutcome::Applied { status: review });
    }
    match ads
        .update_ad_status(
            app,
            creds,
            &AdsStatusUpdateRequest {
                entity,
                id: id.into(),
                status,
            },
            deadline,
        )
        .await
    {
        Ok(status) => Ok(AdsLifecycleOutcome::Applied { status }),
        Err(Error::Network { .. } | Error::DeadlineExceeded { .. }) => {
            Ok(AdsLifecycleOutcome::ReconciliationRequired {
                entity,
                id: id.into(),
                guidance: guidance.into(),
            })
        }
        Err(error) => Err(error),
    }
}
