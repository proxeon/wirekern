//! Safety policy for advertising-management actions.
//!
//! The policy is intentionally a separate boundary from a connector: a
//! network implementation must receive approval before it sees credentials.
//! That makes a future activation endpoint opt-in by construction rather than
//! an accidental extension of a paused-create path.

use crate::ads::PausedAdCreate;
use crate::error::Error;
use crate::types::Site;
#[cfg(feature = "whatsapp-cloud")]
use crate::{
    customer_window_open, normalize_recipient, ConsentKind, WhatsAppConsent, WhatsAppLedger,
    WhatsAppMessage, WhatsAppSendRequest,
};
#[cfg(feature = "whatsapp-cloud")]
use std::sync::Arc;
#[cfg(feature = "whatsapp-cloud")]
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// Emergency stop. Cannot start spend; default policy allows it.
    Pause,
    Archive,
    Delete,
    Duplicate,
    UpdateBudget,
    UpdateBid,
    UpdateSchedule,
    UpdatePlacement,
    UpdateTargeting,
    SwapCreative,
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
            Self::Pause => "pause",
            Self::Archive => "archive",
            Self::Delete => "delete",
            Self::Duplicate => "duplicate",
            Self::UpdateBudget => "update_budget",
            Self::UpdateBid => "update_bid",
            Self::UpdateSchedule => "update_schedule",
            Self::UpdatePlacement => "update_placement",
            Self::UpdateTargeting => "update_targeting",
            Self::SwapCreative => "swap_creative",
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
            | AdsAction::CreateLinkAdCreative
            // Pause stops delivery. It cannot start spend, so the paused-only
            // default keeps it as the emergency valve.
            | AdsAction::Pause => Ok(()),
            AdsAction::Activate
            | AdsAction::Archive
            | AdsAction::Delete
            | AdsAction::Duplicate
            | AdsAction::UpdateBudget
            | AdsAction::UpdateBid
            | AdsAction::UpdateSchedule
            | AdsAction::UpdatePlacement
            | AdsAction::UpdateTargeting
            | AdsAction::SwapCreative => Err(Error::PolicyDenied {
                site: site.clone(),
                action: action.as_str().into(),
                reason: "paused_only".into(),
            }),
        }
    }
}

/// Opt-in for **one** extra spend-shaped action. `--allow-activate` must not
/// also unlock delete or a budget edit.
#[derive(Clone, Copy, Debug)]
pub struct AllowAdsActionPolicy {
    extra: AdsAction,
}

impl AllowAdsActionPolicy {
    pub fn new(extra: AdsAction) -> Self {
        Self { extra }
    }
}

impl AdsPolicy for AllowAdsActionPolicy {
    fn authorize(&self, site: &Site, action: AdsAction) -> Result<(), Error> {
        if action == self.extra {
            return Ok(());
        }
        PausedOnlyAdsPolicy.authorize(site, action)
    }
}

/// X direct messages are private, recipient-targeted external writes. They
/// share the X identity with public posts but must never inherit publishing's
/// implicit authorization merely because a caller registered the connector.
#[cfg(feature = "x")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XDirectMessageAction {
    SendText,
}

#[cfg(feature = "x")]
impl XDirectMessageAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SendText => "send_x_direct_message",
        }
    }
}

/// Independent from public X publishing so application embedders can allow
/// scheduled/operator-controlled posts without also granting access to a
/// private-message transport.
#[cfg(feature = "x")]
pub trait XDirectMessagePolicy: Send + Sync {
    fn authorize(&self, site: &Site, action: XDirectMessageAction) -> Result<(), Error>;
}

/// Production default for X DMs. CLI users must acknowledge the exact send
/// with `--allow-dm`; library users install an allow policy consciously.
#[cfg(feature = "x")]
#[derive(Default)]
pub struct NoXDirectMessagesPolicy;

#[cfg(feature = "x")]
impl XDirectMessagePolicy for NoXDirectMessagesPolicy {
    fn authorize(&self, site: &Site, action: XDirectMessageAction) -> Result<(), Error> {
        Err(Error::PolicyDenied {
            site: site.clone(),
            action: action.as_str().into(),
            reason: "explicit_x_dm_send_required".into(),
        })
    }
}

/// Opt-in policy for a caller that has made its own recipient, consent, and
/// retention decision. X can still reject a send due to recipient settings,
/// blocks, or rate limits; an allow policy is not a delivery guarantee.
#[cfg(feature = "x")]
#[derive(Default)]
pub struct AllowXDirectMessagesPolicy;

#[cfg(feature = "x")]
impl XDirectMessagePolicy for AllowXDirectMessagesPolicy {
    fn authorize(&self, _site: &Site, _action: XDirectMessageAction) -> Result<(), Error> {
        Ok(())
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

    /// Policies that only distinguish message classes can implement
    /// [`Self::authorize`] alone. The stricter file-backed policy receives
    /// the closed request too, which is necessary to make consent and the
    /// 24-hour service window an actual authorization decision rather than
    /// a dashboard hint.
    fn authorize_request(
        &self,
        site: &Site,
        action: WhatsAppAction,
        _request: &WhatsAppSendRequest,
    ) -> Result<(), Error> {
        self.authorize(site, action)
    }
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

/// Compliance policy used by Wirekern's file-backed CLI and HTTP products.
///
/// It is intentionally conservative but does not pretend that local state is
/// a complete Meta compliance system:
///
/// * a recorded opt-out always refuses customer-visible content;
/// * templates require an explicit, local opt-in record;
/// * non-template customer messages require an observed inbound message in
///   the last 24 hours; and
/// * mark-read and typing acknowledgements are not customer content and are
///   left to Meta's inbound-wamid validation.
///
/// Library embedding remains opt-in: callers that install
/// [`AllowWhatsAppSendsPolicy`] retain the documented operator-managed model.
/// The concrete CLI/server path installs this policy when a caller has also
/// supplied the explicit `--allow-send` acknowledgement.
#[cfg(feature = "whatsapp-cloud")]
pub struct EnforceWhatsAppCompliancePolicy {
    ledger: Arc<dyn WhatsAppLedger>,
    consent: Arc<dyn WhatsAppConsent>,
}

#[cfg(feature = "whatsapp-cloud")]
impl EnforceWhatsAppCompliancePolicy {
    pub fn new(ledger: Arc<dyn WhatsAppLedger>, consent: Arc<dyn WhatsAppConsent>) -> Self {
        Self { ledger, consent }
    }

    fn denied(site: &Site, action: WhatsAppAction, reason: &str) -> Error {
        Error::PolicyDenied {
            site: site.clone(),
            action: action.as_str().into(),
            reason: reason.into(),
        }
    }
}

#[cfg(feature = "whatsapp-cloud")]
impl WhatsAppPolicy for EnforceWhatsAppCompliancePolicy {
    fn authorize(&self, _site: &Site, _action: WhatsAppAction) -> Result<(), Error> {
        // Account/Flow/template administration has its own explicit `--yes`
        // acknowledgement. This policy only adds context-sensitive checks to
        // customer-addressed Cloud API requests.
        Ok(())
    }

    fn authorize_request(
        &self,
        site: &Site,
        action: WhatsAppAction,
        request: &WhatsAppSendRequest,
    ) -> Result<(), Error> {
        if request.recipient_type != crate::RecipientType::Individual {
            // A group ID cannot be matched to one person's consent/window
            // record. Refuse rather than treating an opaque group as opted in.
            return Err(Self::denied(
                site,
                action,
                "whatsapp_compliance_group_recipient_unsupported",
            ));
        }

        let Some(raw_recipient) = request.message.recipient() else {
            return Ok(());
        };
        // The request was structurally validated before this hook. Normalize
        // once more for the storage lookup so `+60 11…` and `6011…` cannot
        // accidentally create two different local consent records.
        let recipient = normalize_recipient(raw_recipient)
            .map_err(|_| Self::denied(site, action, "whatsapp_recipient_invalid"))?;
        let recipient = recipient.trim_start_matches('+');

        match self.consent.get(recipient)? {
            Some(record) if record.kind == ConsentKind::OptOut => {
                return Err(Self::denied(site, action, "whatsapp_consent_opted_out"));
            }
            Some(_) | None => {}
        }

        if matches!(request.message, WhatsAppMessage::Template { .. }) {
            return match self.consent.get(recipient)? {
                Some(record) if record.kind == ConsentKind::OptIn => Ok(()),
                // A missing local record is not evidence of consent. Meta
                // still independently validates template policy/approval.
                _ => Err(Self::denied(site, action, "whatsapp_consent_missing")),
            };
        }

        if request.message.requires_customer_service_window() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_secs())
                .unwrap_or(0);
            let open = self
                .ledger
                .last_inbound_at(recipient)?
                .is_some_and(|at| customer_window_open(at, now));
            if !open {
                return Err(Self::denied(
                    site,
                    action,
                    "whatsapp_customer_window_closed",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "whatsapp-cloud"))]
mod whatsapp_compliance_tests {
    use super::*;
    use crate::{
        ConsentRecord, MemoryWhatsAppConsent, MemoryWhatsAppLedger, RecipientType, WhatsAppLedger,
    };

    fn text(to: &str) -> WhatsAppSendRequest {
        WhatsAppSendRequest {
            message: WhatsAppMessage::Text {
                to: to.into(),
                text: "hello".into(),
                preview_url: false,
            },
            idempotency_key: "compliance-1".into(),
            recipient_type: RecipientType::Individual,
        }
    }

    fn template(to: &str) -> WhatsAppSendRequest {
        WhatsAppSendRequest {
            message: WhatsAppMessage::Template {
                to: to.into(),
                name: "order_update".into(),
                language: "en_US".into(),
                body_parameters: vec![],
                named_body_parameters: vec![],
                header: None,
                buttons: vec![],
                limited_time_offer: None,
            },
            idempotency_key: "compliance-2".into(),
            recipient_type: RecipientType::Individual,
        }
    }

    fn policy() -> (
        EnforceWhatsAppCompliancePolicy,
        Arc<MemoryWhatsAppLedger>,
        Arc<MemoryWhatsAppConsent>,
    ) {
        let ledger = Arc::new(MemoryWhatsAppLedger::new());
        let consent = Arc::new(MemoryWhatsAppConsent::new());
        (
            EnforceWhatsAppCompliancePolicy::new(ledger.clone(), consent.clone()),
            ledger,
            consent,
        )
    }

    #[test]
    fn strict_policy_allows_in_window_customer_service_without_template_opt_in() {
        let (policy, ledger, _) = policy();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        ledger.remember_inbound_from("60123456789", now).unwrap();

        policy
            .authorize_request(
                &Site::new("whatsapp_cloud"),
                WhatsAppAction::SendText,
                &text("+60 123456789"),
            )
            .unwrap();
    }

    #[test]
    fn strict_policy_refuses_out_of_window_free_form_content() {
        let (policy, ledger, _) = policy();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        ledger
            .remember_inbound_from("60123456789", now - crate::CUSTOMER_WINDOW_SECS)
            .unwrap();

        let error = policy
            .authorize_request(
                &Site::new("whatsapp_cloud"),
                WhatsAppAction::SendText,
                &text("60123456789"),
            )
            .unwrap_err();
        assert!(
            matches!(error, Error::PolicyDenied { reason, .. } if reason == "whatsapp_customer_window_closed")
        );
    }

    #[test]
    fn strict_policy_requires_an_opt_in_for_templates() {
        let (policy, _, _) = policy();
        let error = policy
            .authorize_request(
                &Site::new("whatsapp_cloud"),
                WhatsAppAction::SendTemplate,
                &template("60123456789"),
            )
            .unwrap_err();
        assert!(
            matches!(error, Error::PolicyDenied { reason, .. } if reason == "whatsapp_consent_missing")
        );
    }

    #[test]
    fn strict_policy_honours_opt_out_even_inside_the_customer_window() {
        let (policy, ledger, consent) = policy();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        ledger.remember_inbound_from("60123456789", now).unwrap();
        consent
            .put(&ConsentRecord {
                wa_id: "60123456789".into(),
                kind: ConsentKind::OptOut,
                at: now,
            })
            .unwrap();

        let error = policy
            .authorize_request(
                &Site::new("whatsapp_cloud"),
                WhatsAppAction::SendText,
                &text("60123456789"),
            )
            .unwrap_err();
        assert!(
            matches!(error, Error::PolicyDenied { reason, .. } if reason == "whatsapp_consent_opted_out")
        );
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
        assert!(policy.authorize(&site, AdsAction::Pause).is_ok());
        let err = policy.authorize(&site, AdsAction::Activate).unwrap_err();
        assert!(
            matches!(err, Error::PolicyDenied { action, reason, .. } if action == "activate" && reason == "paused_only")
        );
        let allowed = AllowAdsActionPolicy::new(AdsAction::Activate);
        assert!(allowed.authorize(&site, AdsAction::Activate).is_ok());
        assert!(allowed.authorize(&site, AdsAction::Pause).is_ok());
        let still_denied = allowed.authorize(&site, AdsAction::Delete).unwrap_err();
        assert!(
            matches!(still_denied, Error::PolicyDenied { action, reason, .. } if action == "delete" && reason == "paused_only")
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
