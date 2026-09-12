//! Insights query types — the read seam (026 §3 flag 1).
//!
//! Core, ungated, plain data: like `Intent`/`Outcome`, these belong to the
//! shared vocabulary so every future metrics connector (meta_ads now, IG /
//! TikTok / Google per 028) speaks the same query shape. Dates are validated
//! with hand-rolled calendar math rather than a date crate so this module
//! compiles under `--no-default-features` (the `time` crate is an optional
//! dep today, and the read seam must not force it on).

use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Hardest bound on a query range. `time_increment=1` daily rows mean a
/// range is also the maximum reply size — 90 rows — which keeps the read
/// seam token-bounded by construction (026 §5).
pub const MAX_RANGE_DAYS: i64 = 90;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightsLevel {
    #[default]
    Account,
    Campaign,
    Adset,
    Ad,
}

impl InsightsLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Campaign => "campaign",
            Self::Adset => "adset",
            Self::Ad => "ad",
        }
    }

    /// Graph insights rows key their entity id by level; this is the field
    /// name the connector reads. Meta-specific, but named here so the row
    /// mapping lives beside the level enum it depends on.
    pub fn id_field(self) -> &'static str {
        match self {
            Self::Account => "account_id",
            Self::Campaign => "campaign_id",
            Self::Adset => "adset_id",
            Self::Ad => "ad_id",
        }
    }
}

impl FromStr for InsightsLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "account" => Ok(Self::Account),
            "campaign" => Ok(Self::Campaign),
            "adset" => Ok(Self::Adset),
            "ad" => Ok(Self::Ad),
            other => Err(format!("unknown_level:{other}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Spend,
    Impressions,
    Clicks,
    Reach,
    Ctr,
    Cpc,
    Cpm,
    Purchases,
    PurchaseValue,
    Roas,
    /// Estimated average impressions per person reached. Not spend.
    Frequency,
    /// Estimated unique people who clicked. Not a billing event.
    UniqueClicks,
    /// Clicks on the ad's destination link.
    InlineLinkClicks,
    /// Destination-link clicks / impressions.
    InlineLinkClickCtr,
    /// Meta delivery diagnostic (`ABOVE_AVERAGE` / …). Not a cost.
    QualityRanking,
    /// ThruPlay count from Meta's video action array. Not spend.
    VideoThruplay,
}

impl Metric {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spend => "spend",
            Self::Impressions => "impressions",
            Self::Clicks => "clicks",
            Self::Reach => "reach",
            Self::Ctr => "ctr",
            Self::Cpc => "cpc",
            Self::Cpm => "cpm",
            Self::Purchases => "purchases",
            Self::PurchaseValue => "purchase_value",
            Self::Roas => "roas",
            Self::Frequency => "frequency",
            Self::UniqueClicks => "unique_clicks",
            Self::InlineLinkClicks => "inline_link_clicks",
            Self::InlineLinkClickCtr => "inline_link_click_ctr",
            Self::QualityRanking => "quality_ranking",
            Self::VideoThruplay => "video_thruplay",
        }
    }

    /// Operator-facing definition so a metric cannot be misread as an invoice.
    pub fn definition(self) -> &'static str {
        match self {
            Self::Spend => {
                "Ad-delivery spend in the ad account currency for the window. Not an invoice or account balance."
            }
            Self::Impressions => "Times the ad was served. Estimated. Not spend.",
            Self::Clicks => "All clicks on the ad. Not a billing event.",
            Self::Reach => "Estimated unique people who saw the ad. Not spend.",
            Self::Ctr => "Clicks / impressions. A rate, not a cost.",
            Self::Cpc => "Spend / clicks in account currency. A ratio, not a billing total.",
            Self::Cpm => "Spend per 1,000 impressions. A ratio, not a billing total.",
            Self::Purchases => {
                "Counted purchase actions (pixel/API). Attribution-window dependent. Not an invoice."
            }
            Self::PurchaseValue => {
                "Sum of purchase action values in account currency. Attribution-window dependent. Not an invoice."
            }
            Self::Roas => "Purchase value / spend for the requested attribution window. Null if spend is 0.",
            Self::Frequency => "Estimated average impressions per person reached. Not spend.",
            Self::UniqueClicks => "Estimated unique people who clicked. Not a billing event.",
            Self::InlineLinkClicks => "Clicks on the ad's destination link.",
            Self::InlineLinkClickCtr => "Destination-link clicks / impressions. A rate, not a cost.",
            Self::QualityRanking => {
                "Meta delivery diagnostic (ABOVE_AVERAGE / AVERAGE / BELOW_AVERAGE / …). Not a cost."
            }
            Self::VideoThruplay => {
                "ThruPlay actions (video played to 15s or completion). Count, not spend."
            }
        }
    }
}

impl FromStr for Metric {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "spend" => Ok(Self::Spend),
            "impressions" => Ok(Self::Impressions),
            "clicks" => Ok(Self::Clicks),
            "reach" => Ok(Self::Reach),
            "ctr" => Ok(Self::Ctr),
            "cpc" => Ok(Self::Cpc),
            "cpm" => Ok(Self::Cpm),
            "purchases" => Ok(Self::Purchases),
            "purchase_value" => Ok(Self::PurchaseValue),
            "roas" => Ok(Self::Roas),
            "frequency" => Ok(Self::Frequency),
            "unique_clicks" => Ok(Self::UniqueClicks),
            "inline_link_clicks" => Ok(Self::InlineLinkClicks),
            "inline_link_click_ctr" => Ok(Self::InlineLinkClickCtr),
            "quality_ranking" => Ok(Self::QualityRanking),
            "video_thruplay" => Ok(Self::VideoThruplay),
            other => Err(format!("unknown_metric:{other}")),
        }
    }
}

/// Dimensions that Meta can split an insights row by in Tier A+. They are a
/// query argument, not metrics: a country or placement must never be summed
/// into a spend field by an unaware caller.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Breakdown {
    Country,
    PublisherPlatform,
    Age,
}

impl Breakdown {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Country => "country",
            Self::PublisherPlatform => "publisher_platform",
            Self::Age => "age",
        }
    }
}

impl FromStr for Breakdown {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "country" => Ok(Self::Country),
            "publisher_platform" => Ok(Self::PublisherPlatform),
            "age" => Ok(Self::Age),
            other => Err(format!("unknown_breakdown:{other}")),
        }
    }
}

/// Attribution is an explicit argument, never a silent default (026 §3):
/// ROAS answers change with the window, and a caller who cannot say which
/// window they meant cannot interpret the number they get back. The string
/// forms are operator-facing presets (Ads Manager display names); the Meta
/// connector maps them onto Graph's atomic `action_attribution_windows`
/// array on the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AttributionWindow {
    #[serde(rename = "7d_click_1d_view")]
    SevenDayClickOneDayView,
    #[serde(rename = "1d_click")]
    OneDayClick,
    #[serde(rename = "1d_view")]
    OneDayView,
}

impl AttributionWindow {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SevenDayClickOneDayView => "7d_click_1d_view",
            Self::OneDayClick => "1d_click",
            Self::OneDayView => "1d_view",
        }
    }
}

impl FromStr for AttributionWindow {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "7d_click_1d_view" => Ok(Self::SevenDayClickOneDayView),
            "1d_click" => Ok(Self::OneDayClick),
            "1d_view" => Ok(Self::OneDayView),
            other => Err(format!("unknown_attribution:{other}")),
        }
    }
}

/// Inclusive `[from, to]` range of calendar days, `YYYY-MM-DD`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DateRange {
    pub from: String,
    pub to: String,
}

impl DateRange {
    /// Real calendar dates, `from ≤ to`, span within [`MAX_RANGE_DAYS`].
    /// Returns a machine-readable reason for the wire error.
    pub fn validate(&self) -> Result<(), String> {
        let f = civil_of(&self.from)?;
        let t = civil_of(&self.to)?;
        let (fo, to) = (days_from_civil(f), days_from_civil(t));
        if fo > to {
            return Err("range_from_after_to".into());
        }
        if to - fo >= MAX_RANGE_DAYS {
            return Err(format!("range_too_long:{}", to - fo + 1));
        }
        Ok(())
    }
}

/// `YYYY-MM-DD` with real-calendar checks (month 1–12, day within month,
/// leap-year February). The format is strict: exactly 4-2-2 ASCII digits.
fn civil_of(s: &str) -> Result<(i32, u32, u32), String> {
    let bad = || format!("bad_date:{s}");
    let bytes = s.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(bad());
    }
    let digit = |b: u8| (b as char).is_ascii_digit().then_some((b - b'0') as i64);
    let num = |range: std::ops::Range<usize>| -> Result<i64, String> {
        let mut v = 0i64;
        for &b in &bytes[range] {
            v = v * 10 + digit(b).ok_or_else(bad)?;
        }
        Ok(v)
    };
    let (y, m, d) = (num(0..4)? as i32, num(5..7)? as u32, num(8..10)? as u32);
    if !(1..=12).contains(&m) {
        return Err(bad());
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=dim[(m - 1) as usize]).contains(&d) {
        return Err(bad());
    }
    Ok((y, m, d))
}

/// Days since 1970-01-01 for a valid civil date (Howard Hinnant's
/// `days_from_civil`): the compact, branchless-accurate way to order two
/// dates and measure their span without a date library.
fn days_from_civil((y, m, d): (i32, u32, u32)) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The read-side analogue of `Intent`: one query, bounded by construction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InsightsQuery {
    #[serde(default)]
    pub level: InsightsLevel,
    pub metrics: Vec<Metric>,
    pub range: DateRange,
    /// Required and explicit: see [`AttributionWindow`].
    pub attribution: AttributionWindow,
    /// Per-override of the stored ad account (accepts `123` or `act_123`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Repeatable, level-specific entity IDs. Connectors serialize these as
    /// their safe structured filter format instead of interpolating IDs into
    /// a path or query expression.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_ids: Vec<String>,
    /// Requested row dimensions, deliberately separate from [`Metric`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub breakdowns: Vec<Breakdown>,
}

/// One daily row. Metrics serialize as a JSON object (alphabetically keyed
/// by the default `serde_json::Map`, so the bytes are deterministic — the
/// property agents and golden tests rely on).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InsightRow {
    pub entity_id: String,
    pub level: InsightsLevel,
    pub date_start: String,
    /// Values for the query's requested [`Breakdown`]s. The map is omitted
    /// for an unbroken-down report so Tier A's output remains compact.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub dimensions: serde_json::Map<String, serde_json::Value>,
    pub metrics: serde_json::Map<String, serde_json::Value>,
}

/// A credential-visible advertising account. `id` is always the canonical
/// `act_<digits>` form accepted by `InsightsQuery.account` and the CLI.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdAccount {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Read-only account inventory, deliberately distinct from local vault
/// account aliases. This is how an operator discovers a Meta `act_<id>`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdAccountsReply {
    pub site: crate::types::Site,
    pub accounts: Vec<AdAccount>,
}

/// The read-side analogue of `Outcome`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InsightsReply {
    pub site: crate::types::Site,
    /// The ad account the numbers describe, `act_<id>`.
    pub account_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    pub rows: Vec<InsightRow>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_and_metric_round_trip() {
        for s in ["account", "campaign", "adset", "ad"] {
            assert_eq!(InsightsLevel::from_str(s).unwrap().as_str(), s);
        }
        for s in [
            "spend",
            "impressions",
            "clicks",
            "reach",
            "ctr",
            "cpc",
            "cpm",
            "purchases",
            "purchase_value",
            "roas",
            "frequency",
            "unique_clicks",
            "inline_link_clicks",
            "inline_link_click_ctr",
            "quality_ranking",
            "video_thruplay",
        ] {
            assert_eq!(Metric::from_str(s).unwrap().as_str(), s);
        }
        assert!(Metric::Spend.definition().contains("Not an invoice"));
        assert!(Metric::Frequency.definition().contains("Not spend"));
        assert!(Metric::QualityRanking.definition().contains("Not a cost"));
        assert_eq!(
            InsightsLevel::from_str("nope").unwrap_err(),
            "unknown_level:nope"
        );
        assert_eq!(
            Metric::from_str("not_a_metric").unwrap_err(),
            "unknown_metric:not_a_metric"
        );
    }

    #[test]
    fn breakdown_round_trip_and_unknown_value() {
        for s in ["country", "publisher_platform", "age"] {
            assert_eq!(Breakdown::from_str(s).unwrap().as_str(), s);
        }
        assert_eq!(
            Breakdown::from_str("device_platform").unwrap_err(),
            "unknown_breakdown:device_platform"
        );
    }

    #[test]
    fn attribution_round_trip_and_no_default() {
        for s in ["7d_click_1d_view", "1d_click", "1d_view"] {
            assert_eq!(AttributionWindow::from_str(s).unwrap().as_str(), s);
        }
        assert!(AttributionWindow::from_str("default").is_err());
    }

    #[test]
    fn valid_ranges_pass() {
        DateRange {
            from: "2026-01-01".into(),
            to: "2026-03-31".into(),
        }
        .validate()
        .unwrap(); // 90 days inclusive — the boundary
        DateRange {
            from: "2024-02-29".into(),
            to: "2024-02-29".into(),
        }
        .validate()
        .unwrap(); // leap day, single day
    }

    #[test]
    fn invalid_dates_and_ranges_fail_with_reasons() {
        let bad_dates = [
            "2026-13-01",
            "2026-00-10",
            "2026-04-31",
            "2023-02-29", // not a leap year
            "2026-1-1",
            "2026-01-1",
            "26-01-01",
            "2026/01/01",
            "2026-01-01x",
            "",
        ];
        for d in bad_dates {
            let err = DateRange {
                from: d.into(),
                to: "2026-01-02".into(),
            }
            .validate()
            .unwrap_err();
            assert!(err.starts_with("bad_date:"), "{d}: {err}");
        }
        assert_eq!(
            DateRange {
                from: "2026-03-01".into(),
                to: "2026-01-01".into(),
            }
            .validate()
            .unwrap_err(),
            "range_from_after_to"
        );
        let err = DateRange {
            from: "2026-01-01".into(),
            to: "2026-04-01".into(), // 91 days inclusive — one past the cap
        }
        .validate()
        .unwrap_err();
        assert_eq!(err, "range_too_long:91");
    }
}
