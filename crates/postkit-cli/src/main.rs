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
    Client, ConsentKind, ConsentRecord, Deadline, DraftStep, Error, FileAppStore, FileDraftStore,
    FileVault, Intent, MediaQuery, OAuthApp, PostRequest, RunPausedDraft, Site, Vault,
    WhatsAppFlowDraft, WhatsAppMessage, WhatsAppPageQuery, WhatsAppSendRequest,
    WhatsAppTemplateDraft, WhatsAppTemplateQuery, DEFAULT_MEDIA_LIMIT,
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
        /// Meta Ads: store a Business Manager System User token. Requires
        /// `--token`. Refuses a user OAuth token reused as a service secret.
        #[arg(long)]
        system_user: bool,
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
        /// Comma-separated typed metrics. Each is defined in docs/meta-ads;
        /// spend is windowed delivery spend, not an invoice.
        #[arg(
            long,
            default_value = "spend,impressions,clicks,purchases",
            help = "Comma-separated: spend,impressions,clicks,reach,ctr,cpc,cpm,purchases,purchase_value,roas,frequency,unique_clicks,inline_link_clicks,inline_link_click_ctr,quality_ranking,video_thruplay"
        )]
        metrics: String,
        /// Explicit window. ROAS answers change with it; there is no default.
        #[arg(
            long,
            help = "7d_click_1d_view | 1d_click | 1d_view | 7d_click | 28d_click | 7d_view | 28d_view"
        )]
        attribution: String,
        /// Override the stored ad account (123 or act_123).
        #[arg(long)]
        ad_account: Option<String>,
        /// Repeatable campaign, ad set, or ad ID filter. Not valid at account level.
        #[arg(long = "entity-id")]
        entity_ids: Vec<String>,
        /// At most two. age+gender and publisher_platform+platform_position are
        /// documented Meta pairs.
        #[arg(
            long,
            default_value = "",
            help = "Comma-separated: country,publisher_platform,age,gender,device_platform,platform_position"
        )]
        breakdowns: String,
        /// POST an Ad Report Run and poll until --deadline. Pending is explicit.
        #[arg(long = "async")]
        async_report: bool,
        /// performance (default mixed) | delivery | creative.
        #[arg(long, default_value = "performance")]
        report: String,
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
    /// Typed WhatsApp Cloud sends, media, business operations, and signed
    /// webhook parsing. Customer sends need --allow-send; management writes
    /// need --yes.
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
    /// MCP stdio server for local agent hosts. Stdout is JSON-RPC only;
    /// omit `--json`. Vault from `--home` / POSTKIT_HOME.
    Mcp,
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
#[allow(clippy::large_enum_variant)]
enum InsightsJobCmd {
    Status {
        site: String,
        #[arg(long)]
        id: String,
    },
    /// Fetch completed rows. Pass the same query flags used to start the job.
    Result {
        site: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long, default_value = "account")]
        level: String,
        #[arg(long, default_value = "spend,impressions,clicks,purchases")]
        metrics: String,
        #[arg(long)]
        attribution: String,
        #[arg(long)]
        ad_account: Option<String>,
        #[arg(long = "entity-id")]
        entity_ids: Vec<String>,
        #[arg(long, default_value = "")]
        breakdowns: String,
        #[arg(long, default_value = "performance")]
        report: String,
    },
    Cancel {
        site: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum AdsCmd {
    /// List Meta ad accounts visible to the selected credential.
    Accounts { site: String },
    /// Async Insights Ad Report Run: status, result, or cancel. Jobs expire
    /// in ~30 days and are not stored in the vault.
    #[command(name = "insights-job", subcommand)]
    InsightsJob(InsightsJobCmd),
    /// Inspect the stored access token via Graph `/debug_token`. Never prints
    /// the token. Requires app config (app access token).
    InspectToken { site: String },
    /// Report Marketing API Access Tier (Limited vs Full). Dashboard is
    /// authoritative; a response header is only a hint.
    AccessTier { site: String },
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
        /// Campaign-level daily budget (CBO). XOR with `--lifetime-budget`.
        #[arg(long)]
        daily_budget: Option<u64>,
        /// Campaign-level lifetime budget (CBO). XOR with `--daily-budget`.
        #[arg(long)]
        lifetime_budget: Option<u64>,
        /// Meta `is_adset_budget_sharing_enabled`. Requires a campaign budget.
        #[arg(long)]
        adset_budget_sharing: bool,
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
        /// Daily budget in the ad account's minor currency unit. XOR with
        /// `--lifetime-budget`. Omit both for a CBO child.
        #[arg(long)]
        daily_budget: Option<u64>,
        /// Lifetime budget in minor units. XOR with `--daily-budget`.
        #[arg(long)]
        lifetime_budget: Option<u64>,
        /// `lowest_cost_without_cap` | `lowest_cost_with_bid_cap` | `cost_cap`
        /// | `lowest_cost_with_min_roas`. Cap/floor strategies need their
        /// constraint flags below.
        #[arg(long)]
        bid_strategy: String,
        /// Required for bid-cap and cost-cap. Minor units.
        #[arg(long)]
        bid_amount: Option<u64>,
        /// Required for min-ROAS. Meta scale: 10000 = 1.0.
        #[arg(long)]
        roas_average_floor: Option<u64>,
        /// impressions | link_clicks
        #[arg(long)]
        billing_event: String,
        /// reach | brand_awareness | link_clicks | landing_page_views | …
        #[arg(long)]
        optimization_goal: String,
        /// Repeatable ISO 3166-1 alpha-2 country (MY, US, …).
        #[arg(long = "country", required = true)]
        countries: Vec<String>,
        #[arg(long)]
        age_min: Option<u8>,
        #[arg(long)]
        age_max: Option<u8>,
        #[arg(long = "publisher-platform")]
        publisher_platforms: Vec<String>,
        #[arg(long = "facebook-position")]
        facebook_positions: Vec<String>,
        #[arg(long = "instagram-position")]
        instagram_positions: Vec<String>,
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
        /// Meta Business Portfolio ID. Enables paged owned-WABA and
        /// system-user reads; it is distinct from the WABA ID.
        #[arg(long)]
        business_id: Option<String>,
        /// Needed only by `whatsapp webhook parse`; it is never printed.
        #[arg(long)]
        app_secret: Option<String>,
        /// Meta GET `hub.verify_token`. Distinct from the app secret HMAC.
        #[arg(long)]
        verify_token: Option<String>,
        /// Extra outbound phone in `alias=phone_number_id` form. An alias is
        /// selected explicitly by `whatsapp send --sender <alias>`; it never
        /// replaces the primary phone used by existing commands.
        #[arg(long = "sender", value_name = "alias=phone_number_id", action = clap::ArgAction::Append)]
        senders: Vec<String>,
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
    /// Send any other closed-schema WhatsApp message type. The JSON request
    /// is deserialized as `WhatsAppSendRequest`; it is not arbitrary Graph
    /// JSON, and every private send still requires --allow-send.
    Send {
        /// JSON file containing one WhatsAppSendRequest. Use `-` for stdin.
        #[arg(long)]
        request: PathBuf,
        /// Configured sender alias, not a raw Meta phone-number ID.
        #[arg(long)]
        sender: Option<String>,
        #[arg(long)]
        allow_send: bool,
    },
    /// Bounded fan-out of up to ten closed-schema requests. All messages use
    /// the same explicit sender and are paced by that phone's local queue.
    SendBatch {
        /// JSON file containing an array of WhatsAppSendRequest. Use `-` for stdin.
        #[arg(long)]
        requests: PathBuf,
        #[arg(long)]
        sender: Option<String>,
        #[arg(long)]
        allow_send: bool,
    },
    /// Media, template, Flow, account, and local-ledger operations use typed
    /// subcommands instead of a raw Graph endpoint escape hatch.
    /// Typed Cloud API media upload/read/download/delete.
    #[command(subcommand)]
    Media(WhatsAppMediaCmd),
    /// WABA template list/read/create/edit/delete operations.
    #[command(subcommand)]
    Templates(WhatsAppTemplatesCmd),
    /// WhatsApp Flow list/read/create/publish operations.
    #[command(subcommand)]
    Flows(WhatsAppFlowsCmd),
    /// WABA, sender-phone, and System User reads and phone setup operations.
    #[command(subcommand)]
    Account(WhatsAppAccountCmd),
    /// Minimal local callback-correlation ledger reads and retention purge.
    #[command(subcommand)]
    Ledger(WhatsAppLedgerCmd),
    /// Local operator consent audit records; not automatic send authorization.
    #[command(subcommand)]
    Consent(WhatsAppConsentCmd),
    /// Parse one signed raw Cloud API webhook body from stdin. This does not
    /// run an HTTP listener or acknowledge Meta's webhook delivery.
    #[command(subcommand)]
    Webhook(WhatsAppWebhookCmd),
}

#[derive(Subcommand, Debug)]
enum WhatsAppMediaCmd {
    Upload {
        #[arg(long)]
        file: PathBuf,
        /// Exact MIME type from Meta's supported media table.
        #[arg(long)]
        mime_type: String,
    },
    Metadata {
        #[arg(long)]
        media_id: String,
    },
    Download {
        #[arg(long)]
        media_id: String,
        /// New local path. Existing files are refused rather than replaced.
        #[arg(long)]
        output: PathBuf,
    },
    Delete {
        #[arg(long)]
        media_id: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum WhatsAppTemplatesCmd {
    List {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        after: Option<String>,
    },
    Get {
        #[arg(long)]
        template_id: String,
    },
    /// Draft JSON is deserialized as the documented WhatsAppTemplateDraft
    /// contract and submitted for Meta approval only after --yes.
    Create {
        #[arg(long)]
        draft: PathBuf,
        #[arg(long)]
        yes: bool,
    },
    Edit {
        #[arg(long)]
        template_id: String,
        #[arg(long)]
        draft: PathBuf,
        #[arg(long)]
        yes: bool,
    },
    Delete {
        #[arg(long)]
        name: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum WhatsAppFlowsCmd {
    List {
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        after: Option<String>,
    },
    Get {
        #[arg(long)]
        flow_id: String,
    },
    Create {
        #[arg(long)]
        draft: PathBuf,
        #[arg(long)]
        yes: bool,
    },
    Publish {
        #[arg(long)]
        flow_id: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum WhatsAppAccountCmd {
    Wabas {
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        after: Option<String>,
    },
    PhoneNumbers {
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        after: Option<String>,
    },
    PhoneHealth,
    SystemUsers {
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        after: Option<String>,
    },
    SubscribeApps {
        #[arg(long)]
        yes: bool,
    },
    RegisterPhone {
        #[arg(long)]
        pin: String,
        #[arg(long)]
        yes: bool,
    },
    SetTwoStepPin {
        #[arg(long)]
        pin: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum WhatsAppLedgerCmd {
    Get {
        #[arg(long)]
        wamid: String,
    },
    Window {
        #[arg(long)]
        wa_id: String,
    },
    Purge {
        #[arg(long)]
        before_unix: u64,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum WhatsAppConsentCmd {
    Get {
        #[arg(long)]
        wa_id: String,
    },
    Set {
        #[arg(long)]
        wa_id: String,
        /// opt_in or opt_out. This records an operator signal; it never
        /// bypasses Meta's policy checks.
        #[arg(long)]
        kind: String,
        #[arg(long)]
        at_unix: Option<u64>,
        #[arg(long)]
        yes: bool,
    },
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
                let webhook_verify = cfg
                    .extra
                    .get("verify_token")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|token| !token.is_empty());
                let source = app_source(&Site::new(&site));
                if json {
                    emit_raw(&serde_json::json!({
                        "site": site,
                        "source": source,
                        "phone_number_id": phone_number_id,
                        "webhook_signing": webhook_signing,
                        "webhook_verify": webhook_verify,
                    }));
                } else {
                    human_line(format!(
                        "site=whatsapp_cloud source={source} phone_number_id={phone_number_id} webhook_signing={webhook_signing} webhook_verify={webhook_verify} app_secret=[redacted] verify_token=[redacted]"
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
        Commands::Mcp => {
            // MCP stdio reserves stdout for JSON-RPC. A `--json` document
            // here would corrupt the host's protocol stream.
            if json {
                eprintln!("mcp uses stdout for JSON-RPC; omit --json");
                return Err(2);
            }
            postkit_mcp::run(&home).await.map_err(|e| fail(&e, false))
        }
        Commands::Keys(KeysCmd::Create { name }) => crate::keys::create(&home, &name, json),
        Commands::Keys(KeysCmd::List) => crate::keys::list(&home, json),
        Commands::Keys(KeysCmd::Revoke { name, yes }) => {
            crate::keys::revoke(&home, &name, yes, json)
        }
        Commands::WhatsApp(WhatsAppCmd::Configure {
            phone_number_id,
            waba_id,
            business_id,
            app_secret,
            verify_token,
            senders,
        }) => {
            let configured_senders = parse_whatsapp_senders(&senders, json)?;
            let cfg = whatsapp_app_config(
                phone_number_id,
                waba_id,
                business_id,
                app_secret,
                verify_token,
                configured_senders,
            )
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
                    "sender_aliases": cfg.extra["senders"].as_array().map(|items| items.iter().filter_map(|item| item.get("alias").and_then(|value| value.as_str())).collect::<Vec<_>>()).unwrap_or_default(),
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

/// The only commands permitted to install the allowing policy are explicit
/// private sends and the two sensitive phone-registration mutations. Each has
/// its own acknowledgement (`--allow-send` or `--yes`) before dispatch.
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
        }) | Commands::WhatsApp(WhatsAppCmd::Send {
            allow_send: true,
            ..
        }) | Commands::WhatsApp(WhatsAppCmd::SendBatch {
            allow_send: true,
            ..
        }) | Commands::WhatsApp(WhatsAppCmd::Account(WhatsAppAccountCmd::RegisterPhone {
            yes: true,
            ..
        })) | Commands::WhatsApp(WhatsAppCmd::Account(WhatsAppAccountCmd::SetTwoStepPin {
            yes: true,
            ..
        }))
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
        Commands::WhatsApp(WhatsAppCmd::Send {
            request, sender, ..
        }) => {
            let request: WhatsAppSendRequest = read_whatsapp_json(&request, json)?;
            one_whatsapp_send_from(
                &client,
                &AccountKey::new("whatsapp_cloud", &account),
                sender.as_deref(),
                request,
                deadline,
                json,
            )
            .await
        }
        Commands::WhatsApp(WhatsAppCmd::SendBatch {
            requests, sender, ..
        }) => {
            let requests: Vec<WhatsAppSendRequest> = read_whatsapp_json(&requests, json)?;
            let key = AccountKey::new("whatsapp_cloud", &account);
            match client
                .send_whatsapp_many_from(&key, sender.as_deref(), requests, deadline)
                .await
            {
                Ok(outcomes) => {
                    if json {
                        emit_raw(&serde_json::json!({ "outcomes": outcomes }));
                    } else {
                        human_line(format!(
                            "whatsapp_cloud {} messages accepted by Meta; delivery statuses arrive via webhook",
                            outcomes.len()
                        ));
                    }
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            }
        }
        Commands::WhatsApp(WhatsAppCmd::Media(command)) => {
            let key = AccountKey::new("whatsapp_cloud", &account);
            match command {
                WhatsAppMediaCmd::Upload { file, mime_type } => {
                    let bytes = std::fs::read(&file).map_err(|_| {
                        fail(
                            &Error::InvalidQuery {
                                site: Site::new("whatsapp_cloud"),
                                reason: "whatsapp_media_unreadable".into(),
                            },
                            json,
                        )
                    })?;
                    let filename = file
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| !name.is_empty())
                        .ok_or_else(|| {
                            fail(
                                &Error::InvalidQuery {
                                    site: Site::new("whatsapp_cloud"),
                                    reason: "whatsapp_media_filename_invalid".into(),
                                },
                                json,
                            )
                        })?
                        .to_string();
                    match client
                        .upload_whatsapp_media(
                            &key,
                            postkit::WhatsAppMediaUpload {
                                bytes,
                                mime_type,
                                filename,
                            },
                            deadline,
                        )
                        .await
                    {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "media upload");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppMediaCmd::Metadata { media_id } => match client
                    .whatsapp_media_metadata(&key, &media_id, deadline)
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "media metadata read");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppMediaCmd::Download { media_id, output } => {
                    match client
                        .download_whatsapp_media(&key, &media_id, deadline)
                        .await
                    {
                        Ok(bytes) => {
                            write_whatsapp_download(&output, &bytes, json)?;
                            if json {
                                emit_raw(&serde_json::json!({
                                    "downloaded": true,
                                    "bytes": bytes.len(),
                                }));
                            } else {
                                human_line("whatsapp_cloud media download completed");
                            }
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppMediaCmd::Delete { media_id, yes } => {
                    require_whatsapp_yes(yes, "delete WhatsApp media", json)?;
                    match client
                        .delete_whatsapp_media(&key, &media_id, deadline)
                        .await
                    {
                        Ok(()) => {
                            emit_whatsapp_value(
                                &serde_json::json!({ "deleted": true }),
                                json,
                                "media delete",
                            );
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
            }
        }
        Commands::WhatsApp(WhatsAppCmd::Templates(command)) => {
            let key = AccountKey::new("whatsapp_cloud", &account);
            match command {
                WhatsAppTemplatesCmd::List {
                    name,
                    status,
                    limit,
                    after,
                } => match client
                    .list_whatsapp_templates(
                        &key,
                        WhatsAppTemplateQuery {
                            name,
                            status,
                            limit,
                            after,
                        },
                        deadline,
                    )
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "template list");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppTemplatesCmd::Get { template_id } => match client
                    .get_whatsapp_template(&key, &template_id, deadline)
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "template read");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppTemplatesCmd::Create { draft, yes } => {
                    require_whatsapp_yes(yes, "submit a template for Meta review", json)?;
                    let draft: WhatsAppTemplateDraft = read_whatsapp_json(&draft, json)?;
                    match client.create_whatsapp_template(&key, draft, deadline).await {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "template create");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppTemplatesCmd::Edit {
                    template_id,
                    draft,
                    yes,
                } => {
                    require_whatsapp_yes(yes, "edit a WhatsApp template", json)?;
                    let draft: WhatsAppTemplateDraft = read_whatsapp_json(&draft, json)?;
                    match client
                        .edit_whatsapp_template(&key, &template_id, draft, deadline)
                        .await
                    {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "template edit");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppTemplatesCmd::Delete { name, yes } => {
                    require_whatsapp_yes(yes, "delete a WhatsApp template", json)?;
                    match client.delete_whatsapp_template(&key, &name, deadline).await {
                        Ok(()) => {
                            emit_whatsapp_value(
                                &serde_json::json!({ "deleted": true }),
                                json,
                                "template delete",
                            );
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
            }
        }
        Commands::WhatsApp(WhatsAppCmd::Flows(command)) => {
            let key = AccountKey::new("whatsapp_cloud", &account);
            match command {
                WhatsAppFlowsCmd::List { limit, after } => match client
                    .list_whatsapp_flows(&key, WhatsAppPageQuery { limit, after }, deadline)
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "Flow list");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppFlowsCmd::Get { flow_id } => {
                    match client.get_whatsapp_flow(&key, &flow_id, deadline).await {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "Flow read");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppFlowsCmd::Create { draft, yes } => {
                    require_whatsapp_yes(yes, "create a WhatsApp Flow", json)?;
                    let draft: WhatsAppFlowDraft = read_whatsapp_json(&draft, json)?;
                    match client.create_whatsapp_flow(&key, draft, deadline).await {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "Flow create");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppFlowsCmd::Publish { flow_id, yes } => {
                    require_whatsapp_yes(yes, "publish a WhatsApp Flow", json)?;
                    match client.publish_whatsapp_flow(&key, &flow_id, deadline).await {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "Flow publish");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
            }
        }
        Commands::WhatsApp(WhatsAppCmd::Account(command)) => {
            let key = AccountKey::new("whatsapp_cloud", &account);
            match command {
                WhatsAppAccountCmd::Wabas { limit, after } => match client
                    .list_whatsapp_wabas(&key, WhatsAppPageQuery { limit, after }, deadline)
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "WABA list");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppAccountCmd::PhoneNumbers { limit, after } => match client
                    .list_whatsapp_phone_numbers(&key, WhatsAppPageQuery { limit, after }, deadline)
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "phone-number list");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppAccountCmd::PhoneHealth => {
                    match client.whatsapp_phone_health(&key, deadline).await {
                        Ok(reply) => {
                            emit_whatsapp_value(&reply, json, "phone health read");
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppAccountCmd::SystemUsers { limit, after } => match client
                    .list_whatsapp_system_users(&key, WhatsAppPageQuery { limit, after }, deadline)
                    .await
                {
                    Ok(reply) => {
                        emit_whatsapp_value(&reply, json, "system-user list");
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                },
                WhatsAppAccountCmd::SubscribeApps { yes } => {
                    require_whatsapp_yes(yes, "subscribe the app to WhatsApp webhooks", json)?;
                    match client.subscribe_whatsapp_apps(&key, deadline).await {
                        Ok(()) => {
                            emit_whatsapp_value(
                                &serde_json::json!({ "subscribed": true }),
                                json,
                                "app subscription",
                            );
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppAccountCmd::RegisterPhone { pin, yes } => {
                    require_whatsapp_yes(yes, "register the WhatsApp phone", json)?;
                    match client.register_whatsapp_phone(&key, &pin, deadline).await {
                        Ok(()) => {
                            emit_whatsapp_value(
                                &serde_json::json!({ "registered": true }),
                                json,
                                "phone registration",
                            );
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
                WhatsAppAccountCmd::SetTwoStepPin { pin, yes } => {
                    require_whatsapp_yes(yes, "set the WhatsApp two-step PIN", json)?;
                    match client.set_whatsapp_two_step_pin(&key, &pin, deadline).await {
                        Ok(()) => {
                            emit_whatsapp_value(
                                &serde_json::json!({ "updated": true }),
                                json,
                                "two-step PIN update",
                            );
                            Ok(())
                        }
                        Err(error) => Err(fail(&error, json)),
                    }
                }
            }
        }
        Commands::WhatsApp(WhatsAppCmd::Ledger(command)) => match command {
            WhatsAppLedgerCmd::Get { wamid } => match client.whatsapp_ledger_get(&wamid) {
                Ok(reply) => {
                    emit_whatsapp_value(&reply, json, "ledger read");
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            },
            WhatsAppLedgerCmd::Window { wa_id } => match client.whatsapp_window_open(&wa_id) {
                Ok(open) => {
                    emit_whatsapp_value(&serde_json::json!({ "open": open }), json, "window read");
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            },
            WhatsAppLedgerCmd::Purge { before_unix, yes } => {
                require_whatsapp_yes(yes, "purge local WhatsApp ledger records", json)?;
                match client.purge_whatsapp_ledger_before(before_unix) {
                    Ok(removed) => {
                        emit_whatsapp_value(
                            &serde_json::json!({ "removed": removed }),
                            json,
                            "ledger purge",
                        );
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                }
            }
        },
        Commands::WhatsApp(WhatsAppCmd::Consent(command)) => match command {
            WhatsAppConsentCmd::Get { wa_id } => match client.get_whatsapp_consent(&wa_id) {
                Ok(reply) => {
                    emit_whatsapp_value(&reply, json, "consent read");
                    Ok(())
                }
                Err(error) => Err(fail(&error, json)),
            },
            WhatsAppConsentCmd::Set {
                wa_id,
                kind,
                at_unix,
                yes,
            } => {
                require_whatsapp_yes(yes, "record a WhatsApp consent decision", json)?;
                let kind = match kind.as_str() {
                    "opt_in" => ConsentKind::OptIn,
                    "opt_out" => ConsentKind::OptOut,
                    _ => {
                        return Err(fail(
                            &invalid_post("whatsapp_cloud", "whatsapp_consent_kind_invalid"),
                            json,
                        ))
                    }
                };
                let at = at_unix.unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|value| value.as_secs())
                        .unwrap_or(0)
                });
                match client.put_whatsapp_consent(ConsentRecord { wa_id, kind, at }) {
                    Ok(()) => {
                        emit_whatsapp_value(
                            &serde_json::json!({ "recorded": true }),
                            json,
                            "consent record",
                        );
                        Ok(())
                    }
                    Err(error) => Err(fail(&error, json)),
                }
            }
        },
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
        Commands::Ads(AdsCmd::InsightsJob(InsightsJobCmd::Status { site, id })) => {
            let key = AccountKey::new(&site, &account);
            match client.insights_job(&key, &id, deadline).await {
                Ok(job) => {
                    if json {
                        emit_raw(&serde_json::to_value(&job).expect("json"));
                    } else {
                        human_line(format!(
                            "{} {} {} {}%",
                            job.site,
                            job.id,
                            format!("{:?}", job.status).to_ascii_lowercase(),
                            job.percent_complete
                        ));
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Ads(AdsCmd::InsightsJob(InsightsJobCmd::Result {
            site,
            id,
            from,
            to,
            level,
            metrics,
            attribution,
            ad_account,
            entity_ids,
            breakdowns,
            report,
        })) => {
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
                    report,
                },
            )
            .map_err(|e| fail(&e, json))?;
            let key = AccountKey::new(&site, &account);
            match client.insights_job_result(&key, &id, query, deadline).await {
                Ok(reply) => emit_insights_reply(&reply, json),
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Ads(AdsCmd::InsightsJob(InsightsJobCmd::Cancel { site, id, yes })) => {
            if !yes {
                eprintln!("pass --yes to cancel insights job {id}");
                return Err(2);
            }
            let key = AccountKey::new(&site, &account);
            match client.cancel_insights_job(&key, &id, deadline).await {
                Ok(job) => {
                    if json {
                        emit_raw(&serde_json::to_value(&job).expect("json"));
                    } else {
                        human_line(format!("{} {} cancelled", job.site, job.id));
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Ads(AdsCmd::InspectToken { site }) => {
            let key = AccountKey::new(&site, &account);
            match client.inspect_ads_token(&key, deadline).await {
                Ok(inspection) => {
                    if json {
                        emit_raw(&serde_json::to_value(&inspection).expect("json"));
                    } else {
                        human_line(format!(
                            "{} {} valid={} type={}",
                            inspection.site,
                            inspection.token_kind.as_str(),
                            inspection.is_valid,
                            inspection.debug_type.as_deref().unwrap_or("-")
                        ));
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
            }
        }
        Commands::Ads(AdsCmd::AccessTier { site }) => {
            let key = AccountKey::new(&site, &account);
            match client.ads_access_tier(&key, deadline).await {
                Ok(tier) => {
                    if json {
                        emit_raw(&serde_json::to_value(&tier).expect("json"));
                    } else {
                        human_line(format!(
                            "{} {} {} ({})",
                            tier.site,
                            tier.tier.as_str(),
                            tier.raw.as_deref().unwrap_or("-"),
                            tier.dashboard
                        ));
                    }
                    Ok(())
                }
                Err(e) => Err(fail(&e, json)),
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
            daily_budget,
            lifetime_budget,
            adset_budget_sharing,
        }) => {
            let request = build_paused_campaign_request(
                &site,
                PausedCampaignOptions {
                    ad_account,
                    name,
                    objective,
                    special_ad_categories,
                    daily_budget,
                    lifetime_budget,
                    is_adset_budget_sharing_enabled: adset_budget_sharing,
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
        Commands::Ads(AdsCmd::CreateAdset {
            site,
            ad_account,
            name,
            campaign_id,
            daily_budget,
            lifetime_budget,
            bid_strategy,
            bid_amount,
            roas_average_floor,
            billing_event,
            optimization_goal,
            countries,
            age_min,
            age_max,
            publisher_platforms,
            facebook_positions,
            instagram_positions,
        }) => {
            let request = build_paused_adset_request(
                &site,
                PausedAdsetOptions {
                    ad_account,
                    name,
                    campaign_id,
                    daily_budget,
                    lifetime_budget,
                    bid_strategy,
                    bid_amount,
                    roas_average_floor,
                    billing_event,
                    optimization_goal,
                    countries,
                    age_min,
                    age_max,
                    publisher_platforms,
                    facebook_positions,
                    instagram_positions,
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
            async_report,
            report,
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
                    report,
                },
            )
            .map_err(|e| fail(&e, json))?;
            let key = AccountKey::new(&site, &account);
            if async_report {
                return run_async_insights(&client, &key, query, deadline, json).await;
            }
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
            system_user,
        } => {
            if listen {
                eprintln!("--listen is not implemented in this scaffold; paste the code instead");
                return Err(2);
            }
            let key = AccountKey::new(&site, &account);
            let result = if system_user {
                if site != "meta_ads" {
                    eprintln!("--system-user is only valid for meta_ads");
                    return Err(2);
                }
                let Some(token) = token else {
                    eprintln!("--system-user requires --token");
                    return Err(2);
                };
                if code.is_some() || password.is_some() {
                    eprintln!("--system-user cannot be combined with --code or --password");
                    return Err(2);
                }
                client
                    .put_ads_system_user_token(&key, &token, deadline)
                    .await
            } else if let Some(password) = password {
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
        InsightsLevel, LinkCallToAction, Metric, PausedAdCreate, PausedAdset, PausedCampaign,
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
                report: "performance".into(),
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
                    report: "performance".into(),
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
                report: "performance".into(),
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
        let cli = Cli::try_parse_from([
            "postkit",
            "auth",
            "meta_ads",
            "--token",
            "SYS",
            "--system-user",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Auth {
                system_user: true,
                token: Some(ref token),
                ..
            } if token == "SYS"
        ));
        let cli = Cli::try_parse_from(["postkit", "ads", "inspect-token", "meta_ads"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Ads(AdsCmd::InspectToken { site }) if site == "meta_ads"
        ));
        let cli = Cli::try_parse_from(["postkit", "ads", "access-tier", "meta_ads"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Ads(AdsCmd::AccessTier { site }) if site == "meta_ads"
        ));
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

        let structured = Cli::try_parse_from([
            "postkit",
            "whatsapp",
            "send",
            "--request",
            "request.json",
            "--sender",
            "marketing",
            "--allow-send",
        ])
        .unwrap();
        assert!(whatsapp_send_allowed(&structured.command));
        assert!(matches!(
            structured.command,
            Commands::WhatsApp(WhatsAppCmd::Send { sender: Some(sender), .. }) if sender == "marketing"
        ));
    }

    #[test]
    fn whatsapp_config_is_phone_only_and_never_needs_oauth_fields() {
        let cfg = whatsapp_app_config(
            "123456789".into(),
            None,
            None,
            Some("app-secret".into()),
            None,
            vec![],
        )
        .unwrap();
        assert!(cfg.oauth.is_none());
        assert_eq!(cfg.extra["phone_number_id"].as_str(), Some("123456789"));
        let senders = parse_whatsapp_senders(&["marketing=987654321".into()], true).unwrap();
        assert_eq!(senders[0].alias, "marketing");
        assert!(parse_whatsapp_senders(&["bad-value".into()], true).is_err());
        assert!(whatsapp_app_config("+6012".into(), None, None, None, None, vec![]).is_err());
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
            "--country",
            "MY",
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
            "--country",
            "MY",
        ])
        .unwrap();
        assert!(matches!(
            adset_cli.command,
            Commands::Ads(AdsCmd::CreateAdset { bid_strategy, .. })
                if bid_strategy == "lowest_cost_without_cap"
        ));

        let campaign = build_paused_campaign_request(
            "meta_ads",
            PausedCampaignOptions {
                ad_account: Some("act_123".into()),
                name: "Paused validation".into(),
                objective: "sales".into(),
                special_ad_categories: "HOUSING, EMPLOYMENT".into(),
                daily_budget: None,
                lifetime_budget: None,
                is_adset_budget_sharing_enabled: false,
            },
        )
        .unwrap();
        assert!(matches!(
            campaign.create,
            PausedAdCreate::Campaign(PausedCampaign { objective: CampaignObjective::Sales, special_ad_categories, .. })
                if special_ad_categories == ["HOUSING", "EMPLOYMENT"]
        ));

        let cbo = build_paused_campaign_request(
            "meta_ads",
            PausedCampaignOptions {
                ad_account: None,
                name: "CBO".into(),
                objective: "awareness".into(),
                special_ad_categories: String::new(),
                daily_budget: Some(5000),
                lifetime_budget: None,
                is_adset_budget_sharing_enabled: true,
            },
        )
        .unwrap();
        assert!(matches!(
            cbo.create,
            PausedAdCreate::Campaign(PausedCampaign {
                daily_budget: Some(5000),
                is_adset_budget_sharing_enabled: true,
                ..
            })
        ));
        let both_budgets = build_paused_campaign_request(
            "meta_ads",
            PausedCampaignOptions {
                ad_account: None,
                name: "x".into(),
                objective: "awareness".into(),
                special_ad_categories: String::new(),
                daily_budget: Some(100),
                lifetime_budget: Some(200),
                is_adset_budget_sharing_enabled: false,
            },
        );
        assert!(
            matches!(both_budgets, Err(Error::InvalidQuery { reason, .. }) if reason == "daily_and_lifetime_budget_mutually_exclusive")
        );

        let adset = build_paused_adset_request(
            "meta_ads",
            PausedAdsetOptions {
                ad_account: None,
                name: "Paused ad set".into(),
                campaign_id: "100".into(),
                daily_budget: Some(2500),
                lifetime_budget: None,
                bid_strategy: "lowest_cost_without_cap".into(),
                bid_amount: None,
                roas_average_floor: None,
                billing_event: "IMPRESSIONS".into(),
                optimization_goal: "REACH".into(),
                countries: vec!["MY".into()],
                age_min: None,
                age_max: None,
                publisher_platforms: vec![],
                facebook_positions: vec![],
                instagram_positions: vec![],
            },
        )
        .unwrap();
        assert!(matches!(adset.create, PausedAdCreate::Adset(_)));

        let lifetime = build_paused_adset_request(
            "meta_ads",
            PausedAdsetOptions {
                ad_account: None,
                name: "Lifetime set".into(),
                campaign_id: "100".into(),
                daily_budget: None,
                lifetime_budget: Some(20_000),
                bid_strategy: "lowest_cost_without_cap".into(),
                bid_amount: None,
                roas_average_floor: None,
                billing_event: "IMPRESSIONS".into(),
                optimization_goal: "REACH".into(),
                countries: vec!["MY".into()],
                age_min: None,
                age_max: None,
                publisher_platforms: vec![],
                facebook_positions: vec![],
                instagram_positions: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            lifetime.create,
            PausedAdCreate::Adset(PausedAdset {
                lifetime_budget: Some(20_000),
                daily_budget: None,
                ..
            })
        ));

        let bad_bid_strategy = build_paused_adset_request(
            "meta_ads",
            PausedAdsetOptions {
                ad_account: None,
                name: "x".into(),
                campaign_id: "100".into(),
                daily_budget: Some(1),
                lifetime_budget: None,
                bid_strategy: "cost_cap".into(),
                bid_amount: None,
                roas_average_floor: None,
                billing_event: "IMPRESSIONS".into(),
                optimization_goal: "REACH".into(),
                countries: vec!["MY".into()],
                age_min: None,
                age_max: None,
                publisher_platforms: vec![],
                facebook_positions: vec![],
                instagram_positions: vec![],
            },
        );
        assert!(
            matches!(bad_bid_strategy, Err(Error::InvalidQuery { reason, .. }) if reason == "missing_bid_amount")
        );
        let cost_cap = build_paused_adset_request(
            "meta_ads",
            PausedAdsetOptions {
                ad_account: None,
                name: "cap".into(),
                campaign_id: "100".into(),
                daily_budget: Some(2500),
                lifetime_budget: None,
                bid_strategy: "cost_cap".into(),
                bid_amount: Some(200),
                roas_average_floor: None,
                billing_event: "IMPRESSIONS".into(),
                optimization_goal: "REACH".into(),
                countries: vec!["MY".into()],
                age_min: None,
                age_max: None,
                publisher_platforms: vec![],
                facebook_positions: vec![],
                instagram_positions: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            cost_cap.create,
            PausedAdCreate::Adset(PausedAdset {
                bid_strategy: postkit::BidStrategy::CostCap,
                bid_amount: Some(200),
                ..
            })
        ));
        let min_roas = build_paused_adset_request(
            "meta_ads",
            PausedAdsetOptions {
                ad_account: None,
                name: "roas".into(),
                campaign_id: "100".into(),
                daily_budget: Some(2500),
                lifetime_budget: None,
                bid_strategy: "lowest_cost_with_min_roas".into(),
                bid_amount: None,
                roas_average_floor: Some(10_000),
                billing_event: "IMPRESSIONS".into(),
                optimization_goal: "REACH".into(),
                countries: vec!["MY".into()],
                age_min: None,
                age_max: None,
                publisher_platforms: vec![],
                facebook_positions: vec![],
                instagram_positions: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            min_roas.create,
            PausedAdCreate::Adset(PausedAdset {
                bid_strategy: postkit::BidStrategy::LowestCostWithMinRoas,
                roas_average_floor: Some(10_000),
                ..
            })
        ));

        let clicks = build_paused_campaign_request(
            "meta_ads",
            PausedCampaignOptions {
                ad_account: None,
                name: "x".into(),
                objective: "clicks".into(),
                special_ad_categories: String::new(),
                daily_budget: None,
                lifetime_budget: None,
                is_adset_budget_sharing_enabled: false,
            },
        );
        assert!(
            matches!(clicks, Err(Error::InvalidQuery { reason, .. }) if reason == "unknown_objective:clicks")
        );
        let no_country = build_paused_adset_request(
            "meta_ads",
            PausedAdsetOptions {
                ad_account: None,
                name: "x".into(),
                campaign_id: "100".into(),
                daily_budget: Some(1),
                lifetime_budget: None,
                bid_strategy: "lowest_cost_without_cap".into(),
                bid_amount: None,
                roas_average_floor: None,
                billing_event: "IMPRESSIONS".into(),
                optimization_goal: "REACH".into(),
                countries: vec![],
                age_min: None,
                age_max: None,
                publisher_platforms: vec![],
                facebook_positions: vec![],
                instagram_positions: vec![],
            },
        );
        assert!(
            matches!(no_country, Err(Error::InvalidQuery { reason, .. }) if reason == "targeting_missing_country")
        );
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
        assert!(help.contains("frequency,unique_clicks"));
        assert!(help.contains("quality_ranking,video_thruplay"));
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
        let cli = Cli::try_parse_from(["postkit", "mcp"]).unwrap();
        assert!(matches!(cli.command, Commands::Mcp));
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
