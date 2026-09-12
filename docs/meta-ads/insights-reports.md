# Meta Ads insights-reports slice

Implementation notes for the four Insights checklist rows. Validated against
[008-meta-ads-connector-online-references.md](../../plans/002-references/008-meta-ads-connector-online-references.md)
items 8–10, [Insights API](https://developers.facebook.com/docs/marketing-api/insights/),
[Breakdowns](https://developers.facebook.com/docs/marketing-api/insights/breakdowns/),
and [async jobs](https://developers.facebook.com/docs/marketing-api/insights/best-practices/)
(2026-09-12). Graph pin stays `v26.0`.

Product rule: typed fields only. No generic Graph `fields=` hatch. Range still
≤ 90 days. Cursor paging still capped. A number that is not an invoice is
labelled so it cannot be read as a billing total.

## 1. Additional typed metrics

Keep the existing ten. Add a closed set with a one-line definition on each
variant (and CLI `--help`):

| Postkit | Graph field | Definition |
|---------|-------------|------------|
| `frequency` | `frequency` | Estimated average impressions per person reached. Not spend. |
| `unique_clicks` | `unique_clicks` | Estimated unique people who clicked. Not a billing event. |
| `inline_link_clicks` | `inline_link_clicks` | Clicks on the ad's destination link. |
| `inline_link_click_ctr` | `inline_link_click_ctr` | Destination-link clicks / impressions. |
| `quality_ranking` | `quality_ranking` | Meta delivery diagnostic (`ABOVE_AVERAGE` / …). Not a cost. |
| `video_thruplay` | `video_thruplay_watched_actions` | Count of ThruPlays from Meta's action array. Not spend. |

Do **not** add billing-account fields (`balance`, `amount_spent` on Ad Account).
`spend` remains the only windowed ad-delivery spend metric and is still
account currency, not an invoice.

## 2. Additional breakdowns / attribution

Attribution: add atomic Graph windows `7d_click`, `28d_click`, `7d_view`,
`28d_view` next to the existing Ads Manager presets. Combined
`7d_click_1d_view` still expands to `["7d_click","1d_view"]` (code 100 if
sent as a single string).

Breakdowns: add `gender`, `device_platform`, `platform_position`. Cap at two
breakdowns. Allow documented pairs (`age+gender`,
`publisher_platform+platform_position`) plus the already-shipped singles and
the existing `country+publisher_platform` / `country+age` tests. Skip hourly
breakdowns (some accounts require async after 2026-08-06).

## 3. Async / bulk Insights jobs

Meta: `POST /act_{id}/insights` → `{report_run_id}`; poll
`GET /{id}?fields=async_status,async_percent_completion`; results
`GET /{id}/insights`. Jobs expire in 30 days — do not persist them in the
vault. v25+ failed jobs return `error_code` / `error_user_msg` on the run.

Bounds: same 90-day range, same 50-page cursor cap, poll until `--deadline`,
hard row cap (5000). Cancel is `DELETE /{report_run_id}` when Meta accepts it.
A deadline that fires while the job is running returns an explicit pending
document (like `ads status --wait`), not a silent timeout.

## 4. Creative / delivery report contracts

`InsightsQuery.report`: `performance` (default, current mixed set) |
`delivery` (impressions, reach, frequency, spend, cpm, quality_ranking) |
`creative` (clicks, ctr, unique_clicks, inline_link_clicks,
inline_link_click_ctr, video_thruplay). Metrics outside the kind fail
`invalid_query` locally. Still one Insights edge, never a raw Graph query.
