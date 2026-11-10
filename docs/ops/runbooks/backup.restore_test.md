# Test a restore

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `backup.restore_test` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- On the schedule of layer 1 (at least weekly for each cluster), after a failed automatic test, and for compliance evidence.

## How

1. Call `backup.restore_test` with the backup. The API restores into an isolated scratch cluster in the region of the cell, runs the checks inside it, and removes the scratch cluster (lifecycle `restored-copy-expiry`).
2. No agent reads the rows of the scratch cluster; only the check results come out.
3. The restore counts in the daily spend limit and in the limit of 2 restores a day per cell.

## How to check

- The checks pass: the restore finishes, the schema version matches, row counts are within 1 % of the source at that time, and sample rows have the same checksums.
- The result goes into the compliance evidence with the time it took (the restore time objective).
- The scratch cluster is gone.

## How to undo

- Nothing to undo: the scratch cluster is removed at the end.

## Stop and escalate

- A restore test fails: open an incident of kind `data` (sev2). The backups of that cluster are not trusted until a test passes.
- The scratch cluster is not removed: the lifecycle asks the person; do not delete it by yourself.
