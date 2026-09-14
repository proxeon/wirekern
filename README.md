# wirekern

**wirekern is an official-API execution kernel:** send now through your apps, with your vault, no calendar. Posts to **Threads**, **Bluesky**, **Facebook Pages**, **Instagram**, **LinkedIn**, and **X**, plus narrowly typed **WhatsApp Cloud** business messages and paused **Meta Ads** drafts. Credentials never leave your machine. Rust library + CLI. No scheduler, no hosted inbox, no cloud.

Success is an `Outcome` with `id` and `url`; failure is a `WireError` you can branch on. No `"ok": true`.

## Install

```bash
cargo install wirekern-cli     # the `wirekern` binary (includes `wirekern serve` and `wirekern mcp`)
cargo install wirekern-serve   # optional: HTTP-only binary
cargo install wirekern-mcp     # optional: MCP stdio-only binary
cargo add wirekern             # the library crate
```

Vault: `~/.wirekern` (0700/0600). Override with `--home` or `WIREKERN_HOME`. Copy [`.env.example`](./.env.example) to `.env` (gitignored) — the CLI does **not** auto-load it.

## Quick start

Threads — paste-code OAuth; full Meta-app walkthrough in [docs/threads](./docs/threads/):

```bash
wirekern apps set threads --client-id ID --client-secret SECRET --redirect-uri 'https://example.com/callback'
wirekern auth threads --json          # print URL, paste the redirect back
wirekern post threads --text "hi" --json
```

Need a small, reviewable client-facing Threads scheduler rather than the kernel/CLI? Run the separate [Threads review app](./docs/threads/review-app.md). It has browser OAuth, explicit scheduled-post consent, privacy/deletion pages, and no reply scope by default.

Bluesky — app password; walkthrough in [docs/bluesky](./docs/bluesky/):

```bash
wirekern auth bluesky --account you.bsky.social --password 'xxxx-xxxx-xxxx-xxxx' --json
wirekern post bluesky --account you.bsky.social --text "hi" --json
```

## What it is

| | |
|--|--|
| **Job** | Official-API execution kernel. Send **now**. `Outcome.id` + `url`, or `WireError`. |
| **Not** | Scheduler, persistent inbox, social drafts, `--at`, media download/hosting |
| **You hold** | Tokens on disk. BYO Meta app / Bluesky app password. |
| **Surfaces** | `cargo add wirekern` (`Client`), the `wirekern` CLI, local MCP stdio (`wirekern mcp`), and HTTP (`pk_live_`) when a caller cannot exec. |

| Site | Capability | Auth | Limit |
|------|------------|------|--------|
| `threads` | `publish.text`, `publish.image` | OAuth paste-code, or `--token` long-lived `THQVJ…` | 500 chars (caption incl.); emoji as UTF-8 bytes; images via public https URL |
| `bluesky` | `publish.text`, `publish.image` | App password (`--password`). `--account` **is** the handle (`default` rejected) | 300 graphemes; images ≤ 2 MB (png/jpg/gif/webp) |
| `instagram` | `publish.image`, `publish.carousel`, `read.media` | Instagram Login paste-code (`instagram_business_basic,instagram_business_content_publish`), long-lived via `ig_exchange_token` | Professional account only; public HTTPS image URLs; carousel 2–10 slides; caption ≤ 2,200 characters; recent-media read is 1–25, first page only |
| `linkedin` | `publish.text` | Paste-code OAuth (`openid,profile,w_member_social`); UserInfo resolves the member ID | Public organic member text only; ≤ 3,000 Unicode characters; no author params, media, organization/Page, analytics, or ads |
| `x` | `publish.text`, `send.direct_message` | OAuth 2.0 Authorization Code + PKCE; DMs require a second opt-in auth | Public text and replies; one-to-one text DM needs `--with-dm` plus `--allow-dm`; no media, inbox, webhooks, or ads |
| `meta_ads` | `read.metrics`, `read.ad_accounts`, `create.paused_ads`, `create.ad_creative` | OAuth paste-code (`ads_read,ads_management,pages_show_list,pages_manage_ads`), long-lived via `fb_exchange_token` | ≤ 90-day reads; ads are fixed `PAUSED` |
| `facebook_pages` | `read.pages`, `publish.text`, `publish.image` | OAuth paste-code (`pages_show_list,pages_manage_posts,pages_read_engagement`), long-lived via `fb_exchange_token` | Page ID required per post; local images upload as multipart bytes |
| `whatsapp_cloud` | `send.reply`, `send.text`, `send.template`, `send.media`, `send.interactive`, `send.flow`, `read.webhook_messages`, `read.webhook_statuses` | Static System User token (`auth whatsapp_cloud --token`), Phone number ID config | Private send requires `--allow-send` + idempotency; inbound via signed webhooks; HTTPS for Meta is your reverse proxy |

### Feature coverage

| Capability | Threads | Bluesky | Instagram | LinkedIn | X | Meta Ads | Facebook Pages |
|------------|---------|---------|-----------|----------|---|----------|----------------|
| Text post | ✓ | ✓ | ✗ image required | ✓ authenticated member only | ✓ authenticated user | ✗ read-only by design | ✓ explicit `page_id` |
| Image post (`--image`) | ✓ public https URL (`--alt` ignored) | ✓ file upload, `--alt` embedded | ✓ public https URL (`--alt` ignored) | ✗ | ✗ v1 | — | ✓ local bytes, caption + alt text |
| Image carousel (repeat `--image`) | ✗ | ✗ | ✓ 2–10 public HTTPS URLs; one parent caption | ✗ | ✗ v1 | — | ✗ |
| Reply chain (repeat `--text`) | ✓ | ✗ `thread_unsupported` | ✗ `thread_unsupported` | ✗ `thread_unsupported` | ✗ `thread_unsupported` | — | ✗ `thread_unsupported` |
| Reply to existing post (`reply_to_id`) | ✓ media id | ✓ `at://` URI | ✗ unsupported | ✗ unsupported | ✓ numeric post ID | — | ✗ unsupported |
| Dry-run probe (`--dry-run`) | ✓ create-only, expires unpublished in 24h | ✗ `dry_run_unsupported` (atomic `createRecord`) | ✗ `dry_run_unsupported` | ✗ `dry_run_unsupported` | ✗ `dry_run_unsupported` | — | ✗ `dry_run_unsupported` |
| Spend / performance insights | ✗ roadmap | ✗ roadmap | ✗ roadmap | ✗ roadmap | ✗ roadmap | ✓ `insights`, daily rows by entity | ✗ roadmap |
| Paused campaign / ad set / ad create | ✗ | ✗ | — | — | — | ✓ `ads create-*`, activation unavailable | — |
| Image upload / Page link creative | ✗ | ✗ | — | — | — | ✓ account asset only; cannot deliver alone | — |
| Token refresh | ✓ auto, within 7 days of expiry | n/a (app passwords) | ✓ re-issue via `ig_refresh_token` | ✓ when LinkedIn issued a refresh token | ✓ with stored X refresh token | ✓ re-issue via `fb_exchange_token` | ✓ re-issue via `fb_exchange_token` |
| `whoami` | ✓ | ✓ | ✓ | ✓ OIDC UserInfo | ✓ `/2/users/me` | ✓ (+ first ad account resolved at auth) | ✓ |
| Recent published media (`media list`) | ✗ | ✗ | ✓ first 1–25 only | ✗ | ✗ | — | ✗ |
| Video | ✗ roadmap | ✗ roadmap | ✗ roadmap | ✗ roadmap | ✗ roadmap | — | ✗ roadmap |
| Scheduling / drafts | ✗ by design | ✗ by design | ✗ by design | ✗ by design | ✗ by design | — | ✗ by design |

Kernel-level, all sites: 0600 vault with atomic writes, CSPRNG OAuth `state`, redirect-following off, per-target results on fan-out.

### WhatsApp Cloud feature coverage

| Capability | Status |
|------------|--------|
| Text reply | ✓ Recipient WhatsApp ID, inbound `wamid`, idempotency key, and `--allow-send` |
| Session text | ✓ In-window `type=text` without `context`; same `--allow-send` + idempotency |
| Approved templates | ✓ Short `template` command for ordered body params; typed `whatsapp send` for header/footer/buttons/named params/LTO; WABA list/get/create/edit/delete |
| Media | ✓ Upload/metadata/download/delete; send image, document, audio, video, sticker (id or https) |
| Interactive and service | ✓ Buttons, list, CTA URL, location request, voice-call, location, contacts, address request, reaction, mark-as-read, typing; `recipient_type: group` |
| Catalog / order / Flows | ✓ Typed catalog, product, and order-status sends; Flow list/get/create/publish |
| Inbound / status | ✓ Signed raw-body parse; `wirekern serve` GET/POST callback with HTTP 200 ACK; local wamid ledger (not an inbox) |
| Account / multi-sender | ✓ Paginated WABA/phone/system-user reads; phone register and two-step PIN; configured sender aliases (`--sender` on `send` / `send-batch` / HTTP) |
| Bounded fan-out | ✓ `send-batch` / `send_whatsapp_many` ≤ 10, process-paced per phone; not campaigns |
| Hosted inbox, calendar, billing dashboard | ✗ by design |
| Token refresh | ✗ Static System User token is operator-managed |

## CLI

```text
wirekern post <site> --text "…"              # publish now; --to threads,bluesky fans out
wirekern post threads --text 'root' --text 'reply'   # reply chain on Threads
wirekern post threads --text 'reply' --reply-to 18367439386214650  # reply to an existing post (threads media id; bluesky takes an at:// URI)
wirekern post threads --text '…' --dry-run   # probe: publish nothing (threads)
wirekern post bluesky --image hero.png --text 'caption' --alt 'description' # image post
wirekern post instagram --image https://cdn.example.com/hero.jpg --text 'caption'
wirekern post instagram --image https://cdn.example.com/slide-1.jpg --image https://cdn.example.com/slide-2.jpg --text 'one carousel caption'
wirekern media list instagram --limit 10 --json
wirekern apps set linkedin --client-id ID --client-secret SECRET --redirect-uri 'https://example.com/callback'
wirekern auth linkedin
wirekern post linkedin --text 'Hello LinkedIn' --idempotency linkedin-1
wirekern apps set x --client-id ID --client-secret SECRET --redirect-uri 'https://example.com/callback'
wirekern auth x
wirekern post x --text 'Hello X' --idempotency x-1
wirekern auth x --with-dm                         # re-authorize for DMs
wirekern x dm --to NUMERIC_USER_ID --text 'Hello' --idempotency dm-1 --allow-dm
wirekern auth <site> [--token | --code | --password]
wirekern insights meta_ads --from 2026-06-01 --until 2026-06-30 --attribution 7d_click_1d_view --level campaign
wirekern ads create-campaign meta_ads --name 'Draft' --objective sales
wirekern ads upload-image meta_ads --file hero.png
wirekern ads create-draft meta_ads --manifest launch.paused.json --state launch.state.json
wirekern pages accounts facebook_pages --json
wirekern post facebook_pages --page-id 123 --text 'Hello from Wirekern'
wirekern whatsapp reply --to 60123456789 --reply-to wamid.inbound --text 'Hello' --idempotency reply-1 --allow-send
wirekern whatsapp send --request message.json --sender marketing --allow-send
wirekern whatsapp webhook parse --signature "$X_HUB_SIGNATURE_256" < webhook.json
wirekern whoami <site>
wirekern capabilities [site]
wirekern mcp                                 # MCP stdio for local agent hosts
wirekern accounts list|delete
wirekern apps show|set
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
wirekern = { version = "0.1", features = ["vault-file", "threads", "bluesky"] }
```

Default features are empty: `vault-file`, `client`, `oauth` (implies `client`), `threads`, `bluesky`, `instagram`, `linkedin`, `x`, `meta-ads`, `facebook-pages`, `whatsapp-cloud`, `draft` (manifest-orchestrated paused launches). `cargo add wirekern --features threads` pulls no Bluesky, no scheduler, nothing you did not ask for.

`Client::{publish, send_x_direct_message, send_whatsapp, whoami, insights, pages, media, auth_start, auth_finish, put_token, run_paused_draft}`. Connectors register on `Registry`. Refresh (Threads/Facebook/Instagram/X long-lived) runs in `Client`, not in `publish`.

## Positioning & roadmap

wirekern keeps what hosted APIs take: tokens stay in **your** vault, you bring **your own** platform apps, cost is **$0**, license MIT OR Apache-2.0. Comparison grid against hosted post APIs, self-hosted schedulers and official SDKs: **[docs/positioning.md](./docs/positioning.md)**.

Roadmap, in order: images/video. HTTP `serve` (`pk_live_`) and local MCP stdio (`wirekern mcp`) are shipped.

## Not in this version

local OAuth callback server, video, schedule, persistent inbox, Telegram, Mastodon. LinkedIn is currently member text posts only; Page posting, media, comments, analytics, and sponsored content remain separate features. HTTP for callers that cannot exec: `wirekern keys create --name n8n` then `wirekern serve` (`127.0.0.1:8788`, `Authorization: Bearer pk_live_…`). Local agent hosts: `wirekern mcp` (stdio JSON-RPC; see [docs/mcp](./docs/mcp/)). Add a site: [docs/connectors.md](./docs/connectors.md).

Notable changes, release by release: [CHANGELOG.md](./CHANGELOG.md).

License: MIT OR Apache-2.0. Repository: [github.com/proxeon/wirekern](https://github.com/proxeon/wirekern).
