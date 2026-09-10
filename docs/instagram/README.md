# instagram runbook

Organic Instagram image publishing through **Instagram Login**. This connector
is intentionally separate from both `facebook_pages` and `meta_ads`: it
authorizes one Instagram professional account directly, and it cannot create
an ad, spend money, or select a Facebook Page.

## One-time setup

1. In the Meta developer dashboard for the app, add the **Manage messaging &
   content on Instagram** use case. Configure the Instagram Login redirect URI
   to exactly the URI Postkit will use. For the paste-code workflow,
   `https://example.com/callback` is fine when it is registered in the same
   dashboard field. After consent, the page may be blank or show an error:
   copy the complete address-bar URL into Postkit.
2. The authorizing Instagram account must be a professional Business or
   Creator account. While the app remains in Development mode, make its owner
   an app admin, developer, or tester and accept the app-role invitation. For
   people outside those roles, obtain Meta's required access/review before
   calling the connector a production integration.
3. The authorization request is intentionally narrow:

   ```text
   instagram_business_basic,instagram_business_content_publish
   ```

4. Store this connector's app registration. Do not put a Facebook Page token
   or a Meta Ads token in these fields:

   ```bash
   postkit apps set instagram \
     --client-id "$POSTKIT_INSTAGRAM_CLIENT_ID" \
     --client-secret "$POSTKIT_INSTAGRAM_CLIENT_SECRET" \
     --redirect-uri "$POSTKIT_INSTAGRAM_REDIRECT_URI"
   ```

   The three environment variable names are in [`.env.example`](../../.env.example).
   Environment values override an `apps/instagram.json` file, so use one
   source at a time when troubleshooting.

5. Run the authorization flow while signed in to the intended Instagram
   account. Paste the full redirect URL, including `state`, back into the
   terminal:

   ```bash
   postkit auth instagram
   postkit whoami instagram --json
   ```

   The connector exchanges the short code for a long-lived Instagram token,
   then stores that token and its resolved Instagram user ID in the local
   vault. Re-authorize after changing scopes; editing app configuration never
   grants an older token new permissions.

## Publish a labelled test image

Instagram v1 requires a public HTTPS image URL; it does not make a text-only
feed post. A caption is optional and limited to 2,200 characters.

```bash
postkit --deadline 90 post instagram \
  --idempotency instagram-image-test-2026-09-10 \
  --image 'https://cdn.example.com/postkit-test.jpg' \
  --text 'Postkit Instagram test — please ignore' \
  --alt 'Ignored by Instagram connector v1'
```

Meta fetches the URL asynchronously. Postkit creates a media container, polls
its read-only `status_code` until it is `FINISHED`, then explicitly publishes
it. The global `--deadline` bounds both the fetch wait and final publish; use
`--deadline 90` for a first test so a remote image host has time to respond.
The successful `Outcome.id` is the published media ID; inspect the profile to
verify the visible post. This is an organic post, not a paused draft, so it
becomes visible if Meta accepts it. Delete the labelled test in Instagram when
it is no longer useful.

`--alt` is accepted so the generic post format remains portable, but v1 does
not send it: Postkit has not claimed an unverified Instagram Login accessibility
wire field. Local paths are refused as `image_source_unsupported:bytes`; the
kernel does not upload or host them. `--param user_id=...` is also refused:
the directly authorized account is the only v1 target.

An idempotency key only replays a confirmed local outcome. If the connection
fails after Meta accepts a write, do **not** blindly retry: inspect Instagram
first, because the remote outcome is ambiguous and a new request may make a
second visible post.

## Common failures

| Symptom | Meaning / next action |
| --- | --- |
| `missing_app_config` | Run `postkit apps set instagram …` or export all three `POSTKIT_INSTAGRAM_*` values in the process environment. |
| Meta says the app is inactive or unavailable | Confirm the app is in the intended mode and the Instagram user accepted a valid app role; app mode and user role are independent. |
| `token_expired` | Keep the app setup available for automatic long-lived refresh, or re-run `postkit auth instagram`. |
| Meta permission / professional-account error | Confirm the **Manage messaging & content on Instagram** use case, the two scopes, and that the user is a Business or Creator account. |
| `image_source_unsupported:bytes` | Supply a public `https://` image URL rather than a local path. |
| `image_url_must_be_https` | Use HTTPS with a real public host; Meta is the final authority on image format, size, and reachability. |
| `caption_empty` / `caption_too_long` | Omit `--text` for no caption, or provide 1–2,200 nonblank characters. |
| `unsupported_param:*` | V1 has no arbitrary target parameters, replies, mentions, or scheduling fields. |

## Deliberate v1 boundary

Implemented: OAuth, long-lived token refresh, identity lookup, and one
public-image feed post with optional caption. Not implemented: local image
upload/hosting, video/Reels, carousel, Stories, comments, insights,
multi-account discovery, Page selection, editing/deletion, scheduling, or an
honest `--dry-run`. Each has a different API shape and visible side effect, so
it needs a separate reviewed contract before it is added.
