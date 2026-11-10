# Rotate a secret, or pin its previous version

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `secret.rotate`, `secret.pin_previous` |
| Class | `secret.rotate` green; `secret.pin_previous` amber (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- The rotation calendar says it is time. The engine reads the calendar (`rotation_due`): green.
- A secret may have leaked: an emergency rotation. The secret is not due, so the engine makes it amber.

## How

1. Call `secret.rotate` with the secret. There is no `scheduled` parameter: the engine decides from the calendar whether the rotation is due.
2. OpenBao writes a new version; the users of the secret reload it; the previous version stays readable for 24 hours. Then the lifecycle `old-secret-version-revoke` revokes it, after its check.
3. Red, never by yourself: every secret that encrypts data at rest (pseudonym keys, vault keys, backup and blob encryption keys) and every root of trust (the policy signing key, CA keys, OpenBao keys, DKIM keys). The engine reads these properties from the secret registry.

## How to check

- Every user of the secret works with the new version (health checks, no auth errors).
- After 24 hours the previous version is revoked, and nothing fails.

## How to undo

- Within 24 hours: `secret.pin_previous` with the secret and the incident. It is amber after a scheduled rotation.
- It is red after an emergency rotation, during an open security incident, and for secrets that encrypt data or are a root of trust. The previous version may be the leaked one; a person decides.
- After 24 hours the previous version is revoked; `secret.pin_previous` is refused. Rotate again instead.

## Stop and escalate

- A user of the secret cannot load the new version after a scheduled rotation: ask for `secret.pin_previous` (amber) and file a bug.
- The same after an emergency rotation: do not pin. Keep the new version, open or update the security incident, and page the person.
