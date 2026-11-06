# Status of step 1

This page says what works on `main` after the wave B merge (2026-11-06), how to run it, the test
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
| 7. WebDAV | `/dav`: PROPFIND, GET with ranges, PUT (a version), MKCOL, MOVE, COPY, DELETE (to the trash), LOCK, quota properties. HTTP Basic with an app password, or an agent token. |
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
cargo run -p ruum-server          # http://127.0.0.1:8490 and https://ruum.localhost (Caddy)
```

Set these too, or tokens and content URLs end at each restart:

- `RUUM_TOKEN_SIGNING_KEY`: base64url of 32 random bytes
  (`openssl rand 32 | openssl base64 -A | tr '+/' '-_' | tr -d '='`).
- `RUUM_CONTENT_SIGNING_KEY`: base64 of 32 random bytes (`openssl rand -base64 32`).

Other settings: `RUUM_REQUEST_TIMEOUT_SECONDS` (default 120), `RUUM_DAV_SPOOL_DIR` (WebDAV
upload spool), `RUUM_PLAN_LIMITS`, `SMTP_URL`, `RUUM_MAIL_FROM`.

### Test users without a browser

```sh
target/debug/ruum-server seed-dev /tmp/seed.env   # same env as the server, same signing key
source /tmp/seed.env
```

It refuses to run unless `RUUM_PUBLIC_URL` is a loopback or `.localhost` origin. It makes a team
tenant with Alice (owner), Bob (a guest without grants), Carol (her own tenant), an agent of
Alice with an API key, and an app password of Alice. The file holds 1-hour user tokens
(`ALICE_TOKEN`, ...), `AGENT_KEY`, `APP_PASSWORD`, `TENANT_A`, `TENANT_B` and the user IDs. The
server sees the seed's SpiceDB tuples after about 5 seconds (see the gaps).

### End-to-end smoke test

```sh
scripts/e2e_smoke.sh              # the stack must be up; RUUM_E2E_PORT, default 8491
```

It builds and starts the server against the stack, seeds, and checks 47 steps: CLI login,
folders, a 3 MB upload in 73 chunks and a download with the same SHA-256, a named version and a
second version, a copy, a share to Bob and his access, Shared with me, the changes feed, MCP
(`initialize`, `tools/list`, `tools/call list_children`), WebDAV (PROPFIND, PUT, GET) and tenant
isolation (Carol gets `404`). Last run on 2026-11-06: `PASSED: 47 checks`.

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

`https://ruum.localhost/dav/` or `http://127.0.0.1:8490/dav/`, HTTP Basic with the user's email
and an app password (`POST /v1/me/app-passwords`, one tenant). Agents send their access token as
Bearer. `crates/ruum-webdav/scripts/smoke.sh` has Finder-like curl requests.

### Web UI

```sh
cd web
npm ci
npm run dev                       # mock API, no server needed
npm run dev:server                # proxies the API to the server on 8490 (see web/README.md)
```

For Zitadel sign-in through Vite, start the server with
`RUUM_OIDC_REDIRECT_URI=http://localhost:5173/auth/callback`.

## Checks (2026-11-06, `main`, `DATABASE_URL` from `dev.sh env`)

- `cargo build --workspace`, `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`: pass.
- `cargo test --workspace`: all pass. ruum-core 188 (1 ignored: `tenants/mail.rs`, needs SMTP),
  ruum-api 34, ruum-cli 4 + 12 (mock server) + 3 (real server), ruum-mcp 4 + 15 (server) + 5
  (tools contract), ruum-server 1, ruum-testkit 7 + 2 (contract) + 1 (isolation) + 2
  (permissions), ruum-webdav 5 + 18.
- `RUUM_REQUIRE_IMPLEMENTED=1 cargo test -p ruum-testkit`: pass. Tenant isolation suite: 188
  checks, 188 passed, 0 pending, 0 failed. Permission matrix: 1209 checks, 1209 passed, 0
  pending, 0 failed.
- `scripts/e2e_smoke.sh`: 47 checks passed.
- `web/`: `npm ci`, `npm run gen:api` (no diff), `npm run lint`, `npm run build`, `npm test`
  (12 files, 88 tests): pass. `npm audit --omit=dev`: 0 vulnerabilities.

## Known gaps

- **The security suites run in memory only.** `ruum_testkit::fixtures::app_state` uses
  `AppState::in_memory()`. The Postgres stores are covered by their conformance suites
  (`#[sqlx::test]`) and by `scripts/e2e_smoke.sh`, not by the isolation and permission suites.
  Next step: a Postgres backend for `app_state` when `DATABASE_URL` is set.
- **SpiceDB freshness across processes.** The SpiceDB engine reads at least as fresh as the
  newest ZedToken that this process wrote or saw. `authz::outbox::latest_token` (the tenant's
  newest token) is not passed to checks yet, so writes made by another process (another server,
  `seed-dev`) are visible only after SpiceDB's quantization window (5 s by default). With more
  than one server this also delays revocations by up to that window.
- **Contract text of `addGrant`.** Commit `653ee9a` makes a grant by user ID reach only members
  and guests of the tenant (`404` for others, no guest is made). The `addGrant` description in
  `contracts/openapi.yaml` still says that a user from another tenant becomes a guest. A contract
  change must settle the text (the `GranteeUser` schema already says "a member or a guest").
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
