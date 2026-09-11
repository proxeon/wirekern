# LinkedIn member text posts

This is the narrow LinkedIn connector: authenticate one LinkedIn member and
publish one public, organic text post as that same member. It uses LinkedIn's
current versioned **Posts API**; it does not use the legacy UGC Posts API.

It does not post to an organization/Page, upload media, list or edit posts,
read analytics, send comments/reactions, create sponsored content, or access
LinkedIn advertising. A successful command creates a visible public organic
post, but cannot create an ad or spend money.

## 1. Configure your LinkedIn application

1. Open [LinkedIn Developer Portal](https://www.linkedin.com/developers/apps)
   and create or select your application.
2. Under **Products**, add both:
   - **Share on LinkedIn** — provides `w_member_social`, the member post
     permission.
   - **Sign in with LinkedIn using OpenID Connect** — provides `openid` and
     `profile`, used only to identify the authenticated member safely.
3. Under **Auth**, add this exact redirect URL (or choose your own and use the
   exact same value in Postkit):

   ```text
   https://example.com/callback
   ```

4. Copy the application's **Client ID** and **Client Secret**. Do not use a
   personal LinkedIn password as an API credential.

LinkedIn requires the redirect URI to match exactly. A missing product often
appears as an OAuth scope or permission error; enable both products before
authorizing again.

## 2. Store app configuration locally

```bash
postkit apps set linkedin \
  --client-id '<LinkedIn Client ID>' \
  --client-secret '<LinkedIn Client Secret>' \
  --redirect-uri 'https://example.com/callback'
```

Or keep these only in your local, Git-ignored `.env` and export them before
running Postkit:

```bash
POSTKIT_LINKEDIN_CLIENT_ID='<LinkedIn Client ID>'
POSTKIT_LINKEDIN_CLIENT_SECRET='<LinkedIn Client Secret>'
POSTKIT_LINKEDIN_REDIRECT_URI='https://example.com/callback'
```

Environment values take precedence over `~/.postkit/apps/linkedin.json`.
Check the non-secret result with:

```bash
postkit apps show linkedin --json
```

## 3. Authorize the member

```bash
postkit auth linkedin --json
```

Open the printed URL in the browser where the intended LinkedIn member is
signed in. Approve the request, then paste the **complete redirected URL**
back into Postkit. Keeping the URL intact lets Postkit verify OAuth `state`;
do not paste the authorization code into a chat, issue tracker, or shell
history.

Postkit exchanges the code, calls LinkedIn OIDC UserInfo, and stores the
access token plus the resolved opaque member ID in the local 0600 vault. It
uses UserInfo rather than the legacy `/v2/me` profile endpoint.

Validate the resulting credential before posting:

```bash
postkit whoami linkedin --json
# {"site":"linkedin","id":"<opaque member id>","handle":"<optional name>"}
```

## 4. Publish a disposable text post

```bash
postkit post linkedin \
  --text 'Postkit LinkedIn connector validation — organic text post.' \
  --idempotency linkedin-validation-001 \
  --json
```

Expected success is an exact LinkedIn post URN, for example:

```json
{
  "site":"linkedin",
  "id":"urn:li:share:1234567890123456789",
  "url":"https://www.linkedin.com/feed/update/urn:li:share:1234567890123456789/"
}
```

LinkedIn returns the post URN in `x-restli-id`. For its known `share` and
`ugcPost` URN forms, Postkit returns the corresponding feed URL as an opening
convenience. LinkedIn may require a signed-in viewer to open it; confirm the
result in the authenticated member's activity feed.

The supplied text must be nonblank and at most 3,000 Unicode characters.
`--param` is deliberately unsupported: the stored OAuth member is the only
v1 author, visibility is always `PUBLIC`, and distribution is always the main
feed. This prevents a normal post command from silently becoming an
organization, targeted, dark, or sponsored post.

## Token lifecycle and failure handling

- If LinkedIn supplied a refresh token, Postkit refreshes it when its recorded
  access-token expiry is near. If no refresh token was issued or retained,
  Postkit leaves the current token alone and asks you to re-run `auth linkedin`
  only when LinkedIn rejects it.
- `401` becomes `auth: token_expired`; re-authorize with `postkit auth
  linkedin`.
- `429` is surfaced as `rate_limited`; retry later.
- Permission failures keep LinkedIn's structured non-secret message. Verify
  that both listed products are enabled and authorize again after any scope
  change.
- A completed idempotency key replays its saved `Outcome` without another API
  call. A timeout or network failure after the request was sent is ambiguous:
  inspect LinkedIn before retrying with a **new** key, because the remote post
  may already exist.

## Deliberately not implemented

Organization access/posting, Page role discovery, image/video/document upload,
multi-image posts, articles, polls, carousels, reshares, comments, reactions,
post reads/listing, analytics, edit/delete, targeting, dark posts, sponsored
content, advertising account access, scheduling, and a public callback server
are separate features. They must not be supplied through arbitrary JSON or
free-form author IDs.

## Primary references

- [Getting access to LinkedIn APIs](https://learn.microsoft.com/en-us/linkedin/shared/authentication/getting-access)
- [LinkedIn OAuth 2.0 authorization-code flow](https://learn.microsoft.com/en-us/linkedin/shared/authentication/authorization-code-flow)
- [Sign in with LinkedIn using OpenID Connect](https://learn.microsoft.com/en-us/linkedin/consumer/integrations/self-serve/sign-in-with-linkedin-v2)
- [Posts API](https://learn.microsoft.com/en-us/linkedin/marketing/community-management/shares/posts-api)
- [Post schema](https://learn.microsoft.com/en-us/linkedin/marketing/community-management/shares/post-api-schema)
- [Marketing API versioning](https://learn.microsoft.com/en-us/linkedin/marketing/versioning)
