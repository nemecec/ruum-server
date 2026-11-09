# Open an incident

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `incident.open` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- An alert, a check or a ticket shows user impact, or a risk of it. When in doubt, open one: it costs nothing.

## How

1. Call `incident.open` with the kind, the severity and the alerts.
2. Severity: sev1 means many users cannot work or data is at risk; sev2 means a feature is down for some; sev3 means degraded.
3. A security incident with possible data loss: also `incident.page_person`.

## How to check

- The incident register lists it; the timeline starts; the on-call agent owns it.

## How to undo

- `incident.close` when it was opened by mistake, with the reason.

## Stop and escalate

- sev1 lasting more than 30 minutes: page the person.
