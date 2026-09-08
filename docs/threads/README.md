# Threads: zero to first post

**Done when:** `postkit post threads --text '…' --json` prints an `id` and `url`, and the post is visible on the tester profile.

This is the operator path we used. Meta’s dashboard is the slow part. postkit does not open a local callback server (`--listen` is a stub). Default is paste-code OAuth.

Do not put App secrets or tokens in this folder. They live in repo-root `.env` (gitignored) and `~/.postkit` (0700/0600).

---

## Checklist

1. Meta app with use case **Access the Threads API**
2. Permissions `threads_content_publish` and `threads_manage_replies` (reply chains)
3. Settings URLs saved as **chips** (redirect + uninstall + delete)
4. **Threads Tester** added (dropdown chip) and **invite accepted** on Threads
5. Threads App ID / secret stored (`.env` then `apps set`)
6. `auth threads` → browser → paste redirect URL / `--code`
7. `whoami` then `post`

Skip 6 if you already have a long-lived token (`THQVJ…`) and use `--token` instead.

---

## 0. CLI

From the repo:

```bash
cd /path/to/postkit
alias pk='cargo run -q -p postkit-cli --'
```

Or `cargo install postkit-cli` and use `postkit` instead of `pk`.

Vault and app files default to `~/.postkit`. Override with `--home` or `POSTKIT_HOME`.

---

## 1. Create the Meta app

1. [developers.facebook.com/apps](https://developers.facebook.com/apps/) → **Create App**.
2. Use case: **Access the Threads API**. Not a generic Facebook Login app.
3. Finish creation. You now have **two** ID/secret pairs:

| Pair | Where | Use with postkit? |
|------|--------|-------------------|
| Facebook App ID / secret | App settings → Basic | No |
| **Threads App ID / secret** | Use Cases → Access the Threads API → **Settings** | **Yes** (`client_id` / `client_secret`) |

`client_id` on `https://threads.net/oauth/authorize` is the **Threads** App ID.

---

## 2. Permission

Use Cases → Access the Threads API → Customize.

- `threads_basic` is required and already on.
- Add **`threads_content_publish`**. Without it you can auth and still fail to post.
- Add **`threads_manage_replies`** for reply chains. Re-auth after adding it.
- Add **`threads_manage_replies`** for reply chains (`reply_to_id`). Graph 500s on replies without it on this app.

---

## 3. Settings URLs

Same Settings page as the Threads App ID.

| Field | Dummy that saves | Notes |
|-------|------------------|--------|
| Redirect Callback URLs | `https://example.com/callback` | **Chip field.** Typing then Save is not enough. |
| Uninstall Callback URL | `https://example.com/deauth` | Plain https URL. Not an email. |
| Delete Callback URL | `https://example.com/delete` | Plain https URL. Not an email. |

`localhost` is often rejected. Meta’s sample maps a fake host. Dummy `https://example.com/…` is enough for paste-code: the browser will 404; you copy the address bar.

### Commit the redirect as a chip

1. Clear leftover chips and text.
2. Paste `https://example.com/callback`. **Do not Save yet.**
3. A dropdown appears under the field. **Click that row.**
4. The URL becomes a **pill with an ×**.
5. Fill Uninstall + Delete, then Save.

If you reload and the pill is gone, it did not save.

Exact error **Redirect URIs: Please specify an OAuth redirect URI** means the field was still empty text, not a chip.

Copy the chip **byte-for-byte** (trailing slash if Meta added one). That string is `--redirect-uri` / `POSTKIT_THREADS_REDIRECT_URI`.

---

## 4. Threads Tester

Development-mode OAuth fails with “user has not accepted the invite” until this is done.

### Add

1. App roles → Roles → **Add People**.
2. Uncheck Administrator / Developer / Tester / Analytics.
3. Check **only Threads Tester**.
4. Type the **Threads username** with no `@` (same as Instagram).
5. Tweak spelling until a **dropdown with avatar** appears. Click it so it becomes a chip.
6. Add. Status: **Pending**.

Submitting the raw string (no chip) yields **does not resolve to a valid user ID**. Facebook Tester / Administrator looks up Facebook users, not Threads handles.

Professional / Creator / Business Instagram accounts often never appear, or appear then Save fails with a generic **Form can't be saved**. Switch that Instagram account to **Personal**, or add a personal alt.

### Accept

On the tester’s Threads account ([settings](https://www.threads.net/settings/account)):

**Website permissions → Invites → Accept.**

Reload App roles. Pending should clear.

---

## 5. Store credentials

Repo-root `.env` (gitignored). Copy from [`.env.example`](../../.env.example). Names the CLI already reads if they are in the **process** environment:

```bash
POSTKIT_THREADS_CLIENT_ID=
POSTKIT_THREADS_CLIENT_SECRET=
POSTKIT_THREADS_REDIRECT_URI=https://example.com/callback
```

The CLI does not auto-load `.env`. Source it, then write `~/.postkit/apps/threads.json` so later commands work without the env:

```bash
set -a && . ./.env && set +a
pk apps set threads \
  --client-id "$POSTKIT_THREADS_CLIENT_ID" \
  --client-secret "$POSTKIT_THREADS_CLIENT_SECRET" \
  --redirect-uri "$POSTKIT_THREADS_REDIRECT_URI"
pk apps show threads --json
```

`apps show` must print the same `redirect_uri` as the dashboard chip.

---

## 6. Paste-code OAuth

On a TTY:

```bash
pk auth threads --json
```

Stderr prints `open: https://threads.net/oauth/authorize?…`. Open it while logged into Threads as the tester. Allow the app.

The redirect lands on `https://example.com/callback?code=AQBx-…&state=…#_` and looks broken. That is expected. Copy the **full address bar**.

- TTY: paste that line into the waiting CLI, Enter.
- Non-TTY (agents): the first `auth` exits after printing `then: postkit auth threads --code <code>`. Finish with:

```bash
pk auth threads --code 'https://example.com/callback?code=AQBx-…#_' --json
```

`--code` accepts the raw code or the whole URL. `#_` is stripped. Codes are one-shot, ~1 hour.

`state` is 128 random bits and the pasted redirect URL must echo it: mismatched or missing `state` is rejected (`state_mismatch` / `missing_state`). The two-invocation `--code` path cannot verify `state`; paste the redirected URL unedited.

Success looks like:

```json
{"site":"threads","id":"…","handle":"yourhandle"}
```

Vault: `~/.postkit/accounts/threads/default.json` — **long-lived** token only (`kind: oauth2`, `extra.expires_at` ~60 days, no short 1h token).

### `--token` shortcut

If Settings → **User Token Generator** (or Graph) already gave a long-lived `THQVJ…`:

```bash
pk auth threads --token 'THQVJ…' --json
```

`--token` does not need `apps set`. Paste-code **does**.

`--token` and `--code` are exclusive. `--listen` is not implemented.

---

## 7. Prove it

```bash
pk whoami threads --json
pk post threads --text 'postkit live' --json
```

`whoami` repeats site / id / handle. `post` prints `id` and a `https://www.threads.com/@…/post/…` url. Confirm on the profile.

Later posts reuse the vault. No browser:

```bash
pk post threads --text 'hello' --json
```

Text limit: **500 UTF-8 bytes** per post. Empty text is rejected.

Reply chain (not a carousel). Repeat `--text`; each line is one Graph post. Segment 2+ send `reply_to_id` of the previous id (create container, then `threads_publish` — not `auto_publish_text`). Not atomic: if a later segment fails, earlier posts stay live (delete in the Threads app). `--to` + two `--text` is refused (`thread_unsupported`). Use `--deadline 60` if a reply is slow to publish.

```bash
pk post threads --json --text 'root' --text '1/' --text '2/'
# { "results": [ { "id": "A", … }, { "id": "B", … }, { "id": "C", … } ] }
```

One reply to an existing post:

```bash
pk post threads --text 'reply' --param reply_to_id=A --json
```

Client refreshes the long-lived token when `expires_at` is within 7 days **and** last refresh is ≥24h. If 60 days pass with no refresh, run `auth` again.

---

## Failures we actually hit

| Symptom | Cause | Fix |
|---------|--------|-----|
| Redirect URIs: Please specify an OAuth redirect URI | Redirect typed, not a chip | Click the dropdown under the field, then Save |
| Delete/Uninstall rejected | Email in those fields | https URLs, even dummies |
| `"handle" does not resolve to a valid user ID` | Threads Tester submitted as plain text, or Facebook role selected | Threads Tester only; click avatar chip |
| Form can't be saved (generic) on Add People | No chip, or Professional IG account | Personal account; wait for dropdown |
| invalid redirect / not whitelisted | `--redirect-uri` ≠ dashboard chip, or `localhost` | Exact string; `example.com` dummy is fine for paste |
| user has not accepted the invite (1349245) | Tester pending | Accept under Threads Website permissions → Invites |
| `missing_app_config` | No `apps set` and no `POSTKIT_THREADS_*` in the process env | Step 5 |
| Authorize URL then hang in a non-TTY | CLI will not read stdin | Use `--code` with the copied URL |

---

## What postkit calls (for debugging)

Authorize (unversioned): `https://threads.net/oauth/authorize`  
Code → short: `POST https://graph.threads.net/oauth/access_token`  
Short → long: `GET https://graph.threads.net/access_token?grant_type=th_exchange_token`  
Refresh: `GET https://graph.threads.net/refresh_access_token?grant_type=th_refresh_token`  
Publish: `POST https://graph.threads.net/v1.0/me/threads` form `media_type=TEXT&auto_publish_text=true` then `GET` permalink.

Scopes: `threads_basic,threads_content_publish,threads_manage_replies`. Host is **`graph.threads.net`**.
