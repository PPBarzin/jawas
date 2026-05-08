# Hunter Analysis Report

## Inputs

- trace file: `docs/specifications/hunter_trace.jsonl`
- metrics file: `docs/specifications/hunter_signal_metrics.jsonl`
- wallet file: `wallet.toml`

## Coverage

- trace entries: `12017`
- signal metric entries: `463`
- trace window: `2026-04-22T22:40:53Z` -> `2026-05-03T05:20:09Z`
- metrics first-seen window: `1776900327207` -> `1777785609400` ms

## Funnel

- `skip`: `8047`
- `ws_received`: `1623`
- `signal_rejected_duplicate`: `742`
- `error`: `545`
- `signal_accepted`: `463`
- `firing`: `362`
- `bundle_sent`: `235`

## Skip Reasons

- `source_obligation_healthy`: `7946`
- `lock_held`: `742`
- `signal_resolution_failed`: `418`
- `bundle_send_failed`: `127`
- `token_not_whitelisted`: `81`
- `wallet_token_zero_cap`: `20`

## Error Hotspots

- `no KLEND liquidate instruction found`: `418`
- `jito rate limit / congestion`: `118`
- `expired blockhash`: `9`

## Healthy Signal Profile

- `helius` healthy skips: `5274`
- `quicknode` healthy skips: `2672`

- `helius` LTV from healthy traces: `count=5260` `avg=0.558897` `min=0.000119` `max=0.919915`
- `quicknode` LTV from healthy traces: `count=2672` `avg=0.533658` `min=0.015414` `max=0.863082`

## Source Race

- `helius` winner lock count: `386`
- `quicknode` winner lock count: `77`

- `helius` lead over next source: `count=289` `avg_ms=49.84` `min_ms=0` `max_ms=607`
- `quicknode` lead over next source: `count=74` `avg_ms=13.31` `min_ms=0` `max_ms=305`

## Fire Outcomes

- `bundle_sent`: `235`
- `bundle_failed`: `127`
- `skipped`: `101`

- `helius` outcomes:
  - `bundle_sent`: `196`
  - `bundle_failed`: `104`
  - `skipped`: `86`
- `quicknode` outcomes:
  - `bundle_sent`: `39`
  - `bundle_failed`: `23`
  - `skipped`: `15`

## Wallet Gaps

- `cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij` seen `21` times outside wallet coverage
- `9zNQRsGLjNKwCUU5Gq5LR8beUCPzQMVMqKAi3SSZh54u` seen `11` times outside wallet coverage
- `6DNSN2BJsaPFdFFc1zP37kkeNe4Usc1Sqkzr9C9vPWcU` seen `10` times outside wallet coverage
- `2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo` seen `7` times outside wallet coverage
- `jtojtomepa8beP8AuQc6eXt5FriJwfFMwQx2v2f9mCL` seen `7` times outside wallet coverage
- `3NZ9JMVBmGAqocybic2c7LQCJScmgsAZ6vQqTDzcqmJh` seen `6` times outside wallet coverage
- `USDSwr9ApdHk5bvJKMjzff41FfuX8bSxdKcR81vTwcA` seen `5` times outside wallet coverage
- `Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh` seen `3` times outside wallet coverage
- `7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs` seen `2` times outside wallet coverage
- `CtzPWv73Sn1dMGVU3ZtLv9yWSyUAanBni19YWDaznnkn` seen `2` times outside wallet coverage
- `XsDoVfqeBukxuZHWhdvWHBhgEHjGNst4MLodqsJHzoB` seen `2` times outside wallet coverage
- `2u1tszSeqZ3qBWF3uNGPFc8TzMk2tdiwknnRMWGWjGWH` seen `1` times outside wallet coverage
- `HzwqbKZw8HxMN6bF2yFZNrht3c2iXXzpKcFu7uBEDKtr` seen `1` times outside wallet coverage
- `Xs8S1uUs1zvS2p7iwtsG3b6fkhpvmwz4GYU3gWAmWHZ` seen `1` times outside wallet coverage
- `jupSoLaHXQiZZTSfEWMTRRgpnyFm8f6sZdosWBjx93v` seen `1` times outside wallet coverage
- `vSoLxydx6akxyMD9XEcPvGYNGq6Nn66oqVb3UkGkei7` seen `1` times outside wallet coverage

## Notes

- `bundle_sent` measures acceptance by the Jito endpoint, not an observed on-chain liquidation win.
- Observed success still has to be correlated with Airtable or another on-chain verification source.
- This report is meant to be regenerated over time to compare the same funnel across different observation windows.
