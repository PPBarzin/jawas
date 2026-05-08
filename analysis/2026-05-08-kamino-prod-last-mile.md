# Kamino Production Last Mile

Date: 2026-05-08

## Objective

Apply the minimum Kamino hunter changes that increase the probability of a real production liquidation without adding a full simulation step to the hot path.

## What The Probe Already Proved

The liquidation probe reached the real Kamino instruction chain on Helius:

1. refresh active reserves
2. refresh obligation
3. create missing destination ATAs idempotently
4. call `LiquidateObligationAndRedeemReserveCollateralV2`

For multiple candidates, the transaction wiring was valid enough to reach the liquidation instruction itself.

That means the remaining production work is not about broad architecture. It is about the last-mile details of how the hunter builds Kamino accounts in the hot path.

## Two Remaining Production Gaps

### 1. Token Program Assumption

The hunter was still hardcoding the classic SPL token program for:

- destination ATA derivation
- repay source ATA derivation
- the three token-program accounts passed to Kamino liquidation

This is weaker than the probe path, which derives token programs from the actual reserve accounts.

Even if most common assets still use the classic SPL program, the production hunter should not assume that.

### 2. Missing Destination ATAs

The probe explicitly creates destination ATAs idempotently for:

- the withdraw collateral mint
- the withdraw liquidity mint

The production hunter was only deriving those ATA addresses and assuming they already existed.

If a destination ATA is missing, the liquidation can fail before the liquidation logic matters.

## Why ATA Creation Is Tricky In Production

Inline ATA creation improves correctness, but it is not free:

- 2 extra instructions
- extra accounts
- higher CU usage
- larger transaction size

The probe already found Kamino cases where the transaction was close to or above the Solana packet-size limit. So adding ATA setup blindly can make some shots unsendable.

## Applied Production Adaptation

The hunter now does three things:

1. uses reserve-derived token programs for Kamino liquidation account construction
2. can include idempotent destination ATA creation in the Kamino hot path
3. automatically drops those ATA setup instructions for a given attempt if they push the signed transaction above the raw size ceiling

This keeps the production behavior pragmatic:

- prefer the safer account layout
- but do not force oversize transactions that can never land

## New Observable Fields

Kamino firing logs now expose:

- `active_reserve_count`
- `full_refresh_context`
- `tx_size_bytes`
- `ata_setup_instruction_count`
- `ata_setup_dropped_for_size`

These fields should make it much easier to distinguish:

- a technically valid shot with full refresh and ATA setup
- a shot where ATA setup had to be dropped to fit size constraints
- a shot that still never lands despite the account layout being aligned

## Practical Read

If production still does not land a Kamino liquidation after this patch, the likely explanations become narrower:

- candidates are still healthy by liquidation time
- transaction size is still too large even after ATA setup fallback
- the shot is simply late in the Jito race

That is a better place to be than before, because the last-mile account wiring is much closer to the validated probe path.
