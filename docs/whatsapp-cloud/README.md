# WhatsApp Cloud runbook

`whatsapp_cloud` sends narrowly typed **private business messages** through
Meta's official WhatsApp Cloud API. It is deliberately unlike `post`: every
send names one recipient, requires an idempotency key, and needs an explicit
`--allow-send` acknowledgement because templates and conversations can have
consent and billing consequences.

The connector sends typed Cloud API messages (text, media, interactive,
templates, catalog/order, Flows) and parses **signed inbound webhooks**.
`postkit serve` provides the challenge/acknowledgement callback and a small
local delivery ledger; it is not a conversation inbox. The server is plain
HTTP and must sit behind your own public HTTPS reverse proxy or tunnel before
Meta can reach it. Every customer send still needs `--allow-send` and an
idempotency key. File-backed CLI/HTTP sends additionally enforce the local
consent and customer-service-window rules described below.

Meta's current Cloud API requirements and message examples are in its
[official WhatsApp Cloud API collection](https://www.postman.com/meta/whatsapp-business-platform/documentation/wlk6lh4/whatsapp-cloud-api?entity=request-13382743-f2eb9575-f109-4767-ab47-4cf74c14444f).

## 1. Prepare Meta assets

In the Meta developer/business setup for the legitimate business:

1. Add the WhatsApp use case/product to the Meta app.
2. Create or attach a WhatsApp Business Account and verify a business phone
   number. Copy its numeric **Phone number ID** (not the visible phone
   number, WABA ID, App ID, or Business Portfolio ID).
3. Create a System User access token that can send WhatsApp business messages
   for that phone number. Treat it as a password.
4. Copy the Meta app secret if this Postkit instance will parse inbound
   webhooks. It verifies `X-Hub-Signature-256`; it is not sent to Graph.
5. In Meta, configure a public HTTPS webhook endpoint and subscribe it to the
   WhatsApp `messages` field. Point it at a TLS reverse proxy or tunnel that
   forwards unchanged bytes to Postkit's local callback route below.

Do not use a personal WhatsApp login password or a scraped WhatsApp Web
session. Cloud API is a business platform and needs these business assets.

## 2. Configure Postkit and verify the token

Keep the token out of committed files. You may configure the sender interactively:

```bash
postkit whatsapp configure \
  --phone-number-id 123456789012345 \
  --sender marketing=987654321098765 \
  --app-secret '<META_APP_SECRET>' \
  --verify-token '<RANDOM_CALLBACK_VERIFY_TOKEN>'

postkit auth whatsapp_cloud --token '<SYSTEM_USER_ACCESS_TOKEN>'
postkit whoami whatsapp_cloud --json
postkit apps show whatsapp_cloud --json
```

`auth` calls the configured phone-number endpoint before writing the token to
the local 0600 vault. The success output includes only the phone-number ID and
verified/display name; it never prints the token.

For CI or a noninteractive server, configure the app data in the environment
instead. The token should still go through `auth … --token` once and live in
the Postkit vault.

```bash
export POSTKIT_WHATSAPP_PHONE_NUMBER_ID=123456789012345
export POSTKIT_WHATSAPP_APP_SECRET='<META_APP_SECRET>' # only needed for webhook parsing
export POSTKIT_WHATSAPP_VERIFY_TOKEN='<RANDOM_CALLBACK_VERIFY_TOKEN>'
# Optional: 64 hex characters. Enables encrypted replay of signed callbacks
# that a future Postkit parser learns to understand; never commit this value.
export POSTKIT_WHATSAPP_REPLAY_DLQ_KEY='<64_HEX_CHARACTERS>'
# Optional: enables paged owned-WABA and system-user reads.
export POSTKIT_WHATSAPP_BUSINESS_ID=123456789012345
postkit auth whatsapp_cloud --token '<SYSTEM_USER_ACCESS_TOKEN>'
```

For WhatsApp, environment values override their matching fields rather than
replacing the whole file: setting only `POSTKIT_WHATSAPP_PHONE_NUMBER_ID`
keeps an app secret previously saved by `whatsapp configure`. Set
`POSTKIT_WHATSAPP_APP_SECRET` only when you deliberately want to replace that
secret. `apps show` redacts the app secret and reports only whether webhook
signing is configured. `--verify-token` / `POSTKIT_WHATSAPP_VERIFY_TOKEN` is
the separate value Meta sends only during the public GET callback challenge.

`--sender alias=phone_number_id` adds a secondary outbound number without
changing the primary phone. Send with `--sender alias`; Postkit accepts only a
configured alias, then applies separate local pacing and idempotency for that
phone. The primary sender remains the default when `--sender` is omitted.

## 3. Send a reply

Replies are tied to a known inbound `wamid`; use the `from` and `id` from a
verified parsed webhook. Recipients may include a leading `+` and
spaces/hyphens/parentheses; Postkit normalizes to digits (keeping `+` if you
typed it) and does not invent a country code.

```bash
postkit --json whatsapp reply \
  --to 60123456789 \
  --reply-to 'wamid.HBgLN...inbound...' \
  --text 'Terima kasih. Kami akan semak pesanan anda.' \
  --idempotency customer-reply-order-42-v1 \
  --allow-send
```

`--allow-send` is required on every private send. Without it the default
policy rejects the command **before** vault/network access. Meta, not Postkit,
remains the final delivery authority. Postkit's file-backed CLI/HTTP clients
also refuse free-form content when no verified inbound callback has opened a
local 24-hour customer-service window; do not bypass that by pretending an
arbitrary old ID is a reply context.

The returned `id` is Meta's accepted outbound `wamid`, **not** proof of
delivery or read. Keep it to correlate the later status webhook.

`--idempotency` records only a **confirmed** success. If the request left
this machine and the response was lost, Postkit does **not** retry. Check
the delivery webhook (or WhatsApp Manager) before sending again — a second
send can be a second private message.

In-window follow-ups that are not quoting a specific inbound `wamid` use
`whatsapp text` (Meta `type=text` with no `context`). Meta still requires an
open customer-service window; Postkit computes a conservative local check
from verified callbacks before it sends.

```bash
postkit --json whatsapp text \
  --to '+60 12-345 6789' \
  --text 'Kami masih semak.' \
  --idempotency follow-up-order-42-v1 \
  --allow-send
```

Parse a forwarded webhook with your own receiver:

```bash
curl -sS -X POST http://127.0.0.1:8788/v1/whatsapp/webhook \
  -H "Authorization: Bearer pk_live_…" \
  -H "X-Hub-Signature-256: sha256=…" \
  --data-binary @webhook.json
```

## 4. Send an approved template

Create and obtain Meta approval for the exact template in WhatsApp Manager
first. The CLI sends an approved template with a lowercase name, language, and
ordered text variables. Richer template components and lifecycle operations
are available through the typed `whatsapp send` and `whatsapp templates`
commands below.

```bash
postkit --json whatsapp template \
  --to 60123456789 \
  --name order_update \
  --language en_US \
  --body-param 'A-42' \
  --body-param 'tomorrow' \
  --idempotency order-update-A-42-v1 \
  --allow-send
```

Template categorisation, approval, messaging limits, user opt-in, and
per-conversation/template pricing are Meta business decisions. Review them in
WhatsApp Manager before adding `--allow-send`. A Postkit success means Meta
accepted the request; monitor signed status webhooks for delivery/failure.

## 4a. Typed command coverage

The short `text`, `reply`, and `template` commands remain useful for their
common cases. `send` provides the rest of the closed message schema (image,
document, audio, video, sticker, interactive, catalog/order, Flow, read, and
typing) from a local JSON document; it never passes arbitrary Graph JSON.

```json
{
  "message": {
    "type": "image",
    "to": "60123456789",
    "link": "https://example.com/receipt.jpg",
    "caption": "Your receipt"
  },
  "idempotency_key": "receipt-42-v1",
  "recipient_type": "individual"
}
```

```bash
postkit --json whatsapp send \
  --request receipt.json \
  --sender marketing \
  --allow-send
```

Use `send-batch --requests requests.json --allow-send` for at most ten typed
requests from one sender. It waits for the selected sender's local pacing slot;
it is not a campaign/broadcast feature.

The matching typed lifecycle commands are available under `whatsapp media`,
`whatsapp templates`, `whatsapp flows`, `whatsapp account`, `whatsapp ledger`,
and `whatsapp consent`. Writes require `--yes`; media download refuses to
overwrite an existing local file. Template and Flow drafts are typed JSON
files, such as `whatsapp templates create --draft utility.json --yes`.

Collection lists accept `--limit` and `--after`. JSON output returns the next
opaque `after` cursor when Meta has another page; supply it unchanged to the
same list command. Do not try to construct or modify a cursor.

## 5. Receive webhooks safely

For a Postkit-managed callback, run the HTTP listener on loopback and terminate
TLS before it. Do not expose `postkit serve` directly to the internet: it does
not own certificates or HTTPS.

```text
Meta HTTPS -> your TLS reverse proxy/tunnel -> http://127.0.0.1:8788/v1/whatsapp/callback
```

```bash
postkit keys create --name whatsapp-callback-read
postkit serve --bind 127.0.0.1:8788
```

### Minimal HTTPS deployment: Caddy

Use a domain that resolves to the host running Postkit and allow inbound TCP
443. Keep Postkit bound to loopback; Caddy owns the public certificate and
forwards the unchanged request locally.

```caddyfile
whatsapp.example.com {
    reverse_proxy 127.0.0.1:8788
}
```

Run Caddy with that file, then set Meta's callback URL to
`https://whatsapp.example.com/v1/whatsapp/callback`. Do **not** configure a
CDN/body-transforming proxy in front of this route: the signature covers the
exact bytes Meta sent. Health-check the local process separately; Postkit does
not issue certificates, redirect HTTP, or bind a public interface for you.

Configure the public `https://…/v1/whatsapp/callback` URL in Meta. The GET
request validates `hub.verify_token` and returns the raw challenge; the POST
request verifies `X-Hub-Signature-256`, records correlation state, and returns
HTTP 200. The proxy must forward the body byte-for-byte and preserve that
signature header. `GET /v1/whatsapp/events/{wamid}` requires a `pk_live_` key.

For a bring-your-own receiver, forward the **unchanged raw bytes** and exact
signature header to the parser. Do not parse/reformat JSON before passing it
along—the HMAC is over the raw body.

```bash
postkit --json whatsapp webhook parse \
  --signature "$X_HUB_SIGNATURE_256" < webhook-body.json
```

The parser has a 1 MiB body bound, requires the `sha256=` signature format,
uses constant-time HMAC-SHA256 verification, checks the event belongs to the
configured Phone number ID, and returns inbound messages and the supported
outbound status callbacks in Meta's payload order:

```json
{
  "site": "whatsapp_cloud",
  "messages": [{
    "id": "wamid.HBgL…",
    "from": "60123456789",
    "type": "text",
    "timestamp": "1720000000",
    "text": "Hello"
  }],
  "statuses": [{
    "id": "wamid.HBgL…",
    "status": "delivered",
    "timestamp": "1720000001"
  }]
}
```

Human output prints only the count to avoid copying customer phone numbers and
message text or message IDs into terminal scrollback. `--json` intentionally
returns that personal data and opaque `wamid` to the explicit caller. The
callback ledger deduplicates/reduces events and keeps only correlation metadata
(`wamid`, sender ID, type, timestamps, reply context, final status, and limited
conversation/pricing fields). It drops text, captions, media, location,
contacts, interactive/reaction/referral/order content, and raw webhook bodies.

The ledger has no implicit retention period: the embedding application chooses
one and calls `Client::purge_whatsapp_ledger_before(unix_timestamp)`. This
purges message/status/window records and hashed dead-letter audit entries, but
deliberately does not erase consent records, which can have a separate
legal-retention basis.

### Encrypted replay for signed parser failures

The default dead-letter audit contains only a reason, body hash, and timestamp.
To retain a signed raw callback for replay after a Postkit parser upgrade, set
`POSTKIT_WHATSAPP_REPLAY_DLQ_KEY` to exactly 64 random hexadecimal characters
before starting Postkit. Generate and store it in your secret manager, not the
repository or the Postkit home directory:

```bash
openssl rand -hex 32
export POSTKIT_WHATSAPP_REPLAY_DLQ_KEY='…generated value…'
```

The bounded (at most 1 MiB) body and HMAC are XChaCha20-Poly1305 encrypted at
rest; list output shows only metadata. After upgrading, inspect and replay
deliberately:

```bash
postkit --json whatsapp ledger dead-letters
postkit --json whatsapp ledger replay --id dlq-… --yes
```

A successful replay verifies the original signature again, reduces the event
into the ordinary privacy-minimal ledger, then deletes its ciphertext. A still
unsupported event remains queued. Losing or rotating the key without draining
the queue makes those retained events unrecoverable by design.

## Consent, window, and pacing boundaries

File-backed CLI and HTTP send paths enforce a conservative local decision after
the explicit `--allow-send` acknowledgement:

- A recorded `opt_out` refuses every customer-visible send.
- An approved template requires a recorded `opt_in` for that individual.
- Free-form text, replies, media, interactive, catalog, and Flow messages need
  an observed inbound callback within the last 24 hours. A missing callback is
  treated as closed, not as permission.
- Read/typing acknowledgements act on an inbound `wamid` and carry no `to`, so
  their final validation remains with Meta. Group sends are refused by this
  strict local policy because Postkit has no individual consent record to
  evaluate.

Record consent using a normalized WhatsApp ID before sending a template:

```bash
postkit whatsapp consent set --wa-id 60123456789 --kind opt_in --yes
```

An explicit `opt_out` is always stronger than an open customer-service window.
These checks are an additional safety control, not proof that the stored local
history is complete or that a template meets Meta policy; Meta remains the
delivery authority. Library embeddings can intentionally install
`AllowWhatsAppSendsPolicy` instead when they provide their own compliance
service.

Postkit paces outbound Cloud API requests at a process-local default of roughly
80 messages/second per configured phone number. Batches are capped at 10 items and
wait for the next slot within the caller's deadline. This is local pacing, not
a distributed quota service or a Meta throughput guarantee.

## Idempotency and uncertain outcomes

Every send has a mandatory local idempotency key. Repeating a **confirmed**
send with the same key replays the stored outcome without making a second API
call. If the process loses the response after the request left your machine,
the remote result is unknown and Postkit records nothing. Do not blindly retry:
inspect the signed status/inbound webhook or WhatsApp Manager first, then make
an intentional operational decision.

## Common failures

| Symptom | Meaning / next action |
| --- | --- |
| `missing_phone_number_id` | Run `whatsapp configure …`, or set `POSTKIT_WHATSAPP_PHONE_NUMBER_ID`. Use the numeric Phone number ID, not the display number/WABA ID. |
| `token_invalid` | Create/renew the authorised System User token, then re-run `auth whatsapp_cloud --token …`. |
| `policy_denied` / `explicit_whatsapp_send_required` | Review consent, window, template and pricing, then repeat the exact typed command with `--allow-send`. |
| `whatsapp_consent_missing` | Record a verified `opt_in` before a template send. |
| `whatsapp_consent_opted_out` | Do not send customer-visible content; honor the local opt-out. |
| `whatsapp_customer_window_closed` | Wait for a verified inbound message or use an approved template after recording opt-in. |
| `recipient_must_be_whatsapp_id` | Use country code + 7–15 digits. A leading `+` and spaces, hyphens, or parentheses are stripped; Postkit does not invent a country code. Letters and other punctuation are refused. |
| `template_name_invalid` | V1 accepts lowercase letters, digits and underscores only; use the approved name exactly. |
| Meta template/window error | Postkit sent a valid wire shape; correct the template approval, customer opt-in, recipient, or policy in WhatsApp Manager. |
| `webhook_signature_invalid` | Pass the unchanged body and exact `X-Hub-Signature-256` value; check the configured app secret. |
| `webhook_phone_number_mismatch` | The signed event belongs to another phone number. Route it to the Postkit configuration for that sender. |
| `webhook_status_unsupported` | Meta sent a delivery state this version does not model. With the replay key configured, inspect `whatsapp ledger dead-letters` after upgrading. |

## Current boundaries

The CLI now covers the typed library's messaging, media, template, Flow,
account, consent, and ledger operations. The authenticated HTTP surface
supports typed sends and the same optional configured `sender` alias; it does
not expose management endpoints. Use the CLI or library for management until
an HTTP-management authorization contract is separately designed.

Postkit does not terminate TLS itself, run distributed rate limits, or provide
a hosted inbox/billing dashboard. It documents a TLS deployment and provides
an opt-in encrypted local replay queue, but the operator still owns the domain,
secret manager, process supervision, and retention choice. A successful send
is still only Meta acceptance; use signed statuses for the final delivery
result.

## Opt-in live contract reads

Mocked tests validate wire shapes. To verify the live, read-only Graph
contracts against a deliberately configured local test account, run the
ignored suite with a local Postkit vault and app configuration:

```bash
POSTKIT_LIVE_WHATSAPP=1 \
POSTKIT_HOME="$HOME/.postkit" \
cargo test -p postkit --features whatsapp-cloud,vault-file tests::live_whatsapp_cloud_reads -- --ignored --exact
```

It calls only `whoami`, first-page template/Flow/WABA/phone/system-user reads,
and phone health. It does not send, upload, create, publish, subscribe, or
change account settings. Omit the environment flag to keep the suite refused.
See [product-completion.md](./product-completion.md) for the implementation
and safety plan.

To validate the media upload/delete wire contract without addressing a
customer, provide a disposable supported fixture and explicitly authorize the
ignored test. It uploads once, then deletes the returned media ID:

```bash
POSTKIT_LIVE_WHATSAPP_WRITE_TESTS=1 \
POSTKIT_HOME="$HOME/.postkit" \
POSTKIT_LIVE_WHATSAPP_MEDIA_FILE=/absolute/path/to/disposable.png \
POSTKIT_LIVE_WHATSAPP_MEDIA_MIME=image/png \
cargo test -p postkit --features whatsapp-cloud,vault-file \
  tests::live_whatsapp_cloud_media_upload_delete -- --ignored --exact
```

Track remaining work as checkboxes in [checklist.md](./checklist.md).
