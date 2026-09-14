//! Clap types for `wirekern whatsapp` and its nested subcommands.
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub(crate) enum WhatsAppCmd {
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
        /// left the machine and the response was lost, Wirekern does not
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
pub(crate) enum WhatsAppMediaCmd {
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
pub(crate) enum WhatsAppTemplatesCmd {
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
pub(crate) enum WhatsAppFlowsCmd {
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
pub(crate) enum WhatsAppAccountCmd {
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
pub(crate) enum WhatsAppLedgerCmd {
    Get {
        #[arg(long)]
        wamid: String,
    },
    Window {
        #[arg(long)]
        wa_id: String,
    },
    /// List metadata for encrypted signed callbacks that failed local parsing.
    /// This is available only when WIREKERN_WHATSAPP_REPLAY_DLQ_KEY is set.
    DeadLetters,
    /// Re-parse one encrypted callback after upgrading Wirekern. A successful
    /// reduction deletes the ciphertext; a failure leaves it queued.
    Replay {
        #[arg(long)]
        id: String,
        #[arg(long)]
        yes: bool,
    },
    Purge {
        #[arg(long)]
        before_unix: u64,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum WhatsAppConsentCmd {
    Get {
        #[arg(long)]
        wa_id: String,
    },
    Set {
        #[arg(long)]
        wa_id: String,
        /// opt_in or opt_out. File-backed sends use this local record for
        /// conservative authorization; it never bypasses Meta's checks.
        #[arg(long)]
        kind: String,
        #[arg(long)]
        at_unix: Option<u64>,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum WhatsAppWebhookCmd {
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
