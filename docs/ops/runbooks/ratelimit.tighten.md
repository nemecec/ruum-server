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
2. The engine compares the limit with the configured value. For a tenant or an app, below 10 % is amber, and below 1 % is red (that is a block of the tenant).
3. An IP range wider than a /24 (IPv4) or a /48 (IPv6), or a range that more than 3 tenants used in the last 24 hours, is amber. The whole platform is amber.
4. More than 2 tightenings of one target in 24 hours, or 20 over all targets: amber.
5. To end it early: `ratelimit.restore`.

## How to check

- The flood's requests get `429`; the latency of other tenants returns to the baseline.
- Normal users of the target still get through (the limit is a brake, not a block).

## How to undo

- `ratelimit.restore`, or wait for `expires_in`.

## Stop and escalate

- The flood moves to other sources: ask the security agent, and consider the CDN's DDoS protection settings (a person).
- Only a near-zero limit stops the flood: that is a block; ask for it as red.
