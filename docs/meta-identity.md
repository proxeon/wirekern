# Meta identity (operator sidenote)

Observed 2026-09 while unblocking Threads after

```json
{"error":"platform","site":"threads","code":"200","message":"API access blocked."}
```

That error is the **Facebook user**, not a bad vault file. `auth`, `whoami`, and `post` all fail the same way until Meta trusts the user again. It applies to every Meta-backed connector (`threads`, `instagram`, `facebook_pages`, `meta_ads`) and to creating/using the developer app that WhatsApp Cloud sits on.

## What we hit

- A login **Security check** (`facebook.com/checkpoint/…`) only offered recovery numbers **already on the account**. A current SIM cannot be typed into that radio list.
- Dead SIMs would not delete in desktop Accounts Center. The Facebook / Instagram **mobile app** could remove them.
- Meta for Developers registration / “verify your account” completed in **Chrome on the phone** (same device as the SIM). Desktop web checkpointed or looped.
- Re-running `postkit auth <site> --code` during the checkpoint still returned Graph `200`. OAuth codes are one-shot; do not retry the last redirect URL.

Meta’s identity work (SMS, 2FA, developer verify, checkpoints) is built around a phone they can text and an app session they already trust. The desktop site is a thinner client of that stack. Publishing is not: once `whoami` returns an id, the desktop CLI is the path.

## Operator split

1. **Identity** — phones, 2FA, developer verify, tester-invite accept: Facebook / Instagram / Threads **app**, or Chrome on the phone.
2. **Publishing** — desktop CLI after `whoami <site> --json` returns an id.

After the checkpoint:

1. [Accounts Center](https://accountscenter.facebook.com/) → **Personal details** → **Contact info**.
2. Add the number you can receive SMS on, confirm it, **then** remove dead SIMs. Do not delete the last recovery phone before the new one verifies. Desktop may refuse the delete; use the mobile app.
3. Finish developer verify on the phone if the dashboard still asks.
4. Start a **new** paste-code `auth`. The previous `--code` is spent.

Do not use a virtual / rented SMS number. If the listed phones are gone and the app is not logged in on another device, Meta’s fallback is identity upload (MyKad / passport / licence) or [facebook.com/hacked](https://www.facebook.com/hacked).
