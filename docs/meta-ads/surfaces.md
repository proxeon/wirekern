# Meta Ads surfaces and error-fixture slice

Implementation notes for the remaining Surfaces and Reliability checklist
rows. Validated against
[008-meta-ads-connector-online-references.md](../../plans/002-references/008-meta-ads-connector-online-references.md)
items 20–21,
[Graph error handling](https://developers.facebook.com/docs/graph-api/guides/error-handling/),
and the [Marketing API error reference](https://developers.facebook.com/docs/marketing-api/error-reference)
(2026-09-12). Graph pin stays `v26.0`.

Product rule: `pk_live_` authenticates a loopback HTTP caller; it is **not**
a spend key. HTTP ads routes are GET-only. MCP paused creates cannot spend
but still require an explicit confirmation field as strict as WhatsApp
`allow_send`. Activate, budget edits, archive, delete, and duplicate stay
off both HTTP and MCP.

## 1. HTTP ads reads

Named non-exec callers (n8n, scripts) already use `POST /v1/posts` with
`Authorization: Bearer pk_live_…`. Ads reads belong on the same listener so
they do not invent a second auth story.

| HTTP | Kernel | CLI twin |
|------|--------|----------|
| `GET /v1/insights` | `Client::insights` | `insights` |
| `GET /v1/ads/accounts` | `Client::ad_accounts` | `ads accounts` |
| `GET /v1/ads/list` | `Client::list_ads_inventory` | `ads list` |
| `GET /v1/ads/inspect` | `Client::inspect_ads_object` | `ads inspect` |
| `GET /v1/ads/status` | `Client::ad_review_status` | `ads status` (no `--wait`) |

Query: `site` (required except insights default `meta_ads`), `account`
(default `default`), plus verb fields (`from`/`to`/`attribution` for
insights; `entity`/`id`/`ad_account` for ads). Deadline is still
`X-Postkit-Deadline`. Missing bearer is 401. `pk_live_` never installs
`AllowAdsActionPolicy`.

Not on HTTP: paused creates, activate, pause, archive, delete, duplicate,
typed edits, creative preview (preview is a local file write).

## 2. MCP paused creates

Tool `ads_create_paused`. Body is `CreatePausedAdRequest` plus:

- `allow_create: true` (omitted/false → `policy_denied` /
  `explicit_paused_create_required` **before** `Client`)
- `account`, `deadline`

Uses the **deny-by-default** client (paused creates are already allowed by
`PausedOnlyAdsPolicy`). The extra boolean is the WhatsApp-style
acknowledgement so a host cannot spawn drafts by listing the tool.

`destructiveHint: true` (remote write) even though the object is `PAUSED`.
No MCP activate/budget.

## 3. Marketing error-shape fixtures

`map_graph_error` already classifies 190 → `token_expired` and 4/17/32/613
→ `rate_limited`, and prefers `error_user_msg`. Gaps vs Meta's tables:

| Shape | Guidance |
|-------|----------|
| code **80004** (ads-management rate limit) | `rate_limited` (not Platform 80004) |
| code **341** (application limit) | `rate_limited` |
| code **102** (API session) | `token_expired` like 190 when no blocking subcode |
| code 190 + subcode **459** / **458** / **464** (checkpoint / app not installed / unconfirmed) | Auth, reason **not** `token_expired` — refresh cannot clear a checkpoint |
| code 190 + subcode **463** / **467** / absent | `token_expired` (refresh) |
| `error_user_title` + `error_user_msg` | Platform message `"title: user_msg"` so title changes operator guidance |

One table-driven test per row. Tokens in bodies stay redacted by existing
path tests.

## Out of scope

HTTP ads writes, MCP activate/budget, Streamable HTTP MCP, `rmcp`.
