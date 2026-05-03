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

The repository also includes analysis tooling to turn raw JSONL traces into dated research reports. This is used to quantify:

- the hunter funnel from signal reception to bundle send
- why many reactive Kamino signals are already too late
- how Helius and QuickNode compare as signal sources
- which repay mints fall outside the current wallet-driven scope

The public value of the project is not "a profitable bot". The value is the instrumentation and the technical analysis of why a straightforward reactive bot is usually late.

## What Does Not Work Well Today

- The hunter is still mostly reactive and therefore frequently second in competitive situations.
- Some execution paths still depend on post-factum transaction reads, which is structurally slower than a pre-computed strategy.
- `bundle_sent` is not equivalent to a confirmed liquidation win. It only means the Jito endpoint accepted the bundle submission.
- Wallet coverage is intentionally narrow, so some observed opportunities are skipped by design.
- Profitability is not demonstrated and should not be assumed.
- RPC quality, propagation delay, and Jito fee strategy dominate outcomes more than code elegance alone.

## Main Hypotheses

- Observation is useful even when liquidation execution is not yet competitive.
- The bottleneck is less "can we send a liquidation transaction?" and more "can we know what to send before the winner is visible on-chain?"
- A wallet-driven shortlist and preloaded state are prerequisites for moving beyond reactive execution.

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
- `AIRTABLE_TOKEN`, `AIRTABLE_BASE_ID`
- `TARGET_PROTOCOL`
- `ENABLE_OBSERVER`, `ENABLE_HUNTER`
- `SOLANA_KEYPAIR_PATH`
- `WALLET_TOML_PATH`
- `JUPITER_BASE_URL`
- `JITO_SEND_MAX_ATTEMPTS`

Recent runtime notes:

- the price oracle is now `Jupiter`-first with a static fallback, to improve research accuracy without introducing heavy infrastructure
- the Jito send path includes a bounded retry on clearly recoverable failures such as congestion or expired blockhash
- hunter traces and signal metrics can be converted into a dated Markdown report for longitudinal comparison

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
cargo run --bin liquidate_one -- <OBLIGATION_PUBKEY> --dry-run
```

## Logging

Runtime logs are standardized with timestamp, source, message, and optional decision/result fields.

Research traces remain file-based:

- `HUNTER_LOG_FILE`: per-event hunter JSONL trace
- `HUNTER_SIGNAL_METRICS_FILE`: signal lock and source comparison metrics
- `LOG_FILE`: raw observer capture

Recommended research loop:

```bash
cargo run --bin jawas
cargo run --bin generate_hunter_report
```

This produces a dated file under `docs/analysis/` that can be compared across runs.

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
- it provides a base for further research without pretending the problem is already solved
