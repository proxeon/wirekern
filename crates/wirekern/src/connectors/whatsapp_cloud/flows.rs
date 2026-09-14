//! WABA Flow list/get/create/publish.
use crate::error::Error;
use crate::facets::WhatsAppFlows;
use crate::types::{AccountCreds, AppConfig, Deadline, Site};
use crate::whatsapp::{WhatsAppFlowDraft, WhatsAppFlowList, WhatsAppFlowRecord, WhatsAppPageQuery};
use async_trait::async_trait;
use serde_json::{json, Value};

use super::graph::{
    access_token, graph_after, graph_page_url, page_query_error, read_json, validate_graph_id,
    value_string, waba_id,
};
use super::{WhatsAppCloud, SITE};

#[async_trait]
impl WhatsAppFlows for WhatsAppCloud {
    async fn list_flows(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        query: &WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<WhatsAppFlowList, Error> {
        page_query_error(query, &self.site)?;
        let waba = waba_id(app)?;
        let token = access_token(creds)?;
        let url = graph_page_url(
            format!(
                "{}/{waba}/flows?fields=id,name,status,categories",
                self.base
            ),
            query,
        );
        let response = self
            .http
            .send(self.http.get(&url).bearer_auth(token), deadline, &self.site)
            .await?;
        let body = read_json(response, &self.site).await?;
        let flows = body
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "flow_list_invalid".into(),
                message: "WhatsApp flow list returned no data array".into(),
            })?
            .iter()
            .map(|v| parse_flow_record(v, "flow_list_invalid"))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(WhatsAppFlowList {
            flows,
            after: graph_after(&body),
        })
    }

    async fn get_flow(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        flow_id: &str,
        deadline: Deadline,
    ) -> Result<WhatsAppFlowRecord, Error> {
        validate_graph_id(flow_id).map_err(|reason| Error::InvalidPost {
            site: self.site.clone(),
            reason,
            limit: None,
        })?;
        let _waba = waba_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .get(&format!(
                        "{}/{flow_id}?fields=id,name,status,categories",
                        self.base
                    ))
                    .bearer_auth(token),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        parse_flow_record(&body, "missing_flow_id")
    }

    async fn create_flow(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        draft: &WhatsAppFlowDraft,
        deadline: Deadline,
    ) -> Result<WhatsAppFlowRecord, Error> {
        draft.validate().map_err(|reason| Error::InvalidPost {
            site: self.site.clone(),
            reason,
            limit: None,
        })?;
        let waba = waba_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .post(&format!("{}/{waba}/flows", self.base))
                    .bearer_auth(token)
                    .json(&json!({
                        "name": draft.name,
                        "categories": draft.categories,
                        "flow_json": draft.flow_json,
                    })),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        if body
            .get("validation_errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty())
        {
            return Err(Error::Platform {
                site: self.site.clone(),
                code: "flow_validation_errors".into(),
                message: "WhatsApp rejected the Flow JSON schema".into(),
            });
        }
        parse_flow_record(&body, "missing_flow_id")
    }

    async fn publish_flow(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        flow_id: &str,
        deadline: Deadline,
    ) -> Result<WhatsAppFlowRecord, Error> {
        validate_graph_id(flow_id).map_err(|reason| Error::InvalidPost {
            site: self.site.clone(),
            reason,
            limit: None,
        })?;
        let _waba = waba_id(app)?;
        let token = access_token(creds)?;
        // Official publish has no JSON body and returns `{success: true}`,
        // not a Flow object. Requiring `id` here would treat a successful
        // publish as a platform error.
        let response = self
            .http
            .send(
                self.http
                    .post(&format!("{}/{flow_id}/publish", self.base))
                    .bearer_auth(token),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        if body.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(Error::Platform {
                site: self.site.clone(),
                code: "flow_publish_failed".into(),
                message: "WhatsApp Flow publish did not return success".into(),
            });
        }
        Ok(WhatsAppFlowRecord {
            id: flow_id.to_string(),
            name: None,
            status: Some("PUBLISHED".into()),
            categories: vec![],
        })
    }
}

pub(super) fn parse_flow_record(value: &Value, missing: &str) -> Result<WhatsAppFlowRecord, Error> {
    let categories = value
        .get("categories")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(value_string).collect::<Vec<_>>())
        .unwrap_or_default();
    let id = value
        .get("id")
        .and_then(value_string)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::Platform {
            site: Site::new(SITE),
            code: missing.into(),
            message: "WhatsApp Flow response had no id".into(),
        })?;
    Ok(WhatsAppFlowRecord {
        id,
        name: value.get("name").and_then(value_string),
        status: value.get("status").and_then(value_string),
        categories,
    })
}
