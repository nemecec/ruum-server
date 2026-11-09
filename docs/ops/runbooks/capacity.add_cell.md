# Add a cell

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `capacity.add_cell` |
| Class | amber (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- The capacity forecast says that the cells reach their planned size within 30 days (design 14, stage 2).

## How

1. Call `capacity.add_cell` with the region (only `hel1` in this draft) and the size.
2. The API creates the cell from the infrastructure code, runs the conformance and isolation suites against it, and adds it to the directory as `empty`.
3. New tenants go to it only after the suites pass.

## How to check

- The cell's health checks pass; the suites pass; the directory lists it.
- Its fixed monthly cost appears in the cost ledger.

## How to undo

- While the cell has no tenants, a person can remove it (`capacity.remove_cell`, red).

## Stop and escalate

- Any suite fails on the new cell: keep it out of the directory and file a bug.
