# Operator references

Human steps to make postkit actually publish. Design notes stay in `design/` (gitignored). Plans stay in `plans/`.

Credentials: copy [`.env.example`](../.env.example) to `.env` (gitignored). The CLI does not auto-load it; `set -a && . ./.env && set +a` then `apps set` / `auth`.

| Path | When |
|------|------|
| [threads/](./threads/) | First live Threads post (Meta app, tester, paste-code OAuth, `post`). |
| [bluesky/](./bluesky/) | First live Bluesky post (handle, app password, `auth` / `post`). |
| [cli.md](./cli.md) | Flag-by-flag reference: `post` / `auth` semantics, vault layout, exit codes. |
| [positioning.md](./positioning.md) | Comparison grid vs hosted APIs / schedulers / SDKs; roadmap rationale. |
| [connectors.md](./connectors.md) | Add a site: 010 §8 checklist (feature flag, httpmock, runbook, CLI register). |
