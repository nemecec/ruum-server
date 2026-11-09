# Restore a database cluster

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `database.restore` |
| Class | green for an isolated restore into a new cluster; amber in place or with traffic (the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- Data is corrupt or wrongly changed in a platform database (metadata, SpiceDB, Zitadel) or a tenant database host, and the change cannot be undone with the journal or versions.
- For inspection only: restore into a new cluster that gets no traffic (green).

## How

1. Find the target time: the last moment before the damage, from the journal, the audit log and the database logs.
2. Call `database.restore` with `mode: new_cluster` first, and compare the restored data with the current data.
3. Only if in-place is needed: `mode: in_place`. The engine takes a restore point of the current state first, and computes the data loss window from the backup times. A restore to `latest` (a damaged cluster back to its last write) has a window of 0.
4. A loss window above 0, or a target older than 7 days, is red: the person decides.

## How to check

- The restored cluster passes the checks of `backup.restore_test` (schema version, row counts, checksums of sample rows).
- In place: the applications work against it; the journal continues without gaps after the restore time.

## How to undo

- In place: restore to the restore point that the engine took before. New cluster: a person deletes it (red); the runbook asks after 72 hours.

## Stop and escalate

- Tenant data may be lost or exposed: open a security or data incident and page the person.
