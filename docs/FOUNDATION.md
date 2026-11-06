# The foundation of step 1

This page says what exists on `main` (the foundation of `docs/DESIGN.md`, section 18.8, tasks
1 to 4, and the merged work areas), how to run it, and what is still missing. The
work areas are in `docs/STEP1.md`.

## What exists

| Part | Where | State |
|---|---|---|
| Contracts | `contracts/` | OpenAPI (93 operations), SpiceDB schema and its tests, event JSON Schemas, MCP tool list. Reviewed once (commit `151a5fd`); architect decisions in `docs/decisions/0001-step1-contract-decisions.md`. See `contracts/README.md`. |
| Migration | `migrations/0001_core.sql` | All step 1 tables, row-level security, the cross-tenant link trigger. Applies to Postgres 17 of the stack. |
| Rust workspace | `crates/` | `ruum-core` (types, traits, in-memory implementations), `ruum-api` (router; operations of areas not merged yet answer `501`), `ruum-server` (binary), `ruum-mcp`, `ruum-webdav`, `ruum-cli` (stubs), `ruum-testkit`. |
| Security harness | `crates/ruum-testkit` | Tenant isolation suite, permission matrix (13 principals x 93 operations), fixtures, contract checks, mutation check. See `docs/TESTING.md`. |
| CI | `.github/workflows/` | `ci.yml`: fmt, clippy, tests, migrations, security suites, cargo-deny, contract lint and diff, web. `nightly.yml`: mutation check, strict suites. |
| Licenses and sources | `deny.toml` | Permissive licenses only; crates.io only; `openssl` banned. |
| Laptop stack | `deploy/dev/`, `scripts/dev.sh` | Postgres 17 (PostGIS, pgvector), Garage, Zitadel, SpiceDB, NATS, OpenBao, Mailpit, Caddy; Forgejo with `--step4`. See `docs/DEVSTACK.md`. |
| Web UI | `web/` | React + TypeScript + Vite scaffold. The API types (`web/src/api/schema.d.ts`) are generated from the contract. |

## How to run it

```sh
cd /Users/neeme/Workspaces/ruum/ruum-server

# The laptop stack
scripts/dev.sh up              # first run makes deploy/dev/.env with random secrets
scripts/dev.sh status          # all services must be healthy
eval "$(scripts/dev.sh env)"   # DATABASE_URL, RUUM_* and the service settings

# The server (runs the migrations at start; RUUM_MIGRATE=false turns that off)
cargo run -p ruum-server       # http://127.0.0.1:8490, and https://ruum.localhost through Caddy

# The checks
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace                                   # pending (501) checks pass
RUUM_REQUIRE_IMPLEMENTED=1 cargo test -p ruum-testkit    # fails and lists every pending operation
cargo deny check

# The web UI
cd web && npm ci && npm run gen:api && npm run lint && npm run build
```

`scripts/dev.sh ca` exports the Caddy root certificate; `docs/DEVSTACK.md` says how to trust it.

## Ports

All ports bind to `127.0.0.1`. The stack never uses 5433, 8080, 8787 or 8788 (they belong to
`kadakas-server`).

| Port | Service |
|---|---|
| 80, 443 | Caddy: `https://ruum.localhost`, `https://*.ruumapps.localhost` |
| 8490 | The Ruum server on the host (`RUUM_BIND`). Caddy proxies `https://ruum.localhost` to it. |
| 5173 | Vite dev server (`npm run dev`); it proxies the API to 8490 |
| 5434 | Postgres 17 |
| 3900, 3903 | Garage S3 API, Garage admin API |
| 8180 | Zitadel |
| 50051, 8543 | SpiceDB gRPC, SpiceDB HTTP |
| 4222, 8222 | NATS client, NATS monitoring |
| 8200 | OpenBao (dev mode) |
| 8025, 1025 | Mailpit web UI, SMTP |
| 3001, 2222 | Forgejo HTTP and SSH (only with `--step4`) |

## Wave A (merged 2026-11-06)

These branches are merged into `main`: `swarm/authz`, `swarm/journal`, `swarm/identity`,
`swarm/webui`.

| Area | What works now |
|---|---|
| 3. Authz and sharing | `authz::require`, `authz::effective_role`, `authz::subject_of`; the in-memory engine and the SpiceDB engine (HTTP API, writes the contract schema at start); the outbox (`authz::outbox::enqueue` in the write transaction, `apply_after_commit`, `run_worker`). All `sharing` operations, in memory and Postgres, with one conformance suite. `MemorySharingStore` is the member cleanup of the in-memory tenant store. |
| 5. Journal and audit | `journal::postgres::append` (caller's transaction) and `MemoryJournal::append`; the read side, the changes feed (visibility, resync markers, long poll, `cursor_expired`); `listChanges`, `getLatestCursor`, `listAudit` (JSON and NDJSON). Memory and Postgres audit log. |
| 4. Identity, tenants and agents | The `RequestContext` extractor (bearer token, or session cookie with CSRF; `Ruum-Tenant`; `Idempotency-Key` layer) and `Principal`, `MaybePrincipal`, `tenant_context`, `JsonBody`, `QueryParams`, `Paging` in `ruum_api::auth`. Core auth for MCP and WebDAV in `ruum_core::identity::auth` (`authenticate_bearer`, `authenticate_mcp_bearer`, `context_for`). Ed25519 token issuer and JWKS, Zitadel sign-in (PKCE), tenants, members, invitations (SMTP), agents, API keys and key exchange, undo, OAuth 2.1 for MCP clients, app passwords. Memory and Postgres stores with conformance suites. |
| 8. Web UI | All step 1 pages against the typed client, a mock API (MSW, `npm run dev`), component tests (Vitest). |
| Testkit | `fixtures::seed` makes real users (sign-in), the team tenant, members and guests (invitations), user tokens, a web session with CSRF, an app password, the OAuth client and consent request. Node, version, tag, grant, link and agent steps call the other stores; while the nodes area is not merged they keep placeholder IDs. |

How wave B uses it:

- Handlers take `RequestContext(ctx)` (one tenant, from `Ruum-Tenant` or the bound token) or
  `Principal(who)` plus `tenant_context` for `/v1/tenants/{id}/...` paths and header-free
  operations. User-only operations call `ruum_core::actor::require_user_actor(&ctx)` first.
- Permission checks: `authz::require(&*services.authz, &*services.nodes, &ctx, node, Permission::...)`.
  `require` reads `NodeStore::ancestors` for scoped principals, so the nodes area must build it.
- Writes: a Postgres store opens `journal::postgres::begin_tenant`, appends with
  `journal::postgres::append` and enqueues tuples with `authz::outbox::enqueue` in the same
  transaction, then calls `authz::outbox::apply_after_commit`. In memory: `MemoryJournal::append`
  and `Authz::write`.
- The nodes area must implement `tenants::RootCreated` on `MemoryNodeStore` and call
  `MemoryArea::set_root_created` in `Services::in_memory` (the Postgres tenant store already
  inserts the root row, named `root`).

## Wiring of `ruum-server`

`crates/ruum-server/src/wiring.rs` builds `Services`. Without `DATABASE_URL` everything is in
memory. With `DATABASE_URL`: SpiceDB when `RUUM_SPICEDB_PRESHARED_KEY` or `SPICEDB_PRESHARED_KEY`
is set (`RUUM_SPICEDB_HTTP_URL`, default `http://127.0.0.1:8543`; otherwise the in-memory engine
with a warning), the outbox worker (every 5 s), Postgres journal, audit log and sharing, and the
identity area (`identity::setup::install`: Postgres identity, tenants, agents, OAuth and
idempotency stores). Nodes, tags, search, blobs and versions stay in memory until their areas are
merged. Settings of the identity area: `RUUM_TOKEN_SIGNING_KEY` (base64url 32-byte seed; not set
by `dev.sh env`, so tokens end at a restart), `RUUM_OIDC_ISSUER`, `RUUM_OIDC_CLIENT_ID`,
`RUUM_OIDC_CLIENT_SECRET`, `RUUM_OIDC_REDIRECT_URI`, `SMTP_URL`, `RUUM_MAIL_FROM`,
`RUUM_PLAN_LIMITS`.

## State of the checks

Checked on 2026-11-06 on `main` after the wave A merge (`DATABASE_URL` from `dev.sh env`):

- `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --check`: pass.
- `cargo test --workspace`: ruum-core 127 passed (3 ignored), ruum-api 22, ruum-mcp 4, testkit
  lib 7, contract 2, isolation suite passes. The permission matrix test FAILS (see below).
- Tenant isolation suite: 186 checks, 87 passed, 99 pending, 0 failed (also with
  `RUUM_REQUIRE_IMPLEMENTED=1`).
- Permission matrix: 1209 checks, 496 passed, 510 pending, 203 failed. Every failure comes from
  the nodes area, which is not merged: `MemoryNodeStore` is `todo!()`, so `seed` cannot make the
  folders, the share link or the scoped agents `agent` and `agent_out` (their scope subtree needs
  `NodeStore::ancestors`). That gives: the rows of `agent` and `agent_out` (401, placeholder
  credentials) for every operation; `404` for the sharing operations on seed nodes
  (`listGrants`, `addGrant`, `updateGrant`, `removeGrant`, `updateNodeSharing`, `listShareLinks`,
  `createShareLink`, `revokeShareLink`, `resolveShareLink`) and the agent operations on the
  narrow agent (`getAgent`, `updateAgent`, `disableAgent`, `enableAgent`, `listAgentKeys`,
  `createAgentKey`, `revokeAgentKey`, `listAgentActivity`, `undoAgent`); `400` for
  `exchangeToken` (the narrow agent's key is a placeholder). They should pass once the nodes area
  is merged and `seed` makes the nodes.
- A server against a new database on the stack Postgres: migrations, SpiceDB schema, `/healthz`
  200, JWKS 200, `/v1/me` 401, `/v1/auth/login` 302 to Zitadel.
- `web/`: `npm ci`, `npm run gen:api` (no diff), `npm run lint`, `npm run build`, `npm test`
  (10 files, 71 tests): pass.

## Known gaps

- The handlers of nodes, tags, search, blobs, versions answer `501`; their in-memory stores are
  `todo!()`.
- `fixtures::seed` makes real users, tenants and credentials, but placeholder nodes, links and
  scoped agents until the nodes area is merged.
- In-memory `TenantStore::usage` reports 0 bytes and 0 nodes. `node.sharing_updated` is not
  undone yet (no `SharingStore` method for it). The `idp` hint of `loginStart` is not sent to
  Zitadel. No end-to-end browser sign-in with Zitadel was done.
- The in-memory `MemoryArea` and `MemorySharingStore` hold each other (`Arc` cycle); a
  `Services::in_memory` set is never freed. Harmless for tests and the in-memory server.
- `scripts/dev.sh seed` creates only the databases; the tenant and user seed comes with the core
  service.
- The server's sign-in uses the public Zitadel client `ruum-web` with PKCE; no secret is needed.
- The CI jobs were written but have not run on GitHub yet (nothing is pushed). The `e2e` job is a
  placeholder until `tests/e2e` exists. `sqlx-cli` is not installed on this Mac; CI installs it.
- `oauth_clients` has no `tenant_id`: an OAuth client registration belongs to no tenant. This is
  an exception to the rule "every table has `tenant_id`" in `CLAUDE.md`, made by the contract
  review; the architect confirmed it (decision 0001-3).
- Open readings of the contract in the permission matrix are marked "assumption" or
  "open question" in `crates/ruum-testkit/permission_matrix.md`, and the contract gaps are listed
  in `docs/TESTING.md`. The open points of the contract review are decided in `docs/decisions/0001-step1-contract-decisions.md`.
- The share-link principal of the harness is a `view` link. The rules for an `edit` link
  (`putContent` and uploads) are not in the matrix yet.
