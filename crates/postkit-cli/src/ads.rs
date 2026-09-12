//! Ads, insights, and paused-draft CLI verbs.

use crate::app::fail;
use crate::output::{emit_ok, emit_raw, human_line};
use postkit::{
    AccountKey, AdAccount, AdEntity, AdPreviewFormat, AdReviewStatus, AdReviewStatusRequest,
    AdReviewWait, AttributionWindow, BidStrategy, Breakdown, CampaignObjective, Client,
    CreateLinkAdCreativeRequest, CreatePausedAdRequest, CreatedAd, CreatedAdCreative,
    CreativePreviewRequest, DateRange, Deadline, DraftImage, DraftStatusReply, Error, InsightRow,
    InsightsLevel, InsightsQuery, LinkAdCreative, LinkCallToAction, Metric, PausedAd,
    PausedAdCreate, PausedAdset, PausedCampaign, PausedDraftManifest, PausedDraftResult,
    PublishedMedia, Site, UploadAdImageRequest, UploadedAdImage,
};
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub(crate) async fn one_paused_create(
    client: &Client,
    key: &AccountKey,
    request: CreatePausedAdRequest,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    match client.create_paused_ad(key, request, deadline).await {
        Ok(created) => {
            emit_ok(&created, json, || created_ad_line(&created));
            Ok(())
        }
        Err(error) => Err(fail(&error, json)),
    }
}

/// Image upload is a remote asset write but has no delivery status. State it
/// plainly so the successful result cannot be mistaken for a running ad.
pub(crate) async fn one_image_upload(
    client: &Client,
    key: &AccountKey,
    request: UploadAdImageRequest,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    match client.upload_ad_image(key, request, deadline).await {
        Ok(uploaded) => {
            emit_ok(&uploaded, json, || uploaded_image_line(&uploaded));
            Ok(())
        }
        Err(error) => Err(fail(&error, json)),
    }
}

/// A creative is reusable account metadata, not a delivery object. Showing
/// that distinction in human output helps an operator take the still-paused
/// `create-ad` step consciously rather than assuming an ad has started.
pub(crate) async fn one_link_creative(
    client: &Client,
    key: &AccountKey,
    request: CreateLinkAdCreativeRequest,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    match client.create_link_ad_creative(key, request, deadline).await {
        Ok(created) => {
            emit_ok(&created, json, || created_creative_line(&created));
            Ok(())
        }
        Err(error) => Err(fail(&error, json)),
    }
}

/// Preview markup is intentionally not passed to `emit_ok`: an iframe body
/// is useful only as a local artifact and could be unwieldy or unsafe in an
/// agent log. The success reply records just enough to locate and review it.
pub(crate) async fn one_creative_preview(
    client: &Client,
    key: &AccountKey,
    request: CreativePreviewRequest,
    output: PathBuf,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    match client.preview_ad_creative(key, request, deadline).await {
        Ok(preview) => {
            write_preview_output(&output, &preview.body).map_err(|error| {
                fail(
                    &ads_input_error(key.site.as_str(), preview_output_reason(&error)),
                    json,
                )
            })?;
            let output = output.to_string_lossy().into_owned();
            if json {
                emit_raw(&serde_json::json!({
                    "site": preview.site,
                    "creative_id": preview.creative_id,
                    "ad_format": preview.ad_format.as_str(),
                    "output": output,
                }));
            } else {
                human_line(format!(
                    "creative {} preview={} output={}",
                    preview.creative_id,
                    preview.ad_format.as_str(),
                    output
                ));
            }
            Ok(())
        }
        Err(error) => Err(fail(&error, json)),
    }
}

/// Status inspection's two modes share the same clear rendering. A waiting
/// result is still a read-only observation: `pending_review` is not an error
/// and never implies that a `PAUSED` object can start delivery.
pub(crate) async fn one_ad_review_status(
    client: &Client,
    key: &AccountKey,
    request: AdReviewStatusRequest,
    wait: bool,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    if wait {
        match client.wait_for_ad_review(key, request, deadline).await {
            Ok(result) => {
                if json {
                    emit_raw(&serde_json::to_value(&result).expect("json"));
                } else {
                    match result {
                        AdReviewWait::Settled(status) => emit_ad_review_status(&status, "settled"),
                        AdReviewWait::PendingReview(status) => {
                            emit_ad_review_status(&status, "pending_review")
                        }
                    }
                }
                Ok(())
            }
            Err(error) => Err(fail(&error, json)),
        }
    } else {
        match client.ad_review_status(key, request, deadline).await {
            Ok(status) => {
                if json {
                    emit_raw(&serde_json::to_value(&status).expect("json"));
                } else {
                    let review = if status.is_pending_review() {
                        "pending_review"
                    } else {
                        "settled"
                    };
                    emit_ad_review_status(&status, review);
                }
                Ok(())
            }
            Err(error) => Err(fail(&error, json)),
        }
    }
}

/// Print the two status layers on every human reply and retain Meta's issue
/// text on separate lines. This makes `PENDING_REVIEW` visibly distinct from
/// a requested `PAUSED` state without hiding the actionable review problem.
pub(crate) fn emit_ad_review_status(status: &AdReviewStatus, review: &str) {
    human_line(format!(
        "{} {} configured={} effective={} review={} issues={}",
        status.entity.as_str(),
        status.id,
        status.configured_status,
        status.effective_status,
        review,
        status.issues.len(),
    ));
    for issue in &status.issues {
        let code = issue.code.as_deref().unwrap_or("-");
        let level = issue.level.as_deref().unwrap_or("-");
        let summary = issue.summary.as_deref().unwrap_or("-");
        let message = issue.message.as_deref().unwrap_or("-");
        human_line(format!(
            "issue code={code} level={level} summary={summary} message={message}"
        ));
    }
}

pub(crate) struct InsightsOptions {
    pub(crate) ad_account: Option<String>,
    pub(crate) entity_ids: Vec<String>,
    pub(crate) breakdowns: String,
    pub(crate) report: String,
}

/// Common builder error shape for advertising input. These errors name only
/// the invalid field, never echo targeting JSON or an operator's local path.
pub(crate) fn ads_input_error(site: &str, reason: impl Into<String>) -> Error {
    Error::InvalidQuery {
        site: Site::new(site),
        reason: reason.into(),
    }
}

/// Parse the reviewed manifest at the operator's chosen path. Read errors
/// never echo the path; the operator just chose it. Serde's own message is
/// kept (it names the offending field) but truncated to one line so a
/// formatting accident cannot flood the terminal.
pub(crate) fn read_draft_manifest(site: &str, path: &Path) -> Result<PausedDraftManifest, Error> {
    let raw =
        std::fs::read_to_string(path).map_err(|_| ads_input_error(site, "manifest_unreadable"))?;
    serde_json::from_str(&raw).map_err(|e| {
        let message = e.to_string();
        let first_line = message.lines().next().unwrap_or("parse error");
        ads_input_error(site, format!("bad_manifest:{first_line}"))
    })
}

/// Resolve the manifest's image reference to bytes + basename — the sole
/// filesystem boundary of the draft flow, mirroring `UploadImage`. A file
/// that is missing or unreadable yields `None`, not an error: the core
/// decides whether the bytes are still needed (a resume whose upload is
/// already checkpointed must not fail on a since-deleted local file).
pub(crate) fn read_draft_image(
    site: &str,
    manifest: &PausedDraftManifest,
) -> Result<Option<DraftImage>, Error> {
    let filename = manifest
        .image_filename()
        .map_err(|r| ads_input_error(site, r))?;
    let Some(bytes) = std::fs::read(&manifest.creative.image_file).ok() else {
        return Ok(None);
    };
    if bytes.is_empty() {
        return Ok(None); // surfaced later as image_file_empty if still needed
    }
    Ok(Some(DraftImage { filename, bytes }))
}

/// Draft results always print the no-spend posture in human mode: the most
/// dangerous misreading of this command is "it launched".
pub(crate) fn emit_draft_result(result: &PausedDraftResult, json: bool) {
    if json {
        emit_raw(&serde_json::to_value(result).expect("draft result serializes"));
        return;
    }
    match result {
        PausedDraftResult::Completed {
            site,
            campaign_id,
            adset_id,
            creative_id,
            ad_id,
            ..
        } => {
            human_line(format!(
                "{site} draft completed — all delivery objects PAUSED, nothing activated, no spend"
            ));
            human_line(format!(
                "campaign {campaign_id} · ad set {adset_id} · creative {creative_id} · ad {ad_id}"
            ));
        }
        PausedDraftResult::InProgress {
            site,
            stage,
            remaining,
            ..
        } => {
            human_line(format!(
                "{site} draft at {stage}; remaining: {}",
                remaining.join(", ")
            ));
        }
        PausedDraftResult::ReconciliationRequired {
            site,
            step,
            guidance,
            ..
        } => {
            human_line(format!(
                "{site} draft step '{step}' has an UNKNOWN outcome — refusing to continue"
            ));
            human_line(guidance);
        }
    }
}

pub(crate) fn emit_draft_status(reply: &DraftStatusReply, json: bool) {
    if json {
        emit_raw(&serde_json::to_value(reply).expect("draft status serializes"));
        return;
    }
    human_line(format!(
        "{} draft at {}{}",
        reply.site,
        reply.stage,
        reply
            .in_flight
            .map(|s| format!(" (in flight: {s})"))
            .unwrap_or_default()
    ));
    for status in &reply.review {
        human_line(format!(
            "{} {}: configured {} · effective {}{}",
            status.entity.as_str(),
            status.id,
            status.configured_status,
            status.effective_status,
            if status.is_pending_review() {
                " · pending review"
            } else {
                ""
            }
        ));
        for issue in &status.issues {
            human_line(format!(
                "  issue: {} {}",
                issue.code.as_deref().unwrap_or("?"),
                issue.summary.as_deref().unwrap_or("")
            ));
        }
    }
}

pub(crate) struct PausedCampaignOptions {
    pub(crate) ad_account: Option<String>,
    pub(crate) name: String,
    pub(crate) objective: String,
    pub(crate) special_ad_categories: String,
    pub(crate) daily_budget: Option<u64>,
    pub(crate) lifetime_budget: Option<u64>,
    pub(crate) is_adset_budget_sharing_enabled: bool,
}

pub(crate) fn build_paused_campaign_request(
    site: &str,
    options: PausedCampaignOptions,
) -> Result<CreatePausedAdRequest, Error> {
    let objective = CampaignObjective::from_str(&options.objective)
        .map_err(|reason| ads_input_error(site, reason))?;
    let request = CreatePausedAdRequest {
        account: options.ad_account,
        create: PausedAdCreate::Campaign(PausedCampaign {
            name: options.name,
            objective,
            special_ad_categories: options
                .special_ad_categories
                .split(',')
                .map(str::trim)
                .filter(|category| !category.is_empty())
                .map(str::to_owned)
                .collect(),
            daily_budget: options.daily_budget,
            lifetime_budget: options.lifetime_budget,
            is_adset_budget_sharing_enabled: options.is_adset_budget_sharing_enabled,
        }),
    };
    request
        .validate()
        .map_err(|reason| ads_input_error(site, reason))?;
    Ok(request)
}

/// Convert the CLI's selected file into the library's path-free request.
/// Keeping this pure makes filename and empty-byte checks testable without a
/// temporary file and preserves the no-local-path error contract.
pub(crate) fn build_upload_ad_image_request(
    site: &str,
    ad_account: Option<String>,
    filename: String,
    bytes: Vec<u8>,
) -> Result<UploadAdImageRequest, Error> {
    let request = UploadAdImageRequest {
        account: ad_account,
        filename,
        bytes,
    };
    request
        .validate()
        .map_err(|reason| ads_input_error(site, reason))?;
    Ok(request)
}

/// Required fields for the one supported creative shape travel together so
/// future image, video, and carousel types cannot silently inherit fields
/// intended only for this Page image-link contract.
pub(crate) struct LinkCreativeOptions {
    pub(crate) ad_account: Option<String>,
    pub(crate) name: String,
    pub(crate) page_id: String,
    pub(crate) image_hash: String,
    pub(crate) message: String,
    pub(crate) headline: String,
    pub(crate) destination_url: String,
    pub(crate) call_to_action: String,
}

pub(crate) fn build_link_ad_creative_request(
    site: &str,
    options: LinkCreativeOptions,
) -> Result<CreateLinkAdCreativeRequest, Error> {
    let call_to_action = LinkCallToAction::from_str(&options.call_to_action)
        .map_err(|reason| ads_input_error(site, reason))?;
    let request = CreateLinkAdCreativeRequest {
        account: options.ad_account,
        creative: LinkAdCreative {
            name: options.name,
            page_id: options.page_id,
            image_hash: options.image_hash,
            message: options.message,
            headline: options.headline,
            destination_url: options.destination_url,
            call_to_action,
        },
    };
    request
        .validate()
        .map_err(|reason| ads_input_error(site, reason))?;
    Ok(request)
}

/// `--ad-format` stays a CLI string only until this builder. Converting to a
/// closed type here keeps a misspelled placement from looking like a valid
/// preview request to a library caller or a remote Marketing API endpoint.
pub(crate) fn build_creative_preview_request(
    site: &str,
    creative_id: &str,
    ad_format: &str,
) -> Result<CreativePreviewRequest, Error> {
    let ad_format =
        AdPreviewFormat::from_str(ad_format).map_err(|reason| ads_input_error(site, reason))?;
    let request = CreativePreviewRequest {
        creative_id: creative_id.into(),
        ad_format,
    };
    request
        .validate()
        .map_err(|reason| ads_input_error(site, reason))?;
    Ok(request)
}

/// Parse the CLI's broad `--entity` string only at the boundary, then carry a
/// closed enum through the library. A typo must be a local, no-HTTP error—not
/// a generic Graph response about an arbitrary object path.
pub(crate) fn build_ad_review_status_request(
    site: &str,
    entity: &str,
    id: &str,
) -> Result<AdReviewStatusRequest, Error> {
    let entity = AdEntity::from_str(entity).map_err(|reason| ads_input_error(site, reason))?;
    let request = AdReviewStatusRequest {
        entity,
        id: id.into(),
    };
    request
        .validate()
        .map_err(|reason| ads_input_error(site, reason))?;
    Ok(request)
}

/// Save the opaque iframe as a new owner-only file. `create_new` is
/// deliberate: a typo cannot silently overwrite an earlier reviewed preview
/// or an unrelated local file. The operator chooses a different path to make
/// another artifact.
pub(crate) fn write_preview_output(path: &Path, body: &str) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        // Preview markup may carry a short-lived, account-scoped iframe URL,
        // so do not create it readable by other local users.
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn preview_output_reason(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::AlreadyExists => "preview_output_exists",
        _ => "preview_output_unwritable",
    }
}

pub(crate) fn build_paused_adset_request(
    site: &str,
    options: PausedAdsetOptions,
) -> Result<CreatePausedAdRequest, Error> {
    let bid_strategy = BidStrategy::from_str(&options.bid_strategy)
        .map_err(|reason| ads_input_error(site, reason))?;
    let billing_event = postkit::BillingEvent::from_str(&options.billing_event)
        .map_err(|reason| ads_input_error(site, reason))?;
    let optimization_goal = postkit::OptimizationGoal::from_str(&options.optimization_goal)
        .map_err(|reason| ads_input_error(site, reason))?;
    let targeting = build_ad_targeting(&options).map_err(|reason| ads_input_error(site, reason))?;
    let request = CreatePausedAdRequest {
        account: options.ad_account,
        create: PausedAdCreate::Adset(PausedAdset {
            name: options.name,
            campaign_id: options.campaign_id,
            daily_budget: options.daily_budget,
            lifetime_budget: options.lifetime_budget,
            bid_amount: None,
            roas_average_floor: None,
            bid_strategy,
            billing_event,
            optimization_goal,
            targeting,
        }),
    };
    request
        .validate()
        .map_err(|reason| ads_input_error(site, reason))?;
    Ok(request)
}

/// The many required ad-set fields travel together from Clap to the pure
/// builder. This keeps adding one targeting option from turning the builder
/// into an error-prone positional parameter list.
pub(crate) struct PausedAdsetOptions {
    pub(crate) ad_account: Option<String>,
    pub(crate) name: String,
    pub(crate) campaign_id: String,
    pub(crate) daily_budget: Option<u64>,
    pub(crate) lifetime_budget: Option<u64>,
    pub(crate) bid_strategy: String,
    pub(crate) billing_event: String,
    pub(crate) optimization_goal: String,
    pub(crate) countries: Vec<String>,
    pub(crate) age_min: Option<u8>,
    pub(crate) age_max: Option<u8>,
    pub(crate) publisher_platforms: Vec<String>,
    pub(crate) facebook_positions: Vec<String>,
    pub(crate) instagram_positions: Vec<String>,
}

fn build_ad_targeting(options: &PausedAdsetOptions) -> Result<postkit::AdTargeting, String> {
    let countries = options
        .countries
        .iter()
        .map(|c| c.trim().to_ascii_uppercase())
        .collect();
    let publisher_platforms = options
        .publisher_platforms
        .iter()
        .map(|p| p.parse())
        .collect::<Result<Vec<_>, _>>()?;
    let facebook_positions = options
        .facebook_positions
        .iter()
        .map(|p| p.parse())
        .collect::<Result<Vec<_>, _>>()?;
    let instagram_positions = options
        .instagram_positions
        .iter()
        .map(|p| p.parse())
        .collect::<Result<Vec<_>, _>>()?;
    Ok(postkit::AdTargeting {
        geo_locations: postkit::GeoLocations { countries },
        age_min: options.age_min,
        age_max: options.age_max,
        publisher_platforms,
        facebook_positions,
        instagram_positions,
    })
}

pub(crate) fn build_paused_ad_request(
    site: &str,
    ad_account: Option<String>,
    name: &str,
    adset_id: &str,
    creative_id: &str,
) -> Result<CreatePausedAdRequest, Error> {
    let request = CreatePausedAdRequest {
        account: ad_account,
        create: PausedAdCreate::Ad(PausedAd {
            name: name.into(),
            adset_id: adset_id.into(),
            creative_id: creative_id.into(),
        }),
    };
    request
        .validate()
        .map_err(|reason| ads_input_error(site, reason))?;
    Ok(request)
}

/// CLI flags → `InsightsQuery`. Pure over its inputs so the parse errors
/// (`unknown_*`, date and range problems) are unit-testable without HTTP.
pub(crate) fn build_insights_query(
    site: &str,
    from: &str,
    to: &str,
    level: &str,
    metrics: &str,
    attribution: &str,
    options: InsightsOptions,
) -> Result<InsightsQuery, Error> {
    let bad = |reason: String| Error::InvalidQuery {
        site: Site::new(site),
        reason,
    };
    let level = InsightsLevel::from_str(level).map_err(bad)?;
    let attribution = AttributionWindow::from_str(attribution).map_err(bad)?;
    let mut parsed = Vec::new();
    for m in metrics.split(',') {
        let m = m.trim();
        if m.is_empty() {
            continue;
        }
        parsed.push(Metric::from_str(m).map_err(bad)?);
    }
    if parsed.is_empty() {
        return Err(bad("no_metrics".into()));
    }
    let report = postkit::InsightsReportKind::from_str(&options.report).map_err(bad)?;
    let mut parsed_breakdowns = Vec::new();
    for breakdown in options.breakdowns.split(',') {
        let breakdown = breakdown.trim();
        if breakdown.is_empty() {
            continue;
        }
        parsed_breakdowns.push(Breakdown::from_str(breakdown).map_err(bad)?);
    }
    let range = DateRange {
        from: from.into(),
        to: to.into(),
    };
    let query = InsightsQuery {
        level,
        metrics: parsed,
        attribution,
        range,
        account: options.ad_account,
        entity_ids: options.entity_ids,
        breakdowns: parsed_breakdowns,
        report,
    };
    query.validate().map_err(bad)?;
    Ok(query)
}

/// One human-mode row: `date level entity dimension=v metric=v …`. Keeping
/// dimensions before metrics prevents a country/platform label from looking
/// like a number that callers may sum across rows.
pub(crate) fn emit_insights_reply(reply: &postkit::InsightsReply, json: bool) -> Result<(), i32> {
    if json {
        emit_raw(&serde_json::to_value(reply).expect("json"));
    } else {
        human_line(format!(
            "{} {} {}",
            reply.site,
            reply.account_id,
            reply.currency.as_deref().unwrap_or("-")
        ));
        for row in &reply.rows {
            human_line(insight_line(row));
        }
    }
    Ok(())
}

pub(crate) async fn run_async_insights(
    client: &Client,
    key: &AccountKey,
    query: InsightsQuery,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    let job = client
        .start_insights_job(key, query.clone(), deadline)
        .await
        .map_err(|e| fail(&e, json))?;
    let waited = client
        .wait_for_insights_job(key, &job.id, query, deadline)
        .await
        .map_err(|e| fail(&e, json))?;
    match waited {
        postkit::InsightsJobWait::Completed(reply) => emit_insights_reply(&reply, json),
        other => {
            if json {
                emit_raw(&serde_json::to_value(&other).expect("json"));
                Ok(())
            } else {
                human_line(format!("{other:?}"));
                Ok(())
            }
        }
    }
}

pub(crate) fn insight_line(row: &InsightRow) -> String {
    let mut parts = vec![
        row.date_start.clone(),
        row.level.as_str().to_string(),
        row.entity_id.clone(),
    ];
    for (k, v) in &row.dimensions {
        parts.push(format!("{k}={v}"));
    }
    for (k, v) in &row.metrics {
        parts.push(format!("{k}={v}"));
    }
    parts.join(" ")
}

/// Human output keeps the canonical `act_<id>` first so it can be copied
/// directly into `insights --ad-account`; names and metadata remain labels.
pub(crate) fn ad_account_line(account: &AdAccount) -> String {
    format!(
        "{} name={} currency={} timezone={} status={}",
        account.id,
        account.name.as_deref().unwrap_or("-"),
        account.currency.as_deref().unwrap_or("-"),
        account.timezone.as_deref().unwrap_or("-"),
        account.status.as_deref().unwrap_or("-")
    )
}

/// Keep human output to copyable identity/link metadata. Captions can contain
/// arbitrary newlines and are already available faithfully in `--json`; never
/// render them as terminal lines where remote content could blur record
/// boundaries for an operator.
pub(crate) fn media_line(media: &PublishedMedia) -> String {
    let media_type = media.media_type.as_deref().unwrap_or("-");
    let permalink = media.permalink.as_deref().unwrap_or("-");
    let timestamp = media.timestamp.as_deref().unwrap_or("-");
    format!(
        "{} type={media_type} timestamp={timestamp} permalink={permalink}",
        media.id
    )
}

/// The id is adjacent to its entity name, followed by explicit `PAUSED`, so
/// it can be copied into Ads Manager without a human mistaking it for active.
pub(crate) fn created_ad_line(created: &CreatedAd) -> String {
    format!(
        "{} {} status={} account={}",
        created.entity.as_str(),
        created.id,
        created.status,
        created.account_id
    )
}

/// The hash is the only value the next creative command needs. It is safe to
/// copy, whereas the source image's local path deliberately never appears.
pub(crate) fn uploaded_image_line(uploaded: &UploadedAdImage) -> String {
    format!(
        "image hash={} account={}",
        uploaded.hash, uploaded.account_id
    )
}

/// Make the non-delivery property visible in text output as it is in the
/// type contract: a creative alone cannot spend or enter an auction.
pub(crate) fn created_creative_line(created: &CreatedAdCreative) -> String {
    format!(
        "creative {} not-delivering account={}",
        created.id, created.account_id
    )
}
