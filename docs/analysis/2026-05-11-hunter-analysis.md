# Hunter Analysis Report

## Inputs

- trace file: `hunter_trace.jsonl`
- metrics file: `hunter_signal_metrics.jsonl`
- wallet file: `wallet.toml`

## Coverage

- trace entries: `1260`
- signal metric entries: `7`
- trace window: `2026-05-11T09:21:06Z` -> `2026-05-11T15:05:21Z`
- metrics first-seen window: `1778503890000` -> `1778510822905` ms

## Funnel

- `shortlist_refresh`: `1038`
- `skip`: `189`
- `ws_received`: `9`
- `firing`: `7`
- `signal_accepted`: `7`
- `error`: `5`
- `bundle_sent`: `4`
- `bundle_retry`: `1`

## Skip Reasons

- `source_obligation_healthy`: `189`
- `bundle_send_failed`: `3`
- `signal_resolution_failed`: `2`
- `retryable_bundle_send_error`: `1`

## Error Hotspots

- `jito rate limit / congestion`: `3`
- `source=primary_rpc getTransaction returned null for signature 2PHkTRZd5DUK6Do1FuiR3A5SvDtyZpZ8vNZbpDitiRuUXw6ZXD7utDmAsiiXeAcAzoirFAg4QzNh99znD2r6s4f2 after 3 attempts (primary_commitment=confirmed fallback=confirmed)`: `1`
- `source=primary_rpc getTransaction returned null for signature F9ttoN9UXCDYsLuBg9Fy4vapyv4UuQJWDsxdf1qjuKxWp7KQhRvmEJkxxVG5jXLthQAw2izpbJyHYVxhTpdQevN after 3 attempts (primary_commitment=confirmed fallback=confirmed)`: `1`

## Healthy Signal Profile

- `primary_rpc` healthy skips: `189`

- `primary_rpc` LTV from healthy traces: `count=189` `avg=0.757828` `min=0.226641` `max=0.912924`

## Source Race

- `primary_rpc` winner lock count: `7`

## Fire Outcomes

- `bundle_sent`: `4`
- `bundle_failed`: `3`

- `primary_rpc` outcomes:
  - `bundle_sent`: `4`
  - `bundle_failed`: `3`

## Shortlist Signals

- `shortlist_hit=false`: `19`
- `shortlist_hit=true`: `3`
- `prepared_context_used=false`: `20`
- `prepared_context_used=true`: `2`
- `shortlist_state=armed`: `3`

## Wallet Gaps

- no `token_not_whitelisted` gap found in the trace window

## Notes

- `bundle_sent` measures acceptance by the Jito endpoint, not an observed on-chain liquidation win.
- Observed success still has to be correlated with Airtable or another on-chain verification source.
- This report is meant to be regenerated over time to compare the same funnel across different observation windows.
