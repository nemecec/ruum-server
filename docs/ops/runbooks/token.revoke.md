# Revoke tokens

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `token.revoke` |
| Class | amber (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- Signs of a stolen session or token: sign-ins from a new country and device followed by mass downloads, an agent token used from an unknown client, a token found in a public place.

## How

1. Collect the evidence as a record (audit log entries, the leak location).
2. Call `token.revoke` with the principal and the kinds of tokens. The guard checks the evidence.
3. Revoking a platform admin, or the only owner of a team, is red.
4. The person gets a calm notice: what was revoked, why, and how to sign in again and make new tokens.

## How to check

- The audit log shows the revocation; requests with the old tokens get `401`.
- The principal can sign in again with a passkey or their identity provider.

## How to undo

- Nothing to undo: the person signs in again and makes new tokens.

## Stop and escalate

- Data may have left the account: open a security incident and page the person.
