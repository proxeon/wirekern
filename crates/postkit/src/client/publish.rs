//! Publish, probe, whoami, and token bootstrap.
use super::{empty_app, Client};
use crate::error::Error;
use crate::publisher::{AuthKind, AuthReply, AuthStart, AuthStartOptions, Publisher};
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

    /// Start an ordinary connector authorization. PKCE connectors need an
    /// account alias so their one-time verifier can be stored safely; callers
    /// should use [`Self::auth_start_for`] for those sites.
    pub async fn auth_start(&self, site: &Site) -> Result<AuthStart, Error> {
        let publisher = self.publisher(site)?;
        let app = self.apps.get(site).unwrap_or_else(|_| empty_app(site));
        let mut start = publisher
            .auth_start_with(&app, &AuthStartOptions::default())
            .await?;
        if matches!(
            &start,
            AuthStart::Browser {
                pending_pkce: Some(_),
                ..
            }
        ) {
            // Never return an OAuth verifier from the public Client API. The
            // account-aware method below persists it owner-only instead.
            return Err(Error::Auth {
                site: site.clone(),
                reason: "auth_start_requires_account".into(),
            });
        }
        if let AuthStart::Browser { pending_pkce, .. } = &mut start {
            *pending_pkce = None;
        }
        Ok(start)
    }

    /// Start an authorization flow for one vault alias. This is the only
    /// public start path used by the CLI because PKCE's verifier must survive
    /// a browser handoff without being printed or passed in a command line.
    pub async fn auth_start_for(
        &self,
        key: &AccountKey,
        options: AuthStartOptions,
    ) -> Result<AuthStart, Error> {
        let publisher = self.publisher(&key.site)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let mut start = publisher.auth_start_with(&app, &options).await?;
        if let AuthStart::Browser { pending_pkce, .. } = &mut start {
            if let Some(session) = pending_pkce.take() {
                self.vault.put_auth_session(key, &session)?;
            }
        }
        Ok(start)
    }

    pub async fn auth_finish(&self, key: &AccountKey, reply: AuthReply) -> Result<WhoAmI, Error> {
        let publisher = self.publisher(&key.site)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let reply = self.attach_pkce_session(key, reply)?;
        let creds = publisher.auth_finish(&app, reply).await?;
        self.vault.put(key, &creds)?;
        publisher.whoami(&app, &creds).await
    }

    /// Convert a pasted full redirect into the PKCE reply only when it
    /// matches a one-time state kept in the vault. Raw codes remain supported
    /// for non-PKCE connectors, but X intentionally refuses them because a
    /// PKCE exchange without its verifier cannot be completed securely.
    fn attach_pkce_session(&self, key: &AccountKey, reply: AuthReply) -> Result<AuthReply, Error> {
        let AuthReply::Redirect { url } = &reply else {
            return Ok(reply);
        };
        let Some(state) = redirect_query_value(url, "state") else {
            return Ok(reply);
        };
        let Some(session) = self.vault.take_auth_session(key, &state)? else {
            return Ok(reply);
        };
        Ok(AuthReply::Pkce {
            code: url.clone(),
            verifier: session.code_verifier,
        })
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

/// `state` is generated as lowercase hexadecimal and therefore never needs
/// percent-decoding. This small parser exists in the always-compiled Client
/// module, whereas the shared OAuth URL parser is feature-gated.
fn redirect_query_value(url: &str, key: &str) -> Option<String> {
    let query = url.split('#').next()?.split_once('?')?.1;
    query.split('&').find_map(|item| {
        let (name, value) = item.split_once('=')?;
        (name == key).then(|| value.to_string())
    })
}
