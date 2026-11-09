# Merge an infrastructure change

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `gitops.merge` |
| Class | green for routine files only; amber otherwise (the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- An infrastructure pull request has a unanimous review of the agent panel and green CI.

## How

1. Call `gitops.merge` with the pull request. The engine classifies each changed file by `path_classes`.
2. Only routine files (image digests, chart versions) with green CI: green. Any security file: red. Anything else: amber.
3. The rollout goes staging, then the canary cell, then all cells.

## How to check

- The reconciler (GitOps controller) reports every cell in sync with the merged commit.
- No new alert in 30 minutes after each stage.

## How to undo

- A revert pull request, merged with `gitops.merge` again.

## Stop and escalate

- A cell does not reach sync: `release.pause` and diagnose.
