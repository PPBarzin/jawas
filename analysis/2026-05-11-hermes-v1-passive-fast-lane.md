# Hermes v1: passive shortlist + reactive fast lane

## Scope

This iteration turns the previous Hermes prototype buried in `hunter.rs` into an explicit subsystem:

- Hermes stays read-only.
- Hermes never fires a liquidation on its own.
- Hermes maintains a bounded Kamino shortlist filtered by wallet-covered repay mints.
- Hermes can arm an obligation on feed updates and provide a lightweight prepared context.
- The hunter still fires only on the normal reactive Kamino signal.

## Implemented behavior

The new module `src/application/hermes_shortlist.rs` now owns:

- reserve -> Pyth feed mapping
- shortlist building from visible Kamino accounts
- explicit per-obligation state: `Warm`, `Armed`, `CoolingDown`
- Hermes feed parsing
- feed-match arming logic
- prepared execution context construction

The shortlist is intentionally bounded and conservative:

- `HERMES_SHORTLIST_SIZE=10`
- `HERMES_REFRESH_SECS=20`
- repay mint must already be covered by `wallet.toml`
- `SIGNAL_FEED_WS_URL` remains primary, with `HERMES_WS_URL` as fallback

## Hunter integration

The hunter now treats Hermes price-feed events as preparation only:

- `PriceFeedPredictedLiquidable` is traced but does not execute a liquidation
- a later reactive Kamino signal can consume the Hermes prepared context
- the execution path records whether the fast lane was actually used

New `hunter_trace` fields added for measurement:

- `hermes_hit`
- `hermes_state`
- `hermes_feed_match_count`
- `hermes_signal_received_at_ms`
- `hermes_to_reactive_delta_ms`
- `prepared_context_source`
- `fast_lane_used`

## Validation

Validated locally:

- `cargo check --bin jawas`
- `cargo test hermes_ --lib`
- `cargo test --lib`

Not validated in this pass:

- live Hermes stream behavior against real Kamino obligations
- measured advance in milliseconds over the reactive signal on production-like traffic
- real reduction of `signal -> bundle_sent` under runtime load

## Current limits

- no Hermes-only trigger
- no transaction pre-signing
- no validator-adjacent architecture change
- no historical backtest harness in this patch

This is a Phase 3 preparation brick, not a claim that Jawas now wins liquidations.
