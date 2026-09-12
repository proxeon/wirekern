# Meta Ads inventory and readback slice

Implementation notes for the two remaining Review/paused-draft checklist
rows. Validated against
[008-meta-ads-connector-online-references.md](../../plans/002-references/008-meta-ads-connector-online-references.md)
items 7, 10, 12–16, the [Ad Account campaigns](https://developers.facebook.com/docs/marketing-api/reference/ad-account/campaigns),
[adsets](https://developers.facebook.com/docs/marketing-api/reference/ad-account/adsets),
[ads](https://developers.facebook.com/docs/marketing-api/reference/ad-account/ads),
[adcreatives](https://developers.facebook.com/docs/marketing-api/reference/ad-account/adcreatives)
edges, object references
([campaign](https://developers.facebook.com/docs/marketing-api/reference/ad-campaign-group/),
[ad set](https://developers.facebook.com/docs/marketing-api/reference/ad-campaign/),
[ad](https://developers.facebook.com/docs/marketing-api/reference/adgroup/),
[creative](https://developers.facebook.com/docs/marketing-api/reference/ad-creative/)),
and [Graph pagination](https://developers.facebook.com/docs/graph-api/results/)
(2026-09-12). Graph pin stays `v26.0`. These are GET-only; they cannot
activate, edit budget, or change targeting.

Product rule: capped pages, stable local order, typed subset of Graph
fields. A misspelled entity or non-numeric inspect ID fails before HTTP.

## 1. List / inventory

Account-scoped GET of one kind:

| Kind | Edge | Filter | Fields |
|------|------|--------|--------|
| campaign | `GET /act_{id}/campaigns` | `effective_status` live-ish list | `id,name,configured_status,effective_status,objective` |
| adset | `GET /act_{id}/adsets` | same | `id,name,campaign_id,configured_status,effective_status` |
| ad | `GET /act_{id}/ads` | same | `id,name,adset_id,campaign_id,configured_status,effective_status` |
| creative | `GET /act_{id}/adcreatives` | none (edge has no parameters); drop `DELETED` locally | `id,name,status,object_type` |

Meta: a campaign request with no filters returns only campaigns that were
not archived or deleted. The documented example `["ACTIVE","PAUSED"]` is too
narrow for paused drafts still in `IN_PROCESS` / `WITH_ISSUES`. Postkit
sends every documented `effective_status` except `DELETED` and `ARCHIVED`.
Ad set and ad edges do not promise the campaign default, so they get the
same explicit list.

Paging: opaque `paging.next` only, `limit=25`, cap 50 pages
(`paging_exceeded`), same as insights and `ads accounts`. Sort items by `id`
after fetch so CLI/MCP order does not follow cursor arrival.

CLI: `ads list --entity campaign|adset|ad|creative [--ad-account]`.
Capability `read.ads_inventory`. MCP `ads_list`. No HTTP `serve`.

`AdEntity` stays campaign/adset/ad (creates and `ads status`). Inventory
adds `AdsInventoryKind::Creative`. `ads status --entity creative` remains
invalid.

## 2. Readback / inspect

`GET /{id}` on a globally unique object, like `ads status`. Needed before
any later activate: budget, bid, targeting, Page, destination.

| Kind | Budget / bid | Targeting | Page | Destination |
|------|--------------|-----------|------|-------------|
| campaign | `daily_budget`, `lifetime_budget`, `bid_strategy` | n/a | n/a | n/a (`objective` as context) |
| adset | `daily_budget`, `lifetime_budget`, `bid_strategy`, `bid_amount` | `targeting` | `promoted_object.page_id` | `destination_type` |
| ad | ad-level `bid_amount` is deprecated; not requested | inherited `targeting` if present | nested creative `object_story_spec.page_id` / `actor_id` | nested creative link / CTA value |
| creative | n/a | n/a | `object_story_spec.page_id`, else `actor_id` | `link_data.link`, video/template CTA or link, else `link_url` / `object_url` |

Do not deserialize Graph `targeting` into `AdTargeting`
(`deny_unknown_fields`). Extract a subset: countries, age_min/max,
publisher_platforms, facebook/instagram/whatsapp positions,
`user_age_unknown`. Placement values stay strings so an unknown Meta
position cannot fail the whole read.

CLI: `ads inspect --entity … --id …` (no `--ad-account`; IDs are globally
addressable). MCP `ads_inspect`. Still GET-only, still outside `AdsPolicy`.

## Out of scope

Activate, pause, archive, delete, duplicate, budget/bid/targeting edits,
creative swap, HTTP ads reads, MCP creates.
