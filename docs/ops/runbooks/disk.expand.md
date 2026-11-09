# Grow a volume

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `disk.expand` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A persistent volume is above 80 % and the forecast says full within 7 days (CloudNativePG data or WAL, NATS, Loki).
- Before growing, check for a cause that grows without limit: WAL that does not archive, a log flood. Fix that first.

## How

1. Call `disk.expand` with the volume and `new_size`. More than double the size is amber; above the budget it is red.
2. The storage class expands online; no restart is needed.

## How to check

- The volume shows the new size in Kubernetes and in the file system.
- Usage is under 70 %, and the disk alert clears.

## How to undo

- Volumes do not shrink. If the growth was not needed, note it in the digest; the capacity agent plans a smaller volume at the next migration.

## Stop and escalate

- Usage keeps rising at the same speed after the expand: a leak. Open an incident of kind `data` or `cost`.
