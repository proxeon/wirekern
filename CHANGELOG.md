# Changelog

All notable changes to postkit are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once a first tag exists.

Nothing has been tagged yet — everything below ships under the pending
`0.1.0`. Entries cite the commit that introduced them.

## [Unreleased]

### Added

- Threads publisher: text posts through the Graph API's
  `auto_publish_text`, with the `postkit` binary registering the
  connector. `1a5e22d`
- Paste-code OAuth for Threads: RFC 6749 authorization-code exchange
  plus Meta's long-lived-token exchange, no local callback server
  needed. `791bba1`
- Bluesky publisher: app-password auth over AT Protocol, text posts via
  `createRecord`, no app file required. `4dbbc28`, `35b0bac`
- Threads reply chains: repeatable `--text` publishes each line as a
  reply to the previous one (container creation then
  `threads_publish` per segment — replies cannot use
  `auto_publish_text`). Other sites refuse two `--text` with
  `thread_unsupported` before any HTTP. `a70f8dc`, `bd9e556`
- Single replies to an existing post: `--param reply_to_id=<id>`
  (threads only; Bluesky rejects unknown params with
  `unsupported_param:<k>` before HTTP). `1318c24`
- `--idempotency <key>`: client-side dedupe in the vault — a retry
  with the same key returns the stored `Outcome` without touching the
  network. Only *completed* publishes are remembered; an attempt that
  died after the platform created the post was never learned, so it
  posts again. `61de1fe`
- `--dry-run` create-only probe on threads: one container creation,
  no publish, nothing visible ever — the container expires unpublished
  after 24h. Answers "would this reply go through right now?" (branch:
  `container_id` ready / Graph `24` not-yet-visible / Graph `100` bad
  id) and smoke-tests credentials without a visible post. Refused on
  sites with no create/publish split (`dry_run_unsupported`) and
  rejected with `--idempotency` or a chain. `f4c16ca`
- GitHub Actions CI: clippy (deny warnings) and tests. `9964305`
- rustfmt adopted and gated in CI. `09da5f4`
- Operator documentation: CLI reference (`docs/cli.md`), site
  walkthroughs (`docs/threads`, `docs/bluesky`), positioning and
  feature-coverage grids, `.env.example`. `3672f52`, `7036e00`,
  `6a794aa`, `d1a1d6d`, `f6b1836`

### Changed

- Output-stream contract made strict: with `--json`, stdout carries
  exactly one JSON document; without it, stdout stays empty and every
  line — results included — goes to stderr. `--json` bytes are
  unchanged. `6691cd1`
- Graph errors are classified by numeric code first; the English
  substring heuristics ("expired", "quota", …) run only when the body
  carries no code, so Meta rewording copy cannot reclassify an error.
  Container publish polling is capped at 10 attempts (~4s) independent
  of the deadline. `4df18d2`
- Threads text limit counts by Meta's rule — every character is 1
  (CJK, Arabic, combining marks included), emoji as their UTF-8 byte
  length — instead of raw `str::len()`, which over-charged non-ASCII
  scripts and falsely rejected valid posts. `39d5163`, `867f50d`
- The duplicated form-encoding implementations (oauth and the Threads
  connector) are consolidated into one `crate::form` primitive.
  `9bc44ef`
- `--home` reads `POSTKIT_HOME` through clap's env folding instead of
  a manual environment read. `3897836`

### Fixed

- OAuth `state` is generated from the OS CSPRNG (128 bits of lowercase
  hex) instead of time nanoseconds, and pasted redirects are now
  verified against it. `5f017f2`, `f33066b`
- The shared HTTP client no longer follows redirects — a redirect to
  an attacker-controlled host would otherwise leak the
  credential-bearing query string. `21891cb`
- Vault writes create their tmp files `0600` with unique names, so a
  concurrent `postkit` run cannot clobber or read another's in-flight
  token file. `c1e1a91`
- Vault listing no longer aliases `.json`-suffixed account names onto
  other accounts' files. `51724f6`
- The vault home is never guessed from the current directory: with
  `HOME` unset (cron, systemd, `env -i`), postkit refuses with a
  re-auth-style message instead of silently writing tokens into
  `./.postkit`. `bac3cdb`
- `--token` bootstrap verifies the token via `whoami` *before* the
  vault write and persists the returned id, so publishing addresses
  `/{user_id}/threads` instead of leaning on the `/me` alias.
  `9c2a2bd`
- `--token` is refused on app-password sites at the door
  (`token_bootstrap_unsupported`) instead of storing creds that fail
  much later inside publish. `b39d97d`
- Proactive token refresh degrades gracefully on transient failures
  (network, 5xx, rate limit, timeout) — it fires while the stored
  token is still valid, so the publish proceeds and the next attempt
  retries the refresh. Auth failures stay fatal. `4ce1b98`
- Interactive app-password prompts suppress echo: an app password is a
  full account-access credential and must not land in terminal
  scrollback or screen shares. `b72ef1b`
- `apps show`/`set` surface when `POSTKIT_<SITE>_CLIENT_*`
  environment credentials outrank the file just written. `098daa7`
- Reply-container creation retries Graph `code 24` (parent not yet
  visible to the write path) at a fixed 2s pacing bounded by
  `--deadline`; roots never retry — a 24 there means a bad target.
  Measured live, the propagation window spans ~30s to 12–15 min night
  by night, so chains with fresh parents should use
  `--deadline 1500`. `86b21bd`, `f4c16ca`
- Docs no longer link tracked files into the private gitignored
  `design/` directory, which broke fresh clones. `eeb690e`

### Removed

- The unused `anyhow` dependency; errors flow through the crate's own
  `Error`/`WireError` pair. `3897836`
