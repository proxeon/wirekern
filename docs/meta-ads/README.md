# meta_ads runbook

Spend/performance insights plus paused-first management through the Meta Marketing API (Graph `v26.0`, pinned). Scope: `ads_read,ads_management`. Creates still hard-code `status=PAUSED`. CLI can activate, pause, archive, delete, duplicate, and edit budget/bid/schedule/placement/targeting/creative; default `PausedOnlyAdsPolicy` **denies** every spend-starting action until an explicit `--allow-*` flag. HTTP ads routes are GET-only (`pk_live_` is not a spend key). MCP paused creates require `allow_create: true`.

Deferred lifecycle, editing, creative, and reporting work is tracked in the
[Meta Ads roadmap](./roadmap.md). Shipped vs missing board:
[checklist.md](./checklist.md).

## One-time setup (operator)

1. **Meta app.** Create one at [developers.facebook.com/apps](https://developers.facebook.com/apps) (or reuse your Threads app — one app can serve both sites). Add the **Marketing API** product.
2. **Redirect URI.** In the app's **Facebook Login for Business → Settings**, add `https://example.com/callback`. `localhost` is **rejected** by the dashboard ("This is an invalid redirect URI") — a dummy domain is fine for paste-code: the browser landing on a 404 is expected; you copy the address bar. Chip-field discipline: typing then Save is not enough — click the dropdown row that appears under the field so the URL becomes a pill, then Save; if the pill is gone on reload it did not save. It must match the `--redirect-uri` you pass to `apps set` **byte-for-byte**.
3. **App config into the vault:**

   ```bash
   wirekern apps set meta_ads --client-id <APP_ID> --client-secret <APP_SECRET> \
     --redirect-uri 'https://example.com/callback'
   ```

   or process env: `WIREKERN_META_ADS_CLIENT_ID` / `_CLIENT_SECRET` / `_REDIRECT_URI`.

4. **Authenticate (paste-code, same flow as Threads):**

   ```bash
   wirekern auth meta_ads
   # open: https://www.facebook.com/dialog/oauth?...scope=ads_read%2Cads_management...
   # approve → browser lands on https://example.com/callback?code=…&state=…#_
   # (page looks broken/404 — that is expected) paste the full address bar back
   ```

   The code is exchanged for a short token, extended via `fb_exchange_token` (~60 days), and the token's **first ad account** is resolved and stored for backwards-compatible defaults. A token with no ad account fails immediately (`no_ad_account`). When more than one account is visible, discover the IDs first and pass the desired account explicitly on each insights or paused-create call. Existing `ads_read` tokens can keep reading, but must be re-authorized to gain `ads_management` before a create succeeds.

Insights extras: `--metrics` now includes `frequency`, `unique_clicks`,
`inline_link_clicks`, `inline_link_click_ctr`, `quality_ranking`,
`video_thruplay` (each defined so it is not an invoice). Attribution also
accepts `7d_click`, `28d_click`, `7d_view`, `28d_view`. Breakdowns add
`gender`, `device_platform`, `platform_position` (max two). `--report
delivery|creative` refuses mixed field lists. Large queries:
`insights … --async-report` then `ads insights-job status|result|cancel`.
`result --id <JOB>` reuses the query cached at start; pass `--from`/`--until`/`--attribution` together to rebuild.

### Unattended System User token

Do **not** paste a user OAuth token into a cron job. Create a System User in
Business Manager, assign the ad account, generate a token for this app, then:

```bash
wirekern auth meta_ads --token '<SYSTEM_USER_TOKEN>' --system-user --json
```

Wirekern calls `GET /debug_token` with the app access token. It refuses PAGE/APP
tokens and person OAuth tokens (`type=USER` with a non-zero `expires_at`).
Never-expiring tokens that Meta still labels `USER` are accepted — `/debug_token`
often types System Users that way. It then verifies `/me` and stores
`token_kind=system_user` plus the first ad account. System User tokens
are **not** refreshed via `fb_exchange_token`; generate a new token in
Business Manager when Meta invalidates one.

```bash
wirekern ads inspect-token meta_ads --json
# type, is_valid, expires_at, scopes, user_id — never the token
wirekern ads access-tier meta_ads --json
# limited | full | unknown. App Dashboard is authoritative.
```

Marketing API Access Tier (Limited vs Full) is an **app** setting, not a
Wirekern flag. Check **App Dashboard → App Review → Permissions and features →
Marketing API Access Tier**. Full access needs 500 Marketing API calls in 15
days with <15% errors. Wirekern cannot grant or bypass the tier.

Graph `200` / `API access blocked` on `auth` / `whoami` / insights is the
Facebook user (checkpoint or developer verify), not a bad vault file.
Identity work is mobile-first; see [meta-identity.md](../meta-identity.md).

## Dev mode is enough to start

A development-mode app can call the Marketing API for **accounts owned by the app's admins/developers/testers** — reading and paused-draft creation on your own ad account need no app review. `ads_read` / `ads_management` Advanced Access + business verification are only required when serving *other people's* accounts (i.e., when serving customers); file that review when that day comes, not before.

## Reading metrics

First, list the **remote Meta accounts** visible to this credential. This is
not the same as `wirekern accounts list`, which only lists local vault aliases.

```bash
wirekern ads accounts meta_ads --json
# copy an `act_<id>` value into --ad-account
```

```bash
wirekern insights meta_ads --from 2026-06-01 --until 2026-06-30 \
  --attribution 7d_click_1d_view --ad-account act_123 \
  --level campaign --entity-id 238001 \
  --breakdown country,publisher_platform \
  --metrics spend,purchases,purchase_value,roas --json
```

- `--level account|campaign|adset|ad`; default metrics `spend,impressions,clicks,purchases` (`reach,ctr,cpc,cpm,purchase_value,roas` available).
- Range ≤ 90 days inclusive; longer → `invalid_query` exit 2 before any HTTP.
- `--ad-account act_999` overrides the stored account. The account list command returns canonical `act_<id>` values plus name, currency, timezone, and status.
- Repeat `--entity-id <id>` to filter campaign, ad set, or ad reports. Account-level filtering is rejected because the account is already selected by `--ad-account` / the stored default.
- `--breakdown country,publisher_platform,age` adds labels under each JSON row's `dimensions` object; do not sum rows with different dimensions as though they were one unbroken-down result.
- `purchase_value` is the matching purchase `action_values` total. `roas` is `purchase_value / spend` per row and is `null` when Meta omits action values or spend is zero.
- Output is deterministic: rows by `(entity_id, date_start, dimensions)`, alphabetical metric and dimension keys.

## Paused-first management

Every Tier B create is approved by `PausedOnlyAdsPolicy` before wirekern reads
credentials or opens a network connection. The connector—not a CLI flag—adds
`status=PAUSED` to every Marketing API form. It exposes no command that can
activate delivery or change a budget after creation.

Prepare targeting as a JSON object in a reviewable file, for example
`targeting.json`:

```json
{
  "geo_locations": { "countries": ["MY"] },
  "age_min": 25,
  "age_max": 45
}
```

Then create the three drafts in dependency order. All values that could affect
a later delivery are explicit; wirekern supplies no objective, targeting,
creative, or budget default.

```bash
# Returns {"entity":"campaign","id":"…","status":"PAUSED",…}
wirekern ads create-campaign meta_ads --ad-account act_123 \
  --name 'Wirekern validation — do not activate' --objective sales \
  --special-ad-categories '' --json

# daily_budget is in the account currency's minor unit (ILS: agorot).
wirekern ads create-adset meta_ads --ad-account act_123 \
  --name 'Wirekern validation ad set — do not activate' --campaign-id <CAMPAIGN_ID> \
  --daily-budget 2500 --bid-strategy lowest_cost_without_cap \
  --billing-event IMPRESSIONS --optimization-goal REACH \
  --targeting-file targeting.json --json

# Uploading is an account-asset write, not an ad create: it has no delivery
# status and cannot spend. Copy the returned hash into the explicit creative.
wirekern ads upload-image meta_ads --ad-account act_123 --file hero.png --json

# A Page image-link creative is also non-delivering. Page identity, copy,
# destination, and CTA are all explicit; `learn_more` is the only supported
# CTA until its alternatives have their own typed value requirements.
wirekern ads create-link-creative meta_ads --ad-account act_123 \
  --name 'Wirekern validation creative' --page-id <PAGE_ID> \
  --image-hash <IMAGE_HASH> --message 'A clear benefit' \
  --headline 'Learn more' --destination-url https://example.com/offer \
  --call-to-action learn_more --json

# A preview is a read of the saved creative, not an ad creation. Meta returns
# iframe markup, so wirekern writes it only to the chosen owner-only file; open
# that file locally to review the Page identity, copy, image, link, and CTA.
wirekern ads preview-creative meta_ads --creative-id <CREATIVE_ID> \
  --ad-format desktop_feed_standard --output preview.html --json

# The final dependency is still a structurally paused ad.
wirekern ads create-ad meta_ads --ad-account act_123 \
  --name 'Wirekern validation ad — do not activate' --adset-id <ADSET_ID> \
  --creative-id <CREATIVE_ID> --json

# Inspect configured versus effective state. This is a GET-only operation;
# `--wait` polls only until the command's --deadline and returns a clear
# pending_review result if Meta has not finished processing the draft.
wirekern --deadline 30 ads status meta_ads --entity ad --id <AD_ID> --json
wirekern --deadline 30 ads status meta_ads --entity ad --id <AD_ID> --wait --json
```

- `--status` does not exist. A request cannot opt out of `PAUSED`.
- `--ad-account` accepts `123` or canonical `act_123`; omitting it uses the
  account stored during OAuth.
- `--daily-budget` and `--lifetime-budget` are mutually exclusive positive
  integers in account minor units. Put the budget on the campaign (`--daily-budget`
  / `--lifetime-budget` on `create-campaign`) for CBO and omit it on the ad set,
  or put it on the ad set and omit it on the campaign. `--adset-budget-sharing`
  is Meta's ABO child-share flag (up to 20%) and is refused with a campaign
  budget (Graph 4834002). Amounts do not
  spend while every object is paused, but they are still reviewed policy input
  for a future activation workflow.
- `--bid-strategy` is required: `lowest_cost_without_cap` (budget only),
  `lowest_cost_with_bid_cap` / `cost_cap` (need `--bid-amount` in minor units),
  or `lowest_cost_with_min_roas` (needs `--roas-average-floor`; Meta scale
  10000 = 1.0 ROAS). A cap strategy without its constraint fails locally.
- `--start-time` / `--end-time` are RFC3339 (Meta's `±HHMM` offset is accepted).
  `--lifetime-budget` requires `--end-time`. When both are set, end must be after start.
- Promoted object is a closed kind: `--promoted-page-id`, `--promoted-pixel-id`
  plus `--custom-event-type`, `--promoted-application-id` plus `--object-store-url`,
  or `--promoted-product-set-id` plus `--custom-event-type`. Required for
  `offsite_conversions`, `app_installs`, `page_likes`, `value`, and
  `lead_generation`. IDs are numeric; kinds cannot be mixed.
- `--targeting-file` must contain a JSON object. Meta performs the final
  platform-specific targeting validation; wirekern refuses malformed local
  data before any HTTP request.
- Image uploads and link creatives are account assets, never delivery objects.
  They still pass through the policy gate before credentials or HTTP, but they
  cannot spend until a separately created (and still `PAUSED`) ad references
  the creative.
- `--file` is read locally only by the CLI. Its filesystem path is never sent
  to Meta or included in a wirekern error; only its basename and bytes upload.
- `--page-id`, `--image-hash`, `--message`, `--headline`, destination HTTPS
  URL, and `--call-to-action` are all required. Website CTAs (`learn_more`,
  `shop_now`, …) send `value.link` as the destination. `like_page` / `call_now`
  / `whatsapp_message` send `value.page`. `get_directions` needs `--geo-link`;
  `install_app` needs `--application-id` and `--app-link`.
- `preview-creative` accepts only `desktop_feed_standard` and
  `mobile_feed_standard` until other placement contracts have explicit types
  and tests. The Creative ID is globally addressed by Meta, so no
  `--ad-account` flag is accepted for this read. `--output` must name a new
  file; Wirekern will not overwrite an earlier preview.
- A preview is visual QA only. It never creates an ad, changes a draft, adds
  funds, or enables delivery. Meta preview iframe URLs may be short-lived, so
  regenerate a preview rather than treating the saved file as a permanent
  share link.
- `ads status` is lifecycle inspection, not an edit. It returns both
  `configured_status` (what Wirekern requested) and `effective_status` (Meta's
  current interpretation), plus any `issues_info` Meta supplied. A new paused
  ad can legitimately show `configured_status: "PAUSED"` with
  `effective_status: "IN_PROCESS"` or `"PENDING_REVIEW"`; it is still unable
  to deliver because its configured state remains paused.
- `ads status --wait` makes repeated GETs every two seconds only until global
  `--deadline` (30 seconds by default). If review remains pending, the JSON
  reply is `{ "review": "pending_review", "status": { … } }`, not a silent
  wait or a delivery action. Re-run it later; do not treat review completion as
  authorization to activate an ad.
- Campaign, ad set, and ad IDs are globally addressed by Graph, so `ads
  status` deliberately has no `--ad-account` flag. The selected credential
  still must be entitled to inspect the object.

### Live validation without spend

After re-authenticating for `ads_management`, create clearly named drafts with
the commands above. In Ads Manager, verify the campaign, ad set, and ad each
show **Paused**. Then use the returned campaign ID with Tier A+:

```bash
wirekern insights meta_ads --from 2026-06-12 --until 2026-09-09 \
  --attribution 7d_click_1d_view --ad-account act_123 \
  --level campaign --entity-id <CAMPAIGN_ID> --metrics spend,purchases,purchase_value,roas --json
```

No delivery is required for this check. Until an activated campaign has both
spend and attributed purchase value, `roas` is expected to be `null`. Meta may
still require a valid payment method before it permits creation of the final
paused **ad** object. Adding a payment method is a financial-account change;
it does not itself activate a paused campaign, ad set, or ad.

## Resumable paused-draft launches (manifests)

The five commands above are the inspectable primitives; a full launch means
copying four IDs between them. A manifest composes them into one run with a
**local checkpoint file**, so an interruption resumes from the last confirmed
remote create instead of reconstructing progress by hand.

Write one reviewed JSON file (e.g. `launch.paused.json`):

```json
{
  "version": 1,
  "ad_account": "act_123456",
  "campaign": { "name": "Wirekern launch — do not activate", "objective": "awareness", "special_ad_categories": [] },
  "adset": {
    "name": "Wirekern launch ad set — do not activate",
    "daily_budget": 2500,
    "bid_strategy": "lowest_cost_without_cap",
    "billing_event": "IMPRESSIONS",
    "optimization_goal": "REACH",
    "targeting": { "geo_locations": { "countries": ["MY"] }, "age_min": 18, "age_max": 65 }
  },
  "creative": {
    "name": "Wirekern launch creative", "image_file": "./hero.png",
    "page_id": "1413299108523738", "message": "A clear benefit.", "headline": "Learn more",
    "destination_url": "https://example.com/offer", "call_to_action": "learn_more"
  },
  "ad": { "name": "Wirekern launch ad — do not activate" }
}
```

The manifest has **no** ID fields and **no** status/active/budget-update keys;
unknown keys are rejected. `daily_budget` is the minor currency unit (RM25.00
for MYR at `2500`) and is refused below the documented ~USD 1/day floor
(`daily_budget_below_minimum:100`). Only awareness-family objective →
optimization → billing pairings are accepted; anything else fails
`validate-draft` **before** any remote object exists
(`unsupported_adset_pairing:…`).

```bash
wirekern ads validate-draft meta_ads --manifest launch.paused.json --json
wirekern ads create-draft meta_ads --manifest launch.paused.json --state launch.state.json --json
wirekern ads resume-draft  meta_ads --manifest launch.paused.json --state launch.state.json --json
wirekern ads status-draft  meta_ads --state launch.state.json --wait --json
wirekern ads adopt-draft-step meta_ads --state launch.state.json --step adset --id 123 --json
```

**Execution order** is image → campaign → ad set → creative → ad: the image
is the only step that reads local disk and the most likely operator error, so
it fails before any Graph object exists. Every write follows a write-ahead
protocol: the state records `in_flight: <step>` *before* the request, and the
confirmed ID only after.

**The state file** (`--state`) is created `0600`, atomically rewritten, and
holds remote IDs plus the manifest's SHA-256 **canonical fingerprint** — no
token, secret, image bytes, or payment data. Reformatting the manifest is
harmless (whitespace and key order don't change the fingerprint); any
semantic change (budget, audience, copy) refuses resume with
`draft_manifest_changed`. A second launch needs a new state path
(`draft_state_exists`), and only one run may hold a state at a time
(`draft_busy`; delete a stale `<state>.lock` file only after confirming no
run is active).

**Ambiguous writes refuse, never retry.** If a run dies mid-write (network
or deadline after the request left), the `in_flight` marker stays and every
later `resume-draft` returns `state: "reconciliation_required"` (exit 0 —
the protocol worked; the next move is human). Reconcile in Ads Manager:

1. The clearly named paused object of that step **exists** → `adopt-draft-step --step <step> --id <its ID>` (delivery objects are remotely verified `PAUSED` first).
2. It **does not exist** → create it with the matching single-step command above, then adopt the returned ID.

Image hashes and creatives have no review edge; adopting them is a recorded
human decision, proven only when the next step uses them. There is no
`--force`: a blind retry could duplicate paused objects.

A completed state is read-only — `resume-draft` replays the result and runs
zero creates. `status-draft --wait` is GET-only and bounded by `--deadline`.
A successful run ends with every delivery object `PAUSED`; nothing activates,
nothing spends.

## Live validation record (2026-09-10)

Full no-spend validation against the dedicated **Wirekern MYR Test** account
(`act_1414222080648203`, MYR), Page `Wirekern Validation`:

- **Interruption path:** one manifest ran image → campaign → ad set, then the
  creative failed *definitively* (Page scopes missing on the token — see
  below). State parked at `adset_created` with no `in_flight`; `resume-draft`
  re-attempted only the creative. Zero duplicate writes (identical hash/IDs).
- **Full path:** fresh manifest completed all five writes in one command —
  image `20b5a184…`, campaign `120250309282020633`, ad set `120250309282200633`,
  creative `2466984410455893`, ad `120250309282900633`. All three delivery
  objects confirmed `PAUSED` configured *and* effective (`status-draft --wait`
  observed the ad settle from `PENDING_REVIEW` to `PAUSED`). Both preview
  formats rendered. Campaign-filtered insights over the window: empty rows —
  zero spend, zero impressions.
- **Found and fixed during validation:** the token's scope list lacked
  `pages_show_list,pages_manage_ads`, making `/me/accounts` empty and the
  Page-backed creative unreachable — Tier B could never have completed on a
  fresh token. Existing tokens keep their original scopes: re-run
  `auth meta_ads` after scope changes. Also fixed the `status-draft --wait`
  deadline race (expiry now emits the last observed state instead of a raw
  timeout).
- Deliberately **not** live-tested: killing a process around a live request
  (ambiguous-write adoption) — deferred until the reconciliation runbook has
  an approved manual procedure, per the plan.

## Token lifetime

The long-lived user token lasts ~60 days. `wirekern` re-issues it via `fb_exchange_token` automatically (refresh within 7 days of expiry, ≥ 24h since the last) whenever the app file exists — the exchange needs the client secret, so `apps set meta_ads …` (or env vars) must remain configured for refresh to work. Re-run `auth meta_ads` when adding `ads_management` to an older read-only token, or if the session dies.

## Errors worth knowing

| Symptom | Meaning |
|---|---|
| "This is an invalid redirect URI" (dashboard or dialog) | `localhost` used, or `--redirect-uri` ≠ the dashboard chip byte-for-byte — use the `example.com` dummy exactly |
| `no_ad_account` | token sees no ad accounts — wrong Business or none assigned |
| `token_expired` after refresh attempts | session dead; re-auth |
| `rate_limited` (exit 4) | app-level Marketing API throttling; retry later |
| `paging_exceeded` | cursor loop past 50 pages — not legitimate for ≤90-day daily ranges; report it |
| `policy_denied` | the application policy refused a spend-shaped operation; Tier B's built-in policy only permits paused creates |
| Meta code 10 / permission error | token lacks `ads_management`; re-run `wirekern auth meta_ads` and approve the expanded scope |
| Meta code 100 with a detailed message | wirekern preserves Meta's `error_user_msg` when present; correct the named Page, creative, billing, or configuration condition before retrying |
| "No payment method" | Meta requires billing before it will create the final ad, even if that ad is `PAUSED`; add a method only if you accept that financial-account change |
| `effective_status: PENDING_REVIEW` or `IN_PROCESS` | Meta is processing/reviewing a paused draft. Use `ads status … --wait` for a bounded read or retry later; it does not spend or activate anything |
