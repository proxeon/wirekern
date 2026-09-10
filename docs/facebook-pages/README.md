# facebook_pages runbook

Organic Facebook Page publishing through the Graph API. This connector is
separate from `meta_ads`: an ad account and a Page have different permissions,
tokens, and targets. A successful Page post is public according to the Page's
visibility settings as soon as Meta accepts it; it is **not** a paused ad and
does not create any advertising spend.

## One-time setup

1. In the Meta app that the intended Facebook user can test, configure the
   Facebook Login redirect URI. For Postkit's paste-code flow,
   `https://example.com/callback` is fine: the browser may show an error or a
   blank page after consent; copy its full address-bar URL back to the CLI.
   The Meta dashboard value and Postkit redirect URI must match byte-for-byte.
2. Make the authenticating user an app admin/developer/tester while the app is
   in Development mode. For customer Pages outside the app-role group, obtain
   Meta's required access/review before treating this connector as production
   access.
3. Add these permissions to the OAuth grant:

   ```text
   pages_show_list,pages_manage_posts,pages_read_engagement
   ```

4. Put the app registration in Postkit. The same Meta app may be used for
   Meta Ads, but use the Pages-specific environment names so one connector's
   configuration cannot silently shadow the other:

   ```bash
   postkit apps set facebook_pages \
     --client-id "$POSTKIT_FACEBOOK_PAGES_CLIENT_ID" \
     --client-secret "$POSTKIT_FACEBOOK_PAGES_CLIENT_SECRET" \
     --redirect-uri "$POSTKIT_FACEBOOK_PAGES_REDIRECT_URI"
   ```

   Or export those three variables directly. See [`.env.example`](../../.env.example).

5. Authenticate, open the printed Facebook URL while signed in as the person
   who can manage the intended Page, approve the request, and paste the full
   redirect URL (including `state`) back into the terminal:

   ```bash
   postkit auth facebook_pages
   ```

   This exchanges the short code and then uses `fb_exchange_token` for a
   long-lived **user** token. Re-run authentication after adding these scopes;
   changing an app configuration file never alters permissions already granted
   to an old token.

## Discover, then choose a Page

```bash
postkit pages accounts facebook_pages --json
# {"site":"facebook_pages","pages":[{"id":"123","name":"Example Page","tasks":["CREATE_CONTENT"]}]}
```

The result deliberately excludes Page access tokens. Postkit resolves the
selected Page's token just in time from Meta and holds it only for that request.
It never chooses a default/first Page: copy the ID you intend to publish to on
every command. Confirm the listed `tasks` include a content-publishing
permission before trying a post.

## Publish a clearly labelled test

```bash
postkit post facebook_pages --param page_id=123 \
  --idempotency page-test-2026-09-10 \
  --text 'Postkit test — please ignore'
```

Then inspect the Page in Facebook. An idempotency key replays a completed local
outcome on a CLI retry; it cannot prove that a connection lost after Meta
accepted a first write did not create a post, so inspect the Page before
retrying an ambiguous network/deadline failure.

For an image test, use a harmless local file:

```bash
postkit post facebook_pages --param page_id=123 \
  --idempotency page-image-test-2026-09-10 \
  --image ./postkit-test.png \
  --text 'Postkit image test — please ignore' \
  --alt 'A labelled Postkit test image'
```

The Page Photos endpoint receives the exact local bytes as multipart `source`,
with optional `caption` and `alt_text_custom`. Public `https://` image URLs are
not accepted: Postkit does not fetch arbitrary URLs or create an implicit image
hosting service. Delete these test posts manually in Facebook if they should
not remain visible.

## Scope, permission, and token failures

| Symptom | Meaning / next action |
| --- | --- |
| `page_not_available` | The selected Page was not returned for this user. Sign in as the genuine Page manager and confirm app role/Page access. |
| `page_access_token_missing` | Meta listed a Page but did not provide a usable Page token. Re-authenticate with the three scopes and confirm Page task access. |
| `token_expired` | The user token could not be used. Keep the app config available for automatic long-lived refresh, or re-run `auth facebook_pages`. |
| Meta code `10` or permission error | The user/app lacks one of the Page permissions or a required Page task. Review Meta app mode, tester invitation, scopes, and Page access. |
| `bad_page_id` / `unsupported_param:*` | `page_id` must be a numeric string and is the only v1 Page parameter. |
| `image_source_unsupported:url` | Pass a local path to `--image`; URL ingestion is intentionally out of scope. |

## Deliberate v1 boundary

Implemented: Page discovery, text posts, and one local image post. Not
implemented: post scheduling, drafts, edit/delete, comments/replies, video or
Reels, remote media URLs, cross-posting, and Page management. A future Page
feature must specify its own exact permission, wire endpoint, token boundary,
and visible side effect before it is added.
