# Jawas

Reactive liquidation bots on Solana are structurally late.

Jawas is an experimental research project that investigates why.

It is not production-ready financial software.

The repository documents a practical investigation around liquidation bots:

- observing live liquidations on Kamino and Save/Solend
- measuring why reactive signals arrive too late
- understanding the limits of replaying competitors' transactions
- preparing a cleaner base for proactive, pre-armed experiments

## What Jawas Does

Jawas currently exposes two runtime modes:

- `observer`: subscribes to protocol logs, enriches liquidation events, and writes research data
- `hunter`: listens for liquidation signals and attempts a reaction pipeline for Kamino or Solend

For Kamino, the current P1 direction is now explicit in the runtime:

- observed liquidation signals can seed a small proactive shortlist
- only obligations whose `repay mint` is already in the wallet are eligible
- firing can now be driven by Hermes shortlist state in `hybrid/only` runtime modes, with reactive signals kept for observability
- the hunter now refreshes all known active reserves before firing and builds Kamino token accounts from reserve-derived token programs rather than assuming a fixed SPL layout

The repository also includes analysis tooling to turn raw JSONL traces into dated research reports. This is used to quantify:

- the hunter funnel from signal reception to bundle send
- why many reactive Kamino signals are already too late
- how Helius and QuickNode compare as signal sources
- which repay mints fall outside the current wallet-driven scope

The public value of the project is not "a profitable bot". The value is the instrumentation and the technical analysis of why a straightforward reactive bot is usually late.

## What Does Not Work Well Today

- The hunter is still mostly reactive and therefore frequently second in competitive situations.
- Some execution paths still depend on post-factum transaction reads, which is structurally slower than a pre-computed strategy.
- The new shortlist logic does not remove the structural dependence on signal timing from Helius or comparable sources.
- Some Kamino candidates still look liquidatable in a stale snapshot or CSV export and become healthy again once all reserves are refreshed in the actual transaction path.
- `bundle_sent` is not equivalent to a confirmed liquidation win. It only means the Jito endpoint accepted the bundle submission.
- Wallet coverage is intentionally narrow, so some observed opportunities are skipped by design.
- Profitability is not demonstrated and should not be assumed.
- RPC quality, propagation delay, and Jito fee strategy dominate outcomes more than code elegance alone.

## Main Hypotheses

- Observation is useful even when liquidation execution is not yet competitive.
- The bottleneck is less "can we send a liquidation transaction?" and more "can we know what to send before the winner is visible on-chain?"
- A wallet-driven shortlist and preloaded state are prerequisites for moving beyond reactive execution.
- A narrow, wallet-first shortlist is more honest for early race experiments than a broad theoretical universe.

## Repository Layout

```text
src/
  app.rs                 bootstrap and runtime orchestration
  application/           observer, hunter, heartbeat services
  config/                environment-driven runtime configuration
  domain/                protocol parsing and pure business logic
  infrastructure/        Airtable, Helius, Jito, Jupiter, oracle adapters
  logging/               shared runtime log formatting
  ports/                 service interfaces
  bin/                   inspection and analysis binaries
docs/
  architecture.md        layer overview and runtime flows
  research-notes.md      current findings and open questions
  specifications/        archived design notes from the exploration
  reference/             protocol reference material used during research
analysis/
  *.md                   dated local research and validation reports
```

## Requirements

- Rust stable toolchain compatible with edition `2021`
- access to Solana RPC and WebSocket endpoints
- Airtable API credentials if you want the full logging pipeline
- optional Docker if you want containerized runs

## Configuration

Jawas reads configuration from environment variables. The committed example is [`.env.example`](./.env.example) and the wallet template is [`wallet.example.toml`](./wallet.example.toml).

Important variables:

- `OBSERVER_RPC_URL`, `OBSERVER_WS_URL`
- `HUNTER_RPC_URL`, `HUNTER_WS_URL`
- `HUNTER_SIGNAL_SECONDARY_RPC_URL`, `HUNTER_SIGNAL_SECONDARY_WS_URL`
- `ENABLE_HUNTER_SIGNAL_PRIMARY`, `ENABLE_HUNTER_SIGNAL_SECONDARY`
- `ENABLE_HUNTER_SIGNAL_PRICE_FEED`, `SIGNAL_FEED_WS_URL`
- `HERMES_SHORTLIST_SIZE`, `HERMES_REFRESH_SECS`, `HERMES_TRIGGER_BUFFER_BPS`
- `HERMES_ARMED_STALE_MS`, `HERMES_COOLDOWN_MS`
- `HERMES_EXECUTION_MODE`, `HERMES_FIRE_ENABLE`
- `HERMES_FIRE_CONFIRMATION_WINDOW_MS`, `HERMES_FIRE_MAX_CONTEXT_AGE_MS`
- `HERMES_FIRE_COOLDOWN_MS`, `HERMES_FIRE_MIN_FEED_MATCH_COUNT`
- `HERMES_FIRE_REQUIRE_PERSISTENCE`
- `HUNTER_SHORTLIST_ENABLED`, `HUNTER_SHORTLIST_MAX_OBLIGATIONS`
- `HUNTER_SHORTLIST_REFRESH_SECS`, `HUNTER_SHORTLIST_REFRESH_DEBOUNCE_MS`
- `JITO_MIN_SEND_INTERVAL_MS`, `JITO_SEND_WAIT_BUDGET_MS`
- `AIRTABLE_TOKEN`, `AIRTABLE_BASE_ID`
- `TARGET_PROTOCOL`
- `ENABLE_OBSERVER`, `ENABLE_HUNTER`
- `SOLANA_KEYPAIR_PATH`
- `WALLET_TOML_PATH`
- `JUPITER_BASE_URL`
- `JITO_SEND_MAX_ATTEMPTS`

Recent runtime notes:

- the price oracle is now `Jupiter`-first with a static fallback, to improve research accuracy without introducing heavy infrastructure
- the Jito send path includes a bounded retry on clearly recoverable failures such as congestion or expired blockhash, plus a local send gate to avoid self-inflicted bursts
- hunter traces and signal metrics can be converted into a dated Markdown report for longitudinal comparison
- RPC variable names are role-based: observer variables configure the observer, hunter variables configure the hunter, and optional hunter secondary signal variables configure only the hunter comparison path
- the Kamino hunter can maintain a wallet-constrained shortlist seeded by observed liquidation signals, with event-driven refresh plus a safety refresh interval
- Hermes runtime now supports explicit execution modes:
  - `prepare`: shortlist/context preparation only (reactive can still fire)
  - `hybrid`: Hermes may fire from armed shortlist context, reactive stays observe-only
  - `only`: Hermes firing path enabled, reactive remains observation-only
- Hermes hybrid firing is experimental and intentionally aggressive; it is used to learn whether pre-signal execution can outperform purely reactive timing

## Install

```bash
cargo check
```

## Run

Observer-oriented local run:

```bash
ENABLE_OBSERVER=true ENABLE_HUNTER=false cargo run --bin jawas
```

Hunter-oriented local run:

```bash
ENABLE_OBSERVER=false ENABLE_HUNTER=true TARGET_PROTOCOL=KAMINO cargo run --bin jawas
```

Containerized run:

```bash
docker compose up -d --build jawas-kamino
```

## Test

```bash
cargo test
```

Focused tests:

```bash
cargo test observer::tests -- --nocapture
cargo test hunter::tests -- --nocapture
```

## Utility Binaries

```bash
cargo run --bin inspect_kamino_obligation -- <OBLIGATION_PUBKEY>
cargo run --bin inspect_solend_obligation -- <OBLIGATION_PUBKEY>
cargo run --bin generate_weekly_token_report
cargo run --bin generate_hunter_report
cargo run --bin select_kamino_candidates -- 2026-05-08T07-46_export.csv --single-borrow-only
cargo run --bin liquidate_one -- <OBLIGATION_PUBKEY> --dry-run
```

## Kamino Liquidation Probe

`liquidate_one` is an experimental single-obligation probe used to validate the transaction chain one link at a time. It is not a batch liquidator and should be used manually on one candidate obligation per run.

`select_kamino_candidates` is the companion selector used to scan a Kamino Risk CSV export locally, keep only obligations whose borrow tokens are covered by `wallet.toml`, and optionally filter them against RPC so stale or already-closed obligations are discarded before the probe. It never sends a transaction.

Recommended workflow:

1. Scan a CSV export and shortlist candidates that match the current wallet coverage.
2. Pick one obligation and run `simulate` to confirm it is still non-healthy on-chain and inspect runtime logs.
3. If simulation succeeds, run `rpc` to prove that a real transaction can leave the wallet and confirm on-chain.
4. Only after that use `jito` to compare block-engine behavior.

Examples:

```bash
cargo run --bin select_kamino_candidates -- 2026-05-08T07-46_export.csv --single-borrow-only --borrow-symbol USDC
cargo run --bin select_kamino_candidates -- 2026-05-08T07-46_export.csv --liquidatable-only
cargo run --bin liquidate_one -- <OBLIGATION_PUBKEY> --mode simulate
cargo run --bin liquidate_one -- <OBLIGATION_PUBKEY> --mode rpc
cargo run --bin liquidate_one -- <OBLIGATION_PUBKEY> --mode jito
```

Important:

- the probe stops before sending if the obligation is healthy or otherwise non-liquidatable on-chain
- `bundle_id` from `sendBundle` proves Jito API acceptance only, not on-chain inclusion
- the goal is transaction-path validation, not profitable liquidation execution

## Logging

Runtime logs are standardized with timestamp, source, message, and optional decision/result fields.

Research traces remain file-based:

- `HUNTER_LOG_FILE`: per-event hunter JSONL trace
- `HUNTER_SIGNAL_METRICS_FILE`: signal lock and source comparison metrics
- `LOG_FILE`: raw observer capture

The hunter trace now also records shortlist-specific fields such as whether a signal hit the shortlist and whether prepared context was reused.

For Kamino shots, the trace now also exposes execution-shape fields such as:

- `active_reserve_count`
- `full_refresh_context`
- `tx_size_bytes`
- `ata_setup_instruction_count`
- `ata_setup_dropped_for_size`

Recommended research loop:

```bash
cargo run --bin jawas
cargo run --bin generate_hunter_report
```

This produces a dated file under `analysis/` that can be compared across runs.

## Safety and Disclaimers

- This project is experimental research code.
- It is not audited.
- It is not production-ready trading or financial software.
- It may lose money, miss opportunities, or behave incorrectly under real market conditions.
- Never commit real secrets, private key material, or paid RPC credentials.

## Why This Repo Is Still Interesting

Even without proven profitability, Jawas is useful as a documented case study in Solana liquidation latency:

- it separates observation from execution
- it preserves practical tooling for obligation inspection
- it makes the "reactive vs proactive" gap explicit
- it now includes a measured P1 attempt at bridging that gap without pretending the signal problem is already solved
- it provides a base for further research without pretending the problem is already solved
