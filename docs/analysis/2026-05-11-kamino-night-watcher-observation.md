# Kamino Night Watcher Observation - 2026-05-11

## Summary

The single-obligation watcher on `C23Wj2st8ovjfEUPZHMMSCmujbv5kPGep1UWDBLvT9wF` did not produce a successful liquidation during the night of `2026-05-10` to `2026-05-11`.

The interesting part is not the absence of success by itself. The watcher captured a brief state where the raw obligation snapshot was already liquidatable, but by the time the full Kamino refresh and liquidation simulation were executed, the position had already been cleaned up on-chain.

This does not introduce a new root cause. It is a direct confirmation of the existing Jawas thesis: a technically correct reactive liquidator can still lose because the observable liquidation window is too short and the useful state is gone by the time the bot acts.

## Context

- Test obligation: `C23Wj2st8ovjfEUPZHMMSCmujbv5kPGep1UWDBLvT9wF`
- Collateral: `SOL`
- Main debt during the relevant period: `USDC`
- Residual dust debt remained on `ETH`
- Local watcher binary: `liquidate_one --mode rpc`
- Wallet coverage:
  - `USDC` configured in `wallet.toml`
  - residual `ETH` also configured

Recent probe improvements were already in place:

- full reserve refresh before liquidation decision
- correct reserve token program handling
- destination ATA setup
- covered-borrow selection based on the largest wallet-covered borrow rather than the first non-zero borrow

## Observed Sequence

### 1. Before the event

For most of the evening, the watcher saw the obligation as healthy after full refresh, with post-refresh LTV values generally in the `74.0%` to `74.7%` area and threshold at `75.0%`.

The watcher repeatedly ended on:

- `status: StoppedHealthyBeforeSend`
- Kamino error: `ObligationHealthy`

### 2. Raw snapshot became liquidatable

At `2026-05-11T02:35:27Z`, the watcher captured a raw obligation snapshot with:

- `current_ltv = 0.752273`
- `unhealthy_ltv = 0.750000`
- `distance_to_liq = -0.002273`
- `is_liquidatable = true`

This means the obligation crossed the threshold in the raw decoded account state.

### 3. Useful state was already gone by simulation time

Later watcher runs showed a drastically changed obligation:

- only dust `ETH` borrow remained visible
- the main `USDC` debt had disappeared
- deposited `SOL` amount had dropped sharply
- simulated post-refresh LTV fell to near zero:
  - `LTV: 0.000178...`

At that point Kamino again returned:

- `ObligationHealthy`

This is the signature of an obligation that has already been partially or fully processed by another liquidator before our watcher could land a useful liquidation attempt.

## What This Means

This event does **not** suggest that the watcher transaction format is the primary problem anymore.

The watcher is now good enough to show:

- the obligation can cross the threshold
- the useful liquidation window can exist
- another actor can still consume that window before our reaction path finishes

In other words, the bottleneck observed here is timing, not only transaction wiring.

## Practical Conclusion For Jawas

This night observation strengthens the existing conclusion:

1. Reactive liquidation is still structurally late on short-lived opportunities.
2. Correct refresh, correct reserve selection, and correct wallet coverage are necessary but insufficient.
3. The highest-leverage work remains upstream of firing:
   - prepare obligations before threshold crossing
   - maintain ready-to-fire execution context
   - reduce decision work at trigger time
   - prioritize signals that arrive before an observed liquidation is already underway

## What This Report Does Not Claim

This report does not prove that Jawas can never win a reactive race.

It only shows that on this real obligation:

- the watcher was technically aligned enough to observe the state transition
- yet the economically relevant state had already changed by the time the simulated liquidation path mattered

So this is not a new explanation. It is a real-world confirmation of the explanation already suspected.

## Takeaway

The watcher produced exactly the kind of evidence Jawas needed:

- not a successful liquidation
- but a concrete proof that a technically valid reactive path can still arrive after the opportunity has already been consumed

That is useful because it narrows the remaining research question:

> the next gains are less about "can we build the liquidation transaction?" and more about "how do we reach a prepared state before the public liquidation window closes?"
