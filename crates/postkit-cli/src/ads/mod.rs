//! Ads, insights, and paused-draft CLI verbs.

mod cmd;
mod dispatch;
mod helpers;

pub(crate) use cmd::*;
pub(crate) use dispatch::{apply_lifecycle_policy, dispatch, run_insights};
pub(crate) use helpers::*;
