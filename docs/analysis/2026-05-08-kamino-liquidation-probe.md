# Kamino Liquidation Probe

Date: 2026-05-08

## Objective

Validate the Kamino liquidation transaction path one link at a time on a single obligation per run.

The immediate goal is not profitability. The goal is to determine whether Jawas can:

1. identify that an obligation is still liquidatable on-chain
2. build a valid liquidation transaction
3. simulate it successfully
4. send it through direct RPC and observe a real on-chain signature
5. compare that result with Jito submission behavior only after RPC is proven

## Why This Probe Exists

Current hunter logs distinguish `firing` and `bundle_sent`, but `bundle_sent` only proves that `sendBundle` returned a `bundle_id`.

It does not prove that:

- the bundle was accepted for inclusion by the leader
- the transaction was valid at execution time
- the transaction landed on-chain
- the transaction failed on-chain after inclusion

Without a unitary probe, it is too easy to misdiagnose the failure as a pure latency problem.

## Validation Chain

The probe follows this strict order:

1. load the target obligation from RPC
2. decode its current metrics
3. stop immediately if it is healthy or non-liquidatable on-chain
4. build the full Kamino liquidation transaction
5. simulate and inspect runtime logs
6. if simulation succeeds, send through direct RPC
7. only after RPC confirmation, compare with Jito API acceptance

This order is intentional. RPC confirmation is the first proof that the transaction path is complete from the wallet to the chain.

## Observable Stop Points

The probe exposes explicit stop statuses:

- `StoppedHealthyBeforeSend`
- `SimulationFailed`
- `RpcSendFailedBeforeSignature`
- `RpcSignatureObtainedNotConfirmed`
- `RpcConfirmed`
- `JitoBundleRejectedApi`
- `JitoBundleAcceptedApi`

These statuses are meant to remove ambiguity about where the chain breaks.

## Operating Procedure

Use one obligation per run.

Recommended workflow:

```bash
cargo run --bin select_kamino_candidates -- 2026-05-08T07-46_export.csv --single-borrow-only
cargo run --bin select_kamino_candidates -- 2026-05-08T07-46_export.csv --liquidatable-only
```

The selector is read-only. It scans a Kamino Risk CSV export, normalizes the obligation public keys, and filters candidates against the currently configured `wallet.toml` coverage so the probe starts from obligations that the wallet can actually repay. It can also re-check the shortlisted obligations against RPC and keep only those that still exist and still decode as liquidatable on-chain.

Then probe one obligation at a time:

```bash
cargo run --bin liquidate_one -- <OBLIGATION_PUBKEY> --mode simulate
cargo run --bin liquidate_one -- <OBLIGATION_PUBKEY> --mode rpc
```

Only after a successful RPC-confirmed case:

```bash
cargo run --bin liquidate_one -- <OBLIGATION_PUBKEY> --mode jito
```

Optional controls:

- `--repay-native <u64>` to inspect liquidation sizing
- `--tip-lamports <u64>` to control the Jito tip amount
- `--cu-limit <u32>` and `--cu-price <u64>` to control compute budget parameters

## What Success Means

For each stage:

- Healthy gate success: the probe correctly refuses to send when the obligation is no longer liquidatable
- Simulation success: the Kamino instruction chain is valid enough to pass local RPC simulation
- RPC success: a real wallet-signed transaction reaches the chain and confirms
- Jito success: only API-level acceptance is proven unless another inclusion check is added later

## Current Limits

- The probe is manual and single-obligation by design
- The selector depends on the freshness of the CSV export; a stale export can still collapse to zero valid candidates once RPC re-checking is applied
- It does not yet poll post-submission Jito bundle status
- It is an execution-path validator, not a production liquidation strategy
