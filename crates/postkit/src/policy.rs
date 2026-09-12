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
    UploadAdVideo,
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
            Self::UploadAdVideo => "upload_ad_video",
            Self::CreateLinkAdCreative => "create_link_ad_creative",
            Self::Activate => "activate",
            Self::UpdateBudget => "update_budget",
        }
    }
}

/// Approves or refuses an advertising action before token lookup or HTTP.
/// Applications may provide a stricter implementation with
/// [`Client::with_ads_policy`](crate::Client::with_ads_policy). Chain it with
/// `with_whatsapp_policy` when both domains need a custom decision.
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
            | AdsAction::UploadAdVideo
            | AdsAction::CreateLinkAdCreative => Ok(()),
            AdsAction::Activate | AdsAction::UpdateBudget => Err(Error::PolicyDenied {
                site: site.clone(),
                action: action.as_str().into(),
                reason: "paused_only".into(),
            }),
        }
    }
}

/// Every private WhatsApp send receives its own decision. A message is not a
/// public post: templates can be billable and even replies must obey Meta's
/// customer-service rules, so falling through to allow would be unsafe.
#[cfg(feature = "whatsapp-cloud")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WhatsAppAction {
    SendReply,
    /// Free-form in-window text. Distinct from a contextual reply so a
    /// forgotten `wamid` cannot be faked as `context`.
    SendText,
    SendTemplate,
    SendMedia,
    SendInteractive,
    SendLocation,
    SendContacts,
    SendReaction,
    MarkRead,
    SendTyping,
    SendCatalog,
    SendFlow,
    ReadFlows,
    ManageFlows,
    ReadTemplates,
    ManageTemplates,
    ReadAccount,
    ManagePhone,
    SubscribeWebhooks,
}

#[cfg(feature = "whatsapp-cloud")]
impl WhatsAppAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SendReply => "send_whatsapp_reply",
            Self::SendText => "send_whatsapp_text",
            Self::SendTemplate => "send_whatsapp_template",
            Self::SendMedia => "send_whatsapp_media",
            Self::SendInteractive => "send_whatsapp_interactive",
            Self::SendLocation => "send_whatsapp_location",
            Self::SendContacts => "send_whatsapp_contacts",
            Self::SendReaction => "send_whatsapp_reaction",
            Self::MarkRead => "send_whatsapp_read",
            Self::SendTyping => "send_whatsapp_typing",
            Self::SendCatalog => "send_whatsapp_catalog",
            Self::SendFlow => "send_whatsapp_flow",
            Self::ReadFlows => "read_whatsapp_flows",
            Self::ManageFlows => "manage_whatsapp_flows",
            Self::ReadTemplates => "read_whatsapp_templates",
            Self::ManageTemplates => "manage_whatsapp_templates",
            Self::ReadAccount => "read_whatsapp_account",
            Self::ManagePhone => "manage_whatsapp_phone",
            Self::SubscribeWebhooks => "subscribe_whatsapp_webhooks",
        }
    }
}

/// Policy for outbound WhatsApp Cloud messages. It is independent of
/// `AdsPolicy`: Meta Ads spend and customer messaging have different risk and
/// approval models, and an application should be able to choose each one.
#[cfg(feature = "whatsapp-cloud")]
pub trait WhatsAppPolicy: Send + Sync {
    fn authorize(&self, site: &Site, action: WhatsAppAction) -> Result<(), Error>;
}

/// Production default: no private message leaves the process merely because
/// a caller registered a WhatsApp connector. The CLI replaces this only for a
/// command carrying its explicit `--allow-send` acknowledgement.
#[cfg(feature = "whatsapp-cloud")]
#[derive(Default)]
pub struct NoWhatsAppSendsPolicy;

#[cfg(feature = "whatsapp-cloud")]
impl WhatsAppPolicy for NoWhatsAppSendsPolicy {
    fn authorize(&self, site: &Site, action: WhatsAppAction) -> Result<(), Error> {
        Err(Error::PolicyDenied {
            site: site.clone(),
            action: action.as_str().into(),
            reason: "explicit_whatsapp_send_required".into(),
        })
    }
}

/// Opt-in policy for callers that have made their own consent, template and
/// billing decision. It does not claim Meta will deliver the message; the
/// signed status webhook is still the source of the final delivery state.
#[cfg(feature = "whatsapp-cloud")]
#[derive(Default)]
pub struct AllowWhatsAppSendsPolicy;

#[cfg(feature = "whatsapp-cloud")]
impl WhatsAppPolicy for AllowWhatsAppSendsPolicy {
    fn authorize(&self, _site: &Site, _action: WhatsAppAction) -> Result<(), Error> {
        Ok(())
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
        assert!(policy.authorize(&site, AdsAction::UploadAdVideo).is_ok());
        assert!(policy
            .authorize(&site, AdsAction::CreateLinkAdCreative)
            .is_ok());
        let err = policy.authorize(&site, AdsAction::Activate).unwrap_err();
        assert!(
            matches!(err, Error::PolicyDenied { action, reason, .. } if action == "activate" && reason == "paused_only")
        );
    }

    #[cfg(feature = "whatsapp-cloud")]
    #[test]
    fn whatsapp_sends_are_deny_by_default_and_explicitly_opt_in() {
        let site = Site::new("whatsapp_cloud");
        let denied = NoWhatsAppSendsPolicy
            .authorize(&site, WhatsAppAction::SendTemplate)
            .unwrap_err();
        assert!(matches!(denied, Error::PolicyDenied { action, reason, .. }
            if action == "send_whatsapp_template" && reason == "explicit_whatsapp_send_required"));
        assert!(AllowWhatsAppSendsPolicy
            .authorize(&site, WhatsAppAction::SendReply)
            .is_ok());
        let denied_text = NoWhatsAppSendsPolicy
            .authorize(&site, WhatsAppAction::SendText)
            .unwrap_err();
        assert!(matches!(denied_text, Error::PolicyDenied { action, .. }
            if action == "send_whatsapp_text"));
    }
}
