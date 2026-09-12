# Meta Ads roadmap

This is the deliberately deferred work after the current Tier A reads and
Tier B paused-draft builder. It is a product/safety backlog, not an assertion
that every Marketing API feature should be copied into Postkit.

## Shipped boundary

Postkit reads metrics and ad accounts; uploads image and video assets; creates
typed paused campaigns, ad sets, ads, and several creative formats; previews
creatives; lists and inspects objects; and can activate, pause, archive,
delete, duplicate, and apply typed edits **from the CLI**. Default
`PausedOnlyAdsPolicy` still denies every spend-starting action before the
vault. `--allow-activate` (and matching `--allow-*` flags) opt in one action.
HTTP and MCP do not activate or edit budget.

`PausedOnlyAdsPolicy` is the boundary. Future work must preserve a clear
approval point before credentials or HTTP, rather than treating a new CLI flag
as authorization to spend.

## Tier C — controlled lifecycle changes

**Shipped on CLI.** `ads activate` / `ads pause` / `--state` write-ahead are
on `main`. Default policy still denies activate; pause is the emergency
valve. Remaining live operator passes are in
[live-remaining.md](./live-remaining.md).

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
