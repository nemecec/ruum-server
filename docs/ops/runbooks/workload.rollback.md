# Roll back a workload

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `workload.rollback` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- The error rate or latency rose clearly after a release, and layer 1 did not roll back by itself.
- A release broke a function that the canary analysis does not measure (found by a check or a support ticket).

## How

1. Find the last known good version in the release history: the version before the release that the timeline points to.
2. Call `workload.rollback` with `to_version` and the incident.
3. Pause the rollout of the bad release in other cells (`release.pause`) if it is still running.
4. The engine checks whether a release between the versions came from `security.patch`. Then the rollback brings the hole back: amber, and red during an open security incident. Prefer a forward fix.

## How to check

- The running version is `to_version` on every pod.
- The error rate and latency return to the baseline of before the release.
- The migration check passes: the old code reads the current schema (the engine makes a rollback over a migration that is not reversible red).

## How to undo

- Promote the rolled-back release again (`release.promote`) when the remediation agent has fixed it, through the normal stages.

## Stop and escalate

- The target is not the previous release, or it is older than 7 days: amber.
- The rollback would undo a security patch: amber; red during a security incident. Say in the incident which advisory comes back.
- A migration between the versions is not reversible: red. Do not try a manual workaround.
