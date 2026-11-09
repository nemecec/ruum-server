# Page the person

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `incident.page_person` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A security incident with possible data loss (always), a sev1 that lasts more than 30 minutes, the swarm cannot make progress, or a runbook says so.

## How

1. Call `incident.page_person` with the incident and a summary of at most 300 characters: what happened, the impact, what the agents did, what the person must decide.
2. Calm and exact; no jokes (the voice for things at stake).

## How to check

- The notification service reports the page as delivered.

## How to undo

- Nothing to undo. Send a follow-up page when the situation changes.

## Stop and escalate

- More than 6 pages in an hour are batched into one summary every 30 minutes; none is dropped.
