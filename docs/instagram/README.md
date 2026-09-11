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
The successful `Outcome.id` is the published media ID. Postkit makes one
best-effort, read-only permalink lookup after the confirmed write, so modern
results also include `Outcome.url` when Meta supplies it. A missing `url` does
not invalidate the visible post: use the returned ID or the media list below,
and Postkit will never retry `media_publish` just to obtain a link. This is an
organic post, not a paused draft, so it becomes visible if Meta accepts it.
Delete the labelled test in Instagram when it is no longer useful.

`--alt` is accepted so the generic post format remains portable, but v1 does
not send it: Postkit has not claimed an unverified Instagram Login accessibility
wire field. Local paths are refused as `image_source_unsupported:bytes`; the
kernel does not upload or host them. `--param user_id=...` is also refused:
the directly authorized account is the only v1 target.

An idempotency key only replays a confirmed local outcome. If the connection
fails after Meta accepts a write, do **not** blindly retry: inspect Instagram
first, because the remote outcome is ambiguous and a new request may make a
second visible post.

## Publish an image carousel

Repeat `--image` **2 through 10 times** to create one swipeable image
carousel. Every image must be a public HTTPS URL. The one optional `--text`
value is the post caption on the carousel parent, not a caption on each
slide:

```bash
postkit --deadline 180 post instagram \
  --idempotency instagram-carousel-test-2026-09-10 \
  --image 'https://cdn.example.com/postkit-slide-1.jpg' \
  --image 'https://cdn.example.com/postkit-slide-2.jpg' \
  --text 'Postkit Instagram carousel test — please ignore'
```

Postkit creates one invisible `is_carousel_item=true` container per image and
waits for each to become `FINISHED`. It then creates one `CAROUSEL` parent
with the ordered child IDs, waits for that parent, and sends exactly one
visible `media_publish` request. A child or parent processing failure leaves
any earlier child containers invisible and stops before publication. The
global deadline covers every child, parent, and permalink read; start with
`--deadline 180` for a first live test.

`--alt` is refused for a carousel (`carousel_alt_unsupported`) rather than
being silently copied to every slide or silently discarded. Carousel video or
mixed-media support, local file hosting, user tags, location, and per-slide
alt text are not implemented. As with a single image, inspect Instagram
before retrying an unknown write outcome; an idempotency key only replays a
confirmed Postkit outcome.

## Read recent published media

The same credential can read the first page of the directly authorized
account's recent media. It is a single GET; it never selects another account,
follows a pagination cursor, downloads images, or creates/changes a post.

```bash
postkit --json media list instagram --limit 5
```

`--limit` is 1 through 25 (default 10). The JSON reply contains `site` and a
server-ordered `media` array. Every item has `id`; `permalink`, `caption`,
`media_type`, and `timestamp` appear only when Meta supplies them. Human
output deliberately prints only copyable identity/link metadata; use `--json`
when a script needs captions. There is no pagination flag in v1, so a single
call always remains bounded.

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
| `carousel_too_few_images` / `carousel_too_many_images` | Use 2–10 `--image` values. One image is the normal image-post flow. |
| `carousel_alt_unsupported` | Omit `--alt`; v1 has no reviewed per-slide alt-text contract. |
| `carousel_caption_multiple` / `carousel_reply_unsupported` | Use at most one parent `--text` and no `reply_to_id` parameter. |
| `unsupported_param:*` | V1 has no arbitrary target parameters, replies, mentions, or scheduling fields. |

## Deliberate v1 boundary

Implemented: OAuth, long-lived token refresh, identity lookup, one
public-image feed post, a 2–10-image carousel (both with optional caption),
and a bounded first-page read of that account's published media.

The following gaps are intentional. They are not implied by a generic
`post` command: each needs its own input contract, safety review, and tests
before Postkit can claim to support it.

1. **Video and Reels.** There is no `--video` input, Reel/video container,
   resumable-upload flow, cover selection, `share_to_feed` control, or
   video-specific processing/retry contract.
2. **Mixed-media carousels.** A carousel accepts only 2–10 public HTTPS images;
   it cannot mix image and video slides.
3. **Local media upload or hosting.** Images must already be on a public HTTPS
   URL. Postkit does not upload local media to Meta or provide temporary
   hosting.
4. **Stories.** No image or video Story publishing is exposed.
5. **Post management.** Existing posts cannot be edited, deleted, archived, or
   have their comment settings changed.
6. **Comments and moderation.** There is no comment listing, reply, hide,
   delete, private-reply, or mention workflow.
7. **Insights.** Account and per-media metrics such as reach, impressions,
   engagement, saves, follower activity, and profile activity are absent.
8. **Richer media reads and pagination.** `media list` reads only the first
   page and a small metadata set; it does not expose cursors or retrieve media
   children, metric fields, or other rich metadata.
9. **Rich publishing metadata.** There are no location, people,
   collaborator, product, branded-content, music/audio, or reviewed
   accessibility/alt-text fields.
10. **Scheduling, drafts, and approvals.** A supported post publishes now;
    Postkit has no editable drafts, schedule, approval queue, or publish-job
    history.
11. **Operational controls.** There is no content-publishing-limit surface,
    webhook receiver, durable resume of interrupted processing, or job
    monitoring.
12. **Broader account management.** The connector uses the stored authorized
    Instagram account. It has no multi-account discovery, explicit account
    selection, or Facebook Page-selection workflow.

The highest-value next increment is **Reels/video publishing**. It can reuse
the existing create-container → wait-for-processing → publish architecture,
while adding a deliberately typed video source and validation contract.
