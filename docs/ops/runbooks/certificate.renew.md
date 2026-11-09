# Renew a certificate

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `certificate.renew` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A certificate expires within 14 days and the automatic renewal (cert-manager, ACME) did not renew it.

## How

1. Read the renewal error (DNS challenge, rate limit, CAA record).
2. Fix a cause that is in policy (for example rerun the challenge job), then call `certificate.renew`.
3. A new CA or a new key type is amber.

## How to check

- The served certificate has the new expiry date on every endpoint.
- The certificate alert clears.

## How to undo

- Nothing to undo: the old certificate stays valid until it expires.

## Stop and escalate

- The error is a CAA record or a DNS zone problem: `dns.update` is red for CAA; page the person.
