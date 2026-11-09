# Post a status notice

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `status.publish` |
| Class | amber (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- An incident affects users who can see it (sev1, or sev2 for more than 30 minutes).

## How

1. Write the notice in the calm voice: what does not work, since when, what users can do, when the next update comes. No names of tenants, people or apps (red).
2. Call `status.publish`. The guard checks the text against the incident facts.
3. A security incident is red: the person decides what customers are told.

## How to check

- The status page shows the notice, and its feed has the entry.

## How to undo

- Post a correction with `status.publish`. A notice is never deleted.

## Stop and escalate

- Not applicable.
