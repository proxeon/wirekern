# Adding a connector

A site is a **`Publisher` module**, not a new `Client`. A checklist written after Threads alone would say “copy Graph”; Bluesky existing is what makes this page general.

**Done when:** `postkit post <site> --json` has worked once for you. Do not add a row to the inventory instead of a live `Outcome.id`.

> Numbered references below (`012`, `010 §5`, …) point to private, untracked design notes. This page is self-contained without them.

Official publish API, BYO credentials, publish only. If the website can do it and the API cannot, it stays out. No calendar, inbox, Chrome, or Meta-dashboard scrape.

Operator runbooks for shipped sites: [threads/](./threads/), [bluesky/](./bluesky/), [facebook-pages/](./facebook-pages/), [instagram/](./instagram/), [linkedin/](./linkedin/), [whatsapp-cloud/](./whatsapp-cloud/).

---

## Contrast (do not copy Threads into a new site)

| | Threads | Bluesky |
|--|---------|---------|
| Feature | `threads = ["client", "oauth"]` | `bluesky = ["client", …]` **no** `oauth` |
| `AuthKind` | `OAuth2AuthCode` | `AppPassword` |
| Hosts | `graph.threads.net` + unversioned oauth | `{pds}/xrpc/…` default `https://bsky.social` |
| App file | Required in the **connector** (`missing_app_config`) | None |
| Vault | `AccountCreds::OAuth2` long-lived token | `AccountCreds::AppPassword` (no JWT) |
| Limit | 500 chars; emoji as UTF-8 bytes | 300 **graphemes** |
| `--account` | usually `default` | **handle** (`default` → `identifier_required`) |

Graph consts, `th_exchange_token`, permalink GET, and `auto_publish_text` stay in `connectors/threads.rs`. AT Proto lexicon and `createSession` stay in `connectors/bluesky.rs`. Core has no site URLs.

---

## 010 §8 checklist (in-tree)

Do these in order. One site per PR.

### 1. Feature flag, off in lib defaults

`crates/postkit/Cargo.toml`:

```toml
# default = []   # must stay empty
client = ["dep:reqwest"]
oauth = ["client"]
# good:
mysite = ["client"]
# only if the site is RFC 6749 authorization-code:
mysite = ["client", "oauth"]
```

`oauth` is **RFC 6749 code exchange** (`crates/postkit/src/oauth.rs`). Extra hops (`th_exchange_token`, LinkedIn whatever) stay in **that** connector, not in `oauth.rs`.

`crates/postkit/src/lib.rs`:

```rust
#[cfg(any(feature = "threads", feature = "bluesky", feature = "mysite"))]
pub mod connectors;
```

`crates/postkit/src/connectors/mod.rs`:

```rust
#[cfg(feature = "mysite")]
pub mod mysite;
```

CLI (`crates/postkit-cli/Cargo.toml`) may enable the feature; the **lib** default must not.

Prove isolation when the site must not pull Graph OAuth:

```rust
#[cfg(all(test, not(feature = "oauth")))]
#[test]
fn mysite_feature_excludes_oauth() {}
```

Same pattern: `connectors/bluesky.rs` `bluesky_feature_excludes_oauth`.

### 2. Own params, capabilities, error map

- Params struct on the connector (see `ThreadsParams`). `chat_id` / `instance` / `user_id` live in `Intent.params`, validated **here**. Missing required param → `Error::InvalidPost` **before** HTTP.
- `capabilities()` is what you implement. `Capability::PublishImage` exists on the enum; listing it without a code path is a lie. `Client` already fail-closes: `Body` asks for a capability you did not file → `UnsupportedCapability`.
- Map transport to `Error` (then `WireError` for `--json`):

| HTTP / site | `Error` |
|-------------|---------|
| 401 / bad password / expired | `Auth { reason }` |
| 429 / quota | `RateLimited` |
| Their 4xx with a code | `Platform { code, message }` — their code, not our tag |
| 5xx / DNS | `Network` |
| Too long / empty | `InvalidPost` before HTTP |

Do not put Graph error subcodes in core. Do not return `"ok": true`.

Use `crate::http::Http` (feature `client`). It sets `User-Agent: postkit/<ver>` and the deadline. **No platform URLs in `http.rs`.**

### 3. No Graph-shaped types in core

Do not add fields to `Client`, `Site` (open string), or `Publisher` “for this site.” `Publisher` is frozen to publish + auth + probe. Extra verbs (insights, ads, pages, media, WhatsApp) are facet traits on `Connector` (`crates/postkit/src/facets.rs`). A publish-only site implements `Publisher` and calls `Registry::register`. A site that also reads metrics implements `InsightsSource` and registers with `Registry::register_connector(Connector::from_publisher(arc.clone()).insights(arc))`.

Allowed vault shapes today (`types.rs`): `OAuth2`, `AppPassword`, `BotToken`. Pick one. Redacting `Debug` already covers secrets.

`AuthKind`: `None` | `AppPassword` | `OAuth2AuthCode`. Telegram-shaped bots are `None` + `BotToken`. Do not implement empty OAuth stubs.

### 4. One mock text fixture, no live network in default tests

httpmock 0.7 is a **dev-dependency**. Happy `publish` + one auth failure is enough for v1 text.

- No `POSTKIT_LIVE` in `cargo test -p postkit` / `--features mysite`.
- Live post: `#[ignore]` + `POSTKIT_LIVE=1` + env (see Threads/Bluesky live tests).
- If the site has no Meta app: `Client::auth_finish` with an **empty** `MemoryAppStore` (copy `connectors::bluesky::tests::client_auth_finish_without_app_file`). Core fallback: `client.rs` `empty_app`. Connector that needs OAuth still errors inside **auth_start** (`threads` `auth_start_needs_app`).

`cargo test -p postkit` (no features) must stay green.

### 5. Skip `OAuth2AuthCode` if the site is not OAuth

| Kind | `auth_start` | `auth_finish` / CLI |
|------|----------------|---------------------|
| `OAuth2AuthCode` | `AuthStart::Browser { authorize_url, state }` | `--code` / paste URL; `extract_code` strips `#_` |
| `AppPassword` | `PasteInstructions` | `--password` + `--account` handle |
| `StaticToken` | `AuthStart::None` | `--token`; verify with `whoami` before saving a redacted `BotToken` |
| `None` | `AuthStart::None` or instructions | no credential bootstrap; an unauthenticated connector owns any setup itself |

`Client::auth_start` / `auth_finish` do **not** require `apps/<site>.json`. Threads still requires `app.oauth` **in the connector**.

### 6. Operator runbook under `docs/<site>/`

Same job as [threads/](./threads/) and [bluesky/](./bluesky/):

- How the human gets credentials (dashboard vs app password vs BotFather).
- Exact CLI, including `--account` if it is not `default`.
- Dummy vs real redirect URIs, chip fields, invites — only if that site has them.
- Limits (bytes vs graphemes).
- Failures you actually hit.
- The HTTP calls for debugging (hosts on the connector, not core).

Link it from [README.md](./README.md). Secrets stay in `.env` (copy [`.env.example`](../.env.example)). The CLI does **not** auto-load `.env`.

### 7. Register in the operator factory

In-tree sites go in `crates/postkit/src/bundle.rs` (`bundled_registry` /
`Client::from_home`). CLI, `postkit-serve`, and later MCP all call that —
do not copy a `Registry::register` list into a surface crate.

```rust
registry.register(Arc::new(postkit::connectors::mysite::MySite::new()?));
// Extra verbs: registry.register_connector(MySite::new()?.connector());
```

Enable the feature on the `postkit` dep in `crates/postkit-cli/Cargo.toml`
and `crates/postkit-serve/Cargo.toml`.

`postkit capabilities --json` must list the site. `unknown_site` (exit 2) means you skipped this step.

---

## Outsiders

Implement `Publisher`, call `Registry::register`. Extra verbs are optional facets on `Connector`, not new methods on `Publisher`. No core PR required. In-tree sites still get a feature flag so `cargo add postkit` default stays empty.

---

## Stay true

| Rule | Where it already is |
|------|---------------------|
| Vault writes only in `Client` | `client.rs` |
| Connectors do not touch `~/.postkit` | 012 |
| Empty app fallback on auth/publish/whoami | `client.rs`; tests in `tests.rs` + bluesky |
| `--account` is the vault name, not `Intent.params` | CLI global; Bluesky handle vs Threads `default` |
| `--json` document: `Outcome` or `WireError` | `error.rs` |
| No `serve` / media / third site to “finish” the kernel | 012 §11, 010 §5, plan 004 gates |

---

## PR shape

One site (or this doc). Do not mix Telegram + Threads image + `serve` in one PR.

Before merge:

```bash
cargo test -p postkit
cargo test -p postkit --features mysite
# if mysite must not pull Graph:
cargo test -p postkit --features mysite -- mysite_feature_excludes_oauth
cargo run -p postkit-cli -- capabilities --json
```
