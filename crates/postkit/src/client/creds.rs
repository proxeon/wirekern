//! Shared credential load, refresh, capability, and facet lookup.
use super::{empty_app, refresh_is_due, Client, CredOp};
use crate::error::Error;
#[cfg(feature = "whatsapp-cloud")]
use crate::facets::WhatsAppSender;
use crate::facets::{AdsManager, InsightsSource, MediaReader, PageDirectory};
use crate::publisher::Publisher;
use crate::types::{AccountCreds, AccountKey, AppConfig, Capability, Deadline, Site};
use std::sync::Arc;

impl Client {
    /// Shared load + optional proactive refresh. Every network verb that
    /// talks with stored OAuth creds goes through here so a missed retry
    /// cannot land on only one of insights/pages/ads.
    pub(super) async fn prepare_creds(
        &self,
        key: &AccountKey,
        deadline: Deadline,
        proactive: bool,
    ) -> Result<(Arc<dyn Publisher>, AppConfig, AccountCreds), Error> {
        let publisher = self.publisher(&key.site)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut creds = self.vault.get(key)?;
        if proactive {
            creds = self
                .maybe_refresh(&*publisher, &app, key, creds, deadline)
                .await?;
        }
        Ok((publisher, app, creds))
    }

    /// One reactive refresh. `token_expired` is the only error that retries;
    /// every other error is returned as-is so callers cannot accidentally
    /// retry a visible write.
    pub(super) async fn recover_expired(
        &self,
        publisher: &dyn Publisher,
        app: &AppConfig,
        key: &AccountKey,
        creds: AccountCreds,
        deadline: Deadline,
        err: Error,
    ) -> Result<AccountCreds, Error> {
        match err {
            Error::Auth { reason, .. } if reason == "token_expired" => {
                let new = publisher.refresh(app, &creds, deadline).await?;
                self.vault.put(key, &new)?;
                Ok(new)
            }
            other => Err(other),
        }
    }

    /// Proactive refresh, then the operation, then at most one
    /// `token_expired` → refresh → retry. The next read/write verb must
    /// call this instead of copying the match.
    pub(super) async fn with_creds<T, F>(
        &self,
        key: &AccountKey,
        deadline: Deadline,
        op: F,
    ) -> Result<T, Error>
    where
        F: Fn(AppConfig, AccountCreds) -> CredOp<T>,
    {
        let (publisher, app, creds) = self.prepare_creds(key, deadline, true).await?;
        match op(app.clone(), creds.clone()).await {
            Err(e) => {
                let creds = self
                    .recover_expired(&*publisher, &app, key, creds, deadline, e)
                    .await?;
                op(app, creds).await
            }
            other => other,
        }
    }

    pub(super) fn publisher(&self, site: &Site) -> Result<Arc<dyn Publisher>, Error> {
        self.registry
            .get(site)
            .ok_or_else(|| Error::UnknownSite(site.clone()))
    }

    pub(super) fn require_capability(&self, site: &Site, need: Capability) -> Result<(), Error> {
        let publisher = self.publisher(site)?;
        if publisher.capabilities().contains(&need) {
            Ok(())
        } else {
            Err(Error::UnsupportedCapability {
                site: site.clone(),
                need,
            })
        }
    }

    /// Facet lookup is fail-closed: advertising a capability without attaching
    /// the matching trait is the same as not implementing it. That is what
    /// keeps `Publisher` frozen — extra verbs cannot sneak in as default
    /// methods the next connector would inherit.
    pub(super) fn missing_facet(site: &Site, need: Capability) -> Error {
        Error::UnsupportedCapability {
            site: site.clone(),
            need,
        }
    }

    pub(super) fn insights_source(
        &self,
        site: &Site,
        need: Capability,
    ) -> Result<Arc<dyn InsightsSource>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.insights_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
    }

    pub(super) fn ads_manager(
        &self,
        site: &Site,
        need: Capability,
    ) -> Result<Arc<dyn AdsManager>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.ads_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
    }

    pub(super) fn page_directory(&self, site: &Site) -> Result<Arc<dyn PageDirectory>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.pages_facet())
            .ok_or_else(|| Self::missing_facet(site, Capability::ReadPages))
    }

    pub(super) fn media_reader(&self, site: &Site) -> Result<Arc<dyn MediaReader>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.media_facet())
            .ok_or_else(|| Self::missing_facet(site, Capability::ReadMedia))
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub(super) fn whatsapp_assets(
        &self,
        site: &Site,
        need: Capability,
    ) -> Result<Arc<dyn crate::facets::WhatsAppAssets>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.whatsapp_assets_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub(super) fn whatsapp_account(
        &self,
        site: &Site,
        need: Capability,
    ) -> Result<Arc<dyn crate::facets::WhatsAppAccount>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.whatsapp_account_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub(super) fn whatsapp_flows(
        &self,
        site: &Site,
        need: Capability,
    ) -> Result<Arc<dyn crate::facets::WhatsAppFlows>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.whatsapp_flows_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub(super) fn whatsapp_templates(
        &self,
        site: &Site,
        need: Capability,
    ) -> Result<Arc<dyn crate::facets::WhatsAppTemplates>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.whatsapp_templates_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub(super) fn whatsapp_sender(
        &self,
        site: &Site,
        need: Capability,
    ) -> Result<Arc<dyn WhatsAppSender>, Error> {
        self.registry
            .connector(site)
            .and_then(|c| c.whatsapp_facet())
            .ok_or_else(|| Self::missing_facet(site, need))
    }

    pub(super) async fn maybe_refresh(
        &self,
        publisher: &dyn Publisher,
        app: &AppConfig,
        key: &AccountKey,
        creds: AccountCreds,
        deadline: Deadline,
    ) -> Result<AccountCreds, Error> {
        if !refresh_is_due(&creds) {
            return Ok(creds);
        }
        // The caller's deadline governs the refresh too (issue 024): one
        // budget for refresh plus the request it serves. A deadline spent
        // here degrades below — publish then fails fast with the same
        // DeadlineExceeded instead of duplicating the wait.
        match publisher.refresh(app, &creds, deadline).await {
            Ok(new) => {
                self.vault.put(key, &new)?;
                Ok(new)
            }
            Err(Error::Auth { reason, .. }) if reason == "no_refresh" => Ok(creds),
            // Proactive refresh is an optimization, not a prerequisite: it
            // fires while the stored token is still valid (up to 7 days
            // left), so a transient failure — network, 5xx, rate limit,
            // timeout — must degrade to publishing with the current token.
            // The next publish retries the refresh. Auth failures stay
            // fatal: a rejected refresh means the session is dead, and
            // failing fast with a re-auth error beats dying later inside
            // publish.
            Err(
                Error::Network { .. }
                | Error::RateLimited { .. }
                | Error::Platform { .. }
                | Error::DeadlineExceeded { .. },
            ) => Ok(creds),
            Err(e) => Err(e),
        }
    }
}
