//! Typed WhatsApp Cloud messaging contracts.
//!
//! A recipient, a template and a reply context have materially different
//! consent/billing semantics from a public social post. Keeping them in a
//! dedicated type prevents a generic `Intent.params` map from silently
//! growing into an unreviewed messaging payload escape hatch.

use crate::types::{valid_name, Capability, Site};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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
    /// Interactive `catalog_message`. Requires a Meta catalog connected to
    /// the WABA. Optional thumbnail is a catalog product retailer id.
    Catalog {
        to: String,
        body: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thumbnail_product_retailer_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        footer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    /// Single product from a connected catalog.
    Product {
        to: String,
        catalog_id: String,
        product_retailer_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        footer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    /// Multi-product list. Header is required by Cloud API.
    ProductList {
        to: String,
        catalog_id: String,
        header: String,
        body: String,
        sections: Vec<ProductSection>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        footer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    /// Interactive `order_status`. Status values are Meta's order lifecycle
    /// strings (pending/processing/shipped/completed/canceled/…).
    OrderStatus {
        to: String,
        body: String,
        reference_id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    /// Interactive Flow CTA. `flow_id` XOR `flow_name`. Flow JSON schema
    /// publishing is a separate WABA endpoint, not this send.
    Flow {
        to: String,
        body: String,
        flow_cta: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        flow_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        flow_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        footer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        flow_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screen: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_message_id: Option<String>,
    },
    /// Mark an inbound `wamid` as read. Cloud API returns `{success: true}`,
    /// not a new outbound wamid. `Outcome.id` is this inbound id so the local
    /// idempotency ledger still has a stable value.
    MarkRead { message_id: String },
    /// Typing indicator. Official docs always pair it with mark-as-read on
    /// the same inbound `wamid` (`status: read` + `typing_indicator`). It
    /// dismisses after 25s or the next send, whichever is first.
    Typing { message_id: String },
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
pub struct ProductSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub product_retailer_ids: Vec<String>,
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
    Text {
        text: String,
    },
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
            Self::Catalog { .. }
            | Self::Product { .. }
            | Self::ProductList { .. }
            | Self::OrderStatus { .. } => Capability::SendCatalog,
            Self::Flow { .. } => Capability::SendFlow,
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
                let mut ids = HashSet::new();
                for b in buttons {
                    if b.id.is_empty() || b.id.len() > 256 {
                        return Err("reply_button_id_invalid".into());
                    }
                    if !ids.insert(b.id.as_str()) {
                        return Err("reply_button_id_duplicate".into());
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
                // Cloud API: up to 10 sections, but only 10 rows across all
                // sections combined — not 10 per section.
                let mut total_rows = 0usize;
                let mut ids = HashSet::new();
                for section in sections {
                    if sections.len() > 1
                        && section
                            .title
                            .as_ref()
                            .map_or(true, |t| t.trim().is_empty() || t.chars().count() > 24)
                    {
                        return Err("list_section_title_invalid".into());
                    }
                    if let Some(title) = &section.title {
                        if title.chars().count() > 24 {
                            return Err("list_section_title_invalid".into());
                        }
                    }
                    if section.rows.is_empty() {
                        return Err("list_rows_count".into());
                    }
                    total_rows += section.rows.len();
                    if total_rows > 10 {
                        return Err("list_rows_count".into());
                    }
                    for row in &section.rows {
                        if row.id.is_empty() || row.id.len() > 200 {
                            return Err("list_row_id_invalid".into());
                        }
                        if !ids.insert(row.id.as_str()) {
                            return Err("list_row_id_duplicate".into());
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
            Self::Catalog {
                to,
                body,
                thumbnail_product_retailer_id,
                footer,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                validate_body(body)?;
                validate_optional_footer(footer)?;
                validate_optional_context(reply_to_message_id)?;
                if let Some(id) = thumbnail_product_retailer_id {
                    validate_catalog_id(id, "catalog_product_id_invalid")?;
                }
            }
            Self::Product {
                to,
                catalog_id,
                product_retailer_id,
                body,
                footer,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                validate_catalog_id(catalog_id, "catalog_id_invalid")?;
                validate_catalog_id(product_retailer_id, "catalog_product_id_invalid")?;
                if let Some(body) = body {
                    validate_body(body)?;
                }
                validate_optional_footer(footer)?;
                validate_optional_context(reply_to_message_id)?;
            }
            Self::ProductList {
                to,
                catalog_id,
                header,
                body,
                sections,
                footer,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                validate_catalog_id(catalog_id, "catalog_id_invalid")?;
                validate_optional_header(&Some(header.clone()))?;
                if header.trim().is_empty() {
                    return Err("interactive_header_invalid".into());
                }
                validate_body(body)?;
                validate_optional_footer(footer)?;
                validate_optional_context(reply_to_message_id)?;
                validate_product_sections(sections)?;
            }
            Self::OrderStatus {
                to,
                body,
                reference_id,
                status,
                description,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                validate_body(body)?;
                validate_optional_context(reply_to_message_id)?;
                if reference_id.trim().is_empty() || reference_id.len() > 256 {
                    return Err("order_reference_invalid".into());
                }
                match status.as_str() {
                    "pending" | "processing" | "partially_shipped" | "shipped" | "completed"
                    | "canceled" => {}
                    _ => return Err("order_status_invalid".into()),
                }
                if description
                    .as_ref()
                    .is_some_and(|d| d.is_empty() || d.chars().count() > 120)
                {
                    return Err("order_description_invalid".into());
                }
            }
            Self::Flow {
                to,
                body,
                flow_cta,
                flow_id,
                flow_name,
                header,
                footer,
                flow_token: _,
                screen: _,
                reply_to_message_id,
            } => {
                validate_destination(to, recipient_type)?;
                validate_body(body)?;
                validate_optional_header(header)?;
                validate_optional_footer(footer)?;
                validate_optional_context(reply_to_message_id)?;
                if flow_cta.is_empty() || flow_cta.chars().count() > 20 {
                    return Err("flow_cta_invalid".into());
                }
                match (flow_id.as_deref(), flow_name.as_deref()) {
                    (Some(id), None) => validate_catalog_id(id, "flow_id_invalid")?,
                    (None, Some(name)) => {
                        if name.is_empty() || name.len() > 256 {
                            return Err("flow_name_invalid".into());
                        }
                    }
                    _ => return Err("flow_id_or_name_required".into()),
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

/// Business Management API template row. Components stay off this record
/// so a get/list cannot become an untyped JSON dump.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppTemplateRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Meta `quality_score.score`: GREEN / YELLOW / RED / UNKNOWN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppTemplateQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Opaque Graph cursor from the preceding page. It is deliberately not
    /// interpreted or reconstructed by Postkit: Meta owns its format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

impl WhatsAppTemplateQuery {
    pub fn validate_page(&self) -> Result<(), String> {
        WhatsAppPageQuery {
            limit: self.limit,
            after: self.after.clone(),
        }
        .validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppTemplateList {
    pub templates: Vec<WhatsAppTemplateRecord>,
    /// Cursor to pass back as `query.after`; absent when Meta has no next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// Cursor and bounded page size shared by Flow, WABA, phone, and system-user
/// reads. A cursor is untrusted opaque data returned by Meta, so Postkit only
/// bounds it before returning it to the corresponding Graph edge.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppPageQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// One configured outbound Cloud API phone. The alias is a local operator
/// choice, not a Meta identifier; sends select this alias instead of allowing
/// an arbitrary phone-number ID to be slipped into a request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppOutboundSender {
    pub alias: String,
    pub phone_number_id: String,
}

impl WhatsAppOutboundSender {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_name(&self.alias) || self.alias == "primary" {
            return Err("whatsapp_sender_alias_invalid".into());
        }
        if self.phone_number_id.is_empty()
            || self.phone_number_id.len() > 32
            || !self
                .phone_number_id
                .bytes()
                .all(|byte| byte.is_ascii_digit())
        {
            return Err("whatsapp_sender_phone_number_id_invalid".into());
        }
        Ok(())
    }
}

impl WhatsAppPageQuery {
    pub fn validate(&self) -> Result<(), String> {
        if self.limit.is_some_and(|value| value == 0 || value > 100) {
            return Err("whatsapp_page_limit_invalid".into());
        }
        if self
            .after
            .as_deref()
            .is_some_and(|cursor| cursor.is_empty() || cursor.len() > 1_024)
        {
            return Err("whatsapp_page_after_invalid".into());
        }
        Ok(())
    }
}

/// Typed template create/edit. Create auto-submits for Meta review — there
/// is no separate "review submit" verb on Cloud API.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppTemplateDraft {
    pub name: String,
    pub language: String,
    pub category: String,
    #[serde(default)]
    pub parameter_format: ParameterFormat,
    pub components: Vec<TemplateCreateComponent>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterFormat {
    #[default]
    Positional,
    Named,
}

impl ParameterFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Positional => "positional",
            Self::Named => "named",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TemplateCreateComponent {
    Header {
        format: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        example_handle: Option<String>,
    },
    Body {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        example: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        named_example: Vec<NamedBodyParameter>,
    },
    /// Footer is defined here, not at send time.
    Footer {
        text: String,
    },
    Buttons {
        buttons: Vec<TemplateCreateButton>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TemplateCreateButton {
    QuickReply { text: String },
    Url { text: String, url: String },
    PhoneNumber { text: String, phone_number: String },
    CopyCode { example: String },
}

impl WhatsAppTemplateDraft {
    pub fn validate(&self) -> Result<(), String> {
        validate_template_name(&self.name)?;
        validate_language(&self.language)?;
        match self.category.as_str() {
            "marketing" | "utility" | "authentication" => {}
            _ => return Err("template_category_invalid".into()),
        }
        if self.components.is_empty() {
            return Err("template_components_empty".into());
        }
        let mut saw_body = false;
        for component in &self.components {
            match component {
                TemplateCreateComponent::Header {
                    format,
                    text,
                    example_handle,
                } => match format.as_str() {
                    "TEXT" => {
                        if text
                            .as_ref()
                            .map_or(true, |t| t.is_empty() || t.chars().count() > 60)
                        {
                            return Err("template_header_text_invalid".into());
                        }
                    }
                    "IMAGE" | "VIDEO" | "DOCUMENT" => {
                        if example_handle.as_ref().map_or(true, |h| h.is_empty()) {
                            return Err("template_header_handle_required".into());
                        }
                    }
                    _ => return Err("template_header_format_invalid".into()),
                },
                TemplateCreateComponent::Body {
                    text,
                    example,
                    named_example,
                } => {
                    saw_body = true;
                    if text.trim().is_empty() || text.chars().count() > MAX_REPLY_TEXT {
                        return Err("template_body_invalid".into());
                    }
                    if !example.is_empty() && !named_example.is_empty() {
                        return Err("template_body_parameters_mixed".into());
                    }
                }
                TemplateCreateComponent::Footer { text } => {
                    if text.trim().is_empty() || text.chars().count() > 60 {
                        return Err("template_footer_invalid".into());
                    }
                }
                TemplateCreateComponent::Buttons { buttons } => {
                    if buttons.is_empty() || buttons.len() > 10 {
                        return Err("template_buttons_too_many".into());
                    }
                    for button in buttons {
                        validate_create_button(button)?;
                    }
                }
            }
        }
        if !saw_body {
            return Err("template_body_required".into());
        }
        Ok(())
    }
}

/// WABA Flow publishing-state row. The Flow JSON schema is not echoed back
/// so a get cannot become an untyped dump.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppFlowRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppFlowList {
    pub flows: Vec<WhatsAppFlowRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// Create a Flow. `flow_json` must be a JSON object with a `version` field —
/// Meta's Flow schema, not an arbitrary Graph payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppFlowDraft {
    pub name: String,
    pub categories: Vec<String>,
    pub flow_json: String,
}

impl WhatsAppFlowDraft {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() || self.name.len() > 256 {
            return Err("flow_name_invalid".into());
        }
        if self.categories.is_empty() {
            return Err("flow_categories_empty".into());
        }
        let parsed: serde_json::Value =
            serde_json::from_str(&self.flow_json).map_err(|_| "flow_json_invalid".to_string())?;
        if !parsed.is_object() || parsed.get("version").is_none() {
            return Err("flow_json_missing_version".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppWaba {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A WABA page, rather than a bare vector, prevents a successful but partial
/// Graph read from looking complete to CLI or embedding callers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppWabaList {
    pub wabas: Vec<WhatsAppWaba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppSystemUser {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppSystemUserList {
    pub users: Vec<WhatsAppSystemUser>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppPhoneNumber {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_phone_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_rating: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_limit_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_verification_status: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppPhoneNumberList {
    pub phone_numbers: Vec<WhatsAppPhoneNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn validate_two_step_pin(pin: &str) -> Result<(), String> {
    if pin.len() != 6 || !pin.bytes().all(|b| b.is_ascii_digit()) {
        return Err("whatsapp_pin_invalid".into());
    }
    Ok(())
}

/// Embedded Signup is Facebook Login for Business, not Cloud API messaging.
/// Builds the documented start URL only; it does not run OAuth.
pub fn embedded_signup_url(
    app_id: &str,
    config_id: &str,
    redirect_uri: &str,
) -> Result<String, String> {
    if app_id.is_empty()
        || !app_id.bytes().all(|b| b.is_ascii_digit())
        || config_id.is_empty()
        || !redirect_uri.starts_with("https://")
    {
        return Err("embedded_signup_params_invalid".into());
    }
    Ok(format!(
        "https://www.facebook.com/v26.0/dialog/oauth?client_id={app_id}&config_id={config_id}&response_type=code&override_default_response_type=true&redirect_uri={}",
        percent_encode(redirect_uri)
    ))
}

fn validate_create_button(button: &TemplateCreateButton) -> Result<(), String> {
    match button {
        TemplateCreateButton::QuickReply { text }
        | TemplateCreateButton::Url { text, .. }
        | TemplateCreateButton::PhoneNumber { text, .. } => {
            if text.is_empty() || text.chars().count() > 25 {
                return Err("template_button_text_invalid".into());
            }
        }
        TemplateCreateButton::CopyCode { example } => {
            let n = example.chars().count();
            if !(4..=15).contains(&n) {
                return Err("template_coupon_code_invalid".into());
            }
        }
    }
    if let TemplateCreateButton::Url { url, .. } = button {
        if !url.starts_with("https://") {
            return Err("template_button_url_must_be_https".into());
        }
    }
    Ok(())
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
        // Meta's table writes `video/3gp`; IANA is `video/3gpp`. Accept both.
        "video/mp4" | "video/3gp" | "video/3gpp" => Some(16 * 1024 * 1024),
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
fn validate_catalog_id(value: &str, err: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '/' || c == '\\')
    {
        return Err(err.into());
    }
    Ok(())
}

fn validate_product_sections(sections: &[ProductSection]) -> Result<(), String> {
    if sections.is_empty() || sections.len() > 10 {
        return Err("product_sections_count".into());
    }
    let mut total = 0usize;
    for section in sections {
        if section
            .title
            .as_ref()
            .is_some_and(|t| t.chars().count() > 24)
        {
            return Err("product_section_title_invalid".into());
        }
        if section.product_retailer_ids.is_empty() {
            return Err("product_section_empty".into());
        }
        total += section.product_retailer_ids.len();
        if total > 30 {
            return Err("product_items_too_many".into());
        }
        for id in &section.product_retailer_ids {
            validate_catalog_id(id, "catalog_product_id_invalid")?;
        }
    }
    Ok(())
}

fn validate_group_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '/' || c == '\\')
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

pub fn validate_template_name(value: &str) -> Result<(), String> {
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
        let draft = WhatsAppTemplateDraft {
            name: "order_update".into(),
            language: "en_US".into(),
            category: "utility".into(),
            parameter_format: ParameterFormat::Positional,
            components: vec![
                TemplateCreateComponent::Body {
                    text: "Hi {{1}}".into(),
                    example: vec!["Ada".into()],
                    named_example: vec![],
                },
                TemplateCreateComponent::Footer {
                    text: "Thanks".into(),
                },
            ],
        };
        assert!(draft.validate().is_ok());
        assert_eq!(
            WhatsAppTemplateDraft {
                name: "order_update".into(),
                language: "en_US".into(),
                category: "utility".into(),
                parameter_format: ParameterFormat::Positional,
                components: vec![TemplateCreateComponent::Footer {
                    text: "Thanks".into(),
                }],
            }
            .validate()
            .unwrap_err(),
            "template_body_required"
        );
        assert!(WhatsAppMessage::Catalog {
            to: "60123456789".into(),
            body: "See catalog".into(),
            thumbnail_product_retailer_id: None,
            footer: None,
            reply_to_message_id: None,
        }
        .validate()
        .is_ok());
        assert_eq!(
            WhatsAppMessage::OrderStatus {
                to: "60123456789".into(),
                body: "Update".into(),
                reference_id: "ord-1".into(),
                status: "unknown".into(),
                description: None,
                reply_to_message_id: None,
            }
            .validate()
            .unwrap_err(),
            "order_status_invalid"
        );
        assert_eq!(
            WhatsAppMessage::Flow {
                to: "60123456789".into(),
                body: "Book".into(),
                flow_cta: "Open".into(),
                flow_id: None,
                flow_name: None,
                header: None,
                footer: None,
                flow_token: None,
                screen: None,
                reply_to_message_id: None,
            }
            .validate()
            .unwrap_err(),
            "flow_id_or_name_required"
        );
        let too_many_rows = WhatsAppMessage::List {
            to: "60123456789".into(),
            body: "Menu".into(),
            button: "Open".into(),
            sections: vec![
                ListSection {
                    title: Some("A".into()),
                    rows: (0..6)
                        .map(|i| ListRow {
                            id: format!("a{i}"),
                            title: format!("A{i}"),
                            description: None,
                        })
                        .collect(),
                },
                ListSection {
                    title: Some("B".into()),
                    rows: (0..5)
                        .map(|i| ListRow {
                            id: format!("b{i}"),
                            title: format!("B{i}"),
                            description: None,
                        })
                        .collect(),
                },
            ],
            header: None,
            footer: None,
            reply_to_message_id: None,
        };
        assert_eq!(too_many_rows.validate().unwrap_err(), "list_rows_count");
        assert!(validate_two_step_pin("123456").is_ok());
        assert_eq!(
            validate_two_step_pin("abc").unwrap_err(),
            "whatsapp_pin_invalid"
        );
        let signup = embedded_signup_url("123", "cfg_1", "https://example.com/cb?x=1").unwrap();
        assert!(signup.contains("redirect_uri=https%3A%2F%2Fexample.com%2Fcb%3Fx%3D1"));
        assert_eq!(
            embedded_signup_url("123", "cfg", "http://insecure.example/x").unwrap_err(),
            "embedded_signup_params_invalid"
        );
        assert!(WhatsAppFlowDraft {
            name: "booking".into(),
            categories: vec!["OTHER".into()],
            flow_json: r#"{"version":"7.0"}"#.into(),
        }
        .validate()
        .is_ok());
        assert_eq!(
            WhatsAppPageQuery {
                limit: Some(0),
                after: None,
            }
            .validate()
            .unwrap_err(),
            "whatsapp_page_limit_invalid"
        );
        assert_eq!(
            WhatsAppOutboundSender {
                alias: "primary".into(),
                phone_number_id: "123456789".into(),
            }
            .validate()
            .unwrap_err(),
            "whatsapp_sender_alias_invalid"
        );
    }
}
