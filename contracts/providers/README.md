# The provider contract

DRAFT for review (step 2 specify phase, `docs/STEP2.md`). Design: `docs/DESIGN.md`, sections 7.1
(the contract), 5.3 (versions of typed nodes) and 8 (databases, the first typed node).

## What a typed node is

The core knows four kinds of node: folder, file, link and **typed**. A typed node is a
placeholder in the tree:

- the **platform** keeps the node: ID, name, parent, grants, tags, trash, journal, and the
  **manifest** (small JSON) as the node's versions;
- a **provider** keeps the real thing behind the manifest: a database, a web app, a repo, a
  drawing.

So sharing, moving, trash and version history work the same for every node, and the provider
only adds what is special about its type.

Databases (provider `ruum.database`, type `database`) are the first typed node. They define the
contract; web apps (step 3), repos (step 4), agent sessions (step 5) and Kadakas drawings
(step 6) use it without changes, or with additive changes only.

## Files

| File | What it is |
|---|---|
| `provider.openapi.yaml` | The contract as OpenAPI 3.1: the endpoints a provider serves (`/provider/v1/...`) and the platform endpoints a provider calls (`/v1/provider-api/...`). Lint: `npx @redocly/cli lint --config contracts/redocly.yaml contracts/providers/provider.openapi.yaml`. |
| `../manifests/database.schema.json` | The manifest of the `database` type (JSON Schema 2020-12). |

## One contract, two forms

- **In process** (first-party providers): the Rust trait `ruum_core::providers::Provider`, with
  one method per operation of `provider.openapi.yaml` and the same request and response types.
  The database provider of step 2 runs in the server process.
- **Over HTTP** (external providers, first Kadakas CAD in step 6): the same operations as HTTP
  calls, signed with a platform token (`platformToken`).

One conformance suite (`crates/ruum-core/src/providers/conformance.rs`) runs against the
in-memory test provider, the database provider and, later, an HTTP adapter.

## Life of a typed node

```
client                platform                                 provider
  | createDatabase       |                                        |
  |--------------------->| 1. JSON Schema check of the manifest   |
  |                      | 2. validateManifest ------------------>|  (pure)
  |                      | 3. one transaction: node (kind typed), |
  |                      |    version (typed_manifest_v1),        |
  |                      |    typed_nodes row (provisioning),     |
  |                      |    journal node.created + version.created
  |<-- 202 provisioning -|                                        |
  |                      | 4. createTypedNode ------------------->|  makes the database
  |                      |<--------------------- 202 / reportStatus (ready | failed)
  | getDatabase          |                                        |
  |--------------------->| status from typed_nodes                |
```

1. **Create.** The platform validates the manifest twice: against the type's JSON Schema, then
   with `validateManifest` for the rules a schema cannot hold (an extension that the cluster has,
   room in the plan). It commits the node, its first manifest version and its `typed_nodes` row
   in one transaction, with the journal entries, and answers `202`. Then it calls
   `createTypedNode`. A typed node always has its `typed_nodes` row (a deferred trigger checks
   it).
2. **Status.** The provider reports with `reportStatus` (or the platform polls
   `getTypedNodeStatus`). States: `provisioning`, `ready`, `updating`, `degraded`, `suspended`,
   `failed`, `deleted`, with a snake_case `reason`. The newest `generation` wins.
3. **Change.** A new manifest version (`updateDatabaseManifest`, or `restoreVersion` of an older
   one) is committed first, then sent with `applyManifest`. Both paths run the same checks:
   the JSON Schema, `validateManifest`, the right the type needs (`manage` for a database) and
   the type's own rules (an agent may not change `data_classes`). The content of a typed node
   changes ONLY through these operations: `putContent` and the upload targets on a typed node
   fail with `409 conflict`. The control loop (section 8.1)
   compares `desired_version` and `observed_version` and calls `applyManifest` again until they
   match or the provider reports `failed`.
4. **Lifecycle hooks** (`deliverHook`), from the journal, in `seq` order per node, with retries
   (table `provider_deliveries`):

   | Hook | When | Provider must |
   |---|---|---|
   | `copied` | `copyNode` copied a typed node (alone or inside a folder) | copy the content to the new node with the `data` that the platform sets. A database copies `full` data ONLY when the actor is a person with `manage` on the source; for every other actor (agents, people with `view` or `edit` on the source) the copy holds `masked` data. A viewer who copies into a folder of their own becomes manager of the copy, so a full copy would give them the secrets they could not read. |
   | `moved`, `renamed` | the node moved or got a new name | usually nothing |
   | `trashed` | the node (or a folder above it) went to the trash | stop serving, keep the content (state `suspended`) |
   | `restored` | the node came back from the trash | serve again |
   | `purged` | the node was deleted for good | delete the content, and the backups after the backup window; retried until it succeeds |
   | `access_changed` | a grant on the node or above it changed, or an agent grant ended | close sessions and connections that lost access (the provider asks `checkAccess`) |
   | `tenant_exported` | the tenant export job | write the export of each node |
   | `tenant_deleted` | the tenant is deleted | delete everything of the tenant |

5. **Export** (`exportTypedNode`). The provider writes the export as a file through the upload
   API, with a one-upload token, so quotas and downloads work as for any file. An export holds
   the content only, never the platform's keys or access objects: a database export leaves out
   the Ruum schemas (`ruum_mask` with the pseudonym key, `ruum_masked`, `ruum_nosecret`), the
   roles and the grants. A restore or import installs the key again from the platform (decision
   0004-29).

## Children of a typed node

A typed node is not a folder. It may have only the children that its `TypeDescriptor.child_types`
lists, and only the type's own operations make them (decision 0004-18). The platform enforces it
for every type, so the app, repo and session nodes of steps 3 to 5 need no new rule:

| Operation | On a typed node |
|---|---|
| `createNode`, `createUpload` (`new_file`) with a typed node as parent | `409 conflict` |
| `moveNode` into a typed node | `409 conflict` |
| `moveNode` of a child that `child_types` lists away from its parent (a database branch) | `409 conflict` |
| `copyNode` into a typed node | `409 conflict` |
| `copyNode` of a database | copies the database only, not its branches (the copy is a new live database) |
| `trashNode`, `purgeNode` of a typed node | its children go along (they are its subtree); each gets its own `trashed` / `purged` hook |

## Versions of a typed node (section 5.3)

The version API is the same for every node, but a typed node has two kinds of version:

| Kind | Where | List | Restore |
|---|---|---|---|
| **Manifest versions** (the configuration history) | the platform's `versions` table, encoding `typed_manifest_v1` | `listVersions` (unchanged step 1 operation) | `restoreVersion` makes a new manifest version with the old content and calls `applyManifest`. It never changes data. |
| **Provider versions** (what the provider keeps) | the provider | `listProviderVersions` | `restoreProviderVersion`, `target` `new_node` or `in_place` |

For a database:

- provider versions are **restore points** (named, or automatic before each write grant,
  migration and in-place restore) and **branches** (child database nodes). Any time inside the
  7-day window can also be restored (`window`).
- `restoreDatabase` (platform API) is the user-facing operation; it calls
  `restoreProviderVersion`. `in_place` first records a restore point of the current state, so a
  restore can be undone.
- the web UI shows one "Version history" panel: manifest versions and restore points on one time
  line.

A repo (step 4) will map provider versions to commits, a web app (step 3) to deployments, a
drawing (step 6) to snapshots.

## Access

The provider never sees a user's token. Every call names the tenant, the node and the principal
pair (`actor`, `on_behalf_of`) of the change. When a provider must decide access itself (an open
editing session, a database connection), it calls `checkAccess`, which answers with the whole
platform rule: the user's right, intersected with the agent's or app's scope, and the type's own
rules. For a database these are the permissions of `contracts/authz/schema.zed`:

| Permission | Meaning | From |
|---|---|---|
| `query_read` | read-only queries | `view` |
| `query_write` | read-write queries | `edit` |
| `change_schema` | settings, migrations, restore, branches with full data | `manage` |
| `read_secret` | sees real `secret` values | `manage`; never an agent |
| `write_live` | an agent writes to a live database (schema.zed `agent_write_live`) | a `write_live` grant, or the node is a branch with `masked` or `schema_only` data |
| `read_personal` | an agent sees real `personal` values (schema.zed `agent_read_personal`) | a `read_personal` grant |

`query_read`, `query_write`, `change_schema` and `read_secret` are checked for the user and
intersected with the agent's scope. `write_live` and `read_personal` are checked for the agent
principal, together with the user's `query_write` or `query_read`, and the grant row must be in
force. For a person, `write_live` equals `query_write` and `read_personal` equals `query_read`.
`secret` values are never shown to agents. People see `secret` values only with `manage`.

## Guests and the host plan

Typed nodes count against the plan of the tenant that holds them. A guest, or an agent of a
guest, may create a typed node (create, branch, restore into a new node, copy) only while the
tenant policy `guests_create_resources` is true (default false). Otherwise the platform answers
`403 permission_denied` before it calls the provider (decision 0002-2).

## Rules for every provider

- Idempotent: the same `Ruum-Delivery-Id` twice does the work once.
- `copied` and `purged` are required hooks. A provider that cannot copy refuses the copy in
  `validateManifest` (the platform then refuses `copyNode` with `409 conflict`).
- A provider never changes the tree, never writes the journal, and never stores grants.
- Data that the provider returns to agents goes through the privacy filter of the platform
  (section 8.5) and is marked untrusted.
- Status messages, errors and `detail` never hold content values (row values for a
  database): every principal with `view` reads them.
- Every write that the provider does on behalf of a principal records `actor` and
  `on_behalf_of` (for a database: the query log, and `application_name` on the connection).
