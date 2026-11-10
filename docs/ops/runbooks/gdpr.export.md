# Prepare a data export, or end it early

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `gdpr.export`, `gdpr.export_revoke` |
| Class | `gdpr.export` amber (until the export path has its own test suite); `gdpr.export_revoke` green (the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- A signed-in person asks for an export of their own data (GDPR article 15 or 20).
- `gdpr.export_revoke`: the person asks to end the download link early, or the export was made for a request that turns out to be wrong.

## How

1. The engine checks that the request came from the signed-in account (`identity_verified`) and from the data subject.
2. Call `gdpr.export` with the request. The guard checks the request. The export goes only to that account's download page, never by e-mail attachment.
3. Red, a person decides:
   - a request from someone else (a lawyer, an authority): a legal request;
   - an export that covers a team space that the requester does not own;
   - an account with a recent security event (a password or e-mail change, a new passkey, a session from a new country, a token revoke, in the last 7 days). After an account takeover, the export is the attacker's first request.
4. More than 20 exports in a day over all agents: red.

## How to check

- The export holds every data class of the account (files, versions, metadata, audit log of their own actions).
- The person got a notice that the export is ready.

## How to undo

- `gdpr.export_revoke` with the request: the link ends and the export file is deleted. The download link also ends by itself after 7 days.

## Stop and escalate

- The account shows signs of a takeover while an export waits: `gdpr.export_revoke`, open a security incident, and consider `token.revoke`.
