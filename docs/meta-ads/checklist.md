# Meta Ads — shipped vs missing

Tick when shipped. This is the full board, not only gaps.

Product rule: this connector is a **paused-first Marketing API kernel**. It
reads performance and assembles drafts that cannot deliver. It is not Ads
Manager, not a spend engine, and not a generic Graph client.
`PausedOnlyAdsPolicy` refuses activation and budget mutation before vault or
HTTP. A successful create is `status: "PAUSED"`, never a delivery claim.

Last aligned with Meta Marketing API / Graph docs and
[008-meta-ads-connector-online-references.md](../../plans/002-references/008-meta-ads-connector-online-references.md)
(2026-09-12). Graph pin: **`v26.0`** (current as of 2026-09-12; released
2026-07-29). Narrative order lives in [roadmap.md](./roadmap.md).

## Auth and account

- [x] Paste-code OAuth (`ads_read,ads_management,pages_show_list,pages_manage_ads`)
- [x] Long-lived exchange via `fb_exchange_token` (~60 days); refresh when the app file exists
- [x] `whoami` (user id / handle; no token)
- [x] First visible ad account stored at auth for backwards-compatible defaults
- [x] `ads accounts` lists remote `act_<id>` + name, currency, timezone, status
- [x] `--ad-account` override (`123` or `act_123`); never silently pick among many on a write
- [x] `no_ad_account` at the door when the token sees zero accounts
- [x] System User token path (unattended; not a user token reused as a secret)
- [x] Token debug / inspect as a first-class verb
- [x] Marketing API Access Tier (Limited vs Full) as operator-facing status (docs/ops, not a bypass)

## Insights (sync, bounded)

- [x] Levels: `account` | `campaign` | `adset` | `ad`
- [x] Inclusive range ≤ 90 days; longer fails `invalid_query` before HTTP
- [x] Metrics: `spend`, `impressions`, `clicks`, `reach`, `ctr`, `cpc`, `cpm`, `purchases`, `purchase_value`, `roas`
- [x] Attribution required: `7d_click_1d_view` | `1d_click` | `1d_view` (no silent default)
- [x] Breakdowns: `country`, `publisher_platform`, `age` (rows not summable across dimensions)
- [x] Repeatable `--entity-id` filter (not at account level)
- [x] Cursor paging, cap 50 pages (`paging_exceeded`); opaque cursors only
- [x] Deterministic row order `(entity_id, date_start, dimensions)`
- [x] Additional typed metrics (each with a definition so it cannot be read as a billing total)
- [x] Additional typed breakdowns / attribution windows
- [x] Async / bulk Insights jobs (bounded job, cancel, poll deadline, result-size cap)
- [x] Separate creative / delivery report contracts (not one unbounded Graph query)

## Paused creates

Every delivery object hard-codes `status=PAUSED` in the connector. There is
no `--status`. Meta still allows `ACTIVE` at create; Postkit never sends it.

- [x] Campaign: typed objectives `awareness` | `traffic` | `engagement` | `leads` | `app_promotion` | `sales` → `OUTCOME_*`
- [x] Campaign: `special_ad_categories` (explicit; blank = none)
- [x] Ad set: `daily_budget` in account minor units (positive integer)
- [x] Ad set: bid strategy `lowest_cost_without_cap` only
- [x] Ad set: `billing_event` + `optimization_goal` (required strings; pairing checked on manifests)
- [x] Ad set: targeting from a reviewed JSON object file
- [x] Ad: name + `adset_id` + `creative_id` (no creative/Page/tracking defaults)
- [x] Policy gate `CreatePausedCampaign` / `CreatePausedAdset` / `CreatePausedAd` before credentials
- [x] Unknown post-send results are not retried (draft protocol records `reconciliation_required`)
- [x] Closed enums for `billing_event` / `optimization_goal` on the primitive CLI (manifest already pairs awareness-family)
- [x] Typed targeting (geo, age, placements) instead of a pass-through JSON object
- [x] Lifetime budget, campaign-level budget, ad-set budget sharing
- [x] Other bid strategies (`COST_CAP`, bid cap, min ROAS) — each needs its constraint fields first
- [x] Schedule (`start_time` / `end_time`) as typed fields
- [x] Promoted object (pixel, app, Page, catalog) as a typed field

## Assets, creatives, previews

- [x] Multipart image upload → account image hash (path never in errors)
- [x] Page-backed image-link creative (`object_story_spec`); HTTPS destination required
- [x] CTA `learn_more` only
- [x] Preview `desktop_feed_standard` | `mobile_feed_standard` to a **new** local HTML file
- [x] Policy gate `UploadAdImage` / `CreateLinkAdCreative` (assets cannot spend alone)
- [x] Additional image-link CTAs (each has extra Meta value requirements)
- [ ] Video upload + processing/status poll
- [ ] Video creative
- [ ] Carousel
- [ ] Catalog / dynamic creative
- [ ] Lead-form creative
- [ ] App-install creative
- [ ] Instagram actor / identity on the creative
- [ ] Advantage+ creative assembly
- [ ] Ads in WhatsApp Status (Marketing API v26.0 addition)

## Review and paused-draft launches

- [x] `ads status` GET-by-id: `configured_status` vs `effective_status` + `issues_info`
- [x] `ads status --wait`: poll every 2s until `--deadline`; pending is `{ "review": "pending_review" }`, not activation
- [x] Manifest `validate-draft` (local only; unknown keys refused; no status/active keys)
- [x] `create-draft` / `resume-draft`: image → campaign → ad set → creative → ad
- [x] State file `0600`, canonical manifest fingerprint, `draft_manifest_changed` / `draft_state_exists` / `draft_busy`
- [x] Write-ahead `in_flight`; timeout after send → `reconciliation_required` (exit 0)
- [x] `adopt-draft-step` records a human-found ID; delivery objects verified `PAUSED` first
- [x] `status-draft --wait` GET-only over checkpointed IDs
- [x] Live no-spend validation (2026-09-10, `act_1414222080648203`)
- [ ] List/inventory: campaigns, ad sets, ads, creatives for a selected account (capped pages, stable order)
- [ ] Readback of budget, bid, targeting, Page, destination on a known object (needed before any activate)

## Lifecycle and edits (not shipped; policy stubs exist)

`AdsAction::Activate` and `AdsAction::UpdateBudget` exist and the default
policy **denies** them (`paused_only`). No CLI/HTTP/MCP verb.

- [ ] Typed `PAUSED` → `ACTIVE` with explicit confirmation of delivery + budget consequences
- [ ] Typed `ACTIVE` → `PAUSED` emergency stop
- [ ] Refuse activate when configured status is not paused or review is unresolved
- [ ] Write-ahead / no blind retry on ambiguous activation
- [ ] Archive
- [ ] Delete (destructive; separate proposal)
- [ ] Duplicate (can inherit delivery/budget; separate proposal)
- [ ] Typed daily-budget edit (currency, current/new minor units, max-change guard)
- [ ] Typed bid-strategy edit
- [ ] Typed schedule edit
- [ ] Typed placement edit
- [ ] Typed targeting edit with before/after diff; refuse special-ad-category contract changes
- [ ] Creative swap on an existing ad

## Surfaces

| Verb | Library | CLI | MCP | HTTP `serve` |
|------|---------|-----|-----|--------------|
| Insights | [x] | [x] | [x] `insights` | [ ] |
| Ad account list | [x] | [x] | [x] `ads_accounts` | [ ] |
| Paused creates / drafts | [x] | [x] | [ ] | [ ] |
| Status / preview | [x] | [x] | [ ] | [ ] |
| Activate / budget edit | policy deny | [ ] | [ ] | [ ] |

- [x] Not routable via generic `post` (`publish_unsupported`)
- [x] MCP omits creates (no unattended spend over a tool call)
- [ ] HTTP ads reads (only if a named non-exec caller needs them; `pk_live_` is not a spend key)
- [ ] MCP paused creates (only with a confirmation field as strict as `--allow-send`)

## Reliability already in the kernel

- [x] Graph version is one connector constant (`v26.0`), not env, not a flag
- [x] Rate-limit → `rate_limited` (exit 4); no blind retry of creates
- [x] Meta `error_user_msg` preserved when present; tokens redacted
- [x] `error_user_title` / Marketing subcodes used when they change guidance
- [ ] Dedicated regression fixture per Marketing error shape that changes retry/guidance (partial today)

## Not this kernel

Do not tick these in as “remaining Meta Ads work.” They are other products,
dead APIs, or custody violations.

- Billing, payment methods, available funds, invoices, tax, auto-reload
- Automated rules, experiments, optimization automation
- Custom audiences, lookalikes, saved audiences
- Pixels / Conversions API (measurement product; needs its own consent design)
- Lead-collection workflows
- Advantage+ Shopping Campaigns / Advantage+ App Campaigns create or update (removed v25.0+)
- Delivery-estimate fields `daily_outcomes_curve`, `budget_guardrail`, `estimate_dau` (removed v26.0)
- Ad-status webhooks until a durable signed receiver + retention design exist (poll `ads status` instead)
- Graph batch as a substitute for atomic writes
- Raw Graph JSON / “run this Marketing API path” escape hatch
- Unversioned Graph calls (Marketing API forbids them)

## Suggested kernel order

1. Inventory reads (list campaigns / ad sets / ads for one `act_`, capped)
2. Tier C: confirmed activate + emergency pause (default policy still deny)
3. One edit family: daily budget only, with confirmation + max-change guard
4. Close targeting / billing / optimization on the primitive CLI (typed, not raw JSON/strings)
5. One new creative format (second CTA **or** video, not both)
6. Reporting breadth after delivery objects can be listed and paused/activated on purpose

No row starts just because Meta exposes the endpoint. It starts when policy,
typed payload, timeout/idempotency, tests, and a live paused/low-budget
validation plan are written down.
