# Research Notes

## Project Position

Jawas is a research repository about liquidation timing on Solana.

The central question is not "can the code send a liquidation?" The central question is:

> Why does a reactive liquidation bot still lose even after the basic pipeline works?

## Current Findings

- Observing successful liquidations is straightforward compared with winning them.
- By the time a competitor transaction is visible enough to replay against, the best actors are often already ahead.
- Hot-path RPC reads are expensive in a latency race.
- A wallet-constrained strategy is more honest than pretending the system can liquidate any asset at any time.
- Signal quality and infrastructure placement matter as much as on-chain instruction building.

## Why Observation Matters

The observer mode still creates value:

- it captures real liquidation behavior
- it measures approximate end-to-end delay
- it helps identify low-competition niches
- it provides concrete cases for later proactive experiments

## Why the Hunter Is Not Yet Convincing

- It is still mostly reactive.
- It still has expensive decision points in the firing path.
- It does not yet maintain a rich enough precomputed watchlist to behave like a pre-armed liquidator.
- It is suitable for experimentation, replay, and latency analysis, not for production claims.

## Practical Research Directions

- reduce hot-path RPC reads further
- precompute target obligations and required accounts
- measure source-by-source signal timing with the existing JSONL traces
- validate whether wallet-driven repay token selection is predictive enough to matter
- compare observer-only findings with hunter dry-run results before attempting more aggressive execution changes

## Local Analysis Artifacts

Runtime JSONL traces remain intentionally ignored. The `analysis/` folder, however, may contain tracked Markdown reports when a concrete investigation produces reusable findings or an execution protocol worth preserving.

The archived notes under `docs/specifications/` are kept as design history, not as a promise that the corresponding approach is complete.
