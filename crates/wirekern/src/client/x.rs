//! Client-side guard for X's private direct-message facet.

use super::Client;
use crate::error::Error;
use crate::policy::XDirectMessageAction;
use crate::types::{AccountKey, Capability, Deadline, Outcome, Site};
use crate::vault::Claim;
use crate::x::XDirectMessageRequest;

impl Client {
    /// Send a single text DM only after the caller installs an explicit X-DM
    /// policy. The public-post policy is intentionally irrelevant here: a
    /// private recipient-targeted write has a distinct consent and retention
    /// decision even when it uses the same OAuth identity.
    pub async fn send_x_direct_message(
        &self,
        key: &AccountKey,
        request: XDirectMessageRequest,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        if key.site != Site::new("x") {
            return Err(Error::InvalidPost {
                site: key.site.clone(),
                reason: "x_direct_message_site_mismatch".into(),
                limit: None,
            });
        }
        request.validate().map_err(|reason| Error::InvalidPost {
            site: key.site.clone(),
            reason,
            limit: None,
        })?;
        // Refuse before inspecting registry, app config, or vault. This
        // means a denied CLI invocation cannot reveal whether an account is
        // installed or reach a bearer-token HTTP request.
        self.x_direct_message_policy
            .authorize(&key.site, XDirectMessageAction::SendText)?;
        let need = Capability::SendDirectMessage;
        self.require_capability(&key.site, need)?;
        let sender = self.x_direct_messages(&key.site, need)?;
        // Keep private DM outcomes distinct from public post idempotency.
        // The caller may choose "campaign-1" for both without a prior tweet
        // outcome being returned as if it were a direct-message result.
        let scoped = format!("x-dm-{}", request.idempotency_key);
        if let Some(outcome) = self.vault.get_outcome(key, &scoped)? {
            return Ok(outcome);
        }
        match self.vault.claim_outcome(key, &scoped)? {
            Claim::Free => {}
            Claim::Taken => {
                return Err(Error::IdempotencyInFlight {
                    site: key.site.clone(),
                    key: request.idempotency_key.clone(),
                })
            }
        }
        // The first lookup and claim are deliberately separate operations;
        // re-check after a successful claim closes the race with a peer that
        // wrote its outcome immediately before releasing its own claim.
        match self.vault.get_outcome(key, &scoped) {
            Ok(Some(outcome)) => {
                self.release_claim(key, Some(&scoped));
                return Ok(outcome);
            }
            Ok(None) => {}
            Err(error) => {
                self.release_claim(key, Some(&scoped));
                return Err(error);
            }
        }
        let attempt = self
            .with_creds(key, deadline, move |app, creds| {
                let sender = sender.clone();
                let request = request.clone();
                Box::pin(async move {
                    sender
                        .send_x_direct_message(&app, &creds, &request, deadline)
                        .await
                })
            })
            .await;
        let outcome = match attempt {
            Ok(outcome) => outcome,
            Err(error) => {
                self.release_claim(key, Some(&scoped));
                return Err(error);
            }
        };
        let recorded = self.vault.put_outcome(key, &scoped, &outcome);
        self.release_claim(key, Some(&scoped));
        recorded?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::MemoryAppStore;
    use crate::policy::AllowXDirectMessagesPolicy;
    use crate::publisher::{AuthKind, AuthReply, Publisher};
    use crate::registry::Registry;
    use crate::types::{AccountCreds, AppConfig, Intent, WhoAmI};
    use crate::vault::MemoryVault;
    use async_trait::async_trait;
    use std::sync::Arc;

    struct DeniedPublisher;

    #[async_trait]
    impl Publisher for DeniedPublisher {
        fn site(&self) -> &Site {
            static SITE: std::sync::OnceLock<Site> = std::sync::OnceLock::new();
            SITE.get_or_init(|| Site::new("x"))
        }
        fn capabilities(&self) -> &[Capability] {
            &[Capability::SendDirectMessage]
        }
        fn auth_kind(&self) -> AuthKind {
            AuthKind::OAuth2AuthCode
        }
        async fn publish(
            &self,
            _: &AppConfig,
            _: &AccountCreds,
            _: Intent,
            _: Deadline,
        ) -> Result<Outcome, Error> {
            unreachable!()
        }
        async fn whoami(&self, _: &AppConfig, _: &AccountCreds) -> Result<WhoAmI, Error> {
            unreachable!()
        }
        async fn auth_finish(&self, _: &AppConfig, _: AuthReply) -> Result<AccountCreds, Error> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn default_policy_refuses_before_missing_account_lookup() {
        let mut registry = Registry::new();
        registry.register(Arc::new(DeniedPublisher));
        let client = Client::new(
            registry,
            Arc::new(MemoryVault::new()),
            Arc::new(MemoryAppStore::new()),
        );
        let err = client
            .send_x_direct_message(
                &AccountKey::new("x", "missing"),
                XDirectMessageRequest {
                    recipient_id: "1".into(),
                    text: "hello".into(),
                    idempotency_key: "one".into(),
                },
                Deadline::from_secs(1),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::PolicyDenied { reason, .. } if reason == "explicit_x_dm_send_required")
        );
    }

    #[test]
    fn allow_policy_is_an_explicit_type() {
        let policy = AllowXDirectMessagesPolicy;
        assert!(crate::policy::XDirectMessagePolicy::authorize(
            &policy,
            &Site::new("x"),
            XDirectMessageAction::SendText
        )
        .is_ok());
    }
}
