# Move a tenant to another cell, and drop the old copy

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `tenant.move_cell`, `tenant.drop_old_copy` |
| Class | amber (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A cell is too full, a tenant grows past its cell's share, or a large customer gets a dedicated cell (design 14).
- `tenant.drop_old_copy`: 7 days after a move, when the checks of the tenant in the new cell pass.

## How

1. Call `tenant.move_cell` with the tenant and the target cell. The engine estimates the write downtime from the tenant size; above 15 minutes it is red.
2. The API exports the tenant, imports it into the target cell, checks it, switches the directory entry, and keeps the old copy read-only.
3. The engine checks again before each step (export, import, switch). If the class rose in the meantime, the step waits for the new decision.
4. After 7 days: `tenant.drop_old_copy`. It is refused earlier, and red when the checks in the new cell have not passed.

## How to check

- The tenant isolation suite passes for that tenant in the target cell.
- Object counts, node counts and journal positions match between the copies.
- The tenant's sign-in, files, databases and apps work (synthetic checks).

## How to undo

- Before the directory switch: the move stops; the directory still points to the old cell, and nothing is lost.
- After the switch: move it back with `tenant.move_cell`. The move back exports the **current** copy from the new cell, with every write since the move. It is a new move with its own checks.
- Never switch the directory back to the old read-only copy: that discards every write since the move. The engine makes such a request red (`move_back_reuses_old_copy`).
- `tenant.drop_old_copy` has no undo.

## Stop and escalate

- Any count does not match before the switch: keep the directory entry on the old cell, and open an incident of kind `data`.
- Any count does not match after the switch: do not drop the old copy; open an incident of kind `data` and page the person.
