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
  connector. `c807fd4`
- Paste-code OAuth for Threads: RFC 6749 authorization-code exchange
  plus Meta's long-lived-token exchange, no local callback server
  needed. `ea165fa`
- Bluesky publisher: app-password auth over AT Protocol, text posts via
  `createRecord`, no app file required. `de5c4f2`, `81a70ad`
- Threads reply chains: repeatable `--text` publishes each line as a
  reply to the previous one (container creation then
  `threads_publish` per segment — replies cannot use
  `auto_publish_text`). Other sites refuse two `--text` with
  `thread_unsupported` before any HTTP. `a4da024`, `dfc9353`
- Single replies to an existing post: `--param reply_to_id=<id>`
  (threads only; Bluesky rejects unknown params with
  `unsupported_param:<k>` before HTTP). `04875fd`
- `--idempotency <key>`: client-side dedupe in the vault — a retry
  with the same key returns the stored `Outcome` without touching the
  network. Only *completed* publishes are remembered; an attempt that
  died after the platform created the post was never learned, so it
  posts again. `8bd2288`
- `--dry-run` create-only probe on threads: one container creation,
  no publish, nothing visible ever — the container expires unpublished
  after 24h. Answers "would this reply go through right now?" (branch:
  `container_id` ready / Graph `24` not-yet-visible / Graph `100` bad
  id) and smoke-tests credentials without a visible post. Refused on
  sites with no create/publish split (`dry_run_unsupported`) and
  rejected with `--idempotency` or a chain. `d758f64`
- GitHub Actions CI: clippy (deny warnings) and tests. `6017c4c`
- rustfmt adopted and gated in CI. `31fed1a`
- Operator documentation: CLI reference (`docs/cli.md`), site
  walkthroughs (`docs/threads`, `docs/bluesky`), positioning and
  feature-coverage grids, `.env.example`. `6a61876`, `51f6188`,
  `161efed`, `4b3d0a9`, `c8eb221`
- Regression tests locking multi-line text on both connectors: threads
  must send newlines as `%0A` in the form body (validated live,
  2026-09-09); bluesky carries JSON-escaped `\n` and counts each
  newline as one grapheme against the 300 limit. `f6d0b0a`

### Changed

- Output-stream contract made strict: with `--json`, stdout carries
  exactly one JSON document; without it, stdout stays empty and every
  line — results included — goes to stderr. `--json` bytes are
  unchanged. `db89f13`
- Graph errors are classified by numeric code first; the English
  substring heuristics ("expired", "quota", …) run only when the body
  carries no code, so Meta rewording copy cannot reclassify an error.
  Container publish polling is capped at 10 attempts (~4s) independent
  of the deadline. `20a6210`
- Threads text limit counts by Meta's rule — every character is 1
  (CJK, Arabic, combining marks included), emoji as their UTF-8 byte
  length — instead of raw `str::len()`, which over-charged non-ASCII
  scripts and falsely rejected valid posts. `059df1e`, `ebdcd1f`
- The duplicated form-encoding implementations (oauth and the Threads
  connector) are consolidated into one `crate::form` primitive.
  `a6b3a3f`
- `--home` reads `POSTKIT_HOME` through clap's env folding instead of
  a manual environment read. `1b137df`

### Fixed

- OAuth `state` is generated from the OS CSPRNG (128 bits of lowercase
  hex) instead of time nanoseconds, and pasted redirects are now
  verified against it. `c31646a`, `d3ce6f1`
- The shared HTTP client no longer follows redirects — a redirect to
  an attacker-controlled host would otherwise leak the
  credential-bearing query string. `75cd627`
- Vault writes create their tmp files `0600` with unique names, so a
  concurrent `postkit` run cannot clobber or read another's in-flight
  token file. `0d3112f`
- Vault listing no longer aliases `.json`-suffixed account names onto
  other accounts' files. `d9913d7`
- The vault home is never guessed from the current directory: with
  `HOME` unset (cron, systemd, `env -i`), postkit refuses with a
  re-auth-style message instead of silently writing tokens into
  `./.postkit`. `bfe12d6`
- `--token` bootstrap verifies the token via `whoami` *before* the
  vault write and persists the returned id, so publishing addresses
  `/{user_id}/threads` instead of leaning on the `/me` alias.
  `d30da48`
- `--token` is refused on app-password sites at the door
  (`token_bootstrap_unsupported`) instead of storing creds that fail
  much later inside publish. `d03e120`
- Proactive token refresh degrades gracefully on transient failures
  (network, 5xx, rate limit, timeout) — it fires while the stored
  token is still valid, so the publish proceeds and the next attempt
  retries the refresh. Auth failures stay fatal. `3dbb81f`
- Interactive app-password prompts suppress echo: an app password is a
  full account-access credential and must not land in terminal
  scrollback or screen shares. `6845882`
- `apps show`/`set` surface when `POSTKIT_<SITE>_CLIENT_*`
  environment credentials outrank the file just written. `57ac69d`
- Reply-container creation retries Graph `code 24` (parent not yet
  visible to the write path) at a fixed 2s pacing bounded by
  `--deadline`; roots never retry — a 24 there means a bad target.
  Measured live, the propagation window spans ~30s to 12–15 min night
  by night, so chains with fresh parents should use
  `--deadline 1500`. `d7db141`, `d758f64`
- Docs no longer link tracked files into the private gitignored
  `design/` directory, which broke fresh clones. `7b36e2f`

### Removed

- The private `plans/` directory from the repository and its history
  (`git filter-repo`); it is gitignored and kept locally, the same
  treatment as `design/` and `issues/`. README and docs links into it
  are dropped so fresh clones carry no dangling references. `e2c4b6b`
- The unused `anyhow` dependency; errors flow through the crate's own
  `Error`/`WireError` pair. `1b137df`
