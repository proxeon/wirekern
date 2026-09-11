//! In-tree connector set for operator surfaces (CLI, serve, later MCP).
//!
//! Surfaces must not copy this list. A new in-tree site is registered here
//! once; every process that builds a file-backed `Client` then sees it.

use crate::error::Error;
use crate::registry::Registry;
#[cfg(feature = "vault-file")]
use crate::vault_file::{FileAppStore, FileVault};
#[cfg(feature = "vault-file")]
use crate::Client;
#[cfg(feature = "vault-file")]
use std::path::Path;
#[cfg(any(
    feature = "vault-file",
    feature = "threads",
    feature = "bluesky",
    feature = "meta-ads",
    feature = "facebook-pages",
    feature = "instagram",
    feature = "whatsapp-cloud"
))]
use std::sync::Arc;

/// Register every compiled in-tree connector. Feature flags keep a
/// threads-only library build from pulling Graph ads or WhatsApp.
pub fn bundled_registry() -> Result<Registry, Error> {
    #[allow(unused_mut)]
    let mut registry = Registry::new();
    // Publisher-only sites register the frozen seam. Extra verbs attach as
    // facets on Connector so the next site cannot enlarge Publisher.
    #[cfg(feature = "threads")]
    registry.register(Arc::new(crate::connectors::threads::Threads::new()?));
    #[cfg(feature = "bluesky")]
    registry.register(Arc::new(crate::connectors::bluesky::Bluesky::new()?));
    #[cfg(feature = "meta-ads")]
    registry.register_connector(crate::connectors::meta_ads::MetaAds::new()?.connector());
    #[cfg(feature = "facebook-pages")]
    registry
        .register_connector(crate::connectors::facebook_pages::FacebookPages::new()?.connector());
    #[cfg(feature = "instagram")]
    registry.register_connector(crate::connectors::instagram::Instagram::new()?.connector());
    #[cfg(feature = "whatsapp-cloud")]
    registry
        .register_connector(crate::connectors::whatsapp_cloud::WhatsAppCloud::new()?.connector());
    Ok(registry)
}

#[cfg(feature = "vault-file")]
impl Client {
    /// File-backed operator client: vault + apps under `home`, in-tree
    /// connectors from [`bundled_registry`]. CLI, serve, and MCP must call
    /// this instead of each assembling a `Registry`.
    ///
    /// `allow_whatsapp_send` is the process-level twin of `--allow-send`.
    /// HTTP still requires `"allow_send": true` per request.
    pub fn from_home(home: impl AsRef<Path>, allow_whatsapp_send: bool) -> Result<Self, Error> {
        let home = home.as_ref();
        let registry = bundled_registry()?;
        let vault = Arc::new(FileVault::new(home)?);
        let apps = Arc::new(FileAppStore::new(home)?);
        let client = Client::new(registry, vault, apps);
        #[cfg(feature = "whatsapp-cloud")]
        if allow_whatsapp_send {
            return Ok(
                client.with_whatsapp_policy(Arc::new(crate::policy::AllowWhatsAppSendsPolicy))
            );
        }
        let _ = allow_whatsapp_send;
        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_registry_lists_compiled_connectors() {
        let registry = bundled_registry().unwrap();
        #[cfg(not(any(
            feature = "threads",
            feature = "bluesky",
            feature = "meta-ads",
            feature = "facebook-pages",
            feature = "instagram",
            feature = "whatsapp-cloud"
        )))]
        assert!(registry.sites().next().is_none());
        #[cfg(feature = "threads")]
        assert!(registry.get(&crate::types::Site::new("threads")).is_some());
        #[cfg(feature = "bluesky")]
        assert!(registry.get(&crate::types::Site::new("bluesky")).is_some());
        #[cfg(feature = "whatsapp-cloud")]
        assert!(registry
            .get(&crate::types::Site::new("whatsapp_cloud"))
            .is_some());
    }

    #[cfg(feature = "vault-file")]
    #[test]
    fn from_home_creates_a_file_backed_client() {
        let tmp = tempfile::tempdir().unwrap();
        let denied = Client::from_home(tmp.path(), false).unwrap();
        assert!(denied.vault().list(None).unwrap().is_empty());
        let allowed = Client::from_home(tmp.path(), true).unwrap();
        assert!(allowed.vault().list(None).unwrap().is_empty());
    }
}
