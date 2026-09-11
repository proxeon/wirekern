#[cfg(feature = "whatsapp-cloud")]
use crate::facets::{WhatsAppAssets, WhatsAppSender};
use crate::facets::{AdsManager, InsightsSource, MediaReader, PageDirectory};
use crate::publisher::Publisher;
use crate::types::{Capability, Site};
use std::collections::HashMap;
use std::sync::Arc;

/// One registered site: the frozen [`Publisher`] seam plus optional facets.
///
/// A publisher-only registration is enough to post. Insights, ads, pages,
/// media, and WhatsApp are extra slots — missing means `UnsupportedCapability`,
/// not a default method on `Publisher` that the next connector would inherit.
pub struct Connector {
    publisher: Arc<dyn Publisher>,
    insights: Option<Arc<dyn InsightsSource>>,
    ads: Option<Arc<dyn AdsManager>>,
    pages: Option<Arc<dyn PageDirectory>>,
    media: Option<Arc<dyn MediaReader>>,
    #[cfg(feature = "whatsapp-cloud")]
    whatsapp: Option<Arc<dyn WhatsAppSender>>,
    #[cfg(feature = "whatsapp-cloud")]
    whatsapp_assets: Option<Arc<dyn WhatsAppAssets>>,
}

impl Connector {
    pub fn from_publisher(publisher: Arc<dyn Publisher>) -> Self {
        Self {
            publisher,
            insights: None,
            ads: None,
            pages: None,
            media: None,
            #[cfg(feature = "whatsapp-cloud")]
            whatsapp: None,
            #[cfg(feature = "whatsapp-cloud")]
            whatsapp_assets: None,
        }
    }

    pub fn insights(mut self, facet: Arc<dyn InsightsSource>) -> Self {
        self.insights = Some(facet);
        self
    }

    pub fn ads(mut self, facet: Arc<dyn AdsManager>) -> Self {
        self.ads = Some(facet);
        self
    }

    pub fn pages(mut self, facet: Arc<dyn PageDirectory>) -> Self {
        self.pages = Some(facet);
        self
    }

    pub fn media(mut self, facet: Arc<dyn MediaReader>) -> Self {
        self.media = Some(facet);
        self
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn whatsapp(mut self, facet: Arc<dyn WhatsAppSender>) -> Self {
        self.whatsapp = Some(facet);
        self
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn whatsapp_assets(mut self, facet: Arc<dyn WhatsAppAssets>) -> Self {
        self.whatsapp_assets = Some(facet);
        self
    }

    pub fn publisher(&self) -> Arc<dyn Publisher> {
        self.publisher.clone()
    }

    pub fn insights_facet(&self) -> Option<Arc<dyn InsightsSource>> {
        self.insights.clone()
    }

    pub fn ads_facet(&self) -> Option<Arc<dyn AdsManager>> {
        self.ads.clone()
    }

    pub fn pages_facet(&self) -> Option<Arc<dyn PageDirectory>> {
        self.pages.clone()
    }

    pub fn media_facet(&self) -> Option<Arc<dyn MediaReader>> {
        self.media.clone()
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn whatsapp_facet(&self) -> Option<Arc<dyn WhatsAppSender>> {
        self.whatsapp.clone()
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn whatsapp_assets_facet(&self) -> Option<Arc<dyn WhatsAppAssets>> {
        self.whatsapp_assets.clone()
    }
}

#[derive(Default)]
pub struct Registry {
    inner: HashMap<Site, Connector>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    /// Publisher-only registration. Extra verbs need [`Self::register_connector`].
    pub fn register(&mut self, p: Arc<dyn Publisher>) {
        self.register_connector(Connector::from_publisher(p));
    }

    pub fn register_connector(&mut self, c: Connector) {
        self.inner.insert(c.publisher.site().clone(), c);
    }

    pub fn get(&self, site: &Site) -> Option<Arc<dyn Publisher>> {
        self.inner.get(site).map(|c| c.publisher())
    }

    pub fn connector(&self, site: &Site) -> Option<&Connector> {
        self.inner.get(site)
    }

    pub fn sites(&self) -> impl Iterator<Item = &Site> {
        self.inner.keys()
    }

    pub fn capabilities_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (site, c) in &self.inner {
            let caps: Vec<&str> = c
                .publisher
                .capabilities()
                .iter()
                .map(|cap| cap.as_str())
                .collect();
            map.insert(site.as_str().into(), serde_json::json!(caps));
        }
        serde_json::Value::Object(map)
    }

    pub fn capabilities_for(&self, site: &Site) -> Option<Vec<Capability>> {
        self.inner
            .get(site)
            .map(|c| c.publisher.capabilities().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::publisher::{AuthKind, Publisher};
    use crate::types::{AccountCreds, AppConfig, Deadline, Intent, Outcome, WhoAmI};
    use async_trait::async_trait;

    struct PublishOnly {
        site: Site,
    }

    #[async_trait]
    impl Publisher for PublishOnly {
        fn site(&self) -> &Site {
            &self.site
        }
        fn capabilities(&self) -> &[Capability] {
            &[Capability::PublishText, Capability::ReadMetrics]
        }
        fn auth_kind(&self) -> AuthKind {
            AuthKind::None
        }
        async fn publish(
            &self,
            _app: &AppConfig,
            _creds: &AccountCreds,
            intent: Intent,
            _deadline: Deadline,
        ) -> Result<Outcome, Error> {
            Ok(Outcome {
                site: intent.site,
                id: Some("p".into()),
                url: None,
                limits: None,
            })
        }
        async fn whoami(&self, _app: &AppConfig, _creds: &AccountCreds) -> Result<WhoAmI, Error> {
            Ok(WhoAmI {
                site: self.site.clone(),
                id: "1".into(),
                handle: None,
            })
        }
    }

    #[test]
    fn publisher_only_registration_has_no_optional_facets() {
        // Advertising a capability is not an implementation. A connector
        // that lists `read.metrics` without attaching InsightsSource must
        // not get a default method on Publisher — Client fail-closes on
        // the missing facet instead.
        let mut registry = Registry::new();
        registry.register(Arc::new(PublishOnly {
            site: Site::new("threads"),
        }));
        let site = Site::new("threads");
        let connector = registry.connector(&site).expect("registered");
        assert!(connector.insights_facet().is_none());
        assert!(connector.ads_facet().is_none());
        assert!(connector.pages_facet().is_none());
        assert!(connector.media_facet().is_none());
        assert_eq!(
            registry.capabilities_for(&site).unwrap(),
            vec![Capability::PublishText, Capability::ReadMetrics]
        );
    }
}
