# Redeliver dead letters

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `queue.redeliver` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- Messages went to a dead-letter subject of NATS JetStream because a consumer was down or a bug is now fixed.

## How

1. Make sure that the cause is gone: the consumer is healthy, or the fix is released.
2. Call `queue.redeliver` with the stream and `max_messages`. Start with 100; more than 1,000 is amber.

## How to check

- The dead-letter count goes down by the number you sent.
- The consumer's error rate stays at its baseline; no message comes back to the dead-letter subject.

## How to undo

- Nothing to undo: consumers are idempotent (idempotency keys). If messages fail again, they return to the dead-letter subject; stop and diagnose.

## Stop and escalate

- Messages fail again after redelivery: stop. Never purge them (red); file a bug with three message IDs.
