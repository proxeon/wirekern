# WhatsApp Cloud — remaining features

Tick when shipped. Shipped v1 items are marked done so this file is the full
board, not only gaps. Product rule: this connector is a **controlled
responder** (typed send + signed parse). Hosted callback + local wamid
ledger are transport/correlation, not a conversation inbox or campaign tool.

Last aligned with Meta Cloud API docs and
[009-whatsapp-cloud-connector-online-references.md](../../plans/002-references/009-whatsapp-cloud-connector-online-references.md)
(2026-09-12). Graph pin: `v26.0`.

## Shipped

- [x] Static System User token; `auth` verifies `GET /{phone-number-id}` before vault write
- [x] One configured Phone Number ID (`whatsapp configure` / env merge)
- [x] Optional app secret for webhook HMAC (redacted in `apps show`)
- [x] Text **reply** (`context.message_id` = inbound `wamid`)
- [x] Approved **body-text** template (name, language, ordered body params ≤ 10)
- [x] `--allow-send` / HTTP `"allow_send": true` (deny-by-default policy)
- [x] Mandatory local idempotency key on every send
- [x] Not routable via generic `post` (`use_whatsapp_command`)
- [x] Signed webhook parse over **raw** body (`X-Hub-Signature-256`, constant-time)
- [x] Phone Number ID match on webhook (mismatch refuses the payload)
- [x] Inbound extract: `id`, `from`, `type`, optional `text.body`, `context.id`
- [x] Status extract: `sent` / `delivered` / `read` / `failed` + timestamp
- [x] `POST /v1/whatsapp` send
- [x] `POST /v1/whatsapp/webhook` parse (HMAC; not a listener)
- [x] `whoami` (id, display number, verified name — no token)

## Kernel holes (still in the v1 slice)

Do these without becoming an inbox.

- [x] Session text: `type=text` **without** `context` (Meta’s 24h customer-service window send)
- [x] Inbound media: image/audio/video/document/sticker **ids**, mime, caption
- [x] Inbound structured: location, contacts, interactive reply, reaction, referral, order, unsupported
- [x] Failed status `errors[]` (code / title only)
- [x] Status extras (optional, privacy-gated): `recipient_id`, `conversation`, `pricing`
- [x] HTTP webhook **parse**: `POST /v1/whatsapp/webhook` (same HMAC as CLI; no listener)
- [x] `preview_url` as a typed opt-in (today hardcoded `false`)
- [x] Recipient grammar: Meta-supported `+` / formatting vs digits-only
- [x] README capability row: add `read.webhook_statuses`
- [x] Document uncertain idempotency (“request left, response lost” — no silent retry)

## Send types (Cloud API, not typed yet)

Each needs a `WhatsAppMessage` variant + `--allow-send` + idempotency. No JSON escape hatch.

### Media

- [x] Media upload / retrieve metadata / download / delete
- [x] Send image (id or link, optional caption)
- [x] Send document (id or link, filename, caption)
- [x] Send audio
- [x] Send video (optional caption)
- [x] Send sticker

### Interactive and other service messages

- [x] Reply buttons (≤ 3)
- [x] List messages
- [x] CTA URL button
- [x] Location request button
- [x] Voice-call button
- [x] Location send
- [x] Contacts send
- [x] Address request
- [x] Reaction (emoji on inbound `wamid`)
- [x] Mark as read
- [x] Typing indicator
- [x] Group send (`recipient_type: group`)

### Templates (beyond body text)

- [x] Header: text
- [x] Header: image / video / document
- [x] Footer
- [x] Buttons (quick reply / URL / phone / copy code)
- [x] Named body parameters (vs ordered only)
- [x] Limited-time offer / coupon components
- [x] Template list / get / status / quality (read)
- [x] Template create / edit / delete / review submit (write — Business Management API)

### Commerce / Flows

- [x] Catalog / product messages
- [x] Order messages
- [x] WhatsApp Flows (own endpoint, schema, publishing state)

## Receive / operate (not send verbs)

### Webhook transport (BYO vs Postkit-hosted — decide before building)

- [x] HTTPS listener
- [x] GET webhook challenge (hub.verify_token)
- [x] HTTP 200 ACK to Meta
- [x] Subscribe WABA to `messages` via API (dashboard works today)
- [x] Dedup / reorder / replay protection
- [x] Durable message + status store (“what happened to `wamid X`?”)
- [x] Final-state reduction (sent → delivered → read / failed)
- [x] Retry / dead-letter for parse failures
- [x] Rate / throughput queue (Meta default ~80 msg/s per number)

Meta has **no** GET-by-`wamid`. History only exists if something stores webhooks.

### Business management

- [x] List WABAs / phone numbers
- [x] Phone registration / two-step PIN (Cloud API register; on-prem backup migrate not typed)
- [x] Quality rating / messaging-limit reads
- [x] System User list (`GET /{business-id}/system_users`; create stays Business Manager)
- [x] Embedded Signup start URL (no Facebook Login dance)
- [x] Multi-sender: more than one Phone Number ID per home

### Compliance and billing (Meta enforces window/pricing; we do not store policy state)

- [x] Opt-in / opt-out records
- [x] 24h customer-service window clock
- [x] Template category / pacing awareness
- [x] Conversation / pricing visibility from status webhooks
- [x] Usage metrics / template-quality reporting (phone health + template quality reads; no billing dashboard)
- [x] Bulk / campaign / marketing automation (`send_whatsapp_many` ≤ 10, rate-capped; no calendar/audience campaigns)

## Suggested kernel order

1. Session text (no `context`)
2. Inbound media ids + failed `errors[]`
3. HTTP `POST /v1/whatsapp/webhook` parse
4. Typed media send and/or richer template components
