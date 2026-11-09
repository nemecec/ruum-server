# Pause a rollout

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `release.pause` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- Anything looks wrong during a rollout: a new alert, a canary metric that moves, a support ticket about the new version.
- When in doubt, pause. A pause is always safe.

## How

1. Call `release.pause` with the release and the incident.
2. The cells that have the release keep it; the others keep the old version.

## How to check

- The release history shows the rollout as paused, and no cell changes version.

## How to undo

- Call `release.promote` to continue, after the incident is understood.

## Stop and escalate

- Cells now run two versions for more than 24 hours: ask the release agent to decide (continue or roll back).
