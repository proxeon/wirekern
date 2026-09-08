# postkit

Official-API publish kernel. You bring the app credentials. No calendar.

Rust library + CLI. HTTP later, same JSON. Text only. Sites: **Threads** and **Bluesky**.

```text
cargo add postkit
cargo install postkit-cli
```

```text
cargo test -p postkit
cargo run -p postkit-cli -- --help
```

Lib default features are empty. CLI enables `vault-file`, `threads`, and `bluesky`. Vault: `~/.postkit` (0700/0600). Override with `--home` or `POSTKIT_HOME`.

Copy [`.env.example`](./.env.example) to `.env` (gitignored). The CLI does **not** auto-load it.

```bash
postkit apps set threads --client-id ID --client-secret SECRET --redirect-uri 'https://example.com/callback'
postkit auth threads --json          # paste redirect URL or --code
postkit post threads --text "hi" --json

postkit auth bluesky --account you.bsky.social --password 'xxxx-xxxx-xxxx-xxxx' --json
postkit post bluesky --account you.bsky.social --text "hi" --json
```

---

## What it does

| | |
|--|--|
| **Job** | Send **now** through official APIs. `Outcome.id` + `url`, or `WireError`. No `"ok": true`. |
| **Not** | Scheduler, inbox, drafts, `--at`, media, `serve` |
| **You hold** | Tokens on disk. BYO Meta app / Bluesky app password. |
| **Surfaces** | `cargo add postkit` (`Client`) and `postkit` CLI. HTTP (`pk_live_`) only when a caller cannot exec. |

| Site | Capability | Auth | Limit |
|------|------------|------|--------|
| `threads` | `publish.text` | OAuth paste-code, or `--token` long-lived `THQVJ…` | 500 chars; emoji as UTF-8 bytes |
| `bluesky` | `publish.text` | App password (`--password`). `--account` **is the handle** (`default` rejected) | 300 graphemes |

### Feature coverage

What works today, per site:

| Capability | Threads | Bluesky |
|------------|---------|---------|
| Text post | ✓ | ✓ |
| Reply chain (repeat `--text`) | ✓ | ✗ `thread_unsupported` |
| Reply to existing post (`--param reply_to_id=`) | ✓ | ✗ |
| Token refresh | ✓ long-lived, auto within 7d of expiry | n/a (app passwords) |
| `whoami` | ✓ | ✓ |
| Images / video | ✗ roadmap | ✗ roadmap |
| Scheduling / drafts | ✗ by design — send **now** or nothing | ✗ by design |

Kernel-level, all sites: 0600 vault with atomic writes, CSPRNG OAuth `state`, redirect-following off, per-target `Outcome`/`WireError` on fan-out, `--json` + exit-code contract.

### Positioning

Stable axes only (prices move; these don't):

| | **postkit** | Hosted post APIs (Ayrshare, Upload-Post, Outstand) | Self-hosted schedulers (Postiz) | Official SDKs |
|---|---|---|---|---|
| Token custody | **Your vault, 0600, local** | Their cloud | The app's DB, even self-hosted | Yours |
| Platform app | **BYO** (your Meta app) | Theirs — no App Review for you | Per-instance | BYO per SDK |
| Cost | **$0** | Paid subscription | $0 + your hosting | $0 |
| Scheduling | ✗ | ✓ | ✓ (the product) | ✗ |
| Media | ✗ roadmap | ✓ | ✓ | Varies |
| Agent surface | **exec CLI / `cargo add`** | REST + API key | Web app | Hand-rolled |
| License | **MIT OR Apache-2.0** | Proprietary | AGPL-3.0 | Vendor ToS |

Roadmap, in priority order: images/video (get on the comparison grid), HTTP `serve` mode with `pk_live_` keys (contest the hosted-API-for-agents lane without giving up custody).

Live setup: [docs/threads](./docs/threads/), [docs/bluesky](./docs/bluesky/). Add a site: [docs/connectors.md](./docs/connectors.md).

---

## CLI

Global flags (all subcommands):

| Flag | Default | |
|------|---------|--|
| `--json` | off | Document on **stdout**; human text on stderr. Agents always pass this. |
| `--home <dir>` | `~/.postkit` | |
| `--deadline <secs>` | `30` | Publish only |
| `--account <name>` | `default` | Vault alias. Bluesky: the handle |
| `--version` / `-V` | | Same string as `User-Agent` |

```text
postkit post         [site] --text "…"
postkit post         --to threads,bluesky --text "…"
postkit post         --stdin
postkit auth         <site> [--token | --code | --password]
postkit whoami       <site>
postkit capabilities [site]
postkit accounts     list|delete
postkit apps         show|set
```

### `post`

```text
postkit post <site> --text <str> [--param k=v]... [--idempotency <key>]
postkit post threads --text 'root' --text 'reply'
postkit post --to threads,bluesky --text <str>
postkit post --stdin
```

| Flag | |
|------|--|
| `<site>` or `--to a,b` | One site, or same text/`--param` on each. Mixed success → `{ "results": [ … ] }` |
| `--text` | `Body::Text`. Repeatable on **threads** = reply chain (`reply_to_id`). One `--text` is a single `Outcome`; two or more is `{ "results": [ … ] }`. Other sites: two `--text` → `thread_unsupported` before HTTP. |
| `--param k=v` | `Intent.params` (repeatable). `--param reply_to_id=` = one reply to an existing post |
| `--idempotency` | Root segment only on a chain. Honored only if the platform does |
| `--stdin` | Raw request JSON. One body. Exclusive with `--text` |

No `--token` on `post`. Auth writes the vault; `post` reads it.

`--to` uses **one** `--account` for every site. Threads is usually `default`; Bluesky is the handle — two commands, or the same alias in both vaults.

`--account` is global; it may sit before the subcommand: `postkit --account you.bsky.social post bluesky --text hi --json`.

### `auth`

```text
postkit auth threads                         # TTY: print URL, paste redirect or code
postkit auth threads --code 'AQBx-…'         # raw code or full callback URL (#_ stripped)
postkit auth threads --token 'THQVJ…'        # bootstrap; no app file
postkit auth bluesky --account you.bsky.social --password 'xxxx-xxxx-xxxx-xxxx'
```

`--token` and `--code` are exclusive. `--password` cannot mix with either. `--listen` is a stub (paste-code is the path).

No flags on Threads: `auth_start`, `open: …` on stderr, wait for paste. Non-TTY prints `then: postkit auth threads --code <code>` and exits. `--json` prints `WhoAmI` only (no token).

Threads paste-code needs `apps set` (or `POSTKIT_THREADS_CLIENT_ID` / `_CLIENT_SECRET` / `_REDIRECT_URI` in the **process** env). Redirect URI must match the Meta chip **byte-for-byte**. Bluesky does not need an app file.

### `whoami` / `capabilities`

```text
postkit whoami threads --json
postkit whoami bluesky --account you.bsky.social --json
postkit capabilities --json
# {"bluesky":["publish.text"],"threads":["publish.text"]}
```

### `apps` / `accounts`

```text
postkit apps set threads --client-id … --client-secret … --redirect-uri …
postkit apps show threads --json          # secret redacted
postkit accounts list [--site threads]
postkit accounts delete <site> --yes
```

`apps set` → `~/.postkit/apps/<site>.json`. Accounts → `~/.postkit/accounts/<site>/<name>.json`. List prints names, not tokens.

---

## Library

```toml
postkit = { version = "0.1", features = ["vault-file", "threads", "bluesky"] }
```

Features: `vault-file`, `client`, `oauth` (implies `client`), `threads = ["client", "oauth"]`, `bluesky = ["client"]` (no `oauth`). Default is empty.

`Client::{publish, whoami, auth_start, auth_finish, put_token}`. Connectors register on `Registry`. Refresh (Threads long-lived) runs in `Client`, not in `publish`.

---

## `--json`

Success is `Outcome`: `{ "site", "id", "url" }`. Fan-out is `{ "results": [ Outcome | WireError, … ] }`. Errors:

```json
{ "error": "invalid_post", "site": "threads", "reason": "text_too_long", "limit": 500 }
```

| `error` | Exit |
|---------|------|
| `unknown_site`, `unknown_account`, `unsupported`, `invalid_post` | 2 |
| `auth` | 3 |
| `rate_limited` | 4 |
| `platform`, `network`, `timeout` | 5 |

---

## Not in this version

`serve` / `pk_live_`, `--listen`, `--image` / video, schedule, inbox, LinkedIn, Telegram, Mastodon. Next site: [docs/connectors.md](./docs/connectors.md). Gates: [plans/001/004-after-kernel.md](./plans/001/004-after-kernel.md).

License: MIT OR Apache-2.0. Repository: [github.com/proxeon/postkit](https://github.com/proxeon/postkit).
