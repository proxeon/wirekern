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
| `meta_ads` | `read.metrics`, `read.ad_accounts`, `create.paused_ads`, `create.ad_creative` | OAuth paste-code (`ads_read,ads_management`), long-lived via `fb_exchange_token` | ≤ 90-day reads; ads are fixed `PAUSED` |

### Feature coverage

| Capability | Threads | Bluesky | Meta Ads |
|------------|---------|---------|----------|
| Text post | ✓ | ✓ | ✗ read-only by design |
| Reply chain (repeat `--text`) | ✓ | ✗ `thread_unsupported` | — |
| Reply to existing post (`reply_to_id`) | ✓ | ✗ | — |
| Dry-run probe (`--dry-run`) | ✓ create-only, expires unpublished in 24h | ✗ `dry_run_unsupported` (atomic `createRecord`) | — |
| Spend / performance insights | ✗ roadmap | ✗ roadmap | ✓ `insights`, daily rows by entity |
| Paused campaign / ad set / ad create | ✗ | ✗ | ✓ `ads create-*`, activation unavailable |
| Image upload / Page link creative | ✗ | ✗ | ✓ account asset only; cannot deliver alone |
| Token refresh | ✓ auto, within 7 days of expiry | n/a (app passwords) | ✓ re-issue via `fb_exchange_token` |
| `whoami` | ✓ | ✓ | ✓ (+ first ad account resolved at auth) |
| Images / video | ✗ roadmap | ✗ roadmap | — |
| Scheduling / drafts | ✗ by design | ✗ by design | — |

Kernel-level, all sites: 0600 vault with atomic writes, CSPRNG OAuth `state`, redirect-following off, per-target results on fan-out.

## CLI

```text
postkit post <site> --text "…"              # publish now; --to threads,bluesky fans out
postkit post threads --text 'root' --text 'reply'   # reply chain on Threads
postkit post threads --text '…' --dry-run   # probe: publish nothing (threads)
postkit auth <site> [--token | --code | --password]
postkit insights meta_ads --from 2026-06-01 --to 2026-06-30 --attribution 7d_click_1d_view --level campaign
postkit ads create-campaign meta_ads --name 'Draft' --objective sales
postkit ads upload-image meta_ads --file hero.png
postkit ads create-draft meta_ads --manifest launch.paused.json --state launch.state.json
postkit whoami <site>
postkit capabilities [site]
postkit accounts list|delete
postkit apps show|set
```

Every subcommand takes `--json`: with it, stdout carries exactly one JSON document; without it, stdout stays empty and every line (results included) goes to stderr.

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

Default features are empty: `vault-file`, `client`, `oauth` (implies `client`), `threads`, `bluesky`, `meta-ads`, `draft` (manifest-orchestrated paused launches). `cargo add postkit --features threads` pulls no Bluesky, no scheduler, nothing you did not ask for.

`Client::{publish, whoami, insights, auth_start, auth_finish, put_token, run_paused_draft}`. Connectors register on `Registry`. Refresh (Threads long-lived) runs in `Client`, not in `publish`.

## Positioning & roadmap

postkit keeps what hosted APIs take: tokens stay in **your** vault, you bring **your own** platform apps, cost is **$0**, license MIT OR Apache-2.0. Comparison grid against hosted post APIs, self-hosted schedulers and official SDKs: **[docs/positioning.md](./docs/positioning.md)**.

Roadmap, in order: images/video, then HTTP `serve` mode with `pk_live_` keys.

## Not in this version

`serve` / `pk_live_`, `--listen`, `--image` / video, schedule, inbox, LinkedIn, Telegram, Mastodon. Add a site: [docs/connectors.md](./docs/connectors.md).

Notable changes, release by release: [CHANGELOG.md](./CHANGELOG.md).

License: MIT OR Apache-2.0. Repository: [github.com/proxeon/postkit](https://github.com/proxeon/postkit).
