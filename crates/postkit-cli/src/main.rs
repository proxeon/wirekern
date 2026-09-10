mod output;

use clap::{Parser, Subcommand};
use output::{emit_err, emit_ok, emit_raw, human_line};
use postkit::connectors::instagram::MAX_CAROUSEL_IMAGES;
use postkit::connectors::threads::validate_text;
use postkit::{
    app_source, extract_code, valid_name, verify_state, AccountKey, AdAccount, AdEntity,
    AdPreviewFormat, AdReviewStatus, AdReviewStatusRequest, AdReviewWait, AppConfig, AppStore,
    AttributionWindow, AuthReply, BidStrategy, Body, Breakdown, CampaignObjective, Client,
    CreateLinkAdCreativeRequest, CreatePausedAdRequest, CreatedAd, CreatedAdCreative,
    CreativePreviewRequest, DateRange, Deadline, DraftImage, DraftStatusReply, DraftStep, Error,
    FileAppStore, FileDraftStore, FileVault, Image, InsightRow, InsightsLevel, InsightsQuery,
    Intent, LinkAdCreative, LinkCallToAction, MediaQuery, Metric, OAuthApp, PausedAd,
    PausedAdCreate, PausedAdset, PausedCampaign, PausedDraftManifest, PausedDraftResult,
    PostRequest, PublishedMedia, Registry, RunPausedDraft, Site, UploadAdImageRequest,
    UploadedAdImage, Vault, DEFAULT_MEDIA_LIMIT,
};
use std::fs::OpenOptions;
use std::io::{self, BufRead, IsTerminal, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(
    name = "postkit",
    version,
    about = "Publish to official APIs. BYO credentials."
)]
struct Cli {
    /// JSON document on stdout (agents). Human text on stderr otherwise.
    #[arg(long, global = true)]
    json: bool,
    /// Vault root. Default ~/.postkit
    #[arg(long, global = true, env = "POSTKIT_HOME")]
    home: Option<PathBuf>,
    /// Seconds for a network operation, including any token refresh on the way. Default 30.
    #[arg(long, global = true, default_value_t = 30)]
    deadline: u64,
    /// Account alias. Default default.
    #[arg(long, global = true, default_value = "default")]
    account: String,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Post {
        site: Option<String>,
        /// Repeatable. Two or more on threads = reply chain.
        #[arg(long, action = clap::ArgAction::Append)]
        text: Vec<String>,
        /// Comma-separated sites. Same text and --param on each.
        #[arg(long)]
        to: Option<String>,
        /// target extras, e.g. chat_id=-100
        #[arg(long = "param", value_name = "k=v")]
        param: Vec<String>,
        #[arg(long)]
        idempotency: Option<String>,
        /// Raw request JSON on stdin.
        #[arg(long)]
        stdin: bool,
        /// Create-only probe (threads): validate everything a publish
        /// would, publish nothing. The container expires in 24h.
        #[arg(long)]
        dry_run: bool,
        /// Repeat for an Instagram image carousel (2–10 public HTTPS URLs).
        /// One image remains the normal site-specific image post: a local
        /// file (Bluesky/Facebook Pages upload bytes) or a public HTTPS URL
        /// (Threads/Instagram crawl it). The kernel never converts forms.
        #[arg(long, action = clap::ArgAction::Append)]
        image: Vec<String>,
        /// Accessibility text for --image (embedded where the platform
        /// supports it; Threads and Instagram v1 have no verified alt field).
        #[arg(long, default_value = "")]
        alt: String,
    },
    Auth {
        site: String,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        code: Option<String>,
        #[arg(long)]
        listen: bool,
        /// Bluesky app password. Omit the value to prompt on a TTY.
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        password: Option<String>,
    },
    Whoami {
        site: String,
    },
    /// Read spend/performance metrics (meta_ads). Range ≤ 90 days.
    Insights {
        site: String,
        /// Range start, YYYY-MM-DD.
        #[arg(long)]
        from: String,
        /// Range end, YYYY-MM-DD (inclusive).
        #[arg(long)]
        to: String,
        /// account | campaign | adset | ad.
        #[arg(long, default_value = "account")]
        level: String,
        /// Comma-separated: spend,impressions,clicks,reach,ctr,cpc,cpm,purchases,purchase_value,roas.
        #[arg(long, default_value = "spend,impressions,clicks,purchases")]
        metrics: String,
        /// 7d_click_1d_view | 1d_click | 1d_view. Explicit — ROAS answers
        /// change with the window, so there is no default.
        #[arg(long)]
        attribution: String,
        /// Override the stored ad account (123 or act_123).
        #[arg(long)]
        ad_account: Option<String>,
        /// Repeatable campaign, ad set, or ad ID filter. Not valid at account level.
        #[arg(long = "entity-id")]
        entity_ids: Vec<String>,
        /// Comma-separated: country,publisher_platform,age.
        #[arg(long, default_value = "")]
        breakdowns: String,
    },
    /// Read-only advertising-account discovery (not local vault aliases).
    #[command(subcommand)]
    Ads(AdsCmd),
    /// Read-only Facebook Page discovery. This lists Page identities and
    /// tasks, never Page access tokens; pass a returned ID as post
    /// `--param page_id=<id>` for an explicit organic publish target.
    #[command(subcommand)]
    Pages(PagesCmd),
    /// Read a deliberately bounded first page of published media. This is
    /// GET-only and never creates, edits, or makes a post visible.
    #[command(subcommand)]
    Media(MediaCmd),
    Capabilities {
        site: Option<String>,
    },
    #[command(subcommand)]
    Accounts(AccountsCmd),
    #[command(subcommand)]
    Apps(AppsCmd),
}

#[derive(Subcommand, Debug)]
enum AccountsCmd {
    List {
        #[arg(long)]
        site: Option<String>,
    },
    Delete {
        site: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum AdsCmd {
    /// List Meta ad accounts visible to the selected credential.
    Accounts { site: String },
    /// Read a campaign, ad set, or ad's configured and effective review
    /// states. `--wait` polls only until the global --deadline and never
    /// changes a draft, activation, budget, or payment setting.
    Status {
        site: String,
        /// campaign | adset | ad.
        #[arg(long)]
        entity: String,
        /// Existing Meta campaign, ad set, or ad ID.
        #[arg(long)]
        id: String,
        /// Poll Meta's read-only status endpoint until review settles or
        /// --deadline expires. A pending result is explicit and retryable.
        #[arg(long)]
        wait: bool,
    },
    /// Render a saved Meta creative locally. This is a read-only review and
    /// cannot create, activate, fund, or otherwise change an ad.
    PreviewCreative {
        site: String,
        /// Existing Meta ad-creative ID.
        #[arg(long)]
        creative_id: String,
        /// desktop_feed_standard | mobile_feed_standard.
        #[arg(long)]
        ad_format: String,
        /// New local HTML file to receive Meta's iframe preview.
        #[arg(long)]
        output: PathBuf,
    },
    /// Upload a local image for use by a later Meta link creative. Uploading
    /// media creates no ad and cannot start delivery.
    UploadImage {
        site: String,
        #[arg(long)]
        ad_account: Option<String>,
        /// Local image file. Its path is never included in CLI errors.
        #[arg(long)]
        file: PathBuf,
    },
    /// Create a Page-backed image-link creative. The creative itself cannot
    /// deliver; a later `create-ad` still creates an ad as PAUSED.
    CreateLinkCreative {
        site: String,
        #[arg(long)]
        ad_account: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        page_id: String,
        #[arg(long)]
        image_hash: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        headline: String,
        #[arg(long)]
        destination_url: String,
        /// `learn_more`; more CTA types need their own typed value fields.
        #[arg(long)]
        call_to_action: String,
    },
    /// Create a Meta campaign with status hard-coded to PAUSED.
    CreateCampaign {
        site: String,
        /// Override the account stored by Meta OAuth (123 or act_123).
        #[arg(long)]
        ad_account: Option<String>,
        #[arg(long)]
        name: String,
        /// awareness | traffic | engagement | leads | app_promotion | sales.
        #[arg(long)]
        objective: String,
        /// Comma-separated Meta special-ad categories; blank means none.
        #[arg(long, default_value = "")]
        special_ad_categories: String,
    },
    /// Create a Meta ad set with status hard-coded to PAUSED.
    CreateAdset {
        site: String,
        #[arg(long)]
        ad_account: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        campaign_id: String,
        /// Daily budget in the ad account's minor currency unit.
        #[arg(long)]
        daily_budget: u64,
        /// `lowest_cost_without_cap`; required so Meta cannot inherit a
        /// bid-cap or ROAS strategy whose constraint is absent.
        #[arg(long)]
        bid_strategy: String,
        #[arg(long)]
        billing_event: String,
        #[arg(long)]
        optimization_goal: String,
        /// JSON object with the Meta targeting specification.
        #[arg(long)]
        targeting_file: PathBuf,
    },
    /// Create a Meta ad with status hard-coded to PAUSED.
    CreateAd {
        site: String,
        #[arg(long)]
        ad_account: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        adset_id: String,
        /// Existing Meta ad-creative ID; postkit creates no creative defaults.
        #[arg(long)]
        creative_id: String,
    },
    /// Validate a paused-draft manifest locally. No vault read, no image
    /// read, no HTTP — the answer is a pure function of the file.
    ValidateDraft {
        site: String,
        /// Reviewed JSON manifest (see docs/meta-ads/README.md).
        #[arg(long)]
        manifest: PathBuf,
    },
    /// Execute a manifest as one paused hierarchy, checkpointing after every
    /// confirmed remote create. The --state file must not exist yet.
    CreateDraft {
        site: String,
        #[arg(long)]
        manifest: PathBuf,
        /// New checkpoint file; owner-only (0600), atomically rewritten.
        #[arg(long)]
        state: PathBuf,
    },
    /// Continue an interrupted manifest from its last durable checkpoint.
    /// Refuses (never retries) a step whose remote outcome is unknown.
    ResumeDraft {
        site: String,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        state: PathBuf,
    },
    /// Read the checkpoint's known objects and their live review states.
    /// GET-only; `--wait` polls until review settles or --deadline expires.
    StatusDraft {
        site: String,
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        wait: bool,
    },
    /// Record the human-resolved ID of an ambiguous write (the state's
    /// in-flight step). Delivery objects are verified PAUSED remotely first.
    AdoptDraftStep {
        site: String,
        #[arg(long)]
        state: PathBuf,
        /// image | campaign | adset | creative | ad.
        #[arg(long)]
        step: String,
        /// The remote object ID (or image hash) resolved in Ads Manager.
        #[arg(long)]
        id: String,
    },
}

#[derive(Subcommand, Debug)]
enum PagesCmd {
    /// List Pages visible to the selected Facebook Pages credential.
    Accounts { site: String },
}

#[derive(Subcommand, Debug)]
enum MediaCmd {
    /// List recent published media for the credential's explicit account.
    List {
        site: String,
        /// First-page size, 1 through 25. Postkit intentionally exposes no
        /// pagination cursor until that larger read contract is reviewed.
        #[arg(long, default_value_t = DEFAULT_MEDIA_LIMIT)]
        limit: u8,
    },
}

#[derive(Subcommand, Debug)]
enum AppsCmd {
    Show {
        site: String,
    },
    Set {
        site: String,
        #[arg(long)]
        client_id: String,
        #[arg(long)]
        client_secret: String,
        #[arg(long)]
        redirect_uri: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        std::process::exit(e);
    }
}

async fn run(cli: Cli) -> Result<(), i32> {
    let home =
        resolve_home(cli.home, std::env::var_os("HOME").map(PathBuf::from)).map_err(|msg| {
            eprintln!("{msg}");
            2
        })?;
    let json = cli.json;
    let account = cli.account.clone();
    let deadline = Deadline::from_secs(cli.deadline);

    match cli.command {
        Commands::Apps(AppsCmd::Set {
            site,
            client_id,
            client_secret,
            redirect_uri,
        }) => {
            check_name(&site, json)?;
            let apps = FileAppStore::new(&home).map_err(|e| fail(&e, json))?;
            apps.put(&AppConfig {
                site: Site::new(&site),
                oauth: Some(OAuthApp {
                    client_id,
                    client_secret,
                    redirect_uri,
                }),
                extra: serde_json::json!({}),
            })
            .map_err(|e| fail(&e, json))?;
            // The env layer outranks the file; say so when the write that
            // just "succeeded" will never be read back while it is set.
            if app_source(&Site::new(&site)) == "env" {
                let key = site.to_ascii_uppercase().replace('-', "_");
                eprintln!(
                    "note: POSTKIT_{key}_CLIENT_ID/_CLIENT_SECRET are set and take precedence over apps/{site}.json"
                );
            }
            if json {
                emit_raw(&serde_json::json!({ "site": site }));
            } else {
                eprintln!("wrote {}/apps/{site}.json", home.display());
            }
            Ok(())
        }
        Commands::Apps(AppsCmd::Show { site }) => {
            check_name(&site, json)?;
            let apps = FileAppStore::new(&home).map_err(|e| fail(&e, json))?;
            let cfg = apps.get(&Site::new(&site)).map_err(|e| fail(&e, json))?;
            let id = cfg
                .oauth
                .as_ref()
                .map(|o| o.client_id.as_str())
                .unwrap_or("");
            let redir = cfg
                .oauth
                .as_ref()
                .map(|o| o.redirect_uri.as_str())
                .unwrap_or("");
            // env credentials silently outrank the file; the operator must
            // be able to see which layer answered
            let source = app_source(&Site::new(&site));
            if json {
                emit_raw(&serde_json::json!({
                    "site": site,
                    "source": source,
                    "client_id": id,
                    "redirect_uri": redir,
                }));
            } else {
                human_line(format!(
                    "site={site} source={source} client_id={id} redirect_uri={redir} client_secret=[redacted]"
                ));
            }
            Ok(())
        }
        Commands::Accounts(AccountsCmd::List { site }) => {
            let vault = FileVault::new(&home).map_err(|e| fail(&e, json))?;
            let filter = site.as_deref().map(Site::new);
            let keys = vault.list(filter.as_ref()).map_err(|e| fail(&e, json))?;
            if json {
                let rows: Vec<_> = keys
                    .iter()
                    .map(|k| serde_json::json!({ "site": k.site, "name": k.name }))
                    .collect();
                emit_raw(&serde_json::json!({ "accounts": rows }));
            } else {
                for k in keys {
                    human_line(format!("{}/{}", k.site, k.name));
                }
            }
            Ok(())
        }
        Commands::Accounts(AccountsCmd::Delete { site, yes }) => {
            if !yes {
                eprintln!("pass --yes to delete {site}/{account}");
                return Err(2);
            }
            let vault = FileVault::new(&home).map_err(|e| fail(&e, json))?;
            let key = AccountKey::new(&site, &account);
            vault.delete(&key).map_err(|e| fail(&e, json))?;
            if json {
                emit_raw(&serde_json::json!({ "deleted": { "site": site, "name": account } }));
            } else {
                eprintln!("deleted {site}/{account}");
            }
            Ok(())
        }
        other => {
            let client = make_client(&home).map_err(|e| fail(&e, json))?;
            dispatch(client, &home, other, json, account, deadline).await
        }
    }
}

async fn dispatch(
    client: Client,
    home: &std::path::Path,
    cmd: Commands,
    json: bool,
    account: String,
    deadline: Deadline,
) -> Result<(), i32> {
    match cmd {
        Commands::Media(MediaCmd::List { site, limit }) => {
            let key = AccountKey::new(&site, &account);
            match client.media(&key, MediaQuery { limit }, deadline).await {
                Ok(reply) => {
                    if json {
                        emit_raw(&serde_json::to_value(&reply).expect("json"));
                    } else {
                        for media in &reply.media {
                            human_line(media_line(media));
                        }
                    }
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            }
        }
        Commands::Pages(PagesCmd::Accounts { site }) => {
            let key = AccountKey::new(&site, &account);
            match client.pages(&key, deadline).await {
                Ok(reply) => {
                    if json {
                        emit_raw(&serde_json::to_value(&reply).expect("json"));
                    } else {
                        for page in &reply.pages {
                            let name = page.name.as_deref().unwrap_or("-");
                            let tasks = if page.tasks.is_empty() {
                                "-".to_string()
                            } else {
                                page.tasks.join(",")
                            };
                            human_line(format!("{} {} tasks={tasks}", page.id, name));
                        }
                    }
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            }
        }
        Commands::Ads(AdsCmd::Accounts { site }) => {
            let key = AccountKey::new(&site, &account);
            match client.ad_accounts(&key, deadline).await {
                Ok(reply) => {
                    if json {
                        emit_raw(&serde_json::to_value(&reply).expect("json"));
                    } else {
                        for ad_account in &reply.accounts {
                            human_line(ad_account_line(ad_account));
                        }
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Ads(AdsCmd::Status {
            site,
            entity,
            id,
            wait,
        }) => {
            let request =
                build_ad_review_status_request(&site, &entity, &id).map_err(|e| fail(&e, json))?;
            one_ad_review_status(
                &client,
                &AccountKey::new(&site, &account),
                request,
                wait,
                deadline,
                json,
            )
            .await
        }
        Commands::Ads(AdsCmd::PreviewCreative {
            site,
            creative_id,
            ad_format,
            output,
        }) => {
            let request = build_creative_preview_request(&site, &creative_id, &ad_format)
                .map_err(|e| fail(&e, json))?;
            one_creative_preview(
                &client,
                &AccountKey::new(&site, &account),
                request,
                output,
                deadline,
                json,
            )
            .await
        }
        Commands::Ads(AdsCmd::UploadImage {
            site,
            ad_account,
            file,
        }) => {
            // The core library intentionally receives opaque bytes rather
            // than a host path. This is the sole filesystem boundary, and it
            // turns an unreadable local file into a stable field-level error
            // without leaking user or CI directory names.
            let filename = file
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .ok_or_else(|| fail(&ads_input_error(&site, "invalid_image_filename"), json))?;
            let bytes = std::fs::read(file)
                .map_err(|_| fail(&ads_input_error(&site, "image_file_unreadable"), json))?;
            let request = build_upload_ad_image_request(&site, ad_account, filename, bytes)
                .map_err(|e| fail(&e, json))?;
            one_image_upload(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        Commands::Ads(AdsCmd::CreateLinkCreative {
            site,
            ad_account,
            name,
            page_id,
            image_hash,
            message,
            headline,
            destination_url,
            call_to_action,
        }) => {
            let request = build_link_ad_creative_request(
                &site,
                LinkCreativeOptions {
                    ad_account,
                    name,
                    page_id,
                    image_hash,
                    message,
                    headline,
                    destination_url,
                    call_to_action,
                },
            )
            .map_err(|e| fail(&e, json))?;
            one_link_creative(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        Commands::Ads(AdsCmd::CreateCampaign {
            site,
            ad_account,
            name,
            objective,
            special_ad_categories,
        }) => {
            let request = build_paused_campaign_request(
                &site,
                ad_account,
                &name,
                &objective,
                &special_ad_categories,
            )
            .map_err(|e| fail(&e, json))?;
            one_paused_create(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        Commands::Ads(AdsCmd::CreateAdset {
            site,
            ad_account,
            name,
            campaign_id,
            daily_budget,
            bid_strategy,
            billing_event,
            optimization_goal,
            targeting_file,
        }) => {
            // A file avoids shell-escaping a nested targeting object and
            // makes the exact audience specification reviewable before any
            // write. Do not put the file path in an error: CI paths and home
            // directories add no actionable operator information.
            let targeting = std::fs::read_to_string(targeting_file).map_err(|_| {
                fail(
                    &Error::InvalidQuery {
                        site: Site::new(&site),
                        reason: "targeting_file_unreadable".into(),
                    },
                    json,
                )
            })?;
            let request = build_paused_adset_request(
                &site,
                PausedAdsetOptions {
                    ad_account,
                    name,
                    campaign_id,
                    daily_budget,
                    bid_strategy,
                    billing_event,
                    optimization_goal,
                    targeting,
                },
            )
            .map_err(|e| fail(&e, json))?;
            one_paused_create(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        Commands::Ads(AdsCmd::CreateAd {
            site,
            ad_account,
            name,
            adset_id,
            creative_id,
        }) => {
            let request =
                build_paused_ad_request(&site, ad_account, &name, &adset_id, &creative_id)
                    .map_err(|e| fail(&e, json))?;
            one_paused_create(
                &client,
                &AccountKey::new(&site, &account),
                request,
                deadline,
                json,
            )
            .await
        }
        Commands::Ads(AdsCmd::ValidateDraft { site, manifest }) => {
            let manifest = read_draft_manifest(&site, &manifest).map_err(|e| fail(&e, json))?;
            match client.validate_paused_draft(&Site::new(&site), &manifest) {
                Ok((account_id, fingerprint)) => {
                    if json {
                        emit_raw(&serde_json::json!({
                            "site": site,
                            "valid": true,
                            "account_id": account_id,
                            "fingerprint": fingerprint,
                        }));
                    } else {
                        human_line(format!("{site} manifest valid; {account_id}"));
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Ads(AdsCmd::CreateDraft {
            site,
            manifest,
            state,
        }) => {
            let manifest = read_draft_manifest(&site, &manifest).map_err(|e| fail(&e, json))?;
            let image = read_draft_image(&site, &manifest).map_err(|e| fail(&e, json))?;
            let key = AccountKey::new(&site, &account);
            let result = client
                .run_paused_draft(RunPausedDraft {
                    key: &key,
                    manifest: &manifest,
                    image: image.as_ref(),
                    store: &FileDraftStore,
                    state_path: &state,
                    resume: false,
                    deadline,
                })
                .await;
            match result {
                Ok(reply) => {
                    emit_draft_result(&reply, json);
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Ads(AdsCmd::ResumeDraft {
            site,
            manifest,
            state,
        }) => {
            let manifest = read_draft_manifest(&site, &manifest).map_err(|e| fail(&e, json))?;
            let image = read_draft_image(&site, &manifest).map_err(|e| fail(&e, json))?;
            let key = AccountKey::new(&site, &account);
            let result = client
                .run_paused_draft(RunPausedDraft {
                    key: &key,
                    manifest: &manifest,
                    image: image.as_ref(),
                    store: &FileDraftStore,
                    state_path: &state,
                    resume: true,
                    deadline,
                })
                .await;
            match result {
                Ok(reply) => {
                    emit_draft_result(&reply, json);
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Ads(AdsCmd::StatusDraft { site, state, wait }) => {
            let key = AccountKey::new(&site, &account);
            // `--wait` polls the read path only, bounded by --deadline; the
            // bounded-wait contract (last observed state, never a raw
            // timeout) lives in the Client and is tested there.
            let reply = if wait {
                client
                    .paused_draft_status_wait(
                        &key,
                        &FileDraftStore,
                        &state,
                        deadline,
                        std::time::Duration::from_secs(2),
                    )
                    .await
            } else {
                client
                    .paused_draft_status(&key, &FileDraftStore, &state, deadline)
                    .await
            }
            .map_err(|e| fail(&e, json))?;
            emit_draft_status(&reply, json);
            Ok(())
        }
        Commands::Ads(AdsCmd::AdoptDraftStep {
            site,
            state,
            step,
            id,
        }) => {
            let step =
                DraftStep::from_str(&step).map_err(|e| fail(&ads_input_error(&site, e), json))?;
            let result = client
                .adopt_paused_draft_step(
                    &AccountKey::new(&site, &account),
                    &FileDraftStore,
                    &state,
                    step,
                    id,
                    deadline,
                )
                .await;
            match result {
                Ok(reply) => {
                    emit_draft_result(&reply, json);
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Capabilities { site } => {
            if let Some(s) = site {
                let site = Site::new(s);
                match client.registry().capabilities_for(&site) {
                    Some(caps) => {
                        let names: Vec<&str> = caps.iter().map(|c| c.as_str()).collect();
                        if json {
                            emit_raw(&serde_json::json!({ site.as_str(): names }));
                        } else {
                            human_line(format!("{}: {}", site, names.join(", ")));
                        }
                    }
                    None => {
                        return Err(fail(&Error::UnknownSite(site), json));
                    }
                }
            } else if json {
                emit_raw(&client.registry().capabilities_json());
            } else {
                human_line(client.registry().capabilities_json().to_string());
            }
            Ok(())
        }
        Commands::Whoami { site } => {
            let key = AccountKey::new(&site, &account);
            match client.whoami(&key).await {
                Ok(w) => {
                    emit_ok(&w, json, || {
                        format!("{} {} {}", w.site, w.id, w.handle.as_deref().unwrap_or(""))
                    });
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Insights {
            site,
            from,
            to,
            level,
            metrics,
            attribution,
            ad_account,
            entity_ids,
            breakdowns,
        } => {
            let query = build_insights_query(
                &site,
                &from,
                &to,
                &level,
                &metrics,
                &attribution,
                InsightsOptions {
                    ad_account,
                    entity_ids,
                    breakdowns,
                },
            )
            .map_err(|e| fail(&e, json))?;
            let key = AccountKey::new(&site, &account);
            match client.insights(&key, query, deadline).await {
                Ok(reply) => {
                    if json {
                        emit_raw(&serde_json::to_value(&reply).expect("json"));
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
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Auth {
            site,
            token,
            code,
            listen,
            password,
        } => {
            if listen {
                eprintln!("--listen is not implemented in this scaffold; paste the code instead");
                return Err(2);
            }
            let key = AccountKey::new(&site, &account);
            let result = if let Some(password) = password {
                if token.is_some() || code.is_some() {
                    eprintln!("--password cannot be combined with --token or --code");
                    return Err(2);
                }
                let secret = if password.is_empty() {
                    if !io::stdin().is_terminal() {
                        eprintln!("then: postkit auth {site} --account <handle> --password <app-password>");
                        return Ok(());
                    }
                    // Echo suppressed: an app password is a full
                    // account-access credential and must not land in the
                    // terminal scrollback, capture panes or screen-shares.
                    rpassword::prompt_password("app password: ").map_err(|_| 5)?
                } else {
                    password
                };
                client
                    .auth_finish(
                        &key,
                        AuthReply::AppPassword {
                            identifier: account.clone(),
                            secret,
                            pds: None,
                        },
                    )
                    .await
            } else if let Some(token) = token {
                if code.is_some() {
                    eprintln!("--token and --code are mutually exclusive");
                    return Err(2);
                }
                client.put_token(&key, &token).await
            } else if let Some(code) = code {
                let code = extract_code(&code).map_err(|e| fail(&e, json))?;
                client.auth_finish(&key, AuthReply::Pasted { code }).await
            } else {
                match client.auth_start(&Site::new(&site)).await {
                    Ok(start) => match start {
                        postkit::AuthStart::Browser {
                            authorize_url,
                            state,
                        } => {
                            eprintln!("open: {authorize_url}");
                            if !io::stdin().is_terminal() {
                                eprintln!("then: postkit auth {site} --code <code>");
                                return Ok(());
                            }
                            eprintln!("paste the redirected URL or code, then Enter");
                            let mut line = String::new();
                            io::stdin().lock().read_line(&mut line).map_err(|_| 5)?;
                            verify_state(&state, &line).map_err(|e| fail(&e, json))?;
                            let code = extract_code(&line).map_err(|e| fail(&e, json))?;
                            client.auth_finish(&key, AuthReply::Pasted { code }).await
                        }
                        postkit::AuthStart::PasteInstructions { hint } => {
                            eprintln!("{hint}");
                            if !io::stdin().is_terminal() {
                                eprintln!("then: postkit auth {site} --account <handle> --password <app-password>");
                                return Ok(());
                            }
                            // Echo suppressed for the same reason as the
                            // --password prompt above.
                            let secret =
                                rpassword::prompt_password("app password: ").map_err(|_| 5)?;
                            client
                                .auth_finish(
                                    &key,
                                    AuthReply::AppPassword {
                                        identifier: account.clone(),
                                        secret,
                                        pds: None,
                                    },
                                )
                                .await
                        }
                        postkit::AuthStart::None => {
                            eprintln!("this site does not use OAuth; pass --token or --password");
                            return Ok(());
                        }
                    },
                    Err(e) => Err(e),
                }
            };
            match result {
                Ok(w) => {
                    emit_ok(&w, json, || {
                        // Say where credentials landed so a misdirected
                        // vault is visible immediately (issue 014).
                        format!(
                            "ok {} {} (vault: {})",
                            w.site,
                            w.handle.as_deref().unwrap_or(&w.id),
                            home.display()
                        )
                    });
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Post {
            site,
            text,
            to,
            param,
            idempotency,
            stdin,
            dry_run,
            image,
            alt,
        } => {
            if let Some(e) = dry_run_conflict(dry_run, idempotency.as_deref(), text.len()) {
                return Err(fail(&e, json));
            }
            // Image exclusions fire before any parsing or I/O: each
            // combination names a wire contract postkit has not verified
            // (015 D4), and half-honoring it is the 022 failure mode.
            if let Some(err) = image_input_conflict(image.len(), text.len(), dry_run, &alt, &param)
            {
                return Err(fail(&invalid_post(&site_or_to(&site, &to), err), json));
            }
            // 025: --stdin is a complete request in itself; any other
            // content-carrying flag would be silently ignored by the stdin
            // branch — refuse the combination before stdin is even read.
            if let Some(e) = stdin_conflict(
                stdin,
                &text,
                &image,
                &alt,
                to.as_deref(),
                &param,
                site.as_deref(),
            ) {
                return Err(fail(&e, json));
            }
            if stdin {
                let mut buf = String::new();
                io::stdin().read_to_string(&mut buf).map_err(|_| 5)?;
                let req: PostRequest = serde_json::from_str(&buf).map_err(|e| {
                    fail(
                        &Error::InvalidPost {
                            site: Site::new(""),
                            reason: format!("json:{e}"),
                            limit: None,
                        },
                        json,
                    )
                })?;
                let (key, mut intent) = req.into_key_intent().map_err(|e| fail(&e, json))?;
                intent.idempotency_key = idempotency;
                if dry_run && matches!(intent.body, Body::Image { .. } | Body::Carousel { .. }) {
                    return Err(fail(
                        &invalid_post(key.site.as_str(), "dry_run_image_unsupported"),
                        json,
                    ));
                }
                // --stdin has no dry_run field of its own; the CLI flag is
                // the single switch, so both input paths stay in parity.
                if dry_run {
                    return one_probe(&client, &key, intent, deadline, json).await;
                }
                return one_post(&client, &key, intent, deadline, json).await;
            }
            // With --image the caption is optional (zero or one --text);
            // without it the existing text rules apply unchanged.
            let texts = if !image.is_empty() {
                if text.iter().any(|t| t == "-") {
                    return Err(fail(
                        &invalid_post(&site_or_to(&site, &to), "image_chain_unsupported"),
                        json,
                    ));
                }
                text
            } else {
                resolve_texts(text)?
            };
            let params = parse_params(&param, json)?;
            let sites = collect_post_sites(site.as_deref(), to.as_deref())?;
            if texts.len() > 1 {
                if let Some(bad) = chain_blocked_site(&sites) {
                    return Err(fail(
                        &Error::InvalidPost {
                            site: Site::new(bad),
                            reason: "thread_unsupported".into(),
                            limit: None,
                        },
                        json,
                    ));
                }
                for t in &texts {
                    validate_text(t).map_err(|e| fail(&e, json))?;
                }
                return chain_threads(
                    &client,
                    &account,
                    &texts,
                    params,
                    idempotency,
                    deadline,
                    json,
                )
                .await;
            }
            // Option: Some = caption (image) or the post text; None is
            // only possible with --image and zero --text flags.
            let text = texts.into_iter().next();
            if let Some(to) = to {
                let mut results = Vec::new();
                let mut code = 0i32;
                for raw in to.split(',') {
                    let s = raw.trim();
                    if s.is_empty() {
                        continue;
                    }
                    let key = AccountKey::new(s, &account);
                    // Bytes are cloned per target: each connector gets its
                    // own copy and a per-target failure (e.g. a URL image
                    // on Bluesky) is isolated in the fan-out results.
                    let body = build_post_body(&image, text.clone(), &alt, s)
                        .map_err(|e| fail(&e, json))?;
                    let intent = Intent {
                        site: Site::new(s),
                        params: params.clone(),
                        body,
                        idempotency_key: idempotency.clone(),
                    };
                    let attempt = if dry_run {
                        client
                            .probe(&key, intent, deadline)
                            .await
                            .map(|p| serde_json::to_value(&p).unwrap())
                    } else {
                        client
                            .publish(&key, intent, deadline)
                            .await
                            .map(|o| serde_json::to_value(&o).unwrap())
                    };
                    match attempt {
                        Ok(v) => results.push(v),
                        Err(e) => {
                            if code == 0 {
                                code = e.exit_code();
                            }
                            results
                                .push(serde_json::to_value(postkit::WireError::from(&e)).unwrap());
                        }
                    }
                }
                print_results(&results, json);
                if code == 0 {
                    Ok(())
                } else {
                    Err(code)
                }
            } else {
                let site = sites.into_iter().next().expect("collect_post_sites");
                let key = AccountKey::new(&site, &account);
                let body =
                    build_post_body(&image, text, &alt, &site).map_err(|e| fail(&e, json))?;
                let intent = Intent {
                    site: Site::new(&site),
                    params,
                    body,
                    idempotency_key: idempotency,
                };
                if dry_run {
                    one_probe(&client, &key, intent, deadline, json).await
                } else {
                    one_post(&client, &key, intent, deadline, json).await
                }
            }
        }
        _ => unreachable!(),
    }
}

/// 027: `--dry-run` contradicts two other flags, and each contradiction
/// is refused loudly rather than half-honored — silently dropping part of
/// the operator's request is exactly the 022 failure mode. With
/// `--idempotency`: a probe neither consults nor records the ledger (see
/// `Client::probe`), so the pairing cannot mean anything. With a chain:
/// segment N+1 replies to segment N's *published* id, and a probe
/// publishes nothing — there is no parent to chain onto.
/// --stdin is a complete request on its own; every other content-carrying
/// flag would be silently ignored by the stdin branch (issue 025) — a
/// script could publish a body its command line does not describe. Refuse
/// them together, before stdin is even read. `--dry-run` and
/// `--idempotency` are deliberately absent here: they select behavior on
/// top of the stdin request, not content.
fn stdin_conflict(
    stdin: bool,
    text: &[String],
    images: &[String],
    alt: &str,
    to: Option<&str>,
    param: &[String],
    site: Option<&str>,
) -> Option<Error> {
    if !stdin {
        return None;
    }
    let clean = text.is_empty()
        && images.is_empty()
        && alt.is_empty()
        && to.is_none()
        && param.is_empty()
        && site.is_none();
    if clean {
        return None;
    }
    Some(Error::InvalidPost {
        site: Site::new(""),
        reason: "stdin_exclusive".into(),
        limit: None,
    })
}

fn dry_run_conflict(dry_run: bool, idempotency: Option<&str>, texts: usize) -> Option<Error> {
    if !dry_run {
        return None;
    }
    if idempotency.is_some() {
        return Some(Error::InvalidPost {
            site: Site::new(""),
            reason: "dry_run_idempotency".into(),
            limit: None,
        });
    }
    if texts > 1 {
        return Some(Error::InvalidPost {
            site: Site::new("threads"),
            reason: "dry_run_chain".into(),
            limit: None,
        });
    }
    None
}

/// Site label for pre-parse refusals: the explicit site when given,
/// otherwise the first --to target, otherwise a blank marker. The label is
/// for the operator's eyes in the error, nothing more.
fn site_or_to(site: &Option<String>, to: &Option<String>) -> String {
    site.clone()
        .or_else(|| {
            to.as_ref()
                .map(|t| t.split(',').next().unwrap_or("").trim().to_string())
        })
        .unwrap_or_default()
}

fn invalid_post(site: &str, reason: &str) -> Error {
    Error::InvalidPost {
        site: Site::new(site),
        reason: reason.into(),
        limit: None,
    }
}

/// Reject a multi-image command shape that Postkit cannot map to one honest
/// carousel. Kept pure so CLI tests prove every refusal occurs before a local
/// image read, credential lookup, or remote container create.
fn image_input_conflict(
    image_count: usize,
    text_count: usize,
    dry_run: bool,
    alt: &str,
    params: &[String],
) -> Option<&'static str> {
    if image_count == 0 {
        return None;
    }
    if text_count > 1 {
        return Some(if image_count > 1 {
            "carousel_caption_multiple"
        } else {
            "image_chain_unsupported"
        });
    }
    if dry_run {
        return Some("dry_run_image_unsupported");
    }
    if image_count > 1 && !alt.is_empty() {
        // A single generic alt string cannot truthfully describe multiple
        // slides. Reject it instead of silently dropping it while Instagram
        // carousel alt text is not a reviewed per-slide wire contract.
        return Some("carousel_alt_unsupported");
    }
    if params
        .iter()
        .any(|param| param.split('=').next().unwrap_or("") == "reply_to_id")
    {
        return Some(if image_count > 1 {
            "carousel_reply_unsupported"
        } else {
            "image_reply_unsupported"
        });
    }
    None
}

/// Build one typed body after the CLI has rejected combinations it cannot
/// represent faithfully. Repeating `--image` is a single carousel body, not
/// a loop that could accidentally create multiple visible posts.
fn build_post_body(
    images: &[String],
    text: Option<String>,
    alt: &str,
    site: &str,
) -> Result<Body, Error> {
    match images {
        [] => Ok(Body::Text {
            text: text.expect("resolve_texts guarantees one when no image exists"),
        }),
        [image] => Ok(Body::Image {
            text,
            image: resolve_image(image, site)?,
            alt: alt.to_string(),
        }),
        images if images.len() > MAX_CAROUSEL_IMAGES => Err(Error::InvalidPost {
            site: Site::new(site),
            reason: "carousel_too_many_images".into(),
            // Check the cardinality before resolving a local filename. A
            // malformed 11-image command should not touch eleven files only
            // to report a platform limit that was already knowable.
            limit: Some(MAX_CAROUSEL_IMAGES as u32),
        }),
        images => Ok(Body::Carousel {
            text,
            images: images
                .iter()
                .map(|image| resolve_image(image, site))
                .collect::<Result<Vec<_>, _>>()?,
        }),
    }
}

/// Resolve --image to the kernel's dual form: an https URL passes through
/// (Threads and Instagram crawl it), anything else is a local file read here
/// — the sole filesystem boundary — into bytes + bare basename. Read errors
/// never echo the operator's path.
fn resolve_image(image: &str, site: &str) -> Result<Image, Error> {
    // Anything scheme-shaped is a URL attempt, not a filename: `http://…`
    // must die as "must be https", never as a confusing unreadable file.
    if image.contains("://") {
        let image = Image::Url(image.to_string());
        image.validate().map_err(|r| invalid_post(site, &r))?;
        return Ok(image);
    }
    let path = std::path::Path::new(image);
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .ok_or_else(|| invalid_post(site, "invalid_image_filename"))?;
    let bytes = std::fs::read(path).map_err(|_| invalid_post(site, "image_file_unreadable"))?;
    Ok(Image::Bytes { filename, bytes })
}

fn resolve_texts(text: Vec<String>) -> Result<Vec<String>, i32> {
    if text.is_empty() {
        eprintln!("--text is required (or --stdin)");
        return Err(2);
    }
    if text.iter().any(|t| t == "-") {
        if text.len() != 1 {
            eprintln!("--text - cannot be combined with other --text");
            return Err(2);
        }
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).map_err(|_| 5)?;
        return Ok(vec![buf]);
    }
    Ok(text)
}

fn collect_post_sites(site: Option<&str>, to: Option<&str>) -> Result<Vec<String>, i32> {
    if let Some(to) = to {
        let sites: Vec<String> = to
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        if sites.is_empty() {
            eprintln!("site or --to is required");
            return Err(2);
        }
        Ok(sites)
    } else if let Some(site) = site {
        Ok(vec![site.to_string()])
    } else {
        eprintln!("site or --to is required");
        Err(2)
    }
}

fn chain_blocked_site(sites: &[String]) -> Option<&str> {
    sites.iter().map(String::as_str).find(|s| *s != "threads")
}

fn with_reply_to(params: &serde_json::Value, id: &str) -> serde_json::Value {
    let mut p = params.clone();
    match p {
        serde_json::Value::Object(ref mut m) => {
            m.insert(
                "reply_to_id".into(),
                serde_json::Value::String(id.to_string()),
            );
        }
        _ => {
            p = serde_json::json!({ "reply_to_id": id });
        }
    }
    p
}

async fn chain_threads(
    client: &Client,
    account: &str,
    texts: &[String],
    params: serde_json::Value,
    idempotency: Option<String>,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    let key = AccountKey::new("threads", account);
    let mut results = Vec::new();
    let mut prev: Option<String> = None;
    let mut code = 0i32;
    for (i, text) in texts.iter().enumerate() {
        let p = if let Some(id) = prev.as_deref() {
            with_reply_to(&params, id)
        } else {
            params.clone()
        };
        let intent = Intent {
            site: Site::new("threads"),
            params: p,
            body: Body::Text { text: text.clone() },
            idempotency_key: if i == 0 { idempotency.clone() } else { None },
        };
        match client.publish(&key, intent, deadline).await {
            Ok(o) => {
                prev = o.id.clone();
                if prev.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
                    let e = Error::Platform {
                        site: Site::new("threads"),
                        code: "missing_id".into(),
                        message: "Graph create returned no id".into(),
                    };
                    code = e.exit_code();
                    results.push(serde_json::to_value(postkit::WireError::from(&e)).unwrap());
                    break;
                }
                results.push(serde_json::to_value(&o).unwrap());
            }
            Err(e) => {
                code = e.exit_code();
                results.push(serde_json::to_value(postkit::WireError::from(&e)).unwrap());
                break;
            }
        }
    }
    print_results(&results, json);
    if code != 0 {
        if !json {
            eprintln!("published {} then failed", results.len().saturating_sub(1));
        }
        return Err(code);
    }
    Ok(())
}

/// Probe twin of `one_post`. The human line states the contract in plain
/// words — nothing was published — because a bare `site id` here would
/// read exactly like a successful post.
async fn one_probe(
    client: &Client,
    key: &AccountKey,
    intent: Intent,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    match client.probe(key, intent, deadline).await {
        Ok(p) => {
            emit_ok(&p, json, || {
                format!(
                    "{} {} dry-run (nothing published, expires in {}h)",
                    p.site, p.container_id, p.expires_in_hours
                )
            });
            Ok(())
        }
        Err(e) => Err(fail(&e, json)),
    }
}

async fn one_post(
    client: &Client,
    key: &AccountKey,
    intent: Intent,
    deadline: Deadline,
    json: bool,
) -> Result<(), i32> {
    match client.publish(key, intent, deadline).await {
        Ok(o) => {
            emit_ok(&o, json, || {
                format!(
                    "{} {}",
                    o.id.as_deref().unwrap_or("-"),
                    o.url.as_deref().unwrap_or("")
                )
            });
            Ok(())
        }
        Err(e) => Err(fail(&e, json)),
    }
}

/// Paused ads have their own result type instead of being rendered as social
/// posts. The status is shown prominently so an operator can verify the
/// safety invariant in scripts and terminal output alike.
async fn one_paused_create(
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
async fn one_image_upload(
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
async fn one_link_creative(
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
async fn one_creative_preview(
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
async fn one_ad_review_status(
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
fn emit_ad_review_status(status: &AdReviewStatus, review: &str) {
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

/// `--json` prints `{ "results": [...] }`; human mode one line per result,
/// on stderr per the output-stream contract.
fn print_results(results: &[serde_json::Value], json: bool) {
    if json {
        emit_raw(&serde_json::json!({ "results": results }));
    } else {
        for r in results {
            human_line(result_line(r));
        }
    }
}

fn result_line(v: &serde_json::Value) -> String {
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        let site = v.get("site").and_then(|s| s.as_str()).unwrap_or("-");
        let reason = v.get("reason").and_then(|r| r.as_str()).unwrap_or("");
        return format!("{err} {site} {reason}").trim_end().into();
    }
    let site = v.get("site").and_then(|s| s.as_str()).unwrap_or("-");
    // probe rows carry container_id, not id — never render them like posts
    if let Some(c) = v.get("container_id").and_then(|i| i.as_str()) {
        return format!("{site} {c} dry-run");
    }
    let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("-");
    let url = v.get("url").and_then(|u| u.as_str()).unwrap_or("");
    format!("{site} {id} {url}").trim_end().into()
}

fn parse_params(param: &[String], json: bool) -> Result<serde_json::Value, i32> {
    let mut map = serde_json::Map::new();
    for p in param {
        let Some((k, v)) = p.split_once('=') else {
            return Err(fail(
                &Error::InvalidPost {
                    site: Site::new(""),
                    reason: format!("param_not_k_eq_v:{p}"),
                    limit: None,
                },
                json,
            ));
        };
        map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
    }
    Ok(serde_json::Value::Object(map))
}

fn make_client(home: &std::path::Path) -> Result<Client, Error> {
    let mut registry = Registry::new();
    registry.register(Arc::new(postkit::connectors::threads::Threads::new()?));
    registry.register(Arc::new(postkit::connectors::bluesky::Bluesky::new()?));
    registry.register(Arc::new(postkit::connectors::meta_ads::MetaAds::new()?));
    registry.register(Arc::new(
        postkit::connectors::facebook_pages::FacebookPages::new()?,
    ));
    registry.register(Arc::new(postkit::connectors::instagram::Instagram::new()?));
    let vault = Arc::new(FileVault::new(home)?);
    let apps = Arc::new(FileAppStore::new(home)?);
    Ok(Client::new(registry, vault, apps))
}

/// The optional, additive parts of an insights query. Keeping them together
/// prevents a growing CLI surface from turning the parser into a brittle,
/// positional argument list.
struct InsightsOptions {
    ad_account: Option<String>,
    entity_ids: Vec<String>,
    breakdowns: String,
}

/// Common builder error shape for advertising input. These errors name only
/// the invalid field, never echo targeting JSON or an operator's local path.
fn ads_input_error(site: &str, reason: impl Into<String>) -> Error {
    Error::InvalidQuery {
        site: Site::new(site),
        reason: reason.into(),
    }
}

/// Parse the reviewed manifest at the operator's chosen path. Read errors
/// never echo the path; the operator just chose it. Serde's own message is
/// kept (it names the offending field) but truncated to one line so a
/// formatting accident cannot flood the terminal.
fn read_draft_manifest(site: &str, path: &Path) -> Result<PausedDraftManifest, Error> {
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
fn read_draft_image(
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
fn emit_draft_result(result: &PausedDraftResult, json: bool) {
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

fn emit_draft_status(reply: &DraftStatusReply, json: bool) {
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

fn build_paused_campaign_request(
    site: &str,
    ad_account: Option<String>,
    name: &str,
    objective: &str,
    special_ad_categories: &str,
) -> Result<CreatePausedAdRequest, Error> {
    let objective =
        CampaignObjective::from_str(objective).map_err(|reason| ads_input_error(site, reason))?;
    let request = CreatePausedAdRequest {
        account: ad_account,
        create: PausedAdCreate::Campaign(PausedCampaign {
            name: name.into(),
            objective,
            special_ad_categories: special_ad_categories
                .split(',')
                .map(str::trim)
                .filter(|category| !category.is_empty())
                .map(str::to_owned)
                .collect(),
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
fn build_upload_ad_image_request(
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
struct LinkCreativeOptions {
    ad_account: Option<String>,
    name: String,
    page_id: String,
    image_hash: String,
    message: String,
    headline: String,
    destination_url: String,
    call_to_action: String,
}

fn build_link_ad_creative_request(
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
fn build_creative_preview_request(
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
fn build_ad_review_status_request(
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
fn write_preview_output(path: &Path, body: &str) -> std::io::Result<()> {
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

fn preview_output_reason(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::AlreadyExists => "preview_output_exists",
        _ => "preview_output_unwritable",
    }
}

fn build_paused_adset_request(
    site: &str,
    options: PausedAdsetOptions,
) -> Result<CreatePausedAdRequest, Error> {
    let bid_strategy = BidStrategy::from_str(&options.bid_strategy)
        .map_err(|reason| ads_input_error(site, reason))?;
    let targeting = serde_json::from_str(&options.targeting)
        .map_err(|_| ads_input_error(site, "bad_targeting_json"))?;
    let request = CreatePausedAdRequest {
        account: options.ad_account,
        create: PausedAdCreate::Adset(PausedAdset {
            name: options.name,
            campaign_id: options.campaign_id,
            daily_budget: options.daily_budget,
            bid_strategy,
            billing_event: options.billing_event,
            optimization_goal: options.optimization_goal,
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
struct PausedAdsetOptions {
    ad_account: Option<String>,
    name: String,
    campaign_id: String,
    daily_budget: u64,
    bid_strategy: String,
    billing_event: String,
    optimization_goal: String,
    targeting: String,
}

fn build_paused_ad_request(
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
fn build_insights_query(
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
    Ok(InsightsQuery {
        level,
        metrics: parsed,
        attribution,
        range,
        account: options.ad_account,
        entity_ids: options.entity_ids,
        breakdowns: parsed_breakdowns,
    })
}

/// One human-mode row: `date level entity dimension=v metric=v …`. Keeping
/// dimensions before metrics prevents a country/platform label from looking
/// like a number that callers may sum across rows.
fn insight_line(row: &InsightRow) -> String {
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
fn ad_account_line(account: &AdAccount) -> String {
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
fn media_line(media: &PublishedMedia) -> String {
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
fn created_ad_line(created: &CreatedAd) -> String {
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
fn uploaded_image_line(uploaded: &UploadedAdImage) -> String {
    format!(
        "image hash={} account={}",
        uploaded.hash, uploaded.account_id
    )
}

/// Make the non-delivery property visible in text output as it is in the
/// type contract: a creative alone cannot spend or enter an auction.
fn created_creative_line(created: &CreatedAdCreative) -> String {
    format!(
        "creative {} not-delivering account={}",
        created.id, created.account_id
    )
}

/// Vault home: `--home`/`POSTKIT_HOME` (clap folds the env var into the
/// flag) > `$HOME/.postkit`. Never falls back to the current directory:
/// with HOME unset (cron, systemd units, `env -i` shells) a CWD fallback
/// would silently write tokens into whatever directory the process
/// started in — possibly a checkout or a world-writable /tmp. Pure over
/// its inputs so the precedence is unit-testable without touching the
/// process environment.
fn resolve_home(
    flag_or_env: Option<PathBuf>,
    user_home: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if let Some(p) = flag_or_env {
        return Ok(p);
    }
    user_home.map(|h| h.join(".postkit")).ok_or_else(|| {
        "POSTKIT_HOME or HOME must be set to locate the vault; refusing to guess from the current directory".into()
    })
}

fn check_name(s: &str, json: bool) -> Result<(), i32> {
    if valid_name(s) {
        Ok(())
    } else {
        Err(fail(&Error::InvalidName(s.into()), json))
    }
}

fn fail(e: &Error, json: bool) -> i32 {
    emit_err(e, json);
    e.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn build_insights_query_parses_and_defaults() {
        let q = build_insights_query(
            "meta_ads",
            "2026-06-01",
            "2026-06-30",
            "campaign",
            "spend, purchases",
            "7d_click_1d_view",
            InsightsOptions {
                ad_account: Some("act_9".into()),
                entity_ids: vec!["238".into(), "239".into()],
                breakdowns: "country,age".into(),
            },
        )
        .unwrap();
        assert_eq!(q.level.as_str(), "campaign");
        assert_eq!(q.metrics, vec![Metric::Spend, Metric::Purchases]);
        assert_eq!(q.account.as_deref(), Some("act_9"));
        assert_eq!(q.entity_ids, vec!["238", "239"]);
        assert_eq!(q.breakdowns, vec![Breakdown::Country, Breakdown::Age]);
    }

    #[test]
    fn build_insights_query_rejects_bad_inputs() {
        // range errors surface via Client's validate(); parse errors here
        let cases: [(&str, &str, &str, &str); 4] = [
            (
                "campaigns",
                "spend",
                "7d_click_1d_view",
                "unknown_level:campaigns",
            ),
            (
                "campaign",
                "not_a_metric",
                "7d_click_1d_view",
                "unknown_metric:not_a_metric",
            ),
            (
                "campaign",
                "spend",
                "default",
                "unknown_attribution:default",
            ),
            ("campaign", " , ", "7d_click_1d_view", "no_metrics"),
        ];
        for (level, metrics, attribution, reason) in cases {
            let err = build_insights_query(
                "meta_ads",
                "2026-06-01",
                "2026-06-02",
                level,
                metrics,
                attribution,
                InsightsOptions {
                    ad_account: None,
                    entity_ids: vec![],
                    breakdowns: String::new(),
                },
            )
            .unwrap_err();
            assert!(
                matches!(&err, Error::InvalidQuery { reason: r, .. } if r == reason),
                "{level}/{metrics}/{attribution}: {err:?}"
            );
        }

        let err = build_insights_query(
            "meta_ads",
            "2026-06-01",
            "2026-06-02",
            "campaign",
            "roas",
            "7d_click_1d_view",
            InsightsOptions {
                ad_account: None,
                entity_ids: vec![],
                breakdowns: "country,unknown".into(),
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::InvalidQuery { reason, .. } if reason == "unknown_breakdown:unknown")
        );
    }

    #[test]
    fn insight_line_renders_row() {
        let mut metrics = serde_json::Map::new();
        metrics.insert("spend".into(), serde_json::json!(12.5));
        metrics.insert("impressions".into(), serde_json::json!(4567));
        let line = insight_line(&InsightRow {
            entity_id: "238".into(),
            level: InsightsLevel::Campaign,
            date_start: "2026-06-01".into(),
            dimensions: serde_json::Map::new(),
            metrics,
        });
        assert_eq!(line, "2026-06-01 campaign 238 impressions=4567 spend=12.5");
    }

    #[test]
    fn ad_account_line_is_copyable_and_labels_metadata() {
        let line = ad_account_line(&AdAccount {
            id: "act_123".into(),
            name: Some("Main".into()),
            currency: Some("ILS".into()),
            timezone: Some("Asia/Jerusalem".into()),
            status: Some("1".into()),
        });
        assert_eq!(
            line,
            "act_123 name=Main currency=ILS timezone=Asia/Jerusalem status=1"
        );
    }

    #[test]
    fn ads_accounts_command_parses() {
        let cli = Cli::try_parse_from(["postkit", "ads", "accounts", "meta_ads"]).unwrap();
        assert!(
            matches!(cli.command, Commands::Ads(AdsCmd::Accounts { site }) if site == "meta_ads")
        );
    }

    #[test]
    fn pages_accounts_command_parses_as_a_separate_read_surface() {
        let cli = Cli::try_parse_from(["postkit", "pages", "accounts", "facebook_pages"]).unwrap();
        assert!(
            matches!(cli.command, Commands::Pages(PagesCmd::Accounts { site }) if site == "facebook_pages")
        );
    }

    #[test]
    fn media_list_command_parses_with_an_explicit_bounded_limit() {
        let cli =
            Cli::try_parse_from(["postkit", "media", "list", "instagram", "--limit", "2"]).unwrap();
        assert!(
            matches!(cli.command, Commands::Media(MediaCmd::List { site, limit }) if site == "instagram" && limit == 2)
        );
    }

    #[test]
    fn ads_status_command_is_closed_and_opt_in_for_bounded_waiting() {
        let cli = Cli::try_parse_from([
            "postkit", "ads", "status", "meta_ads", "--entity", "ad", "--id", "700", "--wait",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Ads(AdsCmd::Status { site, entity, id, wait })
                if site == "meta_ads" && entity == "ad" && id == "700" && wait
        ));

        let request = build_ad_review_status_request("meta_ads", "adset", "700").unwrap();
        assert_eq!(request.entity, AdEntity::Adset);
        let bad_entity = build_ad_review_status_request("meta_ads", "creative", "700").unwrap_err();
        assert!(
            matches!(bad_entity, Error::InvalidQuery { reason, .. } if reason == "unknown_ad_entity:creative")
        );
        let bad_id = build_ad_review_status_request("meta_ads", "ad", "ad-700").unwrap_err();
        assert!(
            matches!(bad_id, Error::InvalidQuery { reason, .. } if reason == "bad_ad_entity_id:ad-700")
        );

        let pending = AdReviewStatus {
            site: Site::new("meta_ads"),
            entity: AdEntity::Ad,
            id: "700".into(),
            name: Some("Paused validation".into()),
            configured_status: "PAUSED".into(),
            effective_status: "PENDING_REVIEW".into(),
            issues: vec![],
        };
        assert!(pending.is_pending_review());
        let wire = serde_json::to_value(AdReviewWait::PendingReview(pending)).unwrap();
        assert_eq!(wire["review"], "pending_review");
        assert_eq!(wire["status"]["configured_status"], "PAUSED");
    }

    #[test]
    fn creative_preview_command_requires_a_closed_format_and_new_output_file() {
        let preview = Cli::try_parse_from([
            "postkit",
            "ads",
            "preview-creative",
            "meta_ads",
            "--creative-id",
            "500",
            "--ad-format",
            "desktop_feed_standard",
            "--output",
            "preview.html",
        ])
        .unwrap();
        assert!(matches!(
            preview.command,
            Commands::Ads(AdsCmd::PreviewCreative { site, creative_id, ad_format, output })
                if site == "meta_ads" && creative_id == "500" && ad_format == "desktop_feed_standard" && output == Path::new("preview.html")
        ));

        let missing_output = Cli::try_parse_from([
            "postkit",
            "ads",
            "preview-creative",
            "meta_ads",
            "--creative-id",
            "500",
            "--ad-format",
            "desktop_feed_standard",
        ]);
        assert!(missing_output.is_err());

        let request =
            build_creative_preview_request("meta_ads", "500", "mobile_feed_standard").unwrap();
        assert_eq!(request.ad_format, AdPreviewFormat::MobileFeedStandard);
        let bad_format =
            build_creative_preview_request("meta_ads", "500", "instagram_standard").unwrap_err();
        assert!(
            matches!(bad_format, Error::InvalidQuery { reason, .. } if reason == "unknown_ad_preview_format:instagram_standard")
        );

        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("preview.html");
        write_preview_output(&output, "<iframe src=\"https://meta.test\"></iframe>").unwrap();
        assert_eq!(
            std::fs::read_to_string(&output).unwrap(),
            "<iframe src=\"https://meta.test\"></iframe>"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&output).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        let exists = write_preview_output(&output, "different").unwrap_err();
        assert_eq!(preview_output_reason(&exists), "preview_output_exists");
    }

    #[test]
    fn image_link_creative_commands_require_explicit_non_delivery_inputs() {
        let upload = Cli::try_parse_from([
            "postkit",
            "ads",
            "upload-image",
            "meta_ads",
            "--file",
            "hero.png",
        ])
        .unwrap();
        assert!(matches!(
            upload.command,
            Commands::Ads(AdsCmd::UploadImage { site, file, .. })
                if site == "meta_ads" && file.file_name().and_then(|name| name.to_str()) == Some("hero.png")
        ));

        let missing_cta = Cli::try_parse_from([
            "postkit",
            "ads",
            "create-link-creative",
            "meta_ads",
            "--name",
            "Hero",
            "--page-id",
            "456",
            "--image-hash",
            "hash-1",
            "--message",
            "A clear benefit",
            "--headline",
            "Learn more",
            "--destination-url",
            "https://example.com/offer",
        ]);
        assert!(missing_cta.is_err());

        let request = build_link_ad_creative_request(
            "meta_ads",
            LinkCreativeOptions {
                ad_account: Some("act_123".into()),
                name: "Hero".into(),
                page_id: "456".into(),
                image_hash: "hash-1".into(),
                message: "A clear benefit".into(),
                headline: "Learn more".into(),
                destination_url: "https://example.com/offer".into(),
                call_to_action: "learn_more".into(),
            },
        )
        .unwrap();
        assert_eq!(request.creative.call_to_action, LinkCallToAction::LearnMore);

        let invalid = build_link_ad_creative_request(
            "meta_ads",
            LinkCreativeOptions {
                ad_account: None,
                name: "Hero".into(),
                page_id: "456".into(),
                image_hash: "hash-1".into(),
                message: "A clear benefit".into(),
                headline: "Learn more".into(),
                destination_url: "http://example.com/offer".into(),
                call_to_action: "shop_now".into(),
            },
        )
        .unwrap_err();
        // CTA parsing happens before URL validation, so a caller gets one
        // precise, local field correction at a time rather than Graph's
        // combined form error after a remote write.
        assert!(
            matches!(invalid, Error::InvalidQuery { reason, .. } if reason == "unknown_link_call_to_action:shop_now")
        );

        let upload = build_upload_ad_image_request(
            "meta_ads",
            None,
            "hero.png".into(),
            b"image bytes".to_vec(),
        )
        .unwrap();
        assert_eq!(
            uploaded_image_line(&UploadedAdImage {
                site: Site::new("meta_ads"),
                account_id: "act_123".into(),
                hash: upload.filename,
            }),
            "image hash=hero.png account=act_123"
        );
        assert_eq!(
            created_creative_line(&CreatedAdCreative {
                site: Site::new("meta_ads"),
                account_id: "act_123".into(),
                id: "500".into(),
            }),
            "creative 500 not-delivering account=act_123"
        );
    }

    #[test]
    fn paused_ads_commands_and_builders_require_explicit_safe_inputs() {
        let cli = Cli::try_parse_from([
            "postkit",
            "ads",
            "create-campaign",
            "meta_ads",
            "--name",
            "Paused validation",
            "--objective",
            "sales",
            "--ad-account",
            "act_123",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Ads(AdsCmd::CreateCampaign { site, ad_account: Some(account), .. })
                if site == "meta_ads" && account == "act_123"
        ));

        // A missing strategy is a parser error rather than a Meta code 100:
        // strategies such as cost cap need additional constraint fields that
        // Tier B deliberately does not infer.
        let missing_bid_strategy = Cli::try_parse_from([
            "postkit",
            "ads",
            "create-adset",
            "meta_ads",
            "--name",
            "Paused ad set",
            "--campaign-id",
            "100",
            "--daily-budget",
            "2500",
            "--billing-event",
            "IMPRESSIONS",
            "--optimization-goal",
            "REACH",
            "--targeting-file",
            "targeting.json",
        ]);
        assert!(missing_bid_strategy.is_err());

        let adset_cli = Cli::try_parse_from([
            "postkit",
            "ads",
            "create-adset",
            "meta_ads",
            "--name",
            "Paused ad set",
            "--campaign-id",
            "100",
            "--daily-budget",
            "2500",
            "--bid-strategy",
            "lowest_cost_without_cap",
            "--billing-event",
            "IMPRESSIONS",
            "--optimization-goal",
            "REACH",
            "--targeting-file",
            "targeting.json",
        ])
        .unwrap();
        assert!(matches!(
            adset_cli.command,
            Commands::Ads(AdsCmd::CreateAdset { bid_strategy, .. })
                if bid_strategy == "lowest_cost_without_cap"
        ));

        let campaign = build_paused_campaign_request(
            "meta_ads",
            Some("act_123".into()),
            "Paused validation",
            "sales",
            "HOUSING, EMPLOYMENT",
        )
        .unwrap();
        assert!(matches!(
            campaign.create,
            PausedAdCreate::Campaign(PausedCampaign { objective: CampaignObjective::Sales, special_ad_categories, .. })
                if special_ad_categories == ["HOUSING", "EMPLOYMENT"]
        ));

        let adset = build_paused_adset_request(
            "meta_ads",
            PausedAdsetOptions {
                ad_account: None,
                name: "Paused ad set".into(),
                campaign_id: "100".into(),
                daily_budget: 2500,
                bid_strategy: "lowest_cost_without_cap".into(),
                billing_event: "IMPRESSIONS".into(),
                optimization_goal: "REACH".into(),
                targeting: r#"{"geo_locations":{"countries":["MY"]}}"#.into(),
            },
        )
        .unwrap();
        assert!(matches!(adset.create, PausedAdCreate::Adset(_)));

        let bad_bid_strategy = build_paused_adset_request(
            "meta_ads",
            PausedAdsetOptions {
                ad_account: None,
                name: "x".into(),
                campaign_id: "100".into(),
                daily_budget: 1,
                bid_strategy: "cost_cap".into(),
                billing_event: "IMPRESSIONS".into(),
                optimization_goal: "REACH".into(),
                targeting: "{}".into(),
            },
        );
        assert!(
            matches!(bad_bid_strategy, Err(Error::InvalidQuery { reason, .. }) if reason == "unknown_bid_strategy:cost_cap")
        );

        for (objective, targeting, reason) in [
            ("clicks", "{}", "unknown_objective:clicks"),
            ("sales", "[]", "targeting_must_be_object"),
            ("sales", "not json", "bad_targeting_json"),
        ] {
            let result = if objective == "clicks" {
                build_paused_campaign_request("meta_ads", None, "x", objective, "")
            } else {
                build_paused_adset_request(
                    "meta_ads",
                    PausedAdsetOptions {
                        ad_account: None,
                        name: "x".into(),
                        campaign_id: "100".into(),
                        daily_budget: 1,
                        bid_strategy: "lowest_cost_without_cap".into(),
                        billing_event: "IMPRESSIONS".into(),
                        optimization_goal: "REACH".into(),
                        targeting: targeting.into(),
                    },
                )
            };
            assert!(
                matches!(result, Err(Error::InvalidQuery { reason: actual, .. }) if actual == reason),
                "{objective}/{targeting} should be {reason}"
            );
        }
    }

    #[test]
    fn paused_create_line_cannot_hide_its_status() {
        let line = created_ad_line(&CreatedAd {
            site: Site::new("meta_ads"),
            account_id: "act_123".into(),
            entity: postkit::AdEntity::Campaign,
            id: "100".into(),
            status: "PAUSED".into(),
        });
        assert_eq!(line, "campaign 100 status=PAUSED account=act_123");
    }

    #[test]
    fn insights_help_lists_all_supported_metrics() {
        // Keep the user-facing discovery text aligned with Metric::from_str.
        // This caught the Tier A+ metrics being accepted by the parser but
        // absent from `postkit insights --help`.
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("insights")
            .expect("insights command")
            .get_arguments()
            .find(|arg| arg.get_id() == "metrics")
            .and_then(|arg| arg.get_help())
            .expect("metrics help")
            .to_string();
        assert!(help.contains("purchase_value,roas"));
    }

    #[test]
    fn home_precedence_and_no_cwd_fallback() {
        let fe = Some(PathBuf::from("/flag-or-env"));
        let hm = Some(PathBuf::from("/user"));
        // --home / POSTKIT_HOME (same field via clap env) > $HOME/.postkit
        assert_eq!(
            resolve_home(fe.clone(), hm.clone()).unwrap(),
            PathBuf::from("/flag-or-env")
        );
        assert_eq!(
            resolve_home(None, hm.clone()).unwrap(),
            PathBuf::from("/user/.postkit")
        );
        // HOME unset (cron, systemd, env -i): never guess the CWD — the old
        // code silently wrote tokens into ./.postkit
        let err = resolve_home(None, None).unwrap_err();
        assert!(err.contains("POSTKIT_HOME or HOME"));
    }

    #[test]
    fn postkit_home_env_feeds_the_home_flag() {
        // clap's env feature folds POSTKIT_HOME into --home, so the var is
        // visible in --help and the manual env read stays out of run().
        std::env::set_var("POSTKIT_HOME", "/from-env");
        let cli = Cli::try_parse_from(["postkit", "whoami", "threads"]).unwrap();
        assert_eq!(cli.home, Some(PathBuf::from("/from-env")));
        std::env::remove_var("POSTKIT_HOME");
    }

    #[test]
    fn resolve_texts_requires_one() {
        assert_eq!(resolve_texts(vec![]).unwrap_err(), 2);
        assert_eq!(resolve_texts(vec!["a".into(), "-".into()]).unwrap_err(), 2);
        assert_eq!(
            resolve_texts(vec!["root".into(), "reply".into()]).unwrap(),
            vec!["root", "reply"]
        );
    }

    #[test]
    fn collect_post_sites_from_to_or_site() {
        assert_eq!(
            collect_post_sites(None, Some("threads, bluesky")).unwrap(),
            vec!["threads", "bluesky"]
        );
        assert_eq!(
            collect_post_sites(Some("threads"), None).unwrap(),
            vec!["threads"]
        );
        assert_eq!(collect_post_sites(None, None).unwrap_err(), 2);
    }

    #[test]
    fn chain_blocked_unless_all_threads() {
        assert_eq!(chain_blocked_site(&["threads".into()]), None);
        assert_eq!(
            chain_blocked_site(&["threads".into(), "threads".into()]),
            None
        );
        assert_eq!(
            chain_blocked_site(&["threads".into(), "bluesky".into()]),
            Some("bluesky")
        );
    }

    #[test]
    fn with_reply_to_overwrites_parent() {
        let p = with_reply_to(&serde_json::json!({ "reply_to_id": "old" }), "A");
        assert_eq!(p["reply_to_id"], "A");
        let p = with_reply_to(&serde_json::json!({}), "B");
        assert_eq!(p["reply_to_id"], "B");
    }

    #[test]
    fn result_line_matches_outcome_and_wire_error() {
        let ok = serde_json::json!({ "site": "threads", "id": "9", "url": "https://x/1" });
        assert_eq!(result_line(&ok), "threads 9 https://x/1");
        let no_url = serde_json::json!({ "site": "bluesky", "id": "at://x" });
        assert_eq!(result_line(&no_url), "bluesky at://x");
        let err = serde_json::json!({ "error": "invalid_post", "site": "threads", "reason": "text_too_long" });
        assert_eq!(result_line(&err), "invalid_post threads text_too_long");
        let terse = serde_json::json!({ "error": "rate_limited", "site": "threads" });
        assert_eq!(result_line(&terse), "rate_limited threads");
        // 027: probe rows must never render like posts — the id is an
        // unpublished container, and a bare "threads C" would read as one
        let probe =
            serde_json::json!({ "site": "threads", "container_id": "C", "expires_in_hours": 24 });
        assert_eq!(result_line(&probe), "threads C dry-run");
    }

    #[test]
    fn dry_run_flag_parses() {
        let cli = Cli::try_parse_from(["postkit", "post", "threads", "--text", "hi", "--dry-run"])
            .unwrap();
        match cli.command {
            Commands::Post { dry_run, .. } => assert!(dry_run),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn stdin_refuses_every_content_flag() {
        // --stdin alone is the intended shape: a complete request.
        assert!(stdin_conflict(true, &[], &[], "", None, &[], None).is_none());
        // Each content-carrying flag must refuse — any of them silently
        // ignored is a post the command line does not describe (025).
        let t = vec!["hi".to_string()];
        let image = vec!["x.png".to_string()];
        let p = vec!["reply_to_id=1".to_string()];
        for e in [
            stdin_conflict(true, &t, &[], "", None, &[], None),
            stdin_conflict(true, &[], &image, "", None, &[], None),
            stdin_conflict(true, &[], &[], "alt", None, &[], None),
            stdin_conflict(true, &[], &[], "", Some("threads"), &[], None),
            stdin_conflict(true, &[], &[], "", None, &p, None),
            stdin_conflict(true, &[], &[], "", None, &[], Some("threads")),
        ] {
            let e = e.expect("must refuse");
            assert!(matches!(&e, Error::InvalidPost { reason, .. } if reason == "stdin_exclusive"));
        }
        // Without --stdin the flags are the normal path, no opinion here.
        assert!(stdin_conflict(false, &t, &image, "alt", Some("threads"), &p, Some("x")).is_none());
    }

    #[test]
    fn dry_run_conflicts_are_refused_loudly() {
        // no dry-run: no opinion, whatever the other flags say
        assert!(dry_run_conflict(false, Some("k"), 3).is_none());
        // dry-run + idempotency: a probe never touches the ledger
        let e = dry_run_conflict(true, Some("k"), 1).unwrap();
        assert!(matches!(&e, Error::InvalidPost { reason, .. } if reason == "dry_run_idempotency"));
        // dry-run + chain: no published parent to reply to
        let e = dry_run_conflict(true, None, 2).unwrap();
        assert!(matches!(&e, Error::InvalidPost { reason, .. } if reason == "dry_run_chain"));
        // dry-run alone with one text is exactly the intended shape
        assert!(dry_run_conflict(true, None, 1).is_none());
    }

    #[test]
    fn draft_subcommands_parse_with_required_flags() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from([
            "postkit",
            "ads",
            "create-draft",
            "meta_ads",
            "--manifest",
            "launch.json",
            "--state",
            "launch.state.json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Ads(AdsCmd::CreateDraft { ref site, .. }) if site == "meta_ads"
        ));
        let cli = Cli::try_parse_from([
            "postkit",
            "ads",
            "adopt-draft-step",
            "meta_ads",
            "--state",
            "launch.state.json",
            "--step",
            "adset",
            "--id",
            "123",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Ads(AdsCmd::AdoptDraftStep { ref step, ref id, .. })
                if step == "adset" && id == "123"
        ));
        // A draft step typo is a parse-level unknown, never a silent default.
        assert!(Cli::try_parse_from([
            "postkit",
            "ads",
            "validate-draft",
            "meta_ads",
            "--manifest",
            "m.json"
        ])
        .is_ok());
    }

    #[test]
    fn draft_manifest_reader_maps_errors_without_leaking_paths() {
        let dir = std::env::temp_dir().join(format!("postkit-draft-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest.json");

        // A typo'd key is named by serde and prefixed with the site family.
        std::fs::write(&path, r#"{"version": 1, "ad_account": "act_1", "x": 0}"#).unwrap();
        let err = read_draft_manifest("meta_ads", &path).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidQuery { reason, .. } if reason.starts_with("bad_manifest:"))
        );

        // Unreadable file: stable reason, no operator path echoed.
        std::fs::remove_file(&path).unwrap();
        let err = read_draft_manifest("meta_ads", &path).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidQuery { reason, .. } if reason == "manifest_unreadable")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn draft_image_reader_tolerates_missing_file_for_resume() {
        let dir = std::env::temp_dir().join(format!("postkit-draft-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("hero.png");
        std::fs::write(&img, b"bytes").unwrap();
        let raw = r#"{"version":1,"ad_account":"act_1",
                "campaign":{"name":"n","objective":"awareness","special_ad_categories":[]},
                "adset":{"name":"n","daily_budget":100,"bid_strategy":"lowest_cost_without_cap",
                    "billing_event":"IMPRESSIONS","optimization_goal":"REACH","targeting":{}},
                "creative":{"name":"n","image_file":"IMGPATH","page_id":"1","message":"m",
                    "headline":"h","destination_url":"https://e.com/x","call_to_action":"learn_more"},
                "ad":{"name":"n"}}"#
            .replace("IMGPATH", &img.display().to_string());
        let manifest: PausedDraftManifest = serde_json::from_str(&raw).unwrap();
        let image = read_draft_image("meta_ads", &manifest).unwrap().unwrap();
        assert_eq!(image.filename, "hero.png");
        // A since-deleted local file yields None (the core decides whether
        // the bytes are still needed), not a hard error.
        std::fs::remove_file(&img).unwrap();
        assert!(read_draft_image("meta_ads", &manifest).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_image_splits_url_from_local_file() {
        let dir = std::env::temp_dir().join(format!("postkit-image-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("hero.png");
        std::fs::write(&img, b"bytes").unwrap();

        let url = resolve_image("https://cdn.test/h.png", "threads").unwrap();
        assert!(matches!(url, Image::Url(u) if u == "https://cdn.test/h.png"));
        // http is not upgraded or silently accepted.
        let err = resolve_image("http://cdn.test/h.png", "threads").unwrap_err();
        assert!(
            matches!(&err, Error::InvalidPost { reason, .. } if reason == "image_url_must_be_https")
        );

        let bytes = resolve_image(&img.display().to_string(), "bluesky").unwrap();
        assert!(
            matches!(bytes, Image::Bytes { ref filename, ref bytes } if filename == "hero.png" && bytes == b"bytes")
        );
        // Unreadable file: stable reason, no path echo.
        std::fs::remove_file(&img).unwrap();
        let err = resolve_image(&img.display().to_string(), "bluesky").unwrap_err();
        assert!(
            matches!(&err, Error::InvalidPost { reason, .. } if reason == "image_file_unreadable")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn image_flags_parse_and_document_the_split() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from([
            "postkit",
            "post",
            "bluesky",
            "--image",
            "./hero.png",
            "--text",
            "caption",
            "--alt",
            "chart",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Post { ref image, ref alt, .. }
                if image == &["./hero.png"] && alt == "chart"
        ));
        // Image without any --text is a valid, caption-less post.
        let cli = Cli::try_parse_from([
            "postkit",
            "post",
            "threads",
            "--image",
            "https://cdn.test/h.png",
        ])
        .unwrap();
        assert!(matches!(cli.command, Commands::Post { ref text, .. } if text.is_empty()));

        let carousel = Cli::try_parse_from([
            "postkit",
            "post",
            "instagram",
            "--image",
            "https://cdn.test/one.jpg",
            "--image",
            "https://cdn.test/two.jpg",
            "--text",
            "one caption",
        ])
        .unwrap();
        assert!(matches!(
            carousel.command,
            Commands::Post { ref image, .. }
                if image == &["https://cdn.test/one.jpg", "https://cdn.test/two.jpg"]
        ));
    }

    #[test]
    fn repeated_images_build_one_carousel_and_reject_ambiguous_flags() {
        let images = vec![
            "https://cdn.test/one.jpg".to_string(),
            "https://cdn.test/two.jpg".to_string(),
        ];
        let body = build_post_body(&images, Some("one caption".into()), "", "instagram").unwrap();
        assert!(matches!(
            &body,
            Body::Carousel { text: Some(text), images }
                if text == "one caption" && images.len() == 2
        ));
        assert_eq!(
            body.required_capability(),
            postkit::Capability::PublishCarousel
        );

        let reply = vec!["reply_to_id=1".to_string()];
        assert_eq!(
            image_input_conflict(2, 1, false, "alt", &[]),
            Some("carousel_alt_unsupported")
        );
        assert_eq!(
            image_input_conflict(2, 2, false, "", &[]),
            Some("carousel_caption_multiple")
        );
        assert_eq!(
            image_input_conflict(2, 1, false, "", &reply),
            Some("carousel_reply_unsupported")
        );
        assert_eq!(
            image_input_conflict(2, 1, true, "", &[]),
            Some("dry_run_image_unsupported")
        );

        let too_many = (0..MAX_CAROUSEL_IMAGES + 1)
            .map(|index| format!("missing-{index}.jpg"))
            .collect::<Vec<_>>();
        let error = build_post_body(&too_many, None, "", "instagram").unwrap_err();
        assert!(matches!(error, Error::InvalidPost { reason, limit, .. }
                if reason == "carousel_too_many_images" && limit == Some(MAX_CAROUSEL_IMAGES as u32)));
    }
}
