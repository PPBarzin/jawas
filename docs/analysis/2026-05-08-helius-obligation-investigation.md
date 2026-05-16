# Helius Obligation Investigation

Date: 2026-05-08

## Summary

The Kamino obligation `4X58VJW7MRGzZjctKCi5Kg3vFKAVT8UEXVgc2S3drej9` is valid.

This was confirmed from a recent Kamino repayment transaction where the obligation appears directly as the `RefreshObligation` account and as the writable obligation account in the final repayment instruction.

## Verified Facts

- Helius can return the transaction `5b2W6vU2E7dZuZYjxXL1YHagCB83sVErdd14dKQ3BzBs4FPiuZwC82npCqbBqokbwC7UFzbdHMs8anYpeqsRHmcp`
- Helius reports that transaction as finalized at slot `418139008`
- The obligation appears at account index `3` in that transaction
- Helius can return `getAccountInfo` for `4X58VJW7MRGzZjctKCi5Kg3vFKAVT8UEXVgc2S3drej9`
- `solana account` against Helius also returns the account successfully
- The account owner is `KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD`
- The account length is `3344` bytes

## Important Correction

Earlier `AccountNotFound` conclusions were polluted by sandbox network failures.

Inside the sandbox, DNS failures against Helius produced false negatives. Once the same commands were re-run outside the sandbox, Helius returned the obligation account correctly.

So the statement "Helius cannot see this obligation" was wrong.

## What Works Now

- `investigate_helius_account` proves Helius can see both:
  - the recent repayment transaction
  - the current obligation account
- `liquidate_one --mode simulate` can fetch and decode the obligation correctly
- `liquidate_one` stops cleanly with `StoppedHealthyBeforeSend`, which is the expected behavior for this non-liquidatable position

## What Still Fails

`inspect_kamino_obligation` fetches and decodes the obligation correctly, but its refresh simulation fails with:

`invalid transaction: Transaction failed to sanitize accounts offsets correctly`

That means the remaining code issue is not Helius account retrieval. It is in the instruction/account construction used by the refresh simulation path of `inspect_kamino_obligation`.

## Practical Conclusion

Helius is usable for this workflow.

The real findings are:

- Helius current-state RPC works for the obligation when queried outside the sandbox
- the liquidation probe can already use Helius to fetch and gate the obligation
- one auxiliary investigation binary still has an account-layout bug in its refresh simulation builder

## Relevant Commands

```bash
cargo run --bin investigate_helius_account -- 4X58VJW7MRGzZjctKCi5Kg3vFKAVT8UEXVgc2S3drej9 5b2W6vU2E7dZuZYjxXL1YHagCB83sVErdd14dKQ3BzBs4FPiuZwC82npCqbBqokbwC7UFzbdHMs8anYpeqsRHmcp
```

```bash
SOLANA_KEYPAIR_PATH=secrets/keypair.json cargo run --bin liquidate_one -- 4X58VJW7MRGzZjctKCi5Kg3vFKAVT8UEXVgc2S3drej9 --mode simulate
```

```bash
solana account 4X58VJW7MRGzZjctKCi5Kg3vFKAVT8UEXVgc2S3drej9 --url "$OBSERVER_RPC_URL"
```
