# Replace a server, and return the old one

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `node.replace`, `node.return` |
| Class | amber (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A cordoned node has a hardware fault that Hetzner confirms, or it failed again after a reboot.
- `node.return`: at least 72 hours after `node.replace`, when the new node works.

## How

1. The node is cordoned and drained (`node.cordon`).
2. Call `node.replace`. The API orders the same server type in the same location from the infrastructure code, installs Talos, joins the cluster, and keeps the old server cordoned.
3. The guard checks the diagnosis and the cost; above the budget it is red.
4. After 72 hours: `node.return`. Returning a server destroys its local disks. It is red when the node still holds a volume with the only or newest copy of any data, or when quorum is not safe.

## How to check

- The new node is `Ready` and gets pods; the storage of local volumes is rebuilt (CloudNativePG replicas resync).
- The old server has no pods and no volume with data before `node.return`.

## How to undo

- Before `node.return`: uncordon the old server and cordon the new one. The order cannot be undone; the cost is in the digest.
- `node.return` has no undo.

## Stop and escalate

- The new server also fails: stop, page the person (a provider or batch problem).
- The old server still holds a volume after 72 hours: do not return it; find out why the replicas did not resync.
