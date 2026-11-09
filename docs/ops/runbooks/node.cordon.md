# Cordon and drain a node, or uncordon it

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `node.cordon`, `node.uncordon` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A node shows hardware or kernel trouble: disk errors, memory errors, a NIC that drops packets, repeated kubelet restarts.
- Before maintenance of the node, or before `node.replace`.

## How

1. Check that the engine says `quorum_safe` (it refuses otherwise): CloudNativePG keeps a primary and a replica, etcd keeps quorum.
2. Call `node.cordon` with the node and the incident. The API drains with the pod disruption budgets.
3. After the node is healthy again, call `node.uncordon`.

## How to check

- The node shows `SchedulingDisabled` and has no workload pods (only daemon sets).
- All moved pods are ready on other nodes; no database lost its replica for more than 10 minutes.

## How to undo

- `node.uncordon`. Pods do not move back by themselves; the scheduler uses the node for new pods.

## Stop and escalate

- The pool would have fewer than 2 ready nodes, or another node is cordoned already: amber.
- The node stays bad after a reboot by layer 1: `node.replace` (amber).
