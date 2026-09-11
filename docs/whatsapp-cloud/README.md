# WhatsApp Cloud runbook

`whatsapp_cloud` sends narrowly typed **private business messages** through
Meta's official WhatsApp Cloud API. It is deliberately unlike `post`: every
send names one recipient, requires an idempotency key, and needs an explicit
`--allow-send` acknowledgement because templates and conversations can have
consent and billing consequences.

The connector supports a text reply attached to an inbound message, an
existing approved text template, and parsing **signed inbound webhooks**. It
does not provide an inbox API or a server. WhatsApp delivers inbound messages
and final sent/delivered/read/failed status to your HTTPS webhook; Postkit can
verify and parse the exact raw body that endpoint receives.

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
   WhatsApp `messages` field. Your own endpoint must perform Meta's webhook
   verification challenge and acknowledge deliveries promptly. Postkit does
   not host that endpoint.

Do not use a personal WhatsApp login password or a scraped WhatsApp Web
session. Cloud API is a business platform and needs these business assets.

## 2. Configure Postkit and verify the token

Keep the token out of committed files. You may configure the sender interactively:

```bash
postkit whatsapp configure \
  --phone-number-id 123456789012345 \
  --app-secret '<META_APP_SECRET>'

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
postkit auth whatsapp_cloud --token '<SYSTEM_USER_ACCESS_TOKEN>'
```

For WhatsApp, environment values override their matching fields rather than
replacing the whole file: setting only `POSTKIT_WHATSAPP_PHONE_NUMBER_ID`
keeps an app secret previously saved by `whatsapp configure`. Set
`POSTKIT_WHATSAPP_APP_SECRET` only when you deliberately want to replace that
secret. `apps show` redacts the app secret and reports only whether webhook
signing is configured.

## 3. Send a reply

Replies are tied to a known inbound `wamid`; use the `from` and `id` from a
verified parsed webhook. Enter the recipient as its WhatsApp ID: country code
plus digits, with no `+`, spaces, or normalisation by Postkit.

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

## 4. Send an approved template

Create and obtain Meta approval for the exact template in WhatsApp Manager
first. Postkit does not create, edit, inspect approval status, or send a raw
template payload. V1 supports only a lowercase template name, language code,
and ordered text variables for the template body.

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

## 5. Parse incoming messages safely

Forward the **unchanged raw bytes** and exact signature header from your HTTPS
webhook application to this command. Do not parse/reformat JSON before passing
it along—the HMAC is over the raw body.

```bash
postkit --json whatsapp webhook parse \
  --signature "$X_HUB_SIGNATURE_256" < webhook-body.json
```

The parser has a 1 MiB body bound, requires the `sha256=` signature format,
uses constant-time HMAC-SHA256 verification, checks the event belongs to the
configured Phone number ID, and returns messages in Meta's order:

```json
{
  "site": "whatsapp_cloud",
  "messages": [{
    "id": "wamid.HBgL…",
    "from": "60123456789",
    "type": "text",
    "timestamp": "1720000000",
    "text": "Hello"
  }]
}
```

Human output prints only the count to avoid copying customer phone numbers and
message text into terminal scrollback. `--json` intentionally returns that
personal data to the explicit caller; Postkit does not persist it, deduplicate
events, or answer Meta's HTTP request.

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

## Deliberate v1 boundary

Implemented: static System User token validation, explicit sender
configuration, text replies, approved text-template sends, mandatory
idempotency, deny-by-default messaging policy, and verified inbound webhook
message extraction.

Not implemented: webhook HTTP hosting/challenge/acknowledgement, inbox or
status persistence, template CRUD/review, pricing/billing surfaces, recipient
discovery, opt-in tracking, conversation-window storage, media, interactive
messages, catalogs, Flows, bulk/campaign sends, multi-phone/WABA discovery,
retries, delivery-status actions, and direct-send beta features. Each has its
own consent, privacy, billing, or payload contract and must be designed before
it is added.
