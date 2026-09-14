//! Review status, token inspection, and Marketing API access tier.
use crate::types::Site;
use serde::{Deserialize, Serialize};

use super::helpers::require_numeric_id;
use super::AdEntity;

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

/// One operator-facing problem Meta attached to an advertising object. Meta
/// may omit any individual property, so preserve the stable fields it did
/// supply instead of replacing a specific review failure with a generic
/// Wirekern error.
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
/// configured state is the explicit state Wirekern requested. They differ
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
