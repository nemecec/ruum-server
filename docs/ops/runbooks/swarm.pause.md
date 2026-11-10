# Pause the operations agents

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `swarm.pause` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- The guard sees a sequence of actions that looks wrong, an agent loops, token budgets run out, or a person asks.
- The engine pauses a role by itself when the role sent more than 5 red requests in a day.

## How

1. Call `swarm.pause` with the roles (empty means all) and the reason.
2. Layer 1 keeps running. Alerts still reach the person by the fallback route.
3. A pause of all roles, of the `security` role or of the `guard` role pages the person. Without the guard, amber requests go to the person.

## How to check

- The paused agents take no actions; the operations API refuses their requests.

## How to undo

- `swarm.resume` is red: only a person resumes the swarm.

## Stop and escalate

- Always page the person after a pause of all roles, the security role or the guard role (the engine does it).
