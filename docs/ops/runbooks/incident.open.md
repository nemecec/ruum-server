# Open an incident

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `incident.open` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- An alert, a check or a ticket shows user impact, or a risk of it. When in doubt, open one.

## How

1. Call `incident.open` with the kind, the severity and the alerts.
2. The engine sets the lowest kind from the labels of the linked alerts: an alert labelled `security` makes the incident at least `security`. You cannot lower a kind later (that is red).
3. An incident without linked alerts cannot justify a change: an action that names it as its reason becomes amber. Link the alerts.
4. Severity: sev1 means many users cannot work or data is at risk; sev2 means a feature is down for some; sev3 means degraded.
5. A security incident with possible data loss: also `incident.page_person`.
6. More than 10 incidents in an hour: amber.

## How to check

- The incident register lists it with the kind and the linked alerts; the timeline starts; the on-call agent owns it.

## How to undo

- `incident.close` when it was opened by mistake, with the reason.

## Stop and escalate

- sev1 lasting more than 30 minutes: page the person.
