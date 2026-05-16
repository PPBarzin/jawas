# Kamino Liquidatable Gap After Refresh

Date: 2026-05-08

## Problem

The CSV selector and the raw on-chain obligation snapshot can both classify a Kamino obligation as liquidatable while the full Kamino transaction path classifies the same obligation as healthy once all reserves are refreshed.

This creates a dangerous false positive:

- candidate looks liquidatable in the shortlist
- candidate looks liquidatable in the stale RPC snapshot
- full liquidation simulation reaches `LiquidateObligationAndRedeemReserveCollateralV2`
- Kamino rejects the liquidation with `ObligationHealthy`

## What Was Validated

The liquidation probe now reaches the full Kamino path on Helius:

1. fetch obligation
2. decode obligation
3. refresh every active reserve
4. refresh the obligation with all active reserves
5. create missing destination ATAs idempotently
6. invoke `LiquidateObligationAndRedeemReserveCollateralV2`

This means the current blocker is no longer account wiring for the tested cases.

## Candidates Tested

### `4GG2VzoCNGx6gexVQA22ZQMxEgChAqvpt8t9eyjCkx4V`

Raw snapshot before refresh:

- collateral: `573.28 USD`
- debt: `465.62 USD`
- current LTV: `81.22%`
- unhealthy LTV: `81.07%`
- `is_liquidatable = true`

After full reserve refresh inside the transaction:

- Kamino logs a liquidation-time LTV of `0.766400608399972837`
- liquidation fails with `ObligationHealthy`

### `Hk86ioR7YEnTSEogSPigKooUpvb4rxagKAJ9iAMVja8q`

Raw snapshot before refresh:

- collateral: `767.14 USD`
- debt: `338.37 USD`
- current LTV: `44.11%`
- unhealthy LTV: `43.22%`
- `is_liquidatable = true`

After full reserve refresh inside the transaction:

- Kamino logs a liquidation-time LTV of `0.259411451869074233`
- liquidation fails with `ObligationHealthy`

## Interpretation

The shortlist signal is still too weak if it relies on:

- CSV export values
- stale obligation state
- raw `obligation.is_liquidatable()` before reserve refresh

For Kamino, the meaningful gate is closer to:

`full refresh simulation -> liquidation attempt result`

not:

`CSV -> raw snapshot -> liquidatable`

## Next Step

The selector should grow a stricter mode that only keeps obligations that survive:

1. reserve refresh
2. obligation refresh
3. liquidation simulation up to the liquidation instruction

The shortest-path version is to batch-run the existing single-obligation probe in `simulate` mode over the shortlist and only keep candidates that do not fail with `ObligationHealthy`.
