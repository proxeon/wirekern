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
        /// Meta link-preview fetch. Default false: a URL in the body stays
        /// literal until the caller opts in.
        #[serde(default)]
        preview_url: bool,
    },
    /// Service-window text with no `context`. Meta allows this only while
    /// a customer-service window is open; Postkit does not track that clock.
    Text {
        to: String,
        text: String,
        #[serde(default)]
        preview_url: bool,
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
                preview_url: _,
            } => {
                validate_recipient(to)?;
                validate_context_id(reply_to_message_id)?;
                validate_reply_text(text)?;
            }
            Self::Text {
                to,
                text,
                preview_url: _,
            } => {
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
///
/// The ledger only records a confirmed `Outcome`. If the request left the
/// machine and the response was lost, the claim is released and a retry is
/// **not** automatic — the operator must reconcile via the delivery webhook
/// before sending again.
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
/// send. Recipient, conversation and pricing stay off this type unless the
/// caller opts into [`WebhookParseOptions::include_status_extras`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeliveryStatus {
    pub id: String,
    pub status: DeliveryStatusKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Meta `statuses[].errors` — code and title only, no href/raw dump.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<DeliveryError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<DeliveryConversation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<DeliveryPricing>,
}

/// Off by default: recipient/conversation/pricing are personal/billing data.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WebhookParseOptions {
    pub include_status_extras: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeliveryConversation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_type: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeliveryPricing {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeliveryError {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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

/// Cloud API media handle. Either `id` (uploaded / inbound) or an `https`
/// `link`. Meta requires exactly one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MediaRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl MediaRef {
    pub fn validate(&self) -> Result<(), String> {
        match (
            self.id.as_deref().filter(|s| !s.is_empty()),
            self.link.as_deref().filter(|s| !s.is_empty()),
        ) {
            (Some(_), None) => Ok(()),
            (None, Some(link)) if link.starts_with("https://") => Ok(()),
            (None, Some(_)) => Err("media_link_must_be_https".into()),
            _ => Err("media_id_or_https_link_required".into()),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        match (
            self.id.as_deref().filter(|s| !s.is_empty()),
            self.link.as_deref().filter(|s| !s.is_empty()),
        ) {
            (Some(id), _) => serde_json::json!({ "id": id }),
            (_, Some(link)) => serde_json::json!({ "link": link }),
            _ => serde_json::json!({}),
        }
    }
}

/// Local file for `POST /{phone-number-id}/media`. Bytes are not Debug-printed.
#[derive(Clone, Eq, PartialEq)]
pub struct WhatsAppMediaUpload {
    pub bytes: Vec<u8>,
    pub mime_type: String,
    pub filename: String,
}

impl std::fmt::Debug for WhatsAppMediaUpload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhatsAppMediaUpload")
            .field("bytes_len", &self.bytes.len())
            .field("mime_type", &self.mime_type)
            .field("filename", &self.filename)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppUploadedMedia {
    pub id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppMediaMeta {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    /// Short-lived download URL (Meta: ~5 minutes). Do not persist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Meta documented MIME types for Cloud API media (image 5 MB, audio/video
/// 16 MB, sticker 500 KB, document 100 MB).
pub fn media_max_bytes(mime: &str) -> Option<usize> {
    match mime {
        "image/jpeg" | "image/png" => Some(5 * 1024 * 1024),
        "image/webp" => Some(500 * 1024),
        "audio/aac" | "audio/amr" | "audio/mpeg" | "audio/mp4" | "audio/ogg" => {
            Some(16 * 1024 * 1024)
        }
        "video/mp4" | "video/3gpp" => Some(16 * 1024 * 1024),
        "text/plain"
        | "application/pdf"
        | "application/msword"
        | "application/vnd.ms-excel"
        | "application/vnd.ms-powerpoint"
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            Some(100 * 1024 * 1024)
        }
        _ => None,
    }
}

pub fn validate_media_upload(upload: &WhatsAppMediaUpload) -> Result<(), String> {
    if upload.filename.is_empty() || upload.filename.contains('/') || upload.filename.contains('\\')
    {
        return Err("media_filename_invalid".into());
    }
    let Some(max) = media_max_bytes(&upload.mime_type) else {
        return Err("media_mime_unsupported".into());
    };
    if upload.bytes.is_empty() {
        return Err("media_bytes_empty".into());
    }
    if upload.bytes.len() > max {
        return Err("media_bytes_too_large".into());
    }
    Ok(())
}

pub fn validate_recipient(value: &str) -> Result<(), String> {
    normalize_recipient(value).map(|_| ())
}

/// Meta accepts `+`, spaces, hyphens, and parentheses. We strip decoration
/// and keep an optional leading `+` plus 7–15 digits so the wire matches
/// Meta's recommendation without inventing a country code.
pub fn normalize_recipient(value: &str) -> Result<String, String> {
    let mut plus = false;
    let mut digits = String::new();
    for c in value.chars() {
        match c {
            '+' if !plus && digits.is_empty() => plus = true,
            '0'..='9' => digits.push(c),
            ' ' | '-' | '(' | ')' => {}
            _ => return Err("recipient_must_be_whatsapp_id".into()),
        }
    }
    if !(MIN_RECIPIENT_DIGITS..=MAX_RECIPIENT_DIGITS).contains(&digits.len()) {
        return Err("recipient_must_be_whatsapp_id".into());
    }
    Ok(if plus { format!("+{digits}") } else { digits })
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
            preview_url: false,
        };
        assert!(reply.validate().is_ok());
        assert!(WhatsAppMessage::Reply {
            to: "+60 12-345 6789".into(),
            reply_to_message_id: "wamid.abc".into(),
            text: "ok".into(),
            preview_url: false,
        }
        .validate()
        .is_ok());
        assert_eq!(
            normalize_recipient("+60 (12) 345-6789").unwrap(),
            "+60123456789"
        );
        assert_eq!(
            WhatsAppMessage::Reply {
                to: "not-a-number".into(),
                reply_to_message_id: "wamid.abc".into(),
                text: "ok".into(),
                preview_url: false,
            }
            .validate()
            .unwrap_err(),
            "recipient_must_be_whatsapp_id"
        );
        assert!(WhatsAppMessage::Text {
            to: "60123456789".into(),
            text: "Hello".into(),
            preview_url: false,
        }
        .validate()
        .is_ok());
        assert_eq!(
            WhatsAppMessage::Text {
                to: "60123456789".into(),
                text: "   ".into(),
                preview_url: false,
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
                preview_url: false,
            },
            idempotency_key: "not/a-filename".into(),
        };
        assert_eq!(request.validate().unwrap_err(), "idempotency_key_invalid");
    }

    #[test]
    fn idempotency_docs_require_a_filename_safe_key() {
        // The key is a vault filename. Spaces/slashes would be a path
        // injection, not a retry token.
        assert!(validate_recipient("60123456789").is_ok());
        let ok = WhatsAppSendRequest {
            message: WhatsAppMessage::Text {
                to: "60123456789".into(),
                text: "hi".into(),
                preview_url: false,
            },
            idempotency_key: "order-42-v1".into(),
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn media_ref_requires_exactly_one_of_id_or_https_link() {
        assert!(MediaRef {
            id: Some("123".into()),
            link: None,
        }
        .validate()
        .is_ok());
        assert!(MediaRef {
            id: None,
            link: Some("https://example.com/a.jpg".into()),
        }
        .validate()
        .is_ok());
        assert_eq!(
            MediaRef {
                id: None,
                link: Some("http://insecure.example/a.jpg".into()),
            }
            .validate()
            .unwrap_err(),
            "media_link_must_be_https"
        );
        assert_eq!(
            MediaRef {
                id: Some("1".into()),
                link: Some("https://example.com/a.jpg".into()),
            }
            .validate()
            .unwrap_err(),
            "media_id_or_https_link_required"
        );
    }

    #[test]
    fn media_upload_rejects_unknown_mime_and_oversize() {
        let small = WhatsAppMediaUpload {
            bytes: vec![1, 2, 3],
            mime_type: "image/jpeg".into(),
            filename: "a.jpg".into(),
        };
        assert!(validate_media_upload(&small).is_ok());
        let bad = WhatsAppMediaUpload {
            bytes: vec![1],
            mime_type: "application/octet-stream".into(),
            filename: "a.bin".into(),
        };
        assert_eq!(
            validate_media_upload(&bad).unwrap_err(),
            "media_mime_unsupported"
        );
    }
}
