# postkit

**postkit is an official-API execution kernel:** send now through your apps, with your vault, no calendar. Posts to **Threads**, **Bluesky**, **Facebook Pages**, and **Instagram**, plus narrowly typed **WhatsApp Cloud** business messages and paused **Meta Ads** drafts. Credentials never leave your machine. Rust library + CLI. No scheduler, no hosted inbox, no cloud.

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
| **Job** | Official-API execution kernel. Send **now**. `Outcome.id` + `url`, or `WireError`. |
| **Not** | Scheduler, persistent inbox, drafts, `--at`, media download/hosting, `serve` |
| **You hold** | Tokens on disk. BYO Meta app / Bluesky app password. |
| **Surfaces** | `cargo add postkit` (`Client`) and the `postkit` CLI. HTTP (`pk_live_`) only when a caller cannot exec. |

| Site | Capability | Auth | Limit |
|------|------------|------|--------|
| `threads` | `publish.text`, `publish.image` | OAuth paste-code, or `--token` long-lived `THQVJ…` | 500 chars (caption incl.); emoji as UTF-8 bytes; images via public https URL |
| `bluesky` | `publish.text`, `publish.image` | App password (`--password`). `--account` **is** the handle (`default` rejected) | 300 graphemes; images ≤ 2 MB (png/jpg/gif/webp) |
| `instagram` | `publish.image`, `publish.carousel`, `read.media` | Instagram Login paste-code (`instagram_business_basic,instagram_business_content_publish`), long-lived via `ig_exchange_token` | Professional account only; public HTTPS image URLs; carousel 2–10 slides; caption ≤ 2,200 characters; recent-media read is 1–25, first page only |
| `meta_ads` | `read.metrics`, `read.ad_accounts`, `create.paused_ads`, `create.ad_creative` | OAuth paste-code (`ads_read,ads_management,pages_show_list,pages_manage_ads`), long-lived via `fb_exchange_token` | ≤ 90-day reads; ads are fixed `PAUSED` |
| `facebook_pages` | `read.pages`, `publish.text`, `publish.image` | OAuth paste-code (`pages_show_list,pages_manage_posts,pages_read_engagement`), long-lived via `fb_exchange_token` | Page ID required per post; local images upload as multipart bytes |
| `whatsapp_cloud` | `send.reply`, `send.template`, `read.webhook_messages` | Static System User token (`auth whatsapp_cloud --token`), Phone number ID config | Private send requires `--allow-send` + idempotency; inbound messages arrive as signed webhooks |

### Feature coverage

| Capability | Threads | Bluesky | Instagram | Meta Ads | Facebook Pages |
|------------|---------|---------|-----------|----------|----------------|
| Text post | ✓ | ✓ | ✗ image required | ✗ read-only by design | ✓ explicit `page_id` |
| Image post (`--image`) | ✓ public https URL (`--alt` ignored) | ✓ file upload, `--alt` embedded | ✓ public https URL (`--alt` ignored) | — | ✓ local bytes, caption + alt text |
| Image carousel (repeat `--image`) | ✗ | ✗ | ✓ 2–10 public HTTPS URLs; one parent caption | — | ✗ |
| Reply chain (repeat `--text`) | ✓ | ✗ `thread_unsupported` | ✗ `thread_unsupported` | — | ✗ `thread_unsupported` |
| Reply to existing post (`reply_to_id`) | ✓ media id | ✓ `at://` URI | ✗ unsupported | — | ✗ unsupported |
| Dry-run probe (`--dry-run`) | ✓ create-only, expires unpublished in 24h | ✗ `dry_run_unsupported` (atomic `createRecord`) | ✗ `dry_run_unsupported` | — | ✗ `dry_run_unsupported` |
| Spend / performance insights | ✗ roadmap | ✗ roadmap | ✗ roadmap | ✓ `insights`, daily rows by entity | ✗ roadmap |
| Paused campaign / ad set / ad create | ✗ | ✗ | — | ✓ `ads create-*`, activation unavailable | — |
| Image upload / Page link creative | ✗ | ✗ | — | ✓ account asset only; cannot deliver alone | — |
| Token refresh | ✓ auto, within 7 days of expiry | n/a (app passwords) | ✓ re-issue via `ig_refresh_token` | ✓ re-issue via `fb_exchange_token` | ✓ re-issue via `fb_exchange_token` |
| `whoami` | ✓ | ✓ | ✓ | ✓ (+ first ad account resolved at auth) | ✓ |
| Recent published media (`media list`) | ✗ | ✗ | ✓ first 1–25 only | — | ✗ |
| Video | ✗ roadmap | ✗ roadmap | ✗ roadmap | — | ✗ roadmap |
| Scheduling / drafts | ✗ by design | ✗ by design | ✗ by design | — | ✗ by design |

Kernel-level, all sites: 0600 vault with atomic writes, CSPRNG OAuth `state`, redirect-following off, per-target results on fan-out.

### WhatsApp Cloud feature coverage

| Capability | Status |
|------------|--------|
| Text reply | ✓ Requires recipient WhatsApp ID, inbound `wamid`, idempotency key, and `--allow-send` |
| Approved template | ✓ Existing approved template; ordered text body variables only |
| Inbound messages | ✓ Parse a signed raw webhook body; no listener or persistent inbox |
| Delivery/read status | ✓ Parse signed `sent`/`delivered`/`read`/`failed` callbacks; no transport or storage |
| Media, interactive messages, Flows, bulk sends | ✗ roadmap; each requires a separate consent/payload contract |
| Token refresh | ✗ Static System User token is operator-managed |

## CLI

```text
postkit post <site> --text "…"              # publish now; --to threads,bluesky fans out
postkit post threads --text 'root' --text 'reply'   # reply chain on Threads
postkit post threads --text '…' --dry-run   # probe: publish nothing (threads)
postkit post bluesky --image hero.png --text 'caption' --alt 'description' # image post
postkit post instagram --image https://cdn.example.com/hero.jpg --text 'caption'
postkit post instagram --image https://cdn.example.com/slide-1.jpg --image https://cdn.example.com/slide-2.jpg --text 'one carousel caption'
postkit media list instagram --limit 10 --json
postkit auth <site> [--token | --code | --password]
postkit insights meta_ads --from 2026-06-01 --to 2026-06-30 --attribution 7d_click_1d_view --level campaign
postkit ads create-campaign meta_ads --name 'Draft' --objective sales
postkit ads upload-image meta_ads --file hero.png
postkit ads create-draft meta_ads --manifest launch.paused.json --state launch.state.json
postkit pages accounts facebook_pages --json
postkit post facebook_pages --param page_id=123 --text 'Hello from Postkit'
postkit whatsapp reply --to 60123456789 --reply-to wamid.inbound --text 'Hello' --idempotency reply-1 --allow-send
postkit whatsapp webhook parse --signature "$X_HUB_SIGNATURE_256" < webhook.json
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

Default features are empty: `vault-file`, `client`, `oauth` (implies `client`), `threads`, `bluesky`, `instagram`, `meta-ads`, `facebook-pages`, `whatsapp-cloud`, `draft` (manifest-orchestrated paused launches). `cargo add postkit --features threads` pulls no Bluesky, no scheduler, nothing you did not ask for.

`Client::{publish, send_whatsapp, whoami, insights, pages, media, auth_start, auth_finish, put_token, run_paused_draft}`. Connectors register on `Registry`. Refresh (Threads/Facebook/Instagram long-lived) runs in `Client`, not in `publish`.

## Positioning & roadmap

postkit keeps what hosted APIs take: tokens stay in **your** vault, you bring **your own** platform apps, cost is **$0**, license MIT OR Apache-2.0. Comparison grid against hosted post APIs, self-hosted schedulers and official SDKs: **[docs/positioning.md](./docs/positioning.md)**.

Roadmap, in order: images/video, then HTTP `serve` mode with `pk_live_` keys.

## Not in this version

`serve` / `pk_live_`, `--listen`, video, schedule, persistent inbox, LinkedIn, Telegram, Mastodon. Add a site: [docs/connectors.md](./docs/connectors.md).

Notable changes, release by release: [CHANGELOG.md](./CHANGELOG.md).

License: MIT OR Apache-2.0. Repository: [github.com/proxeon/postkit](https://github.com/proxeon/postkit).
