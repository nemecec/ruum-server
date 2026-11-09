# Close an incident

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `incident.close`, `incident.reopen` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- The checks of every runbook used in the incident pass, and the alerts stay clear for 30 minutes.

## How

1. Write the end of the timeline: what happened, what was done, what is still open.
2. Call `incident.close`. The postmortem agent gets the incident for a report (sev1 and sev2).
3. A security incident needs the guard (amber), or a person when data loss or exposure is possible (red).

## How to check

- The register shows it closed; a postmortem task exists for sev1 and sev2.

## How to undo

- `incident.reopen` (green) when the problem comes back within 7 days; it keeps the timeline.

## Stop and escalate

- Not applicable.
