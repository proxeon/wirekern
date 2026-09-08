# CLI reference

Global flags work on every subcommand. `--json` prints the machine document on **stdout** and human text on stderr — agents always pass it. Without it, documents are suppressed and only the human lines print.

## Global flags

| Flag | Default | |
|------|---------|--|
| `--json` | off | Document on **stdout**; human text on stderr. |
| `--home <dir>` | `~/.postkit` | Vault root. Also `POSTKIT_HOME`. |
| `--deadline <secs>` | `30` | Publish only. |
| `--account <name>` | `default` | Vault alias. Bluesky: the handle. Global — may sit before the subcommand: `postkit --account you.bsky.social post bluesky --text hi --json`. |
| `--version` / `-V` | | Same string as `User-Agent`. |

## `post`

```text
postkit post <site> --text <str> [--param k=v]... [--idempotency <key>]
postkit post threads --text 'root' --text 'reply'
postkit post --to threads,bluesky --text <str>
postkit post --stdin
```

| Flag | |
|------|--|
| `<site>` or `--to a,b` | One site, or same text/`--param` on each. Mixed success → `{ "results": [ … ] }` |
| `--text` | `Body::Text`. Repeatable on **threads** = reply chain (`reply_to_id`). One `--text` is a single `Outcome`; two or more is `{ "results": [ … ] }`. Other sites: two `--text` → `thread_unsupported` before HTTP. |
| `--param k=v` | `Intent.params` (repeatable). `--param reply_to_id=` = one reply to an existing post |
| `--idempotency` | Root segment only on a chain. Honored only if the platform does |
| `--stdin` | Raw request JSON. One body. Exclusive with `--text` |

Details that bite:

- No `--token` on `post`. Auth writes the vault; `post` reads it.
- `--to` uses **one** `--account` for every site. Threads is usually `default`; Bluesky is the handle — two commands, or the same alias in both vaults.
- Reply chains are not atomic: if a later segment fails, earlier posts stay live (delete them in the app). Use `--deadline 60` if a reply is slow to publish.

## `auth`

```text
postkit auth threads                         # TTY: print URL, paste redirect or code
postkit auth threads --code 'AQBx-…'         # raw code or full callback URL (#_ stripped)
postkit auth threads --token 'THQVJ…'        # bootstrap; no app file
postkit auth bluesky --account you.bsky.social --password 'xxxx-xxxx-xxxx-xxxx'
```

- `--token` and `--code` are exclusive. `--password` cannot mix with either. `--listen` is a stub (paste-code is the path).
- `--token` is an OAuth-site bootstrap (Threads). App-password sites (Bluesky) refuse it with `token_bootstrap_unsupported`.
- Threads with no flags: `auth_start`, `open: …` on stderr, waits for paste. Non-TTY prints `then: postkit auth threads --code <code>` and exits. `--json` prints `WhoAmI` only (no token).
- Threads paste-code needs `apps set` first (or `POSTKIT_THREADS_CLIENT_ID` / `_CLIENT_SECRET` / `_REDIRECT_URI` in the **process** env). Redirect URI must match the Meta dashboard chip **byte-for-byte**.
- The pasted redirect URL must echo the `state` the CLI generated: mismatched or missing `state` is rejected (`state_mismatch` / `missing_state`). The two-invocation `--code` path cannot verify `state` — paste the redirected URL unedited.
- Bluesky does not need an app file.

## `whoami` / `capabilities`

```text
postkit whoami threads --json
postkit whoami bluesky --account you.bsky.social --json
postkit capabilities --json
# {"bluesky":["publish.text"],"threads":["publish.text"]}
```

## `apps` / `accounts`

```text
postkit apps set threads --client-id … --client-secret … --redirect-uri …
postkit apps show threads --json          # secret redacted
postkit accounts list [--site threads]
postkit accounts delete <site> --yes
```

`apps set` → `~/.postkit/apps/<site>.json`. Accounts → `~/.postkit/accounts/<site>/<name>.json`. List prints names, not tokens.

## Output document and exit codes

Success is `Outcome`: `{ "site", "id", "url" }`. Fan-out is `{ "results": [ Outcome | WireError, … ] }`. Errors:

```json
{ "error": "invalid_post", "site": "threads", "reason": "text_too_long", "limit": 500 }
```

| `error` | Exit |
|---------|------|
| `unknown_site`, `unknown_account`, `unsupported`, `invalid_post` | 2 |
| `auth` | 3 |
| `rate_limited` | 4 |
| `platform`, `network`, `timeout` | 5 |
