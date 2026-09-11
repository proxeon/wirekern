# Changelog

All notable changes to postkit are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once a first tag exists.

Nothing has been tagged yet — everything below ships under the pending
`0.1.0`. Entries cite behavior and dates, never commit hashes: hashes
die on every history rewrite, and the commit log already carries the
same detail by subject.

## [Unreleased]

### Changed

- Product sentence is now explicit: postkit is an **official-API execution
  kernel** (send now through your apps, your vault, no calendar). Ads and
  WhatsApp stay in this crate. LinkedIn, Telegram, and video are future
  connectors, not a second product.

### Added

- WhatsApp Cloud business messaging: the new `whatsapp_cloud` connector uses
  a configured Phone number ID and a verified static System User token, not a
  fictional OAuth redirect. It sends only two typed, private shapes: a text
  reply tied to an inbound `wamid`, or an existing Meta-approved template with
  ordered text-body substitutions. Both require a mandatory idempotency key
  and per-command `--allow-send`; the default client policy refuses every
  WhatsApp write before vault or network access. A successful `Outcome.id` is
  Meta's accepted outbound `wamid`, never a delivery/read claim. Inbound
  support is a bounded raw webhook parser: it verifies Meta's
  `X-Hub-Signature-256` HMAC in constant time, requires the configured Phone
  number ID, and returns only inbound messages in provider order. There is no
  listener, webhook acknowledgement, inbox persistence, status store,
  template CRUD/review, billing surface, recipient discovery, bulk send,
  media, interactive message, Flow, or retry-on-unknown-write behavior.

- Instagram organic image publishing: the new `instagram` connector uses the
  direct Instagram Login authorization path, stores only its long-lived token
  and resolved professional-account ID, and publishes one public HTTPS image
  with an optional caption through an explicit media-container → readiness
  poll → publish flow. The container is read until Meta reports `FINISHED`,
  preventing the live `9007` immediate-publish race; terminal container states
  refuse before the visible write.
  It has no text-only post capability, Page target, local-image hosting,
  dry-run, ads, spend, video/Reels, carousel, Stories, comments, or insights
  surface. Captions are locally capped at 2,200 Unicode scalar values; generic
  alt text is deliberately not sent until its Instagram Login wire contract is
  verified. Token refresh uses `ig_refresh_token`, while ambiguous write
  outcomes are never auto-retried to avoid duplicate visible posts.
- Instagram bounded media reads: `postkit media list instagram --limit 1..25`
  adds the separate `read.media` capability for one server-ordered first page
  of the authorized professional account's published media. The reply exposes
  only ID plus optional permalink, caption, media type, and timestamp; it has
  no arbitrary account target or pagination cursor. A confirmed image publish
  now makes one best-effort permalink GET and returns `Outcome.url` when Meta
  supplies it. That follow-up failure never turns the already-confirmed write
  into an error or causes a second visible publish.
- Instagram image carousels: repeat `post instagram --image` 2–10 times to
  create one `publish.carousel` post with an optional single parent caption.
  Postkit creates each public-HTTPS child with `is_carousel_item=true`, waits
  for every child, creates and waits for the `CAROUSEL` parent, and then sends
  exactly one visible publish request. Child/parent errors stop before that
  final write; child IDs stay internal and expiring. Carousel `--alt`, replies,
  dry-runs, local files, videos, and mixed-media albums are refused rather than
  guessed. Confirmed results use the existing best-effort permalink lookup;
  visible writes are never retried automatically.
- Facebook Pages organic publishing: the new `facebook_pages` connector
  discovers token-visible Pages with `postkit pages accounts facebook_pages`,
  then posts text or one local image only when the operator provides an
  explicit `--param page_id=<id>`. Its Page OAuth grant is narrowly scoped to
  `pages_show_list,pages_manage_posts,pages_read_engagement`, stored long-lived
  credentials are user tokens only, and transient Page tokens are never saved
  or emitted in JSON. Image posts use multipart bytes with optional caption
  and alt text; remote image URLs are refused before credential/network access.
  Organic Page posts are immediately visible when Meta accepts them — this is
  not a paused/no-spend advertising capability.

### Fixed

- The dual license now ships its texts (issue 004): `LICENSE-APACHE` and
  `LICENSE-MIT` at the repository root, symlinked into both crates so
  `cargo package` includes them in every published `.crate` — recipients
  previously got the SPDX notice with no terms to comply with.
- `--stdin` now refuses every content flag instead of silently ignoring it
  (issue 025): combining it with `--text`, `--image`, `--alt`, `--param`,
  `--to`, or a positional site exits 2 with `stdin_exclusive` before stdin
  is read — a script can no longer publish a body its command line does
  not describe. `--dry-run` and `--idempotency` still apply on top of the
  stdin request.

- Idempotency check-and-record is now atomic (issue 023): `Client::publish`
  claims the key before publishing and releases it on every exit path, so
  two concurrent same-key callers can no longer both pass the ledger check
  and double-post. A call that finds the key in flight answers a new
  transient `idempotency` wire error (exit 4, same bucket as rate limits);
  the file vault backs the claim with an O_EXCL `0600` claim file, and a
  holder that crashed without releasing is stolen after 15 minutes. A
  failed attempt releases immediately — the key stays retryable.
- Token refresh now runs under the caller's deadline (issue 024):
  `Publisher::refresh` takes the publish/insight deadline, so `--deadline 1`
  bounds refresh plus request end-to-end instead of silently adding a fixed
  30s while credentials are near expiry. A proactive refresh that meets a
  spent budget still degrades gracefully to publishing with the stored
  token, which then fails fast with the same `DeadlineExceeded`.

### Added

- Bluesky replies: `postkit post bluesky --param reply_to_id=<at://…>` with
  the parent's at-URI (the string `Outcome.id` returns). The connector
  resolves the parent's `cid` via one `com.atproto.repo.getRecord` and
  writes the lexicon `reply.root`/`reply.parent` strongRefs — a reply to a
  reply inherits its parent's thread root instead of starting a new thread.
  Malformed targets (non-`at://` URIs, non-post collections) refuse locally
  with `invalid_post:reply_to_id` before any HTTP; image replies remain
  refused (`image_reply_unsupported`) until that wire is taught; unknown
  params still refuse with `unsupported_param:<k>`.
- Images on posts (plans/001/015, `publish.image`): `postkit post <site>
  --image … [--text caption] [--alt …]`. The kernel seam carries both forms
  platforms ingest — bytes (Bluesky `uploadBlob` → `app.bsky.embed.images`
  embed with alt text, ≤ 2 MB, png/jpg/gif/webp) and a public https URL
  (Threads `media_type=IMAGE` container, caption under the same 500-byte
  rule) — and deliberately never bridges them: each connector refuses the
  form it cannot honor (`image_source_unsupported:bytes|url`) before
  credentials or HTTP. Image reply chains, `reply_to_id`, and `--dry-run`
  combinations are refused with stable reasons until their wire contracts
  are live-verified; fan-out isolates per-target failures as usual.

### Fixed

- meta_ads OAuth now requests `pages_show_list` and `pages_manage_ads`
  alongside the ads scopes. Without them `/me/accounts` returns no Pages
  and the Page-backed link-creative step is unreachable — Tier B's creative
  path could not complete on any freshly authorized token. Stored tokens
  keep the scopes they were granted: re-run `auth meta_ads` once.
- `ads status-draft --wait` (now `Client::paused_draft_status_wait`) ends
  at `--deadline` by returning the **last observed status reply** — an
  object still under Meta review stays an explicit pending result — instead
  of starting a poll with the deadline already spent and surfacing a raw
  `timeout` error. Found during the 2026-09-10 live validation.

### Added

- Meta Ads resumable paused-draft launches (plans/001/013): one reviewed
  JSON manifest creates the full paused hierarchy (image → campaign → ad set
  → creative → ad) through the existing Tier B primitives, checkpointing
  every confirmed remote ID into an owner-only local state file. Commands:
  `ads validate-draft` (zero-I/O), `ads create-draft`, `ads resume-draft`
  (runs only remaining steps), `ads status-draft` (GET-only review snapshot,
  bounded `--wait`), and `ads adopt-draft-step` (records the human-resolved
  ID of an ambiguous write; delivery objects are remotely verified `PAUSED`
  first). Ambiguous writes (network/deadline after send) leave an
  `in_flight` marker and refuse every later run with a
  `reconciliation_required` result instead of risking duplicate remote
  objects; there is no `--force`. The state file is `0600`, atomically
  rewritten, exclusive-locked per run, and stores no secrets. Resume is
  bound to the manifest's SHA-256 canonical fingerprint — reformatting is
  harmless, any semantic change refuses (`draft_manifest_changed`). Local
  validation now closes the objective→optimization→billing pairing
  (`unsupported_adset_pairing:…`) and Meta's ~USD 1/day budget floor
  (`daily_budget_below_minimum`) before any remote object exists.
- Meta Ads Tier A+ read surface: remote ad-account discovery, explicit account
  selection, campaign/ad-set/ad entity filters, typed country/platform/age
  breakdowns, purchase value, and derived ROAS. These remain read-only.
- Meta Ads Tier B paused-first management: image upload, Page-backed image
  link creative creation, local desktop/mobile creative previews, and typed
  campaign/ad-set/ad creation. Delivery-object forms structurally hard-code
  `PAUSED`; there is no CLI activation, budget-update, delete, billing, or
  available-funds verb.
- Meta Ads review-status inspection: `postkit ads status meta_ads` reports an
  object's configured and effective lifecycle states plus structured Meta
  review issues. Its opt-in `--wait` poll is bounded by `--deadline` and
  returns an explicit `pending_review` result; it only makes GET requests.
- Meta Ads capabilities and safety seams: `read.ad_accounts`,
  `read.ad_previews`, `read.ad_review_status`, `create.paused_ads`,
  `create.ad_creative`, and the injected `PausedOnlyAdsPolicy`. The default
  policy approves only non-delivering assets and structurally paused creates,
  refusing future activation and budget mutation before credentials or HTTP.
- Meta Ads insights (read-only, design 026 Tier A): `meta_ads` connector
  with the `read.metrics` capability — spend/performance metrics from the
  Marketing API (Graph `v26.0`, pinned) as daily rows by
  account/campaign/adset/ad. Paste-code OAuth on the Facebook dialog
  (`ads_read` scope), long-lived token via `fb_exchange_token`, ad
  account resolved and stored at auth; cursor paging deadline-checked and
  capped. No verb in the connector can spend; management/activation stay
  gated per 026 §5.
- Read seam (kernel): `Capability::ReadMetrics`, `InsightsQuery` /
  `InsightsReply` types (`src/insights.rs`), `Publisher::insights` with a
  default refusal, and `Client::insights` (range ≤ 90 days enforced in
  the kernel, `invalid_query` exit 2; proactive + reactive refresh like
  publish). The same query shape serves the future IG/TikTok/Google
  insights connectors (design 028's grammar).
- `postkit insights <site>` CLI subcommand: `--from/--to` (inclusive,
  `YYYY-MM-DD`), `--level`, `--metrics`, required `--attribution`
  (no silent window default), `--ad-account` override. Deterministic
  JSON rows (entity+date order, alphabetical metric keys).
- Threads publisher: text posts through the Graph API's
  `auto_publish_text`, with the `postkit` binary registering the
  connector.
- Paste-code OAuth for Threads: RFC 6749 authorization-code exchange
  plus Meta's long-lived-token exchange, no local callback server
  needed.
- Bluesky publisher: app-password auth over AT Protocol, text posts via
  `createRecord`, no app file required.
- Threads reply chains: repeatable `--text` publishes each line as a
  reply to the previous one (container creation then
  `threads_publish` per segment — replies cannot use
  `auto_publish_text`). Other sites refuse two `--text` with
  `thread_unsupported` before any HTTP.
- Single replies to an existing post: `--param reply_to_id=<id>`
  (threads only; Bluesky rejects unknown params with
  `unsupported_param:<k>` before HTTP).
- `--idempotency <key>`: client-side dedupe in the vault — a retry
  with the same key returns the stored `Outcome` without touching the
  network. Only *completed* publishes are remembered; an attempt that
  died after the platform created the post was never learned, so it
  posts again.
- `--dry-run` create-only probe on threads: one container creation,
  no publish, nothing visible ever — the container expires unpublished
  after 24h. Answers "would this reply go through right now?" (branch:
  `container_id` ready / Graph `24` not-yet-visible / Graph `100` bad
  id) and smoke-tests credentials without a visible post. Refused on
  sites with no create/publish split (`dry_run_unsupported`) and
  rejected with `--idempotency` or a chain.
- GitHub Actions CI: clippy (deny warnings) and tests.
- rustfmt adopted and gated in CI.
- Operator documentation: CLI reference (`docs/cli.md`), site
  walkthroughs (`docs/threads`, `docs/bluesky`), positioning and
  feature-coverage grids, `.env.example`. ,

- Regression tests locking multi-line text on both connectors: threads
  must send newlines as `%0A` in the form body (validated live,
  2026-09-09); bluesky carries JSON-escaped `\n` and counts each
  newline as one grapheme against the 300 limit.

### Changed

- Output-stream contract made strict: with `--json`, stdout carries
  exactly one JSON document; without it, stdout stays empty and every
  line — results included — goes to stderr. `--json` bytes are
  unchanged.
- Graph errors are classified by numeric code first; the English
  substring heuristics ("expired", "quota", …) run only when the body
  carries no code, so Meta rewording copy cannot reclassify an error.
  Container publish polling is capped at 10 attempts (~4s) independent
  of the deadline.
- Threads text limit counts by Meta's rule — every character is 1
  (CJK, Arabic, combining marks included), emoji as their UTF-8 byte
  length — instead of raw `str::len()`, which over-charged non-ASCII
  scripts and falsely rejected valid posts.
- The duplicated form-encoding implementations (oauth and the Threads
  connector) are consolidated into one `crate::form` primitive.

- `--home` reads `POSTKIT_HOME` through clap's env folding instead of
  a manual environment read.

### Fixed

- Meta Marketing API errors now prefer the operator-facing
  `error_user_msg` over a generic summary such as `Invalid parameter`, so
  billing, Page, creative, and configuration corrections are actionable.
- OAuth `state` is generated from the OS CSPRNG (128 bits of lowercase
  hex) instead of time nanoseconds, and pasted redirects are now
  verified against it.
- The shared HTTP client no longer follows redirects — a redirect to
  an attacker-controlled host would otherwise leak the
  credential-bearing query string.
- Vault writes create their tmp files `0600` with unique names, so a
  concurrent `postkit` run cannot clobber or read another's in-flight
  token file.
- Vault listing no longer aliases `.json`-suffixed account names onto
  other accounts' files.
- The vault home is never guessed from the current directory: with
  `HOME` unset (cron, systemd, `env -i`), postkit refuses with a
  re-auth-style message instead of silently writing tokens into
  `./.postkit`.
- `--token` bootstrap verifies the token via `whoami` *before* the
  vault write and persists the returned id, so publishing addresses
  `/{user_id}/threads` instead of leaning on the `/me` alias.

- `--token` is refused on app-password sites at the door
  (`token_bootstrap_unsupported`) instead of storing creds that fail
  much later inside publish.
- Proactive token refresh degrades gracefully on transient failures
  (network, 5xx, rate limit, timeout) — it fires while the stored
  token is still valid, so the publish proceeds and the next attempt
  retries the refresh. Auth failures stay fatal.
- Interactive app-password prompts suppress echo: an app password is a
  full account-access credential and must not land in terminal
  scrollback or screen shares.
- `apps show`/`set` surface when `POSTKIT_<SITE>_CLIENT_*`
  environment credentials outrank the file just written.
- Reply-container creation retries Graph `code 24` (parent not yet
  visible to the write path) at a fixed 2s pacing bounded by
  `--deadline`; roots never retry — a 24 there means a bad target.
  Measured live, the propagation window spans ~30s to 12–15 min night
  by night, so chains with fresh parents should use
  `--deadline 1500`.
- Docs no longer link tracked files into the private gitignored
  `design/` directory, which broke fresh clones.

### Removed

- The private `plans/` directory from the repository and its history
  (`git filter-repo`); it is gitignored and kept locally, the same
  treatment as `design/` and `issues/`. README and docs links into it
  are dropped so fresh clones carry no dangling references.
- The unused `anyhow` dependency; errors flow through the crate's own
  `Error`/`WireError` pair.
