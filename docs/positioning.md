# Positioning

Who postkit is for, and how it compares. Stable axes only — prices move, these don't.

## The grid

| | **postkit** | Hosted post APIs (Ayrshare, Upload-Post, Outstand) | Self-hosted schedulers (Postiz) | Official SDKs |
|---|---|---|---|---|
| Token custody | **Your vault, 0600, local** | Their cloud | The app's DB, even self-hosted | Yours |
| Platform app | **BYO** (your Meta app) | Theirs — no App Review for you | Per-instance | BYO per SDK |
| Cost | **$0** | Paid subscription | $0 + your hosting | $0 |
| Scheduling | ✗ | ✓ | ✓ (the product) | ✗ |
| Media | **Images ✓** (video roadmap) | ✓ | ✓ | Varies |
| Agent surface | **exec CLI / `cargo add`** | REST + API key | Web app | Hand-rolled |
| License | **MIT OR Apache-2.0** | Proprietary | AGPL-3.0 | Vendor ToS |

Read the table as jobs, not scores. Hosted APIs sell convenience by taking custody (their cloud, their pre-approved platform apps, their metering). Schedulers sell a calendar. postkit is an **official-API execution kernel**: an agent or program sends **now** (a post, a paused ad draft, a typed WhatsApp message), with **your** apps, for **$0**, and errors it can branch on.

That sentence is the product. Ads and WhatsApp stay in this crate because they are official-API execution with the same custody and fail-closed posture. LinkedIn, Telegram, video, and any later site are connectors, not a second product. A "social publish CLI" split would orphan the safety work already shipped.

The trade postkit asks of you: you run your own Meta app (tester invites, redirect chips, eventually App Review — the walkthrough is [threads/](./threads/)) and you get text-only for now.

## Roadmap, and why

In priority order:

1. **Images** — shipped (2026-09-10): one image per post, optional caption; Bluesky uploads bytes (`--image file`), Threads takes a public https URL (`--image https://…`). Video remains roadmap.
2. **HTTP `serve` mode with `pk_live_` keys.** Shipped: `postkit keys create` / `postkit serve` on `127.0.0.1:8788`. Callers that cannot exec speak the same JSON over localhost HTTP without giving up custody.

Not on the roadmap, ever: calendar, a hosted/persistent inbox, drafts. Send
**now** or nothing is the product; a connector may still safely parse an
operator's signed inbound webhook without becoming an inbox product.

Full internal analysis lives in `design/016-rust-and-competitors.md` (untracked design notes).
