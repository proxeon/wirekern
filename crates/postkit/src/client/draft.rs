//! Draft orchestration (plans/001/013). Composes Tier B methods; no new remote verb.
use super::Client;
use crate::ads::{
    AdEntity, AdReviewStatusRequest, CreatePausedAdRequest, PausedAd, PausedAdCreate, PausedAdset,
    PausedCampaign, UploadAdImageRequest,
};
use crate::draft::{
    adoption_entity, manifest_fingerprint, DraftImage, DraftStage, DraftStatusReply, DraftStep,
    DraftStore, PausedDraftManifest, PausedDraftResult, PausedDraftState, CONFIGURED_PAUSED,
    RECONCILE_GUIDANCE,
};
use crate::error::Error;
use crate::policy::AdsAction;
use crate::types::{AccountKey, Deadline, Site};
use std::path::Path;

impl Client {
    /// Zero-I/O validation: the answer to `validate-draft`. Checks the
    /// manifest's closed contracts (pairing table, budget floor, enums,
    /// HTTPS destination, image basename) and returns the account and
    /// fingerprint a run would use. Never reads the vault, the image
    /// file, or any state.
    pub fn validate_paused_draft(
        &self,
        site: &Site,
        manifest: &PausedDraftManifest,
    ) -> Result<(String, String), Error> {
        let account = manifest
            .normalized_account()
            .map_err(|reason| Error::InvalidQuery {
                site: site.clone(),
                reason,
            })?;
        manifest.validate().map_err(|reason| Error::InvalidQuery {
            site: site.clone(),
            reason,
        })?;
        Ok((account, manifest_fingerprint(manifest)))
    }

    /// Execute or resume the manifest's paused hierarchy, checkpointing
    /// after every confirmed remote write. `resume` selects the
    /// existing-state contract (`resume-draft`); without it the state
    /// path must be new (`create-draft`).
    ///
    /// Return contract: `Ok(ReconciliationRequired)` is a *successful*
    /// protocol outcome — an ambiguous write leaves the `in_flight`
    /// marker in place and demands human reconciliation. A `Err` means
    /// the run definitively failed (Meta answered, or nothing left the
    /// machine) and is safe to re-run from the last checkpoint.
    pub async fn run_paused_draft(
        &self,
        run: crate::draft::RunPausedDraft<'_>,
    ) -> Result<PausedDraftResult, Error> {
        let crate::draft::RunPausedDraft {
            key,
            manifest,
            image,
            store,
            state_path,
            resume,
            deadline,
        } = run;
        let site = key.site.clone();
        manifest.validate().map_err(|reason| Error::InvalidQuery {
            site: site.clone(),
            reason,
        })?;
        let fingerprint = manifest_fingerprint(manifest);
        let account = manifest
            .normalized_account()
            .map_err(|reason| Error::InvalidQuery {
                site: site.clone(),
                reason,
            })?;
        // Exclusive lock first: two concurrent runs would both issue
        // the next write, which is exactly the duplicate the protocol
        // exists to prevent.
        let _lock = store
            .try_lock(state_path)
            .map_err(|reason| Error::InvalidQuery {
                site: site.clone(),
                reason,
            })?;
        let mut state = if resume {
            let state = store
                .read(state_path)
                .map_err(|reason| Error::InvalidQuery {
                    site: site.clone(),
                    reason,
                })?;
            // A different manifest (budget, audience, copy, account)
            // must never inherit a partial hierarchy built from the old
            // one; the canonical fingerprint makes that a hard refusal.
            if state.manifest_fingerprint != fingerprint {
                return Err(Error::InvalidQuery {
                    site: site.clone(),
                    reason: "draft_manifest_changed".into(),
                });
            }
            if state.site != site.as_str() {
                return Err(Error::InvalidQuery {
                    site: site.clone(),
                    reason: "draft_state_site".into(),
                });
            }
            if state.account_id != account {
                return Err(Error::InvalidQuery {
                    site: site.clone(),
                    reason: "draft_state_account".into(),
                });
            }
            if let Some(step) = state.in_flight {
                return Ok(PausedDraftResult::ReconciliationRequired {
                    site: site.clone(),
                    account_id: state.account_id.clone(),
                    step: step.as_str(),
                    guidance: RECONCILE_GUIDANCE,
                });
            }
            // A completed hierarchy is read-only: no further creates.
            if state.stage == DraftStage::Completed {
                return completed_result(&site, &state);
            }
            state
        } else {
            let state = PausedDraftState::new(&site, account.clone(), fingerprint.clone());
            store
                .create_new(state_path, &state)
                .map_err(|reason| Error::InvalidQuery {
                    site: site.clone(),
                    reason,
                })?;
            state
        };

        for step in DraftStep::ALL {
            if !state.step_pending(step) {
                continue; // checkpointed by a previous run
            }
            if step == DraftStep::Image && !manifest.needs_image_upload() {
                state.set_output(
                    DraftStep::Image,
                    manifest
                        .creative
                        .image_hash
                        .clone()
                        .unwrap_or_else(|| "skipped".into()),
                );
                store
                    .checkpoint(state_path, &state)
                    .map_err(|reason| Error::InvalidQuery {
                        site: site.clone(),
                        reason,
                    })?;
                continue;
            }
            deadline.check(&site)?;
            // Policy before the marker: a denied step must leave the
            // state exactly as it was found.
            self.authorize_draft_step(&site, step)?;
            let upload = self.image_upload_for(&site, image, &account, step)?;
            // Write-ahead: record that this write is about to happen
            // before it can, so a crash can never leave "no marker and
            // an existing remote object".
            state.in_flight = Some(step);
            store
                .checkpoint(state_path, &state)
                .map_err(|reason| Error::InvalidQuery {
                    site: site.clone(),
                    reason,
                })?;

            let outcome: Result<String, Error> = match step {
                DraftStep::Image => self
                    .upload_ad_image(key, upload.expect("checked above"), deadline)
                    .await
                    .map(|uploaded| uploaded.hash),
                DraftStep::Campaign => {
                    let request = CreatePausedAdRequest {
                        account: Some(account.clone()),
                        create: PausedAdCreate::Campaign(PausedCampaign {
                            name: manifest.campaign.name.clone(),
                            objective: manifest.campaign.objective,
                            special_ad_categories: manifest.campaign.special_ad_categories.clone(),
                            daily_budget: manifest.campaign.daily_budget,
                            lifetime_budget: manifest.campaign.lifetime_budget,
                            is_adset_budget_sharing_enabled: manifest
                                .campaign
                                .is_adset_budget_sharing_enabled,
                        }),
                    };
                    self.create_paused_ad(key, request, deadline)
                        .await
                        .map(|created| created.id)
                }
                DraftStep::Adset => {
                    let request = CreatePausedAdRequest {
                        account: Some(account.clone()),
                        create: PausedAdCreate::Adset(PausedAdset {
                            name: manifest.adset.name.clone(),
                            // Only a checkpointed campaign ID is ever
                            // wired in — the operator never retypes it.
                            campaign_id: state
                                .campaign_id
                                .clone()
                                .expect("lattice guarantees the campaign"),
                            daily_budget: manifest.adset.daily_budget,
                            lifetime_budget: manifest.adset.lifetime_budget,
                            bid_strategy: manifest.adset.bid_strategy,
                            bid_amount: manifest.adset.bid_amount,
                            roas_average_floor: manifest.adset.roas_average_floor,
                            billing_event: manifest.adset.billing_event,
                            optimization_goal: manifest.adset.optimization_goal,
                            targeting: manifest.adset.targeting.clone(),
                            start_time: manifest.adset.start_time.clone(),
                            end_time: manifest.adset.end_time.clone(),
                            promoted_object: manifest.adset.promoted_object.clone(),
                        }),
                    };
                    self.create_paused_ad(key, request, deadline)
                        .await
                        .map(|created| created.id)
                }
                DraftStep::Creative => {
                    self.draft_create_creative(key, manifest, &state, &account, deadline)
                        .await
                }
                DraftStep::Ad => {
                    let request = CreatePausedAdRequest {
                        account: Some(account.clone()),
                        create: PausedAdCreate::Ad(PausedAd {
                            name: manifest.ad.name.clone(),
                            adset_id: state
                                .adset_id
                                .clone()
                                .expect("lattice guarantees the ad set"),
                            creative_id: state
                                .creative_id
                                .clone()
                                .expect("lattice guarantees the creative"),
                        }),
                    };
                    self.create_paused_ad(key, request, deadline)
                        .await
                        .map(|created| created.id)
                }
            };
            match outcome {
                Ok(id) => {
                    state.set_output(step, id);
                    store
                        .checkpoint(state_path, &state)
                        .map_err(|reason| Error::InvalidQuery {
                            site: site.clone(),
                            reason,
                        })?;
                }
                // No HTTP response arrived: Meta may or may not have
                // created the object. The marker stays and every later
                // mutating command refuses until a human reconciles —
                // a conservative false positive beats a duplicate
                // paused hierarchy.
                Err(e @ (Error::Network { .. } | Error::DeadlineExceeded { .. })) => {
                    let _ = e; // already durably recorded in the state
                    return Ok(PausedDraftResult::ReconciliationRequired {
                        site: site.clone(),
                        account_id: state.account_id.clone(),
                        step: step.as_str(),
                        guidance: RECONCILE_GUIDANCE,
                    });
                }
                // Every other failure is definitive: Meta answered with
                // an error, or the request never left (validation, auth,
                // policy). Clearing the marker keeps the step retryable.
                Err(e) => {
                    state.in_flight = None;
                    store
                        .checkpoint(state_path, &state)
                        .map_err(|reason| Error::InvalidQuery {
                            site: site.clone(),
                            reason,
                        })?;
                    return Err(e);
                }
            }
        }
        completed_result(&site, &state)
    }

    /// Read-only snapshot for `status-draft`. Takes no lock: checkpoints
    /// are atomically replaced, so a concurrent run can never expose a
    /// partial read.
    pub async fn paused_draft_status(
        &self,
        key: &AccountKey,
        store: &dyn DraftStore,
        state_path: &Path,
        deadline: Deadline,
    ) -> Result<DraftStatusReply, Error> {
        let state = store
            .read(state_path)
            .map_err(|reason| Error::InvalidQuery {
                site: key.site.clone(),
                reason,
            })?;
        if state.site != key.site.as_str() {
            return Err(Error::InvalidQuery {
                site: key.site.clone(),
                reason: "draft_state_site".into(),
            });
        }
        let mut review = Vec::new();
        let mut pending = false;
        for (id, entity) in [
            (&state.campaign_id, AdEntity::Campaign),
            (&state.adset_id, AdEntity::Adset),
            (&state.ad_id, AdEntity::Ad),
        ] {
            let Some(id) = id else {
                continue;
            };
            let status = self
                .ad_review_status(
                    key,
                    AdReviewStatusRequest {
                        entity,
                        id: id.clone(),
                    },
                    deadline,
                )
                .await?;
            pending |= status.is_pending_review();
            review.push(status);
        }
        Ok(DraftStatusReply {
            site: key.site.clone(),
            account_id: state.account_id.clone(),
            stage: state.stage.as_str(),
            image_hash: state.image_hash.clone(),
            creative_id: state.creative_id.clone(),
            in_flight: state.in_flight.map(|step| step.as_str()),
            review,
            pending,
        })
    }

    /// Poll [`Self::paused_draft_status`](Self::paused_draft_status)
    /// until no object is pending review or `deadline` expires. Expiry
    /// returns the **last observed reply** — a still-pending review is
    /// normal Meta latency and an explicit, retryable result, never a
    /// timeout error. (Regression: the loop used to start a poll with
    /// the deadline already spent, surfacing a raw `timeout` from the
    /// connector.) `poll_interval` is injected so tests are
    /// deterministic, matching `wait_for_ad_review_with_interval`.
    pub async fn paused_draft_status_wait(
        &self,
        key: &AccountKey,
        store: &dyn DraftStore,
        state_path: &Path,
        deadline: Deadline,
        poll_interval: std::time::Duration,
    ) -> Result<DraftStatusReply, Error> {
        loop {
            let reply = self
                .paused_draft_status(key, store, state_path, deadline)
                .await?;
            if !reply.pending {
                return Ok(reply);
            }
            let remaining = deadline.remaining();
            if remaining.is_zero() {
                return Ok(reply);
            }
            let delay = if poll_interval.is_zero() {
                remaining
            } else {
                poll_interval.min(remaining)
            };
            tokio::time::sleep(delay).await;
        }
    }

    /// Record the human-resolved outcome of an ambiguous write. The
    /// `in_flight` marker must name this exact step; a delivery object
    /// (campaign/ad set/ad) is additionally verified remotely to still
    /// be configured `PAUSED` before the ID enters the state. Image
    /// hashes and creatives have no review edge — their adoption is a
    /// recorded human decision, proven only when the next step uses them.
    pub async fn adopt_paused_draft_step(
        &self,
        key: &AccountKey,
        store: &dyn DraftStore,
        state_path: &Path,
        step: DraftStep,
        remote_id: String,
        deadline: Deadline,
    ) -> Result<PausedDraftResult, Error> {
        let _lock = store
            .try_lock(state_path)
            .map_err(|reason| Error::InvalidQuery {
                site: key.site.clone(),
                reason,
            })?;
        let mut state = store
            .read(state_path)
            .map_err(|reason| Error::InvalidQuery {
                site: key.site.clone(),
                reason,
            })?;
        if state.site != key.site.as_str() {
            return Err(Error::InvalidQuery {
                site: key.site.clone(),
                reason: "draft_state_site".into(),
            });
        }
        if state.in_flight != Some(step) {
            return Err(Error::InvalidQuery {
                site: key.site.clone(),
                reason: format!("draft_not_in_flight:{}", step.as_str()),
            });
        }
        if let Some(entity) = adoption_entity(step) {
            let status = self
                .ad_review_status(
                    key,
                    AdReviewStatusRequest {
                        entity,
                        id: remote_id.clone(),
                    },
                    deadline,
                )
                .await?;
            // An ACTIVE or deleted object must never enter a paused
            // hierarchy's checkpoint, whatever Ads Manager shows.
            if status.configured_status != CONFIGURED_PAUSED {
                return Err(Error::InvalidQuery {
                    site: key.site.clone(),
                    reason: format!("adopt_not_paused:{}", status.configured_status),
                });
            }
        }
        state.set_output(step, remote_id);
        store
            .checkpoint(state_path, &state)
            .map_err(|reason| Error::InvalidQuery {
                site: key.site.clone(),
                reason,
            })?;
        if state.stage == DraftStage::Completed {
            return completed_result(&key.site, &state);
        }
        Ok(PausedDraftResult::InProgress {
            site: key.site.clone(),
            account_id: state.account_id.clone(),
            stage: state.stage.as_str(),
            remaining: state.remaining_steps().iter().map(|s| s.as_str()).collect(),
        })
    }

    pub(super) fn authorize_draft_step(&self, site: &Site, step: DraftStep) -> Result<(), Error> {
        let action = match step {
            DraftStep::Image => AdsAction::UploadAdImage,
            DraftStep::Campaign => AdsAction::CreatePausedCampaign,
            DraftStep::Adset => AdsAction::CreatePausedAdset,
            DraftStep::Creative => AdsAction::CreateLinkAdCreative,
            DraftStep::Ad => AdsAction::CreatePausedAd,
        };
        self.ads_policy.authorize(site, action)
    }

    /// The image bytes are needed only while the upload step is pending;
    /// demanding them earlier would make `resume` after the upload fail
    /// on a deleted local file for no protocol reason.
    pub(super) fn image_upload_for(
        &self,
        site: &Site,
        image: Option<&DraftImage>,
        account: &str,
        step: DraftStep,
    ) -> Result<Option<UploadAdImageRequest>, Error> {
        if step != DraftStep::Image {
            return Ok(None);
        }
        let Some(image) = image else {
            return Err(Error::InvalidQuery {
                site: site.clone(),
                reason: "image_bytes_required".into(),
            });
        };
        Ok(Some(UploadAdImageRequest {
            account: Some(account.to_string()),
            filename: image.filename.clone(),
            bytes: image.bytes.clone(),
        }))
    }

    pub(super) async fn draft_create_creative(
        &self,
        key: &AccountKey,
        manifest: &crate::draft::PausedDraftManifest,
        state: &crate::draft::PausedDraftState,
        account: &str,
        deadline: Deadline,
    ) -> Result<String, Error> {
        draft_create_creative_inner(self, key, manifest, state, account, deadline).await
    }
}

fn completed_result(site: &Site, state: &PausedDraftState) -> Result<PausedDraftResult, Error> {
    // The validated lattice guarantees all five outputs here; anything
    // else is a corrupt file that must refuse, not unwrap.
    let (Some(image_hash), Some(campaign_id), Some(adset_id), Some(creative_id), Some(ad_id)) = (
        &state.image_hash,
        &state.campaign_id,
        &state.adset_id,
        &state.creative_id,
        &state.ad_id,
    ) else {
        return Err(Error::InvalidQuery {
            site: site.clone(),
            reason: "draft_state_stage".into(),
        });
    };
    Ok(PausedDraftResult::Completed {
        site: site.clone(),
        account_id: state.account_id.clone(),
        image_hash: image_hash.clone(),
        campaign_id: campaign_id.clone(),
        adset_id: adset_id.clone(),
        creative_id: creative_id.clone(),
        ad_id: ad_id.clone(),
        configured_status: CONFIGURED_PAUSED,
    })
}

async fn draft_create_creative_inner(
    client: &Client,
    key: &AccountKey,
    manifest: &crate::draft::PausedDraftManifest,
    state: &crate::draft::PausedDraftState,
    account: &str,
    deadline: Deadline,
) -> Result<String, Error> {
    let c = &manifest.creative;
    let hash = state
        .image_hash
        .clone()
        .filter(|h| h != "skipped")
        .or_else(|| c.image_hash.clone())
        .unwrap_or_default();
    match c.kind {
        crate::draft::DraftCreativeKind::Link => {
            let request = crate::ads::CreateLinkAdCreativeRequest {
                account: Some(account.into()),
                creative: crate::ads::LinkAdCreative {
                    name: c.name.clone(),
                    page_id: c.page_id.clone(),
                    image_hash: hash,
                    message: c.message.clone(),
                    headline: c.headline.clone(),
                    destination_url: c.destination_url.clone(),
                    call_to_action: c.call_to_action,
                    geo_link: c.geo_link.clone(),
                    application_id: c.application_id.clone(),
                    app_link: c.app_link.clone(),
                    instagram_user_id: None,
                    advantage_plus: false,
                    whatsapp_identity: None,
                },
            };
            client
                .create_link_ad_creative(key, request, deadline)
                .await
                .map(|created| created.id)
        }
        crate::draft::DraftCreativeKind::Video => {
            let request = crate::ads::CreateVideoAdCreativeRequest {
                account: Some(account.into()),
                creative: crate::ads::VideoAdCreative {
                    name: c.name.clone(),
                    page_id: c.page_id.clone(),
                    video_id: c.video_id.clone().unwrap_or_default(),
                    image_hash: hash,
                    message: c.message.clone(),
                    destination_url: c.destination_url.clone(),
                    call_to_action: c.call_to_action,
                    geo_link: c.geo_link.clone(),
                    application_id: c.application_id.clone(),
                    app_link: c.app_link.clone(),
                    instagram_user_id: None,
                    advantage_plus: false,
                    whatsapp_identity: None,
                },
            };
            client
                .create_video_ad_creative(key, request, deadline)
                .await
                .map(|created| created.id)
        }
        other => {
            let kind = match other {
                crate::draft::DraftCreativeKind::Carousel => {
                    crate::ads::AdCreativeKind::Carousel(crate::ads::CarouselAdCreative {
                        name: c.name.clone(),
                        page_id: c.page_id.clone(),
                        message: c.message.clone(),
                        call_to_action: c.call_to_action,
                        cards: c.cards.clone(),
                        instagram_user_id: None,
                        advantage_plus: false,
                        whatsapp_identity: None,
                    })
                }
                crate::draft::DraftCreativeKind::Catalog => {
                    crate::ads::AdCreativeKind::Catalog(crate::ads::CatalogAdCreative {
                        name: c.name.clone(),
                        page_id: c.page_id.clone(),
                        product_set_id: c.product_set_id.clone().unwrap_or_default(),
                        link: c.link.clone().unwrap_or_else(|| c.destination_url.clone()),
                        message: c.message.clone(),
                        call_to_action: c.call_to_action,
                        instagram_user_id: None,
                        advantage_plus: false,
                        whatsapp_identity: None,
                    })
                }
                crate::draft::DraftCreativeKind::LeadForm => {
                    crate::ads::AdCreativeKind::LeadForm(crate::ads::LeadFormAdCreative {
                        name: c.name.clone(),
                        page_id: c.page_id.clone(),
                        image_hash: hash,
                        message: c.message.clone(),
                        headline: c.headline.clone(),
                        destination_url: c.destination_url.clone(),
                        lead_gen_form_id: c.lead_gen_form_id.clone().unwrap_or_default(),
                        call_to_action: c.call_to_action,
                        instagram_user_id: None,
                        advantage_plus: false,
                        whatsapp_identity: None,
                    })
                }
                crate::draft::DraftCreativeKind::AppInstall => {
                    crate::ads::AdCreativeKind::AppInstall(crate::ads::AppInstallAdCreative {
                        name: c.name.clone(),
                        page_id: c.page_id.clone(),
                        image_hash: hash,
                        message: c.message.clone(),
                        application_id: c.application_id.clone().unwrap_or_default(),
                        object_store_url: c.object_store_url.clone().unwrap_or_default(),
                        instagram_user_id: None,
                        advantage_plus: false,
                        whatsapp_identity: None,
                    })
                }
                crate::draft::DraftCreativeKind::Link | crate::draft::DraftCreativeKind::Video => {
                    unreachable!()
                }
            };
            client
                .create_ad_creative(
                    key,
                    crate::ads::CreateAdCreativeRequest {
                        account: Some(account.into()),
                        kind,
                    },
                    deadline,
                )
                .await
                .map(|created| created.id)
        }
    }
}
