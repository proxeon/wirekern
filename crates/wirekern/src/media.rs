//! Bounded published-media read types.
//!
//! Media listings are deliberately a small first page, not a connector's
//! promise to mirror a whole social profile. The bound keeps remote response
//! size, token use, and JSON output predictable for CLI and library callers.

use crate::types::Site;
use serde::{Deserialize, Serialize};

/// The largest first page Wirekern will ask a connector to return in v1.
/// Pagination is intentionally absent until it has a reviewed cursor and
/// cancellation contract; raising this limit would quietly weaken that bound.
pub const MAX_MEDIA_LIMIT: u8 = 25;
/// A useful recent-history default without making an ordinary CLI read noisy.
pub const DEFAULT_MEDIA_LIMIT: u8 = 10;

/// A bounded request for recent published media.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MediaQuery {
    pub limit: u8,
}

impl Default for MediaQuery {
    fn default() -> Self {
        Self {
            limit: DEFAULT_MEDIA_LIMIT,
        }
    }
}

impl MediaQuery {
    /// Local validation is shared by the CLI and library path so an embedding
    /// caller cannot turn a small read into an unbounded platform request.
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_MEDIA_LIMIT).contains(&self.limit) {
            return Err("media_limit_out_of_range".into());
        }
        Ok(())
    }
}

/// One platform-published item. All display properties are optional because
/// APIs can omit them by media type, account configuration, or field rollout;
/// the remote media ID remains the only required identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PublishedMedia {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permalink: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// A connector's single, bounded first page. Results retain platform order:
/// changing it locally would discard a provider's recency semantics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MediaReply {
    pub site: Site,
    pub media: Vec<PublishedMedia>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_query_has_a_small_default_and_closed_bounds() {
        assert_eq!(MediaQuery::default().limit, DEFAULT_MEDIA_LIMIT);
        assert!(MediaQuery { limit: 1 }.validate().is_ok());
        assert!(MediaQuery {
            limit: MAX_MEDIA_LIMIT
        }
        .validate()
        .is_ok());
        for limit in [0, MAX_MEDIA_LIMIT + 1] {
            assert_eq!(
                MediaQuery { limit }.validate().unwrap_err(),
                "media_limit_out_of_range"
            );
        }
    }

    #[test]
    fn absent_remote_fields_are_omitted_from_json() {
        let media = PublishedMedia {
            id: "123".into(),
            permalink: None,
            caption: None,
            media_type: None,
            timestamp: None,
        };
        assert_eq!(
            serde_json::to_value(media).unwrap(),
            serde_json::json!({ "id": "123" })
        );
    }
}
