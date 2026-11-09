# Contracts

The contracts are the shared ground between people and agents that work in parallel
(`docs/DESIGN.md`, section 18.1). Code follows the contracts, never the other way round.

| Contract | What it is | Checked by |
|---|---|---|
| `openapi.yaml` | The REST API of step 1 (OpenAPI 3.1), and the step 2 database operations (tag `databases`, DRAFT; each with `x-ruum-area`): paths, request and response bodies, error codes, auth, paging, idempotency. The web UI client (`web/src/api/schema.d.ts`) is generated from it. | `npx @redocly/cli lint --config contracts/redocly.yaml contracts/openapi.yaml`; the router test in `crates/ruum-api` and the error code test in `crates/ruum-core` |
| `redocly.yaml` | Lint rules for `openapi.yaml`, with the reason for each rule that is off. | - |
| `authz/schema.zed` | The SpiceDB schema (design section 3.3): users, groups, tenants, agents, share links, nodes and their roles and permissions. | `zed validate contracts/authz/schema.test.yaml` |
| `authz/schema.test.yaml` | Relationships and assertions that pin the permission rules (inheritance, member links, share links, team roots). The in-memory authz implementation must pass the same cases. | same |
| `events/journal-entry.schema.json` | The envelope of one journal entry (design section 2.5): `tenant_id`, `seq`, `node_id`, `revision`, `type`, `actor`, `on_behalf_of`, `at`, `data`. Entries of `GET /v1/changes` and agent activity validate against it (the API may add `actor.display_name`). Used by the changes feed, agent activity, the outbox and webhooks. | `ajv validate --spec=draft2020` |
| `events/<type>.schema.json` | The `data` of each event type of step 1: `node.created`, `node.renamed`, `node.moved`, `node.copied`, `node.trashed`, `node.restored`, `node.purged`, `node.sharing_updated`, `version.created`, `version.restored`, `version.labeled`, `tag.attached`, `tag.detached`, `grant.added`, `grant.updated`, `grant.removed`, `share_link.created`, `share_link.revoked`. | same |
| `events/common.schema.json` | Shared definitions (IDs, times, hashes, roles, principals, grantees). | same |
| `events/examples/` | Example entries that must validate. | same |
| `providers/README.md`, `providers/provider.openapi.yaml` | The provider contract of typed nodes (design section 7.1; step 2, DRAFT): create, manifest validation, status, lifecycle hooks, export, and how the versions of a typed node map to the provider (section 5.3). Databases are the first typed node. | `npx @redocly/cli lint --config contracts/redocly.yaml contracts/providers/provider.openapi.yaml` |
| `manifests/database.schema.json` | The manifest of a `database` node (design section 8; step 2, DRAFT): engine, version, plan, extensions, backups, data classes per column. | `ajv validate --spec=draft2020` |
| `mcp/tools.yaml` | The MCP tool list: one tool per REST operation that makes sense for an agent (name, `operationId`, description, input by JSON pointer), and the operations that are not exposed, with the reason. | `cargo test -p ruum-mcp --test tools_contract` |

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
  Only the OAuth endpoints (`/v1/oauth/register`, `/v1/oauth/token`) use the OAuth error body
  (`OAuthError`), because OAuth clients expect it.
- Operations with `x-ruum-user-only: true` refuse agents and share links (`403 user_required`).
- A field marked `x-ruum-content-trust: untrusted` can hold text that an agent wrote, and other
  agents or people read it (decision 0005 B-3). Readers treat it as data, never as instructions.
  Every new field that an agent can write and others can read gets the mark in the same change;
  `crates/ruum-mcp/tests/content_trust.rs` keeps its list equal to `openapi.yaml`.
- Writes accept `Idempotency-Key`. Write responses carry `change` with `actor` and `on_behalf_of`.
- Lists use cursor paging: `cursor`, `limit`, and `{ items, next_cursor }`.
- Tenant-scoped calls name the tenant in the `Ruum-Tenant` header.
