# meta_ads runbook

Read-only spend/performance insights from the Meta Marketing API (Graph `v26.0`, pinned). Scope: `ads_read`. No verb in this connector can spend — management (paused-first) and activation (policy-gated) are Tier B/C per [design/026](../../design/026-meta-ads-connector.md) and deliberately absent.

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
   # open: https://www.facebook.com/dialog/oauth?...scope=ads_read...
   # approve → browser lands on https://example.com/callback?code=…&state=…#_
   # (page looks broken/404 — that is expected) paste the full address bar back
   ```

   The code is exchanged for a short token, extended via `fb_exchange_token` (~60 days), and the token's **first ad account** is resolved and stored for backwards-compatible defaults. A token with no ad account fails immediately (`no_ad_account`). When more than one account is visible, discover the IDs first and pass the desired account explicitly on each insights call.

## Dev mode is enough to start

A development-mode app can call the Marketing API for **accounts owned by the app's admins/developers/testers** — reading your own ad account's insights needs no app review. `ads_read` Advanced Access + business verification are only required when reading *other people's* accounts (i.e., when serving customers); file that review when that day comes, not before.

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

## Token lifetime

The long-lived user token lasts ~60 days. `postkit` re-issues it via `fb_exchange_token` automatically (refresh within 7 days of expiry, ≥ 24h since the last) whenever the app file exists — the exchange needs the client secret, so `apps set meta_ads …` (or env vars) must remain configured for refresh to work. If the session dies anyway, re-run `auth meta_ads`.

## Errors worth knowing

| Symptom | Meaning |
|---|---|
| "This is an invalid redirect URI" (dashboard or dialog) | `localhost` used, or `--redirect-uri` ≠ the dashboard chip byte-for-byte — use the `example.com` dummy exactly |
| `no_ad_account` | token sees no ad accounts — wrong Business or none assigned |
| `token_expired` after refresh attempts | session dead; re-auth |
| `rate_limited` (exit 4) | app-level Marketing API throttling; retry later |
| `paging_exceeded` | cursor loop past 50 pages — not legitimate for ≤90-day daily ranges; report it |
