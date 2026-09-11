//! MCP JSON-RPC 2.0 subset for local stdio.
//!
//! Spec: newline-delimited UTF-8 objects, no embedded newlines, logging on
//! stderr. This is the 2025-11-25 stdio transport plus the tools capability.
//! We do not speak Streamable HTTP here — that is `postkit serve`.

use serde_json::{json, Value};

/// JSON-RPC 2.0 reserved codes used by this surface.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// Revisions this server will echo. Unknown client versions get
/// [`DEFAULT_PROTOCOL_VERSION`] so a newer host can still talk tools.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2024-11-05", "2025-03-26", "2025-11-25", "2026-07-28"];
pub const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";

/// MCP stdio messages must be one JSON object per line. Cap the line so a
/// stuck host cannot grow the process without bound (same 1 MiB as WhatsApp
/// webhook bodies).
pub const MAX_MESSAGE_BYTES: usize = 1_048_576;

pub fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|asked| {
            SUPPORTED_PROTOCOL_VERSIONS
                .iter()
                .copied()
                .find(|supported| *supported == asked)
        })
        .unwrap_or(DEFAULT_PROTOCOL_VERSION)
}

pub fn rpc_result(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

pub fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// Compact encoding: the stdio transport forbids embedded newlines.
pub fn encode_message(value: &Value) -> Result<String, String> {
    let encoded = serde_json::to_string(value).map_err(|err| err.to_string())?;
    if encoded.contains('\n') || encoded.contains('\r') {
        return Err("mcp_message_contains_newline".into());
    }
    Ok(encoded)
}

pub fn parse_message(line: &str) -> Result<Value, Value> {
    match serde_json::from_str::<Value>(line) {
        Ok(value) if value.is_object() => Ok(value),
        Ok(_) => Err(rpc_error(
            Value::Null,
            INVALID_REQUEST,
            "Request must be a JSON object",
        )),
        Err(_) => Err(rpc_error(Value::Null, PARSE_ERROR, "Parse error")),
    }
}

pub fn message_id(value: &Value) -> Option<Value> {
    value.get("id").cloned()
}

pub fn is_notification(value: &Value) -> bool {
    value.get("id").is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echoes_a_known_protocol_version() {
        assert_eq!(negotiate_protocol_version(Some("2026-07-28")), "2026-07-28");
        assert_eq!(negotiate_protocol_version(Some("2025-11-25")), "2025-11-25");
    }

    #[test]
    fn unknown_protocol_version_falls_back() {
        assert_eq!(
            negotiate_protocol_version(Some("1999-01-01")),
            DEFAULT_PROTOCOL_VERSION
        );
        assert_eq!(negotiate_protocol_version(None), DEFAULT_PROTOCOL_VERSION);
    }

    #[test]
    fn encode_message_rejects_pretty_json() {
        let pretty = json!({"ok": true});
        let encoded = encode_message(&pretty).unwrap();
        assert!(!encoded.contains('\n'));
    }
}
