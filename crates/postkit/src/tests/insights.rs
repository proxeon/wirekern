//! Insights routing, capability, and range-validation Client tests.
use super::mock::*;
use crate::apps::MemoryAppStore;
use crate::client::Client;
use crate::error::Error;
use crate::publisher::{AuthKind, Publisher};
use crate::registry::Registry;
use crate::types::{
    AccountCreds, AccountKey, AppConfig, Capability, Deadline, Intent, Outcome, Site, WhoAmI,
};
use crate::vault::{MemoryVault, Vault};
use async_trait::async_trait;
use std::sync::Arc;

/// 026 read seam: Client routes the query to the connector, and the
/// capability check refuses a text-only mock before any HTTP.
#[tokio::test]
async fn client_insights_routes_and_checks_capability() {
    let (c, key) = setup(MockPub::metrics("meta_ads"));
    let reply = c
        .insights(
            &key,
            insights_query("2026-06-01", "2026-06-02"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(reply.account_id, "act_1");
    assert_eq!(reply.rows.len(), 1);

    // a site without read.metrics is refused before reaching the connector
    let (text_only, key) = setup(MockPub::text("meta_ads"));
    let err = text_only
        .insights(
            &key,
            insights_query("2026-06-01", "2026-06-02"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadMetrics)
    );
}

/// `with_creds` is the only token-expiry retry. Insights must not grow a
/// private copy that retries twice or skips the vault put.
#[tokio::test]
async fn insights_retries_once_on_shared_token_expired_helper() {
    let mut mock = MockPub::metrics("meta_ads");
    mock.fail_auth_once = true;
    let (c, key) = setup(mock);
    let reply = c
        .insights(
            &key,
            insights_query("2026-06-01", "2026-06-02"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(reply.account_id, "act_1");
}

/// Advertising `read.metrics` on Publisher is not an implementation. A
/// publisher-only registration must fail closed at the facet lookup, which
/// is what keeps the next connector from inheriting a default insights
/// method on the kernel trait.
#[tokio::test]
async fn advertised_capability_without_facet_is_unsupported() {
    struct Lies {
        site: Site,
    }
    #[async_trait]
    impl Publisher for Lies {
        fn site(&self) -> &Site {
            &self.site
        }
        fn capabilities(&self) -> &[Capability] {
            &[Capability::ReadMetrics]
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

    let mut reg = Registry::new();
    let site = Site::new("threads");
    reg.register(Arc::new(Lies { site: site.clone() }));
    let vault = Arc::new(MemoryVault::new());
    let key = AccountKey::new("threads", "default");
    vault
        .put(
            &key,
            &AccountCreds::OAuth2 {
                access_token: "tok".into(),
                refresh_token: None,
                extra: serde_json::json!({}),
            },
        )
        .unwrap();
    let client = Client::new(reg, vault, Arc::new(MemoryAppStore::new()));
    let err = client
        .insights(
            &key,
            insights_query("2026-06-01", "2026-06-02"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadMetrics)
    );
}

/// The trait's default `insights` must refuse — the same honesty the
/// default `probe` keeps: a connector that never implemented reads cannot
/// let one slip through as something else.
#[tokio::test]
async fn default_insights_refuses() {
    let mut reg = Registry::new();
    reg.register(Arc::new(Bare {
        caps: vec![Capability::PublishText],
    }));
    let vault = Arc::new(MemoryVault::new());
    let apps = Arc::new(MemoryAppStore::new());
    let key = AccountKey::new("bluesky", "default");
    vault
        .put(
            &key,
            &AccountCreds::AppPassword {
                identifier: "you".into(),
                secret: "x".into(),
                pds: None,
            },
        )
        .unwrap();
    let c = Client::new(reg, vault, apps);
    let err = c
        .insights(
            &key,
            insights_query("2026-06-01", "2026-06-02"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::UnsupportedCapability { need, .. } if need == Capability::ReadMetrics)
    );
}

/// Range bounds are enforced in the kernel, not just the CLI: a 91-day
/// query is invalid no matter which caller built it.
#[tokio::test]
async fn insights_range_validated_before_any_routing() {
    let (c, key) = setup(MockPub::metrics("meta_ads"));
    let err = c
        .insights(
            &key,
            insights_query("2026-01-01", "2026-04-01"),
            Deadline::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidQuery { reason, .. } if reason == "range_too_long:91"));
}
