//! Paused advertising-management vocabulary.
//!
//! These types deliberately describe only drafts that cannot deliver. They
//! stay outside the Meta connector so a future ads connector can reuse the
//! Client/Policy seam while mapping its own wire format.

use crate::types::Site;
use serde::{Deserialize, Serialize};
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

    /// Marketing API edge under `act_{id}`. Historical names (`adsets`,
    /// `adcreatives`) are the documented paths, not Postkit aliases.
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

/// Known targeting keys only. Graph targeting carries many fields Postkit
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targeting: Option<AdsTargetingReadback>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adset_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creative_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
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

    /// Operator-facing name (the serde value); `meta_value` is the wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Awareness => "awareness",
            Self::Traffic => "traffic",
            Self::Engagement => "engagement",
            Self::Leads => "leads",
            Self::AppPromotion => "app_promotion",
            Self::Sales => "sales",
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

/// Auction bid strategies Meta documents on the ad set (`v26.0`). Each cap
/// or floor strategy carries its own constraint field: sending the name
/// without that field is a local error, not a Graph code 100.
///
/// `LOWEST_COST_WITHOUT_CAP` — no extra field; spend is bounded by budget.
/// `LOWEST_COST_WITH_BID_CAP` / `COST_CAP` — require `bid_amount` > 0
/// (minor units; per 1000 impressions when billing is IMPRESSIONS).
/// `LOWEST_COST_WITH_MIN_ROAS` — requires `roas_average_floor` on
/// `bid_constraints` (integer, 10000 = 1.0 ROAS).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BidStrategy {
    LowestCostWithoutCap,
    LowestCostWithBidCap,
    CostCap,
    LowestCostWithMinRoas,
}

impl BidStrategy {
    pub fn meta_value(self) -> &'static str {
        match self {
            Self::LowestCostWithoutCap => "LOWEST_COST_WITHOUT_CAP",
            Self::LowestCostWithBidCap => "LOWEST_COST_WITH_BID_CAP",
            Self::CostCap => "COST_CAP",
            Self::LowestCostWithMinRoas => "LOWEST_COST_WITH_MIN_ROAS",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LowestCostWithoutCap => "lowest_cost_without_cap",
            Self::LowestCostWithBidCap => "lowest_cost_with_bid_cap",
            Self::CostCap => "cost_cap",
            Self::LowestCostWithMinRoas => "lowest_cost_with_min_roas",
        }
    }

    pub fn requires_bid_amount(self) -> bool {
        matches!(self, Self::LowestCostWithBidCap | Self::CostCap)
    }

    pub fn requires_roas_floor(self) -> bool {
        matches!(self, Self::LowestCostWithMinRoas)
    }
}

/// Graph datetime: RFC3339, or Meta's documented variants (space instead of
/// `T`, offset without a colon). Returns a UTC instant so `end` > `start`
/// can be checked without sending a typo to Marketing API.
pub fn parse_adset_datetime(field: &str, value: &str) -> Result<time::OffsetDateTime, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("missing_{field}"));
    }
    let normalized = normalize_graph_datetime(trimmed);
    time::OffsetDateTime::parse(&normalized, &time::format_description::well_known::Rfc3339)
        .map_err(|_| format!("bad_{field}:{value}"))
}

/// `2025-11-11T14:25:17-0800` → `2025-11-11T14:25:17-08:00`.
fn normalize_graph_datetime(raw: &str) -> String {
    let with_t = raw.replacen(' ', "T", 1);
    let bytes = with_t.as_bytes();
    let n = bytes.len();
    if n >= 5 {
        let sign = bytes[n - 5];
        if (sign == b'+' || sign == b'-')
            && bytes[n - 4].is_ascii_digit()
            && bytes[n - 3].is_ascii_digit()
            && bytes[n - 2].is_ascii_digit()
            && bytes[n - 1].is_ascii_digit()
        {
            return format!(
                "{}{}{}:{}",
                &with_t[..n - 5],
                sign as char,
                &with_t[n - 4..n - 2],
                &with_t[n - 2..]
            );
        }
    }
    with_t
}

pub fn validate_adset_schedule(
    start_time: Option<&str>,
    end_time: Option<&str>,
    lifetime_budget: Option<u64>,
) -> Result<(), String> {
    if lifetime_budget.is_some() && end_time.is_none() {
        return Err("lifetime_budget_requires_end_time".into());
    }
    let start = start_time
        .map(|v| parse_adset_datetime("start_time", v))
        .transpose()?;
    let end = end_time
        .map(|v| parse_adset_datetime("end_time", v))
        .transpose()?;
    if let (Some(start), Some(end)) = (start, end) {
        if end <= start {
            return Err("end_time_not_after_start_time".into());
        }
    }
    Ok(())
}

/// Cap/floor fields must match the strategy. Meta rejects `bid_amount`
/// together with `bid_constraints`; we fail the same way locally.
///
/// Min-ROAS: `optimization_goal` must be `VALUE` and `roas_average_floor` is
/// in `[100, 10000000]` (10000 = 1.0). Cost cap: `billing_event` must be
/// `IMPRESSIONS`.
pub fn validate_bid_constraints(
    strategy: BidStrategy,
    bid_amount: Option<u64>,
    roas_average_floor: Option<u64>,
    billing: BillingEvent,
    goal: OptimizationGoal,
) -> Result<(), String> {
    if strategy.requires_bid_amount() {
        match bid_amount {
            Some(amount) if amount > 0 => {}
            _ => return Err("missing_bid_amount".into()),
        }
    } else if bid_amount.is_some() {
        return Err("bid_amount_without_cap_strategy".into());
    }
    if strategy.requires_roas_floor() {
        match roas_average_floor {
            Some(floor) if (100..=10_000_000).contains(&floor) => {}
            Some(_) => return Err("roas_average_floor_out_of_range".into()),
            None => return Err("missing_roas_average_floor".into()),
        }
        if goal != OptimizationGoal::Value {
            return Err("min_roas_requires_value_goal".into());
        }
    } else if roas_average_floor.is_some() {
        return Err("roas_floor_without_min_roas_strategy".into());
    }
    if strategy == BidStrategy::CostCap && billing != BillingEvent::Impressions {
        return Err("cost_cap_requires_impressions_billing".into());
    }
    Ok(())
}

impl FromStr for BidStrategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "lowest_cost_without_cap" => Ok(Self::LowestCostWithoutCap),
            "lowest_cost_with_bid_cap" => Ok(Self::LowestCostWithBidCap),
            "cost_cap" => Ok(Self::CostCap),
            "lowest_cost_with_min_roas" => Ok(Self::LowestCostWithMinRoas),
            other => Err(format!("unknown_bid_strategy:{other}")),
        }
    }
}

/// Auction billing events Postkit will send. Meta's historical CPA-only
/// values are omitted: they need extra constraints this kernel does not model.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BillingEvent {
    Impressions,
    LinkClicks,
}

impl BillingEvent {
    pub fn meta_value(self) -> &'static str {
        match self {
            Self::Impressions => "IMPRESSIONS",
            Self::LinkClicks => "LINK_CLICKS",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Impressions => "impressions",
            Self::LinkClicks => "link_clicks",
        }
    }
}

impl FromStr for BillingEvent {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "impressions" | "IMPRESSIONS" => Ok(Self::Impressions),
            "link_clicks" | "LINK_CLICKS" => Ok(Self::LinkClicks),
            other => Err(format!("unknown_billing_event:{other}")),
        }
    }
}

/// Auction optimization goals we have a local billing pairing for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OptimizationGoal {
    Reach,
    BrandAwareness,
    LinkClicks,
    LandingPageViews,
    Impressions,
    OffsiteConversions,
    LeadGeneration,
    AppInstalls,
    PostEngagement,
    PageLikes,
    Value,
    Thruplay,
}

impl OptimizationGoal {
    pub fn meta_value(self) -> &'static str {
        match self {
            Self::Reach => "REACH",
            Self::BrandAwareness => "BRAND_AWARENESS",
            Self::LinkClicks => "LINK_CLICKS",
            Self::LandingPageViews => "LANDING_PAGE_VIEWS",
            Self::Impressions => "IMPRESSIONS",
            Self::OffsiteConversions => "OFFSITE_CONVERSIONS",
            Self::LeadGeneration => "LEAD_GENERATION",
            Self::AppInstalls => "APP_INSTALLS",
            Self::PostEngagement => "POST_ENGAGEMENT",
            Self::PageLikes => "PAGE_LIKES",
            Self::Value => "VALUE",
            Self::Thruplay => "THRUPLAY",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reach => "reach",
            Self::BrandAwareness => "brand_awareness",
            Self::LinkClicks => "link_clicks",
            Self::LandingPageViews => "landing_page_views",
            Self::Impressions => "impressions",
            Self::OffsiteConversions => "offsite_conversions",
            Self::LeadGeneration => "lead_generation",
            Self::AppInstalls => "app_installs",
            Self::PostEngagement => "post_engagement",
            Self::PageLikes => "page_likes",
            Self::Value => "value",
            Self::Thruplay => "thruplay",
        }
    }
}

impl FromStr for OptimizationGoal {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "reach" | "REACH" => Ok(Self::Reach),
            "brand_awareness" | "BRAND_AWARENESS" => Ok(Self::BrandAwareness),
            "link_clicks" | "LINK_CLICKS" => Ok(Self::LinkClicks),
            "landing_page_views" | "LANDING_PAGE_VIEWS" => Ok(Self::LandingPageViews),
            "impressions" | "IMPRESSIONS" => Ok(Self::Impressions),
            "offsite_conversions" | "OFFSITE_CONVERSIONS" => Ok(Self::OffsiteConversions),
            "lead_generation" | "LEAD_GENERATION" => Ok(Self::LeadGeneration),
            "app_installs" | "APP_INSTALLS" => Ok(Self::AppInstalls),
            "post_engagement" | "POST_ENGAGEMENT" => Ok(Self::PostEngagement),
            "page_likes" | "PAGE_LIKES" => Ok(Self::PageLikes),
            "value" | "VALUE" => Ok(Self::Value),
            "thruplay" | "THRUPLAY" => Ok(Self::Thruplay),
            other => Err(format!("unknown_optimization_goal:{other}")),
        }
    }
}

/// Auction billing events Meta allows for each optimization goal (v26.0).
/// LINK_CLICKS billing is only valid with the LINK_CLICKS goal.
pub fn billing_event_allowed(goal: OptimizationGoal, billing: BillingEvent) -> bool {
    match goal {
        OptimizationGoal::LinkClicks => {
            matches!(
                billing,
                BillingEvent::Impressions | BillingEvent::LinkClicks
            )
        }
        _ => matches!(billing, BillingEvent::Impressions),
    }
}

/// Objective → (goal, billing) pairs the draft orchestrator will send.
/// Unlisted combinations fail locally so a campaign is never created first.
pub fn supported_adset_pairing(
    objective: CampaignObjective,
    goal: OptimizationGoal,
    billing: BillingEvent,
) -> bool {
    if !billing_event_allowed(goal, billing) {
        return false;
    }
    matches!(
        (objective, goal),
        (CampaignObjective::Awareness, OptimizationGoal::Reach)
            | (
                CampaignObjective::Awareness,
                OptimizationGoal::BrandAwareness
            )
            | (CampaignObjective::Traffic, OptimizationGoal::LinkClicks)
            | (
                CampaignObjective::Traffic,
                OptimizationGoal::LandingPageViews
            )
            | (
                CampaignObjective::Engagement,
                OptimizationGoal::PostEngagement
            )
            | (CampaignObjective::Engagement, OptimizationGoal::PageLikes)
            | (CampaignObjective::Leads, OptimizationGoal::LeadGeneration)
            | (
                CampaignObjective::Leads,
                OptimizationGoal::OffsiteConversions
            )
            | (
                CampaignObjective::AppPromotion,
                OptimizationGoal::AppInstalls
            )
            | (
                CampaignObjective::Sales,
                OptimizationGoal::OffsiteConversions
            )
            | (CampaignObjective::Sales, OptimizationGoal::Value)
    )
}

/// Image-link CTAs Meta documents on `AdCreativeLinkDataCallToAction`.
/// Website types share `value.link` with the destination URL. Page-click
/// types send `value.page`. `get_directions` and `install_app` need extra
/// typed fields (geo link / application + app link) or they fail locally.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkCallToAction {
    LearnMore,
    ShopNow,
    SignUp,
    Download,
    ApplyNow,
    BookNow,
    Subscribe,
    BuyNow,
    ContactUs,
    GetQuote,
    OrderNow,
    CallNow,
    LikePage,
    WhatsAppMessage,
    GetDirections,
    InstallApp,
}

impl LinkCallToAction {
    pub fn meta_value(self) -> &'static str {
        match self {
            Self::LearnMore => "LEARN_MORE",
            Self::ShopNow => "SHOP_NOW",
            Self::SignUp => "SIGN_UP",
            Self::Download => "DOWNLOAD",
            Self::ApplyNow => "APPLY_NOW",
            Self::BookNow => "BOOK_NOW",
            Self::Subscribe => "SUBSCRIBE",
            Self::BuyNow => "BUY_NOW",
            Self::ContactUs => "CONTACT_US",
            Self::GetQuote => "GET_QUOTE",
            Self::OrderNow => "ORDER_NOW",
            Self::CallNow => "CALL_NOW",
            Self::LikePage => "LIKE_PAGE",
            Self::WhatsAppMessage => "WHATSAPP_MESSAGE",
            Self::GetDirections => "GET_DIRECTIONS",
            Self::InstallApp => "INSTALL_MOBILE_APP",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LearnMore => "learn_more",
            Self::ShopNow => "shop_now",
            Self::SignUp => "sign_up",
            Self::Download => "download",
            Self::ApplyNow => "apply_now",
            Self::BookNow => "book_now",
            Self::Subscribe => "subscribe",
            Self::BuyNow => "buy_now",
            Self::ContactUs => "contact_us",
            Self::GetQuote => "get_quote",
            Self::OrderNow => "order_now",
            Self::CallNow => "call_now",
            Self::LikePage => "like_page",
            Self::WhatsAppMessage => "whatsapp_message",
            Self::GetDirections => "get_directions",
            Self::InstallApp => "install_app",
        }
    }

    /// Page-identity CTAs: Meta's value uses `page`, not a destination link.
    pub fn uses_page(self) -> bool {
        matches!(self, Self::LikePage | Self::CallNow)
    }

    pub fn requires_geo_link(self) -> bool {
        matches!(self, Self::GetDirections)
    }

    pub fn requires_app(self) -> bool {
        matches!(self, Self::InstallApp)
    }
}

impl FromStr for LinkCallToAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "learn_more" | "LEARN_MORE" => Ok(Self::LearnMore),
            "shop_now" | "SHOP_NOW" => Ok(Self::ShopNow),
            "sign_up" | "SIGN_UP" => Ok(Self::SignUp),
            "download" | "DOWNLOAD" => Ok(Self::Download),
            "apply_now" | "APPLY_NOW" => Ok(Self::ApplyNow),
            "book_now" | "BOOK_NOW" => Ok(Self::BookNow),
            "subscribe" | "SUBSCRIBE" => Ok(Self::Subscribe),
            "buy_now" | "BUY_NOW" => Ok(Self::BuyNow),
            "contact_us" | "CONTACT_US" => Ok(Self::ContactUs),
            "get_quote" | "GET_QUOTE" => Ok(Self::GetQuote),
            "order_now" | "ORDER_NOW" => Ok(Self::OrderNow),
            "call_now" | "CALL_NOW" => Ok(Self::CallNow),
            "like_page" | "LIKE_PAGE" => Ok(Self::LikePage),
            "whatsapp_message" | "WHATSAPP_MESSAGE" => Ok(Self::WhatsAppMessage),
            "get_directions" | "GET_DIRECTIONS" => Ok(Self::GetDirections),
            "install_app" | "INSTALL_MOBILE_APP" | "INSTALL_APP" => Ok(Self::InstallApp),
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

/// Bytes for one account-scoped ad video. Same path-free contract as images:
/// the CLI reads the file; the library never sees a filesystem path.
#[derive(Clone, Debug, PartialEq)]
pub struct UploadAdVideoRequest {
    pub account: Option<String>,
    pub filename: String,
    pub bytes: Vec<u8>,
}

impl UploadAdVideoRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_account(self.account.as_deref())?;
        if self.filename.trim().is_empty()
            || self.filename.contains('/')
            || self.filename.contains('\\')
        {
            return Err("invalid_video_filename".into());
        }
        if self.bytes.is_empty() {
            return Err("video_file_empty".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UploadedAdVideo {
    pub site: Site,
    pub account_id: String,
    pub id: String,
}

/// Meta `status.video_status`: ready, processing, uploading, error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdVideoStatusKind {
    Ready,
    Processing,
    Uploading,
    Error,
}

impl AdVideoStatusKind {
    pub fn from_meta(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ready" => Self::Ready,
            "error" => Self::Error,
            "uploading" => Self::Uploading,
            _ => Self::Processing,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Processing => "processing",
            Self::Uploading => "uploading",
            Self::Error => "error",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Ready | Self::Error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdVideoStatusRequest {
    pub video_id: String,
}

impl AdVideoStatusRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("video_id", &self.video_id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdVideoStatus {
    pub site: Site,
    pub video_id: String,
    pub video_status: AdVideoStatusKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

/// Bounded poll result. Pending at deadline is not a delivery claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AdVideoWait {
    Ready(AdVideoStatus),
    Error(AdVideoStatus),
    Pending(AdVideoStatus),
}

/// Page-backed video creative. Thumbnail `image_hash` is required so Meta
/// does not invent a frame. `video_id` is the `/advideos` asset.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VideoAdCreative {
    pub name: String,
    pub page_id: String,
    pub video_id: String,
    pub image_hash: String,
    pub message: String,
    pub destination_url: String,
    pub call_to_action: LinkCallToAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geo_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instagram_user_id: Option<String>,
    #[serde(default)]
    pub advantage_plus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_identity: Option<WhatsAppStatusIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateVideoAdCreativeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub creative: VideoAdCreative,
}

impl CreateVideoAdCreativeRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_account(self.account.as_deref())?;
        require_name(&self.creative.name)?;
        require_numeric_id("page_id", &self.creative.page_id)?;
        require_numeric_id("video_id", &self.creative.video_id)?;
        require_text("image_hash", &self.creative.image_hash)?;
        require_text("message", &self.creative.message)?;
        require_https_url("destination_url", &self.creative.destination_url)?;
        validate_link_cta_values(&LinkAdCreative {
            name: self.creative.name.clone(),
            page_id: self.creative.page_id.clone(),
            image_hash: self.creative.image_hash.clone(),
            message: self.creative.message.clone(),
            headline: String::new(),
            destination_url: self.creative.destination_url.clone(),
            call_to_action: self.creative.call_to_action,
            geo_link: self.creative.geo_link.clone(),
            application_id: self.creative.application_id.clone(),
            app_link: self.creative.app_link.clone(),
            instagram_user_id: self.creative.instagram_user_id.clone(),
            advantage_plus: self.creative.advantage_plus,
            whatsapp_identity: self.creative.whatsapp_identity.clone(),
        })?;
        validate_creative_identity(
            self.creative.instagram_user_id.as_deref(),
            self.creative.whatsapp_identity.as_ref(),
        )?;
        validate_status_creative_compat(
            self.creative.whatsapp_identity.as_ref(),
            self.creative.advantage_plus,
            true,
        )?;
        Ok(())
    }
}

/// One carousel card. Meta allows 2–10; 3+ is recommended.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CarouselCard {
    pub image_hash: String,
    pub link: String,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CarouselAdCreative {
    pub name: String,
    pub page_id: String,
    pub message: String,
    pub call_to_action: LinkCallToAction,
    pub cards: Vec<CarouselCard>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instagram_user_id: Option<String>,
    #[serde(default)]
    pub advantage_plus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_identity: Option<WhatsAppStatusIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CatalogAdCreative {
    pub name: String,
    pub page_id: String,
    pub product_set_id: String,
    pub link: String,
    pub message: String,
    pub call_to_action: LinkCallToAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instagram_user_id: Option<String>,
    #[serde(default)]
    pub advantage_plus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_identity: Option<WhatsAppStatusIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LeadFormAdCreative {
    pub name: String,
    pub page_id: String,
    pub image_hash: String,
    pub message: String,
    pub headline: String,
    pub destination_url: String,
    pub lead_gen_form_id: String,
    pub call_to_action: LinkCallToAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instagram_user_id: Option<String>,
    #[serde(default)]
    pub advantage_plus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_identity: Option<WhatsAppStatusIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppInstallAdCreative {
    pub name: String,
    pub page_id: String,
    pub image_hash: String,
    pub message: String,
    pub application_id: String,
    pub object_store_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instagram_user_id: Option<String>,
    #[serde(default)]
    pub advantage_plus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_identity: Option<WhatsAppStatusIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdCreativeKind {
    Carousel(CarouselAdCreative),
    Catalog(CatalogAdCreative),
    LeadForm(LeadFormAdCreative),
    AppInstall(AppInstallAdCreative),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateAdCreativeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub kind: AdCreativeKind,
}

impl CreateAdCreativeRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_account(self.account.as_deref())?;
        match &self.kind {
            AdCreativeKind::Carousel(c) => {
                require_name(&c.name)?;
                require_numeric_id("page_id", &c.page_id)?;
                require_text("message", &c.message)?;
                if c.cards.len() < 2 || c.cards.len() > 10 {
                    return Err("carousel_cards_out_of_range".into());
                }
                for (i, card) in c.cards.iter().enumerate() {
                    require_text("image_hash", &card.image_hash)
                        .map_err(|_| format!("missing_card_image_hash:{i}"))?;
                    require_https_url("link", &card.link)?;
                    require_text("name", &card.name)
                        .map_err(|_| format!("missing_card_name:{i}"))?;
                }
                validate_creative_identity(
                    c.instagram_user_id.as_deref(),
                    c.whatsapp_identity.as_ref(),
                )?;
                validate_status_creative_compat(
                    c.whatsapp_identity.as_ref(),
                    c.advantage_plus,
                    false,
                )?;
            }
            AdCreativeKind::Catalog(c) => {
                require_name(&c.name)?;
                require_numeric_id("page_id", &c.page_id)?;
                require_numeric_id("product_set_id", &c.product_set_id)?;
                require_https_url("link", &c.link)?;
                require_text("message", &c.message)?;
                validate_creative_identity(
                    c.instagram_user_id.as_deref(),
                    c.whatsapp_identity.as_ref(),
                )?;
                validate_status_creative_compat(
                    c.whatsapp_identity.as_ref(),
                    c.advantage_plus,
                    false,
                )?;
            }
            AdCreativeKind::LeadForm(c) => {
                require_name(&c.name)?;
                require_numeric_id("page_id", &c.page_id)?;
                require_text("image_hash", &c.image_hash)?;
                require_text("message", &c.message)?;
                require_text("headline", &c.headline)?;
                require_https_url("destination_url", &c.destination_url)?;
                require_numeric_id("lead_gen_form_id", &c.lead_gen_form_id)?;
                if !matches!(
                    c.call_to_action,
                    LinkCallToAction::SignUp
                        | LinkCallToAction::ApplyNow
                        | LinkCallToAction::LearnMore
                        | LinkCallToAction::Download
                ) {
                    return Err("lead_form_cta_unsupported".into());
                }
                validate_creative_identity(
                    c.instagram_user_id.as_deref(),
                    c.whatsapp_identity.as_ref(),
                )?;
                validate_status_creative_compat(
                    c.whatsapp_identity.as_ref(),
                    c.advantage_plus,
                    true,
                )?;
            }
            AdCreativeKind::AppInstall(c) => {
                require_name(&c.name)?;
                require_numeric_id("page_id", &c.page_id)?;
                require_text("image_hash", &c.image_hash)?;
                require_text("message", &c.message)?;
                require_numeric_id("application_id", &c.application_id)?;
                require_https_url("object_store_url", &c.object_store_url)?;
                validate_creative_identity(
                    c.instagram_user_id.as_deref(),
                    c.whatsapp_identity.as_ref(),
                )?;
                validate_status_creative_compat(
                    c.whatsapp_identity.as_ref(),
                    c.advantage_plus,
                    true,
                )?;
            }
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
    /// Required for `get_directions` (`fbgeo://…` or HTTPS maps). Refused on
    /// every other CTA so a leftover geo link cannot hitch a ride.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geo_link: Option<String>,
    /// Required with `app_link` for `install_app`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_link: Option<String>,
    /// Instagram professional account that the ad posts as. Numeric Graph ID,
    /// not the deprecated `instagram_actor_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instagram_user_id: Option<String>,
    /// `degrees_of_freedom_spec.standard_enhancements` OPT_IN.
    #[serde(default)]
    pub advantage_plus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_identity: Option<WhatsAppStatusIdentity>,
}

/// Third-party WhatsApp Status creatives must name the identity (v26.0).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WhatsAppStatusIdentity {
    pub identity_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
}

impl WhatsAppStatusIdentity {
    pub fn validate(&self) -> Result<(), String> {
        require_numeric_id("wamo_whatsapp_identity_id", &self.identity_id)
    }

    pub fn meta_json(&self) -> serde_json::Value {
        let mut obj = serde_json::json!({ "wamo_whatsapp_identity_id": self.identity_id });
        if let Some(phone) = &self.phone_number {
            obj["whatsapp_phone_number"] = serde_json::Value::String(phone.clone());
        }
        obj
    }
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
        validate_link_cta_values(&self.creative)?;
        validate_creative_identity(
            self.creative.instagram_user_id.as_deref(),
            self.creative.whatsapp_identity.as_ref(),
        )?;
        validate_status_creative_compat(
            self.creative.whatsapp_identity.as_ref(),
            self.creative.advantage_plus,
            true,
        )?;
        Ok(())
    }
}

pub fn validate_status_creative_compat(
    whatsapp_identity: Option<&WhatsAppStatusIdentity>,
    advantage_plus: bool,
    allows_status: bool,
) -> Result<(), String> {
    if whatsapp_identity.is_none() {
        return Ok(());
    }
    // Status docs: carousel/collection/flexible formats and Advantage+
    // creative tools are not supported on this placement.
    if !allows_status {
        return Err("whatsapp_status_unsupported_creative_format".into());
    }
    if advantage_plus {
        return Err("whatsapp_status_incompatible_with_advantage_plus".into());
    }
    Ok(())
}

pub fn validate_creative_identity(
    instagram_user_id: Option<&str>,
    whatsapp_identity: Option<&WhatsAppStatusIdentity>,
) -> Result<(), String> {
    if let Some(id) = instagram_user_id {
        require_numeric_id("instagram_user_id", id)?;
    }
    if let Some(ident) = whatsapp_identity {
        ident.validate()?;
    }
    Ok(())
}

/// Extra CTA value fields must match the type. Meta's call-to-action value
/// object has no phone-number field; CALL_NOW/LIKE_PAGE/WHATSAPP_MESSAGE use
/// `page`. GET_DIRECTIONS needs a geo link; INSTALL_MOBILE_APP needs the app.
pub fn validate_link_cta_values(creative: &LinkAdCreative) -> Result<(), String> {
    let cta = creative.call_to_action;
    if cta.requires_geo_link() {
        let geo = creative
            .geo_link
            .as_deref()
            .ok_or_else(|| "missing_geo_link".to_string())?;
        if geo.starts_with("fbgeo://") {
            if geo.trim().len() <= "fbgeo://".len() {
                return Err("missing_geo_link".into());
            }
        } else {
            require_https_url("geo_link", geo)?;
        }
    } else if creative.geo_link.is_some() {
        return Err("geo_link_without_get_directions".into());
    }
    if cta.requires_app() {
        let application_id = creative
            .application_id
            .as_deref()
            .ok_or_else(|| "missing_application_id".to_string())?;
        require_numeric_id("application_id", application_id)?;
        let app_link = creative
            .app_link
            .as_deref()
            .ok_or_else(|| "missing_app_link".to_string())?;
        require_https_url("app_link", app_link)?;
    } else if creative.application_id.is_some() || creative.app_link.is_some() {
        return Err("app_fields_without_install_app".into());
    }
    Ok(())
}

/// Meta `call_to_action.value` for this image-link creative.
pub fn link_cta_value_json(creative: &LinkAdCreative) -> serde_json::Value {
    if creative.call_to_action == LinkCallToAction::WhatsAppMessage {
        // Status / click-to-WhatsApp: Meta's documented value is
        // `app_destination=whatsapp`, not `page`.
        serde_json::json!({ "app_destination": "whatsapp" })
    } else if creative.call_to_action.uses_page() {
        serde_json::json!({ "page": creative.page_id })
    } else if creative.call_to_action.requires_geo_link() {
        serde_json::json!({ "link": creative.geo_link.as_deref().unwrap_or_default() })
    } else if creative.call_to_action.requires_app() {
        serde_json::json!({
            "application": creative.application_id.as_deref().unwrap_or_default(),
            "app_link": creative.app_link.as_deref().unwrap_or_default(),
            "link": creative.destination_url,
        })
    } else {
        serde_json::json!({ "link": creative.destination_url })
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

/// Meta accepts `daily_budget` or `lifetime_budget`, never both, at one
/// object. `allow_none` is true for campaigns (ABO: budget lives on the ad
/// set) and for ad sets (CBO: budget lives on the campaign).
fn validate_budget_xor(
    daily: Option<u64>,
    lifetime: Option<u64>,
    allow_none: bool,
) -> Result<(), String> {
    match (daily, lifetime) {
        (None, None) if allow_none => Ok(()),
        (None, None) => Err("missing_budget".into()),
        (Some(0), _) | (_, Some(0)) => Err("budget_must_be_positive".into()),
        (Some(_), Some(_)) => Err("daily_and_lifetime_budget_mutually_exclusive".into()),
        (Some(_), None) | (None, Some(_)) => Ok(()),
    }
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

/// Vault `extra.token_kind` for an unattended Business Manager credential.
/// Distinct from a user OAuth token so refresh never calls `fb_exchange_token`.
pub const SYSTEM_USER_TOKEN_KIND: &str = "system_user";

/// How the stored Meta Ads token was obtained. Inspect surfaces this so an
/// operator can tell a System User credential from a paste-code user token
/// without printing the secret.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdsTokenKind {
    UserOauth,
    SystemUser,
    Unknown,
}

impl AdsTokenKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserOauth => "user_oauth",
            Self::SystemUser => "system_user",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_vault_extra(kind: Option<&str>) -> Self {
        match kind {
            Some(SYSTEM_USER_TOKEN_KIND) => Self::SystemUser,
            Some("user_oauth") => Self::UserOauth,
            _ => Self::Unknown,
        }
    }
}

/// `GET /debug_token` metadata. The access token itself is never stored here.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdsTokenInspection {
    pub site: Site,
    pub token_kind: AdsTokenKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_type: Option<String>,
    pub is_valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_access_expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application: Option<String>,
}

/// Marketing API Access Tier as Meta renamed it in 2026 (formerly AMSA).
/// Header values stay in `raw`; `tier` is the operator-facing Limited/Full map.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketingApiAccessTierKind {
    Limited,
    Full,
    Unknown,
}

impl MarketingApiAccessTierKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Limited => "limited",
            Self::Full => "full",
            Self::Unknown => "unknown",
        }
    }

    /// Map Meta's `ads_api_access_tier` header onto Limited/Full.
    /// `standard_access` is the historical Full-tier label; `development_access`
    /// / `limited_access` are Limited.
    pub fn from_header(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "standard_access" | "full" | "full_access" => Self::Full,
            "development_access" | "limited_access" | "limited" | "development" => Self::Limited,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MarketingApiAccessTier {
    pub site: Site,
    pub tier: MarketingApiAccessTierKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    /// Where the tier value came from (`response_header` or `dashboard`).
    pub source: String,
    /// App Dashboard path. The connector cannot change the tier.
    pub dashboard: String,
}

pub const MARKETING_API_ACCESS_TIER_DASHBOARD: &str =
    "App Dashboard → App Review → Permissions and features → Marketing API Access Tier";

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

    #[test]
    fn objective_is_closed_and_maps_to_meta_outcomes() {
        assert_eq!(
            BillingEvent::from_str("impressions").unwrap().meta_value(),
            "IMPRESSIONS"
        );
        assert_eq!(
            OptimizationGoal::from_str("REACH").unwrap().meta_value(),
            "REACH"
        );
        assert!(billing_event_allowed(
            OptimizationGoal::LinkClicks,
            BillingEvent::LinkClicks
        ));
        assert!(!billing_event_allowed(
            OptimizationGoal::Reach,
            BillingEvent::LinkClicks
        ));
        assert!(supported_adset_pairing(
            CampaignObjective::Awareness,
            OptimizationGoal::Reach,
            BillingEvent::Impressions
        ));
        assert!(!supported_adset_pairing(
            CampaignObjective::Awareness,
            OptimizationGoal::LinkClicks,
            BillingEvent::Impressions
        ));
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
            BidStrategy::from_str("cost_cap").unwrap().meta_value(),
            "COST_CAP"
        );
        assert_eq!(
            BidStrategy::from_str("lowest_cost_with_bid_cap")
                .unwrap()
                .meta_value(),
            "LOWEST_COST_WITH_BID_CAP"
        );
        assert_eq!(
            BidStrategy::from_str("lowest_cost_with_min_roas")
                .unwrap()
                .meta_value(),
            "LOWEST_COST_WITH_MIN_ROAS"
        );
        assert_eq!(
            BidStrategy::from_str("target_cost").unwrap_err(),
            "unknown_bid_strategy:target_cost"
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
            LinkCallToAction::from_str("shop_now").unwrap().meta_value(),
            "SHOP_NOW"
        );
        assert_eq!(
            LinkCallToAction::from_str("swipe_up_shop").unwrap_err(),
            "unknown_link_call_to_action:swipe_up_shop"
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

        let valid_video = UploadAdVideoRequest {
            account: Some("act_123".into()),
            filename: "hero.mp4".into(),
            bytes: b"video bytes".to_vec(),
        };
        assert!(valid_video.validate().is_ok());
        assert_eq!(
            UploadAdVideoRequest {
                filename: "private/hero.mp4".into(),
                ..valid_video.clone()
            }
            .validate()
            .unwrap_err(),
            "invalid_video_filename"
        );
        assert_eq!(
            UploadAdVideoRequest {
                bytes: vec![],
                ..valid_video
            }
            .validate()
            .unwrap_err(),
            "video_file_empty"
        );

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
        assert_eq!(
            AdsInventoryKind::from_str("creative").unwrap(),
            AdsInventoryKind::Creative
        );
        assert_eq!(AdsInventoryKind::Adset.graph_edge(), "adsets");
        assert_eq!(
            AdsInventoryKind::from_str("campaigns").unwrap_err(),
            "unknown_ads_inventory_kind:campaigns"
        );
        assert!(AdsInventoryRequest {
            account: Some("act_123".into()),
            kind: AdsInventoryKind::Ad,
        }
        .validate()
        .is_ok());
        assert_eq!(
            AdsInventoryRequest {
                account: Some("nope".into()),
                kind: AdsInventoryKind::Campaign,
            }
            .validate()
            .unwrap_err(),
            "bad_ad_account:nope"
        );
        assert!(AdsInspectRequest {
            kind: AdsInventoryKind::Adset,
            id: "456".into(),
        }
        .validate()
        .is_ok());
        assert_eq!(
            AdsInspectRequest {
                kind: AdsInventoryKind::Campaign,
                id: "campaign-1".into(),
            }
            .validate()
            .unwrap_err(),
            "bad_ads_inspect_id:campaign-1"
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
                geo_link: None,
                application_id: None,
                app_link: None,
                instagram_user_id: None,
                advantage_plus: false,
                whatsapp_identity: None,
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
                ..valid_creative.creative.clone()
            },
            ..valid_creative.clone()
        };
        assert_eq!(bad_page.validate().unwrap_err(), "bad_page_id:page-456");

        let missing_geo = CreateLinkAdCreativeRequest {
            creative: LinkAdCreative {
                call_to_action: LinkCallToAction::GetDirections,
                ..valid_creative.creative.clone()
            },
            ..valid_creative.clone()
        };
        assert_eq!(missing_geo.validate().unwrap_err(), "missing_geo_link");
        let directions = CreateLinkAdCreativeRequest {
            creative: LinkAdCreative {
                call_to_action: LinkCallToAction::GetDirections,
                geo_link: Some("fbgeo://37.48,-122.15,\"1601 Willow Rd\"".into()),
                ..valid_creative.creative.clone()
            },
            ..valid_creative.clone()
        };
        assert!(directions.validate().is_ok());
        assert_eq!(
            link_cta_value_json(&directions.creative)["link"].as_str(),
            Some("fbgeo://37.48,-122.15,\"1601 Willow Rd\"")
        );

        let missing_app = CreateLinkAdCreativeRequest {
            creative: LinkAdCreative {
                call_to_action: LinkCallToAction::InstallApp,
                ..valid_creative.creative.clone()
            },
            ..valid_creative.clone()
        };
        assert_eq!(
            missing_app.validate().unwrap_err(),
            "missing_application_id"
        );
        let install = CreateLinkAdCreativeRequest {
            creative: LinkAdCreative {
                call_to_action: LinkCallToAction::InstallApp,
                application_id: Some("1".into()),
                app_link: Some("https://apps.apple.com/app/id1".into()),
                ..valid_creative.creative.clone()
            },
            ..valid_creative.clone()
        };
        assert!(install.validate().is_ok());
        let leftover_geo = CreateLinkAdCreativeRequest {
            creative: LinkAdCreative {
                geo_link: Some("fbgeo://1,2".into()),
                ..valid_creative.creative.clone()
            },
            ..valid_creative.clone()
        };
        assert_eq!(
            leftover_geo.validate().unwrap_err(),
            "geo_link_without_get_directions"
        );

        let video = CreateVideoAdCreativeRequest {
            account: Some("123".into()),
            creative: VideoAdCreative {
                name: "Hero video".into(),
                page_id: "456".into(),
                video_id: "9001".into(),
                image_hash: "hash-1".into(),
                message: "Watch".into(),
                destination_url: "https://example.com/offer".into(),
                call_to_action: LinkCallToAction::LearnMore,
                geo_link: None,
                application_id: None,
                app_link: None,
                instagram_user_id: None,
                advantage_plus: false,
                whatsapp_identity: None,
            },
        };
        assert!(video.validate().is_ok());
        assert_eq!(
            CreateVideoAdCreativeRequest {
                creative: VideoAdCreative {
                    video_id: "vid".into(),
                    ..video.creative.clone()
                },
                ..video
            }
            .validate()
            .unwrap_err(),
            "bad_video_id:vid"
        );
        assert_eq!(
            link_cta_value_json(&valid_creative.creative),
            serde_json::json!({ "link": "https://example.com/offer" })
        );
        let like_page = LinkAdCreative {
            call_to_action: LinkCallToAction::LikePage,
            ..valid_creative.creative.clone()
        };
        assert_eq!(
            link_cta_value_json(&like_page),
            serde_json::json!({ "page": "456" })
        );
        let wa = LinkAdCreative {
            call_to_action: LinkCallToAction::WhatsAppMessage,
            ..valid_creative.creative.clone()
        };
        assert_eq!(
            link_cta_value_json(&wa),
            serde_json::json!({ "app_destination": "whatsapp" })
        );
    }

    #[test]
    fn extra_creative_formats_and_whatsapp_status_validate() {
        let card = |n: &str| CarouselCard {
            image_hash: format!("h{n}"),
            link: format!("https://example.com/{n}"),
            name: format!("Card {n}"),
        };
        let too_few = CreateAdCreativeRequest {
            account: None,
            kind: AdCreativeKind::Carousel(CarouselAdCreative {
                name: "C".into(),
                page_id: "1".into(),
                message: "m".into(),
                call_to_action: LinkCallToAction::ShopNow,
                cards: vec![card("1")],
                instagram_user_id: None,
                advantage_plus: false,
                whatsapp_identity: None,
            }),
        };
        assert_eq!(
            too_few.validate().unwrap_err(),
            "carousel_cards_out_of_range"
        );
        let ok = CreateAdCreativeRequest {
            account: None,
            kind: AdCreativeKind::Carousel(CarouselAdCreative {
                cards: vec![card("1"), card("2")],
                ..match too_few.kind {
                    AdCreativeKind::Carousel(c) => c,
                    _ => unreachable!(),
                }
            }),
        };
        assert!(ok.validate().is_ok());

        let mut targeting = sample_targeting();
        targeting.whatsapp_positions = vec![WhatsAppPosition::Status];
        assert_eq!(
            targeting.validate().unwrap_err(),
            "whatsapp_positions_without_whatsapp_platform"
        );
        targeting.publisher_platforms = vec![PublisherPlatform::Whatsapp];
        assert_eq!(
            targeting.validate().unwrap_err(),
            "whatsapp_status_requires_instagram_story"
        );
        targeting.publisher_platforms =
            vec![PublisherPlatform::Whatsapp, PublisherPlatform::Instagram];
        targeting.instagram_positions = vec![InstagramPosition::Story];
        assert_eq!(
            targeting.validate().unwrap_err(),
            "whatsapp_status_requires_user_age_unknown"
        );
        targeting.user_age_unknown = Some(false);
        assert!(targeting.validate().is_ok());

        let ident = WhatsAppStatusIdentity {
            identity_id: "9".into(),
            phone_number: None,
        };
        let status_carousel = CreateAdCreativeRequest {
            account: None,
            kind: AdCreativeKind::Carousel(CarouselAdCreative {
                whatsapp_identity: Some(ident.clone()),
                ..match ok.kind {
                    AdCreativeKind::Carousel(c) => c,
                    _ => unreachable!(),
                }
            }),
        };
        assert_eq!(
            status_carousel.validate().unwrap_err(),
            "whatsapp_status_unsupported_creative_format"
        );
    }

    #[test]
    fn paused_create_validation_rejects_invalid_shapes() {
        let bad_budget = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                name: "Test".into(),
                campaign_id: "12".into(),
                daily_budget: Some(0),
                lifetime_budget: None,
                bid_strategy: BidStrategy::LowestCostWithoutCap,
                bid_amount: None,
                roas_average_floor: None,
                billing_event: BillingEvent::Impressions,
                optimization_goal: OptimizationGoal::Reach,
                targeting: AdTargeting {
                    geo_locations: GeoLocations {
                        countries: vec!["MY".into()],
                    },
                    age_min: None,
                    age_max: None,
                    publisher_platforms: vec![],
                    facebook_positions: vec![],
                    instagram_positions: vec![],
                    whatsapp_positions: vec![],
                    user_age_unknown: None,
                },
                start_time: None,
                end_time: None,
                promoted_object: None,
            }),
        };
        assert_eq!(
            bad_budget.validate().unwrap_err(),
            "budget_must_be_positive"
        );

        let bad_targeting = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                name: "Test".into(),
                campaign_id: "12".into(),
                daily_budget: Some(100),
                lifetime_budget: None,
                bid_strategy: BidStrategy::LowestCostWithoutCap,
                bid_amount: None,
                roas_average_floor: None,
                billing_event: BillingEvent::Impressions,
                optimization_goal: OptimizationGoal::Reach,
                targeting: AdTargeting {
                    geo_locations: GeoLocations { countries: vec![] },
                    age_min: None,
                    age_max: None,
                    publisher_platforms: vec![],
                    facebook_positions: vec![],
                    instagram_positions: vec![],
                    whatsapp_positions: vec![],
                    user_age_unknown: None,
                },
                start_time: None,
                end_time: None,
                promoted_object: None,
            }),
        };
        assert_eq!(
            bad_targeting.validate().unwrap_err(),
            "targeting_missing_country"
        );
        assert_eq!(
            AdTargeting {
                geo_locations: GeoLocations {
                    countries: vec!["my".into()],
                },
                age_min: None,
                age_max: None,
                publisher_platforms: vec![],
                facebook_positions: vec![],
                instagram_positions: vec![],
                whatsapp_positions: vec![],
                user_age_unknown: None,
            }
            .validate()
            .unwrap_err(),
            "bad_country_code:my"
        );
    }

    fn sample_targeting() -> AdTargeting {
        AdTargeting {
            geo_locations: GeoLocations {
                countries: vec!["MY".into()],
            },
            age_min: None,
            age_max: None,
            publisher_platforms: vec![],
            facebook_positions: vec![],
            instagram_positions: vec![],
            whatsapp_positions: vec![],
            user_age_unknown: None,
        }
    }

    fn sample_adset() -> PausedAdset {
        PausedAdset {
            name: "Test".into(),
            campaign_id: "12".into(),
            daily_budget: Some(100),
            lifetime_budget: None,
            bid_strategy: BidStrategy::LowestCostWithoutCap,
            bid_amount: None,
            roas_average_floor: None,
            billing_event: BillingEvent::Impressions,
            optimization_goal: OptimizationGoal::Reach,
            targeting: sample_targeting(),
            start_time: None,
            end_time: None,
            promoted_object: None,
        }
    }

    fn sample_campaign() -> PausedCampaign {
        PausedCampaign {
            name: "Test".into(),
            objective: CampaignObjective::Awareness,
            special_ad_categories: vec![],
            daily_budget: None,
            lifetime_budget: None,
            is_adset_budget_sharing_enabled: false,
        }
    }

    #[test]
    fn budget_xor_and_campaign_sharing_are_local() {
        let both = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Campaign(PausedCampaign {
                daily_budget: Some(100),
                lifetime_budget: Some(200),
                ..sample_campaign()
            }),
        };
        assert_eq!(
            both.validate().unwrap_err(),
            "daily_and_lifetime_budget_mutually_exclusive"
        );

        let sharing = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Campaign(PausedCampaign {
                is_adset_budget_sharing_enabled: true,
                ..sample_campaign()
            }),
        };
        assert!(sharing.validate().is_ok());

        let cbo_and_sharing = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Campaign(PausedCampaign {
                daily_budget: Some(5000),
                is_adset_budget_sharing_enabled: true,
                ..sample_campaign()
            }),
        };
        assert_eq!(
            cbo_and_sharing.validate().unwrap_err(),
            "budget_sharing_incompatible_with_campaign_budget"
        );

        let cbo = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Campaign(PausedCampaign {
                daily_budget: Some(5000),
                ..sample_campaign()
            }),
        };
        assert!(cbo.validate().is_ok());

        let adset_both = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                daily_budget: Some(100),
                lifetime_budget: Some(200),
                ..sample_adset()
            }),
        };
        assert_eq!(
            adset_both.validate().unwrap_err(),
            "daily_and_lifetime_budget_mutually_exclusive"
        );

        // CBO child: the campaign holds the budget, so the ad set may omit
        // both fields. Meta still requires one level to have a budget.
        let cbo_child = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                daily_budget: None,
                lifetime_budget: None,
                ..sample_adset()
            }),
        };
        assert!(cbo_child.validate().is_ok());

        let lifetime = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                daily_budget: None,
                lifetime_budget: Some(20_000),
                ..sample_adset()
            }),
        };
        assert_eq!(
            lifetime.validate().unwrap_err(),
            "lifetime_budget_requires_end_time"
        );
        let lifetime_scheduled = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                daily_budget: None,
                lifetime_budget: Some(20_000),
                start_time: Some("2026-11-11T14:26:09-08:00".into()),
                end_time: Some("2026-11-21T14:26:09-08:00".into()),
                ..sample_adset()
            }),
        };
        assert!(lifetime_scheduled.validate().is_ok());
    }

    #[test]
    fn adset_schedule_is_rfc3339_and_ordered() {
        assert!(parse_adset_datetime("start_time", "2026-11-11T14:25:17-08:00").is_ok());
        // Meta curl examples omit the colon in the offset.
        assert!(parse_adset_datetime("start_time", "2026-11-11T14:25:17-0800").is_ok());
        assert!(parse_adset_datetime("start_time", "2026-11-11 14:25:17-08:00").is_ok());
        assert_eq!(
            parse_adset_datetime("start_time", "next tuesday").unwrap_err(),
            "bad_start_time:next tuesday"
        );

        let inverted = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                start_time: Some("2026-11-21T14:26:09-08:00".into()),
                end_time: Some("2026-11-11T14:26:09-08:00".into()),
                ..sample_adset()
            }),
        };
        assert_eq!(
            inverted.validate().unwrap_err(),
            "end_time_not_after_start_time"
        );
    }

    #[test]
    fn bid_strategies_require_their_constraint_fields() {
        let missing_cap = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                bid_strategy: BidStrategy::CostCap,
                ..sample_adset()
            }),
        };
        assert_eq!(missing_cap.validate().unwrap_err(), "missing_bid_amount");

        let cap = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                bid_strategy: BidStrategy::CostCap,
                bid_amount: Some(200),
                ..sample_adset()
            }),
        };
        assert!(cap.validate().is_ok());

        let bid_cap = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                bid_strategy: BidStrategy::LowestCostWithBidCap,
                bid_amount: Some(300),
                ..sample_adset()
            }),
        };
        assert!(bid_cap.validate().is_ok());

        let leftover_amount = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                bid_amount: Some(200),
                ..sample_adset()
            }),
        };
        assert_eq!(
            leftover_amount.validate().unwrap_err(),
            "bid_amount_without_cap_strategy"
        );

        let missing_roas = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                bid_strategy: BidStrategy::LowestCostWithMinRoas,
                ..sample_adset()
            }),
        };
        assert_eq!(
            missing_roas.validate().unwrap_err(),
            "missing_roas_average_floor"
        );

        let min_roas_wrong_goal = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                bid_strategy: BidStrategy::LowestCostWithMinRoas,
                roas_average_floor: Some(15_000),
                ..sample_adset()
            }),
        };
        assert_eq!(
            min_roas_wrong_goal.validate().unwrap_err(),
            "min_roas_requires_value_goal"
        );

        let min_roas = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                bid_strategy: BidStrategy::LowestCostWithMinRoas,
                roas_average_floor: Some(15_000),
                optimization_goal: OptimizationGoal::Value,
                promoted_object: Some(PromotedObject::Pixel {
                    pixel_id: "789".into(),
                    custom_event_type: CustomEventType::Purchase,
                }),
                ..sample_adset()
            }),
        };
        assert!(min_roas.validate().is_ok());

        let floor_too_small = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                bid_strategy: BidStrategy::LowestCostWithMinRoas,
                roas_average_floor: Some(50),
                optimization_goal: OptimizationGoal::Value,
                ..sample_adset()
            }),
        };
        assert_eq!(
            floor_too_small.validate().unwrap_err(),
            "roas_average_floor_out_of_range"
        );

        let both = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                bid_strategy: BidStrategy::LowestCostWithMinRoas,
                bid_amount: Some(200),
                roas_average_floor: Some(10_000),
                ..sample_adset()
            }),
        };
        assert_eq!(
            both.validate().unwrap_err(),
            "bid_amount_without_cap_strategy"
        );
    }

    #[test]
    fn promoted_object_is_required_for_conversion_goals() {
        let missing = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                optimization_goal: OptimizationGoal::OffsiteConversions,
                billing_event: BillingEvent::Impressions,
                ..sample_adset()
            }),
        };
        assert_eq!(
            missing.validate().unwrap_err(),
            "promoted_object_required:offsite_conversions"
        );

        let wrong_kind = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                optimization_goal: OptimizationGoal::OffsiteConversions,
                promoted_object: Some(PromotedObject::Page {
                    page_id: "456".into(),
                }),
                ..sample_adset()
            }),
        };
        assert_eq!(
            wrong_kind.validate().unwrap_err(),
            "promoted_object_kind_mismatch:offsite_conversions:page"
        );

        let pixel = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                optimization_goal: OptimizationGoal::OffsiteConversions,
                promoted_object: Some(PromotedObject::Pixel {
                    pixel_id: "789".into(),
                    custom_event_type: CustomEventType::Purchase,
                }),
                ..sample_adset()
            }),
        };
        assert!(pixel.validate().is_ok());
        assert_eq!(
            PromotedObject::Pixel {
                pixel_id: "789".into(),
                custom_event_type: CustomEventType::Purchase,
            }
            .meta_json(),
            serde_json::json!({"pixel_id":"789","custom_event_type":"PURCHASE"})
        );

        let bad_id = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                optimization_goal: OptimizationGoal::PageLikes,
                promoted_object: Some(PromotedObject::Page {
                    page_id: "page-x".into(),
                }),
                ..sample_adset()
            }),
        };
        assert_eq!(bad_id.validate().unwrap_err(), "bad_page_id:page-x");

        let app = CreatePausedAdRequest {
            account: Some("123".into()),
            create: PausedAdCreate::Adset(PausedAdset {
                optimization_goal: OptimizationGoal::AppInstalls,
                promoted_object: Some(PromotedObject::App {
                    application_id: "1".into(),
                    object_store_url: "https://apps.apple.com/app/id1".into(),
                }),
                ..sample_adset()
            }),
        };
        assert!(app.validate().is_ok());
    }
}
