# meta_ads runbook

Spend/performance insights plus paused-first drafts through the Meta Marketing API (Graph `v26.0`, pinned). Scope: `ads_read,ads_management`. Tier B can create campaigns, ad sets, and ads, but every form hard-codes `status=PAUSED`. There is no activation, budget-update, or delete command; policy refuses those future spend-shaped actions by default.

## One-time setup (operator)

1. **Meta app.** Create one at [developers.facebook.com/apps](https://developers.facebook.com/apps) (or reuse your Threads app — one app can serve both sites). Add the **Marketing API** product.
2. **Redirect URI.** In the app's **Facebook Login for Business → Settings**, add `https://example.com/callback`. `localhost` is **rejected** by the dashboard ("This is an invalid redirect URI") — a dummy domain is fine for paste-code: the browser landing on a 404 is expected; you copy the address bar. Chip-field discipline: typing then Save is not enough — click the dropdown row that appears under the field so the URL becomes a pill, then Save; if the pill is gone on reload it did not save. It must match the `--redirect-uri` you pass to `apps set` **byte-for-byte**.
3. **App config into the vault:**

   ```bash
   postkit apps set meta_ads --client-id <APP_ID> --client-secret <APP_SECRET> \
     --redirect-uri 'https://example.com/callback'
   ```

   or process env: `POSTKIT_META_ADS_CLIENT_ID` / `_CLIENT_SECRET` / `_REDIRECT_URI`.

4. **Authenticate (paste-code, same flow as Threads):**

   ```bash
   postkit auth meta_ads
   # open: https://www.facebook.com/dialog/oauth?...scope=ads_read%2Cads_management...
   # approve → browser lands on https://example.com/callback?code=…&state=…#_
   # (page looks broken/404 — that is expected) paste the full address bar back
   ```

   The code is exchanged for a short token, extended via `fb_exchange_token` (~60 days), and the token's **first ad account** is resolved and stored for backwards-compatible defaults. A token with no ad account fails immediately (`no_ad_account`). When more than one account is visible, discover the IDs first and pass the desired account explicitly on each insights or paused-create call. Existing `ads_read` tokens can keep reading, but must be re-authorized to gain `ads_management` before a create succeeds.

## Dev mode is enough to start

A development-mode app can call the Marketing API for **accounts owned by the app's admins/developers/testers** — reading and paused-draft creation on your own ad account need no app review. `ads_read` / `ads_management` Advanced Access + business verification are only required when serving *other people's* accounts (i.e., when serving customers); file that review when that day comes, not before.

## Reading metrics

First, list the **remote Meta accounts** visible to this credential. This is
not the same as `postkit accounts list`, which only lists local vault aliases.

```bash
postkit ads accounts meta_ads --json
# copy an `act_<id>` value into --ad-account
```

```bash
postkit insights meta_ads --from 2026-06-01 --to 2026-06-30 \
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

Every Tier B create is approved by `PausedOnlyAdsPolicy` before postkit reads
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
a later delivery are explicit; postkit supplies no objective, targeting,
creative, or budget default.

```bash
# Returns {"entity":"campaign","id":"…","status":"PAUSED",…}
postkit ads create-campaign meta_ads --ad-account act_123 \
  --name 'Postkit validation — do not activate' --objective sales \
  --special-ad-categories '' --json

# daily_budget is in the account currency's minor unit (ILS: agorot).
postkit ads create-adset meta_ads --ad-account act_123 \
  --name 'Postkit validation ad set — do not activate' --campaign-id <CAMPAIGN_ID> \
  --daily-budget 2500 --bid-strategy lowest_cost_without_cap \
  --billing-event IMPRESSIONS --optimization-goal REACH \
  --targeting-file targeting.json --json

# A creative is an intentional external prerequisite; its content, identity,
# destination, and tracking must not be guessed by a management connector.
postkit ads create-ad meta_ads --ad-account act_123 \
  --name 'Postkit validation ad — do not activate' --adset-id <ADSET_ID> \
  --creative-id <CREATIVE_ID> --json
```

- `--status` does not exist. A request cannot opt out of `PAUSED`.
- `--ad-account` accepts `123` or canonical `act_123`; omitting it uses the
  account stored during OAuth.
- `--daily-budget` must be a positive integer in account minor units. It does
  not spend while the ad set is paused, but it is still reviewed policy input
  for a future activation workflow.
- `--bid-strategy lowest_cost_without_cap` is required. It leaves the ad set's
  explicit daily budget as its only bid limit; cost-cap, bid-cap, and ROAS
  strategies are intentionally unsupported until their constraint fields are
  modelled as explicit inputs.
- `--targeting-file` must contain a JSON object. Meta performs the final
  platform-specific targeting validation; postkit refuses malformed local
  data before any HTTP request.
- `--creative-id` is an existing Meta creative ID. Creative creation and
  activation are deliberately later capabilities.

### Live validation without spend

After re-authenticating for `ads_management`, create clearly named drafts with
the commands above. In Ads Manager, verify the campaign, ad set, and ad each
show **Paused**. Then use the returned campaign ID with Tier A+:

```bash
postkit insights meta_ads --from 2026-06-12 --to 2026-09-09 \
  --attribution 7d_click_1d_view --ad-account act_123 \
  --level campaign --entity-id <CAMPAIGN_ID> --metrics spend,purchases,purchase_value,roas --json
```

No delivery is required for this check. Until an activated campaign has both
spend and attributed purchase value, `roas` is expected to be `null`.

## Token lifetime

The long-lived user token lasts ~60 days. `postkit` re-issues it via `fb_exchange_token` automatically (refresh within 7 days of expiry, ≥ 24h since the last) whenever the app file exists — the exchange needs the client secret, so `apps set meta_ads …` (or env vars) must remain configured for refresh to work. Re-run `auth meta_ads` when adding `ads_management` to an older read-only token, or if the session dies.

## Errors worth knowing

| Symptom | Meaning |
|---|---|
| "This is an invalid redirect URI" (dashboard or dialog) | `localhost` used, or `--redirect-uri` ≠ the dashboard chip byte-for-byte — use the `example.com` dummy exactly |
| `no_ad_account` | token sees no ad accounts — wrong Business or none assigned |
| `token_expired` after refresh attempts | session dead; re-auth |
| `rate_limited` (exit 4) | app-level Marketing API throttling; retry later |
| `paging_exceeded` | cursor loop past 50 pages — not legitimate for ≤90-day daily ranges; report it |
| `policy_denied` | the application policy refused a spend-shaped operation; Tier B's built-in policy only permits paused creates |
| Meta code 10 / permission error | token lacks `ads_management`; re-run `postkit auth meta_ads` and approve the expanded scope |
