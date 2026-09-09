//! Paused advertising-management vocabulary.
//!
//! These types deliberately describe only drafts that cannot deliver. They
//! stay outside the Meta connector so a future ads connector can reuse the
//! Client/Policy seam while mapping its own wire format.

use crate::types::Site;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;

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

/// Meta's outcome-based campaign objectives. Keeping this closed prevents a
/// misspelled command-line objective from becoming an opaque Graph error
/// after a write has already been attempted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignObjective {
    Awareness,
    Traffic,
    Engagement,
    Leads,
    AppPromotion,
    Sales,
}

impl CampaignObjective {
    pub fn meta_value(self) -> &'static str {
        match self {
            Self::Awareness => "OUTCOME_AWARENESS",
            Self::Traffic => "OUTCOME_TRAFFIC",
            Self::Engagement => "OUTCOME_ENGAGEMENT",
            Self::Leads => "OUTCOME_LEADS",
            Self::AppPromotion => "OUTCOME_APP_PROMOTION",
            Self::Sales => "OUTCOME_SALES",
        }
    }
}

impl FromStr for CampaignObjective {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "awareness" => Ok(Self::Awareness),
            "traffic" => Ok(Self::Traffic),
            "engagement" => Ok(Self::Engagement),
            "leads" => Ok(Self::Leads),
            "app_promotion" => Ok(Self::AppPromotion),
            "sales" => Ok(Self::Sales),
            other => Err(format!("unknown_objective:{other}")),
        }
    }
}

/// A campaign draft. Status is intentionally absent: the connector adds the
/// only allowed value, `PAUSED`, rather than trusting a caller-provided flag.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PausedCampaign {
    pub name: String,
    pub objective: CampaignObjective,
    #[serde(default)]
    pub special_ad_categories: Vec<String>,
}

/// An ad-set draft. `daily_budget` is the ad account's minor currency unit
/// (for ILS, agorot), matching Meta's integer Marketing API field exactly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PausedAdset {
    pub name: String,
    pub campaign_id: String,
    pub daily_budget: u64,
    pub billing_event: String,
    pub optimization_goal: String,
    pub targeting: Value,
}

/// An ad draft references a pre-created Meta creative. Tier B does not try
/// to invent creative, page identity, or tracking defaults on the operator's
/// behalf; those are material campaign choices.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PausedAd {
    pub name: String,
    pub adset_id: String,
    pub creative_id: String,
}

/// A management request is structurally paused: no enum variant represents
/// an active create. Future activation work must add a new type and cross the
/// policy boundary intentionally.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "entity", content = "spec", rename_all = "snake_case")]
pub enum PausedAdCreate {
    Campaign(PausedCampaign),
    Adset(PausedAdset),
    Ad(PausedAd),
}

impl PausedAdCreate {
    pub fn entity(&self) -> AdEntity {
        match self {
            Self::Campaign(_) => AdEntity::Campaign,
            Self::Adset(_) => AdEntity::Adset,
            Self::Ad(_) => AdEntity::Ad,
        }
    }

    /// Validate syntax before policy, vault, or network access. The
    /// connector owns platform-specific targeting rules, but a blank name,
    /// malformed numeric ID, zero budget, or non-object targeting document
    /// can never mean a valid paused create.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Campaign(campaign) => {
                require_name(&campaign.name)?;
                for category in &campaign.special_ad_categories {
                    if category.trim().is_empty() {
                        return Err("empty_special_ad_category".into());
                    }
                }
            }
            Self::Adset(adset) => {
                require_name(&adset.name)?;
                require_numeric_id("campaign_id", &adset.campaign_id)?;
                if adset.daily_budget == 0 {
                    return Err("daily_budget_must_be_positive".into());
                }
                if adset.billing_event.trim().is_empty() {
                    return Err("missing_billing_event".into());
                }
                if adset.optimization_goal.trim().is_empty() {
                    return Err("missing_optimization_goal".into());
                }
                if !adset.targeting.is_object() {
                    return Err("targeting_must_be_object".into());
                }
            }
            Self::Ad(ad) => {
                require_name(&ad.name)?;
                require_numeric_id("adset_id", &ad.adset_id)?;
                require_numeric_id("creative_id", &ad.creative_id)?;
            }
        }
        Ok(())
    }
}

/// Account override plus a structurally-paused create. `None` means the
/// connector uses the account stored at Meta OAuth time, just as insights do.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreatePausedAdRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub create: PausedAdCreate,
}

impl CreatePausedAdRequest {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(account) = &self.account {
            // Operators copy `act_<id>` from account discovery; accept that
            // canonical spelling as well as a bare numeric API ID.
            require_numeric_id(
                "ad_account",
                account.strip_prefix("act_").unwrap_or(account),
            )?;
        }
        self.create.validate()
    }
}

/// Confirmation of the one entity the platform created. Status is supplied
/// by postkit's fixed request contract, not trusted as an echo from Graph's
/// compact `{ "id": "…" }` response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreatedAd {
    pub site: Site,
    pub account_id: String,
    pub entity: AdEntity,
    pub id: String,
    pub status: String,
}

fn require_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        Err("missing_name".into())
    } else {
        Ok(())
    }
}

fn require_numeric_id(field: &str, id: &str) -> Result<(), String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        Err(format!("bad_{field}:{id}"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn objective_is_closed_and_maps_to_meta_outcomes() {
        assert_eq!(
            CampaignObjective::from_str("sales").unwrap().meta_value(),
            "OUTCOME_SALES"
        );
        assert_eq!(
            CampaignObjective::from_str("clicks").unwrap_err(),
            "unknown_objective:clicks"
        );
    }

    #[test]
    fn paused_create_validation_rejects_invalid_shapes() {
        let bad_budget = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                name: "Test".into(),
                campaign_id: "12".into(),
                daily_budget: 0,
                billing_event: "IMPRESSIONS".into(),
                optimization_goal: "REACH".into(),
                targeting: json!({}),
            }),
        };
        assert_eq!(
            bad_budget.validate().unwrap_err(),
            "daily_budget_must_be_positive"
        );

        let bad_targeting = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                name: "Test".into(),
                campaign_id: "12".into(),
                daily_budget: 100,
                billing_event: "IMPRESSIONS".into(),
                optimization_goal: "REACH".into(),
                targeting: json!([]),
            }),
        };
        assert_eq!(
            bad_targeting.validate().unwrap_err(),
            "targeting_must_be_object"
        );
    }
}
