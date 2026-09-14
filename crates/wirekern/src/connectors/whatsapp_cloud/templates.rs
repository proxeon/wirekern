//! WABA message-template list/get/create/edit/delete.
use crate::error::Error;
use crate::facets::WhatsAppTemplates;
use crate::types::{AccountCreds, AppConfig, Deadline, Site};
use crate::whatsapp::{
    WhatsAppTemplateDraft, WhatsAppTemplateList, WhatsAppTemplateQuery, WhatsAppTemplateRecord,
};
use async_trait::async_trait;
use serde_json::{json, Value};

use super::graph::{
    access_token, graph_after, percent_encode, read_json, validate_graph_id, value_string, waba_id,
};
use super::{WhatsAppCloud, SITE};

#[async_trait]
impl WhatsAppTemplates for WhatsAppCloud {
    async fn list_templates(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        query: &WhatsAppTemplateQuery,
        deadline: Deadline,
    ) -> Result<WhatsAppTemplateList, Error> {
        if let Err(reason) = query.validate_page() {
            return Err(Error::InvalidPost {
                site: self.site.clone(),
                reason,
                limit: None,
            });
        }
        let waba = waba_id(app)?;
        let token = access_token(creds)?;
        let mut url = format!(
            "{}/{waba}/message_templates?fields=id,name,language,status,category,quality_score",
            self.base
        );
        if let Some(name) = &query.name {
            validate_template_query_name(name).map_err(|reason| Error::InvalidPost {
                site: self.site.clone(),
                reason,
                limit: None,
            })?;
            url.push_str("&name=");
            url.push_str(&percent_encode(name));
        }
        if let Some(status) = &query.status {
            validate_template_status_filter(status).map_err(|reason| Error::InvalidPost {
                site: self.site.clone(),
                reason,
                limit: None,
            })?;
            url.push_str("&status=");
            url.push_str(&percent_encode(status));
        }
        if let Some(limit) = query.limit {
            url.push_str(&format!("&limit={limit}"));
        }
        if let Some(after) = &query.after {
            url.push_str("&after=");
            url.push_str(&percent_encode(after));
        }
        let response = self
            .http
            .send(self.http.get(&url).bearer_auth(token), deadline, &self.site)
            .await?;
        let body = read_json(response, &self.site).await?;
        let templates = body
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "template_list_invalid".into(),
                message: "WhatsApp template list returned no data array".into(),
            })?
            .iter()
            .map(|v| parse_template_record(v, "template_list_invalid"))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(WhatsAppTemplateList {
            templates,
            after: graph_after(&body),
        })
    }

    async fn get_template(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        template_id: &str,
        deadline: Deadline,
    ) -> Result<WhatsAppTemplateRecord, Error> {
        validate_graph_id(template_id).map_err(|reason| Error::InvalidPost {
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
                        "{}/{template_id}?fields=id,name,language,status,category,quality_score",
                        self.base
                    ))
                    .bearer_auth(token),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        parse_template_record(&body, "missing_template_id")
    }

    async fn create_template(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        draft: &WhatsAppTemplateDraft,
        deadline: Deadline,
    ) -> Result<WhatsAppTemplateRecord, Error> {
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
                    .post(&format!("{}/{waba}/message_templates", self.base))
                    .bearer_auth(token)
                    .json(&template_draft_payload(draft)),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        parse_template_record(&body, "missing_template_id")
    }

    async fn edit_template(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        template_id: &str,
        draft: &WhatsAppTemplateDraft,
        deadline: Deadline,
    ) -> Result<WhatsAppTemplateRecord, Error> {
        validate_graph_id(template_id).map_err(|reason| Error::InvalidPost {
            site: self.site.clone(),
            reason,
            limit: None,
        })?;
        draft.validate().map_err(|reason| Error::InvalidPost {
            site: self.site.clone(),
            reason,
            limit: None,
        })?;
        let _waba = waba_id(app)?;
        let token = access_token(creds)?;
        let mut payload = template_draft_payload(draft);
        // Name is immutable after create; sending it on edit is rejected.
        payload.as_object_mut().map(|o| o.remove("name"));
        let response = self
            .http
            .send(
                self.http
                    .post(&format!("{}/{template_id}", self.base))
                    .bearer_auth(token)
                    .json(&payload),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        parse_template_record(&body, "missing_template_id")
    }

    async fn delete_template(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        name: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        crate::whatsapp::validate_template_name(name).map_err(|reason| Error::InvalidPost {
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
                    .delete(&format!(
                        "{}/{waba}/message_templates?name={name}",
                        self.base
                    ))
                    .bearer_auth(token),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        if body.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(Error::Platform {
                site: self.site.clone(),
                code: "template_delete_failed".into(),
                message: "WhatsApp template delete did not return success".into(),
            });
        }
        Ok(())
    }
}

pub(super) fn validate_template_query_name(name: &str) -> Result<(), String> {
    crate::whatsapp::validate_template_name(name)
}

pub(super) fn validate_template_status_filter(status: &str) -> Result<(), String> {
    match status {
        "APPROVED" | "PENDING" | "REJECTED" | "PAUSED" | "DISABLED" | "IN_APPEAL"
        | "PENDING_DELETION" | "DELETED" | "LIMIT_EXCEEDED" | "ARCHIVED" => Ok(()),
        _ => Err("template_status_invalid".into()),
    }
}

pub(super) fn parse_template_record(
    value: &Value,
    missing: &str,
) -> Result<WhatsAppTemplateRecord, Error> {
    let quality = value
        .get("quality_score")
        .and_then(|q| q.get("score"))
        .and_then(value_string)
        .or_else(|| value.get("quality").and_then(value_string));
    let id = value
        .get("id")
        .and_then(value_string)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::Platform {
            site: Site::new(SITE),
            code: missing.into(),
            message: "WhatsApp template response had no id".into(),
        })?;
    Ok(WhatsAppTemplateRecord {
        id,
        name: value.get("name").and_then(value_string),
        language: value.get("language").and_then(value_string),
        status: value.get("status").and_then(value_string),
        category: value.get("category").and_then(value_string),
        quality,
    })
}

pub(super) fn template_draft_payload(draft: &WhatsAppTemplateDraft) -> Value {
    use crate::whatsapp::{TemplateCreateButton, TemplateCreateComponent};
    let components: Vec<Value> = draft
        .components
        .iter()
        .map(|component| match component {
            TemplateCreateComponent::Header {
                format,
                text,
                example_handle,
            } => {
                let mut c = json!({ "type": "HEADER", "format": format });
                if let Some(text) = text {
                    c["text"] = json!(text);
                }
                if let Some(handle) = example_handle {
                    c["example"] = json!({ "header_handle": [handle] });
                }
                c
            }
            TemplateCreateComponent::Body {
                text,
                example,
                named_example,
            } => {
                let mut c = json!({ "type": "BODY", "text": text });
                if !named_example.is_empty() {
                    c["example"] = json!({
                        "body_text_named_params": named_example.iter().map(|p| json!({
                            "param_name": p.parameter_name,
                            "example": p.text,
                        })).collect::<Vec<_>>(),
                    });
                } else if !example.is_empty() {
                    c["example"] = json!({ "body_text": [example] });
                }
                c
            }
            TemplateCreateComponent::Footer { text } => {
                json!({ "type": "FOOTER", "text": text })
            }
            TemplateCreateComponent::Buttons { buttons } => json!({
                "type": "BUTTONS",
                "buttons": buttons.iter().map(|b| match b {
                    TemplateCreateButton::QuickReply { text } => json!({
                        "type": "QUICK_REPLY",
                        "text": text,
                    }),
                    TemplateCreateButton::Url { text, url } => json!({
                        "type": "URL",
                        "text": text,
                        "url": url,
                    }),
                    TemplateCreateButton::PhoneNumber { text, phone_number } => json!({
                        "type": "PHONE_NUMBER",
                        "text": text,
                        "phone_number": phone_number,
                    }),
                    TemplateCreateButton::CopyCode { example } => json!({
                        "type": "COPY_CODE",
                        "example": example,
                    }),
                }).collect::<Vec<_>>(),
            }),
        })
        .collect();
    json!({
        "name": draft.name,
        "language": draft.language,
        "category": draft.category.to_uppercase(),
        "parameter_format": draft.parameter_format.as_str(),
        "components": components,
    })
}
