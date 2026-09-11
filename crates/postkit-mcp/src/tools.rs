//! Closed MCP tool catalog.
//!
//! Every tool maps to an existing `Client` method. New verbs belong in the
//! kernel first; this file only adapts JSON-RPC arguments onto those types.

use serde_json::{json, Value};

#[derive(Clone, Copy)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub read_only: bool,
    pub destructive: bool,
}

/// Tools ship as this list grows. Unknown names are JSON-RPC invalid-params
/// (`-32602`), matching the MCP tools spec example for a missing tool.
pub fn catalog() -> &'static [ToolSpec] {
    &[]
}

pub fn list_tools_result() -> Value {
    let tools: Vec<Value> = catalog()
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": true
                },
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
