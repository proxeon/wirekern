//! Typed WhatsApp Cloud messaging contracts.
//!
//! A recipient, a template and a reply context have materially different
//! consent/billing semantics from a public social post. Keeping them in a
//! dedicated type prevents a generic `Intent.params` map from silently
//! growing into an unreviewed messaging payload escape hatch.

use crate::types::{valid_name, Capability, Site};
use serde::{Deserialize, Serialize};

/// WhatsApp's documented maximum for a text message body. Count Unicode
/// scalar values, not bytes, so multilingual support is not accidentally
/// narrowed by Postkit's local validation.
pub const MAX_REPLY_TEXT: usize = 4_096;
const MIN_RECIPIENT_DIGITS: usize = 7;
const MAX_RECIPIENT_DIGITS: usize = 15;
const MAX_CONTEXT_ID: usize = 512;
const MAX_TEMPLATE_NAME: usize = 512;
const MAX_LANGUAGE: usize = 35;
const MAX_BODY_PARAMETERS: usize = 10;

/// One narrowly typed WhatsApp outbound message. Templates are intentionally
/// limited to ordered body text variables in v1: media headers, buttons and
/// Flows each carry their own user-visible and billing contract.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WhatsAppMessage {
    Reply {
        to: String,
        reply_to_message_id: String,
        text: String,
    },
    /// Service-window text with no `context`. Meta allows this only while
    /// a customer-service window is open; Postkit does not track that clock.
    Text {
        to: String,
        text: String,
    },
    Template {
        to: String,
        name: String,
        language: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        body_parameters: Vec<String>,
    },
}

impl WhatsAppMessage {
    pub fn required_capability(&self) -> Capability {
        match self {
            Self::Reply { .. } => Capability::SendReply,
            Self::Text { .. } => Capability::SendText,
            Self::Template { .. } => Capability::SendTemplate,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Reply {
                to,
                reply_to_message_id,
                text,
            } => {
                validate_recipient(to)?;
                validate_context_id(reply_to_message_id)?;
                validate_reply_text(text)?;
            }
            Self::Text { to, text } => {
                validate_recipient(to)?;
                validate_reply_text(text)?;
            }
            Self::Template {
                to,
                name,
                language,
                body_parameters,
            } => {
                validate_recipient(to)?;
                validate_template_name(name)?;
                validate_language(language)?;
                if body_parameters.len() > MAX_BODY_PARAMETERS {
                    return Err("template_body_parameters_too_many".into());
                }
                for parameter in body_parameters {
                    // An empty substitution tends to be rejected by Meta and
                    // almost always means a caller lost required customer
                    // data. Refuse before a chargeable send reaches Meta.
                    if parameter.trim().is_empty() {
                        return Err("template_body_parameter_empty".into());
                    }
                    if parameter.chars().count() > MAX_REPLY_TEXT {
                        return Err("template_body_parameter_too_long".into());
                    }
                }
            }
        }
        Ok(())
    }
}

/// One send plus the caller-supplied idempotency key. Unlike a social post,
/// every WhatsApp send requires a key because an uncertain retry could create
/// a duplicate private message and potentially a second billable event.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppSendRequest {
    pub message: WhatsAppMessage,
    pub idempotency_key: String,
}

impl WhatsAppSendRequest {
    pub fn required_capability(&self) -> Capability {
        self.message.required_capability()
    }

    pub fn validate(&self) -> Result<(), String> {
        self.message.validate()?;
        if !valid_name(&self.idempotency_key) {
            return Err("idempotency_key_invalid".into());
        }
        Ok(())
    }
}

/// A minimal inbound message extracted from a verified WhatsApp webhook.
/// `from` and `text` are personal data, so Postkit returns them only to the
/// explicit caller and deliberately does not persist an inbox in v1.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InboundMessage {
    pub id: String,
    pub from: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_message_id: Option<String>,
    /// Present for image/audio/video/document/sticker inbound messages.
    /// Postkit does not download the bytes; the operator's webhook host can
    /// fetch `id` with the System User token if needed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<InboundMedia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<InboundLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contacts: Option<Vec<InboundContact>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactive: Option<InboundInteractive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reaction: Option<InboundReaction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referral: Option<InboundReferral>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<InboundOrder>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsupported: Option<InboundUnsupported>,
}

/// Identifiers Meta returns for inbound media. Caption/filename are
/// operator-visible; sha256 is omitted until a caller needs integrity checks.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InboundMedia {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InboundLocation {
    pub latitude: String,
    pub longitude: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InboundContact {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formatted_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InboundInteractive {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InboundReaction {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InboundReferral {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InboundOrder {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_id: Option<String>,
}

/// Meta `type=unsupported` plus the first error code/title. No raw dump.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InboundUnsupported {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// The small, closed set of outbound delivery states Postkit can interpret.
/// Keeping this enum closed makes a new Meta state an explicit compatibility
/// decision instead of silently reporting an unreviewed string as delivery.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatusKind {
    Sent,
    Delivered,
    Read,
    Failed,
}

/// One status callback for the exact outbound `wamid` returned by a prior
/// send. Recipient IDs, failure bodies, conversation and pricing details are
/// intentionally excluded: correlating the opaque message ID is sufficient
/// here and the omitted fields need separate privacy/billing contracts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeliveryStatus {
    pub id: String,
    pub status: DeliveryStatusKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// The ordered events contained in one signed webhook delivery. `messages`
/// remains the existing inbound surface; `statuses` is optional in JSON so
/// inbound-only callers receive the same shape they did before this addition.
/// Postkit does not deduplicate, reorder, persist, or infer a final state from
/// these callbacks because each behavior requires application-owned storage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InboundMessages {
    pub site: Site,
    pub messages: Vec<InboundMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<DeliveryStatus>,
}

pub fn validate_recipient(value: &str) -> Result<(), String> {
    if !(MIN_RECIPIENT_DIGITS..=MAX_RECIPIENT_DIGITS).contains(&value.len())
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("recipient_must_be_whatsapp_id".into());
    }
    Ok(())
}

fn validate_context_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_CONTEXT_ID
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err("reply_to_message_id_invalid".into());
    }
    Ok(())
}

fn validate_reply_text(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("reply_text_empty".into());
    }
    if value.chars().count() > MAX_REPLY_TEXT {
        return Err("reply_text_too_long".into());
    }
    Ok(())
}

fn validate_template_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_TEMPLATE_NAME
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err("template_name_invalid".into());
    }
    Ok(())
}

fn validate_language(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_LANGUAGE
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err("template_language_invalid".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_and_template_validation_are_closed_and_local() {
        let reply = WhatsAppMessage::Reply {
            to: "60123456789".into(),
            reply_to_message_id: "wamid.abc".into(),
            text: "Terima kasih".into(),
        };
        assert!(reply.validate().is_ok());
        assert_eq!(
            WhatsAppMessage::Reply {
                to: "+60123456789".into(),
                reply_to_message_id: "wamid.abc".into(),
                text: "ok".into(),
            }
            .validate()
            .unwrap_err(),
            "recipient_must_be_whatsapp_id"
        );
        assert!(WhatsAppMessage::Text {
            to: "60123456789".into(),
            text: "Hello".into(),
        }
        .validate()
        .is_ok());
        assert_eq!(
            WhatsAppMessage::Text {
                to: "60123456789".into(),
                text: "   ".into(),
            }
            .validate()
            .unwrap_err(),
            "reply_text_empty"
        );
        assert_eq!(
            WhatsAppMessage::Template {
                to: "60123456789".into(),
                name: "Order_Update".into(),
                language: "en_US".into(),
                body_parameters: vec![],
            }
            .validate()
            .unwrap_err(),
            "template_name_invalid"
        );
    }

    #[test]
    fn every_send_requires_a_safe_vault_key() {
        let request = WhatsAppSendRequest {
            message: WhatsAppMessage::Reply {
                to: "60123456789".into(),
                reply_to_message_id: "wamid.abc".into(),
                text: "ok".into(),
            },
            idempotency_key: "not/a-filename".into(),
        };
        assert_eq!(request.validate().unwrap_err(), "idempotency_key_invalid");
    }
}
