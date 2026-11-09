# Restart a workload

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `workload.restart` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- Pods of one workload are stuck: failing readiness, a deadlock seen in traces, a memory leak that grows towards the limit.
- The diagnosis says a restart helps. A restart is not the fix: the incident stays open until the remediation agent has a change or a reason.

## How

1. Open an incident first (`incident.open`) and link the alerts.
2. Call `workload.restart` with `workload` and `reason` (the incident).
3. The operations API does a rolling restart: one pod at a time, each waits for readiness.

## How to check

- Every pod is ready and has a new start time.
- The alert that opened the incident clears within 10 minutes.
- The error rate and latency of the workload are back to the 7-day baseline.

## How to undo

- Nothing to undo: a restart changes no state.
- If it made things worse (for example a cold cache overloads the database), wait; do not restart again. Scale or roll back instead.

## Stop and escalate

- The same workload needed 3 restarts in 30 minutes: the policy raises the fourth to amber. Ask the remediation agent for a fix, not a restart.
- The workload is single-replica stateful, or in a security namespace: amber.
