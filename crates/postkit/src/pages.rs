//! Public, token-free Facebook Page discovery types.
//!
//! A Page access token is necessary to make an organic post, but it is not
//! useful output for an operator or a script and is more sensitive than the
//! Page's public identity. Connectors keep that transient token private and
//! return only this deliberately small discovery contract.

use crate::types::Site;
use serde::{Deserialize, Serialize};

/// One Page visible to the authenticated Facebook user.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PageAccount {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Facebook's task names are platform-defined. Preserve them verbatim so
    /// an operator can see why a Page appears but cannot be posted to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,
}

/// Read-only discovery response. It intentionally has no default Page: a
/// publish must name `page_id` each time, rather than trusting list order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PagesReply {
    pub site: Site,
    pub pages: Vec<PageAccount>,
}
