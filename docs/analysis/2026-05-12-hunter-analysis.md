# Hunter Analysis Report

## Inputs

- trace file: `hunter_trace.jsonl`
- metrics file: `hunter_signal_metrics.jsonl`
- wallet file: `wallet.toml`

## Coverage

- trace entries: `4249`
- signal metric entries: `22`
- trace window: `2026-05-11T09:21:06Z` -> `2026-05-12T06:15:25Z`
- metrics first-seen window: `1778503890000` -> `1778550098436` ms

## Funnel

- `shortlist_refresh`: `3778`
- `skip`: `378`
- `ws_received`: `26`
- `signal_accepted`: `22`
- `firing`: `19`
- `bundle_sent`: `14`
- `error`: `9`
- `bundle_retry`: `3`

## Skip Reasons

- `source_obligation_healthy`: `375`
- `bundle_send_failed`: `5`
- `signal_resolution_failed`: `4`
- `retryable_bundle_send_error`: `3`
- `token_not_whitelisted`: `2`
- `wallet_token_zero_cap`: `1`

## Error Hotspots

- `jito rate limit / congestion`: `5`
- `no KLEND liquidate instruction found`: `1`
- `source=primary_rpc getTransaction returned null for signature 2PHkTRZd5DUK6Do1FuiR3A5SvDtyZpZ8vNZbpDitiRuUXw6ZXD7utDmAsiiXeAcAzoirFAg4QzNh99znD2r6s4f2 after 3 attempts (primary_commitment=confirmed fallback=confirmed)`: `1`
- `source=primary_rpc getTransaction returned null for signature AXSgjfurTc8KqVvCTaVUuYABZJSidpW7Vu7srEt2B3GNgTbG5VKsqw17ms4RiXDwGrM4z2uqqWam7NJbBEzmLZ9 after 3 attempts (primary_commitment=confirmed fallback=confirmed)`: `1`
- `source=primary_rpc getTransaction returned null for signature F9ttoN9UXCDYsLuBg9Fy4vapyv4UuQJWDsxdf1qjuKxWp7KQhRvmEJkxxVG5jXLthQAw2izpbJyHYVxhTpdQevN after 3 attempts (primary_commitment=confirmed fallback=confirmed)`: `1`

## Healthy Signal Profile

- `primary_rpc` healthy skips: `375`

- `primary_rpc` LTV from healthy traces: `count=370` `avg=0.681285` `min=0.013663` `max=0.912924`

## Source Race

- `primary_rpc` winner lock count: `22`

## Fire Outcomes

- `bundle_sent`: `14`
- `bundle_failed`: `5`
- `skipped`: `3`

- `primary_rpc` outcomes:
  - `bundle_sent`: `14`
  - `bundle_failed`: `5`
  - `skipped`: `3`

## Shortlist Signals

- `shortlist_hit=false`: `60`
- `shortlist_hit=true`: `6`
- `prepared_context_used=false`: `62`
- `prepared_context_used=true`: `4`
- `shortlist_state=armed`: `6`

## Wallet Gaps

- `jupSoLaHXQiZZTSfEWMTRRgpnyFm8f6sZdosWBjx93v` seen `2` times outside wallet coverage

## Notes

- `bundle_sent` measures acceptance by the Jito endpoint, not an observed on-chain liquidation win.
- Observed success still has to be correlated with Airtable or another on-chain verification source.
- This report is meant to be regenerated over time to compare the same funnel across different observation windows.
