//! Closed campaign objective, bid, billing, and ad-set pairing contracts.
use serde::{Deserialize, Serialize};
use std::str::FromStr;

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

/// Auction billing events Wirekern will send. Meta's historical CPA-only
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
