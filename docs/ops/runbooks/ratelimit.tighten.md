# Tighten a rate limit, or restore it

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `ratelimit.tighten`, `ratelimit.restore` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A flood from one tenant, app or IP range hurts other tenants (design 19.5: abuse flood).

## How

1. Call `ratelimit.tighten` with the target, the new limit and `expires_in` (at most 24 hours).
2. The whole platform as the target is amber; a limit of 0 for a tenant is red (a block).
3. To end it early: `ratelimit.restore`.

## How to check

- The flood's requests get `429`; the latency of other tenants returns to the baseline.

## How to undo

- `ratelimit.restore`, or wait for `expires_in`.

## Stop and escalate

- The flood moves to other sources: ask the security agent, and consider the CDN's DDoS protection settings (a person).
