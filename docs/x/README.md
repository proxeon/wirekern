# X connector: public text and explicitly approved one-to-one DMs

Postkit's X connector is deliberately smaller than the full X API v2. It can
publish public text, reply to one existing post by numeric ID, and send a
one-to-one **text** direct message. X Ads is a separate product/API surface
and is not part of this connector.

The implementation follows X's current OAuth 2.0 Authorization Code + PKCE,
[post creation](https://docs.x.com/x-api/posts/create-post), and
[DM integration](https://docs.x.com/x-api/direct-messages/manage/integrate)
documentation. X API access is billed according to X's current pay-per-use
terms; check the [pricing page](https://docs.x.com/x-api/getting-started/pricing)
before a live validation.

## 1. Configure an X developer application

1. Open the [X Developer Console](https://developer.x.com/en/portal/dashboard).
2. Create or select an App and enable OAuth 2.0.
3. Add the exact callback URL you will store in Postkit, for example:

   ```text
   https://example.com/callback
   ```

4. Copy the **Client ID** and **Client Secret**. The PKCE exchange uses the
   Client ID and verifier rather than putting the secret in the token request;
   Postkit still stores the secret in the local app record so all OAuth app
   settings have one consistent shape.

Store them in the local vault configuration (or export the matching
`POSTKIT_X_*` entries from `.env.example`):

```bash
postkit apps set x \
  --client-id '<X client ID>' \
  --client-secret '<X client secret>' \
  --redirect-uri 'https://example.com/callback'
```

## 2. Authorize public posting

```bash
postkit auth x --json
```

Open the printed URL in the browser for the intended X user. Approve the
public-post scopes, then paste the **complete** redirect URL back. PKCE keeps
its secret verifier owner-only in the local vault for ten minutes and matches
it to the redirect `state`; a raw code cannot complete X auth securely.

Confirm the identity before writing:

```bash
postkit whoami x --json
# {"site":"x","id":"<numeric user ID>","handle":"<username>"}
```

## 3. Validate a public post or reply

Use a disposable post first. A successful response means X accepted the
write; it returns an owner-independent `/i/web/status/<id>` permalink.

```bash
postkit post x \
  --text 'Postkit X connector validation — public text post.' \
  --idempotency x-validation-001 \
  --json
```

Replying is explicit and accepts only a numeric post ID:

```bash
postkit post x \
  --text 'A reply created through Postkit.' \
  --reply-to 1234567890123456789 \
  --idempotency x-reply-001 \
  --json
```

X uses weighted character counting for URLs, emoji, and CJK text. Postkit
refuses blank text locally but leaves the authoritative current weighted
length check to X rather than maintaining a drift-prone local imitation.

## 4. Opt into a one-to-one text DM

Direct messages are private, can be subject to recipient settings, and must
not inherit authorization from public posting. Re-authorize to request the
DM scopes deliberately:

```bash
postkit auth x --with-dm --json
```

Then send to a **numeric X user ID**, not a handle. Each send needs a local
idempotency key and `--allow-dm` acknowledgement:

```bash
postkit x dm \
  --to 1234567890123456789 \
  --text 'Postkit private-message validation.' \
  --idempotency x-dm-validation-001 \
  --allow-dm \
  --json
```

The outcome's ID is X's DM event ID. It intentionally has no URL: DM events
are private and Postkit will not manufacture a browser link that leaks one.
`accepted by X` is not proof of delivery/read; this release does not ingest
DM event webhooks.

## Boundaries and recovery

- The connector implements no media, video, polls, quote posts, group DMs,
  DM inbox/history, webhooks, lists, follows, or analytics.
- It implements **no X Ads API**, ad accounts, campaigns, or spend controls.
- `401` becomes a refreshable `auth: token_expired`; re-run `auth x` if X
  cannot refresh the stored credential. `429` is returned as `rate_limited`;
  wait until X's reset window before retrying.
- A completed idempotency key returns the prior local outcome without a second
  API request. A timeout after the request left the machine is ambiguous:
  inspect X before retrying with a new key.
