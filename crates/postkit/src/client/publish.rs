//! Publish, probe, whoami, and token bootstrap.
use super::{empty_app, Client};
use crate::error::Error;
use crate::publisher::{AuthKind, AuthReply, AuthStart, Publisher};
use crate::types::{AccountCreds, AccountKey, Deadline, Intent, Outcome, Probe, Site, WhoAmI};
use crate::vault::Claim;
use std::sync::Arc;

impl Client {
    pub async fn publish(
        &self,
        key: &AccountKey,
        intent: Intent,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        if key.site != intent.site {
            return Err(Error::InvalidPost {
                site: intent.site,
                reason: "site_mismatch".into(),
                limit: None,
            });
        }
        let publisher = self.publisher(&intent.site)?;
        let need = intent.body.required_capability();
        if !publisher.capabilities().contains(&need) {
            return Err(Error::UnsupportedCapability {
                site: intent.site.clone(),
                need,
            });
        }
        // Client-side idempotency: a retry with the same key returns the
        // first completed Outcome without touching the network. Only
        // *completed* publishes are remembered — an attempt that died after
        // the platform already created the post was never learned, so it
        // cannot dedupe (see docs/cli.md, --idempotency).
        let idem = intent.idempotency_key.clone();
        if let Some(idem) = idem.as_deref() {
            if let Some(out) = self.vault.get_outcome(key, idem)? {
                return Ok(out);
            }
            // Claim the key before publishing (issue 023): without it, two
            // concurrent same-key callers both pass the ledger check above
            // and both publish — a duplicate public post reported as two
            // successes. A Taken answer is a distinct transient error; the
            // caller retries and the ledger then answers.
            match self.vault.claim_outcome(key, idem)? {
                Claim::Free => {}
                Claim::Taken => {
                    return Err(Error::IdempotencyInFlight {
                        site: key.site.clone(),
                        key: idem.to_string(),
                    })
                }
            }
            // Re-check the ledger after winning the claim. The first check
            // and the claim are not one atomic step: a concurrent holder
            // can record its outcome and release between them, and this
            // caller would then claim Free against an already-published
            // key. Because the holder records *before* releasing, any
            // claim we win here happens after that record — one recheck
            // closes the last interleaving.
            if let Some(out) = self.vault.get_outcome(key, idem)? {
                self.release_claim(key, Some(idem));
                return Ok(out);
            }
        }
        // One confined attempt so the claim has exactly one release point:
        // every early `?` inside publish_once lands here, not in the caller.
        let attempt = self.publish_once(publisher, key, intent, deadline).await;
        let out = match attempt {
            Ok(out) => out,
            Err(e) => {
                // A failed attempt must stay retryable — the claim must
                // not outlive it.
                self.release_claim(key, idem.as_deref());
                return Err(e);
            }
        };
        // Record after success only: a failed attempt must stay retryable.
        if let Some(idem) = idem.as_deref() {
            let recorded = self.vault.put_outcome(key, idem, &out);
            self.release_claim(key, Some(idem));
            recorded?;
        }
        Ok(out)
    }

    pub(super) async fn publish_once(
        &self,
        publisher: Arc<dyn Publisher>,
        key: &AccountKey,
        intent: Intent,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        self.with_creds(key, deadline, move |app, creds| {
            let publisher = publisher.clone();
            let intent = intent.clone();
            Box::pin(async move { publisher.publish(&app, &creds, intent, deadline).await })
        })
        .await
    }

    /// Release an idempotency claim on every exit path. Failures are
    /// swallowed deliberately: the file claim's TTL self-heals a stuck
    /// claim, and a release error must not mask the publish's real result.
    pub(super) fn release_claim(&self, key: &AccountKey, idem: Option<&str>) {
        if let Some(idem) = idem {
            let _ = self.vault.release_outcome(key, idem);
        }
    }

    pub async fn whoami(&self, key: &AccountKey) -> Result<WhoAmI, Error> {
        let publisher = self.publisher(&key.site)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        publisher.whoami(&app, &creds).await
    }

    /// Create-only probe (027): same routing, capability check and
    /// credentials as [`publish`](Self::publish), minus everything that
    /// only a real publication is entitled to.
    ///
    /// - No idempotency, read *or* write. The ledger stores completed
    ///   publishes; a probe that recorded its result would make a later
    ///   real publish with the same key "succeed" by replaying the probe,
    ///   and a probe that consulted it could be silenced by an old
    ///   publish. Neither state belongs to the other.
    /// - No proactive `maybe_refresh`. Refresh-on-publish is an
    ///   optimization; here the probe's own response is the instrument —
    ///   it reports the token state exactly as a publish would see it.
    ///   The reactive `token_expired` → refresh → retry mapping is kept,
    ///   because a probe that fails on a refreshable token would report
    ///   "broken" where the next publish would have self-healed.
    pub async fn probe(
        &self,
        key: &AccountKey,
        intent: Intent,
        deadline: Deadline,
    ) -> Result<Probe, Error> {
        if key.site != intent.site {
            return Err(Error::InvalidPost {
                site: intent.site,
                reason: "site_mismatch".into(),
                limit: None,
            });
        }
        let need = intent.body.required_capability();
        self.require_capability(&intent.site, need)?;
        // Probe skips proactive refresh: the probe's own response is the
        // instrument. Reactive token_expired still retries once so a
        // refreshable token is not reported as broken.
        let (publisher, app, creds) = self.prepare_creds(key, deadline, false).await?;
        match publisher
            .probe(&app, &creds, intent.clone(), deadline)
            .await
        {
            Err(e) => {
                let creds = self
                    .recover_expired(&*publisher, &app, key, creds, deadline, e)
                    .await?;
                publisher.probe(&app, &creds, intent, deadline).await
            }
            other => other,
        }
    }

    pub async fn auth_start(&self, site: &Site) -> Result<AuthStart, Error> {
        let publisher = self.publisher(site)?;
        let app = self.apps.get(site).unwrap_or_else(|_| empty_app(site));
        publisher.auth_start(&app).await
    }

    pub async fn auth_finish(&self, key: &AccountKey, reply: AuthReply) -> Result<WhoAmI, Error> {
        let publisher = self.publisher(&key.site)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = publisher.auth_finish(&app, reply).await?;
        self.vault.put(key, &creds)?;
        publisher.whoami(&app, &creds).await
    }

    /// 009 bootstrap. Does not require an app file.
    pub async fn put_token(&self, key: &AccountKey, token: &str) -> Result<WhoAmI, Error> {
        let publisher = self.publisher(&key.site)?;
        // A raw token is valid only for an explicit bearer-token auth kind.
        // App-password and no-auth sites must refuse it at the flag boundary
        // rather than storing a credential their connector will never use.
        if !matches!(
            publisher.auth_kind(),
            AuthKind::OAuth2AuthCode | AuthKind::StaticToken
        ) {
            return Err(Error::Auth {
                site: key.site.clone(),
                reason: "token_bootstrap_unsupported".into(),
            });
        }
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = match publisher.auth_kind() {
            AuthKind::OAuth2AuthCode => AccountCreds::OAuth2 {
                access_token: token.to_string(),
                refresh_token: None,
                extra: serde_json::json!({}),
            },
            AuthKind::StaticToken => AccountCreds::BotToken {
                token: token.to_string(),
            },
            AuthKind::None | AuthKind::AppPassword => unreachable!("auth kind checked above"),
        };
        // Verify the token *before* anything lands in the vault: the old
        // store-then-whoami order persisted an invalid token and only then
        // rejected it. The id that comes back is persisted into `extra`,
        // matching the OAuth path (`creds_from_long`), so both auth shapes
        // publish against /{user_id}/threads instead of this path leaning
        // on the /me alias for its whole lifetime.
        let me = publisher.whoami(&app, &creds).await?;
        let creds = match publisher.auth_kind() {
            AuthKind::OAuth2AuthCode => AccountCreds::OAuth2 {
                access_token: token.to_string(),
                refresh_token: None,
                extra: serde_json::json!({ "user_id": me.id }),
            },
            // A static token's target belongs to application config (for
            // WhatsApp, the selected phone-number ID), not the vault token.
            AuthKind::StaticToken => AccountCreds::BotToken {
                token: token.to_string(),
            },
            AuthKind::None | AuthKind::AppPassword => unreachable!("auth kind checked above"),
        };
        self.vault.put(key, &creds)?;
        Ok(me)
    }
}
