# Lesson Learned: Variable Naming Must Follow Runtime Role

## Context

Jawas hit a production failure because runtime configuration mixed two separate concerns:

- `observer` connectivity
- `hunter` connectivity and signal routing

The bug was not only a bad endpoint. The deeper issue was that one runtime role could silently borrow the variables of another role.

## What Went Wrong

- `OBSERVER_*` variables were used outside the observer path.
- Hunter signal selection relied on provider-flavored naming instead of role-flavored naming.
- A typo such as `OBSERVER__RPC_URL` could partially break one role while the process still looked superficially configured.
- Boot healthchecks were strong enough to kill the whole process even when the failing endpoint was not on the critical path for the hunter.

## Rule

Environment variables must be named after the runtime role they configure, not after the vendor currently plugged behind that role.

Good examples:

- `OBSERVER_RPC_URL`
- `HUNTER_RPC_URL`
- `HUNTER_SIGNAL_SECONDARY_RPC_URL`
- `SIGNAL_FEED_WS_URL`

Bad examples:

- `QUICKNODE_RPC_URL`
- `HELIUS_RPC_URL`
- `OBSERVER_*` reused to drive hunter internals

## Why

- Providers change more often than runtime roles.
- Role-based names keep the code stable when an endpoint moves from Helius to QuickNode or the reverse.
- The code stays legible because the reader can infer intent directly from the variable name.
- Troubleshooting becomes local: a broken observer endpoint should not require reading hunter code to understand impact.

## Operational Consequence

When a runtime needs more than one endpoint for the same role, the naming should stay role-based and explicit about responsibility:

- primary hunter RPC
- secondary hunter signal RPC
- observer RPC
- price-feed endpoint

This keeps the `.env`, the Rust config layer, and the runtime behavior aligned.
