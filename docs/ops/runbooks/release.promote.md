# Promote a release

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `release.promote` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A release passed every CI gate and the review panel on staging (to the canary cell).
- The automatic canary analysis passed in the canary cell (to all cells).

## How

1. Call `release.promote` with `to_stage: canary` or `to_stage: all_cells`.
2. The rollout goes cell by cell, with a pause between cells for the analysis.
3. During a change freeze set by the person, the policy makes this red; wait.

## How to check

- The canary analysis result is `passed` in each cell before the next one starts.
- No new issue fingerprint appears in the 30 minutes after each cell (design 9.6).
- The release history names the release as current in every promoted cell.

## How to undo

- Call `release.pause`, then `workload.rollback` in each cell that got the release.

## Stop and escalate

- The canary analysis is `inconclusive`: amber, the guard looks at the metrics with you.
- The analysis `failed` or did not run, or the release touches a security path: red.
