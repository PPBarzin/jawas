# Kamino `liquidate_one` Multi-Market Gap

Date: 2026-05-14

## Summary

A batch of 19 Kamino obligations was tested with:

```bash
SOLANA_KEYPAIR_PATH=secrets/keypair.json cargo run --bin liquidate_one -- <OBLIGATION_PUBKEY> --mode simulate
```

The initial suspicion was: "the probe cannot find the accounts".

That is not what the runs show.

The obligations are fetched successfully from RPC.

The observed split is:

- 14 obligations stop with `StoppedHealthyBeforeSend`
- 5 obligations stop with `SimulationFailed`

For the 5 failed cases, the error is not `AccountNotFound`.

It is:

`AnchorError caused by account: obligation. Error Code: ConstraintHasOne. Error Number: 2001.`

## Why This Report Exists

The current result is easy to misread because `liquidate_one` prints a local snapshot first:

- `current_ltv`
- `unhealthy_ltv`
- `distance_to_liq`
- `is_liquidatable = true`

That local snapshot suggests the obligation is liquidatable.

But the actual on-chain simulation can still:

- reclassify the obligation as healthy after refresh
- or fail before liquidation because one of the accounts passed to Kamino is inconsistent

This report separates those two cases.

## Tested Obligations

```text
2NLJKvuWjrypC6jAmC7DsWgJt9gvyerByhuvHkwJmE6A
4m28oXA7HXzeWdRaGpZVZx6V1QEh3Vvy4MboEMGESdTd
CDp2jpUMeyErfLB93aHJiWvY2T7xwxjVkYvBg11uWGUd
51x5gDE5bqDw9CVemMwgQz2mY9GRSsLAhSS8TSRM9fe8
2fLN7P949P9nsPDGcG29Ajftk643gJjt79K3wVyWR62U
4KQ2h7sAN1mUoTP8SaSREamKfUFFSCyU9fpiyGTyhgo9
9vmGAf16ayAxbhstpdGG1ApGPpibsSzWFd7xMTYtkz51
542bK3eVnkFmy9gKjTpewKnaQW1zfdYcoMQaghTf6u4g
Hm7rwqdj2sDSRiHpBHXFHDQE1kd7xD5TKaHCviiCWEzk
95ZjCewWg5MkbANEkTPtnzPyS3hpRcto5rCwEpyViJ5s
D5nQrRVGHrETWW929UUrYvELLgTq4Mcj7sxdDfUcNXiH
DPFJ1uMgaqWHCjYHy2u6V2M41nnvJy53WcNuEvbtHbfz
ELHWdnfKwubic9C7WT8rjBQLKWFB4N1mwMqUwRf2iFX2
ACwgxv9Wd25FYqqr3aTwiErX6LHYfJGPCnWtncb2X7Mb
9tvcHbHSg9tfF5eNER5qbgrEkfhu7xN6VZh3XeAH8x9P
jbHezHCdgPx75kXbV342P3zNH5ddukWtUoLCSynzBDj
2AfKdvqcZUWfXCUgM7NAiZewYzNy3qe8KP2QTFH1aVfc
2mL1givbaKLUg6PqCR5pmxaijtAjZ8KaL9gV57fKbE2p
H915X8tWgnAcGGQG47SzHp7jVpLy4CYUhd4sqxDX6dCm
```

## Result Table

| Obligation | Status | current_ltv | unhealthy_ltv | distance_to_liq |
|---|---|---:|---:|---:|
| `2NLJKvuWjrypC6jAmC7DsWgJt9gvyerByhuvHkwJmE6A` | `StoppedHealthyBeforeSend` | 0.944386 | 0.900000 | -0.044386 |
| `4m28oXA7HXzeWdRaGpZVZx6V1QEh3Vvy4MboEMGESdTd` | `StoppedHealthyBeforeSend` | 0.936147 | 0.900000 | -0.036147 |
| `CDp2jpUMeyErfLB93aHJiWvY2T7xwxjVkYvBg11uWGUd` | `StoppedHealthyBeforeSend` | 0.643818 | 0.600000 | -0.043818 |
| `51x5gDE5bqDw9CVemMwgQz2mY9GRSsLAhSS8TSRM9fe8` | `StoppedHealthyBeforeSend` | 0.789070 | 0.750000 | -0.039070 |
| `2fLN7P949P9nsPDGcG29Ajftk643gJjt79K3wVyWR62U` | `StoppedHealthyBeforeSend` | 0.610476 | 0.571656 | -0.038819 |
| `4KQ2h7sAN1mUoTP8SaSREamKfUFFSCyU9fpiyGTyhgo9` | `StoppedHealthyBeforeSend` | 0.788626 | 0.750000 | -0.038626 |
| `9vmGAf16ayAxbhstpdGG1ApGPpibsSzWFd7xMTYtkz51` | `StoppedHealthyBeforeSend` | 0.937857 | 0.900000 | -0.037857 |
| `542bK3eVnkFmy9gKjTpewKnaQW1zfdYcoMQaghTf6u4g` | `StoppedHealthyBeforeSend` | 0.950989 | 0.900000 | -0.050989 |
| `Hm7rwqdj2sDSRiHpBHXFHDQE1kd7xD5TKaHCviiCWEzk` | `SimulationFailed` | 0.953666 | 0.600000 | -0.353666 |
| `95ZjCewWg5MkbANEkTPtnzPyS3hpRcto5rCwEpyViJ5s` | `StoppedHealthyBeforeSend` | 0.950918 | 0.900000 | -0.050918 |
| `D5nQrRVGHrETWW929UUrYvELLgTq4Mcj7sxdDfUcNXiH` | `StoppedHealthyBeforeSend` | 0.793283 | 0.750000 | -0.043283 |
| `DPFJ1uMgaqWHCjYHy2u6V2M41nnvJy53WcNuEvbtHbfz` | `SimulationFailed` | 0.640311 | 0.600000 | -0.040311 |
| `ELHWdnfKwubic9C7WT8rjBQLKWFB4N1mwMqUwRf2iFX2` | `SimulationFailed` | 0.976511 | 0.600000 | -0.376511 |
| `ACwgxv9Wd25FYqqr3aTwiErX6LHYfJGPCnWtncb2X7Mb` | `StoppedHealthyBeforeSend` | 0.785934 | 0.750000 | -0.035934 |
| `9tvcHbHSg9tfF5eNER5qbgrEkfhu7xN6VZh3XeAH8x9P` | `StoppedHealthyBeforeSend` | 0.793780 | 0.750000 | -0.043780 |
| `jbHezHCdgPx75kXbV342P3zNH5ddukWtUoLCSynzBDj` | `SimulationFailed` | 0.984415 | 0.600000 | -0.384415 |
| `2AfKdvqcZUWfXCUgM7NAiZewYzNy3qe8KP2QTFH1aVfc` | `StoppedHealthyBeforeSend` | 0.952339 | 0.900000 | -0.052339 |
| `2mL1givbaKLUg6PqCR5pmxaijtAjZ8KaL9gV57fKbE2p` | `SimulationFailed` | 0.968720 | 0.600000 | -0.368720 |
| `H915X8tWgnAcGGQG47SzHp7jVpLy4CYUhd4sqxDX6dCm` | `StoppedHealthyBeforeSend` | 0.950728 | 0.900000 | -0.050728 |

## Important Observation

No tested obligation failed with `AccountNotFound` when the probe was run against real RPC outside the sandbox.

That means:

- the obligation accounts exist
- the probe can fetch them
- the failure is later in the simulation path

## Meaning of `StoppedHealthyBeforeSend`

This status means:

1. the obligation account was fetched
2. the transaction was built
3. `RefreshReserve` and `RefreshObligation` completed
4. Kamino then decided the obligation was healthy or non-liquidatable after refresh

So these are not "missing account" cases.

They are "the obligation looked liquidatable in the local snapshot, but Kamino rejected liquidation after recomputing the refreshed state" cases.

## Meaning of `SimulationFailed`

This status means:

1. the obligation account was fetched
2. the transaction was built
3. the simulation failed before a clean healthy/non-healthy decision

For all 5 failed obligations, the short error shape was:

`InstructionError(4, Custom(2001))`

## Full Error Example

For obligation `ELHWdnfKwubic9C7WT8rjBQLKWFB4N1mwMqUwRf2iFX2`, the simulation logs show:

```text
Program log: Instruction: RefreshObligation
Program log: AnchorError caused by account: obligation. Error Code: ConstraintHasOne. Error Number: 2001. Error Message: A has one constraint was violated.
Program log: Left:
Program log: 7WQeTuLsFrZsgnHW7ddFdNfhfJAViqH4mvcFZPQ5zuQ9
Program log: Right:
Program log: 7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF
```

## Current Hypothesis

The current `liquidate_one` binary hardcodes:

- lending market `7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF`
- market authority `9DrvZvyWh1HuAoZxvYWMvkf2XCzryCpGgHqrMjyDWpmo`

See:

- [src/bin/liquidate_one.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/bin/liquidate_one.rs:25)
- [src/bin/liquidate_one.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/bin/liquidate_one.rs:26)

But `RefreshObligation` is built with that hardcoded market instead of using the market stored inside the obligation:

- [src/bin/liquidate_one.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/bin/liquidate_one.rs:508)
- [src/bin/liquidate_one.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/bin/liquidate_one.rs:601)

The failed `ConstraintHasOne` log suggests:

- the obligation itself expects lending market `7WQeTuLsFrZsgnHW7ddFdNfhfJAViqH4mvcFZPQ5zuQ9`
- the probe passes `7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF`

If this reading is correct, then `liquidate_one` is currently single-market, while the tested obligation set spans more than one Kamino lending market.

## Practical Interpretation

At this stage the evidence supports the following:

- this is not an RPC retrieval problem
- this is not a global "missing account" problem
- part of the tested set belongs to the market hardcoded in `liquidate_one`
- another part appears to belong to a different Kamino market
- for that second group, `RefreshObligation` fails because the passed lending market does not match the obligation's own `lending_market`

## Open Question For Review

Please verify the interpretation of the `ConstraintHasOne` failure:

Is the correct reading that:

- `Left` is the market stored in the obligation account
- `Right` is the market account passed by `liquidate_one`

If yes, the direct fix path is likely:

1. stop hardcoding the lending market in `liquidate_one`
2. read `obligation.lending_market` from the fetched obligation
3. resolve the matching market authority for that market
4. rebuild both `RefreshObligation` and liquidation instructions with that market context

## Bottom Line

The batch did not prove "we cannot find any account".

It proved a more specific and more actionable issue:

- many obligations are found and simulated successfully up to Kamino's healthy check
- five obligations fail because `liquidate_one` appears to pass the wrong lending market to `RefreshObligation`
