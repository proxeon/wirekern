//! Closed MCP tool catalog.
//!
//! Every tool maps to an existing `Client` method. New verbs belong in the
//! kernel first; this file only adapts JSON-RPC arguments onto those types.

use postkit::{AccountKey, Client, Error, Site, WireError};
use serde::Deserialize;
use serde_json::{json, Value};

const DEFAULT_ACCOUNT: &str = "default";

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

fn invalid_query(site: &str, reason: String) -> Error {
    Error::InvalidQuery {
        site: Site::new(site),
        reason,
    }
}
