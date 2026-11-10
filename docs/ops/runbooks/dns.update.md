# Change a DNS record

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `dns.update` |
| Class | amber (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A Ruum service moves (a new load balancer IP, a CDN change) and the record is not managed by layer 1.

## How

1. Lower the TTL first when the change can wait one TTL. A TTL above 1 hour or below 60 seconds is red.
2. Call `dns.update` with the record and the value.
3. Red: NS, MX and CAA records; mail and verification records (SPF, DKIM, DMARC); every wildcard record; and the records of the sign-in domain, the app domain (apex and wildcard), the API host and the WebDAV host. These send every tenant's traffic.

## How to check

- Public resolvers return the new value after the TTL; the health checks through the name pass.

## How to undo

- `dns.update` with the earlier value (the audit entry holds it).

## Stop and escalate

- Resolvers return different values after twice the TTL: page the person (registrar or DNSSEC problem).
