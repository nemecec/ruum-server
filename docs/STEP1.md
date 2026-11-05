# Step 1 (Core): work areas

This page splits step 1 of `docs/DESIGN.md` (section 16) into work areas for the task cards of the
swarm (section 18.3). Each area owns some folders. A task card of an area may change only the
folders of that area ("May change"), and never the shared files below.

## Shared files (not owned by any area)

Change these only in a contract change or an integrator task:

| Path | What |
|---|---|
| `Cargo.toml` (root), `Cargo.lock` | Workspace and all dependencies. A new dependency needs approval. |
| `contracts/` | OpenAPI, SpiceDB schema, event schemas. Contract changes only. |
| `migrations/` | SQL. One migration task at a time, through the queue. |
| `crates/ruum-core/src/{lib,ids,actor,error,paging,services}.rs` | Common types and the `Services` set. |
| `crates/ruum-api/src/{lib,error,state}.rs`, `crates/ruum-api/src/routes/{mod,tests}.rs` | The main router, the error body, the shared state. |
| `crates/ruum-server/` | Config and wiring of the real implementations. |
| `crates/ruum-testkit/` | The tenant isolation and permission suites (the test harness task, section 18.8). |
| `web/src/api/schema.d.ts` | Generated from `contracts/openapi.yaml` (`npm run gen:api`). |

## How an area is built

Each module in `crates/ruum-core/src/<module>/` has:

- `mod.rs`: the public types and the trait (the module border). Change the signatures only with the
  agreement of the areas that call them.
- `memory.rs`: the in-memory implementation, for unit tests and for running without Docker.
- `postgres.rs` (to add): the real implementation with sqlx runtime queries (no `query!` macros).
- tests: one conformance suite per trait that runs against both implementations.

Each area has one router file `crates/ruum-api/src/routes/<area>.rs`, already merged into the main
router. Every handler there answers `501 not_implemented` until it is built. The handler names are
the `operationId`s in snake_case.

Every area must keep these rules (`CLAUDE.md`): every query filters by `tenant_id`; every write
records `actor` and `on_behalf_of`; every node change appends one journal entry in the same
transaction; the tenant isolation and permission suites pass for every endpoint.

## The areas

### 1. Nodes, tags and search

| | |
|---|---|
| Owns | `crates/ruum-core/src/nodes/`, `crates/ruum-core/src/tags/`, `crates/ruum-core/src/search/`, `crates/ruum-api/src/routes/{nodes,tags,search}.rs` |
| Implements (OpenAPI) | tags `nodes`, `tags`, `search`: `getRoot`, `createNode`, `getNodeByPath`, `getNode`, `listChildren`, `renameNode`, `moveNode`, `copyNode`, `trashNode`, `restoreNode`, `listTrash`, `listTags`, `createTag`, `getTag`, `updateTag`, `deleteTag`, `listNodesByTag`, `listNodeTags`, `attachTag`, `detachTag`, `searchNodes` |
| Events | `node.created`, `node.renamed`, `node.moved`, `node.copied`, `node.trashed`, `node.restored`, `node.purged`, `tag.attached`, `tag.detached` |
| Tables | `nodes` (unique live name index), `tags`, `node_tags` |
| Notes | Links of kind `shortcut` and `member`; a `member` link needs `share` on the target. A copy of a file shares chunks and makes one version (calls the versions area). Trash purge after the plan's trash period. Search uses the trigram index and filters with `Authz::check_bulk`. |

### 2. Blobs, versions and diff

| | |
|---|---|
| Owns | `crates/ruum-core/src/blobs/`, `crates/ruum-core/src/versions/`, `crates/ruum-api/src/routes/{blobs,versions}.rs` |
| Implements (OpenAPI) | tags `uploads`, `versions`: `createUpload`, `getUpload`, `abortUpload`, `putChunk`, `completeUpload`, `downloadContent` (with `Range`, `HEAD`, `If-None-Match`), `putContent`, `listVersions`, `getVersion`, `restoreVersion`, `setVersionLabel`, `diffVersions`, `listDiffPlugins` |
| Events | `version.created`, `version.restored`, `version.labeled` (and `node.created` for a `new_file` upload) |
| Tables | `versions`, `chunks`, `uploads` |
| Notes | FastCDC with `DEFAULT_CHUNKING`, BLAKE3, chunks per tenant at `/<tenant>/<hash-prefix>/<hash>` through `object_store` (Garage on the laptop). Version policy: coalescing, thinning, retention (section 5.3). Diff plugins with the one result model of section 5.4: text, JSON, XML, image and the binary fallback in step 1; the Wasm plugin runtime (`wasmtime`) as far as step 1 needs. Garbage collection of chunks (mark and sweep with a grace period). |

### 3. Authz and sharing

| | |
|---|---|
| Owns | `crates/ruum-core/src/authz/`, `crates/ruum-core/src/sharing/`, `crates/ruum-api/src/routes/sharing.rs` |
| Implements (OpenAPI) | tag `sharing`: `listGrants`, `addGrant`, `removeGrant`, `listShareLinks`, `createShareLink`, `revokeShareLink`, `resolveShareLink`, `listSharedWithMe` |
| Contract | `contracts/authz/schema.zed`; the in-memory `Authz` must pass the cases of `contracts/authz/schema.test.yaml` |
| Events | `grant.added`, `grant.removed`, `share_link.created`, `share_link.revoked` |
| Tables | `grants`, `share_links`, `share_index` |
| Notes | `authz::require` and `authz::effective_role` (user's right intersected with the agent scope) are used by every other area, so build them first. SpiceDB client with consistency tokens. Grants only add rights (decision 8). Guests and links yes, public files later (decision 9). |

### 4. Identity, tenants and agents

| | |
|---|---|
| Owns | `crates/ruum-core/src/identity/`, `crates/ruum-core/src/tenants/`, `crates/ruum-core/src/agents/`, `crates/ruum-api/src/auth.rs`, `crates/ruum-api/src/routes/{auth,tenants,agents}.rs`, `getJwks` in `crates/ruum-api/src/routes/system.rs` |
| Implements (OpenAPI) | tags `auth`, `tenants`, `agents`: `loginStart`, `loginCallback`, `logout`, `getMe`, `listMyIdentities`, `listMySessions`, `endSession`, `listTenants`, `createTenant`, `getTenant`, `updateTenant`, `listMembers`, `updateMember`, `removeMember`, `listInvitations`, `createInvitation`, `revokeInvitation`, `acceptInvitation`, `getLimits`, `listAgents`, `registerAgent`, `getAgent`, `updateAgent`, `disableAgent`, `enableAgent`, `listAgentKeys`, `createAgentKey`, `revokeAgentKey`, `exchangeToken`, `listAgentActivity`, `undoAgent`, `getJwks` |
| Contract | `RequestContext` extractor: session cookie + CSRF, bearer token, `Ruum-Tenant`, `Idempotency-Key` (table `idempotency_keys`) |
| Tables | `tenants`, `journal_counters`, `users`, `identities`, `sessions`, `memberships`, `invitations`, `agents`, `agent_keys`, `idempotency_keys` |
| Notes | Zitadel OIDC code flow with PKCE (passkeys, Google, Microsoft; decision 6). Personal tenant at first sign-in; small teams (decision 7). Plan limits from configuration (section 4.3, decision 10). API keys `ruum_agent_...`, SHA-256 only, with expiry; exchange for 15-minute tokens. Agents only owned by a person (decision 13); a leaving member's agents are disabled. Undo uses the journal and calls the nodes and versions areas. The auth extractor blocks every other endpoint, so build it first. |

### 5. Journal and audit (may join area 1)

| | |
|---|---|
| Owns | `crates/ruum-core/src/journal/`, `crates/ruum-core/src/audit/`, `crates/ruum-api/src/routes/{changes,audit}.rs` |
| Implements (OpenAPI) | tags `changes`, `audit`: `listChanges` (cursor, long poll, `cursor_expired`), `getLatestCursor`, `listAudit` (also NDJSON export) |
| Contract | `contracts/events/journal-entry.schema.json` and all event schemas (every entry must validate) |
| Tables | `journal` (per-tenant `seq` from `ruum_next_seq`), `audit` |
| Notes | `Journal::append` runs in the caller's transaction, so the Postgres implementation needs a transaction handle; agree the signature with areas 1-3 first. The changes feed filters by what the caller can see. The outbox to NATS comes later. |

### 6. MCP server and CLI

| | |
|---|---|
| Owns | `crates/ruum-mcp/`, `crates/ruum-cli/` |
| Implements | The MCP endpoint `/mcp` (remote, OAuth, `rmcp`), one tool per REST operation of step 1, calling the same core services; the `ruum` CLI over the REST API with an agent key |
| Contract | The MCP tool list is not written yet. The first task of this area is a contract change that adds it (for example `contracts/mcp/tools.yaml`) |
| Notes | MCP clients use OAuth with dynamic client registration (section 4.2); in the first version the agent API key and token exchange are the fallback. |

### 7. WebDAV

| | |
|---|---|
| Owns | `crates/ruum-webdav/` |
| Implements | `/dav` (decision 12) with `dav-server`: PROPFIND, GET with ranges, PUT (a new version), MKCOL, MOVE, COPY, DELETE (to the trash), on one tenant's tree |
| Contract | No own contract; it uses the core traits (`NodeStore`, `Transfers`, `VersionStore`) and the same auth (agent token or a per-user app password, to decide in a contract change) |

### 8. Web UI

| | |
|---|---|
| Owns | `web/` except `web/src/api/schema.d.ts` |
| Implements | The small UI of section 12.3 (decision 11): sign-in, account; the file browser (folders, upload and download, move, rename, trash, tags, search by name); the share dialog and Shared with me; version history with diff and restore; agents (create, scope, keys, activity, undo); tenant admin (members, invitations, limits, audit log) |
| Contract | Uses only the generated types of `contracts/openapi.yaml` |

## Order

1. In parallel: the test harness in `ruum-testkit` (isolation and permission suites), the auth
   extractor (area 4), `authz::require` and the in-memory `Authz` (area 3), and `Journal` (area 5).
2. Then everything else in parallel, against the in-memory implementations first.
3. The Postgres, Garage and SpiceDB implementations, each with the conformance suite of its trait.

## Open points for the architect

- **Role `share`**: section 3.3 of the design sketches `permission share = manage`, but section 3.2
  lists `share` as its own role. The schema makes the roles a strict order
  `view < comment < edit < manage < share`, so a `manage` grant can move and trash but not change
  grants. Confirm or change in a contract change.
- **Tenant selection**: node, tag, upload, agent, change and search calls name the tenant in the
  `Ruum-Tenant` header (paths stay as in the design, `/v1/nodes/...`); tenant administration uses
  `/v1/tenants/{tenant_id}`.
- **`users`, `identities`, `sessions`** are global by nature. Their `tenant_id` is the user's
  personal (home) tenant; the RLS policies also let a tenant see its members.
- **Team root**: the schema comments propose that a team tenant's root gives `edit` to
  `tenant#member` and `share` to owners and admins.
- **MCP tool list** and **WebDAV authentication** have no contract yet (areas 6 and 7).
