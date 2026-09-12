//! Status, activate/pause/archive/delete, copies, and typed spend edits.
use crate::types::Site;
use serde::{Deserialize, Serialize};

use super::helpers::{confirm_ids, require_numeric_id};
use super::{
    validate_adset_schedule, AdEntity, AdReviewStatus, AdTargeting, AdsInspectReply,
    AdsTargetingReadback, BidStrategy, FacebookPosition, InstagramPosition, PublisherPlatform,
    WhatsAppPosition,
};

/// The four Graph `status` values Meta documents for campaign/ad set/ad
/// updates. Closed so a typo cannot become `status=ACTIVE` by accident.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdsConfiguredStatus {
    Active,
    Paused,
    Archived,
    Deleted,
}

impl AdsConfiguredStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Archived => "archived",
            Self::Deleted => "deleted",
        }
    }

    pub fn meta_value(self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
            Self::Paused => "PAUSED",
            Self::Archived => "ARCHIVED",
            Self::Deleted => "DELETED",
        }
    }
}

/// Wire POST of one documented `status` on a known object. Client methods
/// that can spend (activate) sit in front of this with policy and preflight.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsStatusUpdateRequest {
    pub entity: AdEntity,
    pub id: String,
    pub status: AdsConfiguredStatus,
}

impl AdsStatusUpdateRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("ad_entity_id", &self.id)
    }
}

/// Operator-confirmed PAUSED → ACTIVE. `confirm_id` must equal `id` so a
/// copied `--allow-activate` cannot aim at a different object. Budget
/// echoes are required only when inspect reports a daily or lifetime budget.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsActivateRequest {
    pub entity: AdEntity,
    pub id: String,
    pub confirm_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_daily_budget: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_lifetime_budget: Option<u64>,
}

impl AdsActivateRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("ad_entity_id", &self.id)?;
        require_numeric_id("confirm_id", &self.confirm_id)?;
        if self.confirm_id != self.id {
            return Err("confirm_id_mismatch".into());
        }
        match (self.confirm_daily_budget, self.confirm_lifetime_budget) {
            (Some(0), _) | (_, Some(0)) => Err("budget_must_be_positive".into()),
            (Some(_), Some(_)) => Err("daily_and_lifetime_budget_mutually_exclusive".into()),
            _ => Ok(()),
        }
    }
}

/// Durable marker written *before* an activate POST. A leftover
/// `in_flight` means the previous attempt may have reached Meta; the next
/// command must not POST again.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsLifecycleCheckpoint {
    pub action: String,
    pub entity: AdEntity,
    pub id: String,
    pub in_flight: bool,
}

/// Activate/pause outcome. `reconciliation_required` is success of the
/// protocol (exit 0): the write may have applied and must not be retried.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "lifecycle", rename_all = "snake_case")]
pub enum AdsLifecycleOutcome {
    Applied {
        status: AdReviewStatus,
    },
    ReconciliationRequired {
        entity: AdEntity,
        id: String,
        guidance: String,
    },
}

pub const ACTIVATE_RECONCILE_GUIDANCE: &str =
    "Activation POST left without a confirmed Graph reply. Read ads status for this id; do not retry activate.";

pub const EDIT_RECONCILE_GUIDANCE: &str =
    "Ad object POST left without a confirmed Graph reply. Inspect this id; do not retry the same edit blindly.";

/// Typed edit outcome. Network/deadline after the POST left is
/// `reconciliation_required` (exit 0), same as activate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
#[serde(tag = "edit", rename_all = "snake_case")]
pub enum AdsEditOutcome {
    Applied {
        inspect: Box<AdsInspectReply>,
        #[serde(skip_serializing_if = "Option::is_none")]
        targeting_diff: Option<AdsTargetingDiff>,
    },
    ReconciliationRequired {
        entity: AdEntity,
        id: String,
        guidance: String,
    },
}

/// Emergency stop. No budget confirmation: pausing cannot start spend.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsPauseRequest {
    pub entity: AdEntity,
    pub id: String,
}

impl AdsPauseRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("ad_entity_id", &self.id)
    }
}

pub const PAUSE_RECONCILE_GUIDANCE: &str =
    "Pause POST left without a confirmed Graph reply. Read ads status for this id; do not retry blindly.";

/// Archive requires `--confirm-id` so a copied allow flag cannot aim at
/// another object. Default policy denies archive.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsArchiveRequest {
    pub entity: AdEntity,
    pub id: String,
    pub confirm_id: String,
}

impl AdsArchiveRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("ad_entity_id", &self.id)?;
        require_numeric_id("confirm_id", &self.confirm_id)?;
        if self.confirm_id != self.id {
            return Err("confirm_id_mismatch".into());
        }
        Ok(())
    }
}

pub const ARCHIVE_RECONCILE_GUIDANCE: &str =
    "Archive POST left without a confirmed Graph reply. Read ads status for this id; do not retry blindly.";

/// Delete is irreversible to live. `confirm_delete` must be true in addition
/// to matching `--confirm-id`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsDeleteRequest {
    pub entity: AdEntity,
    pub id: String,
    pub confirm_id: String,
    pub confirm_delete: bool,
}

impl AdsDeleteRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("ad_entity_id", &self.id)?;
        require_numeric_id("confirm_id", &self.confirm_id)?;
        if self.confirm_id != self.id {
            return Err("confirm_id_mismatch".into());
        }
        if !self.confirm_delete {
            return Err("confirm_delete_required".into());
        }
        Ok(())
    }
}

pub const DELETE_RECONCILE_GUIDANCE: &str =
    "Delete POST left without a confirmed Graph reply. Read ads status for this id; do not retry blindly.";

/// Copy with Meta `status_option=PAUSED` hard-coded. Never inherits ACTIVE.
/// Budget echo is required when the source has a daily/lifetime budget —
/// the copy is paused but still inherits spend shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsDuplicateRequest {
    pub entity: AdEntity,
    pub id: String,
    pub confirm_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_daily_budget: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_lifetime_budget: Option<u64>,
}

impl AdsDuplicateRequest {
    pub fn validate(&self) -> Result<(), String> {
        confirm_ids(&self.id, &self.confirm_id)?;
        match (self.confirm_daily_budget, self.confirm_lifetime_budget) {
            (Some(0), _) | (_, Some(0)) => Err("budget_must_be_positive".into()),
            (Some(_), Some(_)) => Err("daily_and_lifetime_budget_mutually_exclusive".into()),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsDuplicateReply {
    pub site: Site,
    pub entity: AdEntity,
    pub source_id: String,
    pub copied_id: String,
    pub status: String,
}

pub const DEFAULT_BUDGET_MAX_CHANGE_RATIO: f64 = 0.2;

/// Daily-budget edit. Current must match Graph; the relative change cannot
/// exceed `max_change_ratio` (default 0.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdsBudgetUpdateRequest {
    pub entity: AdEntity,
    pub id: String,
    pub confirm_id: String,
    pub current_daily_budget: u64,
    pub new_daily_budget: u64,
    #[serde(default = "default_budget_max_change_ratio")]
    pub max_change_ratio: f64,
}

fn default_budget_max_change_ratio() -> f64 {
    DEFAULT_BUDGET_MAX_CHANGE_RATIO
}

impl AdsBudgetUpdateRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("ad_entity_id", &self.id)?;
        require_numeric_id("confirm_id", &self.confirm_id)?;
        if self.confirm_id != self.id {
            return Err("confirm_id_mismatch".into());
        }
        // Daily budget lives on campaign/ad set. An ad has none.
        if self.entity == AdEntity::Ad {
            return Err("budget_not_on_object".into());
        }
        if self.current_daily_budget == 0 || self.new_daily_budget == 0 {
            return Err("budget_must_be_positive".into());
        }
        if !(self.max_change_ratio > 0.0 && self.max_change_ratio <= 1.0) {
            return Err("max_change_ratio_out_of_range".into());
        }
        let delta = self.new_daily_budget.abs_diff(self.current_daily_budget) as f64;
        let ratio = delta / self.current_daily_budget as f64;
        if ratio > self.max_change_ratio {
            return Err("budget_change_exceeds_guard".into());
        }
        Ok(())
    }
}

/// Lifetime-budget edit. Same guards as daily: current must match Graph,
/// relative change cannot exceed `max_change_ratio`. Campaign/ad set only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdsLifetimeBudgetUpdateRequest {
    pub entity: AdEntity,
    pub id: String,
    pub confirm_id: String,
    pub current_lifetime_budget: u64,
    pub new_lifetime_budget: u64,
    #[serde(default = "default_budget_max_change_ratio")]
    pub max_change_ratio: f64,
}

impl AdsLifetimeBudgetUpdateRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("ad_entity_id", &self.id)?;
        require_numeric_id("confirm_id", &self.confirm_id)?;
        if self.confirm_id != self.id {
            return Err("confirm_id_mismatch".into());
        }
        if self.entity == AdEntity::Ad {
            return Err("budget_not_on_object".into());
        }
        if self.current_lifetime_budget == 0 || self.new_lifetime_budget == 0 {
            return Err("budget_must_be_positive".into());
        }
        if !(self.max_change_ratio > 0.0 && self.max_change_ratio <= 1.0) {
            return Err("max_change_ratio_out_of_range".into());
        }
        let delta = self
            .new_lifetime_budget
            .abs_diff(self.current_lifetime_budget) as f64;
        let ratio = delta / self.current_lifetime_budget as f64;
        if ratio > self.max_change_ratio {
            return Err("budget_change_exceeds_guard".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsBidUpdateRequest {
    pub entity: AdEntity,
    pub id: String,
    pub confirm_id: String,
    pub bid_strategy: BidStrategy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid_amount: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roas_average_floor: Option<u64>,
}

impl AdsBidUpdateRequest {
    pub fn validate(&self) -> Result<(), String> {
        confirm_ids(&self.id, &self.confirm_id)?;
        if self.bid_strategy.requires_bid_amount() {
            match self.bid_amount {
                Some(amount) if amount > 0 => {}
                _ => return Err("missing_bid_amount".into()),
            }
        } else if self.bid_amount.is_some() {
            return Err("bid_amount_without_cap_strategy".into());
        }
        if self.bid_strategy.requires_roas_floor() {
            match self.roas_average_floor {
                Some(floor) if (100..=10_000_000).contains(&floor) => {}
                Some(_) => return Err("roas_average_floor_out_of_range".into()),
                None => return Err("missing_roas_average_floor".into()),
            }
        } else if self.roas_average_floor.is_some() {
            return Err("roas_floor_without_min_roas_strategy".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsScheduleUpdateRequest {
    pub entity: AdEntity,
    pub id: String,
    pub confirm_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
}

impl AdsScheduleUpdateRequest {
    pub fn validate(&self) -> Result<(), String> {
        confirm_ids(&self.id, &self.confirm_id)?;
        if self.entity != AdEntity::Adset {
            return Err("schedule_adset_only".into());
        }
        validate_adset_schedule(self.start_time.as_deref(), self.end_time.as_deref(), None)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsPlacementUpdateRequest {
    pub entity: AdEntity,
    pub id: String,
    pub confirm_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publisher_platforms: Vec<PublisherPlatform>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facebook_positions: Vec<FacebookPosition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instagram_positions: Vec<InstagramPosition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub whatsapp_positions: Vec<WhatsAppPosition>,
}

impl AdsPlacementUpdateRequest {
    pub fn validate(&self) -> Result<(), String> {
        confirm_ids(&self.id, &self.confirm_id)?;
        if self.entity != AdEntity::Adset {
            return Err("placement_adset_only".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdsTargetingUpdateRequest {
    pub entity: AdEntity,
    pub id: String,
    pub confirm_id: String,
    pub targeting: AdTargeting,
}

impl AdsTargetingUpdateRequest {
    pub fn validate(&self) -> Result<(), String> {
        confirm_ids(&self.id, &self.confirm_id)?;
        if self.entity != AdEntity::Adset {
            return Err("targeting_adset_only".into());
        }
        self.targeting.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsTargetingDiff {
    pub before: AdsTargetingReadback,
    pub after: AdsTargetingReadback,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsCreativeSwapRequest {
    pub id: String,
    pub confirm_id: String,
    pub creative_id: String,
}

impl AdsCreativeSwapRequest {
    pub fn validate(&self) -> Result<(), String> {
        confirm_ids(&self.id, &self.confirm_id)?;
        require_numeric_id("creative_id", &self.creative_id)
    }
}
