//! Clap types for `postkit ads` and nested insights-job subcommands.
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum InsightsJobCmd {
    Status {
        site: String,
        #[arg(long)]
        id: String,
    },
    /// Fetch completed rows. Omit `--from`/`--until`/`--attribution` to reuse
    /// the query cached when `insights --async-report` started the job.
    Result {
        site: String,
        #[arg(long)]
        id: String,
        /// Inclusive YYYY-MM-DD. Omit with `--until` and `--attribution` to
        /// reuse the query cached when the job started.
        #[arg(long)]
        from: Option<String>,
        /// Inclusive YYYY-MM-DD.
        #[arg(long)]
        until: Option<String>,
        #[arg(long, default_value = "account")]
        level: String,
        #[arg(long, default_value = "spend,impressions,clicks,purchases")]
        metrics: String,
        /// Required together with `--from` and `--until`, or omit all three
        /// to reuse the query cached when the job started.
        #[arg(long)]
        attribution: Option<String>,
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
#[allow(clippy::large_enum_variant)]
pub(crate) enum AdsCmd {
    /// List Meta ad accounts visible to the selected credential.
    Accounts { site: String },
    /// List campaigns, ad sets, ads, or creatives in one ad account.
    /// Capped pages, sorted by id. GET-only; cannot activate.
    List {
        site: String,
        /// campaign | adset | ad | creative.
        #[arg(long)]
        entity: String,
        /// Override the stored ad account (123 or act_123).
        #[arg(long)]
        ad_account: Option<String>,
    },
    /// Read budget, bid, targeting, Page, and destination on one known object.
    /// GET-only; needed before any later activate. Cannot change delivery.
    Inspect {
        site: String,
        /// campaign | adset | ad | creative.
        #[arg(long)]
        entity: String,
        /// Existing Meta campaign, ad set, ad, or creative ID.
        #[arg(long)]
        id: String,
    },
    /// Confirmed PAUSED → ACTIVE. Default policy denies this. Echo the
    /// object id and, when the object has a budget, its current minor units.
    Activate {
        site: String,
        /// campaign | adset | ad.
        #[arg(long)]
        entity: String,
        #[arg(long)]
        id: String,
        /// Must equal `--id`.
        #[arg(long)]
        confirm_id: String,
        /// Policy opt-in. Without it activate is `paused_only` before vault.
        #[arg(long)]
        allow_activate: bool,
        #[arg(long)]
        confirm_daily_budget: Option<u64>,
        #[arg(long)]
        confirm_lifetime_budget: Option<u64>,
        /// Required write-ahead marker. A leftover in_flight is reconciliation, not a retry.
        #[arg(long)]
        state: PathBuf,
    },
    /// Emergency ACTIVE → PAUSED. Allowed by default; cannot start spend.
    Pause {
        site: String,
        /// campaign | adset | ad.
        #[arg(long)]
        entity: String,
        #[arg(long)]
        id: String,
    },
    /// Archive a campaign, ad set, or ad. Default policy denies.
    Archive {
        site: String,
        #[arg(long)]
        entity: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        confirm_id: String,
        #[arg(long)]
        allow_archive: bool,
    },
    /// Delete a campaign, ad set, or ad. Irreversible to live.
    Delete {
        site: String,
        #[arg(long)]
        entity: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        confirm_id: String,
        #[arg(long)]
        allow_delete: bool,
        /// Must be passed. Confirms the operator intends destruction.
        #[arg(long)]
        confirm_delete: bool,
    },
    /// Copy as PAUSED. Never inherits ACTIVE.
    Duplicate {
        site: String,
        #[arg(long)]
        entity: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        confirm_id: String,
        #[arg(long)]
        allow_duplicate: bool,
        #[arg(long)]
        confirm_daily_budget: Option<u64>,
        #[arg(long)]
        confirm_lifetime_budget: Option<u64>,
    },
    UpdateBudget {
        site: String,
        #[arg(long)]
        entity: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        confirm_id: String,
        #[arg(long)]
        allow_budget_edit: bool,
        #[arg(long)]
        current_daily_budget: u64,
        #[arg(long)]
        new_daily_budget: u64,
        #[arg(long, default_value_t = 0.2)]
        max_change_ratio: f64,
    },
    UpdateLifetimeBudget {
        site: String,
        #[arg(long)]
        entity: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        confirm_id: String,
        #[arg(long)]
        allow_budget_edit: bool,
        #[arg(long)]
        current_lifetime_budget: u64,
        #[arg(long)]
        new_lifetime_budget: u64,
        #[arg(long, default_value_t = 0.2)]
        max_change_ratio: f64,
    },
    UpdateBid {
        site: String,
        #[arg(long)]
        entity: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        confirm_id: String,
        #[arg(long)]
        allow_bid_edit: bool,
        #[arg(long)]
        bid_strategy: String,
        #[arg(long)]
        bid_amount: Option<u64>,
        #[arg(long)]
        roas_average_floor: Option<u64>,
    },
    UpdateSchedule {
        site: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        confirm_id: String,
        #[arg(long)]
        allow_schedule_edit: bool,
        #[arg(long)]
        start_time: Option<String>,
        #[arg(long)]
        end_time: Option<String>,
    },
    UpdatePlacement {
        site: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        confirm_id: String,
        #[arg(long)]
        allow_placement_edit: bool,
        #[arg(long)]
        publisher_platform: Vec<String>,
        #[arg(long)]
        facebook_position: Vec<String>,
        #[arg(long)]
        instagram_position: Vec<String>,
        #[arg(long)]
        whatsapp_position: Vec<String>,
    },
    UpdateTargeting {
        site: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        confirm_id: String,
        #[arg(long)]
        allow_targeting_edit: bool,
        #[arg(long)]
        targeting_file: PathBuf,
    },
    SwapCreative {
        site: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        confirm_id: String,
        #[arg(long)]
        creative_id: String,
        #[arg(long)]
        allow_creative_swap: bool,
    },
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
    /// Upload a local video for a later Meta video creative. Encoding is a
    /// later status poll; uploading creates no ad.
    UploadVideo {
        site: String,
        #[arg(long)]
        ad_account: Option<String>,
        #[arg(long)]
        file: PathBuf,
        /// Poll Graph until ready/error or `--deadline`.
        #[arg(long)]
        wait: bool,
    },
    /// Read Meta `status.video_status` for an uploaded video.
    VideoStatus {
        site: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        wait: bool,
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
        /// learn_more | shop_now | sign_up | download | apply_now | book_now |
        /// subscribe | buy_now | contact_us | get_quote | order_now | call_now |
        /// like_page | whatsapp_message | get_directions | install_app
        #[arg(long)]
        call_to_action: String,
        /// Required for `--call-to-action get_directions`.
        #[arg(long)]
        geo_link: Option<String>,
        /// Required with `--app-link` for `install_app`.
        #[arg(long)]
        application_id: Option<String>,
        #[arg(long)]
        app_link: Option<String>,
        #[arg(long)]
        instagram_user_id: Option<String>,
        #[arg(long)]
        advantage_plus: bool,
        #[arg(long)]
        whatsapp_identity_id: Option<String>,
        #[arg(long)]
        whatsapp_phone_number: Option<String>,
    },
    /// Create a Page-backed video creative. The video must already be uploaded.
    CreateVideoCreative {
        site: String,
        #[arg(long)]
        ad_account: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        page_id: String,
        #[arg(long)]
        video_id: String,
        /// Thumbnail image hash from `upload-image`.
        #[arg(long)]
        image_hash: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        destination_url: String,
        #[arg(long)]
        call_to_action: String,
        #[arg(long)]
        geo_link: Option<String>,
        #[arg(long)]
        application_id: Option<String>,
        #[arg(long)]
        app_link: Option<String>,
        /// Wait until the video is encoded before creating the creative.
        #[arg(long)]
        wait: bool,
    },
    /// Carousel creative (2–10 cards from a JSON file). Always a library
    /// `create_ad_creative`; cannot deliver by itself.
    CreateCarouselCreative {
        site: String,
        #[arg(long)]
        ad_account: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        page_id: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        call_to_action: String,
        /// JSON array of `{image_hash,link,name}` cards.
        #[arg(long)]
        cards_file: PathBuf,
        #[arg(long)]
        instagram_user_id: Option<String>,
        #[arg(long)]
        advantage_plus: bool,
    },
    CreateCatalogCreative {
        site: String,
        #[arg(long)]
        ad_account: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        page_id: String,
        #[arg(long)]
        product_set_id: String,
        #[arg(long)]
        link: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        call_to_action: String,
        #[arg(long)]
        instagram_user_id: Option<String>,
        #[arg(long)]
        advantage_plus: bool,
    },
    CreateLeadFormCreative {
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
        #[arg(long)]
        lead_gen_form_id: String,
        #[arg(long)]
        call_to_action: String,
        #[arg(long)]
        instagram_user_id: Option<String>,
        #[arg(long)]
        advantage_plus: bool,
    },
    CreateAppInstallCreative {
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
        application_id: String,
        #[arg(long)]
        object_store_url: String,
        #[arg(long)]
        instagram_user_id: Option<String>,
        #[arg(long)]
        advantage_plus: bool,
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
        /// Meta `is_adset_budget_sharing_enabled`. ABO only; refused with CBO.
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
        /// RFC3339 start. Required pairing is local: end after start.
        #[arg(long)]
        start_time: Option<String>,
        /// RFC3339 end. Required with `--lifetime-budget`.
        #[arg(long)]
        end_time: Option<String>,
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
        #[arg(long = "whatsapp-position")]
        whatsapp_positions: Vec<String>,
        /// Required when WhatsApp Status is selected (v26.0 unknown-age default).
        #[arg(long)]
        user_age_unknown: Option<bool>,
        /// Page promoted object (`page_id`). XOR with pixel/app/product-set.
        #[arg(long)]
        promoted_page_id: Option<String>,
        /// Pixel promoted object. Requires `--custom-event-type`.
        #[arg(long)]
        promoted_pixel_id: Option<String>,
        #[arg(long)]
        custom_event_type: Option<String>,
        /// App promoted object. Requires `--object-store-url`.
        #[arg(long)]
        promoted_application_id: Option<String>,
        #[arg(long)]
        object_store_url: Option<String>,
        /// Catalog product-set promoted object. Requires `--custom-event-type`.
        #[arg(long)]
        promoted_product_set_id: Option<String>,
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
