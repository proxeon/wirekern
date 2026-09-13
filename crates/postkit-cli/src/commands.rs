//! Top-level `postkit` subcommands.
use crate::accounts::AccountsCmd;
use crate::ads::AdsCmd;
use crate::apps::AppsCmd;
use crate::keys::KeysCmd;
use crate::media::MediaCmd;
use crate::pages::PagesCmd;
use crate::whatsapp::WhatsAppCmd;
use crate::x::XCmd;
use clap::Subcommand;

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Commands {
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
        /// Reply to an existing post: a threads media id or a bluesky
        /// at:// URI. Anchors the first post of a --text chain. Single
        /// target only — ids are site-specific, so a fan-out cannot
        /// carry one honest value.
        #[arg(long = "reply-to", value_name = "ID")]
        reply_to: Option<String>,
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
        /// Omit the value (`--alt`) for an explicit empty string.
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        alt: Option<String>,
        /// Facebook Page id. Sugar for `--param page_id=…`. facebook_pages
        /// only; exclusive with `--param page_id=`.
        #[arg(long = "page-id", value_name = "ID")]
        page_id: Option<String>,
    },
    Auth {
        site: String,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        code: Option<String>,
        /// Bluesky app password. Omit the value to prompt on a TTY.
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        password: Option<String>,
        /// Meta Ads: store a Business Manager System User token. Requires
        /// `--token`. Refuses a user OAuth token reused as a service secret.
        #[arg(long)]
        system_user: bool,
        /// X only: request the private DM scopes in addition to public-post
        /// scopes. Re-authorize whenever you add this capability.
        #[arg(long)]
        with_dm: bool,
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
        until: String,
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
        #[arg(long = "async-report", alias = "async")]
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
    /// `--page-id <id>` (or `--param page_id=<id>`) for an explicit organic
    /// publish target.
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
    /// X-specific private-message operations. Public text posts stay under
    /// `post x --text ...`; DMs require an explicit acknowledgement.
    #[command(subcommand)]
    X(XCmd),
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

pub(crate) fn whatsapp_send_allowed(command: &Commands) -> bool {
    match command {
        Commands::WhatsApp(cmd) => crate::whatsapp::send_allowed(cmd),
        _ => false,
    }
}

pub(crate) fn x_direct_message_allowed(command: &Commands) -> bool {
    match command {
        Commands::X(command) => crate::x::direct_message_allowed(command),
        _ => false,
    }
}
