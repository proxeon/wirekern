# CLI reference

Global flags work on every subcommand. Output-stream contract, one rule: with `--json`, stdout carries exactly one JSON document; without it, **stdout stays empty** and every line — results included — goes to stderr. Scripts that want data on stdout pass `--json`.

## Global flags

| Flag | Default | |
|------|---------|--|
| `--json` | off | Document on **stdout**; human text on stderr. |
| `--home <dir>` | `~/.wirekern` | Vault root. Also `WIREKERN_HOME`. |
| `--deadline <secs>` | `30` | Network-operation deadline. |
| `--account <name>` | `default` | Vault alias. Bluesky: the handle. Global — may sit before the subcommand: `wirekern --account you.bsky.social post bluesky --text hi --json`. |
| `--version` / `-V` | | Same string as `User-Agent`. |

## `post`

```text
wirekern post <site> --text <str> [--param k=v]... [--idempotency <key>]
wirekern post threads --text 'root' --text 'reply'
wirekern post --to threads,bluesky --text <str>
wirekern post bluesky --image hero.png [--text 'caption'] [--alt 'description']
wirekern post threads --image https://cdn.example.com/hero.png [--text 'caption']
wirekern post instagram --image https://cdn.example.com/hero.jpg [--text 'caption']
wirekern post instagram --image https://cdn.example.com/slide-1.jpg --image https://cdn.example.com/slide-2.jpg [--text 'one carousel caption']
wirekern post linkedin --text 'public member post' [--idempotency <key>]
wirekern post x --text 'public X post' [--reply-to <numeric-post-id>] [--idempotency <key>]
wirekern media list instagram [--limit 10]
wirekern post --stdin
```

| Flag | |
|------|--|
| `<site>` or `--to a,b` | One site, or same text/`--param` on each. Mixed success → `{ "results": [ … ] }` |
| `--text` | `Body::Text`. Repeatable on **threads** = reply chain (`reply_to_id`). One `--text` is a single `Outcome`; two or more is `{ "results": [ … ] }`. Other sites: two `--text` → `thread_unsupported` before HTTP. |
| `--reply-to <ID>` | Sugar for `--param reply_to_id=…`. Threads: media id; Bluesky: parent `at://` URI. Prefer this spelling. Exclusive with `--param reply_to_id=`; refused on `--to` fan-out |
| `--page-id <ID>` | Sugar for `--param page_id=…`. `facebook_pages` only; exclusive with `--param page_id=` and with `--to` fan-out |
| `--param k=v` | `Intent.params` (repeatable). Remaining extras after the sugars above. Facebook Pages accepts only `page_id`; X accepts only numeric `reply_to_id`; LinkedIn accepts no params so its authenticated member remains the sole author. Other keys refuse with `unsupported_param:<k>` before HTTP |
| `--idempotency` | Root segment only on a chain. Client-side dedupe: a retry with the same key returns the stored `Outcome` without HTTP (`~/.wirekern/idempotency/…`). Only **completed** publishes are remembered — an attempt that timed out after the platform created the post was never learned and will post again. While one publish under a key is in flight (another process or task), a second call answers `idempotency` (exit 4, retry-later) instead of racing to a duplicate; a crashed holder self-heals — its claim is stolen after 15 minutes |
| `--stdin` | Raw request JSON. One body. Exclusive with every content flag — `--text`, `--image`, `--alt`, `--param`, `--to`, `--reply-to`, `--page-id`, and a positional site refuse with `stdin_exclusive` exit 2 before stdin is read (`--dry-run` and `--idempotency` still apply on top of the stdin request) |
| `--image` | Repeatable only for an Instagram **image carousel**: one image is the normal optional-caption image post; 2–10 become one carousel with a single parent caption. **Two forms, never bridged**: a local file (Bluesky uploads the bytes; png/jpg/gif/webp, ≤ 2 MB enforced locally) or a public **https** URL (Threads and Instagram crawl it; Meta is definitive on reachability, format, and size). A form the site cannot honor fails that target with `image_source_unsupported:bytes\|url` before HTTP. Refused combinations, all exit 2 before any HTTP: with a chain (`image_chain_unsupported` / `carousel_caption_multiple`), with `--param reply_to_id=` (`image_reply_unsupported` / `carousel_reply_unsupported`), with `--dry-run` (`dry_run_image_unsupported`), or carousel `--alt` (`carousel_alt_unsupported`) |
| `--alt` | Accessibility text for `--image`. Omit the flag to send no alt; `--alt` with no value is an explicit empty string (Bluesky lexicon-required, empty allowed). Threads and Instagram v1 have no verified alt field and ignore it |
| `--dry-run` | **threads** only. Create-only probe: one container creation, no publish, nothing visible ever — the container expires in 24h. Refused with `dry_run_unsupported` on sites with no create/publish split (Bluesky), and rejected with `dry_run_idempotency` / `dry_run_chain` when combined with `--idempotency` or a reply chain |

Details that bite:

- No `--token` on `post`. Auth writes the vault; `post` reads it.
- `--to` uses **one** `--account` for every site. Threads is usually `default`; Bluesky is the handle — two commands, or the same alias in both vaults.
- Reply chains are not atomic: if a later segment fails, earlier posts stay live (delete them in the app). Reply creation retries Graph `code 24` (parent propagation) every 2s until the deadline — the window has measured ~30s on one night and 12–15 min on the next, so `--deadline 120` is not always enough: give chains with fresh parents `--deadline 1500`.
- `--dry-run` answers in exactly one attempt — no `code 24` retry, because a probe's product is the write path's *current* state. `{"site":"threads","container_id":"…","expires_in_hours":24}` = ready (a publish would succeed now); Graph `24` = valid id the reply path cannot see yet (the propagation window above — wait, then probe again); Graph `100` = not a valid media id, fix the input. A dry-run never reads or writes the `--idempotency` ledger.

## `auth`

```text
wirekern auth threads                         # TTY: publish-only scopes, print URL, paste redirect or code
wirekern auth threads --with-replies          # explicit extra Threads reply-management scope
wirekern auth threads --code 'AQBx-…'         # raw code or full callback URL (#_ stripped)
wirekern auth threads --token 'THQVJ…'        # bootstrap; no app file
wirekern auth bluesky --account you.bsky.social --password 'xxxx-xxxx-xxxx-xxxx'
wirekern auth meta_ads                        # same paste-code flow, ads_read + ads_management
wirekern auth facebook_pages                  # Page scopes; re-authorize after adding this connector
wirekern auth instagram                        # Instagram Login for one professional account
wirekern auth linkedin                         # LinkedIn OAuth: openid + profile + w_member_social
wirekern auth x                                # X OAuth 2.0 Authorization Code + PKCE, public-post scopes
wirekern auth x --with-dm                      # re-authorize with dm.read + dm.write
wirekern auth whatsapp_cloud --token 'system-user-token' # static System User token
```

- `--token` and `--code` are exclusive. `--password` cannot mix with either. Paste-code is the OAuth path; there is no local callback server.
- `--token` bootstraps a connector that explicitly declares a bearer-token
  auth flow: OAuth sites such as Threads, or WhatsApp Cloud's static System
  User token. App-password sites (Bluesky) refuse it with
  `token_bootstrap_unsupported`.
- Threads with no flags: `auth_start`, `open: …` on stderr, waits for paste. It requests only `threads_basic,threads_content_publish`. `--with-replies` explicitly adds `threads_manage_replies`; re-authorize after enabling it. Non-TTY prints `then: wirekern auth threads --code <code>` and exits. `--json` prints `WhoAmI` only (no token).
- Threads paste-code needs `apps set` first (or `WIREKERN_THREADS_CLIENT_ID` / `_CLIENT_SECRET` / `_REDIRECT_URI` in the **process** env). Redirect URI must match the Meta dashboard chip **byte-for-byte**.
- The pasted redirect URL must echo the `state` the CLI generated: mismatched or missing `state` is rejected (`state_mismatch` / `missing_state`). The two-invocation `--code` path cannot verify `state` — paste the redirected URL unedited.
- Bluesky does not need an app file.
- `meta_ads` uses the same paste-code flow (Facebook dialog, `ads_read,ads_management` scopes) and needs `apps set meta_ads …` first — it may be the **same Meta app** as Threads. The short code is exchanged, then extended via `fb_exchange_token` (~60 days; auto re-issued by refresh while the app file exists). Auth also resolves and stores the token's **first ad account**; none → `no_ad_account`. Existing read-only tokens need re-authentication before paused creation. Runbook: [docs/meta-ads](./meta-ads/README.md).
- `facebook_pages` uses the Facebook dialog too, but is a separate connector
  with `pages_show_list,pages_manage_posts,pages_read_engagement` rather than
  ad scopes. Configure `apps set facebook_pages …` (or
  `WIREKERN_FACEBOOK_PAGES_CLIENT_ID` / `_CLIENT_SECRET` / `_REDIRECT_URI`) and
  complete OAuth again; an older token cannot gain the new scopes silently.
  The vault keeps the long-lived **user** token only. Page tokens are resolved
  transiently during discovery/publish and never print or persist. See the
  [Facebook Pages runbook](./facebook-pages/README.md).
- `instagram` uses Instagram Login rather than Facebook Login. It needs
  `instagram_business_basic,instagram_business_content_publish` and a
  professional Business or Creator Instagram account; it stores the resolved
  account ID and can publish one public-image URL, or a 2–10-image carousel,
  with an optional 2,200-character parent caption. There is no Page target
  parameter or text-only post. See the
  [Instagram runbook](./instagram/README.md).
- `linkedin` needs both **Share on LinkedIn** and **Sign in with LinkedIn
  using OpenID Connect** enabled in the LinkedIn Developer Portal. It requests
  `openid profile w_member_social`, resolves the member identity through OIDC
  UserInfo, and posts only public text as that stored member. The response is
  an exact LinkedIn post URN, not a constructed permalink. See the
  [LinkedIn runbook](./linkedin/README.md).
- `x` uses OAuth 2.0 Authorization Code with **PKCE**. Start with
  `auth x`, paste the complete callback URL (not just the code), then use
  `post x --text …`. Direct messages need a fresh `auth x --with-dm` so
  `dm.read dm.write` are not granted to ordinary posting tokens. The private
  send command is `wirekern x dm --to <numeric-user-id> --text … --idempotency
  <key> --allow-dm`; `--allow-dm` is required for each invocation. See the
  [X runbook](./x/README.md).
- `whatsapp_cloud` is not OAuth: configure the numeric sender with
  `wirekern whatsapp configure --phone-number-id … [--app-secret …]`, then use
  `auth whatsapp_cloud --token …`. The one-time token bootstrap verifies
  `whoami` before storing the static token. See the
  [WhatsApp Cloud runbook](./whatsapp-cloud/README.md).

## Instagram image carousel

Repeat `--image` exactly 2 through 10 times on `post instagram` to create
**one** carousel post. Every slide must be a public HTTPS URL; Wirekern creates
and waits for invisible child containers, creates/waits for one carousel
parent, then makes exactly one `media_publish` write. The optional `--text`
is the parent caption, never a per-slide caption. `--alt` is rejected for a
carousel because one generic value cannot describe all slides and v1 has no
reviewed per-slide accessibility field. A failed child or parent readiness
check creates no visible post; a confirmed post returns its final media ID and
best-effort permalink. See [the Instagram runbook](./instagram/README.md) for
the idempotency and live-validation guidance.

## `media` (instagram)

```text
wirekern media list instagram [--limit 10] [--json]
```

`media list` is the Instagram connector's read-only `read.media` capability.
It reads one server-ordered first page for the account resolved during
`auth instagram`; it cannot choose another user ID, follow pagination, create
media, update media, or download the image. `--limit` is constrained to **1
through 25** (default 10) before credentials or HTTP are touched. The JSON
reply is `{ "site", "media": [ … ] }`; every media item has an `id` and may
include Meta's `permalink`, `caption`, `media_type`, and `timestamp`. Human
output intentionally excludes captions because remote captions can contain
newlines; pass `--json` for structured caption data.

## `whoami` / `capabilities`

```text
wirekern whoami threads --json
wirekern whoami bluesky --account you.bsky.social --json
wirekern capabilities --json
# {"bluesky":["publish.text"],"meta_ads":["read.metrics","read.ad_accounts","create.paused_ads","create.ad_creative"],"threads":["publish.text"]}
```

## `whatsapp` (WhatsApp Cloud)

```text
wirekern whatsapp configure --phone-number-id <numeric-id> [--waba-id <numeric-id>] [--business-id <numeric-id>] [--sender <alias=numeric-phone-id>]... [--app-secret <Meta-app-secret>] [--verify-token <random-callback-token>]
wirekern auth whatsapp_cloud --token <System-User-token>
wirekern whoami whatsapp_cloud --json
wirekern whatsapp text --to <digits> --text <text> --idempotency <key> --allow-send
wirekern whatsapp reply --to <digits> --reply-to <inbound-wamid> --text <text> --idempotency <key> --allow-send
wirekern whatsapp template --to <digits> --name <approved_name> --language <locale> [--body-param <value>]... --idempotency <key> --allow-send
wirekern whatsapp send --request <typed-request.json> [--sender <configured-alias>] --allow-send
wirekern whatsapp send-batch --requests <typed-requests.json> [--sender <configured-alias>] --allow-send
wirekern whatsapp media <upload|metadata|download|delete> ...
wirekern whatsapp templates <list|get|create|edit|delete> ...
wirekern whatsapp flows <list|get|create|publish> ...
wirekern whatsapp account <wabas|phone-numbers|phone-health|system-users|subscribe-apps|register-phone|set-two-step-pin> ...
wirekern whatsapp ledger <get|window|purge> ...
wirekern whatsapp consent <get|set> ...
wirekern whatsapp webhook parse --signature <X-Hub-Signature-256> < raw-webhook.json
```

`configure` stores the Phone number ID, optional WABA/Business IDs, sender
aliases, and optional webhook app secret in the owner-only app configuration;
`auth` separately validates and stores the static System User token in the
vault. The config secret is never displayed by `apps show`. Environment values
`WIREKERN_WHATSAPP_PHONE_NUMBER_ID`, `_WABA_ID`, `_BUSINESS_ID`,
`_APP_SECRET`, and `_VERIFY_TOKEN` override only their corresponding file
fields; for example, an environment sender ID retains a file-backed app secret
unless the environment also supplies a replacement secret.

`whatsapp webhook parse` performs no HTTP request and returns verified inbound
messages plus `sent`, `delivered`, `read`, and `failed` callbacks for the
outbound `wamid`. It is the BYO-receiver adapter. `wirekern serve` also exposes
the challenge/acknowledgement callback and persists a minimal local delivery
ledger, but serves **HTTP only**; deploy it behind a public HTTPS reverse proxy
or tunnel before configuring Meta.

`reply` and `template` are intentionally not `post` subcommands. They are
private, recipient-specific writes: both require an idempotency key and exact
`--allow-send` acknowledgement. The default `WhatsAppPolicy` refuses before
the vault/network path if that flag is absent. A success means Meta accepted
the message and returned a `wamid`; it does **not** mean delivered or read.
Use signed status webhooks for that state.

`reply` only sends plain text and requires a WhatsApp ID (digits with country
code, no `+`) and the `wamid` of a known inbound message. `template` sends an
already approved lowercase template name with a language code and optional
ordered text body substitutions. `send` and `send-batch` deserialize the same
closed `WhatsAppSendRequest` schema used by the library and HTTP surface, so
they cover the broader typed media/interactive/catalog/Flow message set
without becoming a raw Graph JSON escape hatch. Management writes require
`--yes`; list commands return an opaque `after` cursor in `--json` output.

`webhook parse` reads one bounded raw body from stdin and verifies the exact
`sha256=` HMAC header before JSON parsing. It also refuses a callback whose
Phone number ID differs from the configured sender. Human output reports only
the message count; `--json` emits the explicit caller's inbound PII. Full
callback, privacy, and deployment boundaries: [WhatsApp Cloud
runbook](./whatsapp-cloud/README.md).

## `pages` (facebook_pages)

```text
wirekern pages accounts facebook_pages --json
wirekern post facebook_pages --page-id <PAGE_ID> --text 'A Page post'
wirekern post facebook_pages --page-id <PAGE_ID> \
  --image announcement.png --text 'Optional caption' --alt 'Image description'
```

`pages accounts` is remote discovery, not `accounts list`: it returns the
authenticated user's Page IDs, names, and Meta task strings, never Page access
tokens. Copy an ID into `--page-id` (or `--param page_id=…`) for each publish;
Wirekern never chooses the first Page. `page_id` is the only v1 Page parameter,
so unknown keys (including `reply_to_id`) fail before the vault or network.

Text posts use `/{page-id}/feed`; image posts upload a local file as multipart
bytes to `/{page-id}/photos` with optional caption and accessibility text.
Remote image URLs are refused (`image_source_unsupported:url`)—the CLI does
not fetch arbitrary URLs or host files to manufacture a Meta URL. A successful
Page post is organic and visible immediately; it has no `--dry-run`, paused,
budget, billing, scheduling, edit, delete, comment, or reply behaviour in this
release.

## `insights` (meta_ads)

```text
wirekern insights meta_ads --from 2026-06-01 --until 2026-06-30 --attribution 7d_click_1d_view
wirekern insights meta_ads --from … --until … --attribution 1d_click --level campaign --metrics spend,clicks,purchases
wirekern insights meta_ads --from … --until … --attribution 7d_click_1d_view --ad-account act_999
wirekern insights meta_ads --from … --until … --attribution 7d_click_1d_view --async-report
wirekern ads insights-job result meta_ads --id <JOB>
```

Read-only spend/performance metrics (the `read.metrics` capability). Daily rows (`time_increment=1`) grouped by `--level account|campaign|adset|ad`; `purchases` sums the purchase-ish rows of Graph's `actions` breakdown under the window you name.

- `--from`/`--until` are inclusive `YYYY-MM-DD`, **≤ 90 days** — the range is also the reply size, so reads stay token-bounded by construction. Longer ranges fail `invalid_query` exit 2 before any HTTP. `--to` is not a date flag (it is `post` fan-out).
- `--async-report` POSTs an Ad Report Run and polls until `--deadline`. The query is cached under `~/.wirekern/insights-jobs/<id>.json` so `ads insights-job result --id <JOB>` does not re-enter the form. Pass `--from`/`--until`/`--attribution` together to rebuild instead of the cache.
- `--attribution` is **required, no default**: `7d_click_1d_view | 1d_click | 1d_view`. ROAS answers change with the window; a caller who cannot say which window they meant cannot interpret the number.
- `--ad-account` overrides the account resolved at auth (accepts `123` or `act_123`).
- `--entity-id` is repeatable at `campaign|adset|ad`; `--breakdown country,publisher_platform,age` places labels under each row's `dimensions` object. `purchase_value` sums matching purchase `action_values`; `roas` is `purchase_value / spend` and is `null` without action values or with zero spend.
- `--json` prints `InsightsReply`: `{ "site", "account_id", "currency", "rows": [ { "entity_id", "level", "date_start", "dimensions": { … }, "metrics": { … } } ] }` — rows ordered by `(entity_id, date_start, dimensions)`, metric keys alphabetical: same query → same bytes.
- Auth resolves the **first** ad account the token can see and stores it in the vault (`extra.ad_account_id`); a token with no ad account fails at the door (`no_ad_account`).

## `ads` (meta_ads)

```text
wirekern ads accounts meta_ads
wirekern ads list meta_ads --entity campaign|adset|ad|creative [--ad-account act_123]
wirekern ads inspect meta_ads --entity campaign|adset|ad|creative --id <id>
wirekern ads activate meta_ads --entity campaign|adset|ad --id <id> --confirm-id <id> --allow-activate --state path [--confirm-daily-budget N]
wirekern ads pause meta_ads --entity campaign|adset|ad --id <id>
wirekern ads archive meta_ads --entity campaign|adset|ad --id <id> --confirm-id <id> --allow-archive
wirekern ads delete meta_ads --entity campaign|adset|ad --id <id> --confirm-id <id> --allow-delete --confirm-delete
wirekern ads duplicate meta_ads --entity campaign|adset|ad --id <id> --confirm-id <id> --allow-duplicate [--confirm-daily-budget N]
wirekern ads update-budget meta_ads --entity campaign|adset --id <id> --confirm-id <id> --current-daily-budget N --new-daily-budget N --allow-budget-edit
wirekern ads update-lifetime-budget meta_ads --entity campaign|adset --id <id> --confirm-id <id> --current-lifetime-budget N --new-lifetime-budget N --allow-budget-edit
wirekern ads update-bid meta_ads --entity campaign|adset --id <id> --confirm-id <id> --bid-strategy lowest_cost_without_cap --allow-bid-edit
wirekern ads update-schedule meta_ads --id <adset-id> --confirm-id <id> --start-time RFC3339 --end-time RFC3339 --allow-schedule-edit
wirekern ads update-placement meta_ads --id <adset-id> --confirm-id <id> --publisher-platform facebook [--facebook-position feed] --allow-placement-edit
wirekern ads update-targeting meta_ads --id <adset-id> --confirm-id <id> --targeting-file targeting.json --allow-targeting-edit
wirekern ads swap-creative meta_ads --id <ad-id> --confirm-id <id> --creative-id <id> --allow-creative-swap
wirekern ads insights-job status meta_ads --id <JOB>
wirekern ads insights-job result meta_ads --id <JOB>
wirekern ads insights-job cancel meta_ads --id <JOB> --yes
wirekern ads status meta_ads --entity ad --id <id> [--wait]
wirekern ads upload-image meta_ads --file hero.png [--ad-account act_123]
wirekern ads create-link-creative meta_ads --name <name> --page-id <id> --image-hash <hash> --message <copy> --headline <headline> --destination-url https://example.com --call-to-action learn_more [--ad-account act_123]
wirekern ads create-video-creative meta_ads --name <name> --page-id <id> --video-id <id> --image-hash <hash> --message <copy> --destination-url https://example.com --call-to-action learn_more [--wait]
wirekern ads create-carousel-creative meta_ads --name <name> --page-id <id> --message <copy> --call-to-action learn_more --cards-file cards.json
wirekern ads create-catalog-creative meta_ads --name <name> --page-id <id> --product-set-id <id> --link https://example.com --message <copy> --call-to-action shop_now
wirekern ads create-lead-form-creative meta_ads --name <name> --page-id <id> --image-hash <hash> --message <copy> --headline <h> --destination-url https://example.com --lead-gen-form-id <id> --call-to-action sign_up
wirekern ads create-app-install-creative meta_ads --name <name> --page-id <id> --image-hash <hash> --message <copy> --application-id <id> --object-store-url https://play.google.com/store/apps/details?id=app
wirekern ads create-campaign meta_ads --name <name> --objective sales [--daily-budget <minor-units> | --lifetime-budget <minor-units>] [--adset-budget-sharing] [--ad-account act_123]
wirekern ads create-adset meta_ads --name <name> --campaign-id <id> [--daily-budget <minor-units> | --lifetime-budget <minor-units>] [--start-time <rfc3339>] [--end-time <rfc3339>] --bid-strategy lowest_cost_without_cap --billing-event <event> --optimization-goal <goal> --country MY
wirekern ads create-adset meta_ads --name <name> --campaign-id <id> --daily-budget <minor-units> --bid-strategy cost_cap --bid-amount 200 --billing-event impressions --optimization-goal reach --country MY
wirekern ads create-adset meta_ads --name <name> --campaign-id <id> --daily-budget <minor-units> --bid-strategy lowest_cost_without_cap --billing-event impressions --optimization-goal offsite_conversions --promoted-pixel-id <id> --custom-event-type purchase --country MY
wirekern ads create-ad meta_ads --name <name> --adset-id <id> --creative-id <id>
wirekern ads validate-draft meta_ads --manifest launch.paused.json
wirekern ads create-draft meta_ads --manifest launch.paused.json --state launch.state.json
wirekern ads resume-draft meta_ads --manifest launch.paused.json --state launch.state.json
wirekern ads status-draft meta_ads --state launch.state.json [--wait]
wirekern ads adopt-draft-step meta_ads --state launch.state.json --step adset --id <id>
```

`ads accounts` discovers remote Marketing API accounts; it is not `accounts list`, which shows local vault aliases. `ads list` pages one kind of object in a selected account (capped, sorted by id) and is GET-only. `ads inspect` reads budget, bid, targeting, Page, and destination on one known ID — the confirmation surface needed before any later activate. `ads activate` is denied by default (`paused_only`); `--allow-activate` plus `--confirm-id` (and a budget echo when the object has one) are required, review must be settled PAUSED, and an ambiguous POST is `reconciliation_required` rather than a retry. `ads status` reads one campaign, ad set, or ad's `configured_status`, `effective_status`, and Meta review issues. `--wait` is an explicit bounded poll using global `--deadline`; pending review returns a successful `{ "review": "pending_review", "status": … }` reply rather than changing the draft. It has no `--ad-account` because Meta IDs are globally addressable. `upload-image` returns a Meta image hash and `create-link-creative` returns a creative ID; both are non-delivering account assets, not ads. The three delivery-object creates always return `status: "PAUSED"`; they do not accept `--status`, and wirekern has no activation, budget-update, or delete command. `--targeting-file` must contain a JSON object; the ad command references the returned creative ID.

The five `*-draft` commands compose the primitives into one checkpointed launch. `validate-draft` is pure local parsing (no vault, file, or network I/O). `create-draft`/`resume-draft` execute image → campaign → ad set → creative → ad, checkpointing each confirmed remote ID into the owner-only `--state` file; a resume runs only the remaining steps. A write whose remote outcome is unknown (network/deadline after send) returns `{"state":"reconciliation_required","step":"…","guidance":"…"}` with **exit 0** — the protocol succeeded and the next move is human: find or create the step's paused object in Ads Manager, then `adopt-draft-step` records its ID (delivery objects are remotely verified `PAUSED` first). Semantic manifest changes refuse resume (`draft_manifest_changed`); reformatting does not. `draft_state_exists` protects a second launch from adopting an old checkpoint, and `draft_busy` means another run (or a stale `<state>.lock` after a crash) holds the state. See [the Meta Ads runbook](./meta-ads/README.md) for the manifest example, the state-file contract, and the no-spend validation sequence.

## `apps` / `accounts`

```text
wirekern apps set threads --client-id … --client-secret … --redirect-uri …
wirekern apps show threads --json          # secret redacted
wirekern accounts list [--site threads]
wirekern accounts delete <site> --yes
```

`apps set` → `~/.wirekern/apps/<site>.json`; warns on stderr when `WIREKERN_<SITE>_*` env vars will shadow the file. `apps show` reports `"source": "env" | "file"` — env credentials outrank the file whenever both exist. Accounts → `~/.wirekern/accounts/<site>/<name>.json`. List prints names, not tokens.

## `keys` / `serve`

```text
wirekern keys create --name n8n
wirekern keys list
wirekern keys revoke --name n8n --yes
wirekern serve [--bind 127.0.0.1:8788]
wirekern-serve [--bind 127.0.0.1:8788]   # same listen path, HTTP-only binary
wirekern mcp                             # MCP stdio; stdout is JSON-RPC only
wirekern-mcp                             # same stdio path, MCP-only binary
```

For callers that cannot exec the binary. `keys create` prints `pk_live_` + 32 random bytes (unpadded base64url) **once** and stores SHA-256 of the full string in `~/.wirekern/keys/<name>.json` (0600). `serve` binds loopback by default, requires at least one key, and speaks the same JSON as `--json`.

`wirekern mcp` is for local agent hosts (Claude Desktop, Cursor, Grok). The host spawns the process; JSON-RPC is newline-delimited on stdin/stdout. Omit `--json` — that document would corrupt the protocol pipe. There is no `pk_live_` on this path: the operator who configured the host already has the vault. Tool list and safety notes: [docs/mcp](./mcp/).

`wirekern serve --json` writes **one** listen document to stdout (`{"bind","listening":true,"pid"}`) and stays running. A script should parse that document and not wait for the process to exit; each later result is an HTTP response body.

| HTTP | CLI |
|------|-----|
| `POST /v1/posts` | `wirekern post --stdin --json` |
| `POST /v1/whatsapp` | Typed WhatsApp send (`"allow_send": true`) |
| `POST /v1/whatsapp/webhook` | `wirekern whatsapp webhook parse --signature …` (raw body, HMAC; not a listener) |
| `GET\|POST /v1/whatsapp/callback` | Meta webhook challenge / signed event acknowledgement; requires external HTTPS termination |
| `GET /v1/whatsapp/events/{wamid}` | Read a locally recorded delivery row |
| `GET /v1/capabilities` | `wirekern capabilities --json` |
| `GET /v1/accounts?site=` | `wirekern accounts list --json` |
| `GET /v1/whoami?site=&account=` | `wirekern whoami --json` |
| `GET /v1/insights` | `wirekern insights --json` |
| `GET /v1/ads/accounts` | `wirekern ads accounts --json` |
| `GET /v1/ads/list` | `wirekern ads list --json` |
| `GET /v1/ads/inspect` | `wirekern ads inspect --json` |
| `GET /v1/ads/status` | `wirekern ads status --json` |

`Authorization: Bearer pk_live_…`. Optional `Idempotency-Key` and `X-Wirekern-Deadline` (seconds, default 30). Auth dances and `--token` stay on the CLI.

`POST /v1/whatsapp` is the HTTP twin of `--allow-send`: the JSON body must include `"allow_send": true` plus a typed `message` and `idempotency_key`. Omitted or false is `policy_denied` before vault access. `/v1/posts` cannot send WhatsApp (`unsupported` / `use_whatsapp_command`).

`POST /v1/whatsapp/webhook` parses one signed raw body (`X-Hub-Signature-256`) for BYO receivers (requires `pk_live_`). Optional `?status_extras=true` or `X-Wirekern-Status-Extras: true` includes recipient/conversation/pricing.

Meta-facing transport is `GET|POST /v1/whatsapp/callback` (no bearer): GET
echoes `hub.challenge` when `hub.verify_token` matches; POST HMAC-verifies,
ACKs HTTP 200, and records correlation metadata on the local ledger. Query
`GET /v1/whatsapp/events/{wamid}` with a key. `wirekern serve` is plain HTTP,
so Meta must reach it through an HTTPS reverse proxy or tunnel that preserves
the raw body and `X-Hub-Signature-256` header.

WhatsApp idempotency: a confirmed success is not resent. If the request left the machine and the response was lost, Wirekern does not retry — reconcile via the delivery webhook first.

HTTP status tracks `WireError`: 404 unknown site/account, 401 auth, 422 usage, 429 rate/idempotency, 502 platform, 503 network/timeout. No `"ok": true`.

## Output document and exit codes

Success is `Outcome`: `{ "site", "id", "url", "account" }` — `account` echoes the alias the post went out under (also on idempotency-ledger replays). Fan-out is `{ "results": [ Outcome | WireError, … ] }`. Errors:

```json
{ "error": "invalid_post", "site": "threads", "reason": "text_too_long", "limit": 500 }
```

| `error` | Exit |
|---------|------|
| `unknown_site`, `unknown_account`, `unsupported`, `invalid_post`, `invalid_query`, `policy_denied` | 2 |
| `auth` | 3 |
| `rate_limited` | 4 |
| `platform`, `network`, `timeout` | 5 |
