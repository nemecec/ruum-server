# Block an egress destination, or remove the block

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `egress.block`, `egress.unblock` |
| Class | amber (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- An app sends traffic to a known malicious host, scans, or floods a third party through the egress proxy.

## How

1. Call `egress.block` with the app (or tenant), the destination, the evidence and `expires_in` (at most 7 days).
2. A range larger than one /24 or a whole top-level domain is red.
3. To remove it: `egress.unblock` with the block. A block that a person made is red to remove.

## How to check

- The egress proxy log shows refused connections to the destination from that app only.
- Other apps reach the destination as before.

## How to undo

- `egress.unblock`, or wait for `expires_in` (lifecycle `egress-block-expiry`).

## Stop and escalate

- The app keeps trying other destinations: `app.block`.
