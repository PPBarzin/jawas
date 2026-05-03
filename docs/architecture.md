# Architecture

## Intent

Jawas is structured as a small clean-architecture Rust project for Solana liquidation research, not as a generic bot framework.

The design goal is simple:

- keep protocol/domain logic readable
- isolate RPC and external APIs behind ports
- make the runtime bootstrap understandable in one pass
- keep experimental binaries separate from the main application flow

## Layers

### `src/domain`

Pure or mostly pure protocol knowledge:

- Kamino and Solend decoding
- token metadata helpers
- profit and opportunity calculations

This is the best area for unit tests because it contains the most stable logic.

### `src/ports`

Small interfaces consumed by the application layer:

- RPC access
- logging
- Jito submission
- Jupiter swap access
- config-backed whitelist access
- oracle pricing

The point is to keep `application` code independent from concrete HTTP/WebSocket implementations.

### `src/infrastructure`

Concrete adapters:

- `helius.rs`: RPC and WebSocket implementation
- `airtable.rs`: research event persistence and whitelist source
- `jito.rs`: bundle submission
- `jupiter.rs`: swap transaction adapter
- `oracle.rs`: lightweight price adapter

These modules contain external integration details and should not own business decisions.

### `src/application`

Operational services:

- `observer.rs`: watches protocol logs and enriches liquidation observations
- `hunter.rs`: reactive liquidation pipeline and replay tooling
- `heartbeat.rs`: periodic liveness events

This layer orchestrates behavior but should avoid leaking transport-specific details into decision logic.

### `src/config`

Environment parsing and runtime configuration.

This keeps startup concerns out of `main.rs` and makes required variables explicit.

### `src/logging`

Shared formatting for runtime logs. The hunter and observer still keep their dedicated JSONL research traces for latency analysis.

### `src/bin`

Standalone research utilities:

- obligation inspectors
- weekly token report generator
- single-shot liquidation experiment

They are intentionally outside the core runtime so they do not pollute the main application path.

## Runtime Flow

### Observer

1. Subscribe to protocol logs over WebSocket.
2. Filter liquidation-like events.
3. Enrich with transaction data where possible.
4. Estimate delay and competing attempts.
5. Persist structured observations.

### Hunter

1. Listen for protocol-specific signals.
2. Try to resolve the opportunity.
3. Apply wallet and configuration constraints.
4. Build a transaction or bundle.
5. Send or dry-run depending on configuration.
6. Emit trace artifacts for analysis.

## Known Architectural Limits

- `hunter.rs` remains a large module and should still be considered the main refactor candidate.
- Observer and hunter remain operationally coupled to the same adapter family even though their responsibilities are separated.
- Airtable is still doing double duty as both logger and whitelist provider.
- Research artifacts are still filesystem-based rather than modeled as a first-class analysis pipeline.

These are acceptable limits for the current goal: public readability without rewriting the project.
