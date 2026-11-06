# Status of step 1

This page says what works on `main` after the wave B merge, the security fixes of 2026-11-06 and
the panel decisions of `docs/decisions/0002-step1-panel-decisions.md`, how to run it, the test
counts and the known gaps. The work areas are in `docs/STEP1.md`. The foundation and wave A are in
`docs/FOUNDATION.md`.

## What is merged

Wave A: `swarm/authz`, `swarm/journal`, `swarm/identity`, `swarm/webui`.
Wave B: `swarm/nodes`, `swarm/blobs`, `swarm/webdav`, `swarm/mcp`, `swarm/webui2`.

All 93 operations of `contracts/openapi.yaml` are built. No route answers `501`.

| Area | What works |
|---|---|
| 1. Nodes, tags, search | The tree (create, get, by path, children, rename, move, copy, trash, restore, purge, empty trash), shortcut and member links, tags, name search with the trigram index. Memory and Postgres stores, one conformance suite. |
| 2. Blobs, versions, diff | Chunked uploads (FastCDC parameter sets, BLAKE3), downloads with `Range`, `HEAD` and `If-None-Match`, signed content URLs, `putContent`, versions (coalescing, thinning, retention, labels, restore), diff plugins (text, JSON, XML, image, binary), chunk garbage collection. Memory and Postgres stores, chunks in memory or S3 (Garage). |
| 3. Authz, sharing | `authz::require`, grants, share links, `updateNodeSharing`, Shared with me. In-memory engine and SpiceDB, the outbox. |
| 4. Identity, tenants, agents | Sign-in with Zitadel (PKCE), sessions with CSRF, tenants, members, invitations, plans and limits, agents and API keys, token exchange, OAuth 2.1 for MCP clients, app passwords, undo, the sweeper. |
| 5. Journal, audit | The changes feed (cursor, long poll, resync markers), the audit log (JSON and NDJSON). |
| 6. MCP, CLI | `/mcp` (stateless streamable HTTP, OAuth-protected, one tool per entry of `contracts/mcp/tools.yaml`). The `ruum` CLI with an agent API key. |
| 7. WebDAV | Its own host (`https://dav.ruum.localhost/`, listener `RUUM_DAV_BIND`, default `127.0.0.1:8492`; decision 0002-3): PROPFIND, GET with ranges, PUT (a version), MKCOL, MOVE, COPY, DELETE (to the trash), LOCK, quota properties. HTTP Basic with an app password, or an agent token. |
| 8. Web UI | All step 1 pages, chunked resumable uploads in the browser, diff view, previews through the content domain. |

### Links between the areas (integration)

- `Services::in_memory`: the node store is the content store's `versions::HeadUpdate` (a new
  version sets `head_version` and `size_bytes`), and the content store is the node store's
  `nodes::ContentCopy` (a copied file gets one version with the source's head, shared chunks,
  its own `version.created`; a purge forgets the versions).
- Postgres: `copyNode` calls `blobs::postgres::copy_head` in its own transaction, after the
  node entries.
- `ruum-server` wiring (`crates/ruum-server/src/wiring.rs`): without `DATABASE_URL` everything
  is in memory. With it, every store is Postgres, the authz engine is SpiceDB (when a preshared
  key is set) with the outbox worker, chunks go to S3/Garage (`RUUM_S3_*` or `S3_*`), and
  sign-in uses Zitadel (`RUUM_OIDC_ISSUER`, `RUUM_OIDC_CLIENT_ID`). The log says
  `authz engine: SpiceDB` and `stores: Postgres`.

## Security findings and fixes (2026-11-06)

| Finding | Severity | Fix | Regression test |
|---|---|---|---|
| WebDAV `GET` served user files on the platform origin (`/dav` shares the origin of the web UI and `/v1`) with the stored type (`text/html`, `image/svg+xml`) and no CSP, `nosniff` or `Content-Disposition`. A script in an uploaded file ran as the platform (session riding with the readable `ruum_csrf` cookie). Reported twice. | must fix | `ruum-webdav` `content_safety`: every `/dav` answer carries `Content-Security-Policy` = `blobs::signed::CONTENT_CSP` (`sandbox; ...`) and `X-Content-Type-Options: nosniff`; a successful `GET`/`HEAD` also carries `Content-Disposition: attachment` with the file name, and every type that is not passive (`blobs::signed::is_passive_content_type`: plain text, raster images, audio, video, PDF, JSON, archives) goes out as `application/octet-stream`. `CONTENT_CSP` and `disposition_header` moved from `routes/blobs.rs` to `ruum_core::blobs::signed`, so REST and WebDAV share them. | `crates/ruum-webdav/tests/dav.rs` `files_are_served_as_sandboxed_attachments` (`.html`, `.svg`, `.xhtml`, `.xml`, `.js`; `GET`, `HEAD`, range); mutation `webdav-content-safety` |
| Content and version reads ignored the trash rule: a viewer or editor (not `manage`) of a trashed file still downloaded it, got a content URL and listed its versions, although `getNode` answered `404`. The changes feed had the same gap. | should fix | `blobs::logic::require_trashed_read`: `open_content`, `content_grant`, `list_filtered` and `get` of versions (Postgres and memory; `diffVersions` goes through them and `NodeStore::get`) need `manage` on a trashed file and answer `404` otherwise. The feed's `ServicesAccess::can_view` uses `NodeStore::get`, so entries on a trashed node reach only those who may restore it; the others still get `node.trashed` through the old place. | `ruum-testkit` `trashed_file_reads` (`permissions::run_trashed`: every principal, `downloadContent`, `getContentUrl`, `listVersions`, `getVersion`, `diffVersions`, `listChanges` after the plan is trashed; 78 checks); the content conformance suite (memory with a node store that does not check the trash, and Postgres); mutations `blobs-trashed-read-postgres`, `blobs-trashed-read-memory`, `feed-trashed-node-visible` |
| A guest without grants registered agents (`registerAgent`, `oauthConsent` with `new_agent`) that counted against the host tenant's agent limit, so a guest could block owners and members (`plan_limit_reached`). | should fix | A guest's agents are counted apart: each guest may own `agents::guest_agent_limit(plan)` live agents (one member's allowance), and the tenant's limit counts only the agents of owners, admins and members (`agents::postgres::MEMBER_AGENT`). `getLimits` usage counts the same agents. Guests may still register agents, as the permission matrix assumes. | `agents::tests::guests_cannot_use_up_the_tenant_agent_limit` (memory and Postgres); mutations `agents-guest-uses-tenant-limit`, `agents-guest-uses-tenant-limit-postgres` |

## Panel decisions (2026-11-06, decision 0002, `provisional`)

| Question | Decision | Tests |
|---|---|---|
| 1. `addGrant` by user ID | Members and guests of the tenant only; any other ID is `404`, no guest is made. Contract text fixed. With `allow_guests: false` the in-memory store now accepts an email grant to a member's verified email, as Postgres does. | `sharing::conformance` (memory, Postgres); mutation `sharing-email-grant-policy-memory` |
| 2. Guests and agents | Guests may own agents (own limit), but register none while `allow_guests` is false (`registerAgent`, `oauthConsent` with `new_agent`: `403`). | `agents::tests::guests_register_no_agents_while_guests_are_off`; mutations `agents-guest-register-policy`, `agents-guest-register-policy-postgres` |
| 3. WebDAV host | WebDAV moved to its own host and listener; the platform origin answers `404` for `/dav`; the WebDAV host has no API, never reads or sets cookies, keeps the content headers. `createAppPassword` returns `dav_url`. The CSRF value is bound to the session (cookie tossing). Cookie names stay (`__Host-` later). | `app::tests::*`, `dav.rs` `the_dav_host_*`, `the_csrf_value_is_bound_to_the_session`, `a_new_app_password_names_the_dav_host`; mutation `csrf-not-session-bound`; e2e |
| 4. SpiceDB freshness | Every tenant request reads at least as fresh as the tenant's newest outbox token (`PgFreshness`, under RLS); a lookup error fails the request. | `wiring::tests::writes_of_another_server_hold_at_once` (two servers, SpiceDB), `freshness_reads_the_tenant_token_under_rls`, `a_failed_freshness_lookup_fails_the_request`; mutations `authz-ignores-tenant-token`, `authz-spicedb-ignores-request-floor`; e2e without the 6 s wait |
| 5. Matrix rules | Every "assumption" and "open question" rule decided; `logout` and `listAgents` for agents: `any` becomes `ok`. Contract wording for these operations. | permission matrix (1209 checks) |

## How to run it

The cargo settings of this repository (one shared cache):

```sh
export CARGO_TARGET_DIR=$PWD/target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=false CARGO_PROFILE_TEST_DEBUG=false
```

### Stack and server

```sh
scripts/dev.sh up                 # Postgres, Garage, Zitadel, SpiceDB, ...; see docs/DEVSTACK.md
scripts/dev.sh status             # all services healthy
eval "$(scripts/dev.sh env)"
cargo run -p ruum-server          # http://127.0.0.1:8490 and https://ruum.localhost (Caddy);
                                  # WebDAV: http://127.0.0.1:8492 and https://dav.ruum.localhost
```

Set these too, or tokens and content URLs end at each restart:

- `RUUM_TOKEN_SIGNING_KEY`: base64url of 32 random bytes
  (`openssl rand 32 | openssl base64 -A | tr '+/' '-_' | tr -d '='`).
- `RUUM_CONTENT_SIGNING_KEY`: base64 of 32 random bytes (`openssl rand -base64 32`).

Other settings: `RUUM_REQUEST_TIMEOUT_SECONDS` (default 120), `RUUM_DAV_BIND` (the WebDAV
listener, default `127.0.0.1:8492`), `RUUM_DAV_URL` (the public URL of the WebDAV host, default
`https://dav.<host>/` for an https public URL, else `http://<RUUM_DAV_BIND>/`),
`RUUM_DAV_SPOOL_DIR` (WebDAV upload spool), `RUUM_PLAN_LIMITS`, `SMTP_URL`, `RUUM_MAIL_FROM`.

### Test users without a browser

```sh
target/debug/ruum-server seed-dev /tmp/seed.env   # same env as the server, same signing key
source /tmp/seed.env
```

It refuses to run unless `RUUM_PUBLIC_URL` is a loopback or `.localhost` origin. It makes a team
tenant with Alice (owner), Bob (a guest without grants), Carol (her own tenant), an agent of
Alice with an API key, and an app password of Alice. The file holds 1-hour user tokens
(`ALICE_TOKEN`, ...), `AGENT_KEY`, `APP_PASSWORD`, `TENANT_A`, `TENANT_B` and the user IDs. The
server sees the seed's relationships at once: each request reads at least as fresh as its
tenant's newest outbox token (decision 0002-4).

### End-to-end smoke test

```sh
scripts/e2e_smoke.sh              # the stack must be up; RUUM_E2E_PORT (default 8491),
                                  # RUUM_E2E_DAV_PORT (default 8493), RUUM_DEV_SH (for worktrees)
```

It builds and starts the server against the stack, seeds, and checks 52 steps: CLI login,
folders, a 3 MB upload in 73 chunks and a download with the same SHA-256, a named version and a
second version, a copy, a share to Bob and his access, Shared with me, the changes feed, MCP
(`initialize`, `tools/list`, `tools/call list_children`), WebDAV on its own host (PROPFIND, PUT,
GET, `dav_url`, no cookie, no API there, `404` for `/dav` on the platform origin) and tenant
isolation (Carol gets `404`). There is no wait after the seed. Last run on 2026-11-06 (branch
`q/decisions`): `PASSED: 52 checks`.

### CLI

```sh
cargo build -p ruum-cli
target/debug/ruum --url http://127.0.0.1:8490 login --key ruum_agent_...
target/debug/ruum mkdir -p /Projects/Garden
target/debug/ruum upload plan.md /Projects/Garden/
target/debug/ruum ls /Projects/Garden
target/debug/ruum versions /Projects/Garden/plan.md
target/debug/ruum share /Projects --user <user id> --role view
target/debug/ruum changes
```

`--json` prints the API's JSON. The credentials are in `~/.config/ruum/credentials.json`.

### MCP

`POST /mcp` with `Authorization: Bearer <agent access token>` and
`Accept: application/json, text/event-stream`. Without a token it answers `401` with
`WWW-Authenticate: Bearer resource_metadata=".../.well-known/oauth-protected-resource"`, so MCP
clients run the OAuth flow (dynamic registration, consent in the web UI). For scripts: exchange
an agent API key at `POST /v1/oauth/token` (`grant_type=urn:ietf:params:oauth:grant-type:token-exchange`,
`subject_token_type=urn:ruum:params:oauth:token-type:api-key`). A tool's request body goes in the
argument `body` (see `crates/ruum-mcp/src/tools.json`).

### WebDAV

`https://dav.ruum.localhost/` or `http://127.0.0.1:8492/` (its own host, decision 0002-3), HTTP
Basic with the user's email and an app password (`POST /v1/me/app-passwords`, one tenant; the
answer's `dav_url` is the URL to mount). Agents send their access token as Bearer. The platform
origin has no `/dav`. `crates/ruum-webdav/scripts/smoke.sh` has Finder-like curl requests.

### Web UI

```sh
cd web
npm ci
npm run dev                       # mock API, no server needed
npm run dev:server                # proxies the API to the server on 8490 (see web/README.md)
```

For Zitadel sign-in through Vite, start the server with
`RUUM_OIDC_REDIRECT_URI=http://localhost:5173/auth/callback`.

## Checks (2026-11-06, branch `q/decisions`, after the panel decisions, `DATABASE_URL` and SpiceDB from `dev.sh env`)

- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`: pass.
- `cargo test --workspace`: all pass. ruum-core 194 (1 ignored: `tenants/mail.rs`, needs SMTP),
  ruum-api 37, ruum-cli 4 + 12 (mock server) + 3 (real server), ruum-mcp 4 + 15 (server) + 5
  (tools contract), ruum-server 5 (one needs SpiceDB: two servers on one database),
  ruum-testkit 7 + 2 (contract) + 1 (isolation) + 3 (permissions), ruum-webdav 5 + 20.
- `RUUM_REQUIRE_IMPLEMENTED=1 cargo test -p ruum-testkit`: pass. Tenant isolation suite: 188
  checks, 188 passed. Permission matrix: 1209 checks, 1209 passed. Trashed file suite: 78
  checks, 78 passed. 0 pending, 0 failed.
- Contracts: `redocly lint`: valid. `oasdiff breaking` against `main`: no breaking change
  (changelog: one optional response property, `dav_url`).
- `crates/ruum-testkit/scripts/mutation_check.sh` (with `MUTATION_TARGET_DIR=$PWD/target`) for
  the new and related mutations: all 8 killed (`sharing-email-grant-policy-memory`,
  `agents-guest-register-policy`, `agents-guest-register-policy-postgres`,
  `csrf-not-session-bound`, `authz-ignores-tenant-token`, `authz-spicedb-ignores-request-floor`,
  `sharing-grant-by-id-makes-guest`, `webdav-content-safety`). The full run was not repeated on
  this branch; before it, `nodes-tenant-filter` (SKIP) and `sharing-tenant-filter` (SURVIVED)
  were the known exceptions.
- `scripts/e2e_smoke.sh`: 52 checks passed (no wait after the seed).
- `crates/ruum-webdav/scripts/smoke.sh` against `dav_fake_server` at the root path: all passed.
- `web/`: `npm ci`, `npm run gen:api`, `npm run lint`, `npm run build`, `npm test` (12 files,
  88 tests): pass.

## Known gaps

- **Cookie names.** The session and CSRF cookies keep their names (`ruum_session`,
  `ruum_csrf`); the `__Host-` prefix is a later hardening, because the rename breaks the
  contract (decision 0002-3). The CSRF value is bound to the session. The REST download
  (`downloadContent`) keeps the stored type under its CSP sandbox; only WebDAV maps active types
  to `application/octet-stream`.
- **Caddy on a running stack.** The new `dav.ruum.localhost` block needs the Caddy container to
  be recreated (it reads the Caddyfile at start; `RUUM_DAV_UPSTREAM` is new in compose).
- **Signed content URLs and the trash.** A content URL made before the file went to the trash
  works until it expires (at most `CONTENT_URL_SECONDS`, 5 minutes).
- **Mutation check.** `nodes-tenant-filter` does not match the Postgres node queries (SKIP) and
  `sharing-tenant-filter` survives, because the isolation suite runs in memory only. By
  default the script builds into `target/mutation`; pass `MUTATION_TARGET_DIR=$PWD/target` to
  share the cache.
- **The security suites run in memory only.** `ruum_testkit::fixtures::app_state` uses
  `AppState::in_memory()`. The Postgres stores are covered by their conformance suites
  (`#[sqlx::test]`) and by `scripts/e2e_smoke.sh`, not by the isolation and permission suites.
  Next step: a Postgres backend for `app_state` when `DATABASE_URL` is set.
- **SpiceDB freshness inside a request.** A request reads at least as fresh as its tenant's
  newest token at its start and its own writes (decision 0002-4). Work that a handler spawns on
  another task, the outbox worker and `seed-dev` have no request floor and read at the process
  token, as before.
- **Coalescing.** Saves by the same actor within 10 minutes merge into the head version (design
  5.3), also through the CLI, WebDAV and MCP. Name a version (`setVersionLabel`) to keep it. The
  WebDAV ETag of a file is `"<head version>-<revision>"`, because a merged save keeps the version
  ID.
- **Request timeout.** `RUUM_REQUEST_TIMEOUT_SECONDS` (default 120) also covers the body of a
  large WebDAV `PUT` or `putContent` on a slow link. Chunked uploads are not affected.
- In-memory `TenantStore::usage` reports 0 bytes and 0 nodes (Postgres reports real usage).
- No browser sign-in through Zitadel was run end to end; the e2e test uses `seed-dev` tokens.
- The WebDAV example `dav_fake_server` keeps its name but now runs on `Services::in_memory`.
- CI (`.github/workflows/`) has not run on GitHub; nothing is pushed.
- Older gaps of `docs/FOUNDATION.md` still apply (`node.sharing_updated` is not undone, the
  `idp` hint of `loginStart` is not sent, the in-memory `Arc` cycle).

## Dependencies added in wave B

- `unicode-normalization` (workspace, nodes area: NFC names).
- ruum-core uses the existing workspace dependencies `similar`, `quick-xml` and `image` (diff
  plugins). ruum-mcp, ruum-cli and ruum-webdav use workspace dependencies only (`rmcp`, `clap`,
  `dav-server`, ...; see their `Cargo.toml`). Web: `@noble/hashes` (BLAKE3 in the browser).
