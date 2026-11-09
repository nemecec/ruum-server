# Replace a server

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `node.replace` |
| Class | amber (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A cordoned node has a hardware fault that Hetzner confirms, or it failed again after a reboot.

## How

1. The node is cordoned and drained (`node.cordon`).
2. Call `node.replace`. The API orders the same server type in the same location from the infrastructure code, installs Talos, joins the cluster, and keeps the old server cordoned for 72 hours.
3. The guard checks the diagnosis and the cost; above the budget it is red.

## How to check

- The new node is `Ready` and gets pods; the storage of local volumes is rebuilt (CloudNativePG replicas resync).
- The old server has no pods and is marked for return after 72 hours.

## How to undo

- Within 72 hours: uncordon the old server and cordon the new one. After that the order cannot be undone; the cost is in the digest.

## Stop and escalate

- The new server also fails: stop, page the person (a provider or batch problem).
