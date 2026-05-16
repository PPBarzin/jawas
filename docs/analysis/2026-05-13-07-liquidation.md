# Kamino liquidation seen on 2026-05-13 around 07:00 UTC

Date: 2026-05-13

## Objective

Analyze the Kamino liquidation observed on the morning of 2026-05-13 and determine whether Jawas missed detection, missed preparation, or failed in the final send path.

## Event Window

The relevant event in local artifacts is:

- `2026-05-13T07:00:52Z` in `hunter_trace.jsonl`

This corresponds to:

- `2026-05-13 09:00:52` in `Europe/Paris`

The liquidation signal is tied to:

- transaction signature `3TFRrspGdkkiG2oMt3uacBcTbx7J7JWAV3jXcpt5ZCQMwX3ppSVdBa3wfa9pubuKkeTnu7bdqXSQHdnsj5cM3pP4`
- obligation `Noh1vJ8hPVLYuQvtprygQrKGGjkDTJDesRkE9F3wXkB`
- repay mint `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`

## Observed Sequence

### 1. The hunter did see the liquidation

At `07:00:52Z`, the hunter receives a valid Kamino liquidation log from `primary_rpc`:

- stage `ws_received`
- liquidation amount `50549111`
- log contains `Obligation is liquidated with liquidation bonus`

This means the failure is not a signal-ingest miss.

### 2. The signal was handled on the reactive path only

The signal is then accepted immediately:

- stage `signal_accepted`
- `shortlist_hit=false`
- `prepared_context_used=false`
- `prepared_context_source=reactive_path`

So Jawas had no prepared execution context ready before the event.

This is reinforced by the pre-event shortlist state:

- `07:00:45Z`: `shortlist_refresh` with `active=0`
- only after the liquidation is seen do we get `reason=candidate_observed active=1`

So the obligation entered the shortlist after the external liquidation had already started.

### 3. Jawas did build a shot

Still at `07:00:52Z`, Jawas reaches `firing` with:

- `prep=270 ms`
- `elapsed_ms=484`
- `active_reserve_count=2`
- `full_refresh_context=false`
- `ata_setup_instruction_count=2`

This means the system was operational enough to produce a concrete liquidation attempt.

### 4. First send attempt failed at Jito

At `07:00:53Z`, attempt `1/2` hits:

- `retryable_bundle_send_error`
- `Network congested. Endpoint is globally rate limited.`

This is an external send-path rejection, not a transaction-construction failure.

### 5. Second attempt was blocked locally

The second attempt never reached Jito. It is skipped as:

- `reason=jito_rate_gate_busy`
- `gate_reason=gate_min_interval_budget_exceeded`
- `gate_required_wait_ms=751`
- `gate_wait_budget_ms=150`
- `gate_min_send_interval_ms=1100`

So the retry logic became self-defeating:

- attempt 1 starts a send and gets rate-limited by Jito
- attempt 2 is then blocked by the local minimum-send-interval gate before it can retry

## Diagnosis

This event is best explained as a two-layer failure:

### 1. Primary failure: no informational edge

Jawas reacted to an already-visible liquidation log.

The hunter did not have the obligation pre-armed in shortlist or Hermes context before the event. That means the event was handled as a late reactive shot rather than a prepared shot.

On this event, the system was structurally behind the liquidator who was already executing on-chain.

### 2. Aggravating failure: retry path blocked by local Jito gate

Even after receiving the late signal, the system still produced a transaction attempt. But the send path behaved poorly under congestion:

- first attempt rejected by Jito global rate limiting
- second attempt canceled locally due to the gate configuration

That means the retry budget was not aligned with the gate interval:

- min send interval was too large relative to the retry wait budget
- the second retry path had almost no chance to execute after a first send failure

## Related Signals After The Event

From `07:01Z` to `07:09Z`, several additional Kamino liquidation-related signatures are observed and skipped as:

- `source_obligation_healthy`

Their logged LTVs remain around `0.54` to `0.55`.

This supports the broader pattern already seen in earlier runs:

- external liquidation activity is visible
- but our read path often does not recover a locally actionable obligation state by the time the signal is processed

## Practical Conclusion

This morning's liquidation was not missed because Jawas was offline or blind.

It was lost because:

1. the system saw the event only once the liquidation was already happening
2. the system had no prepared context for that obligation
3. the first Jito send hit congestion
4. the second retry was prevented by Jawas' own rate gate

## Immediate Follow-Up

The next task should probably target both layers explicitly:

1. improve pre-liquidation preparation coverage
2. remove the retry contradiction between Jito congestion handling and the local send gate

In other words:

- preparation is the strategic gap
- retry gating is the tactical last-mile bug
