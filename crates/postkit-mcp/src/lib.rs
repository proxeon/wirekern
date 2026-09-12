//! Local MCP stdio adapter. Same `Client::from_home` as CLI and HTTP serve.
//!
//! Stdout is the JSON-RPC pipe. Do not print `--json` documents here — a
//! host that spawned this process treats every stdout line as a protocol
//! message. Kernel errors become tool-execution `isError` results so the
//! model can correct arguments; protocol mistakes stay JSON-RPC errors.

mod protocol;
mod tools;

use protocol::{
    encode_message, is_notification, message_id, negotiate_protocol_version, parse_message,
    rpc_error, rpc_result, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, MAX_MESSAGE_BYTES,
    METHOD_NOT_FOUND,
};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use postkit::{Client, Error, USER_AGENT};

pub use protocol::{DEFAULT_PROTOCOL_VERSION, PARSE_ERROR, SUPPORTED_PROTOCOL_VERSIONS};

/// Two clients, same vault: the default one cannot send WhatsApp; the
/// allowing one is used only after a tool passes `allow_send: true`.
pub struct Server {
    client: Arc<Client>,
    whatsapp: Arc<Client>,
    initialized: AtomicBool,
}

impl Server {
    pub fn from_home(home: impl AsRef<Path>) -> Result<Self, Error> {
        let home = home.as_ref();
        Ok(Self::new(
            Client::from_home(home, false)?,
            Client::from_home(home, true)?,
        ))
    }

    /// Test seam: inject a mocked `Client` the same way `postkit-serve` does.
    pub fn new(client: Client, whatsapp: Client) -> Self {
        Self {
            client: Arc::new(client),
            whatsapp: Arc::new(whatsapp),
            initialized: AtomicBool::new(false),
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn whatsapp(&self) -> &Client {
        &self.whatsapp
    }

    /// One stdio line in, at most one compact JSON line out. Notifications
    /// (`id` omitted) never produce a response.
    pub async fn handle_line(&self, line: &str) -> Option<String> {
        // std::io::Lines keeps a trailing CR from Windows `\r\n` delimiters.
        // The MCP stdio transport is newline-delimited JSON, so strip it.
        let line = line.trim_end_matches('\r');
        if line.len() > MAX_MESSAGE_BYTES {
            return encode_message(&rpc_error(Value::Null, PARSE_ERROR, "Parse error")).ok();
        }
        let message = match parse_message(line) {
            Ok(value) => value,
            Err(error) => return encode_message(&error).ok(),
        };
        let reply = self.dispatch(message).await?;
        encode_message(&reply).ok()
    }

    async fn dispatch(&self, message: Value) -> Option<Value> {
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            let id = message_id(&message).unwrap_or(Value::Null);
            return Some(rpc_error(id, INVALID_REQUEST, "Invalid Request"));
        }
        let method = match message.get("method").and_then(Value::as_str) {
            Some(method) => method.to_string(),
            None => {
                if is_notification(&message) {
                    return None;
                }
                return Some(rpc_error(
                    message_id(&message).unwrap_or(Value::Null),
                    INVALID_REQUEST,
                    "Invalid Request",
                ));
            }
        };
        if is_notification(&message) {
            if method == "notifications/initialized" {
                self.initialized.store(true, Ordering::SeqCst);
            }
            return None;
        }
        let id = message_id(&message).unwrap_or(Value::Null);
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match method.as_str() {
            "initialize" => self.initialize(params),
            "ping" => Ok(json!({})),
            "tools/list" => self
                .require_initialize()
                .map(|_| tools::list_tools_result()),
            "tools/call" => match self.require_initialize() {
                Ok(()) => self.call_tool(params).await,
                Err(error) => Err(error),
            },
            _ => Err(rpc_error(id.clone(), METHOD_NOT_FOUND, "Method not found")),
        };
        Some(match result {
            Ok(value) => rpc_result(id, value),
            Err(error) => {
                // Kernel/tool helpers already built a full JSON-RPC error.
                if error.get("error").is_some() && error.get("jsonrpc").is_some() {
                    let mut error = error;
                    if let Some(obj) = error.as_object_mut() {
                        obj.insert("id".into(), id);
                    }
                    error
                } else {
                    rpc_error(id, INTERNAL_ERROR, "Internal error")
                }
            }
        })
    }

    fn initialize(&self, params: Value) -> Result<Value, Value> {
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let protocol_version = negotiate_protocol_version(requested);
        self.initialized.store(true, Ordering::SeqCst);
        Ok(json!({
            "protocolVersion": protocol_version,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "postkit",
                "title": "postkit",
                "version": env!("CARGO_PKG_VERSION"),
                "description": USER_AGENT
            },
            "instructions": "Local postkit execution kernel. Credentials stay in the operator vault. WhatsApp sends require allow_send=true. Interactive auth stays on the CLI."
        }))
    }

    fn require_initialize(&self) -> Result<(), Value> {
        if self.initialized.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(rpc_error(
                Value::Null,
                INVALID_REQUEST,
                "Server not initialized",
            ))
        }
    }

    async fn call_tool(&self, params: Value) -> Result<Value, Value> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        if name.is_empty() {
            return Err(rpc_error(Value::Null, INVALID_PARAMS, "Missing tool name"));
        }
        if !tools::catalog().iter().any(|tool| tool.name == name) {
            return Err(rpc_error(
                Value::Null,
                INVALID_PARAMS,
                &tools::unknown_tool_message(name),
            ));
        }
        // Hosts omit `arguments` or send JSON null for no-arg tools.
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(value) => value.clone(),
        };
        let result = match name {
            "capabilities" => tools::capabilities(&self.client, arguments),
            "accounts_list" => tools::accounts_list(&self.client, arguments),
            "whoami" => tools::whoami(&self.client, arguments).await,
            "post" => tools::post(&self.client, arguments).await,
            "whatsapp_send" => tools::whatsapp_send(&self.whatsapp, arguments).await,
            "insights" => tools::insights(&self.client, arguments).await,
            "ads_accounts" => tools::ads_accounts(&self.client, arguments).await,
            "ads_list" => tools::ads_list(&self.client, arguments).await,
            "ads_inspect" => tools::ads_inspect(&self.client, arguments).await,
            "ads_status" => tools::ads_status(&self.client, arguments).await,
            "ads_create_paused" => tools::ads_create_paused(&self.client, arguments).await,
            "pages_accounts" => tools::pages_accounts(&self.client, arguments).await,
            "media_list" => tools::media_list(&self.client, arguments).await,
            other => {
                return Err(rpc_error(
                    Value::Null,
                    INVALID_PARAMS,
                    &tools::unknown_tool_message(other),
                ))
            }
        };
        Ok(result)
    }
}

/// Blocking stdio loop. Tokio stdin line-reading keeps the JSON-RPC pipe
/// off a worker that might otherwise hold a connector HTTP wait.
pub async fn run(home: &Path) -> Result<(), Error> {
    let server = Server::from_home(home)?;
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        if let Some(reply) = server.handle_line(&line).await {
            stdout.write_all(reply.as_bytes())?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use postkit::Vault;
    use protocol::DEFAULT_PROTOCOL_VERSION;

    fn server() -> Server {
        let tmp = tempfile::tempdir().unwrap();
        Server::from_home(tmp.path()).unwrap()
    }

    async fn rpc(server: &Server, body: Value) -> Value {
        let line = serde_json::to_string(&body).unwrap();
        let reply = server.handle_line(&line).await.expect("response");
        assert!(!reply.contains('\n'));
        serde_json::from_str(&reply).unwrap()
    }

    #[tokio::test]
    async fn from_home_uses_the_bundled_registry() {
        let server = server();
        assert!(server.client().registry().capabilities_json().is_object());
        assert!(server.whatsapp().registry().capabilities_json().is_object());
    }

    #[tokio::test]
    async fn initialize_echoes_supported_protocol_version() {
        let server = server();
        let reply = rpc(
            &server,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "0" }
                }
            }),
        )
        .await;
        assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(reply["result"]["capabilities"]["tools"], json!({}));
        assert_eq!(reply["result"]["serverInfo"]["name"], "postkit");
    }

    #[tokio::test]
    async fn initialize_falls_back_for_unknown_versions() {
        let server = server();
        let reply = rpc(
            &server,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": "nope" }
            }),
        )
        .await;
        assert_eq!(reply["result"]["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn crlf_lines_and_null_arguments_are_accepted() {
        let server = server();
        let init = server
            .handle_line("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\r")
            .await
            .unwrap();
        let init: Value = serde_json::from_str(&init).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "postkit");
        let listed = rpc(
            &server,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await;
        assert!(listed["result"]["tools"].as_array().unwrap().len() >= 9);
        let caps = rpc(
            &server,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": { "name": "capabilities", "arguments": null }
            }),
        )
        .await;
        assert_eq!(caps["result"]["isError"], false);
        assert!(caps["result"]["structuredContent"]["threads"].is_array());
    }

    #[tokio::test]
    async fn ping_and_initialized_notification() {
        let server = server();
        let _ = rpc(
            &server,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .await;
        let ping = rpc(&server, json!({"jsonrpc":"2.0","id":2,"method":"ping"})).await;
        assert_eq!(ping["result"], json!({}));
        let notify = server
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        assert!(notify.is_none());
    }

    #[tokio::test]
    async fn unknown_method_is_json_rpc_error() {
        let server = server();
        let _ = rpc(
            &server,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .await;
        let reply = rpc(
            &server,
            json!({"jsonrpc":"2.0","id":3,"method":"prompts/list"}),
        )
        .await;
        assert_eq!(reply["error"]["code"], METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn tools_require_initialize() {
        let server = server();
        let reply = rpc(
            &server,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .await;
        assert_eq!(reply["error"]["code"], INVALID_REQUEST);
    }

    #[tokio::test]
    async fn tools_list_names_core_reads_and_unknown_tool_is_invalid_params() {
        let server = server();
        let _ = rpc(
            &server,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .await;
        let listed = rpc(
            &server,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await;
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "capabilities",
                "accounts_list",
                "whoami",
                "post",
                "whatsapp_send",
                "insights",
                "ads_accounts",
                "ads_list",
                "ads_inspect",
                "ads_status",
                "ads_create_paused",
                "pages_accounts",
                "media_list"
            ]
        );
        let missing = rpc(
            &server,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": { "name": "broadcast", "arguments": {} }
            }),
        )
        .await;
        assert_eq!(missing["error"]["code"], INVALID_PARAMS);
        assert_eq!(missing["error"]["message"], "Unknown tool: broadcast");
    }

    async fn call(server: &Server, name: &str, arguments: Value) -> Value {
        rpc(
            server,
            json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }),
        )
        .await
    }

    #[tokio::test]
    async fn capabilities_and_accounts_are_local_reads() {
        let server = server();
        let _ = rpc(
            &server,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .await;
        let caps = call(&server, "capabilities", json!({})).await;
        assert_eq!(caps["result"]["isError"], false);
        assert!(caps["result"]["structuredContent"]["threads"].is_array());
        let unknown = call(&server, "capabilities", json!({ "site": "nope" })).await;
        assert_eq!(unknown["result"]["isError"], true);
        assert_eq!(
            unknown["result"]["structuredContent"]["error"],
            "unknown_site"
        );
        let accounts = call(&server, "accounts_list", json!({})).await;
        assert_eq!(
            accounts["result"]["structuredContent"]["accounts"],
            json!([])
        );
    }

    #[tokio::test]
    async fn whoami_without_an_account_is_a_tool_error() {
        let server = server();
        let _ = rpc(
            &server,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .await;
        let missing = call(&server, "whoami", json!({})).await;
        assert_eq!(missing["result"]["isError"], true);
        let unknown = call(&server, "whoami", json!({ "site": "threads" })).await;
        assert_eq!(unknown["result"]["isError"], true);
        assert_eq!(
            unknown["result"]["structuredContent"]["error"],
            "unknown_account"
        );
    }

    #[tokio::test]
    async fn oversized_line_is_a_parse_error() {
        let server = server();
        let huge = "x".repeat(MAX_MESSAGE_BYTES + 1);
        let reply = server.handle_line(&huge).await.unwrap();
        let value: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(value["error"]["code"], PARSE_ERROR);
    }

    #[tokio::test]
    async fn malformed_json_is_a_parse_error() {
        let server = server();
        let reply = server.handle_line("{").await.unwrap();
        let value: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(value["error"]["code"], PARSE_ERROR);
    }

    struct PostMock {
        site: postkit::Site,
        posts: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl postkit::Publisher for PostMock {
        fn site(&self) -> &postkit::Site {
            &self.site
        }
        fn capabilities(&self) -> &[postkit::Capability] {
            &[postkit::Capability::PublishText]
        }
        fn auth_kind(&self) -> postkit::AuthKind {
            postkit::AuthKind::None
        }
        async fn publish(
            &self,
            _app: &postkit::AppConfig,
            _creds: &postkit::AccountCreds,
            _intent: postkit::Intent,
            _deadline: postkit::Deadline,
        ) -> Result<postkit::Outcome, Error> {
            let n = self.posts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(postkit::Outcome {
                site: self.site.clone(),
                id: Some(format!("post-{n}")),
                url: Some("https://example.com/p".into()),
                limits: None,
            })
        }
        async fn whoami(
            &self,
            _app: &postkit::AppConfig,
            _creds: &postkit::AccountCreds,
        ) -> Result<postkit::WhoAmI, Error> {
            Ok(postkit::WhoAmI {
                site: self.site.clone(),
                id: "1".into(),
                handle: None,
            })
        }
    }

    fn mock_post_server() -> Server {
        let mock = Arc::new(PostMock {
            site: postkit::Site::new("threads"),
            posts: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut registry = postkit::Registry::new();
        registry.register(mock);
        let vault = Arc::new(postkit::MemoryVault::new());
        vault
            .put(
                &postkit::AccountKey::new("threads", "default"),
                &postkit::AccountCreds::OAuth2 {
                    access_token: "tok".into(),
                    refresh_token: None,
                    extra: json!({}),
                },
            )
            .unwrap();
        let apps = Arc::new(postkit::MemoryAppStore::new());
        let client = Client::new(registry, vault, apps);
        let dummy = Client::new(
            postkit::Registry::new(),
            Arc::new(postkit::MemoryVault::new()),
            Arc::new(postkit::MemoryAppStore::new()),
        );
        Server::new(client, dummy)
    }

    #[tokio::test]
    async fn post_tool_publishes_through_the_kernel() {
        let server = mock_post_server();
        let _ = rpc(
            &server,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .await;
        let reply = call(
            &server,
            "post",
            json!({
                "target": { "site": "threads" },
                "body": { "type": "text", "text": "hi" },
                "idempotency_key": "post-1"
            }),
        )
        .await;
        assert_eq!(reply["result"]["isError"], false);
        assert_eq!(reply["result"]["structuredContent"]["id"], "post-0");
    }

    struct WaMock {
        site: postkit::Site,
        sends: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl postkit::Publisher for WaMock {
        fn site(&self) -> &postkit::Site {
            &self.site
        }
        fn capabilities(&self) -> &[postkit::Capability] {
            &[postkit::Capability::SendText]
        }
        fn auth_kind(&self) -> postkit::AuthKind {
            postkit::AuthKind::StaticToken
        }
        async fn publish(
            &self,
            _app: &postkit::AppConfig,
            _creds: &postkit::AccountCreds,
            _intent: postkit::Intent,
            _deadline: postkit::Deadline,
        ) -> Result<postkit::Outcome, Error> {
            Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: "use_whatsapp_command".into(),
                limit: None,
            })
        }
        async fn whoami(
            &self,
            _app: &postkit::AppConfig,
            _creds: &postkit::AccountCreds,
        ) -> Result<postkit::WhoAmI, Error> {
            Ok(postkit::WhoAmI {
                site: self.site.clone(),
                id: "1".into(),
                handle: None,
            })
        }
    }

    #[async_trait::async_trait]
    impl postkit::WhatsAppSender for WaMock {
        async fn send_whatsapp(
            &self,
            _app: &postkit::AppConfig,
            _creds: &postkit::AccountCreds,
            _request: &postkit::WhatsAppSendRequest,
            _deadline: postkit::Deadline,
        ) -> Result<postkit::Outcome, Error> {
            let n = self.sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(postkit::Outcome {
                site: self.site.clone(),
                id: Some(format!("wamid-{n}")),
                url: None,
                limits: None,
            })
        }
    }

    fn mock_whatsapp_server() -> Server {
        let mock = Arc::new(WaMock {
            site: postkit::Site::new("whatsapp_cloud"),
            sends: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut deny_registry = postkit::Registry::new();
        deny_registry.register_connector(
            postkit::Connector::from_publisher(mock.clone()).whatsapp(mock.clone()),
        );
        let mut allow_registry = postkit::Registry::new();
        allow_registry.register_connector(
            postkit::Connector::from_publisher(mock.clone()).whatsapp(mock.clone()),
        );
        let vault = Arc::new(postkit::MemoryVault::new());
        vault
            .put(
                &postkit::AccountKey::new("whatsapp_cloud", "default"),
                &postkit::AccountCreds::BotToken {
                    token: "system-user".into(),
                },
            )
            .unwrap();
        let apps = Arc::new(postkit::MemoryAppStore::new());
        postkit::AppStore::put(
            &*apps,
            &postkit::AppConfig {
                site: postkit::Site::new("whatsapp_cloud"),
                oauth: None,
                extra: json!({ "phone_number_id": "123456789" }),
            },
        )
        .unwrap();
        let deny = Client::new(deny_registry, vault.clone(), apps.clone());
        let allow = Client::new(allow_registry, vault, apps)
            .with_whatsapp_policy(Arc::new(postkit::AllowWhatsAppSendsPolicy));
        Server::new(deny, allow)
    }

    #[tokio::test]
    async fn whatsapp_send_requires_allow_send_then_returns_wamid() {
        let server = mock_whatsapp_server();
        let _ = rpc(
            &server,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .await;
        let denied = call(
            &server,
            "whatsapp_send",
            json!({
                "allow_send": false,
                "idempotency_key": "wa-1",
                "message": { "type": "text", "to": "60123456789", "text": "hi" }
            }),
        )
        .await;
        assert_eq!(denied["result"]["isError"], true);
        assert_eq!(
            denied["result"]["structuredContent"]["error"],
            "policy_denied"
        );
        let sent = call(
            &server,
            "whatsapp_send",
            json!({
                "allow_send": true,
                "idempotency_key": "wa-1",
                "message": { "type": "text", "to": "60123456789", "text": "hi" }
            }),
        )
        .await;
        assert_eq!(sent["result"]["isError"], false);
        assert_eq!(sent["result"]["structuredContent"]["id"], "wamid-0");
    }

    #[tokio::test]
    async fn read_tools_fail_closed_without_accounts() {
        let server = server();
        let _ = rpc(
            &server,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .await;
        let insights = call(
            &server,
            "insights",
            json!({
                "from": "2026-06-01",
                "to": "2026-06-30",
                "attribution": "7d_click_1d_view"
            }),
        )
        .await;
        assert_eq!(insights["result"]["isError"], true);
        assert_eq!(
            insights["result"]["structuredContent"]["error"],
            "unknown_account"
        );
        let pages = call(
            &server,
            "pages_accounts",
            json!({ "site": "facebook_pages" }),
        )
        .await;
        assert_eq!(
            pages["result"]["structuredContent"]["error"],
            "unknown_account"
        );
        let media = call(&server, "media_list", json!({ "site": "instagram" })).await;
        assert_eq!(
            media["result"]["structuredContent"]["error"],
            "unknown_account"
        );
        let ads = call(&server, "ads_accounts", json!({ "site": "meta_ads" })).await;
        assert_eq!(
            ads["result"]["structuredContent"]["error"],
            "unknown_account"
        );
        let listed = call(
            &server,
            "ads_list",
            json!({ "site": "meta_ads", "entity": "campaign" }),
        )
        .await;
        assert_eq!(
            listed["result"]["structuredContent"]["error"],
            "unknown_account"
        );
        let inspected = call(
            &server,
            "ads_inspect",
            json!({ "site": "meta_ads", "entity": "adset", "id": "456" }),
        )
        .await;
        assert_eq!(
            inspected["result"]["structuredContent"]["error"],
            "unknown_account"
        );
        let denied = call(
            &server,
            "ads_create_paused",
            json!({
                "create": { "entity": "campaign", "spec": { "name": "x", "objective": "traffic" } }
            }),
        )
        .await;
        assert_eq!(denied["result"]["structuredContent"]["error"], "policy_denied");
        let status = call(
            &server,
            "ads_status",
            json!({ "site": "meta_ads", "entity": "ad", "id": "1" }),
        )
        .await;
        assert_eq!(
            status["result"]["structuredContent"]["error"],
            "unknown_account"
        );
        let bad_attr = call(
            &server,
            "insights",
            json!({
                "from": "2026-06-01",
                "to": "2026-06-30",
                "attribution": "forever"
            }),
        )
        .await;
        assert_eq!(
            bad_attr["result"]["structuredContent"]["error"],
            "invalid_query"
        );
    }
}
