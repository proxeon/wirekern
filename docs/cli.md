# CLI reference

Global flags work on every subcommand. Output-stream contract, one rule: with `--json`, stdout carries exactly one JSON document; without it, **stdout stays empty** and every line — results included — goes to stderr. Scripts that want data on stdout pass `--json`.

## Global flags

| Flag | Default | |
|------|---------|--|
| `--json` | off | Document on **stdout**; human text on stderr. |
| `--home <dir>` | `~/.postkit` | Vault root. Also `POSTKIT_HOME`. |
| `--deadline <secs>` | `30` | Network-operation deadline. |
| `--account <name>` | `default` | Vault alias. Bluesky: the handle. Global — may sit before the subcommand: `postkit --account you.bsky.social post bluesky --text hi --json`. |
| `--version` / `-V` | | Same string as `User-Agent`. |

## `post`

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
| `--param k=v` | `Intent.params` (repeatable). `--param reply_to_id=` = one reply to an existing post (**threads**; Bluesky rejects unknown params with `unsupported_param:<k>` before HTTP) |
| `--idempotency` | Root segment only on a chain. Client-side dedupe: a retry with the same key returns the stored `Outcome` without HTTP (`~/.postkit/idempotency/…`). Only **completed** publishes are remembered — an attempt that timed out after the platform created the post was never learned and will post again |
| `--stdin` | Raw request JSON. One body. Exclusive with `--text` |
| `--dry-run` | **threads** only. Create-only probe: one container creation, no publish, nothing visible ever — the container expires in 24h. Refused with `dry_run_unsupported` on sites with no create/publish split (Bluesky), and rejected with `dry_run_idempotency` / `dry_run_chain` when combined with `--idempotency` or a reply chain |

Details that bite:

- No `--token` on `post`. Auth writes the vault; `post` reads it.
- `--to` uses **one** `--account` for every site. Threads is usually `default`; Bluesky is the handle — two commands, or the same alias in both vaults.
- Reply chains are not atomic: if a later segment fails, earlier posts stay live (delete them in the app). Reply creation retries Graph `code 24` (parent propagation) every 2s until the deadline — the window has measured ~30s on one night and 12–15 min on the next, so `--deadline 120` is not always enough: give chains with fresh parents `--deadline 1500`.
- `--dry-run` answers in exactly one attempt — no `code 24` retry, because a probe's product is the write path's *current* state. `{"site":"threads","container_id":"…","expires_in_hours":24}` = ready (a publish would succeed now); Graph `24` = valid id the reply path cannot see yet (the propagation window above — wait, then probe again); Graph `100` = not a valid media id, fix the input. A dry-run never reads or writes the `--idempotency` ledger.

## `auth`

```text
postkit auth threads                         # TTY: print URL, paste redirect or code
postkit auth threads --code 'AQBx-…'         # raw code or full callback URL (#_ stripped)
postkit auth threads --token 'THQVJ…'        # bootstrap; no app file
postkit auth bluesky --account you.bsky.social --password 'xxxx-xxxx-xxxx-xxxx'
postkit auth meta_ads                        # same paste-code flow, ads_read + ads_management
```

- `--token` and `--code` are exclusive. `--password` cannot mix with either. `--listen` is a stub (paste-code is the path).
- `--token` is an OAuth-site bootstrap (Threads). App-password sites (Bluesky) refuse it with `token_bootstrap_unsupported`.
- Threads with no flags: `auth_start`, `open: …` on stderr, waits for paste. Non-TTY prints `then: postkit auth threads --code <code>` and exits. `--json` prints `WhoAmI` only (no token).
- Threads paste-code needs `apps set` first (or `POSTKIT_THREADS_CLIENT_ID` / `_CLIENT_SECRET` / `_REDIRECT_URI` in the **process** env). Redirect URI must match the Meta dashboard chip **byte-for-byte**.
- The pasted redirect URL must echo the `state` the CLI generated: mismatched or missing `state` is rejected (`state_mismatch` / `missing_state`). The two-invocation `--code` path cannot verify `state` — paste the redirected URL unedited.
- Bluesky does not need an app file.
- `meta_ads` uses the same paste-code flow (Facebook dialog, `ads_read,ads_management` scopes) and needs `apps set meta_ads …` first — it may be the **same Meta app** as Threads. The short code is exchanged, then extended via `fb_exchange_token` (~60 days; auto re-issued by refresh while the app file exists). Auth also resolves and stores the token's **first ad account**; none → `no_ad_account`. Existing read-only tokens need re-authentication before paused creation. Runbook: [docs/meta-ads](./meta-ads/README.md).

## `whoami` / `capabilities`

```text
postkit whoami threads --json
postkit whoami bluesky --account you.bsky.social --json
postkit capabilities --json
# {"bluesky":["publish.text"],"meta_ads":["read.metrics","read.ad_accounts","create.paused_ads","create.ad_creative"],"threads":["publish.text"]}
```

## `insights` (meta_ads)

```text
postkit insights meta_ads --from 2026-06-01 --to 2026-06-30 --attribution 7d_click_1d_view
postkit insights meta_ads --from … --to … --attribution 1d_click --level campaign --metrics spend,clicks,purchases
postkit insights meta_ads --from … --to … --attribution 7d_click_1d_view --ad-account act_999
```

Read-only spend/performance metrics (the `read.metrics` capability). Daily rows (`time_increment=1`) grouped by `--level account|campaign|adset|ad`; `purchases` sums the purchase-ish rows of Graph's `actions` breakdown under the window you name.

- `--from`/`--to` are inclusive `YYYY-MM-DD`, **≤ 90 days** — the range is also the reply size, so reads stay token-bounded by construction. Longer ranges fail `invalid_query` exit 2 before any HTTP.
- `--attribution` is **required, no default**: `7d_click_1d_view | 1d_click | 1d_view`. ROAS answers change with the window; a caller who cannot say which window they meant cannot interpret the number.
- `--ad-account` overrides the account resolved at auth (accepts `123` or `act_123`).
- `--entity-id` is repeatable at `campaign|adset|ad`; `--breakdown country,publisher_platform,age` places labels under each row's `dimensions` object. `purchase_value` sums matching purchase `action_values`; `roas` is `purchase_value / spend` and is `null` without action values or with zero spend.
- `--json` prints `InsightsReply`: `{ "site", "account_id", "currency", "rows": [ { "entity_id", "level", "date_start", "dimensions": { … }, "metrics": { … } } ] }` — rows ordered by `(entity_id, date_start, dimensions)`, metric keys alphabetical: same query → same bytes.
- Auth resolves the **first** ad account the token can see and stores it in the vault (`extra.ad_account_id`); a token with no ad account fails at the door (`no_ad_account`).

## `ads` (meta_ads)

```text
postkit ads accounts meta_ads
postkit ads upload-image meta_ads --file hero.png [--ad-account act_123]
postkit ads create-link-creative meta_ads --name <name> --page-id <id> --image-hash <hash> --message <copy> --headline <headline> --destination-url https://example.com --call-to-action learn_more [--ad-account act_123]
postkit ads create-campaign meta_ads --name <name> --objective sales [--ad-account act_123]
postkit ads create-adset meta_ads --name <name> --campaign-id <id> --daily-budget <minor-units> --bid-strategy lowest_cost_without_cap --billing-event <event> --optimization-goal <goal> --targeting-file targeting.json
postkit ads create-ad meta_ads --name <name> --adset-id <id> --creative-id <id>
```

`ads accounts` discovers remote Marketing API accounts; it is not `accounts list`, which shows local vault aliases. `upload-image` returns a Meta image hash and `create-link-creative` returns a creative ID; both are non-delivering account assets, not ads. The three delivery-object creates always return `status: "PAUSED"`; they do not accept `--status`, and postkit has no activation, budget-update, or delete command. `--targeting-file` must contain a JSON object; the ad command references the returned creative ID. See [the Meta Ads runbook](./meta-ads/README.md) for the minor-unit budget rule and a no-spend validation sequence.

## `apps` / `accounts`

```text
postkit apps set threads --client-id … --client-secret … --redirect-uri …
postkit apps show threads --json          # secret redacted
postkit accounts list [--site threads]
postkit accounts delete <site> --yes
```

`apps set` → `~/.postkit/apps/<site>.json`; warns on stderr when `POSTKIT_<SITE>_*` env vars will shadow the file. `apps show` reports `"source": "env" | "file"` — env credentials outrank the file whenever both exist. Accounts → `~/.postkit/accounts/<site>/<name>.json`. List prints names, not tokens.

## Output document and exit codes

Success is `Outcome`: `{ "site", "id", "url" }`. Fan-out is `{ "results": [ Outcome | WireError, … ] }`. Errors:

```json
{ "error": "invalid_post", "site": "threads", "reason": "text_too_long", "limit": 500 }
```

| `error` | Exit |
|---------|------|
| `unknown_site`, `unknown_account`, `unsupported`, `invalid_post`, `invalid_query`, `policy_denied` | 2 |
| `auth` | 3 |
| `rate_limited` | 4 |
| `platform`, `network`, `timeout` | 5 |
