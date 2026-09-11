# WhatsApp Cloud product-completion plan

This release closes the operational gaps between the existing typed WhatsApp
library and the command line without adding an unreviewed raw Graph request
surface. It follows Meta's current [official WhatsApp Cloud API
collection](https://www.postman.com/meta/whatsapp-business-platform/documentation/wlk6lh4/whatsapp-cloud-api?entity=request-13382743-f2eb9575-f109-4767-ab47-4cf74c14444f), including its versioned Graph edges for messages, media,
templates, Flows, WABAs, phone numbers, system users, registration, and app
subscription.

## Scope and safety invariants

1. Every generic send is deserialized as `WhatsAppSendRequest`, the same
   closed Rust type used by the library and HTTP surface. Postkit never accepts
   arbitrary Graph JSON.
2. Customer-addressed sends retain both mandatory idempotency and
   `--allow-send`. Management writes instead require `--yes`, so a command
   cannot publish a Flow, submit/change/delete a template, alter phone setup,
   or delete media by accident.
3. Collection responses carry only the opaque `paging.cursors.after` value.
   Postkit does not expose or replay Graph's `paging.next` URL, and it percent
   encodes a supplied cursor before making the next request.
4. A selected sender must be a configured local alias. A raw phone-number ID
   is never accepted in a send request. The resolved phone number namespaces
   local pacing and idempotency, preventing a result from one business number
   from replaying for another.
5. Live tests are ignored by default and read-only. They must be explicitly
   requested with local credentials; they do not send a customer message,
   create templates/Flows, or change account settings.

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

## Explicitly deferred

This is not a distributed rate limiter, a hosted inbox, a billing product, or
a replayable dead-letter queue. Consent/customer-window records remain
operator signals rather than automatic authorization. A separate release can
add a durable, encrypted event archive and replay protocol after its privacy
and operations contract is reviewed.
