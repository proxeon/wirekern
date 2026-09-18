//! Typed Cloud API sends.
use crate::error::Error;
use crate::facets::WhatsAppSender;
use crate::types::{AccountCreds, AppConfig, Deadline, Outcome};
use crate::whatsapp::{RecipientType, WhatsAppMessage, WhatsAppSendRequest};
use async_trait::async_trait;
use serde_json::{json, Value};

use super::graph::{access_token, phone_number_id, read_json, value_string};
use super::WhatsAppCloud;

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
                account: None,
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
            account: None,
            site: self.site.clone(),
            id: Some(id),
            url: None,
            limits: None,
        })
    }
}

pub(super) fn wire_recipient(to: &str) -> String {
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

pub(super) fn template_header_component(header: &crate::whatsapp::TemplateHeader) -> Value {
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

pub(super) fn template_button_component(button: &crate::whatsapp::TemplateButton) -> Value {
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

pub(super) fn interactive_payload(
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

pub(super) fn media_payload(
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
