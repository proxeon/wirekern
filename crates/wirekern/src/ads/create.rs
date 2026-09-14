//! Structurally paused creates and their confirmation types.
use crate::types::Site;
use serde::{Deserialize, Serialize};

use super::helpers::{require_name, require_numeric_id, validate_account, validate_budget_xor};
use super::{
    billing_event_allowed, validate_adset_schedule, validate_bid_constraints,
    validate_promoted_object, AdEntity, AdPreviewFormat, PausedAdset, PausedCampaign,
};

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
/// Ad-set payloads carry targeting and schedule; boxing them would change
/// the public request type for a clippy size lint.
#[allow(clippy::large_enum_variant)]
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
                validate_budget_xor(campaign.daily_budget, campaign.lifetime_budget, true)?;
                if campaign.is_adset_budget_sharing_enabled
                    && (campaign.daily_budget.is_some() || campaign.lifetime_budget.is_some())
                {
                    // Meta 4834002: sharing is ABO-only, not CBO.
                    return Err("budget_sharing_incompatible_with_campaign_budget".into());
                }
            }
            Self::Adset(adset) => {
                require_name(&adset.name)?;
                require_numeric_id("campaign_id", &adset.campaign_id)?;
                validate_budget_xor(adset.daily_budget, adset.lifetime_budget, true)?;
                validate_bid_constraints(
                    adset.bid_strategy,
                    adset.bid_amount,
                    adset.roas_average_floor,
                    adset.billing_event,
                    adset.optimization_goal,
                )?;
                validate_adset_schedule(
                    adset.start_time.as_deref(),
                    adset.end_time.as_deref(),
                    adset.lifetime_budget,
                )?;
                validate_promoted_object(adset.optimization_goal, adset.promoted_object.as_ref())?;
                if !billing_event_allowed(adset.optimization_goal, adset.billing_event) {
                    return Err(format!(
                        "unsupported_billing_event:{}:{}",
                        adset.optimization_goal.as_str(),
                        adset.billing_event.as_str()
                    ));
                }
                adset.targeting.validate()?;
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
        validate_account(self.account.as_deref())?;
        self.create.validate()
    }
}

/// Confirmation of the one entity the platform created. Status is supplied
/// by wirekern's fixed request contract, not trusted as an echo from Graph's
/// compact `{ "id": "…" }` response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreatedAd {
    pub site: Site,
    pub account_id: String,
    pub entity: AdEntity,
    pub id: String,
    pub status: String,
}

/// The stable hash Meta assigned to a successfully uploaded account image.
/// This is the only media handle the first link-creative format accepts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UploadedAdImage {
    pub site: Site,
    pub account_id: String,
    pub hash: String,
}

/// Confirmation that Meta created an ad creative. It is deliberately not a
/// `CreatedAd`: no creative ID can start delivery until a separately-paused
/// ad references it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreatedAdCreative {
    pub site: Site,
    pub account_id: String,
    pub id: String,
}

/// Meta returns preview markup as an iframe body. It stays available to
/// library callers for writing to an explicitly chosen file, but is skipped
/// from serialization so a CLI or log cannot accidentally dump remote HTML.
#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct CreativePreview {
    pub site: Site,
    pub creative_id: String,
    pub ad_format: AdPreviewFormat,
    #[serde(skip_serializing)]
    pub body: String,
}

impl std::fmt::Debug for CreativePreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // An iframe's source can be short-lived and account-scoped. Match the
        // token types' redaction rule so a `{:?}` from an embedding program
        // cannot bypass the intentional JSON/CLI omission above.
        f.debug_struct("CreativePreview")
            .field("site", &self.site)
            .field("creative_id", &self.creative_id)
            .field("ad_format", &self.ad_format)
            .field("body", &"[redacted]")
            .finish()
    }
}
