# Prepare a data export

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `gdpr.export` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A signed-in person asks for an export of their own data (GDPR article 15 or 20).

## How

1. Check that the request came from the signed-in account (the engine checks `identity_verified`).
2. Call `gdpr.export` with the request. The export goes only to that account's download page, never by email attachment.
3. A request from someone else (a lawyer, an authority) is a legal request: red.

## How to check

- The export holds every data class of the account (files, versions, metadata, audit log of their own actions).
- The person got a notice that the export is ready.

## How to undo

- The download link ends after 7 days; end it earlier through the same request if the person asks.

## Stop and escalate

- The export would include a team space that the requester does not own: red.
