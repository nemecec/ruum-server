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

1. Call `ai_provider.failover` with the primary or the named fallback. The list is in the infrastructure repository (a security path) and holds only providers with a data processing agreement and an EU region. Any other provider is red.
2. When the guard and an acting role then run on the same provider, the failover is amber. While it lasts, every amber request goes to the person, because the guard is not independent.

## How to check

- The operations agents answer health prompts through the new provider.
- Token spend is now counted on the new provider in the cost ledger.
- The guard runs on a different provider from the acting roles, or the person knows that amber requests come to them.

## How to undo

- `ai_provider.failover` back to the primary when it is healthy for 30 minutes.

## Stop and escalate

- Every configured provider fails: layer 1 keeps running; page the person.
