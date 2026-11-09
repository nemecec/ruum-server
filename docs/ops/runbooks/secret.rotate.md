# Rotate a secret

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `secret.rotate` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- The rotation calendar says it is time (green).
- A secret may have leaked: an emergency rotation (amber).

## How

1. Call `secret.rotate` with the secret and `scheduled` (checked against the calendar by the engine).
2. OpenBao writes a new version; the users of the secret reload it; the old version stays readable for 24 hours.
3. Tenant pseudonym keys, vault keys and the policy signing key are red: never rotate them by yourself.

## How to check

- Every user of the secret works with the new version (health checks, no auth errors).
- After 24 hours the old version is revoked, and nothing fails.

## How to undo

- Within 24 hours: pin the old version in OpenBao, through `secret.rotate` with the old version as the target.

## Stop and escalate

- A user of the secret cannot load the new version: pin the old one and file a bug.
