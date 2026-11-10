# Restore a database cluster

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `database.restore` |
| Class | green only for an isolated restore of a platform database into a new cluster; amber for a tenant database, in place, or with traffic (the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- Data is corrupt or wrongly changed in a platform database (metadata, SpiceDB, Zitadel) or a tenant database host, and the change cannot be undone with the journal or versions.
- For inspection only: restore into a new cluster that gets no traffic.

## How

1. Find the target time: the last moment before the damage, from the journal, the audit log and the database logs.
2. Call `database.restore` with `mode: new_cluster` first.
   - A platform database: green. A cluster with tenant data: amber, because the restore is a copy of tenant data.
   - The new cluster runs in the region of the target's cell. Another region is red for tenant data.
   - The cost counts in the daily spend limit. At most 2 restores a day per cell; then amber.
3. Compare the restored data with the current data only through `diagnostics.read`, with the restore's incident as the reason. You get pseudonyms and the hidden-value marker (decision 0005 B-2), never raw values. Nobody gets credentials for the new cluster.
4. Only if in-place is needed: `mode: in_place` (amber). The engine takes a restore point of the current state first, and computes the data loss window from the backup times. A restore to `latest` (a damaged cluster back to its last write) has a window of 0.
5. A loss window above 0, or a target older than 7 days, is red: the person decides.
6. The engine checks the loss window again just before the restore starts. If the newest backup aged or the window grew, the approval is void, and the request is decided again.

## How to check

- The restored cluster passes the checks of `backup.restore_test` (schema version, row counts, checksums of sample rows).
- In place: the applications work against it; the journal continues without gaps after the restore time.

## How to undo

- In place: restore to the restore point that the engine took before. That undo discards every write after the first restore, so its loss window is above 0 and it is **red**. Do not plan on an amber undo.
- New cluster: nothing to undo. The copy expires after 72 hours (lifecycle `restored-copy-expiry`). If an open incident names it as evidence, the lifecycle asks the person.

## Stop and escalate

- Tenant data may be lost or exposed: open a security or data incident and page the person.
