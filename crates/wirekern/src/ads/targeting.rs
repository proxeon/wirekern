//! Paused campaign/ad-set payloads, targeting, placements, and promoted objects.
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use super::helpers::{require_https_url, require_numeric_id};
use super::{BidStrategy, BillingEvent, CampaignObjective, OptimizationGoal};

/// A campaign draft. Status is intentionally absent: the connector adds the
/// only allowed value, `PAUSED`, rather than trusting a caller-provided flag.
///
/// Budget lives at **either** this campaign (Advantage campaign budget / CBO)
/// **or** each child ad set, never both: Meta's create docs say you can set
/// `daily_budget` / `lifetime_budget` at one level. `None`/`None` keeps the
/// historical ad-set-budget path. Sharing (`is_adset_budget_sharing_enabled`)
/// is Meta's up-to-20% ABO child-share flag (v24.0+ required when the
/// campaign has no budget). It is incompatible with a campaign budget
/// (Marketing error 4834002).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PausedCampaign {
    pub name: String,
    pub objective: CampaignObjective,
    #[serde(default)]
    pub special_ad_categories: Vec<String>,
    /// Campaign-level daily budget in account minor units. XOR with
    /// `lifetime_budget`. Setting either makes this a CBO campaign.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_budget: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime_budget: Option<u64>,
    /// Meta `is_adset_budget_sharing_enabled`. Default `false` is independent
    /// ad-set budgets. `true` is ABO sharing and is refused with CBO.
    #[serde(default)]
    pub is_adset_budget_sharing_enabled: bool,
}

/// ISO 3166-1 alpha-2 countries plus optional age and placement lists.
/// Unknown keys are refused so this is not a Graph targeting hatch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdTargeting {
    pub geo_locations: GeoLocations,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_min: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_max: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publisher_platforms: Vec<PublisherPlatform>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facebook_positions: Vec<FacebookPosition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instagram_positions: Vec<InstagramPosition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub whatsapp_positions: Vec<WhatsAppPosition>,
    /// Explicit when WhatsApp Status is selected so Meta's default `true`
    /// cannot silently expand to unknown-age users (v26.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_age_unknown: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeoLocations {
    pub countries: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublisherPlatform {
    Facebook,
    Instagram,
    AudienceNetwork,
    Messenger,
    Whatsapp,
}

impl PublisherPlatform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Facebook => "facebook",
            Self::Instagram => "instagram",
            Self::AudienceNetwork => "audience_network",
            Self::Messenger => "messenger",
            Self::Whatsapp => "whatsapp",
        }
    }
}

impl FromStr for PublisherPlatform {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "facebook" => Ok(Self::Facebook),
            "instagram" => Ok(Self::Instagram),
            "audience_network" => Ok(Self::AudienceNetwork),
            "messenger" => Ok(Self::Messenger),
            "whatsapp" => Ok(Self::Whatsapp),
            other => Err(format!("unknown_publisher_platform:{other}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacebookPosition {
    Feed,
    Story,
    Reels,
}

impl FacebookPosition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Feed => "feed",
            Self::Story => "story",
            Self::Reels => "reels",
        }
    }
}

impl FromStr for FacebookPosition {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "feed" => Ok(Self::Feed),
            "story" => Ok(Self::Story),
            "reels" => Ok(Self::Reels),
            other => Err(format!("unknown_facebook_position:{other}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstagramPosition {
    Stream,
    Story,
    Reels,
}

impl InstagramPosition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stream => "stream",
            Self::Story => "story",
            Self::Reels => "reels",
        }
    }
}

impl FromStr for InstagramPosition {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stream" => Ok(Self::Stream),
            "story" => Ok(Self::Story),
            "reels" => Ok(Self::Reels),
            other => Err(format!("unknown_instagram_position:{other}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhatsAppPosition {
    Status,
}

impl WhatsAppPosition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
        }
    }
}

impl FromStr for WhatsAppPosition {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "status" => Ok(Self::Status),
            other => Err(format!("unknown_whatsapp_position:{other}")),
        }
    }
}

impl AdTargeting {
    pub fn validate(&self) -> Result<(), String> {
        if self.geo_locations.countries.is_empty() {
            return Err("targeting_missing_country".into());
        }
        for country in &self.geo_locations.countries {
            if country.len() != 2 || !country.chars().all(|c| c.is_ascii_uppercase()) {
                return Err(format!("bad_country_code:{country}"));
            }
        }
        if let Some(min) = self.age_min {
            if !(13..=65).contains(&min) {
                return Err(format!("age_min_out_of_range:{min}"));
            }
        }
        if let Some(max) = self.age_max {
            if !(13..=65).contains(&max) {
                return Err(format!("age_max_out_of_range:{max}"));
            }
        }
        if let (Some(min), Some(max)) = (self.age_min, self.age_max) {
            if min > max {
                return Err("age_min_after_age_max".into());
            }
        }
        let has_facebook = self.publisher_platforms.is_empty()
            || self
                .publisher_platforms
                .contains(&PublisherPlatform::Facebook);
        if !self.facebook_positions.is_empty() && !has_facebook {
            return Err("facebook_positions_without_facebook_platform".into());
        }
        let has_instagram = self.publisher_platforms.is_empty()
            || self
                .publisher_platforms
                .contains(&PublisherPlatform::Instagram);
        if !self.instagram_positions.is_empty() && !has_instagram {
            return Err("instagram_positions_without_instagram_platform".into());
        }
        let has_whatsapp = self
            .publisher_platforms
            .contains(&PublisherPlatform::Whatsapp);
        if !self.whatsapp_positions.is_empty() && !has_whatsapp {
            return Err("whatsapp_positions_without_whatsapp_platform".into());
        }
        if self.whatsapp_positions.contains(&WhatsAppPosition::Status) {
            // v26.0: Status is not a standalone placement; Instagram Stories
            // must be selected with it.
            let has_ig_story = self
                .publisher_platforms
                .contains(&PublisherPlatform::Instagram)
                && self.instagram_positions.contains(&InstagramPosition::Story);
            if !has_ig_story {
                return Err("whatsapp_status_requires_instagram_story".into());
            }
            if self.user_age_unknown.is_none() {
                return Err("whatsapp_status_requires_user_age_unknown".into());
            }
        }
        Ok(())
    }
}

/// An ad-set draft. Budgets are the ad account's minor currency unit
/// (for ILS, agorot), matching Meta's integer Marketing API fields exactly.
///
/// `daily_budget` XOR `lifetime_budget`. Both omitted is valid only as a CBO
/// child (the parent campaign holds the budget). Lifetime still needs an
/// `end_time`; that check lands with the typed schedule fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PausedAdset {
    pub name: String,
    pub campaign_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_budget: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime_budget: Option<u64>,
    pub bid_strategy: BidStrategy,
    /// Required for `LOWEST_COST_WITH_BID_CAP` and `COST_CAP`. Refused on
    /// `LOWEST_COST_WITHOUT_CAP` so a leftover cap cannot hitch a ride.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid_amount: Option<u64>,
    /// Meta `bid_constraints.roas_average_floor`. 10000 = 1.0 ROAS.
    /// Required for `LOWEST_COST_WITH_MIN_ROAS`; refused on every other
    /// strategy (`bid_amount` and this field are mutually exclusive at Meta).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roas_average_floor: Option<u64>,
    pub billing_event: BillingEvent,
    pub optimization_goal: OptimizationGoal,
    pub targeting: AdTargeting,
    /// RFC3339 (Meta also accepts a space instead of `T` and `±HHMM` offsets).
    /// Optional; omitted means Graph starts delivery when the object is later
    /// activated. Compared locally when `end_time` is also set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    /// Required with `lifetime_budget` (Meta will not accept an open-ended
    /// lifetime spend). Must be after `start_time` when both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    /// What the ad set promotes. Required for conversion/app/Page/value
    /// goals; Meta infers conversion specs from this and ignores extras.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_object: Option<PromotedObject>,
}

/// Standard pixel/app events we will send as `custom_event_type`. Unlisted
/// Meta names stay out until they have a reviewed pairing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CustomEventType {
    Purchase,
    Lead,
    CompleteRegistration,
    AddToCart,
    InitiatedCheckout,
    AddPaymentInfo,
    ContentView,
    Search,
    Subscribe,
    Contact,
    Other,
}

impl CustomEventType {
    pub fn meta_value(self) -> &'static str {
        match self {
            Self::Purchase => "PURCHASE",
            Self::Lead => "LEAD",
            Self::CompleteRegistration => "COMPLETE_REGISTRATION",
            Self::AddToCart => "ADD_TO_CART",
            Self::InitiatedCheckout => "INITIATED_CHECKOUT",
            Self::AddPaymentInfo => "ADD_PAYMENT_INFO",
            Self::ContentView => "CONTENT_VIEW",
            Self::Search => "SEARCH",
            Self::Subscribe => "SUBSCRIBE",
            Self::Contact => "CONTACT",
            Self::Other => "OTHER",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Purchase => "purchase",
            Self::Lead => "lead",
            Self::CompleteRegistration => "complete_registration",
            Self::AddToCart => "add_to_cart",
            Self::InitiatedCheckout => "initiated_checkout",
            Self::AddPaymentInfo => "add_payment_info",
            Self::ContentView => "content_view",
            Self::Search => "search",
            Self::Subscribe => "subscribe",
            Self::Contact => "contact",
            Self::Other => "other",
        }
    }
}

impl FromStr for CustomEventType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "purchase" | "PURCHASE" => Ok(Self::Purchase),
            "lead" | "LEAD" => Ok(Self::Lead),
            "complete_registration" | "COMPLETE_REGISTRATION" => Ok(Self::CompleteRegistration),
            "add_to_cart" | "ADD_TO_CART" => Ok(Self::AddToCart),
            "initiate_checkout" | "initiated_checkout" | "INITIATED_CHECKOUT" => {
                Ok(Self::InitiatedCheckout)
            }
            "add_payment_info" | "ADD_PAYMENT_INFO" => Ok(Self::AddPaymentInfo),
            "content_view" | "CONTENT_VIEW" => Ok(Self::ContentView),
            "search" | "SEARCH" => Ok(Self::Search),
            "subscribe" | "SUBSCRIBE" => Ok(Self::Subscribe),
            "contact" | "CONTACT" => Ok(Self::Contact),
            "other" | "OTHER" => Ok(Self::Other),
            other => Err(format!("unknown_custom_event_type:{other}")),
        }
    }
}

/// Closed promoted-object shapes from Meta's Ad Promoted Object reference.
/// Internally tagged so a manifest cannot mix `page_id` with `pixel_id`.
/// The connector serializes the Meta-facing object (no `kind` key).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromotedObject {
    Page {
        page_id: String,
    },
    Pixel {
        pixel_id: String,
        custom_event_type: CustomEventType,
    },
    App {
        application_id: String,
        object_store_url: String,
    },
    ProductSet {
        product_set_id: String,
        custom_event_type: CustomEventType,
    },
}

impl PromotedObject {
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Page { .. } => "page",
            Self::Pixel { .. } => "pixel",
            Self::App { .. } => "app",
            Self::ProductSet { .. } => "product_set",
        }
    }

    /// JSON Meta's `promoted_object` field expects (ids + event, no tag).
    pub fn meta_json(&self) -> serde_json::Value {
        match self {
            Self::Page { page_id } => serde_json::json!({ "page_id": page_id }),
            Self::Pixel {
                pixel_id,
                custom_event_type,
            } => serde_json::json!({
                "pixel_id": pixel_id,
                "custom_event_type": custom_event_type.meta_value(),
            }),
            Self::App {
                application_id,
                object_store_url,
            } => serde_json::json!({
                "application_id": application_id,
                "object_store_url": object_store_url,
            }),
            Self::ProductSet {
                product_set_id,
                custom_event_type,
            } => serde_json::json!({
                "product_set_id": product_set_id,
                "custom_event_type": custom_event_type.meta_value(),
            }),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Page { page_id } => require_numeric_id("page_id", page_id),
            Self::Pixel { pixel_id, .. } => require_numeric_id("pixel_id", pixel_id),
            Self::App {
                application_id,
                object_store_url,
            } => {
                require_numeric_id("application_id", application_id)?;
                require_https_url("object_store_url", object_store_url)
            }
            Self::ProductSet { product_set_id, .. } => {
                require_numeric_id("product_set_id", product_set_id)
            }
        }
    }
}

/// Conversion/app/Page/value goals need a promoted object on create.
pub fn promoted_object_required(goal: OptimizationGoal) -> bool {
    matches!(
        goal,
        OptimizationGoal::OffsiteConversions
            | OptimizationGoal::AppInstalls
            | OptimizationGoal::PageLikes
            | OptimizationGoal::Value
            | OptimizationGoal::LeadGeneration
    )
}

fn promoted_object_matches_goal(goal: OptimizationGoal, object: &PromotedObject) -> bool {
    matches!(
        (goal, object),
        (OptimizationGoal::PageLikes, PromotedObject::Page { .. })
            | (OptimizationGoal::AppInstalls, PromotedObject::App { .. })
            | (
                OptimizationGoal::OffsiteConversions,
                PromotedObject::Pixel { .. }
            )
            | (
                OptimizationGoal::Value,
                PromotedObject::Pixel { .. } | PromotedObject::ProductSet { .. }
            )
            | (
                OptimizationGoal::LeadGeneration,
                PromotedObject::Page { .. } | PromotedObject::Pixel { .. }
            )
    )
}

pub fn validate_promoted_object(
    goal: OptimizationGoal,
    object: Option<&PromotedObject>,
) -> Result<(), String> {
    match object {
        None if promoted_object_required(goal) => {
            Err(format!("promoted_object_required:{}", goal.as_str()))
        }
        None => Ok(()),
        Some(object) => {
            object.validate()?;
            if promoted_object_required(goal) && !promoted_object_matches_goal(goal, object) {
                return Err(format!(
                    "promoted_object_kind_mismatch:{}:{}",
                    goal.as_str(),
                    object.kind_str()
                ));
            }
            Ok(())
        }
    }
}
