# postkit

Send text posts to **Threads** and **Bluesky** right now, through the official APIs, with credentials that never leave your machine. Rust library + CLI. No scheduler, no inbox, no cloud.

Success is an `Outcome` with `id` and `url`; failure is a `WireError` you can branch on. No `"ok": true`.

## Install

```bash
cargo install postkit-cli   # the `postkit` binary
cargo add postkit           # the library crate
```

Vault: `~/.postkit` (0700/0600). Override with `--home` or `POSTKIT_HOME`. Copy [`.env.example`](./.env.example) to `.env` (gitignored) — the CLI does **not** auto-load it.

## Quick start

Threads — paste-code OAuth; full Meta-app walkthrough in [docs/threads](./docs/threads/):

```bash
postkit apps set threads --client-id ID --client-secret SECRET --redirect-uri 'https://example.com/callback'
postkit auth threads --json          # print URL, paste the redirect back
postkit post threads --text "hi" --json
```

Bluesky — app password; walkthrough in [docs/bluesky](./docs/bluesky/):

```bash
postkit auth bluesky --account you.bsky.social --password 'xxxx-xxxx-xxxx-xxxx' --json
postkit post bluesky --account you.bsky.social --text "hi" --json
```

## What it is

| | |
|--|--|
| **Job** | Send **now** through official APIs. `Outcome.id` + `url`, or `WireError`. |
| **Not** | Scheduler, inbox, drafts, `--at`, media, `serve` |
| **You hold** | Tokens on disk. BYO Meta app / Bluesky app password. |
| **Surfaces** | `cargo add postkit` (`Client`) and the `postkit` CLI. HTTP (`pk_live_`) only when a caller cannot exec. |

| Site | Capability | Auth | Limit |
|------|------------|------|--------|
| `threads` | `publish.text` | OAuth paste-code, or `--token` long-lived `THQVJ…` | 500 chars; emoji as UTF-8 bytes |
| `bluesky` | `publish.text` | App password (`--password`). `--account` **is** the handle (`default` rejected) | 300 graphemes |

### Feature coverage

| Capability | Threads | Bluesky |
|------------|---------|---------|
| Text post | ✓ | ✓ |
| Reply chain (repeat `--text`) | ✓ | ✗ `thread_unsupported` |
| Reply to existing post (`reply_to_id`) | ✓ | ✗ |
| Token refresh | ✓ auto, within 7 days of expiry | n/a (app passwords) |
| `whoami` | ✓ | ✓ |
| Images / video | ✗ roadmap | ✗ roadmap |
| Scheduling / drafts | ✗ by design | ✗ by design |

Kernel-level, all sites: 0600 vault with atomic writes, CSPRNG OAuth `state`, redirect-following off, per-target results on fan-out.

## CLI

```text
postkit post <site> --text "…"              # publish now; --to threads,bluesky fans out
postkit post threads --text 'root' --text 'reply'   # reply chain on Threads
postkit auth <site> [--token | --code | --password]
postkit whoami <site>
postkit capabilities [site]
postkit accounts list|delete
postkit apps show|set
```

Every subcommand takes `--json`: the machine document on **stdout**, human text on stderr. Agents always pass this.

Flag-by-flag reference, auth flows, vault layout, exit codes: **[docs/cli.md](./docs/cli.md)**.

## Output contract

```json
{"site":"threads","id":"…","url":"https://www.threads.com/@…/post/…"}
```

Fan-out returns one result per target: `{ "results": [ Outcome | WireError, … ] }`. Errors carry a reason — `{"error":"invalid_post","site":"threads","reason":"text_too_long","limit":500}` — and exit codes agents can branch on: usage `2`, auth `3`, rate-limited `4`, platform/network/timeout `5`. Full table in [docs/cli.md](./docs/cli.md).

## Library

```toml
postkit = { version = "0.1", features = ["vault-file", "threads", "bluesky"] }
```

Default features are empty: `vault-file`, `client`, `oauth` (implies `client`), `threads`, `bluesky`. `cargo add postkit --features threads` pulls no Bluesky, no scheduler, nothing you did not ask for.

`Client::{publish, whoami, auth_start, auth_finish, put_token}`. Connectors register on `Registry`. Refresh (Threads long-lived) runs in `Client`, not in `publish`.

## Positioning & roadmap

postkit keeps what hosted APIs take: tokens stay in **your** vault, you bring **your own** platform apps, cost is **$0**, license MIT OR Apache-2.0. Comparison grid against hosted post APIs, self-hosted schedulers and official SDKs: **[docs/positioning.md](./docs/positioning.md)**.

Roadmap, in order: images/video, then HTTP `serve` mode with `pk_live_` keys.

## Not in this version

`serve` / `pk_live_`, `--listen`, `--image` / video, schedule, inbox, LinkedIn, Telegram, Mastodon. Add a site: [docs/connectors.md](./docs/connectors.md). Gates: [plans/001/004-after-kernel.md](./plans/001/004-after-kernel.md).

License: MIT OR Apache-2.0. Repository: [github.com/proxeon/postkit](https://github.com/proxeon/postkit).
