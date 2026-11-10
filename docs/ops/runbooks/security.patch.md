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
2. In production: call `security.patch` with the release and the advisory. The guard checks the advisory and the change. This action rolls out to the canary cell only.
3. The next stage (all cells) is a `release.promote` request, with its canary rules. A release with an irreversible migration or a change of a security path is red.

## How to check

- The scanners no longer report the advisory for the patched images.
- The canary analysis passes in each cell.

## How to undo

- `workload.rollback`. Over a security patch it is amber, and red during an open security incident, because the hole comes back. Prefer a forward fix.

## Stop and escalate

- The patch changes a security path, or its release has an irreversible migration: red.
