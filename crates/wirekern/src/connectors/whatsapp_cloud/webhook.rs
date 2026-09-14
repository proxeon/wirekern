//! Signed webhook verify/parse. Wirekern does not run a listener.
use crate::error::Error;
use crate::types::{AppConfig, Site};
use crate::whatsapp::{
    DeliveryConversation, DeliveryError, DeliveryPricing, DeliveryStatus, DeliveryStatusKind,
    InboundContact, InboundInteractive, InboundLocation, InboundMedia, InboundMessage,
    InboundMessages, InboundOrder, InboundReaction, InboundReferral, InboundUnsupported,
    WebhookParseOptions,
};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;

use super::graph::{configured_phone_ids, value_string};
use super::{WhatsAppCloud, MAX_WEBHOOK_BYTES, SITE, WEBHOOK_SIGNATURE_PREFIX};

type HmacSha256 = Hmac<Sha256>;

impl WhatsAppCloud {
    /// Verify and parse raw `messages` webhook bytes. This is intentionally a
    /// pure adapter for an application's HTTP endpoint: Wirekern does not run
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
                // that it belongs to the Wirekern-configured sender. Refusing
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

pub(super) fn webhook_secret(app: &AppConfig) -> Result<String, Error> {
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

pub(super) fn inbound_message(value: &Value) -> Result<InboundMessage, Error> {
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

pub(super) fn inbound_media(kind: &str, value: &Value) -> Option<InboundMedia> {
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

pub(super) fn json_coord(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| value.as_f64().map(|n| n.to_string()))
        .or_else(|| value.as_i64().map(|n| n.to_string()))
}

pub(super) fn inbound_location(value: &Value) -> Option<InboundLocation> {
    let object = value.get("location")?;
    Some(InboundLocation {
        latitude: json_coord(object.get("latitude")?)?,
        longitude: json_coord(object.get("longitude")?)?,
        name: object.get("name").and_then(value_string),
        address: object.get("address").and_then(value_string),
    })
}

pub(super) fn inbound_contacts(value: &Value) -> Option<Vec<InboundContact>> {
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

pub(super) fn inbound_interactive(value: &Value) -> Option<InboundInteractive> {
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

pub(super) fn inbound_reaction(value: &Value) -> Option<InboundReaction> {
    let object = value.get("reaction")?;
    Some(InboundReaction {
        emoji: object.get("emoji").and_then(value_string),
        message_id: object.get("message_id").and_then(value_string),
    })
}

pub(super) fn inbound_referral(value: &Value) -> Option<InboundReferral> {
    let object = value.get("referral")?;
    Some(InboundReferral {
        source_type: object.get("source_type").and_then(value_string),
        source_id: object.get("source_id").and_then(value_string),
        source_url: object.get("source_url").and_then(value_string),
    })
}

pub(super) fn inbound_order(value: &Value) -> Option<InboundOrder> {
    let object = value.get("order")?;
    Some(InboundOrder {
        catalog_id: object.get("catalog_id").and_then(value_string),
    })
}

pub(super) fn inbound_unsupported(kind: &str, value: &Value) -> Option<InboundUnsupported> {
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

pub(super) fn delivery_status(
    value: &Value,
    include_extras: bool,
) -> Result<DeliveryStatus, Error> {
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

pub(super) fn delivery_conversation(value: &Value) -> Option<DeliveryConversation> {
    let object = value.get("conversation")?;
    Some(DeliveryConversation {
        id: object.get("id").and_then(value_string),
        origin_type: object
            .get("origin")
            .and_then(|o| o.get("type"))
            .and_then(value_string),
    })
}

pub(super) fn delivery_pricing(value: &Value) -> Option<DeliveryPricing> {
    let object = value.get("pricing")?;
    Some(DeliveryPricing {
        billable: object.get("billable").and_then(Value::as_bool),
        pricing_model: object.get("pricing_model").and_then(value_string),
        category: object.get("category").and_then(value_string),
    })
}

pub(super) fn delivery_errors(value: &Value) -> Vec<DeliveryError> {
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

pub(super) fn webhook_error(reason: &str) -> Error {
    Error::InvalidQuery {
        site: Site::new(SITE),
        reason: reason.into(),
    }
}

pub(super) fn decode_signature(value: &str) -> Option<[u8; 32]> {
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

pub(super) fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
