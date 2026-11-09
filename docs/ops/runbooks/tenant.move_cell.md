# Move a tenant to another cell

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `tenant.move_cell` |
| Class | amber (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A cell is too full, a tenant grows past its cell's share, or a large customer gets a dedicated cell (design 14).

## How

1. Call `tenant.move_cell` with the tenant and the target cell. The engine estimates the write downtime from the tenant size; above 15 minutes it is red.
2. The API exports the tenant, imports it into the target cell, checks it, switches the directory entry, and keeps the old copy read-only for 7 days.

## How to check

- The tenant isolation suite passes for that tenant in the target cell.
- Object counts, node counts and journal positions match between the copies.
- The tenant's sign-in, files, databases and apps work (synthetic checks).

## How to undo

- Within 7 days: move it back with `tenant.move_cell`. The old copy is still there.

## Stop and escalate

- Any count does not match: keep the directory entry on the old cell, and open an incident of kind `data`.
