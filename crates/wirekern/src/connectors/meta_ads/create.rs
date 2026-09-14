//! Paused campaign/ad-set/ad creates and asset uploads.
use crate::ads::{CreatedAd, PausedAdCreate, UploadAdImageRequest, UploadedAdImage};
use crate::error::Error;
use crate::form::form;
use crate::http::Http;
use crate::types::{Deadline, Site};
use serde_json::Value;

use super::graph::{read_json, value_string};

/// Submit the one intentionally narrow Tier B form. `status=PAUSED` lives in
/// this function rather than in a public request type, so neither CLI users
/// nor library callers have a way to turn a create into an active delivery.
pub(super) async fn create_paused_ad(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    create: &PausedAdCreate,
    deadline: Deadline,
) -> Result<CreatedAd, Error> {
    let (path, entity, mut fields) = match create {
        PausedAdCreate::Campaign(campaign) => {
            let mut fields = vec![
                ("name", campaign.name.clone()),
                ("objective", campaign.objective.meta_value().into()),
                (
                    "special_ad_categories",
                    serde_json::to_string(&campaign.special_ad_categories)
                        .expect("Vec<String> serializes"),
                ),
                (
                    // Always explicit for ABO (v24.0+). `false` keeps
                    // independent ad-set budgets; `true` is Meta's up-to-20%
                    // ABO share and is refused locally with a campaign budget.
                    "is_adset_budget_sharing_enabled",
                    if campaign.is_adset_budget_sharing_enabled {
                        "true".into()
                    } else {
                        "false".into()
                    },
                ),
            ];
            // CBO: Meta's campaign create takes daily XOR lifetime. Sending
            // neither leaves budget on the ad set (Advantage campaign budget
            // off). Sending both is refused before HTTP.
            if let Some(daily) = campaign.daily_budget {
                fields.push(("daily_budget", daily.to_string()));
            }
            if let Some(lifetime) = campaign.lifetime_budget {
                fields.push(("lifetime_budget", lifetime.to_string()));
            }
            ("campaigns", crate::ads::AdEntity::Campaign, fields)
        }
        PausedAdCreate::Adset(adset) => {
            let mut fields = vec![
                ("name", adset.name.clone()),
                ("campaign_id", adset.campaign_id.clone()),
                ("bid_strategy", adset.bid_strategy.meta_value().into()),
                ("billing_event", adset.billing_event.meta_value().into()),
                (
                    "optimization_goal",
                    adset.optimization_goal.meta_value().into(),
                ),
                (
                    "targeting",
                    serde_json::to_string(&adset.targeting).expect("AdTargeting serializes"),
                ),
            ];
            // Ad-set daily XOR lifetime. Both omitted is a CBO child: the
            // parent campaign already posted the shared budget.
            if let Some(daily) = adset.daily_budget {
                fields.push(("daily_budget", daily.to_string()));
            }
            if let Some(lifetime) = adset.lifetime_budget {
                fields.push(("lifetime_budget", lifetime.to_string()));
            }
            // Cap strategies: Meta `bid_amount` in account minor units.
            // Min-ROAS: `bid_constraints.roas_average_floor` (10000 = 1.0);
            // Meta forbids combining this with `bid_amount`.
            if let Some(amount) = adset.bid_amount {
                fields.push(("bid_amount", amount.to_string()));
            }
            if let Some(floor) = adset.roas_average_floor {
                fields.push((
                    "bid_constraints",
                    serde_json::json!({ "roas_average_floor": floor }).to_string(),
                ));
            }
            // Ad-set schedule is Meta's delivery window, not a Wirekern
            // calendar. Lifetime budget requires end_time (checked locally).
            if let Some(start) = &adset.start_time {
                fields.push(("start_time", start.clone()));
            }
            if let Some(end) = &adset.end_time {
                fields.push(("end_time", end.clone()));
            }
            if let Some(promoted) = &adset.promoted_object {
                fields.push(("promoted_object", promoted.meta_json().to_string()));
            }
            ("adsets", crate::ads::AdEntity::Adset, fields)
        }
        PausedAdCreate::Ad(ad) => (
            "ads",
            crate::ads::AdEntity::Ad,
            vec![
                ("name", ad.name.clone()),
                ("adset_id", ad.adset_id.clone()),
                (
                    "creative",
                    serde_json::json!({ "creative_id": ad.creative_id }).to_string(),
                ),
            ],
        ),
    };
    fields.push(("status", "PAUSED".into()));
    // The token goes in the form body rather than a URL query so proxy logs,
    // errors, and test output have fewer opportunities to expose it.
    fields.push(("access_token", token.into()));
    let pairs: Vec<(&str, &str)> = fields
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    let body = form(&pairs);
    let url = format!("{base}/act_{account}/{path}");
    let response = http
        .send(
            http.post(&url)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body),
            deadline,
            site,
        )
        .await?;
    let response = read_json(response, site).await?;
    let id = value_string(response.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_id".into(),
        message: "paused create returned no id".into(),
    })?;
    Ok(CreatedAd {
        site: site.clone(),
        account_id: format!("act_{account}"),
        entity,
        id,
        status: "PAUSED".into(),
    })
}

/// Upload an account image using Meta's multipart `filename` part. The
/// operator-supplied filename is validated as a basename before this point;
/// keeping it as multipart metadata lets Meta preserve media type inference
/// without leaking a local filesystem path into a request or error.
pub(super) async fn upload_ad_image(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    request: &UploadAdImageRequest,
    deadline: Deadline,
) -> Result<UploadedAdImage, Error> {
    let form = reqwest::multipart::Form::new()
        .part(
            "filename",
            reqwest::multipart::Part::bytes(request.bytes.clone())
                .file_name(request.filename.clone()),
        )
        // Put the bearer in the multipart body for the same log-safety reason
        // as paused create forms: never place credentials in a request URL.
        .text("access_token", token.to_string());
    let url = format!("{base}/act_{account}/adimages");
    let response = http
        .send(http.post(&url).multipart(form), deadline, site)
        .await?;
    let response = read_json(response, site).await?;
    let hash = response
        .get("images")
        .and_then(Value::as_object)
        .and_then(|images| {
            images
                .values()
                .find_map(|image| value_string(image.get("hash")))
        })
        .or_else(|| value_string(response.get("hash")))
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_image_hash".into(),
            message: "image upload returned no hash".into(),
        })?;
    Ok(UploadedAdImage {
        site: site.clone(),
        account_id: format!("act_{account}"),
        hash,
    })
}

/// Multipart `source` upload to `/advideos`. Encoding is a later GET of
/// `status.video_status`; this call only returns the numeric video id.
pub(super) async fn upload_ad_video(
    http: &Http,
    base: &str,
    site: &Site,
    account: &str,
    token: &str,
    request: &crate::ads::UploadAdVideoRequest,
    deadline: Deadline,
) -> Result<crate::ads::UploadedAdVideo, Error> {
    let form = reqwest::multipart::Form::new()
        .part(
            "source",
            reqwest::multipart::Part::bytes(request.bytes.clone())
                .file_name(request.filename.clone()),
        )
        .text("access_token", token.to_string());
    let url = format!("{base}/act_{account}/advideos");
    let response = http
        .send(http.post(&url).multipart(form), deadline, site)
        .await?;
    let response = read_json(response, site).await?;
    let id = value_string(response.get("id")).ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_video_id".into(),
        message: "video upload returned no id".into(),
    })?;
    Ok(crate::ads::UploadedAdVideo {
        site: site.clone(),
        account_id: format!("act_{account}"),
        id,
    })
}

pub(super) async fn read_ad_video_status(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    request: &crate::ads::AdVideoStatusRequest,
    deadline: Deadline,
) -> Result<crate::ads::AdVideoStatus, Error> {
    let params = form(&[("fields", "status"), ("access_token", token)]);
    let url = format!("{base}/{}?{params}", request.video_id);
    let response = http.send(http.get(&url), deadline, site).await?;
    let response = read_json(response, site).await?;
    let raw = response
        .get("status")
        .and_then(|status| status.get("video_status"))
        .and_then(Value::as_str)
        .or_else(|| response.get("status").and_then(Value::as_str));
    let raw = raw.ok_or_else(|| Error::Platform {
        site: site.clone(),
        code: "missing_video_status".into(),
        message: "video status returned no video_status".into(),
    })?;
    Ok(crate::ads::AdVideoStatus {
        site: site.clone(),
        video_id: request.video_id.clone(),
        video_status: crate::ads::AdVideoStatusKind::from_meta(raw),
        raw: Some(raw.to_string()),
    })
}
