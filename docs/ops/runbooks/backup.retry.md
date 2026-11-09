# Retry a backup

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `backup.retry` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A scheduled backup (base backup, object storage copy to the second site or the second provider) failed for a temporary reason: timeout, provider error, a lost node.

## How

1. Read the error. Credentials or quota errors will fail again: that needs a person or the security agent.
2. Call `backup.retry` with the backup job.

## How to check

- The backup catalogue lists the new backup with its checksum.
- `backup_age` of the target is under the schedule interval again.

## How to undo

- Nothing to undo: a backup adds a copy; retention removes old copies.

## Stop and escalate

- 3 failures in 24 hours: amber, and the backup alert stays on. A backup alert is never silenced (red).
