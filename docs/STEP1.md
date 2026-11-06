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

Rules from the contract review (all areas):

- **Users only.** An operation marked `x-ruum-user-only: true` in `openapi.yaml` calls
  `ruum_core::actor::require_user_actor` first and fails with `403 user_required` for agents and
  share-link bearers, even when the agent's user is allowed. The list is
  `ruum_testkit::matrix::USER_ONLY_OPERATIONS`; a test keeps it equal to the contract.
- **Permission checks** use `authz::require(authz, nodes, ctx, node, role)`. The subject is
  `authz::subject_of(ctx)` (the user, also for an agent; or the share link), narrowed by
  `ctx.scope` (`authz::Scope`, for agents, share links and later apps).
- **Atomic writes.** A Postgres store writes the journal with `journal::postgres::append` on its
  own transaction, and relationship changes with `authz::outbox::enqueue` in the same
  transaction; the outbox is applied to SpiceDB after the commit.
- **Tokens.** Ruum access tokens carry `aud` (the API origin) and `typ` (`agent`, `user`,
  `share_link`); the API refuses any other (`bearerAuth` in `openapi.yaml`).

## The areas

### 1. Nodes, tags and search

| | |
|---|---|
| Owns | `crates/ruum-core/src/nodes/`, `crates/ruum-core/src/tags/`, `crates/ruum-core/src/search/`, `crates/ruum-api/src/routes/{nodes,tags,search}.rs` |
| Implements (OpenAPI) | tags `nodes`, `tags`, `search`: `getRoot`, `createNode`, `getNodeByPath`, `getNode`, `listChildren`, `renameNode`, `moveNode`, `copyNode`, `trashNode`, `restoreNode`, `listTrash`, `purgeNode`, `emptyTrash`, `listTags`, `createTag`, `getTag`, `updateTag`, `deleteTag`, `listNodesByTag`, `listNodeTags`, `attachTag`, `detachTag`, `searchNodes` |
| Events | `node.created`, `node.renamed`, `node.moved`, `node.copied`, `node.trashed`, `node.restored`, `node.purged`, `tag.attached`, `tag.detached` |
| Tables | `nodes` (unique live name index), `tags`, `node_tags` |
| Notes | Links of kind `shortcut` and `member`; the target is in the same tenant (also a database trigger); a `member` link needs `share` on the target and then acts like a `view` grant by its creator. Names: NFC, at most 255 characters, unique case-insensitively (`validate_name` returns the NFC name). A copy of a file shares chunks and makes one version (calls the versions area). Trash: members see and restore for `trash_days`, admins until `admin_restore_days`, then purge; on team plans only admins purge by hand. Node responses fill `my_role`. Search uses the trigram index and filters with `Authz::check_bulk`. |

### 2. Blobs, versions and diff

| | |
|---|---|
| Owns | `crates/ruum-core/src/blobs/`, `crates/ruum-core/src/versions/`, `crates/ruum-api/src/routes/{blobs,versions}.rs` |
| Implements (OpenAPI) | tags `uploads`, `versions`: `getChunkingParams`, `createUpload`, `getUpload`, `abortUpload`, `putChunk`, `completeUpload`, `downloadContent` (with `Range`, `HEAD`, `If-None-Match`), `getContentUrl` (signed URLs on the content domain `*.content.ruumapps.net`), `putContent`, `listVersions`, `getVersion`, `restoreVersion`, `setVersionLabel`, `diffVersions`, `listDiffPlugins` |
| Events | `version.created`, `version.restored`, `version.labeled` (and `node.created` for a `new_file` upload) |
| Tables | `versions`, `chunks`, `uploads` |
| Notes | FastCDC with the published parameter sets (`chunking_for`: `DEFAULT_CHUNKING`, `SMALL_CHUNKING` for small and text files); the client names the set (`chunking`) and the server refuses others. Manifests carry a format (`ManifestFormat`, `versions.encoding`). BLAKE3, chunks per tenant at `/<tenant>/<hash-prefix>/<hash>` through `object_store` (Garage on the laptop). Version policy: coalescing, thinning, retention (section 5.3). Diff plugins with the one result model of section 5.4: text, JSON, XML, image and the binary fallback in step 1; the Wasm plugin runtime (`wasmtime`) as far as step 1 needs. Garbage collection of chunks (mark and sweep with a grace period). |

### 3. Authz and sharing

| | |
|---|---|
| Owns | `crates/ruum-core/src/authz/`, `crates/ruum-core/src/sharing/`, `crates/ruum-api/src/routes/sharing.rs` |
| Implements (OpenAPI) | tag `sharing`: `listGrants`, `addGrant`, `updateGrant`, `removeGrant`, `listShareLinks`, `createShareLink`, `revokeShareLink`, `resolveShareLink`, `listSharedWithMe` |
| Contract | `contracts/authz/schema.zed`; the in-memory `Authz` must pass the cases of `contracts/authz/schema.test.yaml` |
| Events | `grant.added`, `grant.updated`, `grant.removed`, `share_link.created`, `share_link.revoked` |
| Tables | `grants`, `share_links`, `share_index`, `authz_outbox` |
| Notes | `authz::require` (with the node ancestry for scope subtrees) and `authz::effective_role` (the subject's right intersected with `ctx.scope`) are used by every other area, so build them first. SpiceDB client with consistency tokens; all writes through the outbox (`authz::outbox`). Grants only add rights (decision 8). Guests and links yes, public files later (decision 9), under the tenant policy (`TenantPolicy`: links, edit links, link expiry, guests). Email grants bind only through the invitation token or a verified email of a trusted provider. Share links have `ShareLinkRole` (never `manage` or `share`). |

### 4. Identity, tenants and agents

| | |
|---|---|
| Owns | `crates/ruum-core/src/identity/`, `crates/ruum-core/src/tenants/`, `crates/ruum-core/src/agents/`, `crates/ruum-api/src/auth.rs`, `crates/ruum-api/src/routes/{auth,tenants,agents}.rs`, `getJwks` in `crates/ruum-api/src/routes/system.rs` |
| Implements (OpenAPI) | tags `auth`, `tenants`, `agents`: `getProtectedResourceMetadata`, `getAuthorizationServerMetadata`, `registerOAuthClient`, `getOAuthClient`, `oauthAuthorize`, `oauthConsent`, `loginStart`, `loginCallback`, `logout`, `getMe`, `listMyIdentities`, `listMySessions`, `endSession`, `listTenants`, `createTenant`, `getTenant`, `updateTenant`, `listMembers`, `updateMember`, `removeMember`, `listInvitations`, `createInvitation`, `revokeInvitation`, `acceptInvitation`, `getLimits`, `listAgents`, `registerAgent`, `getAgent`, `updateAgent`, `disableAgent`, `enableAgent`, `listAgentKeys`, `createAgentKey`, `revokeAgentKey`, `exchangeToken`, `listAgentActivity`, `undoAgent`, `getJwks` |
| Contract | `RequestContext` extractor: session cookie + CSRF, bearer token, `Ruum-Tenant`, `Idempotency-Key` (table `idempotency_keys`) |
| Tables | `tenants`, `journal_counters`, `users`, `identities`, `sessions`, `memberships`, `invitations`, `agents`, `agent_keys`, `oauth_clients`, `agent_oauth_grants`, `oauth_codes`, `idempotency_keys` |
| Notes | Zitadel OIDC code flow with PKCE (passkeys, Google, Microsoft; decision 6). `users.email_verified`: only verified emails are unique and claim invitations. Personal tenant at first sign-in; small teams (decision 7). Tenant policy in `tenants.settings` (`TenantPolicy`); `members_can_share` writes or deletes the team root tuple. Plan limits from configuration (section 4.3, decision 10), including `admin_restore_days`. OAuth 2.1 for MCP clients (decision 3; `agents::oauth`): dynamic registration, authorize, consent screen that creates or binds an agent with its scope, code with PKCE, rotating refresh tokens; Ruum signs the agent tokens. API keys `ruum_agent_...` for scripts and the CLI, SHA-256 only, with expiry; exchange for 15-minute tokens. Tokens: `aud` + `typ` (`AccessClaims`); agent tokens carry no scope, the agent row is read on each request. Agents only owned by a person (decision 13). `removeMember` also removes the member's direct grants, revokes the share links they or their agents made, disables their agents and ends their OAuth grants. Undo (`AgentStore::undo`, `undo_action`) inverts every event type, by time range or by `seqs`. The auth extractor blocks every other endpoint, so build it first. |

### 5. Journal and audit (may join area 1)

| | |
|---|---|
| Owns | `crates/ruum-core/src/journal/`, `crates/ruum-core/src/audit/`, `crates/ruum-api/src/routes/{changes,audit}.rs` |
| Implements (OpenAPI) | tags `changes`, `audit`: `listChanges` (cursor, long poll, `cursor_expired`), `getLatestCursor`, `listAudit` (also NDJSON export) |
| Contract | `contracts/events/journal-entry.schema.json` and all event schemas (every entry must validate) |
| Tables | `journal` (per-tenant `seq` from `ruum_next_seq`), `audit` |
| Notes | Appends go through `journal::postgres::append(&mut PgConnection, ...)` in the caller's transaction (in memory: `MemoryJournal::append`); the `Journal` trait is the read side. Entries carry the node `revision`. The changes feed checks visibility before and after each change and adds per-caller `resync` markers (`ResyncMarker`) when access to a subtree changes; a contract test validates a `/v1/changes` response against `journal-entry.schema.json`. The outbox to NATS comes later. |

### 6. MCP server and CLI

| | |
|---|---|
| Owns | `crates/ruum-mcp/`, `crates/ruum-cli/` |
| Implements | The MCP endpoint `/mcp` (remote, OAuth, `rmcp`), one tool per REST operation of step 1, calling the same core services; the `ruum` CLI over the REST API with an agent key |
| Contract | The MCP tool list is not written yet. The first task of this area is a contract change that adds it (for example `contracts/mcp/tools.yaml`) |
| Notes | MCP clients use OAuth 2.1 with dynamic client registration (decision 3, sections 3.4 and 4.2; the endpoints of area 4): `/mcp` answers `401` with `WWW-Authenticate: Bearer resource_metadata=".../.well-known/oauth-protected-resource"`. The CLI uses an agent API key and token exchange. |

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
| Implements | The small UI of section 12.3 (decision 11): sign-in, account; the file browser (folders, upload and download, move, rename, trash, tags, search by name); the share dialog and Shared with me; version history with diff and restore; agents (create, scope, keys, activity, undo); the OAuth consent screen (`/oauth/consent?request=...`, calls `oauthConsent`); tenant admin (members, invitations, policy, limits, audit log). Downloads and previews use `getContentUrl` (content domain), never the API-origin download. |
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
  grants. This deviates from the text of section 3.3. Confirm it with a decision record, or change
  the schema in a contract change.
- **Tenant selection**: node, tag, upload, agent, change and search calls name the tenant in the
  `Ruum-Tenant` header (paths stay as in the design, `/v1/nodes/...`); tenant administration uses
  `/v1/tenants/{tenant_id}`. The header is optional in the contract (agent and share-link tokens
  are bound to a tenant) and required for users by the server.
- **`users`, `identities`, `sessions`** are global by nature. Their `tenant_id` is the user's
  personal (home) tenant; the RLS policies also let a tenant see its members.
- **`oauth_clients`** is the one table without `tenant_id`: a dynamic registration exists before
  a user picks a tenant and holds no tenant data. Only system lookups can read it. The consent
  bindings (`agent_oauth_grants`) and codes have a tenant.
- **Team root** (decided in the contract review): members get `manager` (edit, move, trash,
  restore), owners and admins `sharer`; members also get `sharer` while the tenant policy
  `members_can_share` is true (the default). No tuple names the creator of a node.
- **Cross-tenant links** are refused in step 1 (database trigger). "Add to my drive" for nodes
  shared from another tenant needs a contract change later (a nullable `link_target_tenant_id`
  and a policy check against the target tenant).
- **Member links and their creator**: a member link acts like a `view` grant by its creator and
  stays when the creator loses `share` (the same as a grant). Showing the member links that point
  at a node to the people who can share it (in `listGrants`) is not in the contract yet.
- **Known gaps, accepted for step 1**: `grantee_kind` has no `group` or `app` yet, and there is
  no `groups` table (SCIM and app bindings need an enum and table change later); `plan` is an
  enum in SQL and OpenAPI although plans are configuration (section 4.3); grant events and
  `listGrants` show grantee emails to everyone with `view` on the node; the `missing` list of
  `createUpload` tells a member whether the tenant already stores a chunk (per tenant only).
- **MCP tool list** and **WebDAV authentication** have no contract yet (areas 6 and 7). They are
  the first task cards of those areas, before any implementation there. The database manifest and
  the provider contract belong to step 2.
- **Migration 0001** was changed in place by the contract review (it was never applied outside
  laptops). Recreate local databases (`scripts/dev.sh clean`) once.
