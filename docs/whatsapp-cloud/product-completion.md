# WhatsApp Cloud product-completion plan

This release closes the operational gaps between the existing typed WhatsApp
library and the command line without adding an unreviewed raw Graph request
surface. It follows Meta's current [official WhatsApp Cloud API
collection](https://www.postman.com/meta/whatsapp-business-platform/documentation/wlk6lh4/whatsapp-cloud-api?entity=request-13382743-f2eb9575-f109-4767-ab47-4cf74c14444f), including its versioned Graph edges for messages, media,
templates, Flows, WABAs, phone numbers, system users, registration, and app
subscription.

## Scope and safety invariants

1. Every generic send is deserialized as `WhatsAppSendRequest`, the same
   closed Rust type used by the library and HTTP surface. Wirekern never accepts
   arbitrary Graph JSON.
2. Customer-addressed sends retain both mandatory idempotency and
   `--allow-send`. Management writes instead require `--yes`, so a command
   cannot publish a Flow, submit/change/delete a template, alter phone setup,
   or delete media by accident.
3. Collection responses carry only the opaque `paging.cursors.after` value.
   Wirekern does not expose or replay Graph's `paging.next` URL, and it percent
   encodes a supplied cursor before making the next request.
4. A selected sender must be a configured local alias. A raw phone-number ID
   is never accepted in a send request. The resolved phone number namespaces
   local pacing and idempotency, preventing a result from one business number
   from replaying for another.
5. Live tests are ignored by default. Read-only Graph contracts require an
   explicit environment switch; a separate media upload/delete contract needs
   a second write switch and a disposable local fixture. Neither test sends a
   customer message, creates templates/Flows, or changes account settings.

## Implementation sequence

1. Add opaque pagination contracts and return cursors for templates, Flows,
   WABAs, phone numbers, and system users. Preserve node reads (one configured
   WABA) as a single page and reject a cursor there.
2. Add CLI commands for the existing typed sends, media, template, Flow,
   account, ledger, and consent operations. JSON drafts are typed and local;
   media download creates a new output file only.
3. Add multi-sender configuration, explicit `--sender` routing, per-phone
   throughput queues, and per-phone idempotency namespaces. Keep webhook
   acceptance for each configured sender.
4. Add mocked wire/validation tests plus an opt-in, credential-gated,
   read-only live contract suite. Document how to run it and what it proves.

## Safety hardening completed after the operating release

The follow-up safety release adds a concrete Caddy/TLS deployment runbook,
strict file-backed consent/window authorization, and an opt-in encrypted
dead-letter replay queue. The replay key is supplied only through
`WIREKERN_WHATSAPP_REPLAY_DLQ_KEY`; normal operation retains a hash-only audit
record and does not archive customer content.

Wirekern remains intentionally outside the scope of a distributed rate limiter,
hosted inbox, billing product, or campaign/broadcast system. Those require a
separate operational ownership model rather than a local connector feature.
