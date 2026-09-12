//! Closed MCP tool catalog.
//!
//! Every tool maps to an existing `Client` method. New verbs belong in the
//! kernel first; this file only adapts JSON-RPC arguments onto those types.

use postkit::{
    AccountKey, AttributionWindow, Breakdown, Client, DateRange, Deadline, Error, InsightsLevel,
    InsightsQuery, MediaQuery, Metric, PostRequest, Site, WhatsAppSendRequest, WireError,
    DEFAULT_MEDIA_LIMIT,
};
use serde::Deserialize;
use serde_json::{json, Value};

const DEFAULT_ACCOUNT: &str = "default";
const DEFAULT_DEADLINE_SECS: u64 = 30;

#[derive(Clone, Copy)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub read_only: bool,
    pub destructive: bool,
    pub schema: fn() -> Value,
}

pub fn catalog() -> &'static [ToolSpec] {
    &[
        ToolSpec {
            name: "capabilities",
            description: "List compiled Postkit connector capabilities. Optional site filter.",
            read_only: true,
            destructive: false,
            schema: capabilities_schema,
        },
        ToolSpec {
            name: "accounts_list",
            description: "List local vault account aliases. Returns names only, never tokens.",
            read_only: true,
            destructive: false,
            schema: accounts_schema,
        },
        ToolSpec {
            name: "whoami",
            description: "Resolve the stored account identity for one site. Does not print tokens.",
            read_only: true,
            destructive: false,
            schema: whoami_schema,
        },
        ToolSpec {
            name: "post",
            description: "Publish now through a configured site. Same PostRequest as HTTP POST /v1/posts. Not WhatsApp.",
            read_only: false,
            destructive: true,
            schema: post_schema,
        },
        ToolSpec {
            name: "whatsapp_send",
            description: "Send a typed WhatsApp Cloud message. Requires allow_send=true and an idempotency_key. Sender must be a configured alias.",
            read_only: false,
            destructive: true,
            schema: whatsapp_schema,
        },
        ToolSpec {
            name: "insights",
            description: "Read Meta Ads spend/performance metrics. Attribution window is required. Range ≤ 90 days.",
            read_only: true,
            destructive: false,
            schema: insights_schema,
        },
        ToolSpec {
            name: "ads_accounts",
            description: "List remote advertising accounts for the stored credential. Not local vault aliases.",
            read_only: true,
            destructive: false,
            schema: site_account_schema,
        },
        ToolSpec {
            name: "pages_accounts",
            description: "List Facebook Pages visible to the stored credential. Returns identities, never Page tokens.",
            read_only: true,
            destructive: false,
            schema: site_account_schema,
        },
        ToolSpec {
            name: "media_list",
            description: "Read one bounded first page of published media (limit 1–25).",
            read_only: true,
            destructive: false,
            schema: media_schema,
        },
    ]
}

pub fn list_tools_result() -> Value {
    let tools: Vec<Value> = catalog()
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": (tool.schema)(),
                "annotations": {
                    "readOnlyHint": tool.read_only,
                    "destructiveHint": tool.destructive,
                    "openWorldHint": !tool.read_only,
                    "idempotentHint": tool.read_only
                }
            })
        })
        .collect();
    json!({ "tools": tools })
}

pub fn unknown_tool_message(name: &str) -> String {
    format!("Unknown tool: {name}")
}

pub fn tool_ok(value: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": value.to_string() }],
        "structuredContent": value,
        "isError": false
    })
}

pub fn tool_err(error: Error) -> Value {
    let wire = serde_json::to_value(WireError::from(&error)).expect("WireError serializes");
    json!({
        "content": [{ "type": "text", "text": wire.to_string() }],
        "structuredContent": wire,
        "isError": true
    })
}

fn capabilities_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "site": { "type": "string", "description": "Connector id such as threads or whatsapp_cloud." }
        },
        "additionalProperties": false
    })
}

fn accounts_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "site": { "type": "string", "description": "Optional vault site filter." }
        },
        "additionalProperties": false
    })
}

fn whoami_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "site": { "type": "string" },
            "account": { "type": "string", "default": "default" }
        },
        "required": ["site"],
        "additionalProperties": false
    })
}

fn post_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "account": { "type": "string", "default": "default" },
            "target": {
                "type": "object",
                "properties": { "site": { "type": "string" } },
                "required": ["site"]
            },
            "body": {
                "type": "object",
                "properties": { "type": { "type": "string" } },
                "required": ["type"]
            },
            "idempotency_key": { "type": "string" },
            "deadline": { "type": "integer", "minimum": 1, "default": 30 }
        },
        "required": ["target", "body"],
        "additionalProperties": false
    })
}

fn whatsapp_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "account": { "type": "string", "default": "default" },
            "allow_send": { "type": "boolean" },
            "sender": { "type": "string", "description": "Configured sender alias, never a raw phone-number ID." },
            "message": { "type": "object" },
            "idempotency_key": { "type": "string" },
            "recipient_type": { "type": "string", "enum": ["individual", "group"] },
            "deadline": { "type": "integer", "minimum": 1, "default": 30 }
        },
        "required": ["allow_send", "message", "idempotency_key"],
        "additionalProperties": false
    })
}

fn site_account_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "site": { "type": "string" },
            "account": { "type": "string", "default": "default" },
            "deadline": { "type": "integer", "minimum": 1, "default": 30 }
        },
        "required": ["site"],
        "additionalProperties": false
    })
}

fn insights_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "site": { "type": "string", "default": "meta_ads" },
            "account": { "type": "string", "default": "default" },
            "from": { "type": "string", "description": "Inclusive YYYY-MM-DD." },
            "to": { "type": "string", "description": "Inclusive YYYY-MM-DD." },
            "attribution": { "type": "string", "enum": ["7d_click_1d_view", "1d_click", "1d_view"] },
            "level": { "type": "string", "enum": ["account", "campaign", "adset", "ad"], "default": "account" },
            "metrics": { "type": "array", "items": { "type": "string" } },
            "ad_account": { "type": "string" },
            "entity_ids": { "type": "array", "items": { "type": "string" } },
            "breakdowns": { "type": "array", "items": { "type": "string" } },
            "deadline": { "type": "integer", "minimum": 1, "default": 30 }
        },
        "required": ["from", "to", "attribution"],
        "additionalProperties": false
    })
}

fn media_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "site": { "type": "string" },
            "account": { "type": "string", "default": "default" },
            "limit": { "type": "integer", "minimum": 1, "maximum": 25, "default": 10 },
            "deadline": { "type": "integer", "minimum": 1, "default": 30 }
        },
        "required": ["site"],
        "additionalProperties": false
    })
}

#[derive(Deserialize, Default)]
struct SiteFilter {
    site: Option<String>,
}

#[derive(Deserialize)]
struct WhoAmIArgs {
    site: String,
    #[serde(default = "default_account")]
    account: String,
}

fn default_account() -> String {
    DEFAULT_ACCOUNT.into()
}

fn default_deadline() -> u64 {
    DEFAULT_DEADLINE_SECS
}

#[derive(Deserialize)]
struct PostToolArgs {
    #[serde(flatten)]
    request: PostRequest,
    #[serde(default)]
    idempotency_key: Option<String>,
    #[serde(default = "default_deadline")]
    deadline: u64,
}

#[derive(Deserialize)]
struct WhatsAppToolArgs {
    #[serde(default = "default_account")]
    account: String,
    #[serde(default)]
    allow_send: bool,
    sender: Option<String>,
    #[serde(default = "default_deadline")]
    deadline: u64,
    #[serde(flatten)]
    request: WhatsAppSendRequest,
}

pub fn capabilities(client: &Client, arguments: Value) -> Value {
    let filter: SiteFilter = match serde_json::from_value(arguments) {
        Ok(value) => value,
        Err(error) => return tool_err(invalid_query("", format!("json:{error}"))),
    };
    let all = client.registry().capabilities_json();
    match filter.site {
        Some(site) => match all.get(&site).cloned() {
            Some(caps) => tool_ok(json!({ "site": site, "capabilities": caps })),
            None => tool_err(Error::UnknownSite(Site::new(site))),
        },
        None => tool_ok(all),
    }
}

pub fn accounts_list(client: &Client, arguments: Value) -> Value {
    let filter: SiteFilter = match serde_json::from_value(arguments) {
        Ok(value) => value,
        Err(error) => return tool_err(invalid_query("", format!("json:{error}"))),
    };
    let site = filter.site.as_deref().map(Site::new);
    match client.vault().list(site.as_ref()) {
        Ok(keys) => {
            let rows: Vec<Value> = keys
                .iter()
                .map(|key| json!({ "site": key.site, "name": key.name }))
                .collect();
            tool_ok(json!({ "accounts": rows }))
        }
        Err(error) => tool_err(error),
    }
}

pub async fn whoami(client: &Client, arguments: Value) -> Value {
    let args: WhoAmIArgs = match serde_json::from_value(arguments) {
        Ok(value) => value,
        Err(error) => return tool_err(invalid_query("", format!("json:{error}"))),
    };
    let key = AccountKey::new(&args.site, &args.account);
    match client.whoami(&key).await {
        Ok(who) => match serde_json::to_value(who) {
            Ok(value) => tool_ok(value),
            Err(error) => tool_err(invalid_query(&args.site, format!("json:{error}"))),
        },
        Err(error) => tool_err(error),
    }
}

pub async fn post(client: &Client, arguments: Value) -> Value {
    let args: PostToolArgs = match serde_json::from_value(arguments) {
        Ok(value) => value,
        Err(error) => return tool_err(invalid_query("", format!("json:{error}"))),
    };
    let (key, mut intent) = match args.request.into_key_intent() {
        Ok(value) => value,
        Err(error) => return tool_err(error),
    };
    if let Some(idem) = args.idempotency_key.filter(|key| !key.is_empty()) {
        intent.idempotency_key = Some(idem);
    }
    match client
        .publish(&key, intent, Deadline::from_secs(args.deadline.max(1)))
        .await
    {
        Ok(outcome) => match serde_json::to_value(outcome) {
            Ok(value) => tool_ok(value),
            Err(error) => tool_err(invalid_query(key.site.as_str(), format!("json:{error}"))),
        },
        Err(error) => tool_err(error),
    }
}

pub async fn whatsapp_send(allowed: &Client, arguments: Value) -> Value {
    let args: WhatsAppToolArgs = match serde_json::from_value(arguments) {
        Ok(value) => value,
        Err(error) => return tool_err(invalid_query("whatsapp_cloud", format!("json:{error}"))),
    };
    // Gate before the allowing client is used, matching HTTP POST /v1/whatsapp.
    if !args.allow_send {
        return tool_err(Error::PolicyDenied {
            site: Site::new("whatsapp_cloud"),
            action: "send_whatsapp".into(),
            reason: "explicit_whatsapp_send_required".into(),
        });
    }
    let key = AccountKey::new("whatsapp_cloud", &args.account);
    match allowed
        .send_whatsapp_from(
            &key,
            args.sender.as_deref(),
            args.request,
            Deadline::from_secs(args.deadline.max(1)),
        )
        .await
    {
        Ok(outcome) => match serde_json::to_value(outcome) {
            Ok(value) => tool_ok(value),
            Err(error) => tool_err(invalid_query("whatsapp_cloud", format!("json:{error}"))),
        },
        Err(error) => tool_err(error),
    }
}

#[derive(Deserialize)]
struct SiteAccountArgs {
    site: String,
    #[serde(default = "default_account")]
    account: String,
    #[serde(default = "default_deadline")]
    deadline: u64,
}

#[derive(Deserialize)]
struct InsightsArgs {
    #[serde(default = "default_ads_site")]
    site: String,
    #[serde(default = "default_account")]
    account: String,
    from: String,
    to: String,
    attribution: String,
    #[serde(default = "default_level")]
    level: String,
    #[serde(default)]
    metrics: Option<Vec<String>>,
    ad_account: Option<String>,
    #[serde(default)]
    entity_ids: Vec<String>,
    #[serde(default)]
    breakdowns: Vec<String>,
    #[serde(default)]
    report: Option<String>,
    #[serde(default = "default_deadline")]
    deadline: u64,
}

#[derive(Deserialize)]
struct MediaArgs {
    site: String,
    #[serde(default = "default_account")]
    account: String,
    #[serde(default = "default_media_limit")]
    limit: u8,
    #[serde(default = "default_deadline")]
    deadline: u64,
}

fn default_ads_site() -> String {
    "meta_ads".into()
}

fn default_level() -> String {
    "account".into()
}

fn default_media_limit() -> u8 {
    DEFAULT_MEDIA_LIMIT
}

pub async fn insights(client: &Client, arguments: Value) -> Value {
    let args: InsightsArgs = match serde_json::from_value(arguments) {
        Ok(value) => value,
        Err(error) => return tool_err(invalid_query("meta_ads", format!("json:{error}"))),
    };
    let query = match insights_query(&args) {
        Ok(query) => query,
        Err(error) => return tool_err(error),
    };
    let key = AccountKey::new(&args.site, &args.account);
    match client
        .insights(&key, query, Deadline::from_secs(args.deadline.max(1)))
        .await
    {
        Ok(reply) => value_ok(&args.site, reply),
        Err(error) => tool_err(error),
    }
}

pub async fn ads_accounts(client: &Client, arguments: Value) -> Value {
    let args: SiteAccountArgs = match serde_json::from_value(arguments) {
        Ok(value) => value,
        Err(error) => return tool_err(invalid_query("", format!("json:{error}"))),
    };
    let key = AccountKey::new(&args.site, &args.account);
    match client
        .ad_accounts(&key, Deadline::from_secs(args.deadline.max(1)))
        .await
    {
        Ok(reply) => value_ok(&args.site, reply),
        Err(error) => tool_err(error),
    }
}

pub async fn pages_accounts(client: &Client, arguments: Value) -> Value {
    let args: SiteAccountArgs = match serde_json::from_value(arguments) {
        Ok(value) => value,
        Err(error) => return tool_err(invalid_query("", format!("json:{error}"))),
    };
    let key = AccountKey::new(&args.site, &args.account);
    match client
        .pages(&key, Deadline::from_secs(args.deadline.max(1)))
        .await
    {
        Ok(reply) => value_ok(&args.site, reply),
        Err(error) => tool_err(error),
    }
}

pub async fn media_list(client: &Client, arguments: Value) -> Value {
    let args: MediaArgs = match serde_json::from_value(arguments) {
        Ok(value) => value,
        Err(error) => return tool_err(invalid_query("", format!("json:{error}"))),
    };
    let key = AccountKey::new(&args.site, &args.account);
    match client
        .media(
            &key,
            MediaQuery { limit: args.limit },
            Deadline::from_secs(args.deadline.max(1)),
        )
        .await
    {
        Ok(reply) => value_ok(&args.site, reply),
        Err(error) => tool_err(error),
    }
}

fn insights_query(args: &InsightsArgs) -> Result<InsightsQuery, Error> {
    let site = args.site.as_str();
    let level: InsightsLevel = args
        .level
        .parse()
        .map_err(|reason| invalid_query(site, reason))?;
    let attribution: AttributionWindow = args
        .attribution
        .parse()
        .map_err(|reason| invalid_query(site, reason))?;
    let metrics = match &args.metrics {
        Some(values) if !values.is_empty() => values
            .iter()
            .map(|metric| metric.parse())
            .collect::<Result<Vec<Metric>, String>>()
            .map_err(|reason| invalid_query(site, reason))?,
        _ => vec![
            Metric::Spend,
            Metric::Impressions,
            Metric::Clicks,
            Metric::Purchases,
        ],
    };
    let breakdowns = args
        .breakdowns
        .iter()
        .map(|item| item.parse())
        .collect::<Result<Vec<Breakdown>, String>>()
        .map_err(|reason| invalid_query(site, reason))?;
    Ok(InsightsQuery {
        level,
        metrics,
        range: DateRange {
            from: args.from.clone(),
            to: args.to.clone(),
        },
        attribution,
        account: args.ad_account.clone(),
        entity_ids: args.entity_ids.clone(),
        breakdowns,
        report: args
            .report
            .as_deref()
            .unwrap_or("performance")
            .parse()
            .map_err(|reason| invalid_query(site, reason))?,
    })
}

fn value_ok<T: serde::Serialize>(site: &str, value: T) -> Value {
    match serde_json::to_value(value) {
        Ok(json) => tool_ok(json),
        Err(error) => tool_err(invalid_query(site, format!("json:{error}"))),
    }
}

fn invalid_query(site: &str, reason: String) -> Error {
    Error::InvalidQuery {
        site: Site::new(site),
        reason,
    }
}
