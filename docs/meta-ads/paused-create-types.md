# Meta Ads paused-create types slice

Implementation notes for the six paused-create checklist rows. Validated
against [008-meta-ads-connector-online-references.md](../../plans/002-references/008-meta-ads-connector-online-references.md)
items 12–15, the [Ad Set reference](https://developers.facebook.com/docs/graph-api/reference/ad-account/adsets/),
[billing events](https://developers.facebook.com/docs/marketing-api/bidding/overview/billing-events/),
[bidding](https://developers.facebook.com/docs/marketing-api/bidding-and-optimization),
and [targeting specs](https://developers.facebook.com/docs/marketing-api/targeting-specs)
(2026-09-12). Graph pin stays `v26.0`. Status remains hard-coded `PAUSED`.

Product rule: closed types, local pairing, no Graph JSON hatch. A misspelled
`optimization_goal` must fail before HTTP.

## 1. Closed `billing_event` / `optimization_goal`

Enums with Meta SCREAMING_SNAKE wire names so existing manifests keep
`IMPRESSIONS` / `REACH`. CLI accepts `impressions` or `IMPRESSIONS`.

Billing: `impressions`, `link_clicks` (the only two still valid for auction
creates we support). Optimization: the live-tested awareness pair plus the
documented auction pairs for our six `OUTCOME_*` objectives (`reach`,
`brand_awareness`, `link_clicks`, `landing_page_views`, `offsite_conversions`,
`lead_generation`, `app_installs`, `post_engagement`, `page_likes`, `value`,
`thruplay`). Local table: goal → allowed billing. Draft table: objective →
(goal, billing). Unlisted combinations fail `unsupported_adset_pairing`.

## 2. Typed targeting

`AdTargeting` with `geo_locations.countries` (ISO 3166-1 alpha-2), optional
`age_min`/`age_max` (13–65), `publisher_platforms`, `facebook_positions`,
`instagram_positions`. Deny unknown keys. Primitive CLI: `--country`,
`--age-min`, `--age-max`, `--publisher-platform`, `--facebook-position`. Drop
`--targeting-file` on the primitive command (references item 15). Manifests
that already used `{ "geo_locations": { "countries": ["MY"] } }` still parse.

## 3. Lifetime / campaign budget / sharing

Ad set: `daily_budget` XOR `lifetime_budget` (positive minor units). Lifetime
requires `end_time`. Campaign: optional `daily_budget` / `lifetime_budget`.
`is_adset_budget_sharing_enabled` is ABO-only (v24 requires the boolean when
the campaign has no budget) and is refused with CBO. CBO: campaign has the
budget; ad set omits its own.

## 4. Bid strategies with constraints

`LOWEST_COST_WITHOUT_CAP` (no extra field). `LOWEST_COST_WITH_BID_CAP` and
`COST_CAP` require `bid_amount` > 0; cost cap also requires `IMPRESSIONS`
billing. `LOWEST_COST_WITH_MIN_ROAS` requires `optimization_goal=VALUE` and
`roas_average_floor` in `[100, 10000000]` (10000 = 1.0). Sending a cap
strategy without its constraint fails locally.

## 5. Schedule

Optional RFC3339 `start_time` / `end_time` on the ad set. `end_time` required
with `lifetime_budget`. `end` must be after `start` when both are set.

## 6. Promoted object

Closed tagged enum: `page` (`page_id`), `pixel` (`pixel_id` +
`custom_event_type`), `app` (`application_id` + `object_store_url`),
`product_set` (`product_set_id` + `custom_event_type`). Required for
`offsite_conversions`, `app_installs`, `page_likes`, `value`,
`lead_generation`. Numeric IDs only.
