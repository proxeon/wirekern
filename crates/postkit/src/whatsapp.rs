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
    /// Approved template send. Footer text is baked into the template at
    /// create time (Business Management API) — Cloud API send has no footer
    /// component. Header/buttons/named body/LTO are send-time substitutions.
    Template {
        to: String,
        name: String,
        language: String,
        /// Positional body `{{1}}`, `{{2}}`, … Mutually exclusive with
        /// `named_body_parameters`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        body_parameters: Vec<String>,
        /// Named body `{{first_name}}`. Requires a template created with
        /// `parameter_format: named`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        named_body_parameters: Vec<NamedBodyParameter>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<TemplateHeader>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        buttons: Vec<TemplateButton>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limited_time_offer: Option<LimitedTimeOffer>,
    },
    Image {
        to: String,
        #[serde(flatten)]
        media: MediaRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    Document {
        to: String,
        #[serde(flatten)]
        media: MediaRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    Audio {
        to: String,
        #[serde(flatten)]
        media: MediaRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    Video {
        to: String,
        #[serde(flatten)]
        media: MediaRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    Sticker {
        to: String,
        #[serde(flatten)]
        media: MediaRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    Buttons {
        to: String,
        body: String,
        buttons: Vec<ReplyButton>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        footer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    List {
        to: String,
        body: String,
        button: String,
        sections: Vec<ListSection>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        footer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    CtaUrl {
        to: String,
        body: String,
        display_text: String,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        footer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    LocationRequest {
        to: String,
        body: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    VoiceCall {
        to: String,
        body: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl_minutes: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    /// Location pin. Cloud API takes latitude/longitude as decimal strings;
    /// name/address are optional map labels, not a geocode lookup.
    Location {
        to: String,
        latitude: String,
        longitude: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    /// Contact card(s). Meta requires `name.formatted_name`; extra phones are
    /// card fields, not a substitute for the recipient `to`.
    Contacts {
        to: String,
        contacts: Vec<OutboundContact>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    /// Interactive `address_message`. `country` is ISO 3166-1 alpha-2; Meta
    /// only collects addresses in supported countries.
    AddressRequest {
        to: String,
        body: String,
        country: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    /// Emoji reaction on an inbound `wamid`. Empty emoji is refused: Meta
    /// treats an omitted emoji as "remove reaction", which must be explicit
    /// later rather than a silent default.
    Reaction {
        to: String,
        message_id: String,
        emoji: String,
    },
    /// Mark an inbound `wamid` as read. Cloud API returns `{success: true}`,
    /// not a new outbound wamid. `Outcome.id` is this inbound id so the local
    /// idempotency ledger still has a stable value.
    MarkRead {
        message_id: String,
    },
    /// Typing indicator. Official docs always pair it with mark-as-read on
    /// the same inbound `wamid` (`status: read` + `typing_indicator`). It
    /// dismisses after 25s or the next send, whichever is first.
    Typing {
        message_id: String,
    },
}

/// Cloud API `recipient_type`. Group `to` is a Groups API id, not a phone
/// number — see Meta group messaging (`recipient_type: group`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipientType {
    #[default]
    Individual,
    Group,
}

impl RecipientType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Individual => "individual",
            Self::Group => "group",
        }
    }
}

impl std::str::FromStr for RecipientType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "individual" => Ok(Self::Individual),
            "group" => Ok(Self::Group),
            _ => Err("recipient_type_invalid".into()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NamedBodyParameter {
    pub parameter_name: String,
    pub text: String,
}

/// Send-time header substitution. Text headers support one variable; media
/// headers take a Cloud API media id or HTTPS link (same `MediaRef` XOR).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TemplateHeader {
    Text { text: String },
    Image {
        #[serde(flatten)]
        media: MediaRef,
    },
    Video {
        #[serde(flatten)]
        media: MediaRef,
    },
    Document {
        #[serde(flatten)]
        media: MediaRef,
    },
}

/// Send-time button parameters. Phone-number buttons are static on the
/// template; we still emit the index so the caller can name the slot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "sub_type", rename_all = "snake_case")]
pub enum TemplateButton {
    QuickReply { index: u8, payload: String },
    Url { index: u8, text: String },
    PhoneNumber { index: u8 },
    CopyCode { index: u8, coupon_code: String },
}

/// Limited-time offer send component. `expiration_time_ms` is Unix epoch
/// milliseconds as required by Cloud API LTO templates.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LimitedTimeOffer {
    pub expiration_time_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OutboundContact {
    pub formatted_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phones: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReplyButton {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub rows: Vec<ListRow>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListRow {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl WhatsAppMessage {
    pub fn required_capability(&self) -> Capability {
        match self {
            Self::Reply { .. } => Capability::SendReply,
            Self::Text { .. } => Capability::SendText,
            Self::Template { .. } => Capability::SendTemplate,
            Self::Image { .. }
            | Self::Document { .. }
            | Self::Audio { .. }
            | Self::Video { .. }
            | Self::Sticker { .. } => Capability::SendMedia,
            Self::Buttons { .. }
            | Self::List { .. }
            | Self::CtaUrl { .. }
            | Self::LocationRequest { .. }
            | Self::VoiceCall { .. }
            | Self::AddressRequest { .. } => Capability::SendInteractive,
            Self::Location { .. } => Capability::SendLocation,
            Self::Contacts { .. } => Capability::SendContacts,
            Self::Reaction { .. } => Capability::SendReaction,
            Self::MarkRead { .. } => Capability::MarkRead,
            Self::Typing { .. } => Capability::SendTyping,
        }
    }

    /// True when Meta acknowledges with `{success: true}` instead of a wamid.
    pub fn is_status_ack(&self) -> bool {
        matches!(self, Self::MarkRead { .. } | Self::Typing { .. })
    }

    pub fn validate(&self) -> Result<(), String> {
        self.validate_for(RecipientType::Individual)
    }

    pub fn validate_for(&self, recipient_type: RecipientType) -> Result<(), String> {
        match self {
            Self::Reply {
                to,
                reply_to_message_id,
                text,
                preview_url: _,
            } => {
                validate_destination(to, recipient_type)?;
                validate_context_id(reply_to_message_id)?;
                validate_reply_text(text)?;
            }
            Self::Text {
                to,
                text,
                preview_url: _,
            } => {
                validate_destination(to, recipient_type)?;
                validate_reply_text(text)?;
            }
            Self::Template {
                to,
                name,
                language,
                body_parameters,
                named_body_parameters,
                header,
                buttons,
                limited_time_offer,
            } => {
                validate_destination(to, recipient_type)?;
                validate_template_name(name)?;
                validate_language(language)?;
                if !body_parameters.is_empty() && !named_body_parameters.is_empty() {
                    return Err("template_body_parameters_mixed".into());
                }
                if body_parameters.len() > MAX_BODY_PARAMETERS
                    || named_body_parameters.len() > MAX_BODY_PARAMETERS
                {
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
                for parameter in named_body_parameters {
                    validate_named_parameter(parameter)?;
                }
                if let Some(header) = header {
                    validate_template_header(header)?;
                }
                if buttons.len() > 10 {
                    return Err("template_buttons_too_many".into());
                }
                for button in buttons {
                    validate_template_button(button)?;
                }
                if limited_time_offer
                    .as_ref()
                    .is_some_and(|o| o.expiration_time_ms == 0)
                {
                    return Err("template_lto_expiration_invalid".into());
                }
            }
            Self::Image {
                to,
                media,
                caption,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                media.validate()?;
                validate_optional_caption(caption)?;
                validate_optional_context(reply_to_message_id)?;
            }
            Self::Document {
                to,
                media,
                caption,
                filename,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                media.validate()?;
                validate_optional_caption(caption)?;
                if let Some(name) = filename {
                    if name.is_empty() || name.contains('/') {
                        return Err("media_filename_invalid".into());
                    }
                }
                validate_optional_context(reply_to_message_id)?;
            }
            Self::Audio {
                to,
                media,
                reply_to_message_id,
            }
            | Self::Sticker {
                to,
                media,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                media.validate()?;
                validate_optional_context(reply_to_message_id)?;
            }
            Self::Video {
                to,
                media,
                caption,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                media.validate()?;
                validate_optional_caption(caption)?;
                validate_optional_context(reply_to_message_id)?;
            }
            Self::Buttons {
                to,
                body,
                buttons,
                header,
                footer,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                validate_body(body)?;
                validate_optional_header(header)?;
                validate_optional_footer(footer)?;
                validate_optional_context(reply_to_message_id)?;
                if !(1..=3).contains(&buttons.len()) {
                    return Err("reply_buttons_count".into());
                }
                for b in buttons {
                    if b.id.is_empty() || b.id.len() > 256 {
                        return Err("reply_button_id_invalid".into());
                    }
                    if b.title.is_empty() || b.title.chars().count() > 20 {
                        return Err("reply_button_title_invalid".into());
                    }
                }
            }
            Self::List {
                to,
                body,
                button,
                sections,
                header,
                footer,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                validate_body(body)?;
                validate_optional_header(header)?;
                validate_optional_footer(footer)?;
                validate_optional_context(reply_to_message_id)?;
                if button.is_empty() || button.chars().count() > 20 {
                    return Err("list_button_invalid".into());
                }
                if sections.is_empty() || sections.len() > 10 {
                    return Err("list_sections_count".into());
                }
                for section in sections {
                    if let Some(title) = &section.title {
                        if title.chars().count() > 24 {
                            return Err("list_section_title_invalid".into());
                        }
                    }
                    if section.rows.is_empty() || section.rows.len() > 10 {
                        return Err("list_rows_count".into());
                    }
                    for row in &section.rows {
                        if row.id.is_empty() || row.id.len() > 200 {
                            return Err("list_row_id_invalid".into());
                        }
                        if row.title.is_empty() || row.title.chars().count() > 24 {
                            return Err("list_row_title_invalid".into());
                        }
                        if row
                            .description
                            .as_ref()
                            .is_some_and(|d| d.chars().count() > 72)
                        {
                            return Err("list_row_description_invalid".into());
                        }
                    }
                }
            }
            Self::CtaUrl {
                to,
                body,
                display_text,
                url,
                header,
                footer,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                validate_body(body)?;
                validate_optional_header(header)?;
                validate_optional_footer(footer)?;
                validate_optional_context(reply_to_message_id)?;
                if display_text.is_empty() || display_text.chars().count() > 20 {
                    return Err("cta_display_text_invalid".into());
                }
                if !url.starts_with("https://") {
                    return Err("cta_url_must_be_https".into());
                }
            }
            Self::LocationRequest {
                to,
                body,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                validate_body(body)?;
                validate_optional_context(reply_to_message_id)?;
            }
            Self::VoiceCall {
                to,
                body,
                display_text,
                ttl_minutes,
                payload: _,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                validate_body(body)?;
                validate_optional_context(reply_to_message_id)?;
                if display_text
                    .as_ref()
                    .is_some_and(|t| t.is_empty() || t.chars().count() > 20)
                {
                    return Err("voice_call_display_text_invalid".into());
                }
                if ttl_minutes.is_some_and(|m| !(1..=43200).contains(&m)) {
                    return Err("voice_call_ttl_invalid".into());
                }
            }
            Self::Location {
                to,
                latitude,
                longitude,
                name: _,
                address: _,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                validate_optional_context(reply_to_message_id)?;
                validate_coordinates(latitude, longitude)?;
            }
            Self::Contacts {
                to,
                contacts,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                validate_optional_context(reply_to_message_id)?;
                if contacts.is_empty() {
                    return Err("contacts_empty".into());
                }
                for c in contacts {
                    if c.formatted_name.trim().is_empty() {
                        return Err("contact_name_empty".into());
                    }
                }
            }
            Self::AddressRequest {
                to,
                body,
                country,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                validate_body(body)?;
                validate_optional_context(reply_to_message_id)?;
                if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_alphabetic()) {
                    return Err("address_country_invalid".into());
                }
            }
            Self::Reaction {
                to,
                message_id,
                emoji,
            } => {
                validate_destination(to, recipient_type)?;
                validate_context_id(message_id)?;
                if emoji.trim().is_empty() {
                    return Err("reaction_emoji_empty".into());
                }
            }
            Self::MarkRead { message_id } | Self::Typing { message_id } => {
                if recipient_type != RecipientType::Individual {
                    return Err("status_ack_not_group".into());
                }
                validate_context_id(message_id)?;
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
    /// Default `individual`. `group` puts a Groups API id in `to` and sets
    /// Cloud API `recipient_type: group`. Status acks (read/typing) refuse it.
    #[serde(default)]
    pub recipient_type: RecipientType,
}

impl WhatsAppSendRequest {
    pub fn required_capability(&self) -> Capability {
        self.message.required_capability()
    }

    pub fn validate(&self) -> Result<(), String> {
        self.message.validate_for(self.recipient_type)?;
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

fn validate_destination(value: &str, recipient_type: RecipientType) -> Result<(), String> {
    match recipient_type {
        RecipientType::Individual => validate_recipient(value),
        RecipientType::Group => validate_group_id(value),
    }
}

/// Groups API ids are opaque (often base64). Refuse empty, whitespace, and
/// path separators so a group `to` cannot be a URL or filename.
fn validate_group_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(|c| c.is_whitespace() || c == '/' || c == '\\')
    {
        return Err("group_id_invalid".into());
    }
    Ok(())
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

fn validate_body(text: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.chars().count() > 1024 {
        return Err("interactive_body_invalid".into());
    }
    Ok(())
}

fn validate_optional_header(header: &Option<String>) -> Result<(), String> {
    if header.as_ref().is_some_and(|t| t.chars().count() > 60) {
        return Err("interactive_header_invalid".into());
    }
    Ok(())
}

fn validate_optional_footer(footer: &Option<String>) -> Result<(), String> {
    if footer.as_ref().is_some_and(|t| t.chars().count() > 60) {
        return Err("interactive_footer_invalid".into());
    }
    Ok(())
}

fn validate_optional_caption(caption: &Option<String>) -> Result<(), String> {
    if let Some(text) = caption {
        if text.chars().count() > 1024 {
            return Err("caption_too_long".into());
        }
    }
    Ok(())
}

fn validate_optional_context(id: &Option<String>) -> Result<(), String> {
    match id {
        Some(id) => validate_context_id(id),
        None => Ok(()),
    }
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

fn validate_named_parameter(parameter: &NamedBodyParameter) -> Result<(), String> {
    if parameter.parameter_name.is_empty()
        || parameter.parameter_name.len() > 64
        || !parameter
            .parameter_name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err("template_named_parameter_invalid".into());
    }
    if parameter.text.trim().is_empty() {
        return Err("template_body_parameter_empty".into());
    }
    if parameter.text.chars().count() > MAX_REPLY_TEXT {
        return Err("template_body_parameter_too_long".into());
    }
    Ok(())
}

fn validate_template_header(header: &TemplateHeader) -> Result<(), String> {
    match header {
        TemplateHeader::Text { text } => {
            if text.trim().is_empty() || text.chars().count() > 60 {
                return Err("template_header_text_invalid".into());
            }
        }
        TemplateHeader::Image { media }
        | TemplateHeader::Video { media }
        | TemplateHeader::Document { media } => media.validate()?,
    }
    Ok(())
}

fn validate_template_button(button: &TemplateButton) -> Result<(), String> {
    let index = match button {
        TemplateButton::QuickReply { index, payload } => {
            if payload.is_empty() || payload.len() > 128 {
                return Err("template_button_payload_invalid".into());
            }
            *index
        }
        TemplateButton::Url { index, text } => {
            if text.is_empty() || text.len() > 2048 {
                return Err("template_button_url_invalid".into());
            }
            *index
        }
        TemplateButton::PhoneNumber { index } => *index,
        TemplateButton::CopyCode { index, coupon_code } => {
            let n = coupon_code.chars().count();
            if !(4..=15).contains(&n) {
                return Err("template_coupon_code_invalid".into());
            }
            *index
        }
    };
    if index > 9 {
        return Err("template_button_index_invalid".into());
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

fn validate_coordinates(latitude: &str, longitude: &str) -> Result<(), String> {
    let lat: f64 = latitude
        .parse()
        .map_err(|_| "location_coordinates_invalid".to_string())?;
    let lon: f64 = longitude
        .parse()
        .map_err(|_| "location_coordinates_invalid".to_string())?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err("location_coordinates_invalid".into());
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
                named_body_parameters: vec![],
                header: None,
                buttons: vec![],
                limited_time_offer: None,
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
            recipient_type: RecipientType::Individual,
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
            recipient_type: RecipientType::Individual,
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

    #[test]
    fn location_contacts_address_and_reaction_validate_locally() {
        assert!(WhatsAppMessage::Location {
            to: "60123456789".into(),
            latitude: "3.139".into(),
            longitude: "101.687".into(),
            name: Some("KL".into()),
            address: None,
            reply_to_message_id: None,
        }
        .validate()
        .is_ok());
        assert_eq!(
            WhatsAppMessage::Location {
                to: "60123456789".into(),
                latitude: "91".into(),
                longitude: "0".into(),
                name: None,
                address: None,
                reply_to_message_id: None,
            }
            .validate()
            .unwrap_err(),
            "location_coordinates_invalid"
        );
        assert_eq!(
            WhatsAppMessage::Contacts {
                to: "60123456789".into(),
                contacts: vec![],
                reply_to_message_id: None,
            }
            .validate()
            .unwrap_err(),
            "contacts_empty"
        );
        assert!(WhatsAppMessage::Contacts {
            to: "60123456789".into(),
            contacts: vec![OutboundContact {
                formatted_name: "Ada".into(),
                phones: vec!["6011".into()],
            }],
            reply_to_message_id: None,
        }
        .validate()
        .is_ok());
        assert_eq!(
            WhatsAppMessage::AddressRequest {
                to: "60123456789".into(),
                body: "Share address".into(),
                country: "MYS".into(),
                reply_to_message_id: None,
            }
            .validate()
            .unwrap_err(),
            "address_country_invalid"
        );
        assert!(WhatsAppMessage::AddressRequest {
            to: "60123456789".into(),
            body: "Share address".into(),
            country: "MY".into(),
            reply_to_message_id: None,
        }
        .validate()
        .is_ok());
        assert_eq!(
            WhatsAppMessage::Reaction {
                to: "60123456789".into(),
                message_id: "wamid.in".into(),
                emoji: "   ".into(),
            }
            .validate()
            .unwrap_err(),
            "reaction_emoji_empty"
        );
        assert!(WhatsAppMessage::Reaction {
            to: "60123456789".into(),
            message_id: "wamid.in".into(),
            emoji: "thumbs".into(),
        }
        .validate()
        .is_ok());
        assert!(WhatsAppMessage::MarkRead {
            message_id: "wamid.in".into(),
        }
        .validate()
        .is_ok());
        assert!(WhatsAppMessage::Typing {
            message_id: "wamid.in".into(),
        }
        .validate()
        .is_ok());
        assert_eq!(
            WhatsAppMessage::MarkRead {
                message_id: "wamid.in".into(),
            }
            .validate_for(RecipientType::Group)
            .unwrap_err(),
            "status_ack_not_group"
        );
        assert!(WhatsAppMessage::Text {
            to: "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD".into(),
            text: "hello group".into(),
            preview_url: false,
        }
        .validate_for(RecipientType::Group)
        .is_ok());
        assert_eq!(
            WhatsAppMessage::Text {
                to: "not a group".into(),
                text: "hello".into(),
                preview_url: false,
            }
            .validate_for(RecipientType::Group)
            .unwrap_err(),
            "group_id_invalid"
        );
    }

    #[test]
    fn template_send_components_validate_named_header_and_buttons() {
        let ok = WhatsAppMessage::Template {
            to: "60123456789".into(),
            name: "order_update".into(),
            language: "en_US".into(),
            body_parameters: vec![],
            named_body_parameters: vec![NamedBodyParameter {
                parameter_name: "first_name".into(),
                text: "Ada".into(),
            }],
            header: Some(TemplateHeader::Text {
                text: "Hello".into(),
            }),
            buttons: vec![
                TemplateButton::QuickReply {
                    index: 0,
                    payload: "yes".into(),
                },
                TemplateButton::Url {
                    index: 1,
                    text: "ada".into(),
                },
                TemplateButton::CopyCode {
                    index: 2,
                    coupon_code: "SAVE10".into(),
                },
            ],
            limited_time_offer: Some(LimitedTimeOffer {
                expiration_time_ms: 1_700_000_000_000,
            }),
        };
        assert!(ok.validate().is_ok());
        assert_eq!(
            WhatsAppMessage::Template {
                to: "60123456789".into(),
                name: "order_update".into(),
                language: "en_US".into(),
                body_parameters: vec!["Ada".into()],
                named_body_parameters: vec![NamedBodyParameter {
                    parameter_name: "first_name".into(),
                    text: "Ada".into(),
                }],
                header: None,
                buttons: vec![],
                limited_time_offer: None,
            }
            .validate()
            .unwrap_err(),
            "template_body_parameters_mixed"
        );
    }
}
