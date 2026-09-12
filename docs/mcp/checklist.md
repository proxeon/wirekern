# MCP crate — remaining features

Tick when shipped. Product rule: this crate is a **stdio adapter** over
`Client::from_home`, not a second product and not a hosted MCP. Same
fail-closed policy as CLI/HTTP. No calendar, inbox, or Graph JSON hatch.

Last aligned with the MCP stdio + tools spec
([2025-11-25 transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports),
[tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools))
and the official Rust SDK's 2026-07-28 notes (2026-09-12). Graph pin for
connectors is unchanged (`v26.0` where already pinned). Workspace MSRV
stays **1.80**.

## Crate and protocol

- [x] Workspace member `postkit-mcp`; clap-free lib so the CLI can call `run`
- [x] `Client::from_home` (deny-by-default + allowing WhatsApp client)
- [x] Newline-delimited JSON-RPC stdio; stderr logging; 1 MiB line cap
- [x] `initialize` / `notifications/initialized` / `ping`
- [x] `tools/list` and `tools/call`; unknown method → `-32601`
- [x] Protocol version echo when the client asks for a known revision

## Core tools

- [x] `capabilities`
- [x] `accounts_list`
- [x] `whoami`

## Writes (policy-gated)

- [x] `post` (`PostRequest` + optional idempotency key)
- [x] `whatsapp_send` (`allow_send: true` + idempotency; configured `--sender` alias only)
- [x] `ads_create_paused` (`allow_create: true`; always `PAUSED`)

## Reads

- [x] `insights`
- [x] `ads_accounts`
- [x] `ads_list`
- [x] `ads_inspect`
- [x] `ads_status`
- [x] `pages_accounts`
- [x] `media_list`

## Surfaces and docs

- [x] `postkit mcp` and standalone `postkit-mcp`
- [x] Host config snippet; README / cli.md / changelog / positioning

## Explicitly deferred

- [ ] Official `rmcp` crate (MSRV 1.88 / edition 2024 as of 3.3.0)
- [ ] Streamable HTTP MCP (use `postkit serve` for non-exec HTTP)
- [ ] Auth / apps / keys tools (secrets in host logs)
- [ ] Ads activation, budget mutation, WhatsApp management writes
- [ ] Prompts and resources as a product surface
