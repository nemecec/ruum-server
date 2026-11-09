# Switch the AI provider

Status: draft: awaiting the person's approval.

| | |
|---|---|
| Action | `ai_provider.failover` |
| Class | green (base; the rules and limits in [`policy.yaml`](../policy.yaml) can raise it) |
| Owner | the on-call agent unless the policy says otherwise |

This page never overrides the policy. When they differ, the policy wins, and the runbook gets a fix.

## When

- The primary model or provider of the operations agents fails or is slow for 10 minutes.

## How

1. Call `ai_provider.failover` with a provider from the configured list. Any other provider is red.

## How to check

- The operations agents answer health prompts through the new provider.
- Token spend is now counted on the new provider in the cost ledger.

## How to undo

- `ai_provider.failover` back to the primary when it is healthy for 30 minutes.

## Stop and escalate

- Every configured provider fails: layer 1 keeps running; page the person.
