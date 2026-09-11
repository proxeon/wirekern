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
idempotency key.

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
postkit auth whatsapp_cloud --token '<SYSTEM_USER_ACCESS_TOKEN>'
```

For WhatsApp, environment values override their matching fields rather than
replacing the whole file: setting only `POSTKIT_WHATSAPP_PHONE_NUMBER_ID`
keeps an app secret previously saved by `whatsapp configure`. Set
`POSTKIT_WHATSAPP_APP_SECRET` only when you deliberately want to replace that
secret. `apps show` redacts the app secret and reports only whether webhook
signing is configured. `--verify-token` / `POSTKIT_WHATSAPP_VERIFY_TOKEN` is
the separate value Meta sends only during the public GET callback challenge.

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
enforces whether the reply is inside the customer-service window and otherwise
permitted. Do not bypass that requirement by pretending an arbitrary old ID is
a reply context.

The returned `id` is Meta's accepted outbound `wamid`, **not** proof of
delivery or read. Keep it to correlate the later status webhook.

`--idempotency` records only a **confirmed** success. If the request left
this machine and the response was lost, Postkit does **not** retry. Check
the delivery webhook (or WhatsApp Manager) before sending again — a second
send can be a second private message.

In-window follow-ups that are not quoting a specific inbound `wamid` use
`whatsapp text` (Meta `type=text` with no `context`). Meta still requires an
open customer-service window; Postkit does not track that clock.

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
ordered text variables. The library also has typed template-management APIs
and richer typed components; those management verbs are not CLI commands yet.

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

## Consent, window, and pacing boundaries

Postkit stores opt-in/opt-out records and computes a 24-hour window from its
local callback history, but neither is automatic permission to send or a
replacement for Meta's policy decision. Incomplete callback history must not
be mistaken for a complete customer record. Review consent, template approval,
pricing, and the customer-service window before passing `--allow-send`; Meta
remains the delivery authority.

Postkit paces outbound Cloud API requests at a process-local default of roughly
80 messages/second per configured account. Batches are capped at 10 items and
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
| `recipient_must_be_whatsapp_id` | Use country code + digits only, no `+`, spaces, or local formatting. |
| `template_name_invalid` | V1 accepts lowercase letters, digits and underscores only; use the approved name exactly. |
| Meta template/window error | Postkit sent a valid wire shape; correct the template approval, customer opt-in, recipient, or policy in WhatsApp Manager. |
| `webhook_signature_invalid` | Pass the unchanged body and exact `X-Hub-Signature-256` value; check the configured app secret. |
| `webhook_phone_number_mismatch` | The signed event belongs to another phone number. Route it to the Postkit configuration for that sender. |
| `webhook_status_unsupported` | Meta sent a delivery state this version does not model. Preserve the raw signed payload in your own webhook system and upgrade Postkit after reviewing it. |

## Current boundaries

The library and typed HTTP send surface are wider than the command-line UX:
the CLI currently exposes configuration, text, reply, basic template sends,
and raw webhook parsing. The HTTP send endpoint supports the other typed
message variants; template/Flow/media/account operations and ledger retention
remain library integrations until CLI or HTTP-management parity is designed.

Postkit does not terminate TLS, paginate large Meta collections, select an
outbound sender from multiple configured Phone Number IDs, run distributed rate
limits, replay dead letters, or provide a hosted inbox/billing dashboard. A
successful send is still only Meta acceptance; use signed statuses for the
final delivery result.

Track remaining work as checkboxes in [checklist.md](./checklist.md).
