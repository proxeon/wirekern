//! WhatsApp Cloud Client verbs: send, webhook, ledger, assets, templates, account.
use super::{empty_app, Client, WhatsAppPacingKey};
use crate::error::Error;
use crate::policy::WhatsAppAction;
use crate::types::{AccountKey, AppConfig, Capability, Deadline, Outcome, Site};
use crate::vault::Claim;
use crate::whatsapp::{WhatsAppMessage, WhatsAppSendRequest};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

impl Client {
    /// Send one private WhatsApp Cloud message through the separate messaging
    /// contract. The normal policy denies it before vault access; callers
    /// must explicitly install an allowing `WhatsAppPolicy`. A `wamid` means
    /// Meta accepted the request, not that the recipient received it — status
    /// webhooks provide the final delivery state.
    pub async fn send_whatsapp(
        &self,
        key: &AccountKey,
        request: WhatsAppSendRequest,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        self.send_whatsapp_from(key, None, request, deadline).await
    }

    /// Send through the primary configured phone or an explicitly configured
    /// sender alias. The alias is resolved locally before Graph is called;
    /// callers cannot use this method to target an arbitrary phone ID.
    ///
    /// Idempotency and pacing are namespaced by the resolved phone ID. Reusing
    /// an idempotency key on two different senders therefore cannot replay a
    /// delivery result from the wrong business number.
    #[cfg(feature = "whatsapp-cloud")]
    pub async fn send_whatsapp_from(
        &self,
        key: &AccountKey,
        sender_alias: Option<&str>,
        request: WhatsAppSendRequest,
        deadline: Deadline,
    ) -> Result<Outcome, Error> {
        request.validate().map_err(|reason| Error::InvalidPost {
            site: key.site.clone(),
            reason,
            limit: None,
        })?;
        let action = match &request.message {
            WhatsAppMessage::Reply { .. } => WhatsAppAction::SendReply,
            WhatsAppMessage::Text { .. } => WhatsAppAction::SendText,
            WhatsAppMessage::Template { .. } => WhatsAppAction::SendTemplate,
            WhatsAppMessage::Image { .. }
            | WhatsAppMessage::Document { .. }
            | WhatsAppMessage::Audio { .. }
            | WhatsAppMessage::Video { .. }
            | WhatsAppMessage::Sticker { .. } => WhatsAppAction::SendMedia,
            WhatsAppMessage::Buttons { .. }
            | WhatsAppMessage::List { .. }
            | WhatsAppMessage::CtaUrl { .. }
            | WhatsAppMessage::LocationRequest { .. }
            | WhatsAppMessage::VoiceCall { .. }
            | WhatsAppMessage::AddressRequest { .. } => WhatsAppAction::SendInteractive,
            WhatsAppMessage::Location { .. } => WhatsAppAction::SendLocation,
            WhatsAppMessage::Contacts { .. } => WhatsAppAction::SendContacts,
            WhatsAppMessage::Reaction { .. } => WhatsAppAction::SendReaction,
            WhatsAppMessage::MarkRead { .. } => WhatsAppAction::MarkRead,
            WhatsAppMessage::Typing { .. } => WhatsAppAction::SendTyping,
            WhatsAppMessage::Catalog { .. }
            | WhatsAppMessage::Product { .. }
            | WhatsAppMessage::ProductList { .. }
            | WhatsAppMessage::OrderStatus { .. } => WhatsAppAction::SendCatalog,
            WhatsAppMessage::Flow { .. } => WhatsAppAction::SendFlow,
        };
        // Do this before registry/vault lookup. A denied send must reveal
        // neither whether an account is configured nor a bearer token to the
        // connector's HTTP path.
        self.whatsapp_policy
            .authorize_request(&key.site, action, &request)?;
        let publisher = self.publisher(&key.site)?;
        let need = request.required_capability();
        if !publisher.capabilities().contains(&need) {
            return Err(Error::UnsupportedCapability {
                site: key.site.clone(),
                need,
            });
        }

        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let (sender_app, phone_number_id) =
            resolve_whatsapp_outbound_sender(&app, sender_alias, &key.site)?;

        // WhatsApp sends require a key, so this follows the same atomic
        // claim/record discipline as public publishing. Only a confirmed
        // response is remembered; an unknown post-send failure remains
        // intentionally ambiguous and must be reconciled via webhook/status.
        let idem = scoped_whatsapp_idempotency(&phone_number_id, &request.idempotency_key);
        if let Some(out) = self.vault.get_outcome(key, &idem)? {
            return Ok(out);
        }
        match self.vault.claim_outcome(key, &idem)? {
            Claim::Free => {}
            Claim::Taken => {
                return Err(Error::IdempotencyInFlight {
                    site: key.site.clone(),
                    key: request.idempotency_key.clone(),
                })
            }
        }
        let after_claim = match self.vault.get_outcome(key, &idem) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.release_claim(key, Some(&idem));
                return Err(error);
            }
        };
        if let Some(out) = after_claim {
            self.release_claim(key, Some(&idem));
            return Ok(out);
        }
        // Keep credential lookup inside the same guarded attempt as the HTTP
        // call. A missing/corrupt vault entry must release the claim just like
        // a rejected platform request, otherwise a later corrected command
        // would be blocked behind an abandoned idempotency key.
        let sender = self.whatsapp_sender(&key.site, need)?;
        let attempt = async {
            // Every Cloud API message, including a one-off send, shares this
            // sender's local pacing queue. A batch is only a loop over this
            // operation, so its second item waits instead of being rejected
            // with a synthetic local rate-limit error.
            self.wait_for_whatsapp_slot(key, &phone_number_id, deadline)
                .await?;
            let creds = self.vault.get(key)?;
            sender
                .send_whatsapp(&sender_app, &creds, &request, deadline)
                .await
        }
        .await;
        let out = match attempt {
            Ok(out) => out,
            Err(error) => {
                self.release_claim(key, Some(&idem));
                return Err(error);
            }
        };
        let recorded = self.vault.put_outcome(key, &idem, &out);
        self.release_claim(key, Some(&idem));
        recorded?;
        Ok(out)
    }

    /// Verify and parse a forwarded Cloud API webhook. This is not a listener:
    /// the caller supplies the exact raw body and `X-Hub-Signature-256`.
    #[cfg(feature = "whatsapp-cloud")]
    pub fn parse_whatsapp_webhook(
        &self,
        signature: &str,
        raw_body: &[u8],
        options: crate::whatsapp::WebhookParseOptions,
    ) -> Result<crate::whatsapp::InboundMessages, Error> {
        let app = self.apps.get(&Site::new("whatsapp_cloud"))?;
        crate::connectors::whatsapp_cloud::WhatsAppCloud::parse_signed_webhook_with(
            &app, signature, raw_body, options,
        )
    }

    /// Meta GET handshake. Echoes `hub.challenge` when the verify token matches.
    #[cfg(feature = "whatsapp-cloud")]
    pub fn verify_whatsapp_callback_challenge(
        &self,
        mode: &str,
        token: &str,
        challenge: &str,
    ) -> Result<String, Error> {
        let app = self.apps.get(&Site::new("whatsapp_cloud"))?;
        crate::connectors::whatsapp_cloud::WhatsAppCloud::verify_callback_challenge(
            &app, mode, token, challenge,
        )
    }

    /// Parse a signed webhook and, if a ledger is attached, persist wamids.
    /// HMAC failures stay errors so Meta can retry; parse failures after a
    /// valid signature should be ACK'd by the host and recorded as dead letters.
    #[cfg(feature = "whatsapp-cloud")]
    pub fn ingest_whatsapp_webhook(
        &self,
        signature: &str,
        raw_body: &[u8],
        options: crate::whatsapp::WebhookParseOptions,
    ) -> Result<crate::whatsapp::InboundMessages, Error> {
        match self.parse_whatsapp_webhook(signature, raw_body, options) {
            Ok(parsed) => {
                if let Some(ledger) = &self.whatsapp_ledger {
                    crate::whatsapp_ops::ingest_parsed(ledger.as_ref(), &parsed)?;
                }
                Ok(parsed)
            }
            Err(error) => {
                // Unsigned/oversized junk must not fill the dead-letter log.
                // The parser bounds size before it can verify an HMAC, so all
                // three early failures are intentionally non-auditable.
                let unauthenticated_or_oversized = matches!(
                    &error,
                    Error::InvalidQuery { reason, .. }
                        if reason == "webhook_signature_invalid"
                            || reason == "missing_webhook_app_secret"
                            || reason == "webhook_body_too_large"
                );
                if !unauthenticated_or_oversized {
                    let reason = match &error {
                        Error::InvalidQuery { reason, .. } => reason.as_str(),
                        _ => "webhook_ingest_failed",
                    };
                    if let Some(ledger) = &self.whatsapp_ledger {
                        let sha = crate::whatsapp_ops::sha256_hex(raw_body);
                        let _ = ledger.put_dead_letter(reason, &sha);
                    }
                    // Replay is a deliberate privacy opt-in. The original
                    // parse error remains authoritative even if local capture
                    // fails; a webhook host can still ACK a valid signature
                    // and avoid Meta retry storms.
                    if raw_body.len() <= crate::connectors::whatsapp_cloud::MAX_WEBHOOK_BYTES {
                        if let Some(dead_letters) = &self.whatsapp_replay_dead_letters {
                            let _ = dead_letters.capture(reason, signature, raw_body);
                        }
                    }
                }
                Err(error)
            }
        }
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn whatsapp_ledger_get(
        &self,
        wamid: &str,
    ) -> Result<Option<crate::whatsapp_ops::WhatsAppLedgerRecord>, Error> {
        match &self.whatsapp_ledger {
            Some(ledger) => ledger.get(wamid),
            None => Err(Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "whatsapp_ledger_disabled".into(),
            }),
        }
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn whatsapp_window_open(&self, wa_id: &str) -> Result<bool, Error> {
        let Some(ledger) = &self.whatsapp_ledger else {
            return Err(Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "whatsapp_ledger_disabled".into(),
            });
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let wa_id =
            crate::whatsapp::normalize_recipient(wa_id).map_err(|reason| Error::InvalidPost {
                site: Site::new("whatsapp_cloud"),
                reason,
                limit: None,
            })?;
        let wa_id = wa_id.trim_start_matches('+');
        Ok(ledger
            .last_inbound_at(wa_id)?
            .is_some_and(|at| crate::whatsapp_ops::customer_window_open(at, now)))
    }

    /// List only metadata for encrypted, replayable signed callback failures.
    /// Raw callback bodies never leave the local store through this API.
    #[cfg(feature = "whatsapp-cloud")]
    pub fn list_whatsapp_replay_dead_letters(
        &self,
    ) -> Result<Vec<crate::whatsapp_ops::WhatsAppDeadLetterSummary>, Error> {
        match &self.whatsapp_replay_dead_letters {
            Some(store) => store.list(),
            None => Err(Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "whatsapp_replay_dlq_disabled".into(),
            }),
        }
    }

    /// Re-run one encrypted raw callback through the current HMAC and typed
    /// parser. Successful reduction deletes the ciphertext; a still-unknown
    /// event remains queued so an operator can retry only after upgrading.
    #[cfg(feature = "whatsapp-cloud")]
    pub fn replay_whatsapp_dead_letter(
        &self,
        id: &str,
        options: crate::whatsapp::WebhookParseOptions,
    ) -> Result<crate::whatsapp::InboundMessages, Error> {
        let store =
            self.whatsapp_replay_dead_letters
                .as_ref()
                .ok_or_else(|| Error::InvalidQuery {
                    site: Site::new("whatsapp_cloud"),
                    reason: "whatsapp_replay_dlq_disabled".into(),
                })?;
        let event = store.load(id)?.ok_or_else(|| Error::InvalidQuery {
            site: Site::new("whatsapp_cloud"),
            reason: "whatsapp_replay_dlq_not_found".into(),
        })?;
        let parsed = self.parse_whatsapp_webhook(&event.signature, &event.raw_body, options)?;
        if let Some(ledger) = &self.whatsapp_ledger {
            crate::whatsapp_ops::ingest_parsed(ledger.as_ref(), &parsed)?;
        }
        store.delete(&event.summary.id)?;
        Ok(parsed)
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn put_whatsapp_consent(
        &self,
        mut record: crate::whatsapp_ops::ConsentRecord,
    ) -> Result<(), Error> {
        // Consent keys use the same canonical digits as webhook `from` and
        // Cloud API `to`. Without this, an operator recording `+60 11…`
        // could not protect a later send addressed as `6011…`.
        record.wa_id = crate::whatsapp::normalize_recipient(&record.wa_id)
            .map_err(|reason| Error::InvalidPost {
                site: Site::new("whatsapp_cloud"),
                reason,
                limit: None,
            })?
            .trim_start_matches('+')
            .to_string();
        match &self.whatsapp_consent {
            Some(store) => store.put(&record),
            None => Err(Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "whatsapp_consent_disabled".into(),
            }),
        }
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub fn get_whatsapp_consent(
        &self,
        wa_id: &str,
    ) -> Result<Option<crate::whatsapp_ops::ConsentRecord>, Error> {
        let wa_id =
            crate::whatsapp::normalize_recipient(wa_id).map_err(|reason| Error::InvalidPost {
                site: Site::new("whatsapp_cloud"),
                reason,
                limit: None,
            })?;
        let wa_id = wa_id.trim_start_matches('+');
        match &self.whatsapp_consent {
            Some(store) => store.get(wa_id),
            None => Err(Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "whatsapp_consent_disabled".into(),
            }),
        }
    }

    /// Remove local delivery records older than `before_unix`.
    ///
    /// This deliberately affects the delivery ledger only. Consent records
    /// can have a separate legal-retention basis, so expiring message history
    /// must never silently erase an explicit opt-out.
    #[cfg(feature = "whatsapp-cloud")]
    pub fn purge_whatsapp_ledger_before(&self, before_unix: u64) -> Result<usize, Error> {
        match &self.whatsapp_ledger {
            Some(ledger) => {
                let removed = ledger.purge_before(before_unix)?;
                if let Some(dead_letters) = &self.whatsapp_replay_dead_letters {
                    let _ = dead_letters.purge_before(before_unix)?;
                }
                Ok(removed)
            }
            None => Err(Error::InvalidQuery {
                site: Site::new("whatsapp_cloud"),
                reason: "whatsapp_ledger_disabled".into(),
            }),
        }
    }

    /// Upload bytes to Cloud API media. Not a customer send: no `--allow-send`.
    #[cfg(feature = "whatsapp-cloud")]
    pub async fn upload_whatsapp_media(
        &self,
        key: &AccountKey,
        upload: crate::whatsapp::WhatsAppMediaUpload,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppUploadedMedia, Error> {
        self.require_capability(&key.site, Capability::ManageWhatsAppMedia)?;
        let assets = self.whatsapp_assets(&key.site, Capability::ManageWhatsAppMedia)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        assets.upload_media(&app, &creds, &upload, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn whatsapp_media_metadata(
        &self,
        key: &AccountKey,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppMediaMeta, Error> {
        self.require_capability(&key.site, Capability::ReadWhatsAppMedia)?;
        let assets = self.whatsapp_assets(&key.site, Capability::ReadWhatsAppMedia)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        assets
            .media_metadata(&app, &creds, media_id, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn download_whatsapp_media(
        &self,
        key: &AccountKey,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<Vec<u8>, Error> {
        self.require_capability(&key.site, Capability::ReadWhatsAppMedia)?;
        let assets = self.whatsapp_assets(&key.site, Capability::ReadWhatsAppMedia)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        assets
            .download_media(&app, &creds, media_id, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn delete_whatsapp_media(
        &self,
        key: &AccountKey,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        self.require_capability(&key.site, Capability::ManageWhatsAppMedia)?;
        let assets = self.whatsapp_assets(&key.site, Capability::ManageWhatsAppMedia)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        assets.delete_media(&app, &creds, media_id, deadline).await
    }

    /// List WABA templates (id/name/status/quality). Not a customer send.
    #[cfg(feature = "whatsapp-cloud")]
    pub async fn list_whatsapp_templates(
        &self,
        key: &AccountKey,
        query: crate::whatsapp::WhatsAppTemplateQuery,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppTemplateList, Error> {
        self.require_capability(&key.site, Capability::ReadTemplates)?;
        let templates = self.whatsapp_templates(&key.site, Capability::ReadTemplates)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        templates
            .list_templates(&app, &creds, &query, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn get_whatsapp_template(
        &self,
        key: &AccountKey,
        template_id: &str,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppTemplateRecord, Error> {
        self.require_capability(&key.site, Capability::ReadTemplates)?;
        let templates = self.whatsapp_templates(&key.site, Capability::ReadTemplates)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        templates
            .get_template(&app, &creds, template_id, deadline)
            .await
    }

    /// Create (and auto-submit for review) a typed template. Not a customer
    /// send: no `--allow-send`, but still requires `manage.templates`.
    #[cfg(feature = "whatsapp-cloud")]
    pub async fn create_whatsapp_template(
        &self,
        key: &AccountKey,
        draft: crate::whatsapp::WhatsAppTemplateDraft,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppTemplateRecord, Error> {
        self.require_capability(&key.site, Capability::ManageTemplates)?;
        let templates = self.whatsapp_templates(&key.site, Capability::ManageTemplates)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        templates
            .create_template(&app, &creds, &draft, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn edit_whatsapp_template(
        &self,
        key: &AccountKey,
        template_id: &str,
        draft: crate::whatsapp::WhatsAppTemplateDraft,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppTemplateRecord, Error> {
        self.require_capability(&key.site, Capability::ManageTemplates)?;
        let templates = self.whatsapp_templates(&key.site, Capability::ManageTemplates)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        templates
            .edit_template(&app, &creds, template_id, &draft, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn delete_whatsapp_template(
        &self,
        key: &AccountKey,
        name: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        self.require_capability(&key.site, Capability::ManageTemplates)?;
        let templates = self.whatsapp_templates(&key.site, Capability::ManageTemplates)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        templates
            .delete_template(&app, &creds, name, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn list_whatsapp_flows(
        &self,
        key: &AccountKey,
        query: crate::whatsapp::WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppFlowList, Error> {
        self.require_capability(&key.site, Capability::ReadFlows)?;
        let flows = self.whatsapp_flows(&key.site, Capability::ReadFlows)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        flows.list_flows(&app, &creds, &query, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn get_whatsapp_flow(
        &self,
        key: &AccountKey,
        flow_id: &str,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppFlowRecord, Error> {
        self.require_capability(&key.site, Capability::ReadFlows)?;
        let flows = self.whatsapp_flows(&key.site, Capability::ReadFlows)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        flows.get_flow(&app, &creds, flow_id, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn create_whatsapp_flow(
        &self,
        key: &AccountKey,
        draft: crate::whatsapp::WhatsAppFlowDraft,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppFlowRecord, Error> {
        self.require_capability(&key.site, Capability::ManageFlows)?;
        let flows = self.whatsapp_flows(&key.site, Capability::ManageFlows)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        flows.create_flow(&app, &creds, &draft, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn publish_whatsapp_flow(
        &self,
        key: &AccountKey,
        flow_id: &str,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppFlowRecord, Error> {
        self.require_capability(&key.site, Capability::ManageFlows)?;
        let flows = self.whatsapp_flows(&key.site, Capability::ManageFlows)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        flows.publish_flow(&app, &creds, flow_id, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn list_whatsapp_wabas(
        &self,
        key: &AccountKey,
        query: crate::whatsapp::WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppWabaList, Error> {
        self.require_capability(&key.site, Capability::ReadWhatsAppAccount)?;
        let account = self.whatsapp_account(&key.site, Capability::ReadWhatsAppAccount)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account.list_wabas(&app, &creds, &query, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn list_whatsapp_phone_numbers(
        &self,
        key: &AccountKey,
        query: crate::whatsapp::WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppPhoneNumberList, Error> {
        self.require_capability(&key.site, Capability::ReadWhatsAppAccount)?;
        let account = self.whatsapp_account(&key.site, Capability::ReadWhatsAppAccount)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account
            .list_phone_numbers(&app, &creds, &query, deadline)
            .await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn whatsapp_phone_health(
        &self,
        key: &AccountKey,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppPhoneNumber, Error> {
        self.require_capability(&key.site, Capability::ReadWhatsAppAccount)?;
        let account = self.whatsapp_account(&key.site, Capability::ReadWhatsAppAccount)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account.phone_health(&app, &creds, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn subscribe_whatsapp_apps(
        &self,
        key: &AccountKey,
        deadline: Deadline,
    ) -> Result<(), Error> {
        self.require_capability(&key.site, Capability::ManageWhatsAppPhone)?;
        let account = self.whatsapp_account(&key.site, Capability::ManageWhatsAppPhone)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account.subscribe_apps(&app, &creds, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn register_whatsapp_phone(
        &self,
        key: &AccountKey,
        pin: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        self.whatsapp_policy
            .authorize(&key.site, WhatsAppAction::ManagePhone)?;
        self.require_capability(&key.site, Capability::ManageWhatsAppPhone)?;
        let account = self.whatsapp_account(&key.site, Capability::ManageWhatsAppPhone)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account.register_phone(&app, &creds, pin, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn set_whatsapp_two_step_pin(
        &self,
        key: &AccountKey,
        pin: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        self.whatsapp_policy
            .authorize(&key.site, WhatsAppAction::ManagePhone)?;
        self.require_capability(&key.site, Capability::ManageWhatsAppPhone)?;
        let account = self.whatsapp_account(&key.site, Capability::ManageWhatsAppPhone)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account.set_two_step_pin(&app, &creds, pin, deadline).await
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub async fn list_whatsapp_system_users(
        &self,
        key: &AccountKey,
        query: crate::whatsapp::WhatsAppPageQuery,
        deadline: Deadline,
    ) -> Result<crate::whatsapp::WhatsAppSystemUserList, Error> {
        self.require_capability(&key.site, Capability::ReadWhatsAppAccount)?;
        let account = self.whatsapp_account(&key.site, Capability::ReadWhatsAppAccount)?;
        let app = self
            .apps
            .get(&key.site)
            .unwrap_or_else(|_| empty_app(&key.site));
        let creds = self.vault.get(key)?;
        account
            .list_system_users(&app, &creds, &query, deadline)
            .await
    }

    /// Bounded fan-out, not a campaign tool. More than 10 messages is refused.
    /// Every item uses the shared, deadline-aware pacing path from
    /// [`Self::send_whatsapp`], never a batch-local rate-limit shortcut.
    #[cfg(feature = "whatsapp-cloud")]
    pub async fn send_whatsapp_many(
        &self,
        key: &AccountKey,
        requests: Vec<WhatsAppSendRequest>,
        deadline: Deadline,
    ) -> Result<Vec<Outcome>, Error> {
        self.send_whatsapp_many_from(key, None, requests, deadline)
            .await
    }

    /// Bounded multi-send through one configured sender alias. Keep one alias
    /// for the whole batch so its pacing and idempotency scope are obvious to
    /// the operator; mixed-sender fan-out needs a separate reviewed contract.
    #[cfg(feature = "whatsapp-cloud")]
    pub async fn send_whatsapp_many_from(
        &self,
        key: &AccountKey,
        sender_alias: Option<&str>,
        requests: Vec<WhatsAppSendRequest>,
        deadline: Deadline,
    ) -> Result<Vec<Outcome>, Error> {
        if requests.len() > 10 {
            return Err(Error::InvalidPost {
                site: key.site.clone(),
                reason: "whatsapp_batch_too_large".into(),
                limit: Some(10),
            });
        }
        let mut out = Vec::new();
        for request in requests {
            out.push(
                self.send_whatsapp_from(key, sender_alias, request, deadline)
                    .await?,
            );
        }
        Ok(out)
    }

    #[cfg(feature = "whatsapp-cloud")]
    pub(super) fn whatsapp_throughput_for(
        &self,
        key: &AccountKey,
        phone_number_id: &str,
    ) -> Arc<crate::whatsapp_ops::ThroughputQueue> {
        let mut queues = self
            .whatsapp_throughput
            .lock()
            .expect("whatsapp throughput");
        queues
            .entry(WhatsAppPacingKey {
                account: key.clone(),
                phone_number_id: phone_number_id.to_string(),
            })
            .or_insert_with(|| Arc::new(crate::whatsapp_ops::ThroughputQueue::default_cloud_api()))
            .clone()
    }

    /// Wait for a process-local slot without exceeding the caller's existing
    /// deadline. The queue returns a duration instead of sleeping itself so
    /// this async client never blocks a Tokio worker.
    #[cfg(feature = "whatsapp-cloud")]
    pub(super) async fn wait_for_whatsapp_slot(
        &self,
        key: &AccountKey,
        phone_number_id: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        let queue = self.whatsapp_throughput_for(key, phone_number_id);
        loop {
            deadline.check(&key.site)?;
            let now_ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let wait = std::time::Duration::from_nanos(queue.wait_ns(now_ns));
            if wait.is_zero() {
                return Ok(());
            }
            // Do not begin a sleep that cannot finish before the request
            // deadline. A caller gets the normal timeout, not a misleading
            // local RateLimited result caused by another batch item.
            if wait >= deadline.remaining() {
                return Err(Error::DeadlineExceeded {
                    site: key.site.clone(),
                });
            }
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(feature = "whatsapp-cloud")]
pub(super) fn resolve_whatsapp_outbound_sender(
    app: &AppConfig,
    sender_alias: Option<&str>,
    site: &Site,
) -> Result<(AppConfig, String), Error> {
    let primary = app
        .extra
        .get("phone_number_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| {
            !id.is_empty() && id.len() <= 32 && id.bytes().all(|byte| byte.is_ascii_digit())
        })
        .map(str::to_owned);

    let selected = match sender_alias {
        // Let the connector retain the established `missing_phone_number_id`
        // failure for an unconfigured primary sender. This also keeps Client
        // generic enough for test/embedding connectors that do not model the
        // WhatsApp app extension at all.
        None => primary.unwrap_or_else(|| "primary".into()),
        Some(alias) => {
            if !crate::types::valid_name(alias) || alias == "primary" {
                return Err(Error::InvalidQuery {
                    site: site.clone(),
                    reason: "whatsapp_sender_alias_invalid".into(),
                });
            }
            let configured = app
                .extra
                .get("senders")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| Error::InvalidQuery {
                    site: site.clone(),
                    reason: "whatsapp_sender_unknown".into(),
                })?;
            let mut found = None;
            for value in configured {
                let sender: crate::whatsapp::WhatsAppOutboundSender =
                    serde_json::from_value(value.clone()).map_err(|_| Error::InvalidQuery {
                        site: site.clone(),
                        reason: "whatsapp_sender_config_invalid".into(),
                    })?;
                sender.validate().map_err(|reason| Error::InvalidQuery {
                    site: site.clone(),
                    reason,
                })?;
                if sender.alias == alias && found.replace(sender.phone_number_id).is_some() {
                    return Err(Error::InvalidQuery {
                        site: site.clone(),
                        reason: "whatsapp_sender_alias_duplicate".into(),
                    });
                }
            }
            found.ok_or_else(|| Error::InvalidQuery {
                site: site.clone(),
                reason: "whatsapp_sender_unknown".into(),
            })?
        }
    };

    // Connector wire methods still read `phone_number_id` from AppConfig.
    // Clone only the in-memory config so selecting a sender never rewrites a
    // user's primary sender or leaks into webhook configuration on disk.
    let mut selected_app = app.clone();
    if selected != "primary" {
        selected_app.extra["phone_number_id"] = serde_json::Value::String(selected.clone());
    }
    Ok((selected_app, selected))
}

#[cfg(feature = "whatsapp-cloud")]
pub(super) fn scoped_whatsapp_idempotency(phone_number_id: &str, idempotency_key: &str) -> String {
    // Both segments are locally validated to `[A-Za-z0-9._-]+`/digits. This
    // becomes a vault-private namespace, not a Meta idempotency header.
    format!("wa-sender-{phone_number_id}-{idempotency_key}")
}
