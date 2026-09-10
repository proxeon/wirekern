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

/// The one bid strategy Tier B can safely express without a bid cap or a
/// ROAS-floor constraint. Other Meta strategies need additional money-shaped
/// inputs, so accepting their names before modelling those inputs would turn
/// a local validation error into an opaque platform rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BidStrategy {
    LowestCostWithoutCap,
}

impl BidStrategy {
    pub fn meta_value(self) -> &'static str {
        match self {
            Self::LowestCostWithoutCap => "LOWEST_COST_WITHOUT_CAP",
        }
    }
}

impl FromStr for BidStrategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "lowest_cost_without_cap" => Ok(Self::LowestCostWithoutCap),
            other => Err(format!("unknown_bid_strategy:{other}")),
        }
    }
}

/// The CTA supported by the first image-link creative format. More CTA kinds
/// are not aliases: Meta gives some of them additional value requirements, so
/// each must be modelled deliberately rather than accepted as a raw string.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkCallToAction {
    LearnMore,
}

impl LinkCallToAction {
    pub fn meta_value(self) -> &'static str {
        match self {
            Self::LearnMore => "LEARN_MORE",
        }
    }
}

impl FromStr for LinkCallToAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "learn_more" => Ok(Self::LearnMore),
            other => Err(format!("unknown_link_call_to_action:{other}")),
        }
    }
}

/// The initial preview placements are deliberately a small closed set. Meta
/// exposes many placement names, but accepting arbitrary strings would turn a
/// typo into a remote error and would suggest a placement is safe to review
/// before Postkit has tested its rendering contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdPreviewFormat {
    DesktopFeedStandard,
    MobileFeedStandard,
}

impl AdPreviewFormat {
    pub fn meta_value(self) -> &'static str {
        match self {
            Self::DesktopFeedStandard => "DESKTOP_FEED_STANDARD",
            Self::MobileFeedStandard => "MOBILE_FEED_STANDARD",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::DesktopFeedStandard => "desktop_feed_standard",
            Self::MobileFeedStandard => "mobile_feed_standard",
        }
    }
}

impl FromStr for AdPreviewFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "desktop_feed_standard" => Ok(Self::DesktopFeedStandard),
            "mobile_feed_standard" => Ok(Self::MobileFeedStandard),
            other => Err(format!("unknown_ad_preview_format:{other}")),
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
    pub bid_strategy: BidStrategy,
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

/// The bytes for one account-scoped ad image. The library takes bytes rather
/// than a filesystem path so its core remains usable by non-CLI callers; the
/// CLI reads the operator-selected file and deliberately never reports its
/// local path in an error.
#[derive(Clone, Debug, PartialEq)]
pub struct UploadAdImageRequest {
    pub account: Option<String>,
    pub filename: String,
    pub bytes: Vec<u8>,
}

impl UploadAdImageRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_account(self.account.as_deref())?;
        if self.filename.trim().is_empty()
            || self.filename.contains('/')
            || self.filename.contains('\\')
        {
            return Err("invalid_image_filename".into());
        }
        if self.bytes.is_empty() {
            return Err("image_file_empty".into());
        }
        Ok(())
    }
}

/// The exact input for a static image website creative. It purposefully has
/// no implicit Page, media, copy, destination, or CTA: these determine the
/// future ad even though the creative alone cannot deliver.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinkAdCreative {
    pub name: String,
    pub page_id: String,
    pub image_hash: String,
    pub message: String,
    pub headline: String,
    pub destination_url: String,
    pub call_to_action: LinkCallToAction,
}

/// Account override plus one image-link creative. Unlike a campaign/ad set/ad
/// this object has no status because Meta creatives cannot independently
/// deliver; only the later ad object is structurally forced to `PAUSED`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateLinkAdCreativeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub creative: LinkAdCreative,
}

impl CreateLinkAdCreativeRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_account(self.account.as_deref())?;
        require_name(&self.creative.name)?;
        require_numeric_id("page_id", &self.creative.page_id)?;
        require_text("image_hash", &self.creative.image_hash)?;
        require_text("message", &self.creative.message)?;
        require_text("headline", &self.creative.headline)?;
        require_https_url("destination_url", &self.creative.destination_url)?;
        Ok(())
    }
}

/// Request a render for an existing Meta creative. An ad creative ID is
/// globally addressable by Graph, so the preview edge has no account path or
/// account override; the selected credential is still required to read it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreativePreviewRequest {
    pub creative_id: String,
    pub ad_format: AdPreviewFormat,
}

impl CreativePreviewRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("creative_id", &self.creative_id)
    }
}

/// Identify exactly one delivery object for a status inspection. The entity
/// type is carried with the ID rather than inferred from a Graph response so
/// the operator can see whether they inspected the campaign, ad set, or final
/// ad in a paused hierarchy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdReviewStatusRequest {
    pub entity: AdEntity,
    pub id: String,
}

impl AdReviewStatusRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("ad_entity_id", &self.id)
    }
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
        validate_account(self.account.as_deref())?;
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

/// One operator-facing problem Meta attached to an advertising object. Meta
/// may omit any individual property, so preserve the stable fields it did
/// supply instead of replacing a specific review failure with a generic
/// Postkit error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdReviewIssue {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

/// The effective state is Meta's current delivery/review interpretation;
/// configured state is the explicit state Postkit requested. They differ
/// while Meta processes a fresh paused draft, and both must be shown so a
/// `PENDING_REVIEW` value is never mistaken for delivery.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdReviewStatus {
    pub site: Site,
    pub entity: AdEntity,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub configured_status: String,
    pub effective_status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<AdReviewIssue>,
}

impl AdReviewStatus {
    /// Meta documents these two transitional values while it processes a
    /// newly created object. A configured `PAUSED` object remains unable to
    /// deliver during either state; callers can poll without issuing a write.
    pub fn is_pending_review(&self) -> bool {
        matches!(
            self.effective_status.as_str(),
            "PENDING_REVIEW" | "IN_PROCESS"
        )
    }
}

/// The bounded wait result is deliberately a value, not a timeout error.
/// Pending review is a normal Meta lifecycle state: callers need the last
/// observed status and issues to decide when to try again, not a silent wait
/// or an ambiguous transport failure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "review", content = "status", rename_all = "snake_case")]
pub enum AdReviewWait {
    Settled(AdReviewStatus),
    PendingReview(AdReviewStatus),
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

fn validate_account(account: Option<&str>) -> Result<(), String> {
    if let Some(account) = account {
        // Operators copy `act_<id>` from account discovery; accept that
        // canonical spelling as well as a bare numeric API ID.
        require_numeric_id(
            "ad_account",
            account.strip_prefix("act_").unwrap_or(account),
        )?;
    }
    Ok(())
}

fn require_text(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("missing_{field}"));
    }
    Ok(())
}

fn require_https_url(field: &str, value: &str) -> Result<(), String> {
    let Some(authority_and_path) = value.strip_prefix("https://") else {
        return Err(format!("{field}_must_be_https"));
    };
    // Split at the first path/query/fragment separator. This rejects values
    // such as `https:///offer`: they have the required scheme text but no
    // authority, so Meta would only return a less actionable form error.
    let authority = authority_and_path
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(format!("{field}_must_be_https"));
    }
    Ok(())
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
        assert_eq!(AdEntity::from_str("adset").unwrap(), AdEntity::Adset);
        assert_eq!(
            AdEntity::from_str("creative").unwrap_err(),
            "unknown_ad_entity:creative"
        );
        assert_eq!(
            CampaignObjective::from_str("clicks").unwrap_err(),
            "unknown_objective:clicks"
        );
    }

    #[test]
    fn bid_strategy_is_closed_and_maps_to_meta_wire_value() {
        assert_eq!(
            BidStrategy::from_str("lowest_cost_without_cap")
                .unwrap()
                .meta_value(),
            "LOWEST_COST_WITHOUT_CAP"
        );
        assert_eq!(
            BidStrategy::from_str("cost_cap").unwrap_err(),
            "unknown_bid_strategy:cost_cap"
        );
    }

    #[test]
    fn image_link_creative_contract_is_closed_and_validates_every_input() {
        assert_eq!(
            LinkCallToAction::from_str("learn_more")
                .unwrap()
                .meta_value(),
            "LEARN_MORE"
        );
        assert_eq!(
            LinkCallToAction::from_str("shop_now").unwrap_err(),
            "unknown_link_call_to_action:shop_now"
        );
        assert_eq!(
            AdPreviewFormat::from_str("desktop_feed_standard")
                .unwrap()
                .meta_value(),
            "DESKTOP_FEED_STANDARD"
        );
        assert_eq!(
            AdPreviewFormat::from_str("instagram_standard").unwrap_err(),
            "unknown_ad_preview_format:instagram_standard"
        );

        let valid_image = UploadAdImageRequest {
            account: Some("act_123".into()),
            filename: "hero.png".into(),
            bytes: b"image bytes".to_vec(),
        };
        assert!(valid_image.validate().is_ok());
        let invalid_filename = UploadAdImageRequest {
            filename: "private/hero.png".into(),
            ..valid_image.clone()
        };
        assert_eq!(
            invalid_filename.validate().unwrap_err(),
            "invalid_image_filename"
        );
        let empty_image = UploadAdImageRequest {
            bytes: vec![],
            ..valid_image
        };
        assert_eq!(empty_image.validate().unwrap_err(), "image_file_empty");

        assert!(CreativePreviewRequest {
            creative_id: "789".into(),
            ad_format: AdPreviewFormat::MobileFeedStandard,
        }
        .validate()
        .is_ok());
        assert_eq!(
            CreativePreviewRequest {
                creative_id: "not-an-id".into(),
                ad_format: AdPreviewFormat::DesktopFeedStandard,
            }
            .validate()
            .unwrap_err(),
            "bad_creative_id:not-an-id"
        );
        assert!(AdReviewStatusRequest {
            entity: AdEntity::Ad,
            id: "789".into(),
        }
        .validate()
        .is_ok());
        assert_eq!(
            AdReviewStatusRequest {
                entity: AdEntity::Campaign,
                id: "campaign-789".into(),
            }
            .validate()
            .unwrap_err(),
            "bad_ad_entity_id:campaign-789"
        );
        // The raw iframe is intentionally available in-process for a caller
        // to write to a file, but its derived JSON form must never become a
        // surprise terminal/log payload.
        let serialized = serde_json::to_value(CreativePreview {
            site: Site::new("meta_ads"),
            creative_id: "789".into(),
            ad_format: AdPreviewFormat::DesktopFeedStandard,
            body: "<iframe secret-ish-preview-url>".into(),
        })
        .unwrap();
        assert!(serialized.get("body").is_none());
        assert!(!format!(
            "{:?}",
            CreativePreview {
                site: Site::new("meta_ads"),
                creative_id: "789".into(),
                ad_format: AdPreviewFormat::DesktopFeedStandard,
                body: "<iframe secret-ish-preview-url>".into(),
            }
        )
        .contains("secret-ish-preview-url"));

        let valid_creative = CreateLinkAdCreativeRequest {
            account: Some("123".into()),
            creative: LinkAdCreative {
                name: "Hero link".into(),
                page_id: "456".into(),
                image_hash: "hash-1".into(),
                message: "A clear benefit".into(),
                headline: "Learn more".into(),
                destination_url: "https://example.com/offer".into(),
                call_to_action: LinkCallToAction::LearnMore,
            },
        };
        assert!(valid_creative.validate().is_ok());
        let insecure_url = CreateLinkAdCreativeRequest {
            creative: LinkAdCreative {
                destination_url: "http://example.com".into(),
                ..valid_creative.creative.clone()
            },
            ..valid_creative.clone()
        };
        assert_eq!(
            insecure_url.validate().unwrap_err(),
            "destination_url_must_be_https"
        );
        let missing_host = CreateLinkAdCreativeRequest {
            creative: LinkAdCreative {
                destination_url: "https:///offer".into(),
                ..valid_creative.creative.clone()
            },
            ..valid_creative.clone()
        };
        assert_eq!(
            missing_host.validate().unwrap_err(),
            "destination_url_must_be_https"
        );
        let bad_page = CreateLinkAdCreativeRequest {
            creative: LinkAdCreative {
                page_id: "page-456".into(),
                ..valid_creative.creative
            },
            ..valid_creative
        };
        assert_eq!(bad_page.validate().unwrap_err(), "bad_page_id:page-456");
    }

    #[test]
    fn paused_create_validation_rejects_invalid_shapes() {
        let bad_budget = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                name: "Test".into(),
                campaign_id: "12".into(),
                daily_budget: 0,
                bid_strategy: BidStrategy::LowestCostWithoutCap,
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
                bid_strategy: BidStrategy::LowestCostWithoutCap,
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
