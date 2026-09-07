# Bluesky: zero to first post

**Done when:** `postkit post bluesky --account you.bsky.social --text '…' --json` prints an `id` (`at://…`) and `url`, and the post is visible on the profile.

This is the operator path we used. There is **no Meta dashboard**, no OAuth redirect, no `apps set`. Auth is an **app password** (not the login password). Default PDS is `https://bsky.social`.

Do not put app passwords in this folder. They live in repo-root `.env` (gitignored) and `~/.postkit` (0700/0600).

---

## Checklist

1. Bluesky account with a handle (`you.bsky.social` or a custom domain)
2. **App password** from Settings (not the account password)
3. Handle + password in `.env`, then `auth bluesky --account … --password …`
4. `whoami` then `post` with the **same** `--account`

No Threads-style tester invite. No `POSTKIT_THREADS_*`. `Client` does not need `~/.postkit/apps/bluesky.json`.

---

## 0. CLI

From the repo:

```bash
cd /path/to/postkit
alias pk='cargo run -q -p postkit-cli --'
```

Or `cargo install postkit-cli` and use `postkit` instead of `pk`.

`--account` is global and **required** for Bluesky. The CLI default is `default`; that identifier is rejected (`identifier_required`). Use the handle as the account name. That is also the vault filename.

```bash
pk --account you.bsky.social whoami bluesky --json
pk --account you.bsky.social post bluesky --text 'hi' --json
```

`--account` may sit before or after the subcommand. Vault root: `~/.postkit` (`--home` / `POSTKIT_HOME`).

---

## 1. Handle

1. Create or open an account at [bsky.app](https://bsky.app).
2. Settings should show **@you.bsky.social** (or `you.com` if you set a custom domain).
3. That string is `--account` and `POSTKIT_BSKY_HANDLE`. No `@`.

The handle need not match Threads. We used a Bluesky handle that differed from the Threads username.

Confirm the profile exists: `https://bsky.app/profile/you.bsky.social`.

---

## 2. App password

On [bsky.app](https://bsky.app): **Settings → Privacy and security → App passwords → Add**.

- Copy `xxxx-xxxx-xxxx-xxxx` (dashes optional; 16 characters without them also worked).
- **Not** the password you type to log into bsky.app.
- Revoke in the same screen if it leaks.

postkit proves the password with `createSession`, then stores identifier + secret. It does **not** vault `accessJwt` / `refreshJwt`. Each `whoami` / `post` mints a new session.

---

## 3. Store credentials

Repo-root `.env` (gitignored). Copy from [`.env.example`](../../.env.example):

```bash
POSTKIT_BSKY_HANDLE=you.bsky.social
POSTKIT_BSKY_APP_PASSWORD=
```

The CLI does not auto-load `.env`. Source it, then auth:

```bash
set -a && . ./.env && set +a
pk auth bluesky \
  --account "$POSTKIT_BSKY_HANDLE" \
  --password "$POSTKIT_BSKY_APP_PASSWORD" \
  --json
```

`--password` with no value prompts on a TTY. Non-TTY: pass the value or the CLI prints `then: postkit auth bluesky --account <handle> --password <app-password>` and exits.

`--password` cannot combine with `--token` or `--code`. Bluesky does not use those.

---

## 4. Prove it

```bash
pk --account you.bsky.social whoami bluesky --json
pk --account you.bsky.social post bluesky --text 'postkit live' --json
```

`whoami` prints `did:plc:…` and the handle. `post` prints:

```json
{
  "site": "bluesky",
  "id": "at://did:plc:…/app.bsky.feed.post/…",
  "url": "https://bsky.app/profile/you.bsky.social/post/…"
}
```

Text limit: **300 graphemes** (not UTF-8 bytes; Threads is 500 bytes). Empty text is rejected.

Later posts reuse the vault. No browser:

```bash
pk --account you.bsky.social post bluesky --text 'hello' --json
```

Vault: `~/.postkit/accounts/bluesky/you.bsky.social.json` — `{ kind: app_password, identifier, secret, pds }`. No JWT.

Custom PDS: the vault may store `pds`. Default is `https://bsky.social`. postkit has no CLI flag for PDS yet; omit unless you patch creds.

---

## 5. Dual path (optional)

Same `Client`, two HTTP stacks:

```bash
pk --account you.bsky.social post --to threads,bluesky --text 'hi' --json
```

`--account` applies to **both** sites. Threads vault is usually `default`; Bluesky is the handle. Fan-out with different account names is not one flag — run two `post` commands, or put both accounts under the same alias (not how we dogfooded).

For Threads+Bluesky as we ran them:

```bash
pk post threads --text 'hi' --json
pk --account you.bsky.social post bluesky --text 'hi' --json
```

---

## Failures we actually hit

| Symptom | Cause | Fix |
|---------|--------|-----|
| `missing_app_config` | Old `Client::auth_finish` required `apps/*.json` | Fixed: empty app fallback. Upgrade past `35b0bac`. No `apps set bluesky`. |
| `identifier_required` | `--account` omitted (defaults to `default`) | `--account you.bsky.social` |
| `use_password` | `--token` / `--code` on Bluesky | `--password` only |
| `bad_password` / 401 | Login password, revoked app password, typo | New app password from Settings |
| `text_too_long` | >300 graphemes | Shorter copy (Threads 500 **bytes** is a different limit) |
| `unknown_account` | Posted without `--account` after auth under the handle | Same `--account` as `auth` |
| Profile 404 on bsky.app | Handle does not exist yet | Create the account first |

---

## What postkit calls (for debugging)

Default PDS: `https://bsky.social` (no `/v1.0`).

Session (auth, and again on every whoami/publish):

```http
POST {pds}/xrpc/com.atproto.server.createSession
Content-Type: application/json

{ "identifier": "<handle or did>", "password": "<app password>" }
```

Publish:

```http
POST {pds}/xrpc/com.atproto.repo.createRecord
Authorization: Bearer <accessJwt>
```

Lexicon: `app.bsky.feed.post`. Record id in the outcome is an AT URI.

No OAuth. No redirect URI. No chip.
