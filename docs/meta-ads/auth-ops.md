# Meta Ads auth-ops slice

Implementation notes for the first three missing checklist rows. Validated
against [008-meta-ads-connector-online-references.md](../../plans/002-references/008-meta-ads-connector-online-references.md)
and current Meta docs (2026-09-12). Graph pin stays `v26.0`.

## 1. System User token path

Meta: [Marketing API authentication](https://developers.facebook.com/docs/marketing-api/get-started/authentication)
distinguishes **user** tokens (interactive OAuth, ~60-day `fb_exchange_token`)
from **system user** tokens (Business Manager, unattended, often non-expiring).
References item 3: do not reuse a user token as a service secret.

Postkit already has `auth meta_ads --token`, but that path stores a generic
OAuth2 blob, skips `ad_account_id`, and will later call `fb_exchange_token`
as if the token were a user token.

Plan:

- Explicit CLI: `postkit auth meta_ads --token … --system-user`
- Before vault write: `GET /debug_token` with an **app** access token
  (`{app-id}|{app-secret}`). Refuse `type=USER` / `PAGE` / `APP`.
- Verify `GET /me` and resolve the first ad account (same door as OAuth).
- Vault extra: `token_kind=system_user`, `user_id`, `ad_account_id`.
- `refresh` / `refresh_is_due`: never `fb_exchange_token` this kind.
- Operator still creates the system user in Business Manager; Postkit does
  not call `POST /{business-id}/system_users`.

## 2. Token debug / inspect

Meta: [Debug Token](https://developers.facebook.com/docs/graph-api/reference/debug_token/)
(`GET /v26.0/debug_token?input_token=…`) plus
[Access Tokens: Debugging](https://developers.facebook.com/docs/facebook-login/guides/access-tokens/debugging/).
Requires an app access token or a developer user token for the **same app**.

Plan:

- `postkit ads inspect-token meta_ads`
- Library: `Client::inspect_ads_token`
- Return `type`, `is_valid`, `expires_at` (omit when 0/never),
  `data_access_expires_at`, `scopes`, `user_id`, `app_id`, `application`,
  vault `token_kind`. Never print the token or app secret.

## 3. Marketing API Access Tier

Meta renamed Ads Management Standard Access → **Marketing API Access Tier**
(Limited vs Full). [Authorization](https://developers.facebook.com/docs/marketing-api/access/):
no code changes; dashboard is source of truth. Rate-limit headers may include
`ads_api_access_tier` (`development_access` / `standard_access`).

Plan:

- Operator docs: Limited vs Full table, App Dashboard path, 500 calls / 15%
  error rate for Full. Not a bypass.
- `postkit ads access-tier meta_ads`: cheap Marketing API GET, map header
  `standard_access` → `full`, `development_access`/`limited_access` → `limited`.
  Always include the dashboard pointer. Missing header → `unknown`, still
  print how to check the dashboard.
