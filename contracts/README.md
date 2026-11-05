# Contracts

The contracts are the shared ground between people and agents that work in parallel
(`docs/DESIGN.md`, section 18.1). Code follows the contracts, never the other way round.

| Contract | What it is | Checked by |
|---|---|---|
| `openapi.yaml` | The REST API of step 1 (OpenAPI 3.1): paths, request and response bodies, error codes, auth, paging, idempotency. The web UI client (`web/src/api/schema.d.ts`) is generated from it. | `npx @redocly/cli lint --config contracts/redocly.yaml contracts/openapi.yaml`; the router test in `crates/ruum-api` and the error code test in `crates/ruum-core` |
| `redocly.yaml` | Lint rules for `openapi.yaml`, with the reason for each rule that is off. | - |
| `authz/schema.zed` | The SpiceDB schema (design section 3.3): users, groups, tenants, agents, share links, nodes and their roles and permissions. | `zed validate contracts/authz/schema.test.yaml` |
| `authz/schema.test.yaml` | Relationships and assertions that pin the permission rules (inheritance, member links, share links, team roots). The in-memory authz implementation must pass the same cases. | same |
| `events/journal-entry.schema.json` | The envelope of one journal entry (design section 2.5): `tenant_id`, `seq`, `node_id`, `type`, `actor`, `on_behalf_of`, `at`, `data`. Used by the changes feed, agent activity, the outbox and webhooks. | `ajv validate --spec=draft2020` |
| `events/<type>.schema.json` | The `data` of each event type of step 1: `node.created`, `node.renamed`, `node.moved`, `node.copied`, `node.trashed`, `node.restored`, `node.purged`, `version.created`, `version.restored`, `version.labeled`, `tag.attached`, `tag.detached`, `grant.added`, `grant.removed`, `share_link.created`, `share_link.revoked`. | same |
| `events/common.schema.json` | Shared definitions (IDs, times, hashes, roles, principals, grantees). | same |
| `events/examples/` | Example entries that must validate. | same |

The SQL schema in `migrations/` is a contract too, with its own rule: one migration change at a time.

## The rule

**A contract changes only in a contract change**: a separate commit or pull request that changes
only the contract (and the code generated from it), with a short reason, approved by a person.
A feature change never edits a contract as a side effect. If a feature needs a contract change,
stop and ask for one.

- Additive changes (a new optional field, a new endpoint, a new event type) are normal contract
  changes.
- Breaking changes (removing or renaming a field, changing a type, making a field required,
  changing an error code) need an explicit decision. CI will detect them with an OpenAPI diff and a
  schema diff and block them in normal pull requests.
- Clients and types are generated from the contracts (`npm run gen:api` in `web/`), never written by
  hand.

## Conventions

- IDs are UUIDv7 strings. Times are RFC 3339 in UTC.
- Errors: `{ "error": { "code": "snake_case_code", "message": "...", "hint": "..." } }`. The code list
  is the `ErrorCode` schema in `openapi.yaml` and `ErrorCode` in `crates/ruum-core/src/error.rs`.
- Writes accept `Idempotency-Key`. Write responses carry `change` with `actor` and `on_behalf_of`.
- Lists use cursor paging: `cursor`, `limit`, and `{ items, next_cursor }`.
- Tenant-scoped calls name the tenant in the `Ruum-Tenant` header.
