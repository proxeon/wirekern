//! WABA, phone, health, subscribe, register, PIN, and system-user reads.
use crate::error::Error;
use crate::facets::WhatsAppAccount;
use crate::types::{AccountCreds, AppConfig, Deadline, Site};
use crate::whatsapp::{
    WhatsAppPageQuery, WhatsAppPhoneNumber, WhatsAppPhoneNumberList, WhatsAppSystemUser,
    WhatsAppSystemUserList, WhatsAppWaba, WhatsAppWabaList,
};
use async_trait::async_trait;
use serde_json::{json, Value};

use super::graph::{
    access_token, extra_digits, graph_after, graph_page_url, page_query_error, phone_number_id,
    read_json, value_string, waba_id,
};
use super::{WhatsAppCloud, SITE};

#[async_trait]
impl WhatsAppAccount for WhatsAppCloud {
    async fn list_wabas(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        query: &WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<WhatsAppWabaList, Error> {
        page_query_error(query, &self.site)?;
        let token = access_token(creds)?;
        if let Some(business_id) = extra_digits(app, "business_id") {
            let url = graph_page_url(
                format!(
                    "{}/{business_id}/owned_whatsapp_business_accounts?fields=id,name",
                    self.base
                ),
                query,
            );
            let response = self
                .http
                .send(self.http.get(&url).bearer_auth(token), deadline, &self.site)
                .await?;
            let body = read_json(response, &self.site).await?;
            return parse_waba_list(&body);
        }
        // A configured single WABA is a node read, not an edge. Meta cannot
        // return a next cursor here, so reject one rather than silently
        // pretending that a caller paginated it.
        if query.after.is_some() {
            return Err(Error::InvalidQuery {
                site: self.site.clone(),
                reason: "waba_cursor_requires_business_id".into(),
            });
        }
        let waba = waba_id(app)?;
        let response = self
            .http
            .send(
                self.http
                    .get(&format!("{}/{waba}?fields=id,name", self.base))
                    .bearer_auth(token),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        Ok(WhatsAppWabaList {
            wabas: vec![parse_waba(&body)?],
            after: None,
        })
    }

    async fn list_phone_numbers(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        query: &WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<WhatsAppPhoneNumberList, Error> {
        page_query_error(query, &self.site)?;
        let waba = waba_id(app)?;
        let token = access_token(creds)?;
        let url = graph_page_url(
            format!(
                "{}/{waba}/phone_numbers?fields=id,display_phone_number,verified_name,quality_rating,messaging_limit_tier,code_verification_status",
                self.base
            ),
            query,
        );
        let response = self
            .http
            .send(self.http.get(&url).bearer_auth(token), deadline, &self.site)
            .await?;
        let body = read_json(response, &self.site).await?;
        let rows = body
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "phone_list_invalid".into(),
                message: "WhatsApp phone list returned no data array".into(),
            })?;
        Ok(WhatsAppPhoneNumberList {
            phone_numbers: rows
                .iter()
                .map(parse_phone_number)
                .collect::<Result<_, _>>()?,
            after: graph_after(&body),
        })
    }

    async fn phone_health(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<WhatsAppPhoneNumber, Error> {
        let phone = phone_number_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .get(&format!(
                        "{}/{phone}?fields=id,display_phone_number,verified_name,quality_rating,messaging_limit_tier,code_verification_status",
                        self.base
                    ))
                    .bearer_auth(token),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        parse_phone_number(&body)
    }

    async fn subscribe_apps(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        deadline: Deadline,
    ) -> Result<(), Error> {
        let waba = waba_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .post(&format!("{}/{waba}/subscribed_apps", self.base))
                    .bearer_auth(token)
                    .json(&json!({})),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        if body.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(Error::Platform {
                site: self.site.clone(),
                code: "subscribe_failed".into(),
                message: "WhatsApp subscribed_apps did not return success".into(),
            });
        }
        Ok(())
    }

    async fn register_phone(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        pin: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        crate::whatsapp::validate_two_step_pin(pin).map_err(|reason| Error::InvalidPost {
            site: self.site.clone(),
            reason,
            limit: None,
        })?;
        let phone = phone_number_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .post(&format!("{}/{phone}/register", self.base))
                    .bearer_auth(token)
                    .json(&json!({
                        "messaging_product": "whatsapp",
                        "pin": pin,
                    })),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        if body.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(Error::Platform {
                site: self.site.clone(),
                code: "register_failed".into(),
                message: "WhatsApp phone register did not return success".into(),
            });
        }
        Ok(())
    }

    async fn set_two_step_pin(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        pin: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        crate::whatsapp::validate_two_step_pin(pin).map_err(|reason| Error::InvalidPost {
            site: self.site.clone(),
            reason,
            limit: None,
        })?;
        let phone = phone_number_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .post(&format!("{}/{phone}", self.base))
                    .bearer_auth(token)
                    .json(&json!({ "pin": pin })),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        if body.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(Error::Platform {
                site: self.site.clone(),
                code: "two_step_failed".into(),
                message: "WhatsApp two-step PIN did not return success".into(),
            });
        }
        Ok(())
    }

    async fn list_system_users(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        query: &WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<WhatsAppSystemUserList, Error> {
        page_query_error(query, &self.site)?;
        let business_id = extra_digits(app, "business_id").ok_or_else(|| Error::Auth {
            site: self.site.clone(),
            reason: "missing_business_id".into(),
        })?;
        let token = access_token(creds)?;
        let url = graph_page_url(
            format!(
                "{}/{business_id}/system_users?fields=id,name,role",
                self.base
            ),
            query,
        );
        let response = self
            .http
            .send(self.http.get(&url).bearer_auth(token), deadline, &self.site)
            .await?;
        let body = read_json(response, &self.site).await?;
        let rows = body
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "system_user_list_invalid".into(),
                message: "System user list returned no data array".into(),
            })?;
        let users = rows
            .iter()
            .map(|v| {
                let id = v
                    .get("id")
                    .and_then(value_string)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| Error::Platform {
                        site: self.site.clone(),
                        code: "missing_system_user_id".into(),
                        message: "System user row had no id".into(),
                    })?;
                Ok::<WhatsAppSystemUser, Error>(WhatsAppSystemUser {
                    id,
                    name: v.get("name").and_then(value_string),
                    role: v.get("role").and_then(value_string),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(WhatsAppSystemUserList {
            users,
            after: graph_after(&body),
        })
    }
}

pub(super) fn parse_waba_list(body: &Value) -> Result<WhatsAppWabaList, Error> {
    let rows = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Platform {
            site: Site::new(SITE),
            code: "waba_list_invalid".into(),
            message: "WhatsApp WABA list returned no data array".into(),
        })?;
    Ok(WhatsAppWabaList {
        wabas: rows.iter().map(parse_waba).collect::<Result<_, _>>()?,
        after: graph_after(body),
    })
}

pub(super) fn parse_waba(value: &Value) -> Result<WhatsAppWaba, Error> {
    let id = value
        .get("id")
        .and_then(value_string)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::Platform {
            site: Site::new(SITE),
            code: "missing_waba_id".into(),
            message: "WhatsApp WABA response had no id".into(),
        })?;
    Ok(WhatsAppWaba {
        id,
        name: value.get("name").and_then(value_string),
    })
}

pub(super) fn parse_phone_number(value: &Value) -> Result<WhatsAppPhoneNumber, Error> {
    let id = value
        .get("id")
        .and_then(value_string)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::Platform {
            site: Site::new(SITE),
            code: "missing_phone_number_id".into(),
            message: "WhatsApp phone response had no id".into(),
        })?;
    Ok(WhatsAppPhoneNumber {
        id,
        display_phone_number: value.get("display_phone_number").and_then(value_string),
        verified_name: value.get("verified_name").and_then(value_string),
        quality_rating: value.get("quality_rating").and_then(value_string),
        messaging_limit_tier: value.get("messaging_limit_tier").and_then(value_string),
        code_verification_status: value.get("code_verification_status").and_then(value_string),
    })
}
