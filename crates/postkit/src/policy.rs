//! Safety policy for advertising-management actions.
//!
//! The policy is intentionally a separate boundary from a connector: a
//! network implementation must receive approval before it sees credentials.
//! That makes a future activation endpoint opt-in by construction rather than
//! an accidental extension of a paused-create path.

use crate::ads::PausedAdCreate;
use crate::error::Error;
use crate::types::Site;

/// Every spend-shaped ads action has a named policy decision. When a future
/// variant is added, Rust requires every policy implementation to decide
/// whether it is safe; falling through to allow is impossible.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdsAction {
    CreatePausedCampaign,
    CreatePausedAdset,
    CreatePausedAd,
    UploadAdImage,
    CreateLinkAdCreative,
    Activate,
    UpdateBudget,
}

impl AdsAction {
    pub fn for_paused_create(create: &PausedAdCreate) -> Self {
        match create {
            PausedAdCreate::Campaign(_) => Self::CreatePausedCampaign,
            PausedAdCreate::Adset(_) => Self::CreatePausedAdset,
            PausedAdCreate::Ad(_) => Self::CreatePausedAd,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CreatePausedCampaign => "create_paused_campaign",
            Self::CreatePausedAdset => "create_paused_adset",
            Self::CreatePausedAd => "create_paused_ad",
            Self::UploadAdImage => "upload_ad_image",
            Self::CreateLinkAdCreative => "create_link_ad_creative",
            Self::Activate => "activate",
            Self::UpdateBudget => "update_budget",
        }
    }
}

/// Approves or refuses an advertising action before token lookup or HTTP.
/// Applications may provide a stricter implementation with
/// [`Client::with_ads_policy`](crate::Client::with_ads_policy).
pub trait AdsPolicy: Send + Sync {
    fn authorize(&self, site: &Site, action: AdsAction) -> Result<(), Error>;
}

/// The production default: drafts may be assembled while paused, but no
/// action that can start delivery or change future spend is approved.
#[derive(Default)]
pub struct PausedOnlyAdsPolicy;

impl AdsPolicy for PausedOnlyAdsPolicy {
    fn authorize(&self, site: &Site, action: AdsAction) -> Result<(), Error> {
        match action {
            AdsAction::CreatePausedCampaign
            | AdsAction::CreatePausedAdset
            | AdsAction::CreatePausedAd
            // Images and creatives are account assets, not delivery objects.
            // Their later use is still gated by the structurally paused ad.
            | AdsAction::UploadAdImage
            | AdsAction::CreateLinkAdCreative => Ok(()),
            AdsAction::Activate | AdsAction::UpdateBudget => Err(Error::PolicyDenied {
                site: site.clone(),
                action: action.as_str().into(),
                reason: "paused_only".into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_allows_non_delivering_assets_and_paused_creates() {
        let policy = PausedOnlyAdsPolicy;
        let site = Site::new("meta_ads");
        assert!(policy
            .authorize(&site, AdsAction::CreatePausedCampaign)
            .is_ok());
        assert!(policy.authorize(&site, AdsAction::UploadAdImage).is_ok());
        assert!(policy
            .authorize(&site, AdsAction::CreateLinkAdCreative)
            .is_ok());
        let err = policy.authorize(&site, AdsAction::Activate).unwrap_err();
        assert!(
            matches!(err, Error::PolicyDenied { action, reason, .. } if action == "activate" && reason == "paused_only")
        );
    }
}
