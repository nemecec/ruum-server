# Status of step 1

This page says what works on `main` after the wave B merge, the security fixes of 2026-11-06,
the panel decisions of `docs/decisions/0002-step1-panel-decisions.md`, the quality wave (wave
C), the UX fixes of the web UI (`q/uxfix`), the synthetic-user fixes (`q/synthfix`), the
names of actors (decision 0003, `f/followups`) and the step 2 specification (decision 0004,
`f/step2spec`), how to run it, the test counts and the known gaps. The work areas are in `docs/STEP1.md`. The foundation and wave A are in
`docs/FOUNDATION.md`.

## What is merged

Wave A: `swarm/authz`, `swarm/journal`, `swarm/identity`, `swarm/webui`.
Wave B: `swarm/nodes`, `swarm/blobs`, `swarm/webdav`, `swarm/mcp`, `swarm/webui2`.
Wave C (2026-11-06, rebased onto `main` and fast-forwarded, in this order): `q/decisions`,
`q/pgsuites`, `q/props`, `q/browser`, `q/bench`, `q/uxdesign`.

| Branch | What it adds |
|---|---|
| `q/decisions` | Decision record 0002 and its implementation (see "Panel decisions" below). |
| `q/pgsuites` | The isolation, permission and trashed file suites also run on the Postgres stores (`RUUM_SUITE_BACKEND=postgres`, optional `RUUM_SUITE_AUTHZ=spicedb`, `RUUM_SUITE_RLS=off`); Postgres tenant-filter mutations; the mutation check runs strict. |
| `q/props` | Property tests (`crates/ruum-core/tests/properties`: authz, changes feed, chunking, diff) and two fixes they found: a member link can no longer form a loop of `view` (`rules::member_link_loop`, `422`), and the changes feed keeps an agent's grant entries inside its scope. |
| `q/browser` | Playwright tests in `web/e2e` against the real server, Zitadel and Mailpit (`scripts/e2e_browser.sh`), the browser driver for agents (`scripts/ux/drive.mjs`), the CI job `e2e-browser`. |
| `q/bench` | The first agent benchmark over MCP (`bench/agent/`, 8 step 1 tasks; first run 7 of 8 passed; `docs/BENCHMARK.md`). |
| `q/uxdesign` | The UX package (`docs/ux/`: personas, journeys, IA, design system, copy, agent experience, gap analysis), provisional. |

UX fixes (2026-11-06, `git merge --no-ff q/uxfix`, no conflicts; only `web/` and `docs/ux/`
changed): the blockers and most majors of the UX panel. Details and contract needs C-7 to C-11
are in `docs/ux/decisions.md`.

| Area | What changed in the web UI |
|---|---|
| Shared with me | Every row opens the item (`/t/<space>/files/<id>`, `/t/<space>/item/<id>`); the space and the sharer by name, never an ID; a space that is not in the person's list opens in a guest shell. |
| Share links | `/s#token` visitor page (password, ended link, subfolders, Download and Preview with the link's bearer token; the token never goes into a URL). |
| Spaces, invitations | "Start a team space..." in the switcher and on Account; `/invitations/accept` (sign-in keeps `return_to`, one click joins); a personal space offers only Guest. "Send an email" is off (`notify: false`) until the server sends share emails. |
| Agents | `/t/<space>/agents/connect` (MCP address, steps for Claude Desktop, ChatGPT, Claude Code); consent defaults to one folder and 90 days; Pause/Resume, access summary, activity with item names. |
| Files | Names instead of IDs (from `listMembers`, `listAgents`), agent marks in Changed, a coalescing warning before "Upload new version", one upload panel with a conflict question per batch, selection with bulk Move and Trash (Undo), sorting, error copy for every `ErrorCode` (`api/errors.ts`), welcome card and empty states. |
| Tests | Flow tests for share links, invitations, uploads, selection, errors; `web/e2e/visual.e2e.ts` (88 snapshots on the mock UI, darwin only) and `web/e2e/a11y.e2e.ts` (axe, WCAG 2.2 A/AA); the 4 UI `test.fixme` of the browser tests now run. |

Synthetic-user fixes (2026-11-06, `q/synthfix` fast-forwarded onto `main`, no conflicts; only
`web/`, `docs/ux/decisions.md` and `scripts/ux/README.md` changed; no server or contract change):
the 2 blockers and the majors that two synthetic users (design 18.9) found. Decisions S1 to S6
and contract needs C-12 to C-14 are in `docs/ux/decisions.md`.

| Area | What changed in the web UI |
|---|---|
| Share dialog | A share to a member or guest goes by user ID (`addGrant {type: "user"}`), so it reaches the account at once; the access from the space role ("Everyone in *Space* except guests: Can manage") is listed and a weaker share is called out; one "Email them an invitation" box for a person outside, plus "Use a link instead"; own row read-only; "added by". The mock API keeps email grants as email grants, like the server. |
| Upload conflict | "Save as a new version" is the default and first names the person's own recent unnamed version ("Before the upload of HH:MM"), so nothing is lost. |
| Activity log | Access, role, sharing rule, link and member entries name the item (a link), the person and the old and new values (from `AuditEntry.data`). |
| Smaller | Change to Guest asks first; sharing rule inherit shows the parent's value and "Saved."; version times; "Keep forever..."; compare summary; image icon; sign-in labels. |

Night integration (2026-11-07, rebased onto `main` and fast-forwarded, in this order; linear
history, no merge commit):

| Branch | What it adds |
|---|---|
| `f/followups` | Decision record 0003 (step 1 follow-ups, `provisional`; items 1 to 8 decided) and its item 5 (C-7, names of actors): migrations `0002_principal_names.sql` and `0003_agent_names_unique.sql`; `display_name` and `on_behalf_of_display_name` in every answer (middleware `ruum-api/src/names.rs`, MCP included); unique agent names per owner (" (2)"); `listChanges` filters `actor` and `since`; the web UI shows server names, an agent as "*Agent* (agent of *User*)"; bench task `09_undo_agent_two_bots`. |
| `f/step2spec` | The step 2 (Databases) specification and decision record 0004 (`provisional`, items 1 to 33): `contracts/providers/`, `contracts/manifests/database.schema.json`, 24 database operations in `contracts/openapi.yaml` marked `x-ruum-status: planned`, `schema.zed` relations for databases, step 2 event schemas, `migrations/0004_databases.sql` (draft), `docs/STEP2.md` (task cards), `docs/ux/databases/`. The test kit reports the checks of planned operations as planned (also with `RUUM_REQUIRE_IMPLEMENTED=1`) and still fails on leaks; the MCP server offers no tool of a planned operation. |

Fixes made during the night integration:

- The rebase of `f/step2spec` renamed its migration `0002_databases.sql` to `0004_databases.sql`
  (0002 and 0003 went to decision 0003); the SQL did not change. `docs/STEP2.md` and decision
  0004 say "0004" now.
- `crates/ruum-mcp/src/tools.json` and `web/src/api/schema.d.ts` were regenerated at each
  rebased commit (both branches changed `openapi.yaml` and `tools.yaml`; the YAML merged without
  conflicts).

Content role of team members (decision 0003-1, branch `f/content-role`): members get a
content role `view | comment | edit | manage` (default `manage`) on the team tree.
Migration `0005_member_content_role.sql` (`memberships.content_role`, `invitations.content_role`,
CHECK: set if and only if `role = member`); `schema.zed` relations `content_viewer`,
`content_commenter`, `content_editor` and permissions `team_manager`, `team_editor`,
`team_commenter`, `team_viewer`; optional `content_role` in `updateMember`, `createInvitation`,
`Member` and `Invitation`; accepting an invitation never lowers a content role; rule (b) of
0002-6 counts only managers (`TenantStore::manager_since`); tag writes need content role `edit`.
The permission matrix has three new columns (`member_view`, `member_comment`, `member_edit`).
The web UI part is a card in `ruum-server-wt/panel/ui-followups.md` (not built).

The digest for the person is `docs/digests/2026-11-06-night.md` (earlier:
`docs/digests/2026-11-06.md`, `docs/digests/2026-11-06-evening.md`). The worktrees of
`q/browser`, `q/decisions`, `q/synthfix`, `f/followups` and `f/step2spec` are removed; the
branches stay.

Fixes made during the `q/uxfix` integration:

- Two flow tests (`shell.test.tsx` "starts a team space from the space switcher",
  `invitation.test.tsx` "joins the space with one click") read the space switcher before React
  Router committed the new location (a transition) and failed about one full run in three. The
  same assertions now run in `waitFor`.
- Sign out raced a sign-in redirect: the server ends the session first, a call still under way
  answered `401`, and the `401` listener sent the browser to `/signin?return_to=...` while
  `browser.assign(logout_url)` started. The e2e test "sign out ends the session" failed with
  `net::ERR_ABORTED` (3 of 8 repeats). `api/client.ts` now skips the sign-in listeners while the
  person signs out (`setSigningOut`); a `401` from logout lands on `/signin`. Two flow tests;
  the e2e test waits for the end of the chain (`/signin`).

Fixes made during the integration:

- `scripts/dev.sh env` printed `RUUM_DAV_BIND=127.0.0.1:` (no port): the heredoc used a
  variable that was set inside the heredoc. It now prints `127.0.0.1:8492` (from
  `RUUM_DAV_UPSTREAM`).
- `scripts/e2e_browser.sh` gives the WebDAV listener its own port (`RUUM_E2E_BROWSER_DAV_PORT`,
  default 8496). It inherited `RUUM_DAV_BIND` from `dev.sh env`, the same port as the server.
- The Postgres suite backend (`crates/ruum-testkit/src/backend.rs`) wires `PgFreshness`, as
  `ruum-server` does since decision 0002-4. The seed (`fixtures::seed`, `seed_bridge`) and the
  trash step of the trashed file suite run with a request freshness floor;
  `authz::in_current_request` carries the floor into the tasks that the seed spawns. Without
  this, worlds that run at the same time on one SpiceDB engine overwrote the process token, and
  the SpiceDB run failed 11 to 19 trashed checks and 45 matrix checks (setup steps answered
  `404`).
- `web/e2e`: two timing races. `files.e2e.ts` matched any `status` element while the folder
  showed `Loading...`; `auth.setup.e2e.ts` navigated while the sign-out redirect chain still ran.

All 93 step 1 operations of `contracts/openapi.yaml` are built. The 24 step 2 database
operations are specified and marked `x-ruum-status: planned` (decision 0004-21); they are not
built yet.

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
| Content and version reads ignored the trash rule: a viewer or editor (not `manage`) of a trashed file still downloaded it, got a content URL and listed its versions, although `getNode` answered `404`. The changes feed had the same gap. | should fix | `blobs::logic::require_trashed_read`: `open_content`, `content_grant`, `list_filtered` and `get` of versions (Postgres and memory; `diffVersions` goes through them and `NodeStore::get`) need `manage` on a trashed file and answer `404` otherwise. The feed's `ServicesAccess::can_view` uses `NodeStore::get`, so entries on a trashed node reach only those who may restore it. A `node.trashed` or `node.purged` entry, and any entry on a node in the trash or purged, reaches only those who could see the node just before the trash or could restore it; access that began after the trash (also a membership) never reveals it (decision 0002-6). | `ruum-testkit` `trashed_file_reads` (`permissions::run_trashed`: every principal, `downloadContent`, `getContentUrl`, `listVersions`, `getVersion`, `diffVersions`, `listChanges` after the plan is trashed; 78 checks); the content conformance suite (memory with a node store that does not check the trash, and Postgres); mutations `blobs-trashed-read-postgres`, `blobs-trashed-read-memory`, `feed-trashed-node-visible` |
| A guest without grants registered agents (`registerAgent`, `oauthConsent` with `new_agent`) that counted against the host tenant's agent limit, so a guest could block owners and members (`plan_limit_reached`). | should fix | A guest's agents are counted apart: each guest may own `agents::guest_agent_limit(plan)` live agents (one member's allowance), and the tenant's limit counts only the agents of owners, admins and members (`agents::postgres::MEMBER_AGENT`). `getLimits` usage counts the same agents. Guests may still register agents, as the permission matrix assumes. | `agents::tests::guests_cannot_use_up_the_tenant_agent_limit` (memory and Postgres); mutations `agents-guest-uses-tenant-limit`, `agents-guest-uses-tenant-limit-postgres` |

## Panel decisions (2026-11-06, decision 0002, `provisional`)

| Question | Decision | Tests |
|---|---|---|
| 1. `addGrant` by user ID | Members and guests of the tenant only; any other ID is `404`, no guest is made. Contract text fixed. With `allow_guests: false` the in-memory store now accepts an email grant to a member's verified email, as Postgres does. | `sharing::conformance` (memory, Postgres); mutation `sharing-email-grant-policy-memory` |
| 2. Guests and agents | Guests may own agents (own limit), but register none while `allow_guests` is false (`registerAgent`, `oauthConsent` with `new_agent`: `403`). | `agents::tests::guests_register_no_agents_while_guests_are_off`; mutations `agents-guest-register-policy`, `agents-guest-register-policy-postgres` |
| 3. WebDAV host | WebDAV moved to its own host and listener; the platform origin answers `404` for `/dav`; the WebDAV host has no API, never reads or sets cookies, keeps the content headers. `createAppPassword` returns `dav_url`. The CSRF value is bound to the session (cookie tossing). Cookie names stay (`__Host-` later). | `app::tests::*`, `dav.rs` `the_dav_host_*`, `the_csrf_value_is_bound_to_the_session`, `a_new_app_password_names_the_dav_host`; mutation `csrf-not-session-bound`; e2e |
| 4. SpiceDB freshness | Every tenant request reads at least as fresh as the tenant's newest outbox token (`PgFreshness`, under RLS); a lookup error fails the request. | `wiring::tests::writes_of_another_server_hold_at_once` (two servers, SpiceDB), `freshness_reads_the_tenant_token_under_rls`, `a_failed_freshness_lookup_fails_the_request`; mutations `authz-ignores-tenant-token`, `authz-spicedb-ignores-request-floor`; e2e without the 6 s wait |
| 5. Matrix rules | Every "assumption" and "open question" rule decided; `logout` and `listAgents` for agents: `any` becomes `ok`. Contract wording for these operations. | permission matrix (1209 checks) |
| 6. The trash in the changes feed | A trash or purge entry, and any entry on a node in the trash or purged, reaches a subject only when it could see the node just before the trash, or could restore it (`manage`, capped by an agent's scope) while it was in the trash. Access that began after the trash never counts; memberships are dated by `memberships.updated_at` (`TenantStore::member_since`). | `journal::feed::tests`, journal and tenant conformance (memory and Postgres), the feed property test; mutations `feed-gone-node-old-place`, `feed-gone-node-ancestor-now`, `feed-member-joined-after-purge` |

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

## Checks (2026-11-07, `main` after the night integration of `f/followups` and `f/step2spec`, `DATABASE_URL` and SpiceDB from `dev.sh env`)

The Rust, suite and web checks ran on the rebased `f/step2spec` tip; `main` has the same tree
plus four commits to `docs/setup/ACCOUNTS.md` (made while the checks ran, so `f/step2spec` was
rebased once more without a change to code). The smoke test and the mutation check ran on `main`.

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`: pass.
- `cargo test --workspace`: 355 passed, 0 failed, 1 ignored (`tenants/mail.rs`, needs SMTP).
  ruum-core 210 + 15 (properties), ruum-api 39, ruum-cli 4 + 12 (mock server) + 3 (real
  server), ruum-mcp 5 + 15 (server) + 5 (tools contract), ruum-server 5, ruum-testkit 10 + 2
  (contract) + 1 (isolation) + 1 (names) + 3 (permissions), ruum-webdav 5 + 20.
- `RUUM_REQUIRE_IMPLEMENTED=1 cargo test -p ruum-testkit` on every backend: tenant isolation
  328 checks (251 passed, 77 planned), permission matrix 1521 checks (1209 passed, 312
  planned), trashed file suite 78/78; 0 pending, 0 failed, with `RUUM_SUITE_BACKEND=memory`,
  `postgres` (memory authz) and `postgres` with `RUUM_SUITE_AUTHZ=spicedb`. The runs were one
  after another, not in parallel.
- `MUTATION_STRICT=1 MUTATION_TARGET_DIR=$PWD/target crates/ruum-testkit/scripts/mutation_check.sh`:
  31 of 31 mutations killed, none survived or skipped (the 27 rules of 2026-11-06 and the 4
  `names-*` rules of decision 0003-5).
- `scripts/e2e_smoke.sh`: `PASSED: 52 checks`.
- `web/`: `npm run lint` (0 warnings), `npm run build`, `npx vitest run` (16 files, 159 tests),
  `npx tsc -p e2e/tsconfig.json`: pass.
- `oasdiff breaking --fail-on ERR` (`tufin/oasdiff:v1.11.4`, as in CI) from the old `main`
  (`9c86f0d`) to the new contract: 0 errors, 2438 warnings, all
  `response-property-enum-value-added` (new `ErrorCode` values in the shared error schema).
- On `f/followups` alone (before the step 2 merge): the same checks passed (workspace 348 passed,
  1 ignored, suites memory and Postgres 251/1209/78, web 150 tests), except three
  SpiceDB tests that could not write their schema to the shared SpiceDB (see "One SpiceDB for all
  worktrees" below).
- Not run at this integration: `scripts/e2e_browser.sh` (the Linux snapshots of 0003-5 come from
  CI), `redocly`, `zed validate`, `crates/ruum-webdav/scripts/smoke.sh`, `cargo deny check`.

## Known gaps

- **Step 1 follow-ups** (decision 0003, `provisional`): item 5 (C-7, names of actors) is built
  and merged; item 1 (content role) is built on `f/content-role`. Items 2 (C-12, email grants
  bind) and 3 (C-13, share and invitation emails) are built on the server in `f/email-grants`,
  stacked on `f/content-role` (migration `0006_email_grants_and_mail.sql`, the mail outbox
  worker, `addGrant` answers `notified`). Their web UI parts are cards in
  `ruum-server-wt/panel/ui-followups.md`, built on the design system (`w/design-system`).
  Items 4 (C-14), 6 (C-8 to C-11), 7 (search `type`, batch, hints, `safe_stop`) and 8 (IDs of
  gone nodes) are decided and not built here.
- **Mail.** Mail goes only to the relay of `SMTP_URL` (Mailpit on the laptop). Production
  sending stays with a person (design 18.9). There is no GDPR deletion job yet; it must delete
  a user's `mail_outbox` rows (and `principal_names`).

- **Step 2** (decision 0004, `provisional`): only the specification is merged. The 24 database
  operations have no route yet and count as planned in the suites (77 isolation and 312 matrix
  checks). `migrations/0004_databases.sql` is a draft, but it is applied like any migration (by
  `sqlx::test`, the suites, and the server at start while `RUUM_MIGRATE` is on, the default), so
  a change to it before step 2 is built needs a fresh database. Open: spike S-1 before the role kinds freeze, the PostgreSQL Anonymizer change of
  design 8.5 (a person confirms), UX contract needs C-20 to C-23.
- **Linux visual snapshots.** Decision 0003-5 changed `versions-desktop-light` and
  `versions-desktop-dark`; the darwin references are updated, the Linux references
  (`web/e2e/visual.e2e.ts-snapshots/*-visual-linux.png`) must be refreshed by CI.
- **One SpiceDB for all worktrees.** The SpiceDB of the laptop stack holds one schema. After a
  branch with new relations (here `f/step2spec`) wrote its schema and relationships, a branch
  without those relations cannot write its schema (`cannot delete relation ...`), and its SpiceDB
  tests fail (`spicedb_authz_conforms`, `spicedb_agrees_with_the_memory_engine`,
  `writes_of_another_server_hold_at_once`). This hit `f/followups` before the step 2 merge; on
  `main` they pass.
- **Cookie names.** The session and CSRF cookies keep their names (`ruum_session`,
  `ruum_csrf`); the `__Host-` prefix is a later hardening, because the rename breaks the
  contract (decision 0002-3). The CSRF value is bound to the session. The REST download
  (`downloadContent`) keeps the stored type under its CSP sandbox; only WebDAV maps active types
  to `application/octet-stream`.
- **Caddy on a running stack.** The new `dav.ruum.localhost` block needs the Caddy container to
  be recreated (it reads the Caddyfile at start; `RUUM_DAV_UPSTREAM` is new in compose).
- **Signed content URLs and the trash.** A content URL made before the file went to the trash
  works until it expires (at most `CONTENT_URL_SECONDS`, 5 minutes).
- **Mutation check.** By default the script builds into `target/mutation`; pass
  `MUTATION_TARGET_DIR=$PWD/target` to share the cache.
- **Product gaps found by the browser tests** (`test.fixme`, see `docs/TESTING.md`): the 4 web
  UI gaps (dialog focus, upload race, cold-load `return_to`, no `/invitations/accept` page) are
  fixed by `q/uxfix`. The server gaps behind the two sharing `test.fixme` are closed in
  `f/email-grants` (a bind makes the person a guest; every sign-in and web session binds;
  `addGrant` queues a share email); the tests stay `fixme` until the web UI card sends
  `notify: true`. `docs/TESTING.md` still lists the fixed UI
  gaps and has no rows for `visual.e2e.ts` and `a11y.e2e.ts`.
- **Web UI after `q/uxfix`** (`docs/ux/decisions.md`): names come from the server since
  decision 0003-5 (C-7: `display_name` and `on_behalf_of_display_name` in
  every answer; agents show as "*Agent* (agent of *User*)"); the space
  name of an unlisted space is unknown (C-10); the server still coalesces an explicit upload
  when the person picks "Replace it" (C-8). Not done: the session-ended dialog (UX-09), pausing
  uploads offline, the "Changed by agents" filter (C-2), the "Shared" mark on rows (C-1), the
  share dialog people picker, grouping agent activity by runs, version day groups, Shift-range
  selection, the grid view, the app shell rail (UX-17), UX-20. The visual snapshots exist only
  for darwin; a Linux CI job needs its own reviewed set.
- **After `q/synthfix`** (`docs/ux/decisions.md`): agents (MCP, CLI) and guests who share by
  email still make email grants that never reach a member (C-12); `addGrant` sends no share
  email and the invitation email names no role or end date (C-13); no code calls
  `audit::events::sign_in`, so no space logs sign-ins (C-14); the accept page can't name the
  space (C-9). A Member manages everything in a team space; a view-only member role is a
  product question for the architect panel. No folder upload control.
- **SpiceDB freshness inside a request.** A request reads at least as fresh as its tenant's
  newest token at its start and its own writes (decision 0002-4). Work that a handler spawns on
  another task (unless it uses `authz::in_current_request`), the outbox worker and `seed-dev`
  have no request floor and read at the process token. Concurrent writers outside a request can
  overwrite the process token with an older one; the suites hit this before the integration fix.
- **Coalescing.** Saves by the same actor within 10 minutes merge into the head version (design
  5.3), also through the CLI, WebDAV and MCP. Name a version (`setVersionLabel`) to keep it. The
  WebDAV ETag of a file is `"<head version>-<revision>"`, because a merged save keeps the version
  ID.
- **Request timeout.** `RUUM_REQUEST_TIMEOUT_SECONDS` (default 120) also covers the body of a
  large WebDAV `PUT` or `putContent` on a slow link. Chunked uploads are not affected.
- In-memory `TenantStore::usage` reports 0 bytes and 0 nodes (Postgres reports real usage).
- Passkeys, Google and Microsoft sign-in are only in the manual checklist of `docs/TESTING.md`.
- The WebDAV example `dav_fake_server` keeps its name but now runs on `Services::in_memory`.
- CI (`.github/workflows/`) has not run on GitHub; nothing is pushed.
- Older gaps of `docs/FOUNDATION.md` still apply (`node.sharing_updated` is not undone, the
  `idp` hint of `loginStart` is not sent, the in-memory `Arc` cycle).

## Dependencies added in wave B

- `unicode-normalization` (workspace, nodes area: NFC names).
- ruum-core uses the existing workspace dependencies `similar`, `quick-xml` and `image` (diff
  plugins). ruum-mcp, ruum-cli and ruum-webdav use workspace dependencies only (`rmcp`, `clap`,
  `dav-server`, ...; see their `Cargo.toml`). Web: `@noble/hashes` (BLAKE3 in the browser).
