# Silence an alert

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `alert.silence`, `alert.unsilence` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- An alert fires again and again for an incident that is open and handled, and the noise hides other alerts.

## How

1. Call `alert.silence` with the alert rule, the incident and a duration (at most 2 hours green; up to 24 hours amber).
2. Security, backup and data integrity alerts are red: never silence them.

## How to check

- The silence is listed with its end time and the incident link.

## How to undo

- `alert.unsilence`, or wait for the end time.

## Stop and escalate

- More than 5 silences open at once: amber for the next one; look for the shared cause.
