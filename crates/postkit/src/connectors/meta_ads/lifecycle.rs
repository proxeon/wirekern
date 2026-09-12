//! Review status, status updates, copies, and field POSTs.
use crate::ads::{
    AdReviewIssue, AdReviewStatus, AdReviewStatusRequest, AdsDuplicateReply, AdsDuplicateRequest,
    AdsStatusUpdateRequest,
};
use crate::error::Error;
use crate::form::form;
use crate::http::Http;
use crate::types::{Deadline, Site};
use serde_json::Value;

use super::graph::{nonempty_value_string, read_json};

pub(super) async fn read_ad_review_status(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    request: &AdReviewStatusRequest,
    deadline: Deadline,
) -> Result<AdReviewStatus, Error> {
    let params = form(&[
        // `issues_info` carries Meta's concrete review/configuration errors;
        // without it operators see only a status and must switch tools to
        // learn why a draft cannot settle.
        (
            "fields",
            "id,name,configured_status,effective_status,issues_info",
        ),
        ("access_token", token),
    ]);
    let url = format!("{base}/{}?{params}", request.id);
    let response = http.send(http.get(&url), deadline, site).await?;
    let response = read_json(response, site).await?;
    let configured_status =
        nonempty_value_string(response.get("configured_status")).ok_or_else(|| {
            Error::Platform {
                site: site.clone(),
                code: "missing_configured_status".into(),
                message: "ad status returned no configured status".into(),
            }
        })?;
    let effective_status =
        nonempty_value_string(response.get("effective_status")).ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_effective_status".into(),
            message: "ad status returned no effective status".into(),
        })?;
    let issues = response
        .get("issues_info")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(review_issue_from).collect())
        .unwrap_or_default();
    Ok(AdReviewStatus {
        site: site.clone(),
        entity: request.entity,
        // Echo the validated request ID instead of trusting an optional Graph
        // response field: a malformed or partial reply cannot make a status
        // line appear to describe a different object.
        id: request.id.clone(),
        name: nonempty_value_string(response.get("name")),
        configured_status,
        effective_status,
        issues,
    })
}

/// Preserve Meta's review vocabulary without making the connector depend on
/// a particular issue subtype. Graph has used numeric and string error codes
/// across fields, so `value_string` normalizes both while absent fields remain
/// absent in Postkit's stable JSON response.
pub(super) fn review_issue_from(value: &Value) -> AdReviewIssue {
    AdReviewIssue {
        code: nonempty_value_string(value.get("error_code")),
        summary: nonempty_value_string(value.get("error_summary")),
        message: nonempty_value_string(value.get("error_message")),
        level: nonempty_value_string(value.get("level")),
    }
}

pub(super) async fn update_ad_status(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    request: &AdsStatusUpdateRequest,
    deadline: Deadline,
) -> Result<AdReviewStatus, Error> {
    let status = request.status.meta_value();
    let body = form(&[("status", status), ("access_token", token)]);
    let url = format!("{base}/{}", request.id);
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
    // Meta documents `{success: true}` for status updates. A false or
    // missing success is not a delivery claim.
    let success = response
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Err(Error::Platform {
            site: site.clone(),
            code: "status_update_unconfirmed".into(),
            message: "ad status update returned no success".into(),
        });
    }
    read_ad_review_status(
        http,
        base,
        site,
        token,
        &AdReviewStatusRequest {
            entity: request.entity,
            id: request.id.clone(),
        },
        deadline,
    )
    .await
}

/// Copy with Meta's documented default `status_option=PAUSED`. Postkit
/// never sends ACTIVE or INHERITED_FROM_SOURCE.
pub(super) async fn duplicate_ad_object(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    request: &AdsDuplicateRequest,
    deadline: Deadline,
) -> Result<AdsDuplicateReply, Error> {
    let body = form(&[("status_option", "PAUSED"), ("access_token", token)]);
    let url = format!("{base}/{}/copies", request.id);
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
    let copied_id = nonempty_value_string(response.get("copied_campaign_id"))
        .or_else(|| nonempty_value_string(response.get("copied_adset_id")))
        .or_else(|| nonempty_value_string(response.get("copied_ad_id")))
        .or_else(|| nonempty_value_string(response.get("copied_adgroup_id")))
        .ok_or_else(|| Error::Platform {
            site: site.clone(),
            code: "missing_copied_id".into(),
            message: "ad copy returned no copied id".into(),
        })?;
    Ok(AdsDuplicateReply {
        site: site.clone(),
        entity: request.entity,
        source_id: request.id.clone(),
        copied_id,
        status: "PAUSED".into(),
    })
}

pub(super) async fn post_ad_update(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    id: &str,
    fields: &[(String, String)],
    deadline: Deadline,
) -> Result<(), Error> {
    let mut pairs: Vec<(&str, &str)> = fields
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    pairs.push(("access_token", token));
    let body = form(&pairs);
    let url = format!("{base}/{id}");
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
    if !response
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(Error::Platform {
            site: site.clone(),
            code: "update_unconfirmed".into(),
            message: "ad object update returned no success".into(),
        });
    }
    Ok(())
}
