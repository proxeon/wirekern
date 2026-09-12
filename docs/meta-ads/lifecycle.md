# Meta Ads lifecycle and typed edits slice

Implementation notes for the Lifecycle and edits checklist. Validated
against
[008-meta-ads-connector-online-references.md](../../plans/002-references/008-meta-ads-connector-online-references.md)
items 12–15, 20–21,
[Manage Your Ad Object's Status](https://developers.facebook.com/docs/marketing-api/best-practices/manage-your-ad-object-status),
campaign/ad set/ad **Updating** (`POST /{id}`),
[`/{campaign_id}/copies`](https://developers.facebook.com/docs/marketing-api/reference/ad-campaign-group/)
(`status_option` default `PAUSED`), and Graph error handling (2026-09-12).
Graph pin stays `v26.0`.

Product rule: default `PausedOnlyAdsPolicy` still refuses every action that
can start or increase spend. A CLI flag is not authorization by itself —
policy, typed confirmation, and a preflight GET all sit in front of the
vault. MCP and HTTP `serve` stay off this surface (unattended spend).

## Wire (all updates)

Meta: `configured_status` / `status` enum is `ACTIVE, PAUSED, DELETED, ARCHIVED`.
`POST /{id}` with `status=<value>` is the documented mutate. Delete may also
be `HTTP DELETE`; Postkit uses `POST status=DELETED` so archive/delete share
one helper. Copies: `POST /{id}/copies` with **hard-coded**
`status_option=PAUSED` (Meta default; Postkit never sends `ACTIVE` or
`INHERITED_FROM_SOURCE`).

Creatives have no delivery `status` pair. Lifecycle kinds are `AdEntity`
(campaign / adset / ad) only.

## Policy

| Action | Default `PausedOnlyAdsPolicy` | CLI acknowledgement |
|--------|-------------------------------|---------------------|
| Pause (`ACTIVE` → `PAUSED`) | **allow** (emergency stop; cannot start spend) | none beyond numeric id |
| Activate | deny `paused_only` | `--allow-activate` + `--confirm-id` + budget echo |
| Archive | deny | `--allow-archive` + `--confirm-id` |
| Delete | deny | `--allow-delete` + `--confirm-id` + `--confirm-delete` |
| Duplicate | deny | `--allow-duplicate` + `--confirm-id` |
| UpdateBudget / Bid / Schedule / Placement / Targeting / SwapCreative | deny | matching `--allow-*` + `--confirm-id` |

`AllowAdsActionPolicy` permits **one** extra `AdsAction` on top of the
paused-only set. `--allow-activate` cannot unlock delete.

Capability: `manage.ads_lifecycle`. Distinct from `create.paused_ads`.

## 1–4. Activate + refuse + write-ahead

`POST /{id}` `status=ACTIVE` only after:

1. Request validate: numeric id, `confirm_id == id`, budget xor matches
   inspect when the object has a daily/lifetime budget.
2. Policy `Activate` (denied by default).
3. GET review: `configured_status` must be `PAUSED`;
   `PENDING_REVIEW` / `IN_PROCESS` refuse; any `issues_info` refuse.
4. GET inspect: `--confirm-daily-budget` / `--confirm-lifetime-budget`
   must equal Graph's current minor units when present. Ads with no own
   budget skip the echo.
5. Write-ahead `--state` file is **required**. `in_flight=true` before POST.
   A leftover marker is `reconciliation_required` (exit 0) and **no retry**.
6. POST. Network/deadline after the POST left → `reconciliation_required`,
   never a second POST. Graph `Platform` errors mean the write did not
   apply. `ARCHIVED` stays refused (`not_paused:ARCHIVED`); Postkit does
   not restore archived objects to ACTIVE.

CLI:

```text
postkit ads activate meta_ads --entity campaign|adset|ad --id <ID> \
  --confirm-id <ID> --allow-activate \
  [--confirm-daily-budget N | --confirm-lifetime-budget N] [--state path]
```

## 5. Emergency pause

`POST /{id}` `status=PAUSED`. Allowed by default. Already-paused is
idempotent success. `ARCHIVED` / `DELETED` refuse. No budget confirmation.

```text
postkit ads pause meta_ads --entity campaign|adset|ad --id <ID>
```

## 6. Archive

`POST status=ARCHIVED`. Meta: archived objects are queryable by id; only
`name` and `status→DELETED` remain writable. Default policy deny.

## 7. Delete

`POST status=DELETED`. Destructive and irreversible to live. Default deny.
Requires `--confirm-delete` (literal) plus `--confirm-id`.

## 8. Duplicate

`POST /{id}/copies` with `status_option=PAUSED` hard-coded. Deep copy is
off (`deep_copy` default false). Returned `copied_*_id` is a paused draft,
not a delivery claim. When the source has a daily or lifetime budget, the
operator must echo it (`--confirm-daily-budget` / `--confirm-lifetime-budget`)
because the copy still inherits spend shape.

## 9. Daily-budget edit

`POST daily_budget=<new>` after GET current. Confirmation:
`--confirm-id`, `--current-daily-budget` must match Graph, `--new-daily-budget`
positive, and `|new-current| / current <= max_change_ratio` (default 0.2;
`--max-change-ratio` explicit). Currency is the account's; values are
minor units (same as paused create). Lifetime budget is out of this row.

## 10. Bid-strategy edit

`POST bid_strategy` plus `bid_amount` or `bid_constraints` using the same
local pairing as paused create. GET current strategy first.

## 11. Schedule edit

Ad set only. `POST start_time` / `end_time` through `validate_adset_schedule`.
Campaign start/stop are read-only on Meta.

## 12. Placement edit

GET raw `targeting` as JSON (not `AdTargeting` — `deny_unknown_fields`).
Merge only `publisher_platforms` / `facebook_positions` / `instagram_positions`
/ `whatsapp_positions`. POST the merged object so unknown Graph keys survive.

## 13. Targeting edit

Same merge GET/POST. Before/after diff of the typed subset (countries, ages,
placements). Refuse when the parent campaign `special_ad_categories` is
non-empty (not `NONE`): Postkit will not rewrite a Special Ad Category
contract.

## 14. Creative swap

`POST /{ad_id}` `creative={"creative_id":"<numeric>"}`. Ad entity only.

## Out of scope

MCP/HTTP lifecycle, automated rules, untyped Graph `status` strings,
`status_option=ACTIVE` copies, lifetime-budget edit (own row later),
billing/payment.
