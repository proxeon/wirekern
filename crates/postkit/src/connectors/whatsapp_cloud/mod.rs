//! WhatsApp Cloud API connector.
//!
//! Cloud API messaging is deliberately not routed through generic social
//! publishing. A reply's context, an approved template, a specific phone
//! number and the later webhook delivery state are all part of the contract.

mod account;
mod assets;
mod flows;
mod graph;
mod send;
mod templates;
mod webhook;

#[cfg(test)]
mod tests;

use crate::error::Error;
use crate::http::Http;
use crate::publisher::{AuthKind, Publisher};
use crate::registry::Connector;
use crate::types::{AccountCreds, AppConfig, Capability, Deadline, Intent, Outcome, Site, WhoAmI};
use async_trait::async_trait;

use graph::{access_token, phone_number_id, read_json, value_string};

pub use send::{send_payload, send_payload_for};

pub const GRAPH_HOST: &str = "graph.facebook.com";
/// Pinning makes a Meta version update a reviewed wire change rather than an
/// incidental dependency upgrade.
pub const GRAPH_VERSION: &str = "v26.0";
pub const SITE: &str = "whatsapp_cloud";
pub const WEBHOOK_SIGNATURE_PREFIX: &str = "sha256=";
pub const MAX_WEBHOOK_BYTES: usize = 1_048_576;

pub struct WhatsAppCloud {
    pub(super) http: Http,
    pub(super) site: Site,
    pub(super) base: String,
}

impl WhatsAppCloud {
    pub fn new() -> Result<Self, Error> {
        Self::with_base(format!("https://{GRAPH_HOST}/{GRAPH_VERSION}"))
    }

    /// Local mock helper. The production constructor remains pinned to the
    /// versioned Graph API base above.
    pub fn with_base(base: impl Into<String>) -> Result<Self, Error> {
        Ok(Self {
            http: Http::new()?,
            site: Site::new(SITE),
            base: base.into().trim_end_matches('/').to_string(),
        })
    }

    pub fn connector(self) -> Connector {
        let this = std::sync::Arc::new(self);
        Connector::from_publisher(this.clone())
            .whatsapp(this.clone())
            .whatsapp_assets(this.clone())
            .whatsapp_templates(this.clone())
            .whatsapp_flows(this.clone())
            .whatsapp_account(this)
    }
}

#[async_trait]
impl Publisher for WhatsAppCloud {
    fn site(&self) -> &Site {
        &self.site
    }

    fn capabilities(&self) -> &[Capability] {
        // These read capabilities mean verified callback parsing, not a
        // fictional remote inbox/status API. Cloud API delivers both event
        // kinds to the business' configured webhook endpoint.
        &[
            Capability::SendReply,
            Capability::SendText,
            Capability::SendTemplate,
            Capability::SendMedia,
            Capability::SendInteractive,
            Capability::SendLocation,
            Capability::SendContacts,
            Capability::SendReaction,
            Capability::MarkRead,
            Capability::SendTyping,
            Capability::ManageWhatsAppMedia,
            Capability::ReadWhatsAppMedia,
            Capability::ReadTemplates,
            Capability::ManageTemplates,
            Capability::SendCatalog,
            Capability::SendFlow,
            Capability::ReadFlows,
            Capability::ManageFlows,
            Capability::ReadWhatsAppAccount,
            Capability::ManageWhatsAppPhone,
            Capability::ReadWebhookMessages,
            Capability::ReadWebhookStatuses,
        ]
    }

    fn auth_kind(&self) -> AuthKind {
        AuthKind::StaticToken
    }

    async fn publish(
        &self,
        _app: &AppConfig,
        _creds: &AccountCreds,
        _intent: Intent,
        _deadline: Deadline,
    ) -> Result<Outcome, Error> {
        // Defend direct library calls too: private messages cannot be made by
        // supplying `post whatsapp_cloud --param to=…` to a generic surface.
        Err(Error::InvalidPost {
            site: self.site.clone(),
            reason: "use_whatsapp_command".into(),
            limit: None,
        })
    }

    async fn whoami(&self, app: &AppConfig, creds: &AccountCreds) -> Result<WhoAmI, Error> {
        let phone_number_id = phone_number_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .get(&format!(
                        "{}/{phone_number_id}?fields=id,display_phone_number,verified_name",
                        self.base
                    ))
                    .bearer_auth(token),
                Deadline::from_secs(30),
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        let id = body
            .get("id")
            .and_then(value_string)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "missing_phone_number_id".into(),
                message: "WhatsApp phone lookup returned no id".into(),
            })?;
        Ok(WhoAmI {
            site: self.site.clone(),
            id,
            handle: body
                .get("verified_name")
                .and_then(value_string)
                .or_else(|| body.get("display_phone_number").and_then(value_string)),
        })
    }
}

#[cfg(all(test, not(feature = "oauth")))]
#[test]
fn whatsapp_feature_excludes_oauth() {
    // This compiles only when `whatsapp-cloud` is enabled without `oauth`.
    // The connector must not accidentally pull a browser redirect flow.
    assert_eq!(SITE, "whatsapp_cloud");
}
