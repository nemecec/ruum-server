# Fail over a database cluster

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `database.failover` |
| Class | amber (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- The primary of a CloudNativePG cluster is unhealthy and the automatic failover of layer 1 did not act (for example a primary that answers health checks but not queries).
- A planned switchover before node maintenance.

## How

1. Check the replication lag of each replica. Choose the replica with zero lag.
2. Call `database.failover` with the cluster, the replica and the incident. The guard checks the lag and the primary's state.
3. If the primary still takes writes and the replica lags, the policy makes it red: do not force it.

## How to check

- The new primary accepts writes; the applications reconnect through the proxy within 1 minute.
- The old primary rejoins as a replica, or is fenced.
- The engine reports a data loss window of 0.

## How to undo

- After the old primary rejoins as a replica with zero lag, `database.failover` back to it (only if there is a reason).

## Stop and escalate

- The engine computes a data loss window above 0: red.
