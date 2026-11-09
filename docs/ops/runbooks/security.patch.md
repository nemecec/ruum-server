# Roll out a security patch

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `security.patch` |
| Class | amber (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- An advisory affects a component that Ruum runs (an image, Talos, a crate, an npm package), and a patched release passed every CI gate.

## How

1. On staging this is green: roll it out and run the game-day checks.
2. In production: call `security.patch` with the release and the advisory. The guard checks the advisory and the change.
3. The rollout uses the normal stages (canary cell first).

## How to check

- The scanners no longer report the advisory for the patched images.
- The canary analysis passes in each cell.

## How to undo

- `workload.rollback`. Before you roll back, say in the incident that the hole comes back; prefer a forward fix.

## Stop and escalate

- The patch changes a security path: red.
