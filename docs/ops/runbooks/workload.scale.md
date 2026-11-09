# Scale a workload

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `workload.scale` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- Saturation: CPU or memory above 80 % for 15 minutes, a queue that grows, latency above the SLO because of load.
- Idle capacity found by the capacity and cost agent (scale down).

## How

1. Read the declared min and max of the workload (its manifest in the infrastructure repo).
2. Call `workload.scale` with the new `replicas` or requests and the incident as `reason`.
3. The engine computes the cost change from the cost ledger. Above EUR 100 a month it is amber; above the budget it is red.

## How to check

- The new pods are ready, or the removed pods drained without failed requests.
- Saturation is under 70 % within 15 minutes; the queue shrinks.
- The cost change appears in the cost ledger the next day.

## How to undo

- Call `workload.scale` with the earlier values. The audit entry of the first call holds them.

## Stop and escalate

- The load keeps growing after two scale-ups: open a capacity task for the capacity and cost agent.
- Scaling up does not help: the bottleneck is elsewhere (database, lock, external API). Stop scaling and diagnose.
