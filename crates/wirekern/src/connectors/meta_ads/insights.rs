//! Insights reads and async insight jobs.
use crate::error::Error;
use crate::facets::InsightsSource;
use crate::form::form;
use crate::http::Http;
use crate::insights::{
    AdAccountsReply, AttributionWindow, InsightRow, InsightsJob, InsightsJobStatus, InsightsLevel,
    InsightsQuery, InsightsReply, Metric, MAX_INSIGHTS_RESULT_ROWS,
};
use crate::types::{AccountCreds, AppConfig, Deadline, Site};
use async_trait::async_trait;
use serde_json::Value;

use super::accounts::{account_currency, list_ad_accounts};
use super::graph::{access_token, account_id, map_graph_error, read_json, MAX_PAGES};
use super::{MetaAds, SITE};

#[async_trait]
impl InsightsSource for MetaAds {
    async fn insights(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        query: &InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, query.account.as_deref())?;
        let params = insights_form(query, token)?;
        let url = format!("{}/act_{}/insights?{}", self.base, account, params);
        let rows = fetch_insights_pages(&self.http, &self.site, &url, query, deadline).await?;
        let currency = account_currency(&self.http, &self.base, &account, token, deadline).await?;
        Ok(InsightsReply {
            site: self.site.clone(),
            account_id: format!("act_{account}"),
            currency,
            rows,
        })
    }

    async fn ad_accounts(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<AdAccountsReply, Error> {
        let token = access_token(creds)?;
        let accounts = list_ad_accounts(&self.http, &self.base, token, deadline).await?;
        Ok(AdAccountsReply {
            site: self.site.clone(),
            accounts,
        })
    }

    async fn start_insights_job(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        query: &InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        let token = access_token(creds)?;
        let account = account_id(creds, query.account.as_deref())?;
        let params = insights_form(query, token)?;
        let url = format!("{}/act_{}/insights", self.base, account);
        let resp = self
            .http
            .send(
                self.http
                    .post(&url)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(params),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(resp, &self.site).await?;
        let id = body
            .get("report_run_id")
            .or_else(|| body.get("id"))
            .and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "missing_report_run_id".into(),
                message: "insights job did not return report_run_id".into(),
            })?;
        Ok(InsightsJob {
            site: self.site.clone(),
            id,
            status: InsightsJobStatus::NotStarted,
            percent_complete: 0,
            error_code: None,
            error_message: None,
        })
    }

    async fn insights_job(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        job_id: &str,
        deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        let token = access_token(creds)?;
        read_insights_job(&self.http, &self.base, &self.site, token, job_id, deadline).await
    }

    async fn insights_job_result(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        job_id: &str,
        query: &InsightsQuery,
        deadline: Deadline,
    ) -> Result<InsightsReply, Error> {
        let token = access_token(creds)?;
        validate_insights_job_id(job_id)?;
        let account = account_id(creds, query.account.as_deref())?;
        let q = form(&[("access_token", token)]);
        let url = format!("{}/{}/insights?{q}", self.base, job_id);
        let rows = fetch_insights_pages(&self.http, &self.site, &url, query, deadline).await?;
        let currency = account_currency(&self.http, &self.base, &account, token, deadline).await?;
        Ok(InsightsReply {
            site: self.site.clone(),
            account_id: format!("act_{account}"),
            currency,
            rows,
        })
    }

    async fn cancel_insights_job(
        &self,
        _app: &AppConfig,
        creds: &AccountCreds,
        job_id: &str,
        deadline: Deadline,
    ) -> Result<InsightsJob, Error> {
        let token = access_token(creds)?;
        validate_insights_job_id(job_id)?;
        let q = form(&[("access_token", token)]);
        let url = format!("{}/{job_id}?{q}", self.base);
        let resp = self
            .http
            .send(self.http.delete(&url), deadline, &self.site)
            .await?;
        // DELETE may return `{success:true}` or an empty 200. Do not require
        // an Ad Report Run document after cancel.
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|_| Error::request_failed(&self.site))?;
        if !status.is_success() {
            return Err(map_graph_error(status.as_u16(), &text));
        }
        Ok(InsightsJob {
            site: self.site.clone(),
            id: job_id.into(),
            status: InsightsJobStatus::Skipped,
            percent_complete: 0,
            error_code: None,
            error_message: None,
        })
    }
}

pub(super) fn meta_field(m: Metric) -> Option<&'static str> {
    match m {
        Metric::Spend => Some("spend"),
        Metric::Impressions => Some("impressions"),
        Metric::Clicks => Some("clicks"),
        Metric::Reach => Some("reach"),
        Metric::Ctr => Some("ctr"),
        Metric::Cpc => Some("cpc"),
        Metric::Cpm => Some("cpm"),
        Metric::Purchases => None,
        Metric::PurchaseValue | Metric::Roas => None,
        Metric::Frequency => Some("frequency"),
        Metric::UniqueClicks => Some("unique_clicks"),
        Metric::InlineLinkClicks => Some("inline_link_clicks"),
        Metric::InlineLinkClickCtr => Some("inline_link_click_ctr"),
        Metric::QualityRanking => Some("quality_ranking"),
        Metric::VideoThruplay => Some("video_thruplay_watched_actions"),
    }
}

/// Graph's `action_attribution_windows` wants an array of atomic windows
/// (`["7d_click","1d_view"]`); the combined `7d_click_1d_view` is only the
/// Ads Manager display name for that preset and is rejected with code 100.
pub(super) fn attribution_param(a: AttributionWindow) -> &'static str {
    a.graph_windows()
}

pub(super) fn insights_form(query: &InsightsQuery, token: &str) -> Result<String, Error> {
    let mut fields: std::collections::BTreeSet<&str> = query
        .metrics
        .iter()
        .copied()
        .filter_map(meta_field)
        .collect();
    if query.metrics.contains(&Metric::Purchases) {
        fields.insert("actions");
    }
    if query
        .metrics
        .iter()
        .any(|metric| matches!(metric, Metric::PurchaseValue | Metric::Roas))
    {
        fields.insert("action_values");
    }
    if query.metrics.contains(&Metric::Roas) {
        fields.insert("spend");
    }
    let fields: Vec<&str> = fields.into_iter().collect();
    let field_list = fields.join(",");
    let range = format!(
        "{{\"since\":\"{}\",\"until\":\"{}\"}}",
        query.range.from, query.range.to
    );
    let filter = entity_filter(query)?;
    let breakdowns = query
        .breakdowns
        .iter()
        .map(|breakdown| breakdown.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let mut pairs = vec![
        ("level", query.level.as_str()),
        ("fields", field_list.as_str()),
        ("time_range", range.as_str()),
        ("time_increment", "1"),
        (
            "action_attribution_windows",
            attribution_param(query.attribution),
        ),
        ("access_token", token),
    ];
    if let Some(filter) = filter.as_deref() {
        pairs.push(("filtering", filter));
    }
    if !breakdowns.is_empty() {
        pairs.push(("breakdowns", breakdowns.as_str()));
    }
    Ok(form(&pairs))
}

pub(super) fn validate_insights_job_id(id: &str) -> Result<(), Error> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(Error::InvalidQuery {
            site: Site::new(SITE),
            reason: "bad_insights_job_id".into(),
        });
    }
    Ok(())
}

pub(super) async fn fetch_insights_pages(
    http: &Http,
    site: &Site,
    start_url: &str,
    query: &InsightsQuery,
    deadline: Deadline,
) -> Result<Vec<InsightRow>, Error> {
    let mut rows: Vec<InsightRow> = Vec::new();
    let mut next = Some(start_url.to_string());
    let mut pages = 0usize;
    while let Some(url) = next {
        deadline.check(site)?;
        pages += 1;
        if pages > MAX_PAGES {
            return Err(Error::Platform {
                site: site.clone(),
                code: "paging_exceeded".into(),
                message: format!("insights paging exceeded {MAX_PAGES} pages"),
            });
        }
        let resp = http.send(http.get(&url), deadline, site).await?;
        let body = read_json(resp, site).await?;
        if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
            for item in data {
                rows.push(row_from(item, query));
                if rows.len() > MAX_INSIGHTS_RESULT_ROWS {
                    return Err(Error::InvalidQuery {
                        site: site.clone(),
                        reason: format!("insights_row_cap:{MAX_INSIGHTS_RESULT_ROWS}"),
                    });
                }
            }
        }
        next = body
            .get("paging")
            .and_then(|p| p.get("next"))
            .and_then(|n| n.as_str())
            .map(str::to_string);
    }
    rows.sort_by(|a, b| {
        (
            &a.entity_id,
            &a.date_start,
            serde_json::to_string(&a.dimensions).unwrap_or_default(),
        )
            .cmp(&(
                &b.entity_id,
                &b.date_start,
                serde_json::to_string(&b.dimensions).unwrap_or_default(),
            ))
    });
    Ok(rows)
}

pub(super) async fn read_insights_job(
    http: &Http,
    base: &str,
    site: &Site,
    token: &str,
    job_id: &str,
    deadline: Deadline,
) -> Result<InsightsJob, Error> {
    validate_insights_job_id(job_id)?;
    let q = form(&[
        (
            "fields",
            "async_status,async_percent_completion,error_code,error_message,error_user_msg",
        ),
        ("access_token", token),
    ]);
    let url = format!("{base}/{job_id}?{q}");
    let resp = http.send(http.get(&url), deadline, site).await?;
    let body = read_json(resp, site).await?;
    let status = InsightsJobStatus::from_meta(
        body.get("async_status")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    let percent = body
        .get("async_percent_completion")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_f64().map(|n| n as u64))
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0)
        .min(100) as u8;
    let error_message = body
        .get("error_user_msg")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| body.get("error_message").and_then(|v| v.as_str()))
        .map(str::to_string);
    Ok(InsightsJob {
        site: site.clone(),
        id: job_id.into(),
        status,
        percent_complete: percent,
        error_code: body
            .get("error_code")
            .map(|v| {
                v.as_i64()
                    .map(|n| n.to_string())
                    .or_else(|| v.as_str().map(str::to_string))
                    .unwrap_or_default()
            })
            .filter(|s| !s.is_empty()),
        error_message,
    })
}

/// Graph returns numerics as JSON *strings* ("12.34", "12345") — parse
/// either shape, preserving int-ness for count metrics.
pub(super) fn number(v: &Value) -> Option<Value> {
    if let Some(s) = v.as_str() {
        return s
            .parse::<u64>()
            .map(Value::from)
            .ok()
            .or_else(|| s.parse::<f64>().ok().map(Value::from));
    }
    if v.is_number() {
        return Some(v.clone());
    }
    None
}

/// Sum the action rows that mean "purchase". Meta's event taxonomy has
/// several purchase-ish action_types; the two below cover API and pixel.
pub(super) fn is_purchase_action(kind: &str) -> bool {
    kind == "purchase" || kind == "offsite_conversion.fb_pixel_purchase"
}

pub(super) fn purchases_of(item: &Value) -> Value {
    let mut total: u64 = 0;
    if let Some(actions) = item.get("actions").and_then(|a| a.as_array()) {
        for a in actions {
            let kind = a.get("action_type").and_then(|t| t.as_str()).unwrap_or("");
            if is_purchase_action(kind) {
                total += a
                    .get("value")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .or_else(|| a.get("value").and_then(|v| v.as_u64()))
                    .unwrap_or(0);
            }
        }
    }
    Value::from(total)
}

/// Sum the monetary purchase events that correspond to `purchases_of`.
/// `None` means Graph omitted `action_values`; zero is a meaningful result
/// when Graph supplied the array but it contained no purchase event.
pub(super) fn purchase_value_of(item: &Value) -> Option<Value> {
    let values = item.get("action_values")?.as_array()?;
    let mut total = 0.0f64;
    for value in values {
        let kind = value
            .get("action_type")
            .and_then(|kind| kind.as_str())
            .unwrap_or("");
        if is_purchase_action(kind) {
            total += number(value.get("value").unwrap_or(&Value::Null))?.as_f64()?;
        }
    }
    Some(Value::from(total))
}

pub(super) fn video_thruplay_of(item: &Value) -> Value {
    let mut total: u64 = 0;
    if let Some(actions) = item
        .get("video_thruplay_watched_actions")
        .and_then(|a| a.as_array())
    {
        for action in actions {
            total += action
                .get("value")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .or_else(|| action.get("value").and_then(|v| v.as_u64()))
                .unwrap_or(0);
        }
    }
    Value::from(total)
}

/// ROAS is a per-row derived value, not a Meta field. Null protects callers
/// from treating absent attribution data or a zero denominator as a real 0x.
pub(super) fn roas_of(item: &Value) -> Option<Value> {
    let spend = number(item.get("spend").unwrap_or(&Value::Null))?.as_f64()?;
    if spend == 0.0 {
        return None;
    }
    let value = purchase_value_of(item)?.as_f64()?;
    Some(Value::from(value / spend))
}

/// Translate generic entity IDs into Meta's structured filtering grammar.
/// Accounts are already selected by the `/act_<id>/insights` path, so an
/// account-level entity filter would be misleading and is rejected early.
pub(super) fn entity_filter(query: &InsightsQuery) -> Result<Option<String>, Error> {
    if query.entity_ids.is_empty() {
        return Ok(None);
    }
    let field = match query.level {
        InsightsLevel::Campaign => "campaign.id",
        InsightsLevel::Adset => "adset.id",
        InsightsLevel::Ad => "ad.id",
        InsightsLevel::Account => {
            return Err(Error::InvalidQuery {
                site: Site::new(SITE),
                reason: "entity_filter_unsupported:account".into(),
            });
        }
    };
    let mut ids = std::collections::BTreeSet::new();
    for id in &query.entity_ids {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::InvalidQuery {
                site: Site::new(SITE),
                reason: format!("bad_entity_id:{id}"),
            });
        }
        ids.insert(id);
    }
    Ok(Some(
        serde_json::json!([{
            "field": field,
            "operator": "IN",
            "value": ids.into_iter().collect::<Vec<_>>(),
        }])
        .to_string(),
    ))
}

pub(super) fn row_from(item: &Value, query: &InsightsQuery) -> InsightRow {
    let entity_id = item
        .get(query.level.id_field())
        .and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .unwrap_or_default();
    let mut metrics = serde_json::Map::new();
    for m in &query.metrics {
        let value = match m {
            Metric::Purchases => purchases_of(item),
            Metric::PurchaseValue => purchase_value_of(item).unwrap_or(Value::Null),
            Metric::Roas => roas_of(item).unwrap_or(Value::Null),
            Metric::VideoThruplay => video_thruplay_of(item),
            Metric::QualityRanking => item
                .get("quality_ranking")
                .and_then(|v| v.as_str())
                .map(Value::from)
                .unwrap_or(Value::Null),
            _ => number(item.get(m.as_str()).unwrap_or(&Value::Null)).unwrap_or(Value::Null),
        };
        metrics.insert(m.as_str().into(), value);
    }
    let mut dimensions = serde_json::Map::new();
    for breakdown in &query.breakdowns {
        dimensions.insert(
            breakdown.as_str().into(),
            item.get(breakdown.as_str()).cloned().unwrap_or(Value::Null),
        );
    }
    InsightRow {
        entity_id,
        level: query.level,
        date_start: item
            .get("date_start")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string(),
        dimensions,
        metrics,
    }
}
