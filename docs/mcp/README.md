# MCP stdio surface

`postkit-mcp` is the local Model Context Protocol face of the same
`Client::from_home` used by the CLI and `postkit serve`. An agent host
spawns the process; JSON-RPC travels on stdin/stdout; tokens stay in the
operator vault. It is not a hosted social MCP and it does not terminate
TLS or take custody.

Validated against the MCP specification revisions
[2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
(stdio + tools) and the 2026-07-28 compatibility notes in the official
Rust SDK. Postkit does **not** depend on `rmcp` 3.x: that crate's
published `rust-version` is 1.88 / edition 2024, while this workspace is
MSRV 1.80 / edition 2021. The protocol subset we speak (initialize, ping,
tools/list, tools/call, newline-delimited stdio) is small enough to keep
in-tree without pulling a second HTTP/OAuth stack onto the kernel.

## How an agent host talks to it

stdio, per the spec:

- The host launches `postkit mcp` (or the `postkit-mcp` binary) as a
  subprocess with the operator's `HOME` / `POSTKIT_HOME`.
- JSON-RPC messages are UTF-8, one object per line, **no embedded
  newlines**. Stdout is protocol-only. Logging goes to stderr.
- Auth is the OS user plus the local vault. There is no `pk_live_` on
  this path: a key is a firewall for a TCP listener, not for a child the
  operator just spawned. HTTP callers still use `postkit serve`.

Example host config (Claude Desktop / Cursor / any stdio MCP client):

```json
{
  "mcpServers": {
    "postkit": {
      "command": "postkit",
      "args": ["mcp"]
    }
  }
}
```

`POSTKIT_HOME` may be set in the host's `env` block when the vault is
not `~/.postkit`.

## Tools

Each tool is a closed kernel verb. There is no Graph JSON escape hatch
and no calendar.

| Tool | Kernel | Notes |
|------|--------|-------|
| `capabilities` | `Registry::capabilities_json` | Optional `site` filter |
| `accounts_list` | `Vault::list` | Names only, never tokens |
| `whoami` | `Client::whoami` | Requires a stored account |
| `post` | `Client::publish` | Same `PostRequest` as HTTP `--stdin` |
| `whatsapp_send` | `Client::send_whatsapp_from` | Requires `allow_send: true` |
| `insights` | `Client::insights` | Explicit attribution window |
| `ads_accounts` | `Client::ad_accounts` | Remote `act_` ids, not vault aliases |
| `pages_accounts` | `Client::pages` | Page identities, never Page tokens |
| `media_list` | `Client::media` | First page, limit 1–25 |

Writes that can charge or publish (`post`, `whatsapp_send`) set MCP
`destructiveHint`. Reads set `readOnlyHint`. Kernel `WireError` values
return as tool-execution errors (`isError: true`) so the model can
correct arguments; unknown tool names are JSON-RPC `-32601`.

## Not on this surface

Interactive `auth`, `apps set`, and `keys create` stay on the CLI so
secrets are not tool arguments (hosts log those). Ads activation, budget
mutation, WhatsApp template/Flow/phone writes, and the Meta webhook
listener stay off MCP until they have their own confirmation contract.
Streamable HTTP is `postkit serve`, not this crate.

## Run

```bash
postkit mcp
postkit-mcp                 # same stdio path, MCP-only binary
```

Omit `--json`: stdout is the JSON-RPC pipe. `--home` / `POSTKIT_HOME`
select the vault. Per-tool `deadline` (seconds, default 30) bounds the
kernel call the same way the CLI global flag does.

Track remaining work in [checklist.md](./checklist.md).
