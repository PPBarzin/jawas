# Hermes Hybrid Firing v1

Date: 2026-05-13

## Objective

Move from passive Hermes preparation to an experimental **Hermes-first execution** mode.

This v1 does not claim production profitability. It introduces a controlled firing path where:

- Hermes can trigger liquidation attempts directly.
- Reactive Kamino signals remain enabled for observation and correlation.
- In hybrid mode, reactive signals do not fire.

## Runtime Modes

`HERMES_EXECUTION_MODE` controls behavior:

- `prepare`: Hermes prepares shortlist/context only. Reactive path can still fire.
- `hybrid`: Hermes can fire. Reactive path is observe-only.
- `only`: Hermes can fire. Reactive path is observe-only.

## Firing Contract (v1)

A Hermes-originated firing attempt is admissible only if:

1. obligation is currently `Armed`
2. repay mint is wallet-covered
3. a prepared context exists
4. context freshness is within bound
5. feed match count satisfies threshold

Micro-confirmation policy:

- short confirmation window
- persistence re-check at end of window
- if state degraded during window, skip

Current aggressive defaults:

- `HERMES_FIRE_ENABLE=true`
- `HERMES_FIRE_CONFIRMATION_WINDOW_MS=120`
- `HERMES_FIRE_MAX_CONTEXT_AGE_MS=2000`
- `HERMES_FIRE_COOLDOWN_MS=5000`
- `HERMES_FIRE_MIN_FEED_MATCH_COUNT=1`
- `HERMES_FIRE_REQUIRE_PERSISTENCE=true`

Send policy:

- single attempt only
- no retry loop
- cooldown after a Hermes firing attempt

## Observability

The hunter trace must make Hermes firing decisions explicit:

- `hermes_signal_received`
- `hermes_firing_candidate`
- `hermes_firing_skipped`
- `firing`
- `bundle_sent` / `error`
- `reactive_observe_only`

The goal is to distinguish:

- information timing losses
- local policy skips
- send-path failures

without conflating them under the old reactive funnel.

## Scope Boundaries

In scope:

- Kamino only
- Hermes shortlist-derived prepared context
- aggressive experimental hybrid mode

Out of scope:

- autonomous all-market scanning
- profitability claims
- removal of existing reactive instrumentation

## Exit Criteria Toward Hermes-only

Hybrid mode should produce enough evidence to decide an `only` rollout:

1. Hermes-originated attempts are observable and stable.
2. Reactive observation is no longer needed as a firing fallback.
3. False-positive and stale-context behavior are bounded by policy.
