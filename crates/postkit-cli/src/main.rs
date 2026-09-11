mod ads;
mod app;
mod keys;
mod output;
mod post;
mod whatsapp;

use crate::ads::*;
use crate::app::{
    check_name, fail, invalid_post, make_client, parse_params, print_results, resolve_home,
};
use crate::post::*;
use crate::whatsapp::*;

use clap::{Parser, Subcommand};
use output::{emit_ok, emit_raw, human_line};
use postkit::connectors::threads::validate_text;
use postkit::{
    app_source, extract_code, verify_state, AccountKey, AppConfig, AppStore, AuthReply, Body,
    Client, Deadline, DraftStep, Error, FileAppStore, FileDraftStore, FileVault, Intent,
    MediaQuery, OAuthApp, PostRequest, RunPausedDraft, Site, Vault, WhatsAppMessage,
    WhatsAppSendRequest, DEFAULT_MEDIA_LIMIT,
};
use std::io::{self, BufRead, IsTerminal, Read};
use std::path::PathBuf;
use std::str::FromStr;

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
    /// WhatsApp Cloud replies, approved templates, and signed webhook parsing.
    #[command(name = "whatsapp", subcommand)]
    WhatsApp(WhatsAppCmd),
    /// Local HTTP for callers that cannot exec. With --json, prints one
    /// listen document then runs until interrupt; request results are HTTP
    /// bodies, not a second stdout document.
    Serve {
        /// Default 127.0.0.1:8788. Passing 0.0.0.0 is an explicit LAN bind.
        #[arg(long)]
        bind: Option<String>,
    },
    #[command(subcommand)]
    Keys(KeysCmd),
}

#[derive(Subcommand, Debug)]
enum KeysCmd {
    /// Print a `pk_live_` key once; store only its SHA-256.
    Create {
        #[arg(long)]
        name: String,
    },
    List,
    Revoke {
        #[arg(long)]
        name: String,
        #[arg(long)]
        yes: bool,
    },
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

#[derive(Subcommand, Debug)]
enum WhatsAppCmd {
    /// Store the sender phone-number ID and optional webhook app secret.
    /// The permanent System User token is added separately with
    /// `auth whatsapp_cloud --token …` and never enters this config file.
    Configure {
        #[arg(long)]
        phone_number_id: String,
        /// WhatsApp Business Account ID. Required for template list/create.
        #[arg(long)]
        waba_id: Option<String>,
        /// Needed only by `whatsapp webhook parse`; it is never printed.
        #[arg(long)]
        app_secret: Option<String>,
        /// Meta GET `hub.verify_token`. Distinct from the app secret HMAC.
        #[arg(long)]
        verify_token: Option<String>,
    },
    /// In-window service text with no `context`. Meta only delivers this
    /// while a customer-service window is open; `--allow-send` acknowledges
    /// a real private message. Not a quoted reply — use `reply` for that.
    Text {
        /// WhatsApp ID. `+`, spaces, hyphens, and parentheses are stripped.
        #[arg(long)]
        to: String,
        #[arg(long)]
        text: String,
        #[arg(long)]
        preview_url: bool,
        #[arg(long)]
        idempotency: String,
        #[arg(long)]
        allow_send: bool,
        /// `individual` (default) or `group`. Group `to` is a Groups API id.
        #[arg(long, default_value = "individual")]
        recipient_type: String,
    },
    /// Reply with text to an inbound message. Meta enforces its service
    /// window; `--allow-send` acknowledges this is a real private message.
    Reply {
        /// WhatsApp ID. `+`, spaces, hyphens, and parentheses are allowed.
        #[arg(long)]
        to: String,
        /// The inbound `wamid` this reply is attached to.
        #[arg(long = "reply-to")]
        reply_to_message_id: String,
        #[arg(long)]
        text: String,
        /// Ask Meta to unfurl URLs in the body (extra remote fetch).
        #[arg(long)]
        preview_url: bool,
        /// Required. Confirmed successes are not resent. If the HTTP call
        /// left the machine and the response was lost, Postkit does not
        /// retry — check the delivery webhook first.
        #[arg(long)]
        idempotency: String,
        /// Explicitly authorize this one private, potentially chargeable send.
        #[arg(long)]
        allow_send: bool,
        /// `individual` (default) or `group`. Group `to` is a Groups API id.
        #[arg(long, default_value = "individual")]
        recipient_type: String,
    },
    /// Send one existing Meta-approved template with ordered body variables.
    /// It cannot create, edit, or submit a template for approval.
    Template {
        /// WhatsApp ID. `+`, spaces, hyphens, and parentheses are allowed.
        #[arg(long)]
        to: String,
        /// Existing approved template name, e.g. `order_update`.
        #[arg(long)]
        name: String,
        /// Meta locale code, e.g. `en_US` or `ms`.
        #[arg(long)]
        language: String,
        /// Ordered text substitution for the template body; repeat per value.
        #[arg(long = "body-param", action = clap::ArgAction::Append)]
        body_parameters: Vec<String>,
        /// Required to prevent duplicate private sends on a confirmed retry.
        #[arg(long)]
        idempotency: String,
        /// Explicitly authorize this one private, potentially chargeable send.
        #[arg(long)]
        allow_send: bool,
        /// `individual` (default) or `group`. Group `to` is a Groups API id.
        #[arg(long, default_value = "individual")]
        recipient_type: String,
    },
    /// Parse one signed raw Cloud API webhook body from stdin. This does not
    /// run an HTTP listener or acknowledge Meta's webhook delivery.
    #[command(subcommand)]
    Webhook(WhatsAppWebhookCmd),
}

#[derive(Subcommand, Debug)]
enum WhatsAppWebhookCmd {
    Parse {
        /// The request's exact `X-Hub-Signature-256` value.
        #[arg(long)]
        signature: String,
        /// Include recipient_id, conversation, and pricing on statuses.
        /// Off by default: those fields are personal/billing data.
        #[arg(long)]
        status_extras: bool,
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
            if site == "whatsapp_cloud" {
                let phone_number_id = cfg
                    .extra
                    .get("phone_number_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let webhook_signing = cfg
                    .extra
                    .get("app_secret")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|secret| !secret.is_empty());
                let source = app_source(&Site::new(&site));
                if json {
                    emit_raw(&serde_json::json!({
                        "site": site,
                        "source": source,
                        "phone_number_id": phone_number_id,
                        "webhook_signing": webhook_signing,
                    }));
                } else {
                    human_line(format!(
                        "site=whatsapp_cloud source={source} phone_number_id={phone_number_id} webhook_signing={webhook_signing} app_secret=[redacted]"
                    ));
                }
                return Ok(());
            }
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
        Commands::Serve { bind } => postkit_serve::run(&home, bind.as_deref(), json)
            .await
            .map_err(|e| fail(&e, json)),
        Commands::Keys(KeysCmd::Create { name }) => crate::keys::create(&home, &name, json),
        Commands::Keys(KeysCmd::List) => crate::keys::list(&home, json),
        Commands::Keys(KeysCmd::Revoke { name, yes }) => {
            crate::keys::revoke(&home, &name, yes, json)
        }
        Commands::WhatsApp(WhatsAppCmd::Configure {
            phone_number_id,
            waba_id,
            app_secret,
            verify_token,
        }) => {
            let cfg = whatsapp_app_config(phone_number_id, waba_id, app_secret, verify_token)
                .map_err(|e| fail(&e, json))?;
            let apps = FileAppStore::new(&home).map_err(|e| fail(&e, json))?;
            apps.put(&cfg).map_err(|e| fail(&e, json))?;
            if app_source(&Site::new("whatsapp_cloud")) == "env" {
                eprintln!(
                    "note: WhatsApp environment values override only their matching fields; an unset app secret remains available from apps/whatsapp_cloud.json"
                );
            }
            let phone_number_id = cfg.extra["phone_number_id"]
                .as_str()
                .expect("validated phone id");
            if json {
                emit_raw(&serde_json::json!({
                    "site": "whatsapp_cloud",
                    "phone_number_id": phone_number_id,
                    "webhook_signing": cfg.extra["app_secret"].is_string(),
                }));
            } else {
                eprintln!(
                    "configured whatsapp_cloud sender {phone_number_id}; webhook signing={}",
                    cfg.extra["app_secret"].is_string()
                );
            }
            Ok(())
        }
        other => {
            let allow_whatsapp_send = whatsapp_send_allowed(&other);
            let client = make_client(&home, allow_whatsapp_send).map_err(|e| fail(&e, json))?;
            dispatch(client, &home, other, json, account, deadline).await
        }
    }
}

/// The only commands permitted to install the allowing policy are the two
/// typed sends, and each still requires its own explicit `--allow-send`.
fn whatsapp_send_allowed(command: &Commands) -> bool {
    matches!(
        command,
        Commands::WhatsApp(WhatsAppCmd::Reply {
            allow_send: true,
            ..
        }) | Commands::WhatsApp(WhatsAppCmd::Text {
            allow_send: true,
            ..
        }) | Commands::WhatsApp(WhatsAppCmd::Template {
            allow_send: true,
            ..
        })
    )
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
        Commands::WhatsApp(WhatsAppCmd::Text {
            to,
            text,
            preview_url,
            idempotency,
            recipient_type,
            ..
        }) => {
            let request = WhatsAppSendRequest {
                message: WhatsAppMessage::Text {
                    to,
                    text,
                    preview_url,
                },
                idempotency_key: idempotency,
                recipient_type: parse_recipient_type(&recipient_type, json)?,
            };
            one_whatsapp_send(
                &client,
                &AccountKey::new("whatsapp_cloud", &account),
                request,
                deadline,
                json,
            )
            .await
        }
        Commands::WhatsApp(WhatsAppCmd::Reply {
            to,
            reply_to_message_id,
            text,
            preview_url,
            idempotency,
            recipient_type,
            ..
        }) => {
            let request = WhatsAppSendRequest {
                message: WhatsAppMessage::Reply {
                    to,
                    reply_to_message_id,
                    text,
                    preview_url,
                },
                idempotency_key: idempotency,
                recipient_type: parse_recipient_type(&recipient_type, json)?,
            };
            one_whatsapp_send(
                &client,
                &AccountKey::new("whatsapp_cloud", &account),
                request,
                deadline,
                json,
            )
            .await
        }
        Commands::WhatsApp(WhatsAppCmd::Template {
            to,
            name,
            language,
            body_parameters,
            idempotency,
            recipient_type,
            ..
        }) => {
            let request = WhatsAppSendRequest {
                message: WhatsAppMessage::Template {
                    to,
                    name,
                    language,
                    body_parameters,
                    named_body_parameters: vec![],
                    header: None,
                    buttons: vec![],
                    limited_time_offer: None,
                },
                idempotency_key: idempotency,
                recipient_type: parse_recipient_type(&recipient_type, json)?,
            };
            one_whatsapp_send(
                &client,
                &AccountKey::new("whatsapp_cloud", &account),
                request,
                deadline,
                json,
            )
            .await
        }
        Commands::WhatsApp(WhatsAppCmd::Webhook(WhatsAppWebhookCmd::Parse {
            signature,
            status_extras,
        })) => {
            let raw = read_whatsapp_webhook_stdin(json)?;
            let reply = client
                .parse_whatsapp_webhook(
                    &signature,
                    &raw,
                    postkit::WebhookParseOptions {
                        include_status_extras: status_extras,
                    },
                )
                .map_err(|error| fail(&error, json))?;
            if json {
                emit_raw(&serde_json::to_value(&reply).expect("webhook reply serializes"));
            } else {
                // Do not echo phone numbers or customer text to a terminal by
                // default. Scripts that explicitly need the PII use --json.
                human_line(whatsapp_webhook_line(&reply));
            }
            Ok(())
        }
        // `Configure` is handled before a Client is constructed because it
        // changes the app configuration that `auth --token` must validate.
        Commands::WhatsApp(WhatsAppCmd::Configure { .. }) => unreachable!("handled in run"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::*;
    use clap::CommandFactory;
    use postkit::connectors::instagram::MAX_CAROUSEL_IMAGES;
    use postkit::{
        AdAccount, AdEntity, AdPreviewFormat, AdReviewStatus, AdReviewWait, Breakdown,
        CampaignObjective, CreatedAd, CreatedAdCreative, Image, InboundMessages, InsightRow,
        InsightsLevel, LinkCallToAction, Metric, PausedAdCreate, PausedCampaign,
        PausedDraftManifest, UploadedAdImage,
    };
    use std::path::Path;

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
    fn whatsapp_commands_require_explicit_send_acknowledgement_and_idempotency() {
        let allowed = Cli::try_parse_from([
            "postkit",
            "whatsapp",
            "reply",
            "--to",
            "60123456789",
            "--reply-to",
            "wamid.inbound",
            "--text",
            "Terima kasih",
            "--idempotency",
            "reply-1",
            "--allow-send",
        ])
        .unwrap();
        assert!(whatsapp_send_allowed(&allowed.command));
        assert!(matches!(
            allowed.command,
            Commands::WhatsApp(WhatsAppCmd::Reply { idempotency, .. }) if idempotency == "reply-1"
        ));

        let session = Cli::try_parse_from([
            "postkit",
            "whatsapp",
            "text",
            "--to",
            "60123456789",
            "--text",
            "Hello",
            "--idempotency",
            "text-1",
            "--allow-send",
        ])
        .unwrap();
        assert!(whatsapp_send_allowed(&session.command));
        assert!(matches!(
            session.command,
            Commands::WhatsApp(WhatsAppCmd::Text { .. })
        ));

        let unacknowledged = Cli::try_parse_from([
            "postkit",
            "whatsapp",
            "template",
            "--to",
            "60123456789",
            "--name",
            "order_update",
            "--language",
            "en_US",
            "--idempotency",
            "template-1",
        ])
        .unwrap();
        assert!(!whatsapp_send_allowed(&unacknowledged.command));

        // Clap makes idempotency non-optional: a private send cannot silently
        // fall back to the less-safe no-ledger behavior of generic posts.
        assert!(Cli::try_parse_from([
            "postkit",
            "whatsapp",
            "reply",
            "--to",
            "60123456789",
            "--reply-to",
            "wamid.inbound",
            "--text",
            "hi",
        ])
        .is_err());
    }

    #[test]
    fn whatsapp_config_is_phone_only_and_never_needs_oauth_fields() {
        let cfg =
            whatsapp_app_config("123456789".into(), None, Some("app-secret".into()), None).unwrap();
        assert!(cfg.oauth.is_none());
        assert_eq!(cfg.extra["phone_number_id"].as_str(), Some("123456789"));
        assert!(whatsapp_app_config("+6012".into(), None, None, None).is_err());
    }

    #[test]
    fn whatsapp_webhook_human_output_reports_counts_without_message_identifiers() {
        let reply = InboundMessages {
            site: Site::new("whatsapp_cloud"),
            messages: vec![],
            statuses: vec![
                postkit::DeliveryStatus {
                    id: "wamid.private-one".into(),
                    status: postkit::DeliveryStatusKind::Delivered,
                    timestamp: Some("1".into()),
                    errors: vec![],
                    recipient_id: None,
                    conversation: None,
                    pricing: None,
                },
                postkit::DeliveryStatus {
                    id: "wamid.private-two".into(),
                    status: postkit::DeliveryStatusKind::Read,
                    timestamp: Some("2".into()),
                    errors: vec![],
                    recipient_id: None,
                    conversation: None,
                    pricing: None,
                },
            ],
        };
        let line = whatsapp_webhook_line(&reply);
        assert_eq!(
            line,
            "whatsapp_cloud verified webhook: 0 inbound message(s), 2 delivery status(es)"
        );
        assert!(!line.contains("wamid.private"));
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
    fn serve_and_keys_commands_parse() {
        let cli = Cli::try_parse_from(["postkit", "serve", "--bind", "127.0.0.1:9000"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Serve { bind: Some(ref b) } if b == "127.0.0.1:9000"
        ));
        let cli = Cli::try_parse_from(["postkit", "keys", "create", "--name", "n8n"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Keys(KeysCmd::Create { ref name }) if name == "n8n"
        ));
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
