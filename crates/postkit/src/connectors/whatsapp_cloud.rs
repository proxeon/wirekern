//! WhatsApp Cloud API connector.
//!
//! Cloud API messaging is deliberately not routed through generic social
//! publishing. A reply's context, an approved template, a specific phone
//! number and the later webhook delivery state are all part of the contract.

use crate::error::Error;
use crate::facets::{
    WhatsAppAccount, WhatsAppAssets, WhatsAppFlows, WhatsAppSender, WhatsAppTemplates,
};
use crate::http::Http;
use crate::publisher::{AuthKind, Publisher};
use crate::registry::Connector;
use crate::types::{AccountCreds, AppConfig, Capability, Deadline, Intent, Outcome, Site, WhoAmI};
use crate::whatsapp::{
    validate_media_upload, DeliveryConversation, DeliveryError, DeliveryPricing, DeliveryStatus,
    DeliveryStatusKind, InboundContact, InboundInteractive, InboundLocation, InboundMedia,
    InboundMessage, InboundMessages, InboundOrder, InboundReaction, InboundReferral,
    InboundUnsupported, RecipientType, WebhookParseOptions, WhatsAppFlowDraft, WhatsAppFlowList,
    WhatsAppFlowRecord, WhatsAppMediaMeta, WhatsAppMediaUpload, WhatsAppMessage, WhatsAppPageQuery,
    WhatsAppPhoneNumber, WhatsAppPhoneNumberList, WhatsAppSendRequest, WhatsAppSystemUser,
    WhatsAppSystemUserList, WhatsAppTemplateDraft, WhatsAppTemplateList, WhatsAppTemplateQuery,
    WhatsAppTemplateRecord, WhatsAppUploadedMedia, WhatsAppWaba, WhatsAppWabaList,
};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

pub const GRAPH_HOST: &str = "graph.facebook.com";
/// Pinning makes a Meta version update a reviewed wire change rather than an
/// incidental dependency upgrade.
pub const GRAPH_VERSION: &str = "v26.0";
pub const SITE: &str = "whatsapp_cloud";
pub const WEBHOOK_SIGNATURE_PREFIX: &str = "sha256=";
pub const MAX_WEBHOOK_BYTES: usize = 1_048_576;

type HmacSha256 = Hmac<Sha256>;

pub struct WhatsAppCloud {
    http: Http,
    site: Site,
    base: String,
}

impl WhatsAppCloud {
    pub fn new() -> Result<Self, Error> {
        Self::with_base(format!("https://{GRAPH_HOST}/{GRAPH_VERSION}"))
    }

    /// Local mock helper. The production constructor remains pinned to the
    /// versioned Graph API base above.
    pub fn with_base(base: impl Into<String>) -> Result<Self, Error> {
        Ok(Self {
            http: Http::new()?,
            site: Site::new(SITE),
            base: base.into().trim_end_matches('/').to_string(),
        })
    }

    pub fn connector(self) -> Connector {
        let this = std::sync::Arc::new(self);
        Connector::from_publisher(this.clone())
            .whatsapp(this.clone())
            .whatsapp_assets(this.clone())
            .whatsapp_templates(this.clone())
            .whatsapp_flows(this.clone())
            .whatsapp_account(this)
    }

    /// Verify and parse raw `messages` webhook bytes. This is intentionally a
    /// pure adapter for an application's HTTP endpoint: Postkit does not run
    /// a public listener, acknowledge Meta's delivery, or persist an inbox.
    /// It returns delivery callbacks in Meta's payload order but does not
    /// deduplicate or infer a final state from potentially reordered events.
    pub fn parse_signed_webhook(
        app: &AppConfig,
        signature: &str,
        raw_body: &[u8],
    ) -> Result<InboundMessages, Error> {
        Self::parse_signed_webhook_with(app, signature, raw_body, WebhookParseOptions::default())
    }

    /// GET `hub.verify_token` handshake. Returns the raw `hub.challenge`
    /// string Meta expects as the HTTP body (never JSON).
    pub fn verify_callback_challenge(
        app: &AppConfig,
        mode: &str,
        token: &str,
        challenge: &str,
    ) -> Result<String, Error> {
        let stored = app
            .extra
            .get("verify_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| webhook_error("webhook_verify_token_missing"))?;
        crate::whatsapp_ops::verify_webhook_challenge(stored, mode, token, challenge)
            .map_err(|reason| webhook_error(&reason))
    }

    pub fn parse_signed_webhook_with(
        app: &AppConfig,
        signature: &str,
        raw_body: &[u8],
        options: WebhookParseOptions,
    ) -> Result<InboundMessages, Error> {
        if raw_body.len() > MAX_WEBHOOK_BYTES {
            return Err(webhook_error("webhook_body_too_large"));
        }
        let secret = webhook_secret(app)?;
        let supplied = decode_signature(signature)
            .ok_or_else(|| webhook_error("webhook_signature_invalid"))?;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .expect("HMAC-SHA256 accepts every key length");
        mac.update(raw_body);
        // `verify_slice` performs a constant-time comparison. Parsing an
        // unsigned payload first would turn this CLI helper into a convenient
        // source of untrusted personal-data output.
        if mac.verify_slice(&supplied).is_err() {
            return Err(webhook_error("webhook_signature_invalid"));
        }

        let body: Value =
            serde_json::from_slice(raw_body).map_err(|_| webhook_error("webhook_json_invalid"))?;
        if body.get("object").and_then(Value::as_str) != Some("whatsapp_business_account") {
            return Err(webhook_error("webhook_object_invalid"));
        }
        let expected_phone_ids = configured_phone_ids(app)?;
        let entries = body
            .get("entry")
            .and_then(Value::as_array)
            .ok_or_else(|| webhook_error("webhook_entry_invalid"))?;
        let mut messages = Vec::new();
        let mut statuses = Vec::new();
        for entry in entries {
            let Some(changes) = entry.get("changes").and_then(Value::as_array) else {
                return Err(webhook_error("webhook_changes_invalid"));
            };
            for change in changes {
                // Meta batches inbound messages and outbound delivery status
                // callbacks on this same field. The configured phone check
                // applies before either kind is returned to the caller.
                if change.get("field").and_then(Value::as_str) != Some("messages") {
                    continue;
                }
                let value = change
                    .get("value")
                    .and_then(Value::as_object)
                    .ok_or_else(|| webhook_error("webhook_change_invalid"))?;
                let phone_number_id = value
                    .get("metadata")
                    .and_then(|metadata| metadata.get("phone_number_id"))
                    .and_then(value_string)
                    .ok_or_else(|| webhook_error("webhook_phone_number_id_missing"))?;
                // A valid app signature proves Meta sent this callback, not
                // that it belongs to the Postkit-configured sender. Refusing
                // a mismatched number prevents accidental cross-number data
                // handling in a multi-WABA webhook endpoint.
                if !expected_phone_ids.iter().any(|id| id == &phone_number_id) {
                    return Err(webhook_error("webhook_phone_number_mismatch"));
                }
                if let Some(delivery_statuses) = value.get("statuses") {
                    let delivery_statuses = delivery_statuses
                        .as_array()
                        .ok_or_else(|| webhook_error("webhook_statuses_invalid"))?;
                    for status in delivery_statuses {
                        statuses.push(delivery_status(status, options.include_status_extras)?);
                    }
                }
                let Some(inbound) = value.get("messages").and_then(Value::as_array) else {
                    continue; // a valid status-only change
                };
                for message in inbound {
                    messages.push(inbound_message(message)?);
                }
            }
        }
        Ok(InboundMessages {
            site: Site::new(SITE),
            messages,
            statuses,
        })
    }
}

#[async_trait]
impl Publisher for WhatsAppCloud {
    fn site(&self) -> &Site {
        &self.site
    }

    fn capabilities(&self) -> &[Capability] {
        // These read capabilities mean verified callback parsing, not a
        // fictional remote inbox/status API. Cloud API delivers both event
        // kinds to the business' configured webhook endpoint.
        &[
            Capability::SendReply,
            Capability::SendText,
            Capability::SendTemplate,
            Capability::SendMedia,
            Capability::SendInteractive,
            Capability::SendLocation,
            Capability::SendContacts,
            Capability::SendReaction,
            Capability::MarkRead,
            Capability::SendTyping,
            Capability::ManageWhatsAppMedia,
            Capability::ReadWhatsAppMedia,
            Capability::ReadTemplates,
            Capability::ManageTemplates,
            Capability::SendCatalog,
            Capability::SendFlow,
            Capability::ReadFlows,
            Capability::ManageFlows,
            Capability::ReadWhatsAppAccount,
            Capability::ManageWhatsAppPhone,
            Capability::ReadWebhookMessages,
            Capability::ReadWebhookStatuses,
        ]
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::StaticToken
    }

    async fn publish(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _intent: Intent,
        _deadline: Deadline,
    ) -> Result<Outcome, Error> {
        // Defend direct library calls too: private messages cannot be made by
        // supplying `post whatsapp_cloud --param to=…` to a generic surface.
        Err(Error::InvalidPost {
            site: self.site.clone(),
            reason: "use_whatsapp_command".into(),
            limit: None,
        })
    }

    async fn whoami(&self, app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error> {
        let phone_number_id = phone_number_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .get(&format!(
                        "{}/{phone_number_id}?fields=id,display_phone_number,verified_name",
                        self.base
                    ))
                    .bearer_auth(token),
                Deadline::from_secs(30),
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        let id = body
            .get("id")
            .and_then(value_string)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "missing_phone_number_id".into(),
                message: "WhatsApp phone lookup returned no id".into(),
            })?;
        Ok(WhoAmI {
            site: self.site.clone(),
            id,
            handle: body
                .get("verified_name")
                .and_then(value_string)
                .or_else(|| body.get("display_phone_number").and_then(value_string)),
        })
    }
}

#[async_trait]
impl WhatsAppSender for WhatsAppCloud {
    async fn send_whatsapp(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        request: &WhatsAppSendRequest,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        request.validate().map_err(|reason| Error::InvalidPost {
            site: self.site.clone(),
            reason,
            limit: None,
        })?;
        let phone_number_id = phone_number_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .post(&format!("{}/{phone_number_id}/messages", self.base))
                    .bearer_auth(token)
                    .json(&send_payload_for(&request.message, request.recipient_type)),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        // Mark-as-read and typing return `{success: true}` (Messages API
        // MarkMessageResponsePayload). Requiring a wamid would treat a
        // successful ack as a platform error.
        if request.message.is_status_ack() {
            let ok = body.get("success").and_then(Value::as_bool) == Some(true);
            if !ok {
                return Err(Error::Platform {
                    site: self.site.clone(),
                    code: "missing_success".into(),
                    message: "WhatsApp read/typing acknowledgement was not success".into(),
                });
            }
            let id = match &request.message {
                WhatsAppMessage::MarkRead { message_id }
                | WhatsAppMessage::Typing { message_id } => message_id.clone(),
                _ => unreachable!("is_status_ack"),
            };
            return Ok(Outcome {
                site: self.site.clone(),
                id: Some(id),
                url: None,
                limits: None,
            });
        }
        let id = body
            .get("messages")
            .and_then(Value::as_array)
            .and_then(|messages| messages.first())
            .and_then(|message| message.get("id"))
            .and_then(value_string)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "missing_message_id".into(),
                message: "WhatsApp accepted the request without a message id".into(),
            })?;
        Ok(Outcome {
            site: self.site.clone(),
            id: Some(id),
            url: None,
            limits: None,
        })
    }
}

#[async_trait]
impl WhatsAppAssets for WhatsAppCloud {
    async fn upload_media(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        upload: &WhatsAppMediaUpload,
        deadline: Deadline,
    ) -> Result<WhatsAppUploadedMedia, Error> {
        validate_media_upload(upload).map_err(|reason| Error::InvalidPost {
            site: self.site.clone(),
            reason,
            limit: None,
        })?;
        let phone_number_id = phone_number_id(app)?;
        let token = access_token(creds)?;
        let part = reqwest::multipart::Part::bytes(upload.bytes.clone())
            .file_name(upload.filename.clone())
            .mime_str(&upload.mime_type)
            .map_err(|_| Error::InvalidPost {
                site: self.site.clone(),
                reason: "media_mime_unsupported".into(),
                limit: None,
            })?;
        let form = reqwest::multipart::Form::new()
            .text("messaging_product", "whatsapp")
            .text("type", upload.mime_type.clone())
            .part("file", part);
        let response = self
            .http
            .send(
                self.http
                    .post(&format!("{}/{phone_number_id}/media", self.base))
                    .bearer_auth(token)
                    .multipart(form),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        let id = body
            .get("id")
            .and_then(value_string)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "missing_media_id".into(),
                message: "WhatsApp media upload returned no id".into(),
            })?;
        Ok(WhatsAppUploadedMedia { id })
    }

    async fn media_metadata(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<WhatsAppMediaMeta, Error> {
        if media_id.is_empty() {
            return Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: "media_id_empty".into(),
                limit: None,
            });
        }
        let phone = phone_number_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .get(&format!("{}/{media_id}?phone_number_id={phone}", self.base))
                    .bearer_auth(token),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        Ok(WhatsAppMediaMeta {
            id: body
                .get("id")
                .and_then(value_string)
                .unwrap_or_else(|| media_id.to_string()),
            mime_type: body.get("mime_type").and_then(value_string),
            sha256: body.get("sha256").and_then(value_string),
            file_size: body.get("file_size").and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            }),
            url: body.get("url").and_then(value_string),
        })
    }

    async fn download_media(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<Vec<u8>, Error> {
        let meta = self.media_metadata(app, creds, media_id, deadline).await?;
        let url = meta
            .url
            .filter(|u| media_download_url_allowed(u))
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "missing_media_url".into(),
                message: "WhatsApp media metadata had no https url".into(),
            })?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(self.http.get(&url).bearer_auth(token), deadline, &self.site)
            .await?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let text = response.text().await.unwrap_or_default();
            return Err(map_graph_error(status, &text));
        }
        Ok(response
            .bytes()
            .await
            .map_err(|_| Error::request_failed(&self.site))?
            .to_vec())
    }

    async fn delete_media(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        if media_id.is_empty() {
            return Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: "media_id_empty".into(),
                limit: None,
            });
        }
        let phone = phone_number_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .delete(&format!("{}/{media_id}?phone_number_id={phone}", self.base))
                    .bearer_auth(token),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        if body.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(Error::Platform {
                site: self.site.clone(),
                code: "media_delete_failed".into(),
                message: "WhatsApp media delete did not return success".into(),
            });
        }
        Ok(())
    }
}

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

fn extra_digits(app: &AppConfig, key: &str) -> Option<String> {
    app.extra
        .get(key)
        .and_then(value_string)
        .filter(|id| !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_digit()))
}

/// Graph returns the next page cursor under `paging.cursors.after`. Expose
/// only that opaque continuation token, never the `paging.next` URL, which
/// is an implementation detail that can carry unrelated query parameters.
fn graph_after(body: &Value) -> Option<String> {
    body.pointer("/paging/cursors/after")
        .and_then(value_string)
        .filter(|cursor| !cursor.is_empty() && cursor.len() <= 1_024)
}

fn page_query_error(query: &WhatsAppPageQuery, site: &Site) -> Result<(), Error> {
    query.validate().map_err(|reason| Error::InvalidQuery {
        site: site.clone(),
        reason,
    })
}

/// Append one bounded opaque page query to an existing Graph edge URL. Both
/// values are percent encoded; cursors are data, never fragments of a URL.
fn graph_page_url(mut url: String, query: &WhatsAppPageQuery) -> String {
    if let Some(limit) = query.limit {
        url.push_str(&format!("&limit={limit}"));
    }
    if let Some(after) = &query.after {
        url.push_str("&after=");
        url.push_str(&percent_encode(after));
    }
    url
}

/// Graph query values can contain cursor punctuation. Encode them locally
/// rather than trusting a caller-provided cursor/name to stay inside one
/// parameter (the `fields` portion above is Postkit-owned static text).
fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn parse_waba_list(body: &Value) -> Result<WhatsAppWabaList, Error> {
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

fn parse_waba(value: &Value) -> Result<WhatsAppWaba, Error> {
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

fn parse_phone_number(value: &Value) -> Result<WhatsAppPhoneNumber, Error> {
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

fn parse_flow_record(value: &Value, missing: &str) -> Result<WhatsAppFlowRecord, Error> {
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

/// The exact limited JSON grammar Postkit allows on the message endpoint.
/// Keeping it public makes wire tests and embedding callers inspectable
/// without permitting arbitrary unreviewed JSON components.
fn media_download_url_allowed(url: &str) -> bool {
    url.starts_with("https://")
        || url.starts_with("http://127.0.0.1:")
        || url.starts_with("http://localhost:")
}

fn wire_recipient(to: &str) -> String {
    // send_whatsapp validates first. This helper must not panic if a test
    // or inspector calls send_payload on an unvalidated message.
    crate::whatsapp::normalize_recipient(to).unwrap_or_else(|_| to.to_string())
}

pub fn send_payload(message: &WhatsAppMessage) -> Value {
    send_payload_for(message, RecipientType::Individual)
}

pub fn send_payload_for(message: &WhatsAppMessage, recipient_type: RecipientType) -> Value {
    let mut payload = match message {
        WhatsAppMessage::Reply {
            to,
            reply_to_message_id,
            text,
            preview_url,
        } => json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": wire_recipient(to),
            "context": { "message_id": reply_to_message_id },
            "type": "text",
            "text": { "body": text, "preview_url": preview_url },
        }),
        WhatsAppMessage::Text {
            to,
            text,
            preview_url,
        } => json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": wire_recipient(to),
            "type": "text",
            "text": { "body": text, "preview_url": preview_url },
        }),
        WhatsAppMessage::Template {
            to,
            name,
            language,
            body_parameters,
            named_body_parameters,
            header,
            buttons,
            limited_time_offer,
        } => {
            let mut template = json!({
                "name": name,
                "language": { "code": language },
            });
            let mut components = Vec::new();
            if let Some(header) = header {
                components.push(template_header_component(header));
            }
            if !body_parameters.is_empty() {
                components.push(json!({
                    "type": "body",
                    "parameters": body_parameters.iter().map(|text| json!({
                        "type": "text",
                        "text": text,
                    })).collect::<Vec<_>>(),
                }));
            } else if !named_body_parameters.is_empty() {
                components.push(json!({
                    "type": "body",
                    "parameters": named_body_parameters.iter().map(|p| json!({
                        "type": "text",
                        "parameter_name": p.parameter_name,
                        "text": p.text,
                    })).collect::<Vec<_>>(),
                }));
            }
            if let Some(offer) = limited_time_offer {
                components.push(json!({
                    "type": "limited_time_offer",
                    "parameters": [{
                        "type": "limited_time_offer",
                        "limited_time_offer": {
                            "expiration_time_ms": offer.expiration_time_ms,
                        },
                    }],
                }));
            }
            for button in buttons {
                components.push(template_button_component(button));
            }
            if !components.is_empty() {
                template["components"] = json!(components);
            }
            json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "to": wire_recipient(to),
                "type": "template",
                "template": template,
            })
        }
        WhatsAppMessage::Image {
            to,
            media,
            caption,
            reply_to_message_id,
        } => media_payload(
            to,
            "image",
            media,
            caption.as_deref(),
            None,
            reply_to_message_id.as_deref(),
        ),
        WhatsAppMessage::Document {
            to,
            media,
            caption,
            filename,
            reply_to_message_id,
        } => media_payload(
            to,
            "document",
            media,
            caption.as_deref(),
            filename.as_deref(),
            reply_to_message_id.as_deref(),
        ),
        WhatsAppMessage::Audio {
            to,
            media,
            reply_to_message_id,
        } => media_payload(
            to,
            "audio",
            media,
            None,
            None,
            reply_to_message_id.as_deref(),
        ),
        WhatsAppMessage::Video {
            to,
            media,
            caption,
            reply_to_message_id,
        } => media_payload(
            to,
            "video",
            media,
            caption.as_deref(),
            None,
            reply_to_message_id.as_deref(),
        ),
        WhatsAppMessage::Sticker {
            to,
            media,
            reply_to_message_id,
        } => media_payload(
            to,
            "sticker",
            media,
            None,
            None,
            reply_to_message_id.as_deref(),
        ),
        WhatsAppMessage::Buttons {
            to,
            body,
            buttons,
            header,
            footer,
            reply_to_message_id,
        } => {
            let interactive = json!({
                "type": "button",
                "body": { "text": body },
                "action": {
                    "buttons": buttons.iter().map(|b| json!({
                        "type": "reply",
                        "reply": { "id": b.id, "title": b.title },
                    })).collect::<Vec<_>>(),
                },
            });
            interactive_payload(
                to,
                interactive,
                header.as_deref(),
                footer.as_deref(),
                reply_to_message_id.as_deref(),
            )
        }
        WhatsAppMessage::List {
            to,
            body,
            button,
            sections,
            header,
            footer,
            reply_to_message_id,
        } => {
            let interactive = json!({
                "type": "list",
                "body": { "text": body },
                "action": {
                    "button": button,
                    "sections": sections,
                },
            });
            interactive_payload(
                to,
                interactive,
                header.as_deref(),
                footer.as_deref(),
                reply_to_message_id.as_deref(),
            )
        }
        WhatsAppMessage::CtaUrl {
            to,
            body,
            display_text,
            url,
            header,
            footer,
            reply_to_message_id,
        } => {
            let interactive = json!({
                "type": "cta_url",
                "body": { "text": body },
                "action": {
                    "name": "cta_url",
                    "parameters": { "display_text": display_text, "url": url },
                },
            });
            interactive_payload(
                to,
                interactive,
                header.as_deref(),
                footer.as_deref(),
                reply_to_message_id.as_deref(),
            )
        }
        WhatsAppMessage::LocationRequest {
            to,
            body,
            reply_to_message_id,
        } => {
            let interactive = json!({
                "type": "location_request_message",
                "body": { "text": body },
                "action": { "name": "send_location" },
            });
            interactive_payload(to, interactive, None, None, reply_to_message_id.as_deref())
        }
        WhatsAppMessage::VoiceCall {
            to,
            body,
            display_text,
            ttl_minutes,
            payload,
            reply_to_message_id,
        } => {
            let mut parameters =
                json!({ "display_text": display_text.as_deref().unwrap_or("Call Now") });
            if let Some(ttl) = ttl_minutes {
                parameters["ttl_minutes"] = json!(ttl);
            }
            if let Some(payload) = payload {
                parameters["payload"] = json!(payload);
            }
            let interactive = json!({
                "type": "voice_call",
                "body": { "text": body },
                "action": { "name": "voice_call", "parameters": parameters },
            });
            interactive_payload(to, interactive, None, None, reply_to_message_id.as_deref())
        }
        WhatsAppMessage::Location {
            to,
            latitude,
            longitude,
            name,
            address,
            reply_to_message_id,
        } => {
            let mut location = json!({ "latitude": latitude, "longitude": longitude });
            if let Some(name) = name {
                location["name"] = json!(name);
            }
            if let Some(address) = address {
                location["address"] = json!(address);
            }
            let mut payload = json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "to": wire_recipient(to),
                "type": "location",
                "location": location,
            });
            if let Some(id) = reply_to_message_id {
                payload["context"] = json!({ "message_id": id });
            }
            payload
        }
        WhatsAppMessage::Contacts {
            to,
            contacts,
            reply_to_message_id,
        } => {
            let contacts: Vec<_> = contacts
                .iter()
                .map(|c| {
                    let mut o = json!({ "name": { "formatted_name": c.formatted_name } });
                    if !c.phones.is_empty() {
                        o["phones"] = json!(c
                            .phones
                            .iter()
                            .map(|p| json!({ "phone": p }))
                            .collect::<Vec<_>>());
                    }
                    o
                })
                .collect();
            let mut payload = json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "to": wire_recipient(to),
                "type": "contacts",
                "contacts": contacts,
            });
            if let Some(id) = reply_to_message_id {
                payload["context"] = json!({ "message_id": id });
            }
            payload
        }
        WhatsAppMessage::AddressRequest {
            to,
            body,
            country,
            reply_to_message_id,
        } => {
            let interactive = json!({
                "type": "address_message",
                "body": { "text": body },
                "action": {
                    "name": "address_message",
                    "parameters": { "country": country.to_uppercase() },
                },
            });
            interactive_payload(to, interactive, None, None, reply_to_message_id.as_deref())
        }
        WhatsAppMessage::Reaction {
            to,
            message_id,
            emoji,
        } => json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": wire_recipient(to),
            "type": "reaction",
            "reaction": { "message_id": message_id, "emoji": emoji },
        }),
        // No `to` / `recipient_type`: these are inbound-wamid acks, not
        // customer-addressed messages.
        WhatsAppMessage::Catalog {
            to,
            body,
            thumbnail_product_retailer_id,
            footer,
            reply_to_message_id,
        } => {
            let mut action = json!({ "name": "catalog_message" });
            if let Some(id) = thumbnail_product_retailer_id {
                action["parameters"] = json!({ "thumbnail_product_retailer_id": id });
            }
            let interactive = json!({
                "type": "catalog_message",
                "body": { "text": body },
                "action": action,
            });
            interactive_payload(
                to,
                interactive,
                None,
                footer.as_deref(),
                reply_to_message_id.as_deref(),
            )
        }
        WhatsAppMessage::Product {
            to,
            catalog_id,
            product_retailer_id,
            body,
            footer,
            reply_to_message_id,
        } => {
            let mut interactive = json!({
                "type": "product",
                "action": {
                    "catalog_id": catalog_id,
                    "product_retailer_id": product_retailer_id,
                },
            });
            if let Some(body) = body {
                interactive["body"] = json!({ "text": body });
            }
            interactive_payload(
                to,
                interactive,
                None,
                footer.as_deref(),
                reply_to_message_id.as_deref(),
            )
        }
        WhatsAppMessage::ProductList {
            to,
            catalog_id,
            header,
            body,
            sections,
            footer,
            reply_to_message_id,
        } => {
            let interactive = json!({
                "type": "product_list",
                "body": { "text": body },
                "action": {
                    "catalog_id": catalog_id,
                    "sections": sections.iter().map(|s| {
                        let mut o = json!({
                            "product_items": s.product_retailer_ids.iter().map(|id| json!({
                                "product_retailer_id": id,
                            })).collect::<Vec<_>>(),
                        });
                        if let Some(title) = &s.title {
                            o["title"] = json!(title);
                        }
                        o
                    }).collect::<Vec<_>>(),
                },
            });
            interactive_payload(
                to,
                interactive,
                Some(header),
                footer.as_deref(),
                reply_to_message_id.as_deref(),
            )
        }
        WhatsAppMessage::OrderStatus {
            to,
            body,
            reference_id,
            status,
            description,
            reply_to_message_id,
        } => {
            let mut order = json!({ "status": status });
            if let Some(description) = description {
                order["description"] = json!(description);
            }
            let interactive = json!({
                "type": "order_status",
                "body": { "text": body },
                "action": {
                    "name": "review_order",
                    "parameters": {
                        "reference_id": reference_id,
                        "order": order,
                    },
                },
            });
            interactive_payload(to, interactive, None, None, reply_to_message_id.as_deref())
        }
        WhatsAppMessage::Flow {
            to,
            body,
            flow_cta,
            flow_id,
            flow_name,
            header,
            footer,
            flow_token,
            screen,
            reply_to_message_id,
        } => {
            let mut parameters = json!({
                "flow_message_version": "3",
                "flow_cta": flow_cta,
            });
            if let Some(id) = flow_id {
                parameters["flow_id"] = json!(id);
            }
            if let Some(name) = flow_name {
                parameters["flow_name"] = json!(name);
            }
            if let Some(token) = flow_token {
                parameters["flow_token"] = json!(token);
            }
            if let Some(screen) = screen {
                parameters["flow_action"] = json!("navigate");
                parameters["flow_action_payload"] = json!({ "screen": screen });
            }
            let interactive = json!({
                "type": "flow",
                "body": { "text": body },
                "action": { "name": "flow", "parameters": parameters },
            });
            interactive_payload(
                to,
                interactive,
                header.as_deref(),
                footer.as_deref(),
                reply_to_message_id.as_deref(),
            )
        }
        WhatsAppMessage::MarkRead { message_id } => json!({
            "messaging_product": "whatsapp",
            "status": "read",
            "message_id": message_id,
        }),
        WhatsAppMessage::Typing { message_id } => json!({
            "messaging_product": "whatsapp",
            "status": "read",
            "message_id": message_id,
            "typing_indicator": { "type": "text" },
        }),
    };
    if !message.is_status_ack() {
        payload["recipient_type"] = json!(recipient_type.as_str());
    }
    payload
}

fn template_header_component(header: &crate::whatsapp::TemplateHeader) -> Value {
    use crate::whatsapp::TemplateHeader;
    match header {
        TemplateHeader::Text { text } => json!({
            "type": "header",
            "parameters": [{ "type": "text", "text": text }],
        }),
        TemplateHeader::Image { media } => json!({
            "type": "header",
            "parameters": [{ "type": "image", "image": media.to_json() }],
        }),
        TemplateHeader::Video { media } => json!({
            "type": "header",
            "parameters": [{ "type": "video", "video": media.to_json() }],
        }),
        TemplateHeader::Document { media } => json!({
            "type": "header",
            "parameters": [{ "type": "document", "document": media.to_json() }],
        }),
    }
}

fn template_button_component(button: &crate::whatsapp::TemplateButton) -> Value {
    use crate::whatsapp::TemplateButton;
    match button {
        TemplateButton::QuickReply { index, payload } => json!({
            "type": "button",
            "sub_type": "quick_reply",
            "index": index.to_string(),
            "parameters": [{ "type": "payload", "payload": payload }],
        }),
        TemplateButton::Url { index, text } => json!({
            "type": "button",
            "sub_type": "url",
            "index": index.to_string(),
            "parameters": [{ "type": "text", "text": text }],
        }),
        TemplateButton::PhoneNumber { index } => json!({
            "type": "button",
            "sub_type": "phone_number",
            "index": index.to_string(),
        }),
        TemplateButton::CopyCode { index, coupon_code } => json!({
            "type": "button",
            "sub_type": "copy_code",
            "index": index.to_string(),
            "parameters": [{ "type": "coupon_code", "coupon_code": coupon_code }],
        }),
    }
}

fn interactive_payload(
    to: &str,
    mut interactive: Value,
    header: Option<&str>,
    footer: Option<&str>,
    reply_to: Option<&str>,
) -> Value {
    if let Some(header) = header {
        interactive["header"] = json!({ "type": "text", "text": header });
    }
    if let Some(footer) = footer {
        interactive["footer"] = json!({ "text": footer });
    }
    let mut payload = json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": wire_recipient(to),
        "type": "interactive",
        "interactive": interactive,
    });
    if let Some(id) = reply_to {
        payload["context"] = json!({ "message_id": id });
    }
    payload
}

fn media_payload(
    to: &str,
    kind: &str,
    media: &crate::whatsapp::MediaRef,
    caption: Option<&str>,
    filename: Option<&str>,
    reply_to: Option<&str>,
) -> Value {
    let mut asset = media.to_json();
    if let Some(caption) = caption {
        asset["caption"] = json!(caption);
    }
    if let Some(filename) = filename {
        asset["filename"] = json!(filename);
    }
    let mut payload = json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": wire_recipient(to),
        "type": kind,
        kind: asset,
    });
    if let Some(id) = reply_to {
        payload["context"] = json!({ "message_id": id });
    }
    payload
}

fn waba_id(app: &AppConfig) -> Result<String, Error> {
    app.extra
        .get("waba_id")
        .and_then(value_string)
        .filter(|id| {
            !id.is_empty() && id.len() <= 32 && id.bytes().all(|byte| byte.is_ascii_digit())
        })
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "missing_waba_id".into(),
        })
}

fn validate_graph_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 32 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err("template_id_invalid".into());
    }
    Ok(())
}

fn validate_template_query_name(name: &str) -> Result<(), String> {
    crate::whatsapp::validate_template_name(name)
}

fn validate_template_status_filter(status: &str) -> Result<(), String> {
    match status {
        "APPROVED" | "PENDING" | "REJECTED" | "PAUSED" | "DISABLED" | "IN_APPEAL"
        | "PENDING_DELETION" | "DELETED" | "LIMIT_EXCEEDED" | "ARCHIVED" => Ok(()),
        _ => Err("template_status_invalid".into()),
    }
}

fn parse_template_record(value: &Value, missing: &str) -> Result<WhatsAppTemplateRecord, Error> {
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

fn template_draft_payload(draft: &WhatsAppTemplateDraft) -> Value {
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

fn configured_phone_ids(app: &AppConfig) -> Result<Vec<String>, Error> {
    let mut ids = Vec::new();
    if let Ok(primary) = phone_number_id(app) {
        ids.push(primary);
    }
    if let Some(senders) = app.extra.get("senders").and_then(Value::as_array) {
        for sender in senders {
            if let Some(id) = sender
                .get("phone_number_id")
                .and_then(value_string)
                .filter(|id| {
                    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_digit())
                })
            {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    if ids.is_empty() {
        return Err(Error::Auth {
            site: Site::new(SITE),
            reason: "missing_phone_number_id".into(),
        });
    }
    Ok(ids)
}

fn phone_number_id(app: &AppConfig) -> Result<String, Error> {
    app.extra
        .get("phone_number_id")
        .and_then(value_string)
        .filter(|id| {
            !id.is_empty() && id.len() <= 32 && id.bytes().all(|byte| byte.is_ascii_digit())
        })
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "missing_phone_number_id".into(),
        })
}

fn webhook_secret(app: &AppConfig) -> Result<String, Error> {
    app.extra
        .get("app_secret")
        .and_then(Value::as_str)
        .filter(|secret| !secret.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Error::Auth {
            site: Site::new(SITE),
            reason: "missing_webhook_app_secret".into(),
        })
}

fn access_token(creds: &AccountCreds) -> Result<&str, Error> {
    match creds {
        AccountCreds::BotToken { token } if !token.is_empty() => Ok(token),
        _ => Err(Error::Auth {
            site: Site::new(SITE),
            reason: "credential_kind".into(),
        }),
    }
}

fn inbound_message(value: &Value) -> Result<InboundMessage, Error> {
    let id = value
        .get("id")
        .and_then(value_string)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| webhook_error("webhook_message_id_missing"))?;
    let from = value
        .get("from")
        .and_then(value_string)
        .filter(|from| !from.is_empty())
        .ok_or_else(|| webhook_error("webhook_message_from_missing"))?;
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| webhook_error("webhook_message_type_missing"))?;
    let media = inbound_media(&kind, value);
    let unsupported = inbound_unsupported(&kind, value);
    Ok(InboundMessage {
        id,
        from,
        kind,
        timestamp: value.get("timestamp").and_then(value_string),
        text: value
            .get("text")
            .and_then(|text| text.get("body"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        context_message_id: value
            .get("context")
            .and_then(|context| context.get("id"))
            .and_then(value_string),
        media,
        location: inbound_location(value),
        contacts: inbound_contacts(value),
        interactive: inbound_interactive(value),
        reaction: inbound_reaction(value),
        referral: inbound_referral(value),
        order: inbound_order(value),
        unsupported,
    })
}

fn inbound_media(kind: &str, value: &Value) -> Option<InboundMedia> {
    let key = match kind {
        "image" | "audio" | "video" | "document" | "sticker" => kind,
        _ => return None,
    };
    let object = value.get(key)?;
    let id = object
        .get("id")
        .and_then(value_string)
        .filter(|id| !id.is_empty())?;
    Some(InboundMedia {
        id,
        mime_type: object.get("mime_type").and_then(value_string),
        caption: object.get("caption").and_then(value_string),
        filename: object.get("filename").and_then(value_string),
    })
}

fn json_coord(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| value.as_f64().map(|n| n.to_string()))
        .or_else(|| value.as_i64().map(|n| n.to_string()))
}

fn inbound_location(value: &Value) -> Option<InboundLocation> {
    let object = value.get("location")?;
    Some(InboundLocation {
        latitude: json_coord(object.get("latitude")?)?,
        longitude: json_coord(object.get("longitude")?)?,
        name: object.get("name").and_then(value_string),
        address: object.get("address").and_then(value_string),
    })
}

fn inbound_contacts(value: &Value) -> Option<Vec<InboundContact>> {
    let list = value.get("contacts")?.as_array()?;
    let contacts: Vec<_> = list
        .iter()
        .map(|c| InboundContact {
            formatted_name: c
                .get("name")
                .and_then(|n| n.get("formatted_name"))
                .and_then(value_string),
        })
        .collect();
    if contacts.is_empty() {
        None
    } else {
        Some(contacts)
    }
}

fn inbound_interactive(value: &Value) -> Option<InboundInteractive> {
    let object = value.get("interactive")?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_owned();
    let reply = object
        .get(kind.as_str())
        .or_else(|| object.get("button_reply"))
        .or_else(|| object.get("list_reply"));
    Some(InboundInteractive {
        kind,
        id: reply.and_then(|r| r.get("id")).and_then(value_string),
        title: reply.and_then(|r| r.get("title")).and_then(value_string),
    })
}

fn inbound_reaction(value: &Value) -> Option<InboundReaction> {
    let object = value.get("reaction")?;
    Some(InboundReaction {
        emoji: object.get("emoji").and_then(value_string),
        message_id: object.get("message_id").and_then(value_string),
    })
}

fn inbound_referral(value: &Value) -> Option<InboundReferral> {
    let object = value.get("referral")?;
    Some(InboundReferral {
        source_type: object.get("source_type").and_then(value_string),
        source_id: object.get("source_id").and_then(value_string),
        source_url: object.get("source_url").and_then(value_string),
    })
}

fn inbound_order(value: &Value) -> Option<InboundOrder> {
    let object = value.get("order")?;
    Some(InboundOrder {
        catalog_id: object.get("catalog_id").and_then(value_string),
    })
}

fn inbound_unsupported(kind: &str, value: &Value) -> Option<InboundUnsupported> {
    if kind != "unsupported" {
        return None;
    }
    let first = value
        .get("errors")
        .and_then(Value::as_array)
        .and_then(|e| e.first());
    Some(InboundUnsupported {
        code: first.and_then(|e| e.get("code")).and_then(value_string),
        title: first.and_then(|e| e.get("title")).and_then(value_string),
    })
}

fn delivery_status(value: &Value, include_extras: bool) -> Result<DeliveryStatus, Error> {
    let id = value
        .get("id")
        .and_then(value_string)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| webhook_error("webhook_status_id_missing"))?;
    let status = match value
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| !status.is_empty())
    {
        Some("sent") => DeliveryStatusKind::Sent,
        Some("delivered") => DeliveryStatusKind::Delivered,
        Some("read") => DeliveryStatusKind::Read,
        Some("failed") => DeliveryStatusKind::Failed,
        // Meta can add statuses over time. Refusing an unmodeled value is
        // safer than presenting it as a known delivery outcome or quietly
        // dropping a signed event that an operator needs to investigate.
        Some(_) => return Err(webhook_error("webhook_status_unsupported")),
        None => return Err(webhook_error("webhook_status_missing")),
    };
    Ok(DeliveryStatus {
        id,
        status,
        timestamp: value.get("timestamp").and_then(value_string),
        errors: delivery_errors(value),
        recipient_id: include_extras
            .then(|| value.get("recipient_id").and_then(value_string))
            .flatten(),
        conversation: include_extras
            .then(|| delivery_conversation(value))
            .flatten(),
        pricing: include_extras.then(|| delivery_pricing(value)).flatten(),
    })
}

fn delivery_conversation(value: &Value) -> Option<DeliveryConversation> {
    let object = value.get("conversation")?;
    Some(DeliveryConversation {
        id: object.get("id").and_then(value_string),
        origin_type: object
            .get("origin")
            .and_then(|o| o.get("type"))
            .and_then(value_string),
    })
}

fn delivery_pricing(value: &Value) -> Option<DeliveryPricing> {
    let object = value.get("pricing")?;
    Some(DeliveryPricing {
        billable: object.get("billable").and_then(Value::as_bool),
        pricing_model: object.get("pricing_model").and_then(value_string),
        category: object.get("category").and_then(value_string),
    })
}

fn delivery_errors(value: &Value) -> Vec<DeliveryError> {
    let Some(list) = value.get("errors").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .map(|error| DeliveryError {
            code: error.get("code").and_then(value_string),
            title: error.get("title").and_then(value_string),
        })
        .collect()
}

fn value_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|number| number.to_string()))
        .or_else(|| value.as_i64().map(|number| number.to_string()))
}

fn webhook_error(reason: &str) -> Error {
    Error::InvalidQuery {
        site: Site::new(SITE),
        reason: reason.into(),
    }
}

fn decode_signature(value: &str) -> Option<[u8; 32]> {
    let hex = value.strip_prefix(WEBHOOK_SIGNATURE_PREFIX)?;
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut decoded = [0u8; 32];
    for (index, byte) in decoded.iter_mut().enumerate() {
        let high = hex_value(hex.as_bytes()[index * 2])?;
        let low = hex_value(hex.as_bytes()[index * 2 + 1])?;
        *byte = (high << 4) | low;
    }
    Some(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

async fn read_json(response: reqwest::Response, site: &Site) -> Result<Value, Error> {
    let status = response.status();
    let text = response
        .text()
        .await
        // A reqwest body diagnostic can carry credential-bearing request
        // details, so its content never enters a public Postkit error.
        .map_err(|_| Error::request_failed(site))?;
    if !status.is_success() {
        return Err(map_graph_error(status.as_u16(), &text));
    }
    let value: Value = serde_json::from_str(&text).map_err(|_| Error::Platform {
        site: site.clone(),
        code: "bad_json".into(),
        message: "WhatsApp returned invalid JSON".into(),
    })?;
    if value.get("error").is_some() {
        return Err(map_graph_error(status.as_u16(), &text));
    }
    Ok(value)
}

fn map_graph_error(status: u16, body: &str) -> Error {
    let site = Site::new(SITE);
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let error = value.get("error");
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    // Never fall back to an arbitrary raw body: reverse proxies can echo
    // Authorization headers or operator input. Meta's structured message is
    // the only useful and bounded diagnostics channel.
    let message = error
        .and_then(|error| error.get("error_user_msg"))
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .or_else(|| {
            error
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .unwrap_or("WhatsApp request failed");
    if code == 190 || status == 401 {
        return Error::Auth {
            site,
            reason: "token_invalid".into(),
        };
    }
    if matches!(code, 4 | 17 | 32 | 613) || status == 429 {
        return Error::RateLimited {
            site,
            retry_after: None,
        };
    }
    if status >= 500 && code == 0 {
        return Error::Network {
            site,
            message: format!("http_{status}"),
        };
    }
    Error::Platform {
        site,
        code: code.to_string(),
        message: message.to_string(),
    }
}

#[cfg(all(test, not(feature = "oauth")))]
#[test]
fn whatsapp_feature_excludes_oauth() {
    // This compiles only when `whatsapp-cloud` is enabled without `oauth`.
    // The connector must not accidentally pull a browser redirect flow.
    assert_eq!(SITE, "whatsapp_cloud");
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn app() -> AppConfig {
        AppConfig {
            site: Site::new(SITE),
            oauth: None,
            extra: json!({
                "phone_number_id": "123456789",
                "waba_id": "102290129340398",
                "app_secret": "webhook-secret",
            }),
        }
    }

    fn creds() -> AccountCreds {
        AccountCreds::BotToken {
            token: "system-user-token".into(),
        }
    }

    fn reply_request() -> WhatsAppSendRequest {
        WhatsAppSendRequest {
            message: WhatsAppMessage::Reply {
                to: "60123456789".into(),
                reply_to_message_id: "wamid.inbound".into(),
                text: "Terima kasih".into(),
                preview_url: false,
            },
            idempotency_key: "reply-1".into(),
            recipient_type: RecipientType::Individual,
        }
    }

    #[tokio::test]
    async fn reply_uses_context_and_returns_only_accepted_wamid() {
        let server = MockServer::start();
        let payload = json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "60123456789",
            "context": { "message_id": "wamid.inbound" },
            "type": "text",
            "text": { "body": "Terima kasih", "preview_url": false },
        });
        let send = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/123456789/messages")
                .header("authorization", "Bearer system-user-token")
                .json_body(payload);
            then.status(200).json_body(json!({
                "messaging_product": "whatsapp",
                "messages": [{ "id": "wamid.outbound" }],
            }));
        });
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let outcome = connector
            .send_whatsapp(&app(), &creds(), &reply_request(), Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(outcome.id.as_deref(), Some("wamid.outbound"));
        assert!(outcome.url.is_none());
        assert_eq!(send.hits(), 1);
    }

    #[tokio::test]
    async fn image_send_uses_media_id_and_optional_caption() {
        let server = MockServer::start();
        let payload = json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "60123456789",
            "type": "image",
            "image": { "id": "media-1", "caption": "hi" },
        });
        let send = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/123456789/messages")
                .json_body(payload);
            then.status(200)
                .json_body(json!({ "messages": [{ "id": "wamid.img" }] }));
        });
        let request = WhatsAppSendRequest {
            message: WhatsAppMessage::Image {
                to: "60123456789".into(),
                media: crate::whatsapp::MediaRef {
                    id: Some("media-1".into()),
                    link: None,
                },
                caption: Some("hi".into()),
                reply_to_message_id: None,
            },
            idempotency_key: "img-1".into(),
            recipient_type: RecipientType::Individual,
        };
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let out = connector
            .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(out.id.as_deref(), Some("wamid.img"));
        assert_eq!(send.hits(), 1);
    }

    #[test]
    fn document_audio_video_sticker_payloads_are_typed() {
        let doc = send_payload(&WhatsAppMessage::Document {
            to: "60123456789".into(),
            media: crate::whatsapp::MediaRef {
                id: None,
                link: Some("https://example.com/a.pdf".into()),
            },
            caption: Some("doc".into()),
            filename: Some("a.pdf".into()),
            reply_to_message_id: None,
        });
        assert_eq!(doc["type"], "document");
        assert_eq!(doc["document"]["link"], "https://example.com/a.pdf");
        assert_eq!(doc["document"]["filename"], "a.pdf");
        let audio = send_payload(&WhatsAppMessage::Audio {
            to: "60123456789".into(),
            media: crate::whatsapp::MediaRef {
                id: Some("a1".into()),
                link: None,
            },
            reply_to_message_id: Some("wamid.in".into()),
        });
        assert_eq!(audio["type"], "audio");
        assert_eq!(audio["context"]["message_id"], "wamid.in");
        assert_eq!(
            send_payload(&WhatsAppMessage::Video {
                to: "60123456789".into(),
                media: crate::whatsapp::MediaRef {
                    id: Some("v1".into()),
                    link: None,
                },
                caption: None,
                reply_to_message_id: None,
            })["type"],
            "video"
        );
        assert_eq!(
            send_payload(&WhatsAppMessage::Sticker {
                to: "60123456789".into(),
                media: crate::whatsapp::MediaRef {
                    id: Some("s1".into()),
                    link: None,
                },
                reply_to_message_id: None,
            })["type"],
            "sticker"
        );
    }

    #[test]
    fn interactive_payloads_match_cloud_api() {
        use crate::whatsapp::{ListRow, ListSection, ReplyButton};
        let buttons = send_payload(&WhatsAppMessage::Buttons {
            to: "60123456789".into(),
            body: "Pick".into(),
            buttons: vec![ReplyButton {
                id: "yes".into(),
                title: "Yes".into(),
            }],
            header: None,
            footer: None,
            reply_to_message_id: None,
        });
        assert_eq!(buttons["interactive"]["type"], "button");
        assert_eq!(
            buttons["interactive"]["action"]["buttons"][0]["reply"]["id"],
            "yes"
        );
        let list = send_payload(&WhatsAppMessage::List {
            to: "60123456789".into(),
            body: "Menu".into(),
            button: "Open".into(),
            sections: vec![ListSection {
                title: Some("A".into()),
                rows: vec![ListRow {
                    id: "r1".into(),
                    title: "One".into(),
                    description: None,
                }],
            }],
            header: None,
            footer: None,
            reply_to_message_id: None,
        });
        assert_eq!(list["interactive"]["type"], "list");
        let cta = send_payload(&WhatsAppMessage::CtaUrl {
            to: "60123456789".into(),
            body: "See".into(),
            display_text: "Open".into(),
            url: "https://example.com".into(),
            header: None,
            footer: None,
            reply_to_message_id: None,
        });
        assert_eq!(cta["interactive"]["type"], "cta_url");
        assert_eq!(
            cta["interactive"]["action"]["parameters"]["url"],
            "https://example.com"
        );
        assert_eq!(
            send_payload(&WhatsAppMessage::LocationRequest {
                to: "60123456789".into(),
                body: "Share pin".into(),
                reply_to_message_id: None,
            })["interactive"]["type"],
            "location_request_message"
        );
        assert_eq!(
            send_payload(&WhatsAppMessage::VoiceCall {
                to: "60123456789".into(),
                body: "Call us".into(),
                display_text: Some("Call".into()),
                ttl_minutes: Some(60),
                payload: None,
                reply_to_message_id: None,
            })["interactive"]["type"],
            "voice_call"
        );
    }

    #[test]
    fn location_contacts_address_and_reaction_payloads_match_cloud_api() {
        use crate::whatsapp::OutboundContact;
        let loc = send_payload(&WhatsAppMessage::Location {
            to: "60123456789".into(),
            latitude: "3.139".into(),
            longitude: "101.687".into(),
            name: Some("KLCC".into()),
            address: Some("Kuala Lumpur".into()),
            reply_to_message_id: Some("wamid.in".into()),
        });
        assert_eq!(loc["type"], "location");
        assert_eq!(loc["location"]["latitude"], "3.139");
        assert_eq!(loc["location"]["name"], "KLCC");
        assert_eq!(loc["context"]["message_id"], "wamid.in");
        let contacts = send_payload(&WhatsAppMessage::Contacts {
            to: "60123456789".into(),
            contacts: vec![OutboundContact {
                formatted_name: "Ada".into(),
                phones: vec!["6011".into()],
            }],
            reply_to_message_id: None,
        });
        assert_eq!(contacts["type"], "contacts");
        assert_eq!(contacts["contacts"][0]["name"]["formatted_name"], "Ada");
        assert_eq!(contacts["contacts"][0]["phones"][0]["phone"], "6011");
        let addr = send_payload(&WhatsAppMessage::AddressRequest {
            to: "60123456789".into(),
            body: "Share address".into(),
            country: "my".into(),
            reply_to_message_id: None,
        });
        assert_eq!(addr["interactive"]["type"], "address_message");
        assert_eq!(addr["interactive"]["action"]["parameters"]["country"], "MY");
        let reaction = send_payload(&WhatsAppMessage::Reaction {
            to: "60123456789".into(),
            message_id: "wamid.in".into(),
            emoji: "thumbs".into(),
        });
        assert_eq!(reaction["type"], "reaction");
        assert_eq!(reaction["reaction"]["message_id"], "wamid.in");
        assert_eq!(reaction["reaction"]["emoji"], "thumbs");
    }

    #[test]
    fn mark_read_and_typing_are_status_acks_not_customer_sends() {
        let read = send_payload(&WhatsAppMessage::MarkRead {
            message_id: "wamid.in".into(),
        });
        assert_eq!(read["status"], "read");
        assert_eq!(read["message_id"], "wamid.in");
        assert!(read.get("to").is_none());
        assert!(read.get("recipient_type").is_none());
        let typing = send_payload(&WhatsAppMessage::Typing {
            message_id: "wamid.in".into(),
        });
        assert_eq!(typing["status"], "read");
        assert_eq!(typing["typing_indicator"]["type"], "text");
        let group = send_payload_for(
            &WhatsAppMessage::Text {
                to: "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD".into(),
                text: "hello group".into(),
                preview_url: false,
            },
            RecipientType::Group,
        );
        assert_eq!(group["recipient_type"], "group");
        assert_eq!(
            group["to"],
            "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD"
        );
    }

    #[tokio::test]
    async fn mark_read_records_success_ack_not_a_wamid() {
        let server = MockServer::start();
        let payload = json!({
            "messaging_product": "whatsapp",
            "status": "read",
            "message_id": "wamid.in",
        });
        let send = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/123456789/messages")
                .json_body(payload);
            then.status(200).json_body(json!({ "success": true }));
        });
        let request = WhatsAppSendRequest {
            message: WhatsAppMessage::MarkRead {
                message_id: "wamid.in".into(),
            },
            idempotency_key: "read-1".into(),
            recipient_type: RecipientType::Individual,
        };
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let out = connector
            .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(out.id.as_deref(), Some("wamid.in"));
        assert_eq!(send.hits(), 1);
    }

    #[tokio::test]
    async fn group_text_sets_recipient_type_group() {
        let server = MockServer::start();
        let payload = json!({
            "messaging_product": "whatsapp",
            "recipient_type": "group",
            "to": "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD",
            "type": "text",
            "text": { "body": "hello group", "preview_url": false },
        });
        let send = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/123456789/messages")
                .json_body(payload);
            then.status(200)
                .json_body(json!({ "messages": [{ "id": "wamid.g" }] }));
        });
        let request = WhatsAppSendRequest {
            message: WhatsAppMessage::Text {
                to: "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD".into(),
                text: "hello group".into(),
                preview_url: false,
            },
            idempotency_key: "group-1".into(),
            recipient_type: RecipientType::Group,
        };
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let out = connector
            .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(out.id.as_deref(), Some("wamid.g"));
        assert_eq!(send.hits(), 1);
    }

    #[tokio::test]
    async fn session_text_has_no_context_object() {
        let server = MockServer::start();
        let payload = json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "60123456789",
            "type": "text",
            "text": { "body": "Hello", "preview_url": false },
        });
        let send = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/123456789/messages")
                .json_body(payload);
            then.status(200)
                .json_body(json!({ "messages": [{ "id": "wamid.text" }] }));
        });
        let request = WhatsAppSendRequest {
            message: WhatsAppMessage::Text {
                to: "60123456789".into(),
                text: "Hello".into(),
                preview_url: false,
            },
            idempotency_key: "text-1".into(),
            recipient_type: RecipientType::Individual,
        };
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let outcome = connector
            .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(outcome.id.as_deref(), Some("wamid.text"));
        assert_eq!(send.hits(), 1);
    }

    #[tokio::test]
    async fn media_upload_get_and_delete_use_phone_scoped_graph_paths() {
        let server = MockServer::start();
        let upload = server.mock(|when, then| {
            when.method(POST).path("/v26.0/123456789/media");
            then.status(200).json_body(json!({ "id": "media-99" }));
        });
        let meta = server.mock(|when, then| {
            when.method(GET).path("/v26.0/media-99");
            then.status(200).json_body(json!({
                "id": "media-99",
                "mime_type": "image/jpeg",
                "url": format!("{}/file.bin", server.base_url()),
                "file_size": 3
            }));
        });
        let file = server.mock(|when, then| {
            when.method(GET).path("/file.bin");
            then.status(200).body("abc");
        });
        let del = server.mock(|when, then| {
            when.method(DELETE).path("/v26.0/media-99");
            then.status(200).json_body(json!({ "success": true }));
        });
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let up = connector
            .upload_media(
                &app(),
                &creds(),
                &WhatsAppMediaUpload {
                    bytes: vec![1, 2, 3],
                    mime_type: "image/jpeg".into(),
                    filename: "a.jpg".into(),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(up.id, "media-99");
        let got = connector
            .media_metadata(&app(), &creds(), "media-99", Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(got.mime_type.as_deref(), Some("image/jpeg"));
        let bytes = connector
            .download_media(&app(), &creds(), "media-99", Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(bytes, b"abc");
        connector
            .delete_media(&app(), &creds(), "media-99", Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(upload.hits(), 1);
        assert!(meta.hits() >= 1);
        assert_eq!(file.hits(), 1);
        assert_eq!(del.hits(), 1);
    }

    #[tokio::test]
    async fn formatted_recipient_is_normalized_on_the_wire() {
        let server = MockServer::start();
        let payload = json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+60123456789",
            "type": "text",
            "text": { "body": "Hello", "preview_url": false },
        });
        let send = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/123456789/messages")
                .json_body(payload);
            then.status(200)
                .json_body(json!({ "messages": [{ "id": "wamid.fmt" }] }));
        });
        let request = WhatsAppSendRequest {
            message: WhatsAppMessage::Text {
                to: "+60 12-345 6789".into(),
                text: "Hello".into(),
                preview_url: false,
            },
            idempotency_key: "fmt-1".into(),
            recipient_type: RecipientType::Individual,
        };
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        connector
            .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(send.hits(), 1);
    }

    #[tokio::test]
    async fn preview_url_opt_in_reaches_the_wire() {
        let server = MockServer::start();
        let payload = json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "60123456789",
            "type": "text",
            "text": { "body": "https://example.com", "preview_url": true },
        });
        let send = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/123456789/messages")
                .json_body(payload);
            then.status(200)
                .json_body(json!({ "messages": [{ "id": "wamid.prev" }] }));
        });
        let request = WhatsAppSendRequest {
            message: WhatsAppMessage::Text {
                to: "60123456789".into(),
                text: "https://example.com".into(),
                preview_url: true,
            },
            idempotency_key: "prev-1".into(),
            recipient_type: RecipientType::Individual,
        };
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        connector
            .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(send.hits(), 1);
    }

    #[tokio::test]
    async fn template_has_only_approved_name_language_and_body_values() {
        let server = MockServer::start();
        let payload = json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "60123456789",
            "type": "template",
            "template": {
                "name": "order_update",
                "language": { "code": "en_US" },
                "components": [{
                    "type": "body",
                    "parameters": [
                        { "type": "text", "text": "A-42" },
                        { "type": "text", "text": "tomorrow" },
                    ],
                }],
            },
        });
        let send = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/123456789/messages")
                .json_body(payload);
            then.status(200)
                .json_body(json!({ "messages": [{ "id": "wamid.template" }] }));
        });
        let request = WhatsAppSendRequest {
            message: WhatsAppMessage::Template {
                to: "60123456789".into(),
                name: "order_update".into(),
                language: "en_US".into(),
                body_parameters: vec!["A-42".into(), "tomorrow".into()],
                named_body_parameters: vec![],
                header: None,
                buttons: vec![],
                limited_time_offer: None,
            },
            idempotency_key: "template-1".into(),
            recipient_type: RecipientType::Individual,
        };
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let outcome = connector
            .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(outcome.id.as_deref(), Some("wamid.template"));
        assert_eq!(send.hits(), 1);
    }

    #[test]
    fn template_header_named_body_buttons_and_lto_match_cloud_api() {
        use crate::whatsapp::{
            LimitedTimeOffer, NamedBodyParameter, TemplateButton, TemplateHeader,
        };
        let payload = send_payload(&WhatsAppMessage::Template {
            to: "60123456789".into(),
            name: "fall_sale".into(),
            language: "en_US".into(),
            body_parameters: vec![],
            named_body_parameters: vec![NamedBodyParameter {
                parameter_name: "first_name".into(),
                text: "Ada".into(),
            }],
            header: Some(TemplateHeader::Image {
                media: crate::whatsapp::MediaRef {
                    id: Some("media-1".into()),
                    link: None,
                },
            }),
            buttons: vec![
                TemplateButton::CopyCode {
                    index: 0,
                    coupon_code: "SAVE10".into(),
                },
                TemplateButton::Url {
                    index: 1,
                    text: "promo".into(),
                },
                TemplateButton::PhoneNumber { index: 2 },
            ],
            limited_time_offer: Some(LimitedTimeOffer {
                expiration_time_ms: 1_700_000_000_000,
            }),
        });
        let components = &payload["template"]["components"];
        assert_eq!(components[0]["type"], "header");
        assert_eq!(components[0]["parameters"][0]["type"], "image");
        assert_eq!(
            components[1]["parameters"][0]["parameter_name"],
            "first_name"
        );
        assert_eq!(components[2]["type"], "limited_time_offer");
        assert_eq!(components[3]["sub_type"], "copy_code");
        assert_eq!(components[3]["parameters"][0]["coupon_code"], "SAVE10");
        assert_eq!(components[4]["sub_type"], "url");
        assert_eq!(components[5]["sub_type"], "phone_number");
    }

    fn sample_draft() -> crate::whatsapp::WhatsAppTemplateDraft {
        use crate::whatsapp::{
            TemplateCreateButton, TemplateCreateComponent, WhatsAppTemplateDraft,
        };
        WhatsAppTemplateDraft {
            name: "order_update".into(),
            language: "en_US".into(),
            category: "utility".into(),
            parameter_format: crate::whatsapp::ParameterFormat::Positional,
            components: vec![
                TemplateCreateComponent::Header {
                    format: "TEXT".into(),
                    text: Some("Update".into()),
                    example_handle: None,
                },
                TemplateCreateComponent::Body {
                    text: "Hi {{1}}, your order is ready.".into(),
                    example: vec!["Ada".into()],
                    named_example: vec![],
                },
                TemplateCreateComponent::Footer {
                    text: "Thanks".into(),
                },
                TemplateCreateComponent::Buttons {
                    buttons: vec![TemplateCreateButton::QuickReply { text: "OK".into() }],
                },
            ],
        }
    }

    #[tokio::test]
    async fn template_list_get_create_edit_delete_use_waba_paths() {
        let server = MockServer::start();
        let list = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/102290129340398/message_templates");
            then.status(200).json_body(json!({
                "data": [{
                    "id": "920070352646140",
                    "name": "order_update",
                    "language": "en_US",
                    "status": "APPROVED",
                    "category": "UTILITY",
                    "quality_score": { "score": "GREEN" }
                }]
            }));
        });
        let get = server.mock(|when, then| {
            when.method(GET).path("/v26.0/920070352646140");
            then.status(200).json_body(json!({
                "id": "920070352646140",
                "name": "order_update",
                "status": "APPROVED",
                "quality_score": { "score": "YELLOW" }
            }));
        });
        let create = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/102290129340398/message_templates");
            then.status(200).json_body(json!({
                "id": "111",
                "status": "PENDING",
                "category": "UTILITY"
            }));
        });
        let edit = server.mock(|when, then| {
            when.method(POST).path("/v26.0/111");
            then.status(200).json_body(json!({
                "id": "111",
                "status": "PENDING",
                "category": "UTILITY"
            }));
        });
        let del = server.mock(|when, then| {
            when.method(DELETE)
                .path("/v26.0/102290129340398/message_templates");
            then.status(200).json_body(json!({ "success": true }));
        });
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let listed = connector
            .list_templates(
                &app(),
                &creds(),
                &crate::whatsapp::WhatsAppTemplateQuery::default(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(listed.templates[0].quality.as_deref(), Some("GREEN"));
        let got = connector
            .get_template(&app(), &creds(), "920070352646140", Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(got.quality.as_deref(), Some("YELLOW"));
        let created = connector
            .create_template(&app(), &creds(), &sample_draft(), Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(created.status.as_deref(), Some("PENDING"));
        connector
            .edit_template(
                &app(),
                &creds(),
                "111",
                &sample_draft(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        connector
            .delete_template(&app(), &creds(), "order_update", Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(list.hits(), 1);
        assert_eq!(get.hits(), 1);
        assert_eq!(create.hits(), 1);
        assert_eq!(edit.hits(), 1);
        assert_eq!(del.hits(), 1);
    }

    #[test]
    fn page_cursor_is_opaque_encoded_and_extracted_without_next_url() {
        let url = graph_page_url(
            "https://graph.example/flows?fields=id".into(),
            &WhatsAppPageQuery {
                limit: Some(25),
                after: Some("cursor+/=&".into()),
            },
        );
        assert_eq!(
            url,
            "https://graph.example/flows?fields=id&limit=25&after=cursor%2B%2F%3D%26"
        );
        assert_eq!(
            graph_after(&json!({
                "paging": {
                    "cursors": { "after": "next-page" },
                    "next": "https://graph.example/secretly-unrelated"
                }
            })),
            Some("next-page".into())
        );
    }

    #[tokio::test]
    async fn template_list_round_trips_meta_after_cursor() {
        let server = MockServer::start();
        let list = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/102290129340398/message_templates")
                .query_param("after", "old-page");
            then.status(200).json_body(json!({
                "data": [{ "id": "1", "name": "one" }],
                "paging": { "cursors": { "after": "next-page" } }
            }));
        });
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let page = connector
            .list_templates(
                &app(),
                &creds(),
                &WhatsAppTemplateQuery {
                    name: None,
                    status: None,
                    limit: Some(25),
                    after: Some("old-page".into()),
                },
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(page.templates[0].id, "1");
        assert_eq!(page.after.as_deref(), Some("next-page"));
        assert_eq!(list.hits(), 1);
    }

    #[test]
    fn catalog_product_order_and_flow_payloads_match_cloud_api() {
        use crate::whatsapp::ProductSection;
        let catalog = send_payload(&WhatsAppMessage::Catalog {
            to: "60123456789".into(),
            body: "See our catalog".into(),
            thumbnail_product_retailer_id: Some("sku-1".into()),
            footer: Some("Shop".into()),
            reply_to_message_id: None,
        });
        assert_eq!(catalog["interactive"]["type"], "catalog_message");
        assert_eq!(
            catalog["interactive"]["action"]["parameters"]["thumbnail_product_retailer_id"],
            "sku-1"
        );
        let product = send_payload(&WhatsAppMessage::Product {
            to: "60123456789".into(),
            catalog_id: "cat-1".into(),
            product_retailer_id: "sku-1".into(),
            body: Some("Nice".into()),
            footer: None,
            reply_to_message_id: None,
        });
        assert_eq!(product["interactive"]["type"], "product");
        let list = send_payload(&WhatsAppMessage::ProductList {
            to: "60123456789".into(),
            catalog_id: "cat-1".into(),
            header: "Items".into(),
            body: "Pick".into(),
            sections: vec![ProductSection {
                title: Some("A".into()),
                product_retailer_ids: vec!["sku-1".into()],
            }],
            footer: None,
            reply_to_message_id: None,
        });
        assert_eq!(list["interactive"]["type"], "product_list");
        let order = send_payload(&WhatsAppMessage::OrderStatus {
            to: "60123456789".into(),
            body: "Update".into(),
            reference_id: "ord-1".into(),
            status: "processing".into(),
            description: None,
            reply_to_message_id: None,
        });
        assert_eq!(order["interactive"]["type"], "order_status");
        assert_eq!(
            order["interactive"]["action"]["parameters"]["order"]["status"],
            "processing"
        );
        let flow = send_payload(&WhatsAppMessage::Flow {
            to: "60123456789".into(),
            body: "Book".into(),
            flow_cta: "Open".into(),
            flow_id: Some("123".into()),
            flow_name: None,
            header: None,
            footer: None,
            flow_token: None,
            screen: Some("WELCOME".into()),
            reply_to_message_id: None,
        });
        assert_eq!(flow["interactive"]["type"], "flow");
        assert_eq!(
            flow["interactive"]["action"]["parameters"]["flow_id"],
            "123"
        );
        assert_eq!(
            flow["interactive"]["action"]["parameters"]["flow_message_version"],
            "3"
        );
    }

    #[tokio::test]
    async fn flows_list_create_and_publish_use_waba_flow_paths() {
        let server = MockServer::start();
        let list = server.mock(|when, then| {
            when.method(GET).path("/v26.0/102290129340398/flows");
            then.status(200).json_body(json!({
                "data": [{ "id": "123", "name": "booking", "status": "DRAFT", "categories": ["OTHER"] }]
            }));
        });
        let create = server.mock(|when, then| {
            when.method(POST).path("/v26.0/102290129340398/flows");
            then.status(200)
                .json_body(json!({ "id": "123", "status": "DRAFT" }));
        });
        let publish = server.mock(|when, then| {
            when.method(POST).path("/v26.0/123/publish");
            then.status(200).json_body(json!({ "success": true }));
        });
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let listed = connector
            .list_flows(
                &app(),
                &creds(),
                &crate::whatsapp::WhatsAppPageQuery::default(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(listed.flows[0].status.as_deref(), Some("DRAFT"));
        let draft = crate::whatsapp::WhatsAppFlowDraft {
            name: "booking".into(),
            categories: vec!["OTHER".into()],
            flow_json: r#"{"version":"7.0","screens":[]}"#.into(),
        };
        connector
            .create_flow(&app(), &creds(), &draft, Deadline::from_secs(30))
            .await
            .unwrap();
        let published = connector
            .publish_flow(&app(), &creds(), "123", Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(published.status.as_deref(), Some("PUBLISHED"));
        assert_eq!(list.hits(), 1);
        assert_eq!(create.hits(), 1);
        assert_eq!(publish.hits(), 1);
    }

    #[tokio::test]
    async fn account_list_subscribe_register_and_health_use_waba_paths() {
        let server = MockServer::start();
        let phones = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/102290129340398/phone_numbers");
            then.status(200).json_body(json!({
                "data": [{
                    "id": "123456789",
                    "display_phone_number": "+60 12",
                    "quality_rating": "GREEN",
                    "messaging_limit_tier": "TIER_1K"
                }]
            }));
        });
        let health = server.mock(|when, then| {
            when.method(GET).path("/v26.0/123456789");
            then.status(200).json_body(json!({
                "id": "123456789",
                "quality_rating": "YELLOW",
                "messaging_limit_tier": "TIER_250"
            }));
        });
        let sub = server.mock(|when, then| {
            when.method(POST)
                .path("/v26.0/102290129340398/subscribed_apps");
            then.status(200).json_body(json!({ "success": true }));
        });
        let register = server.mock(|when, then| {
            when.method(POST).path("/v26.0/123456789/register");
            then.status(200).json_body(json!({ "success": true }));
        });
        let pin = server.mock(|when, then| {
            when.method(POST).path("/v26.0/123456789");
            then.status(200).json_body(json!({ "success": true }));
        });
        let waba = server.mock(|when, then| {
            when.method(GET).path("/v26.0/102290129340398");
            then.status(200)
                .json_body(json!({ "id": "102290129340398", "name": "Test" }));
        });
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let listed = connector
            .list_phone_numbers(
                &app(),
                &creds(),
                &crate::whatsapp::WhatsAppPageQuery::default(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(
            listed.phone_numbers[0].quality_rating.as_deref(),
            Some("GREEN")
        );
        let health_row = connector
            .phone_health(&app(), &creds(), Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(health_row.quality_rating.as_deref(), Some("YELLOW"));
        connector
            .subscribe_apps(&app(), &creds(), Deadline::from_secs(30))
            .await
            .unwrap();
        connector
            .register_phone(&app(), &creds(), "123456", Deadline::from_secs(30))
            .await
            .unwrap();
        connector
            .set_two_step_pin(&app(), &creds(), "654321", Deadline::from_secs(30))
            .await
            .unwrap();
        let wabas = connector
            .list_wabas(
                &app(),
                &creds(),
                &crate::whatsapp::WhatsAppPageQuery::default(),
                Deadline::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(wabas.wabas[0].id, "102290129340398");
        assert_eq!(phones.hits(), 1);
        assert_eq!(health.hits(), 1);
        assert_eq!(sub.hits(), 1);
        assert_eq!(register.hits(), 1);
        assert_eq!(pin.hits(), 1);
        assert_eq!(waba.hits(), 1);
    }

    #[tokio::test]
    async fn malformed_send_refuses_before_http() {
        let server = MockServer::start();
        let send = server.mock(|when, then| {
            when.method(POST);
            then.status(200);
        });
        let bad = WhatsAppSendRequest {
            message: WhatsAppMessage::Reply {
                to: "not-a-number".into(),
                reply_to_message_id: "wamid.inbound".into(),
                text: "ok".into(),
                preview_url: false,
            },
            idempotency_key: "bad-1".into(),
            recipient_type: RecipientType::Individual,
        };
        let connector = WhatsAppCloud::with_base(server.base_url()).unwrap();
        let error = connector
            .send_whatsapp(&app(), &creds(), &bad, Deadline::from_secs(30))
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::InvalidPost { reason, .. } if reason == "recipient_must_be_whatsapp_id")
        );
        assert_eq!(send.hits(), 0);
    }

    fn signed(raw: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(b"webhook-secret").unwrap();
        mac.update(raw);
        let bytes = mac.finalize().into_bytes();
        format!(
            "sha256={}",
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    }

    #[test]
    fn signed_webhook_extracts_inbound_messages_and_delivery_statuses() {
        let raw = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "messages":[{
                "from":"60123456789",
                "id":"wamid.inbound",
                "timestamp":"1720000000",
                "type":"text",
                "text":{"body":"Hello"},
                "context":{"id":"wamid.parent"}
              }],
              "statuses":[{
                "id":"wamid.outbound",
                "status":"delivered",
                "timestamp":"1720000001",
                "recipient_id":"60123456789",
                "conversation":{"id":"billing-data-not-returned"}
              }]
            }
          }]}]
        }"#;
        let reply = WhatsAppCloud::parse_signed_webhook(&app(), &signed(raw), raw).unwrap();
        assert_eq!(reply.messages.len(), 1);
        assert_eq!(reply.messages[0].id, "wamid.inbound");
        assert_eq!(reply.messages[0].text.as_deref(), Some("Hello"));
        assert_eq!(
            reply.messages[0].context_message_id.as_deref(),
            Some("wamid.parent")
        );
        assert_eq!(reply.statuses.len(), 1);
        assert_eq!(reply.statuses[0].id, "wamid.outbound");
        assert_eq!(reply.statuses[0].status, DeliveryStatusKind::Delivered);
        assert_eq!(reply.statuses[0].timestamp.as_deref(), Some("1720000001"));
        // The status model intentionally does not reproduce recipient or
        // conversation data from the signed payload.
        let wire = serde_json::to_value(&reply).unwrap();
        assert!(wire["statuses"][0].get("recipient_id").is_none());
        assert!(wire["statuses"][0].get("conversation").is_none());
        assert!(reply.messages[0].media.is_none());
    }

    #[test]
    fn signed_webhook_extracts_inbound_media_id_mime_and_caption() {
        let raw = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "messages":[{
                "from":"60123456789",
                "id":"wamid.image",
                "type":"image",
                "image":{"id":"media-1","mime_type":"image/jpeg","caption":"photo"}
              }]
            }
          }]}]
        }"#;
        let reply = WhatsAppCloud::parse_signed_webhook(&app(), &signed(raw), raw).unwrap();
        let media = reply.messages[0].media.as_ref().expect("media");
        assert_eq!(media.id, "media-1");
        assert_eq!(media.mime_type.as_deref(), Some("image/jpeg"));
        assert_eq!(media.caption.as_deref(), Some("photo"));
        assert!(media.filename.is_none());
    }

    #[test]
    fn signed_webhook_extracts_structured_inbound_fields() {
        let raw = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "messages":[
                {"from":"1","id":"wamid.loc","type":"location",
                 "location":{"latitude":3.14,"longitude":101.6,"name":"KL"}},
                {"from":"1","id":"wamid.btn","type":"interactive",
                 "interactive":{"type":"button_reply","button_reply":{"id":"yes","title":"Yes"}}},
                {"from":"1","id":"wamid.rx","type":"reaction",
                 "reaction":{"emoji":"thumbs","message_id":"wamid.parent"}},
                {"from":"1","id":"wamid.un","type":"unsupported",
                 "errors":[{"code":131051,"title":"unsupported"}]}
              ]
            }
          }]}]
        }"#;
        let reply = WhatsAppCloud::parse_signed_webhook(&app(), &signed(raw), raw).unwrap();
        assert_eq!(
            reply.messages[0].location.as_ref().unwrap().name.as_deref(),
            Some("KL")
        );
        assert_eq!(
            reply.messages[1]
                .interactive
                .as_ref()
                .unwrap()
                .id
                .as_deref(),
            Some("yes")
        );
        assert_eq!(
            reply.messages[2]
                .reaction
                .as_ref()
                .unwrap()
                .emoji
                .as_deref(),
            Some("thumbs")
        );
        assert_eq!(
            reply.messages[3]
                .unsupported
                .as_ref()
                .unwrap()
                .code
                .as_deref(),
            Some("131051")
        );
    }

    #[test]
    fn failed_status_exposes_code_and_title_only() {
        let raw = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "statuses":[{
                "id":"wamid.fail",
                "status":"failed",
                "errors":[{"code":131026,"title":"Message undeliverable","href":"https://example.invalid"}]
              }]
            }
          }]}]
        }"#;
        let reply = WhatsAppCloud::parse_signed_webhook(&app(), &signed(raw), raw).unwrap();
        assert_eq!(reply.statuses[0].status, DeliveryStatusKind::Failed);
        assert_eq!(reply.statuses[0].errors[0].code.as_deref(), Some("131026"));
        assert_eq!(
            reply.statuses[0].errors[0].title.as_deref(),
            Some("Message undeliverable")
        );
        let wire = serde_json::to_value(&reply).unwrap();
        assert!(wire["statuses"][0]["errors"][0].get("href").is_none());
    }

    #[test]
    fn status_extras_are_off_by_default_and_opt_in() {
        let raw = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "statuses":[{
                "id":"wamid.outbound",
                "status":"delivered",
                "recipient_id":"60123456789",
                "conversation":{"id":"conv-1","origin":{"type":"service"}},
                "pricing":{"billable":false,"pricing_model":"PMP","category":"service"}
              }]
            }
          }]}]
        }"#;
        let hidden = WhatsAppCloud::parse_signed_webhook(&app(), &signed(raw), raw).unwrap();
        assert!(hidden.statuses[0].recipient_id.is_none());
        assert!(hidden.statuses[0].conversation.is_none());
        assert!(hidden.statuses[0].pricing.is_none());
        let shown = WhatsAppCloud::parse_signed_webhook_with(
            &app(),
            &signed(raw),
            raw,
            WebhookParseOptions {
                include_status_extras: true,
            },
        )
        .unwrap();
        assert_eq!(
            shown.statuses[0].recipient_id.as_deref(),
            Some("60123456789")
        );
        assert_eq!(
            shown.statuses[0]
                .conversation
                .as_ref()
                .unwrap()
                .id
                .as_deref(),
            Some("conv-1")
        );
        assert_eq!(
            shown.statuses[0]
                .pricing
                .as_ref()
                .unwrap()
                .category
                .as_deref(),
            Some("service")
        );
    }

    #[test]
    fn webhook_refuses_invalid_signature_and_foreign_phone_without_echoing_body() {
        let raw = br#"{"object":"whatsapp_business_account","entry":[{"changes":[{"field":"messages","value":{"metadata":{"phone_number_id":"other"},"messages":[]}}]}]}"#;
        let invalid = WhatsAppCloud::parse_signed_webhook(&app(), "sha256=00", raw).unwrap_err();
        assert!(
            matches!(invalid, Error::InvalidQuery { reason, .. } if reason == "webhook_signature_invalid")
        );

        let foreign = WhatsAppCloud::parse_signed_webhook(&app(), &signed(raw), raw).unwrap_err();
        assert!(
            matches!(foreign, Error::InvalidQuery { ref reason, .. } if reason == "webhook_phone_number_mismatch")
        );
        assert!(!foreign.to_string().contains("other"));
        assert!(!foreign.to_string().contains("webhook-secret"));
    }

    #[test]
    fn signed_status_only_webhook_preserves_all_supported_states_in_order() {
        let status_only = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "statuses":[
                {"id":"wamid.sent","status":"sent","timestamp":"1"},
                {"id":"wamid.delivered","status":"delivered","timestamp":2},
                {"id":"wamid.read","status":"read","timestamp":"3"},
                {"id":"wamid.failed","status":"failed"}
              ]
            }
          }]}]
        }"#;
        let reply =
            WhatsAppCloud::parse_signed_webhook(&app(), &signed(status_only), status_only).unwrap();
        assert!(reply.messages.is_empty());
        assert_eq!(
            reply
                .statuses
                .iter()
                .map(|status| (
                    status.id.as_str(),
                    &status.status,
                    status.timestamp.as_deref()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("wamid.sent", &DeliveryStatusKind::Sent, Some("1")),
                ("wamid.delivered", &DeliveryStatusKind::Delivered, Some("2")),
                ("wamid.read", &DeliveryStatusKind::Read, Some("3")),
                ("wamid.failed", &DeliveryStatusKind::Failed, None),
            ]
        );
    }

    #[test]
    fn webhook_rejects_malformed_json_and_unmodeled_statuses_without_echoing_them() {
        let malformed = b"not-json";
        let error =
            WhatsAppCloud::parse_signed_webhook(&app(), &signed(malformed), malformed).unwrap_err();
        assert!(
            matches!(error, Error::InvalidQuery { reason, .. } if reason == "webhook_json_invalid")
        );

        let unsupported = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "statuses":[{"id":"wamid.outbound","status":"deleted"}]
            }
          }]}]
        }"#;
        let error = WhatsAppCloud::parse_signed_webhook(&app(), &signed(unsupported), unsupported)
            .unwrap_err();
        assert!(
            matches!(error, Error::InvalidQuery { ref reason, .. } if reason == "webhook_status_unsupported")
        );
        assert!(!error.to_string().contains("deleted"));
    }

    #[test]
    fn webhook_refuses_a_statuses_object_instead_of_an_array() {
        let malformed = br#"{
          "object":"whatsapp_business_account",
          "entry":[{"changes":[{
            "field":"messages",
            "value":{
              "metadata":{"phone_number_id":"123456789"},
              "statuses":{"id":"wamid.outbound","status":"sent"}
            }
          }]}]
        }"#;
        let error =
            WhatsAppCloud::parse_signed_webhook(&app(), &signed(malformed), malformed).unwrap_err();
        assert!(
            matches!(error, Error::InvalidQuery { reason, .. } if reason == "webhook_statuses_invalid")
        );
    }

    #[test]
    fn capabilities_describe_both_verified_webhook_read_surfaces() {
        let connector = WhatsAppCloud::new().unwrap();
        assert!(connector
            .capabilities()
            .contains(&Capability::ReadWebhookMessages));
        assert!(connector
            .capabilities()
            .contains(&Capability::ReadWebhookStatuses));
    }

    #[test]
    fn oversized_webhook_is_refused_before_signature_or_json_work() {
        let oversized = vec![b'x'; MAX_WEBHOOK_BYTES + 1];
        let error =
            WhatsAppCloud::parse_signed_webhook(&app(), "sha256=00", &oversized).unwrap_err();
        assert!(
            matches!(error, Error::InvalidQuery { reason, .. } if reason == "webhook_body_too_large")
        );
    }

    #[tokio::test]
    async fn whoami_uses_configured_phone_and_static_bearer_token() {
        let server = MockServer::start();
        let lookup = server.mock(|when, then| {
            when.method(GET)
                .path("/v26.0/123456789")
                .query_param("fields", "id,display_phone_number,verified_name")
                .header("authorization", "Bearer system-user-token");
            then.status(200).json_body(json!({
                "id": "123456789",
                "display_phone_number": "6012 345 6789",
                "verified_name": "Postkit Test",
            }));
        });
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let who = connector.whoami(&app(), &creds()).await.unwrap();
        assert_eq!(who.id, "123456789");
        assert_eq!(who.handle.as_deref(), Some("Postkit Test"));
        assert_eq!(lookup.hits(), 1);
    }
}
