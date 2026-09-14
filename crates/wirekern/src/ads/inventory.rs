//! Account-scoped inventory kinds and spend-shaped inspect replies.
use crate::types::Site;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use super::helpers::{require_numeric_id, validate_account};

/// The three entities a Tier B request can create. `Adset` follows Meta's
/// API spelling so its serialized value maps directly to operator language.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdEntity {
    Campaign,
    Adset,
    Ad,
}

impl AdEntity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Campaign => "campaign",
            Self::Adset => "adset",
            Self::Ad => "ad",
        }
    }
}

impl FromStr for AdEntity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "campaign" => Ok(Self::Campaign),
            "adset" => Ok(Self::Adset),
            "ad" => Ok(Self::Ad),
            other => Err(format!("unknown_ad_entity:{other}")),
        }
    }
}

/// Account-scoped inventory kinds. Creatives belong here because they are
/// listed under `act_{id}/adcreatives`; they are not `AdEntity` because
/// review-status and paused creates have no creative delivery state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdsInventoryKind {
    Campaign,
    Adset,
    Ad,
    Creative,
}

impl AdsInventoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Campaign => "campaign",
            Self::Adset => "adset",
            Self::Ad => "ad",
            Self::Creative => "creative",
        }
    }

    pub fn from_entity(entity: AdEntity) -> Self {
        match entity {
            AdEntity::Campaign => Self::Campaign,
            AdEntity::Adset => Self::Adset,
            AdEntity::Ad => Self::Ad,
        }
    }

    /// Marketing API edge under `act_{id}`. Historical names (`adsets`,
    /// `adcreatives`) are the documented paths, not Wirekern aliases.
    pub fn graph_edge(self) -> &'static str {
        match self {
            Self::Campaign => "campaigns",
            Self::Adset => "adsets",
            Self::Ad => "ads",
            Self::Creative => "adcreatives",
        }
    }
}

impl FromStr for AdsInventoryKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "campaign" => Ok(Self::Campaign),
            "adset" => Ok(Self::Adset),
            "ad" => Ok(Self::Ad),
            "creative" => Ok(Self::Creative),
            other => Err(format!("unknown_ads_inventory_kind:{other}")),
        }
    }
}

/// One kind of object in a selected ad account. `--ad-account` overrides the
/// credential default the same way insights and paused creates do.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsInventoryRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub kind: AdsInventoryKind,
}

impl AdsInventoryRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_account(self.account.as_deref())
    }
}

/// One inventory row. Delivery objects carry configured/effective status;
/// creatives carry library `status` and have no delivery pair.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsInventoryItem {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adset_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_type: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsInventoryReply {
    pub site: Site,
    pub account_id: String,
    pub kind: AdsInventoryKind,
    pub items: Vec<AdsInventoryItem>,
}

/// Identify one existing object for a spend-shaped GET. IDs are globally
/// unique, so this request has no ad-account field — same as review status.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsInspectRequest {
    pub kind: AdsInventoryKind,
    pub id: String,
}

impl AdsInspectRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("ads_inspect_id", &self.id)
    }
}

/// Known targeting keys only. Graph targeting carries many fields Wirekern
/// does not write; `AdTargeting` denies unknowns, so inspect extracts a
/// subset instead of deserializing the whole object.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
pub struct AdsTargetingReadback {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub countries: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_min: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_max: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publisher_platforms: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facebook_positions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instagram_positions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub whatsapp_positions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_age_unknown: Option<bool>,
}

impl AdsTargetingReadback {
    pub fn is_empty(&self) -> bool {
        self.countries.is_empty()
            && self.age_min.is_none()
            && self.age_max.is_none()
            && self.publisher_platforms.is_empty()
            && self.facebook_positions.is_empty()
            && self.instagram_positions.is_empty()
            && self.whatsapp_positions.is_empty()
            && self.user_age_unknown.is_none()
    }
}

/// Budget, bid, targeting, Page, and destination as Graph returned them.
/// Absent fields stay absent so a campaign read cannot invent an ad-set
/// bid or a creative destination.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsInspectReply {
    pub site: Site,
    pub kind: AdsInventoryKind,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily_budget: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifetime_budget: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bid_strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bid_amount: Option<String>,
    /// Min-ROAS floor from `bid_constraints.roas_average_floor` (10000 = 1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roas_average_floor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targeting: Option<AdsTargetingReadback>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_to_action_type: Option<String>,
    /// Catalog / Advantage+ catalog ads: top-level creative `product_set_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_set_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instagram_user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub whatsapp_identity_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adset_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creative_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
}
