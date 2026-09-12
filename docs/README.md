# Operator references

Human steps to make postkit actually publish. Design notes stay in `design/` (gitignored). Plans stay in `plans/` (gitignored, local).

Credentials: copy [`.env.example`](../.env.example) to `.env` (gitignored). The CLI does not auto-load it; `set -a && . ./.env && set +a` then `apps set` / `auth`.

| Path | When |
|------|------|
| [threads/](./threads/) | First live Threads post (Meta app, tester, paste-code OAuth, `post`). |
| [bluesky/](./bluesky/) | First live Bluesky post (handle, app password, `auth` / `post`). |
| [facebook-pages/](./facebook-pages/) | First live Facebook Page post (Page scopes, re-auth, discovery, explicit target). |
| [instagram/](./instagram/) | First live Instagram image post (Instagram Login, professional account, public HTTPS image). |
| [linkedin/](./linkedin/) | First live LinkedIn member text post (OIDC identity, Share on LinkedIn, versioned Posts API). |
| [meta-ads/](./meta-ads/) | Meta Ads: insights + paused drafts. Shipped vs missing: [checklist.md](./meta-ads/checklist.md). Auth-ops: [auth-ops.md](./meta-ads/auth-ops.md). Insights reports: [insights-reports.md](./meta-ads/insights-reports.md). |
| [whatsapp-cloud/](./whatsapp-cloud/) | WhatsApp Cloud setup: System User token, typed sends (text/reply/template/media/interactive/Flows), signed webhook parse, local wamid ledger. Remaining work: [checklist.md](./whatsapp-cloud/checklist.md). |
| [mcp/](./mcp/) | Local MCP stdio adapter for agent hosts. Remaining work: [checklist.md](./mcp/checklist.md). |
| [cli.md](./cli.md) | Flag-by-flag reference: `post` / `auth` semantics, vault layout, exit codes. |
| [positioning.md](./positioning.md) | Comparison grid vs hosted APIs / schedulers / SDKs; roadmap rationale. |
| [connectors.md](./connectors.md) | Add a site: 010 §8 checklist (feature flag, httpmock, runbook, CLI register). |
