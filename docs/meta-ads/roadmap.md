# Meta Ads roadmap

This is the deliberately deferred work after the current Tier A reads and
Tier B paused-draft builder. It is a product/safety backlog, not an assertion
that every Marketing API feature should be copied into Postkit.

## Shipped boundary

Postkit currently reads metrics and ad accounts; uploads image assets; creates
one Page-backed image-link creative; creates campaign, ad set, and ad objects
with `status=PAUSED`; previews creatives; reads review state; and resumes a
checkpointed paused-draft launch. It exposes no command that can deliver an
ad, change a budget, or change billing.

`PausedOnlyAdsPolicy` is the boundary. Future work must preserve a clear
approval point before credentials or HTTP, rather than treating a new CLI flag
as authorization to spend.

## Tier C — controlled lifecycle changes

**Goal:** let an operator deliberately move a reviewed paused draft through a
small lifecycle without turning Postkit into an unattended spending engine.

1. Define a typed lifecycle request for a known campaign, ad set, or ad:
   `PAUSED` → `ACTIVE`, plus a reverse `ACTIVE` → `PAUSED` emergency stop.
   Do not accept arbitrary Graph `status` strings.
2. Add an explicit ads policy action for activation and pause. The default
   policy must continue to deny activation. A policy that allows it must be
   deliberately selected by the embedding application or command, record the
   exact object ID, and require a human-readable confirmation of the delivery
   and budget consequences.
3. Before activation, read and show configured/effective status, review
   issues, ad account currency, selected daily/lifetime budget, bid strategy,
   optimization goal, Page/creative identity, and destination URL. Refuse a
   non-paused object or unresolved review issue rather than guessing.
4. Keep a write-ahead checkpoint/audit record that distinguishes a confirmed
   Meta response from a timeout after send. Never blindly retry an ambiguous
   activation write.
5. Test success, denial before vault/HTTP, malformed ID, review-pending,
   token-expiry retry, idempotency/concurrency, ambiguous network outcome, and
   a live test on a deliberately funded low-budget account.

**Non-goal:** automated activation rules, auto-bidding, or auto-reload. Those
are a separate product decision with materially different financial risk.

## Tier D — safe edits and lifecycle inventory

**Goal:** inspect and make bounded changes to objects Postkit already knows.

- Read/list campaigns, ad sets, ads, creatives, and their effective status by
  selected account, with pagination caps and stable ordering.
- Typed edits for one field family at a time: budget, bid strategy, targeting,
  placements, schedule, creative, and status must not share an unvalidated
  free-form update payload.
- A budget change needs its own policy action, current/new minor-currency
  value, account currency, change direction, and maximum-change guard.
- A targeting/placement edit should produce a before/after diff suitable for
  human review and be refused when it changes a special-ad category contract.
- Delete and duplicate remain separate proposals: deletion is destructive;
  duplication can inherit delivery/budget fields unexpectedly.

## Creative and campaign coverage

The existing image-link creative is intentionally narrow. Add formats only
after their exact Meta wire contract and review path are specified:

1. Additional image-link CTA shapes and Page/Instagram identity validation.
2. Video upload and video creative, including processing/status polling.
3. Carousel, catalog, dynamic creative, lead-form, and app-install shapes.
4. Instagram placement/identity and cross-account permissions.

Each format needs typed input, local asset validation, preview/readback where
available, paused-only creation, mocked wire tests, and a separate live test.

## Reporting expansion

- More typed metrics and attribution settings, with definitions documented per
  metric so a number cannot be misread as a billing total.
- Async/bulk reporting only with bounded jobs, cancellation, polling deadlines,
  pagination, and result-size limits.
- Separate read contracts for creative, delivery, and breakdown reports rather
  than a single unbounded generic Graph query.

## Explicitly out of scope today

- Billing, payment methods, available funds, invoices, tax/business settings,
  and auto-reload.
- Automated rules, experiments, optimization automation, custom audiences,
  pixels/Conversions API, and lead collection workflows.
- A generic "run arbitrary Marketing API request" escape hatch. It would
  bypass the typed validation and policy boundary that makes paused drafts
  safe.

## Implementation order

1. Tier C lifecycle policy and an operator-confirmed pause/activate model.
2. Read/list inventory and lifecycle readback for known objects.
3. One bounded edit family, starting with budget only if its confirmation and
   policy contract are accepted.
4. Additional creative formats, one typed format at a time.
5. Reporting breadth after delivery/creative structures are stable.

No Tier should start just because Meta exposes an endpoint. It starts once its
operator authority, local validation, error/timeout behaviour, test plan, and
live no-surprise validation are written down.
