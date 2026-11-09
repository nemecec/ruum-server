# Rerun a failed job

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `job.rerun` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A background job failed for a reason that is gone: a timeout, a node lost during the job, a dependency that was down (media thumbnails, search index, webhook delivery, a reconciler run).

## How

1. Read the job's last error. A failure from bad input (a corrupt file, an invalid manifest) will fail again; do not rerun it, file a bug.
2. Call `job.rerun` with the job.

## How to check

- The job's state is `succeeded`.
- Its effect is visible: the thumbnail exists, the index has the item, the webhook receiver answered 2xx.

## How to undo

- Nothing to undo: jobs are idempotent. A rerun gives the same result as one successful run.

## Stop and escalate

- The job failed 3 times in 24 hours: amber, and file a bug with the three errors.
- The job deletes data (GC sweep, trash purge, retention): red. Never rerun a deleting job by yourself.
