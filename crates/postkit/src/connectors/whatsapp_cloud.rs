//! WhatsApp Cloud API connector.
//!
//! Cloud API messaging is deliberately not routed through generic social
//! publishing. A reply's context, an approved template, a specific phone
//! number and the later webhook delivery state are all part of the contract.

use crate::error::Error;
use crate::facets::{WhatsAppAssets, WhatsAppSender};
use crate::http::Http;
use crate::publisher::{AuthKind, Publisher};
use crate::registry::Connector;
use crate::types::{AccountCreds, AppConfig, Capability, Deadline, Intent, Outcome, Site, WhoAmI};
use crate::whatsapp::{
    validate_media_upload, WhatsAppMediaMeta, WhatsAppMediaUpload, WhatsAppUploadedMedia,
    DeliveryConversation, DeliveryError, DeliveryPricing, DeliveryStatus, DeliveryStatusKind,
    InboundContact, InboundInteractive, InboundLocation, InboundMedia, InboundMessage,
    InboundMessages, InboundOrder, InboundReaction, InboundReferral, InboundUnsupported,
    WebhookParseOptions, WhatsAppMessage, WhatsAppSendRequest,
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
            .whatsapp_assets(this)
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
        let expected_phone_number_id = phone_number_id(app)?;
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
                if phone_number_id != expected_phone_number_id {
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
            Capability::ManageWhatsAppMedia,
            Capability::ReadWhatsAppMedia,
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
                    .json(&send_payload(&request.message)),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
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
    match message {
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
        } => {
            let mut template = json!({
                "name": name,
                "language": { "code": language },
            });
            if !body_parameters.is_empty() {
                template["components"] = json!([{
                    "type": "body",
                    "parameters": body_parameters.iter().map(|text| json!({
                        "type": "text",
                        "text": text,
                    })).collect::<Vec<_>>(),
                }]);
            }
            json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "to": wire_recipient(to),
                "type": "template",
                "template": template,
            })
        }
    }
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
            },
            idempotency_key: "template-1".into(),
        };
        let connector = WhatsAppCloud::with_base(format!("{}/v26.0", server.base_url())).unwrap();
        let outcome = connector
            .send_whatsapp(&app(), &creds(), &request, Deadline::from_secs(30))
            .await
            .unwrap();
        assert_eq!(outcome.id.as_deref(), Some("wamid.template"));
        assert_eq!(send.hits(), 1);
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
