# Fail over a database cluster

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `database.failover` |
| Class | amber (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A planned switchover before node maintenance.
- The primary of a CloudNativePG cluster is unhealthy and the automatic failover of layer 1 did not act (for example a primary that answers health checks but not queries).

## How

1. Use `mode: switchover` whenever the primary is reachable. CloudNativePG fences the primary, waits until the replica's lag is 0, and then promotes it. The loss window is 0: amber.
2. Call `database.failover` with the cluster, the replica, the mode and the incident. The guard checks the lag and the primary's state.
3. `mode: failover` is red. With a reachable primary it loses writes; with an unreachable primary the loss window cannot be computed. Layer 1 does that failover by itself; ask the person when it did not.

## How to check

- The new primary accepts writes; the applications reconnect through the proxy within 1 minute.
- The old primary rejoins as a replica, or is fenced.
- The engine reports a data loss window of 0.

## How to undo

- After the old primary rejoins as a replica with zero lag, `database.failover` with `mode: switchover` back to it (only if there is a reason).

## Stop and escalate

- The switchover does not finish because the lag does not reach 0: stop and diagnose; do not change to failover.
- The engine computes a data loss window above 0: red.
