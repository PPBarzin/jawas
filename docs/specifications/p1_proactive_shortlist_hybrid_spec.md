# P1 Specification: Proactive Shortlist and Hybrid Reactive Firing

## Status

Draft for implementation planning.

Related Airtable task:

- `jawas-task`: `Prioriser l'implementation detection amont + shortlist + logique hybride`

## Objective

Improve Jawas on the first high-priority track without opening a heavy infrastructure project.

The target is not to beat Helius on raw detection latency. The target is to convert a received signal faster and with less hot-path work by preparing a small set of candidate obligations in advance.

This P1 combines three ideas already prioritized in Airtable:

- detect upstream which obligations are near liquidation
- maintain a reduced hot shortlist
- keep the final trigger reactive, but make the preparation proactive

## Frozen Product Decisions

The following decisions are accepted for P1 and should be treated as implementation constraints, not as open design space.

### Decision 1: shortlist size

- max shortlist size: `10` obligations

Reason:

- large enough to avoid a caricaturally narrow experiment
- small enough to remain a true shortlist and preserve quota discipline

### Decision 2: shortlist seed source

- primary seed source: `observer`

Reason:

- the shortlist must be grounded in observed liquidation reality
- P1 should prepare what Jawas actually sees, not a theoretical universe

Operational rule:

- observer proposes candidate obligations
- shortlist filtering decides whether they are retained

### Decision 3: wallet filter

- an obligation is shortlistable only if its `repay mint` is already present in the operational wallet

Reason:

- P1 is optimized for winning the race on immediately playable opportunities
- no swap, no quick borrow, and no other annex transaction should be required to exploit a shortlisted obligation

This is a deliberately rigid v1 choice.

### Decision 4: refresh policy

- refresh the shortlist immediately when a shortlisted obligation is liquidated
- run a safety refresh every `20` seconds

Reason:

- shortlist freshness should react to meaningful market events
- periodic refresh remains a fallback against silent staleness
- this is more quota-efficient than fixed high-frequency polling

Implementation note:

- event-driven refresh should eventually include a small debounce or cooldown to avoid redundant rebuild storms

### Decision 5: prepared context

For each shortlisted obligation, Jawas prepares a light execution context, not a prebuilt signed transaction.

Prepared context includes:

- obligation pubkey
- repay mint
- repay symbol
- wallet eligibility
- repay reserve
- withdraw reserve
- withdraw mint
- distance-to-threshold or equivalent health metric
- last refreshed timestamp
- reason for shortlist inclusion

Prepared context excludes:

- pre-signed transactions
- per-obligation blockhash preparation
- precomputed Jito tip per candidate
- annex transaction planning

Reason:

- the goal is to preheat the decision path, not to overfit a fragile transaction artifact

## Problem Statement

Current Jawas remains mostly reactive.

When a Kamino liquidation-like signal is received, the hunter still spends part of its budget on work that could have been prepared earlier:

- determining whether the obligation is interesting for Jawas
- checking whether the repay asset fits wallet constraints
- resolving or reusing obligation-specific metadata
- deciding whether the signal is worth converting into a firing attempt

This means the system is still late even when the signal is valid.

## Important Clarification

This P1 does **not** aim to improve segments `1` and `2`.

As long as the final trigger still depends on Helius or another comparable reactive signal source, Jawas remains late on:

- `1. Modification du cours -> position liquidable`
- `2. Position liquidable -> tx source sur la blockchain`

P1 is designed to improve the downstream conversion path:

- `6. Nous recevons le signal -> echec de resolution`
- `7. Nous recevons le signal -> firing`
- `8. Firing -> bundle_sent`
- `9. Nous recevons le signal -> bundle_sent`

## Non-Goals

This specification explicitly excludes:

- firing on every slot
- scanning the whole Kamino universe on every slot
- replacing Helius as the main trigger source
- building a validator-adjacent signal stack
- creating a generic multi-protocol watchlist system in v1

## Constraints

### Quota constraint

Naive reevaluation on every slot is not acceptable.

At roughly `400ms` per slot, polling `10` obligations individually at every slot would imply around `64.8M` API calls per month. Even aggressive batching still remains expensive.

P1 must therefore follow this rule:

> reevaluate very little, frequently enough, and incrementally

### Financial constraint

P1 must not rely on firing speculative liquidation transactions every slot.

False firing attempts that land on-chain consume:

- Solana base fees
- priority fees
- potentially Jito tips, depending on how the transaction is structured

### Architectural constraint

The implementation should fit the current project shape:

- keep domain logic in `src/domain`
- orchestration in `src/application`
- RPC/oracle/Airtable details in `src/infrastructure`

P1 should not begin with a large refactor of `hunter.rs`.

## Proposed Product Behavior

### High-level behavior

Jawas maintains a small in-memory shortlist of Kamino obligations judged "close enough" to liquidation to justify preparation work.

For those obligations only, Jawas keeps a precomputed execution context warm:

- obligation pubkey
- repay mint
- wallet eligibility
- reserve metadata needed by the firing path
- current risk score or distance-to-liquidation estimate

When a reactive signal later arrives for one of those obligations, the hunter uses this warm context to skip or reduce avoidable work in the hot path.

### Trigger model

P1 uses a hybrid trigger model:

- proactive path: maintain and refresh a shortlist of candidate obligations
- reactive path: only fire when a real liquidation-like signal is received

This is a preparation strategy, not a fully autonomous pre-liquidation shooter.

## Scope of V1

### In scope

- Kamino only
- shortlist size capped by configuration
- in-memory shortlist state
- limited background refresh
- precomputation of obligation context for shortlisted items
- hot-path consumption of cached context when signal and shortlist meet
- instrumentation to measure whether the shortlist improves conversion

### Out of scope for V1

- Solend proactive shortlist
- persistent database-backed watchlist
- autonomous firing without reactive signal
- global market scan at sub-second cadence
- dynamic tip strategy redesign

## Proposed Design

### 1. Shortlist seeding

The shortlist should be seeded from a reduced candidate universe, not from all obligations.

Initial seed source:

- `observer`

Then apply mandatory shortlist filters:

- repay mint must already be present in the operational wallet
- candidate must remain compatible with Jawas wallet-first execution strategy

Weekly reports or manual research may still be used as secondary support material, but not as the primary seed for P1.

V1 assumes a small universe:

- at most `10` active obligations in the shortlist at a given time

### 2. Candidate states

Each tracked obligation should live in one of a few explicit states:

- `Warm`: worth tracking, but not near immediate action
- `Armed`: close enough to liquidation to justify full preparation
- `CoolingDown`: recently signaled, fired, or invalidated; avoid thrash
- `Dropped`: removed from shortlist

The state machine matters more than the exact threshold values in v1.

### 3. Refresh strategy

V1 should not attempt a true every-slot polling loop.

The preferred refresh order is:

1. event-driven refresh when a shortlisted obligation is liquidated
2. bounded fallback refresh on a coarse cadence
3. explicit refresh on relevant incoming signal when needed in the hot path

Accepted cadence for P1:

- immediate refresh on shortlisted liquidation
- safety refresh every `20s`

This is intentionally much slower than "every slot", but more aligned with real events and more realistic under quota pressure.

### 4. Precomputed context

For an `Armed` obligation, Jawas should prepare a light deterministic execution context before the final signal:

- repay mint and symbol
- wallet coverage and repay feasibility
- reserve pubkeys and cached reserve metadata
- expected withdraw mint / reserve pairing
- last known health metric relevant to filtering
- shortlist inclusion reason
- last refreshed timestamp

The hot path should not recompute this from scratch if the obligation is already armed.

P1 explicitly does not prebuild a signed transaction per obligation.

### 5. Reactive firing rule

Jawas still fires only when a liquidation-like signal is received.

When the signal arrives:

- if the obligation is `Armed`, use warm context first
- if the obligation is unknown, keep the current reactive path
- if the obligation is known but stale, refresh once under strict timeout, then decide

This preserves current behavior while creating a faster lane for prepared obligations.

## Proposed Code Touchpoints

### `src/application/hunter.rs`

Current Kamino flow already has useful concepts:

- `HunterSignalEvent`
- `HunterSignalKind`
- `HunterSignalSource`
- `run_kamino`
- `execute_kamino_opportunity`

P1 should extend this flow rather than replace it.

Likely additions:

- a background shortlist manager
- a watchlist cache owned by the Kamino hunter runtime
- a hot-path branch that consumes prepared candidate context

### New application module

Add a focused module for shortlist behavior, for example:

- `src/application/kamino_shortlist.rs`

Responsibility:

- own candidate state
- refresh candidates
- expose read-only prepared contexts to the hunter

This keeps `hunter.rs` from absorbing even more watchlist-specific logic.

### `src/domain`

Add pure helpers for:

- candidate scoring
- distance-to-liquidation classification
- state transitions and thresholds

These should remain testable without RPC dependencies.

### `src/infrastructure`

Reuse existing RPC/oracle adapters where possible.

P1 should avoid introducing a new provider dependency.

## Configuration

V1 should add explicit config flags instead of hidden constants.

Suggested initial parameters:

- `SHORTLIST_ENABLED`
- `SHORTLIST_MAX_OBLIGATIONS`
- `SHORTLIST_REFRESH_SECS`
- `SHORTLIST_ARMED_REFRESH_SECS`
- `SHORTLIST_DIST_TO_LIQ_THRESHOLD`
- `SHORTLIST_COOLDOWN_SECS`
- `SHORTLIST_ALLOWED_REPAY_MINTS`

Exact names can be adjusted to repo conventions.

## Logging and Observability

P1 is not acceptable without measurement.

Jawas must emit enough data to answer:

- did the obligation belong to the shortlist at signal time?
- was it `Warm`, `Armed`, or stale?
- how much hot-path work was skipped?
- did this reduce time to firing?
- did it improve bundle-sent rate?

Minimum new trace fields:

- `shortlist_hit`
- `shortlist_state`
- `shortlist_age_ms`
- `prepared_context_used`
- `candidate_score`
- `refresh_reason`

The Airtable log layer does not need every internal detail, but JSONL traces should.

## Success Criteria

P1 succeeds only if it produces a measurable operational gain.

Primary success criteria:

- lower rate of `echec de resolution` after signal reception
- lower median elapsed time between signal receipt and `FIRING`
- higher share of prepared obligations reaching `bundle_sent`
- no quota explosion

Guardrail criteria:

- shortlist remains small and understandable
- no significant increase in false firing attempts
- no large degradation of runtime stability

## Failure Modes to Watch

- shortlist too broad, creating churn and noise
- shortlist too narrow, missing relevant opportunities
- prepared context goes stale too often to matter
- refresh cadence still too expensive in practice
- armed candidates consume effort but do not improve downstream conversion
- implementation adds complexity without reducing hot-path latency

## Rollout Plan

### Phase A: instrumentation first

- add trace fields and shortlist hit accounting
- no behavior change yet, or minimal one

### Phase B: passive shortlist

- compute and maintain shortlist
- observe hit rate against real reactive signals
- do not change firing behavior yet beyond logging

### Phase C: prepared-context fast lane

- when signal matches an armed candidate, use cached context
- measure whether this reduces hot-path latency and resolution failures

### Phase D: threshold tuning

- tighten shortlist size
- adjust arm/disarm thresholds
- validate quota cost against observed benefit

## Deliberate Rejection

This specification deliberately rejects the following v1 idea:

> reevaluate every slot and fire a prebuilt transaction continuously

Reason:

- too expensive in API quota
- too expensive in false transaction fees
- too dependent on state churn and blockhash freshness
- mismatched with the current research stage of Jawas

## Open Questions

- what is the smallest candidate universe that still catches meaningful Kamino opportunities?
- should V1 seed from offline reports, live wallet-compatible pairs, or both?
- which health metric should arm a candidate in practice: raw LTV, distance to liquidation, or a custom score?
- can existing `PriceFeedPredictedLiquidable` signals be reused as shortlist refresh hints?
- how much of `execute_kamino_opportunity` can actually be skipped when prepared context is available?

## Implementation Exit Condition

Implementation should start only after the following are agreed:

- shortlist size cap
- initial refresh cadence
- initial arm/disarm threshold
- exact trace fields to add
- concrete definition of "prepared context"

These decisions are now frozen for P1:

- shortlist size cap: `10`
- seed source: `observer`
- wallet filter: repay mint must already be in wallet
- refresh policy: event-driven on shortlisted liquidation + fallback every `20s`
- prepared context: light execution context, no pre-signed transaction

The remaining implementation work should follow these constraints rather than reopen them implicitly in code.
