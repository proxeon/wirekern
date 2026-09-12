# Remaining live validation (operator, not CI)

These two gap rows cannot run in CI: they need the operator's Marketing API
token and a real `act_`. Graph pin `v26.0`.

Account used in the 2026-09-10 no-spend live pass: `act_1414222080648203`.

## Live GET inventory + inspect

```bash
postkit ads list meta_ads --entity campaign --ad-account act_1414222080648203 --json
postkit ads list meta_ads --entity adset --ad-account act_1414222080648203 --json
postkit ads inspect meta_ads --entity campaign --id <id> --json
```

HTTP (same credential via `pk_live_` is **read-only**; it cannot activate):

```bash
curl -sS -H "Authorization: Bearer pk_live_…" \
  'http://127.0.0.1:8080/v1/ads/list?site=meta_ads&entity=campaign'
```

## Live POST activate / edits

Only on a **paused**, low daily-budget object after `ads inspect` confirms
the echo values. Default policy denies activate until `--allow-activate`.

```bash
postkit ads activate meta_ads --entity adset --id <id> --confirm-id <id> \
  --confirm-daily-budget <n> --allow-activate --state activate.state.json
postkit ads pause meta_ads --entity adset --id <id>
```

Do not run this from CI. A leftover `--state` `in_flight` is
`reconciliation_required`; read `ads status` before any retry.
