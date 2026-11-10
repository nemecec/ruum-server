# Merge an infrastructure change

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `gitops.merge` |
| Class | green for routine files only; amber for data and config files; red for everything else (the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- An infrastructure pull request has a unanimous review of the agent panel and green CI.

## How

1. Call `gitops.merge` with the pull request. The engine classifies each changed file by `path_classes.infrastructure`; the most severe file decides.
2. Routine files (image digests, chart versions): green. Data and config files: amber.
3. Red: security files (access, network, secrets, alert rules, audit, the egress proxy, Talos, the operations API, the policy engine, the guard, the kill switch, the agents' allow-lists and provider list), retention and lifecycle settings (a shorter retention deletes backups, logs or messages), and every file that matches no path class.
4. A revert that lowers an image digest that a `security.patch` raised: amber, red during a security incident.
5. The merge rolls out to staging and the canary cell. All cells is a `release.promote` request.

## How to check

- The reconciler (GitOps controller) reports every cell of the stage in sync with the merged commit.
- No new alert in 30 minutes after each stage.

## How to undo

- A revert pull request, merged with `gitops.merge` again. The revert has its own class.

## Stop and escalate

- A cell does not reach sync: `release.pause` and diagnose.
