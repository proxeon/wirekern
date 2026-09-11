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
        Err(rpc_error(
            Value::Null,
            INVALID_PARAMS,
            &tools::unknown_tool_message(name),
        ))
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
    async fn tools_list_starts_empty_and_unknown_tool_is_invalid_params() {
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
        assert_eq!(listed["result"]["tools"], json!([]));
        let missing = rpc(
            &server,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": { "name": "post", "arguments": {} }
            }),
        )
        .await;
        assert_eq!(missing["error"]["code"], INVALID_PARAMS);
        assert_eq!(missing["error"]["message"], "Unknown tool: post");
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
}
