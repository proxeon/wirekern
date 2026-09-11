# WhatsApp Cloud — remaining features

Tick when shipped. Shipped v1 items are marked done so this file is the full
board, not only gaps. Product rule: this connector is a **controlled
responder** (typed send + signed parse). Hosting an inbox, calendar, or
campaign tool is a separate product decision — those rows stay unchecked on
purpose until that decision changes.

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
- [x] `POST /v1/whatsapp` send (not parse)
- [x] `whoami` (id, display number, verified name — no token)

## Kernel holes (still in the v1 slice)

Do these without becoming an inbox.

- [x] Session text: `type=text` **without** `context` (Meta’s 24h customer-service window send)
- [x] Inbound media: image/audio/video/document/sticker **ids**, mime, caption
- [x] Inbound structured: location, contacts, interactive reply, reaction, referral, order, unsupported
- [x] Failed status `errors[]` (code / title only)
- [ ] Status extras (optional, privacy-gated): `recipient_id`, `conversation`, `pricing`
- [ ] HTTP webhook **parse**: `POST /v1/whatsapp/webhook` (same HMAC as CLI; no listener)
- [ ] `preview_url` as a typed opt-in (today hardcoded `false`)
- [ ] Recipient grammar: Meta-supported `+` / formatting vs digits-only
- [ ] README capability row: add `read.webhook_statuses`
- [ ] Document uncertain idempotency (“request left, response lost” — no silent retry)

## Send types (Cloud API, not typed yet)

Each needs a `WhatsAppMessage` variant + `--allow-send` + idempotency. No JSON escape hatch.

### Media

- [ ] Media upload / retrieve metadata / download / delete
- [ ] Send image (id or link, optional caption)
- [ ] Send document (id or link, filename, caption)
- [ ] Send audio
- [ ] Send video (optional caption)
- [ ] Send sticker

### Interactive and other service messages

- [ ] Reply buttons (≤ 3)
- [ ] List messages
- [ ] CTA URL button
- [ ] Location request button
- [ ] Voice-call button
- [ ] Location send
- [ ] Contacts send
- [ ] Address request
- [ ] Reaction (emoji on inbound `wamid`)
- [ ] Mark as read
- [ ] Typing indicator
- [ ] Group send (`recipient_type: group`)

### Templates (beyond body text)

- [ ] Header: text
- [ ] Header: image / video / document
- [ ] Footer
- [ ] Buttons (quick reply / URL / phone / copy code)
- [ ] Named body parameters (vs ordered only)
- [ ] Limited-time offer / coupon components
- [ ] Template list / get / status / quality (read)
- [ ] Template create / edit / delete / review submit (write — Business Management API)

### Commerce / Flows

- [ ] Catalog / product messages
- [ ] Order messages
- [ ] WhatsApp Flows (own endpoint, schema, publishing state)

## Receive / operate (not send verbs)

### Webhook transport (BYO vs Postkit-hosted — decide before building)

- [ ] HTTPS listener
- [ ] GET webhook challenge (hub.verify_token)
- [ ] HTTP 200 ACK to Meta
- [ ] Subscribe WABA to `messages` via API (dashboard works today)
- [ ] Dedup / reorder / replay protection
- [ ] Durable message + status store (“what happened to `wamid X`?”)
- [ ] Final-state reduction (sent → delivered → read / failed)
- [ ] Retry / dead-letter for parse failures
- [ ] Rate / throughput queue (Meta default ~80 msg/s per number)

Meta has **no** GET-by-`wamid`. History only exists if something stores webhooks.

### Business management

- [ ] List WABAs / phone numbers
- [ ] Phone registration / two-step PIN / migration
- [ ] Quality rating / messaging-limit reads
- [ ] System User provisioning
- [ ] Embedded Signup (other businesses’ WABAs)
- [ ] Multi-sender: more than one Phone Number ID per home

### Compliance and billing (Meta enforces window/pricing; we do not store policy state)

- [ ] Opt-in / opt-out records
- [ ] 24h customer-service window clock
- [ ] Template category / pacing awareness
- [ ] Conversation / pricing visibility from status webhooks
- [ ] Usage metrics / template-quality reporting
- [ ] Bulk / campaign / marketing automation

## Suggested kernel order

1. Session text (no `context`)
2. Inbound media ids + failed `errors[]`
3. HTTP `POST /v1/whatsapp/webhook` parse
4. Typed media send and/or richer template components
