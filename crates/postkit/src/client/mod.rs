//! One credentialed attempt through the bundled registry, local vault, and policy.
use crate::apps::AppStore;
use crate::error::Error;
use crate::policy::{AdsPolicy, PausedOnlyAdsPolicy};
#[cfg(feature = "whatsapp-cloud")]
use crate::policy::{NoWhatsAppSendsPolicy, WhatsAppPolicy};
#[cfg(feature = "x")]
use crate::policy::{NoXDirectMessagesPolicy, XDirectMessagePolicy};
use crate::registry::Registry;
#[cfg(feature = "whatsapp-cloud")]
use crate::types::AccountKey;
use crate::types::{AccountCreds, AppConfig, Site};
use crate::vault::Vault;
#[cfg(feature = "whatsapp-cloud")]
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(feature = "whatsapp-cloud")]
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "meta-ads")]
mod ads;
#[cfg(feature = "meta-ads")]
mod ads_inner;
mod creds;
#[cfg(feature = "meta-ads")]
mod insights;
mod publish;
mod reads;

#[cfg(feature = "x")]
mod x;

#[cfg(feature = "whatsapp-cloud")]
mod whatsapp;

#[cfg(feature = "draft")]
mod draft;

/// One credentialed attempt. Boxed so `with_creds` can call the same
/// operation twice (first try, then once after a `token_expired` refresh)
/// on MSRV 1.80 without async closures.
pub(super) type CredOp<T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send>>;

/// Meta review normally takes longer than a single HTTP response but should
/// never turn a CLI call into an unbounded background worker. The public wait
/// method always caps polls with the caller's existing `Deadline`.
#[cfg(feature = "meta-ads")]
pub(super) const REVIEW_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

pub struct Client {
    pub(super) registry: Registry,
    pub(super) vault: Arc<dyn Vault>,
    pub(super) apps: Arc<dyn AppStore>,
    pub(super) ads_policy: Arc<dyn AdsPolicy>,
    #[cfg(feature = "x")]
    pub(super) x_direct_message_policy: Arc<dyn XDirectMessagePolicy>,
    #[cfg(feature = "whatsapp-cloud")]
    pub(super) whatsapp_policy: Arc<dyn WhatsAppPolicy>,
    #[cfg(feature = "whatsapp-cloud")]
    pub(super) whatsapp_ledger: Option<Arc<dyn crate::whatsapp_ops::WhatsAppLedger>>,
    #[cfg(feature = "whatsapp-cloud")]
    pub(super) whatsapp_consent: Option<Arc<dyn crate::whatsapp_ops::WhatsAppConsent>>,
    #[cfg(feature = "whatsapp-cloud")]
    pub(super) whatsapp_replay_dead_letters:
        Option<Arc<dyn crate::whatsapp_ops::WhatsAppReplayableDeadLetters>>,
    #[cfg(feature = "whatsapp-cloud")]
    // Pacing belongs to the Client, not an individual batch. Otherwise two
    // callers can both believe they own the next process-local send slot.
    pub(super) whatsapp_throughput:
        Arc<Mutex<HashMap<WhatsAppPacingKey, Arc<crate::whatsapp_ops::ThroughputQueue>>>>,
}

/// The Cloud API quota applies to a phone number, while Postkit credentials
/// are selected by an account alias. Retaining both avoids coupling two
/// independent sender aliases to one local queue just because they share a
/// token, and avoids splitting queues across different stored credentials.
#[cfg(feature = "whatsapp-cloud")]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct WhatsAppPacingKey {
    pub(super) account: AccountKey,
    pub(super) phone_number_id: String,
}

impl Client {
    pub fn new(registry: Registry, vault: Arc<dyn Vault>, apps: Arc<dyn AppStore>) -> Self {
        Self {
            registry,
            vault,
            apps,
            ads_policy: Arc::new(PausedOnlyAdsPolicy),
            #[cfg(feature = "x")]
            x_direct_message_policy: Arc::new(NoXDirectMessagesPolicy),
            #[cfg(feature = "whatsapp-cloud")]
            whatsapp_policy: Arc::new(NoWhatsAppSendsPolicy),
            #[cfg(feature = "whatsapp-cloud")]
            whatsapp_ledger: None,
            #[cfg(feature = "whatsapp-cloud")]
            whatsapp_consent: None,
            #[cfg(feature = "whatsapp-cloud")]
            whatsapp_replay_dead_letters: None,
            #[cfg(feature = "whatsapp-cloud")]
            whatsapp_throughput: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Replace only the advertising policy. Chain with
    /// [`Self::with_whatsapp_policy`] — the two domains are independent and
    /// must not reset each other.
    pub fn with_ads_policy(mut self, ads_policy: Arc<dyn AdsPolicy>) -> Self {
        self.ads_policy = ads_policy;
        self
    }

    /// Replace only the X direct-message policy. Public post publishing and
    /// every other connector keep their existing authorization boundary.
    #[cfg(feature = "x")]
    pub fn with_x_direct_message_policy(
        mut self,
        x_direct_message_policy: Arc<dyn XDirectMessagePolicy>,
    ) -> Self {
        self.x_direct_message_policy = x_direct_message_policy;
        self
    }

    /// Replace only the WhatsApp send policy. Ads stay whatever they were
    /// (paused-only by default).
    #[cfg(feature = "whatsapp-cloud")]
    pub fn with_whatsapp_policy(mut self, whatsapp_policy: Arc<dyn WhatsAppPolicy>) -> Self {
        self.whatsapp_policy = whatsapp_policy;
        self
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn with_whatsapp_ledger(
        mut self,
        ledger: Arc<dyn crate::whatsapp_ops::WhatsAppLedger>,
    ) -> Self {
        self.whatsapp_ledger = Some(ledger);
        self
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn with_whatsapp_consent(
        mut self,
        consent: Arc<dyn crate::whatsapp_ops::WhatsAppConsent>,
    ) -> Self {
        self.whatsapp_consent = Some(consent);
        self
    }

    /// Attach an explicit encrypted replay store. Leaving it unset preserves
    /// the privacy-first default: malformed signed callbacks retain only a
    /// hash audit entry in the normal delivery ledger.
    #[cfg(feature = "whatsapp-cloud")]
    pub fn with_whatsapp_replay_dead_letters(
        mut self,
        dead_letters: Arc<dyn crate::whatsapp_ops::WhatsAppReplayableDeadLetters>,
    ) -> Self {
        self.whatsapp_replay_dead_letters = Some(dead_letters);
        self
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn vault(&self) -> &dyn Vault {
        &*self.vault
    }

    pub fn apps(&self) -> &dyn AppStore {
        &*self.apps
    }
}

pub(super) fn empty_app(site: &Site) -> AppConfig {
    AppConfig {
        site: site.clone(),
        oauth: None,
        extra: serde_json::json!({}),
    }
}

/// Proactive refresh: `expires_at` within 7 days and last refresh ≥ 24h (009).
pub fn refresh_is_due(creds: &AccountCreds) -> bool {
    let AccountCreds::OAuth2 { extra, .. } = creds else {
        return false;
    };
    if extra.get("token_kind").and_then(|v| v.as_str()) == Some(crate::ads::SYSTEM_USER_TOKEN_KIND)
    {
        return false;
    }
    let expires_at = extra.get("expires_at").and_then(|v| v.as_u64());
    let Some(expires_at) = expires_at else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if expires_at.saturating_sub(now) > 7 * 24 * 3600 {
        return false;
    }
    let refreshed_at = extra
        .get("refreshed_at")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    now.saturating_sub(refreshed_at) >= 24 * 3600
}
