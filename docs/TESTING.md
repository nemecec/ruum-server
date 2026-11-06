# Testing

This page explains the CI gates and the security test harness. The design rules are in
`docs/DESIGN.md`, sections 15, 18.2 and 18.5.

## CI gates

`.github/workflows/ci.yml` runs on every pull request and on `main`. Each job is a required check.

| Job | What it checks |
|---|---|
| `rust-lint` | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` |
| `rust-test` | the migrations apply to an empty Postgres 17 (twice), unit and conformance tests with `DATABASE_URL` |
| `security-suites` | the tenant isolation suite and the permission matrix (`crates/ruum-testkit`), on the in-memory services and on Postgres (a service container; the suites connect as a role without superuser, so row-level security applies) |
| `cargo-deny` | advisories, licenses, banned crates and sources (`deny.toml`) |
| `contracts` | Redocly lint of `contracts/openapi.yaml`, `zed validate` of the SpiceDB schema, and an `oasdiff` breaking-change diff against the base branch |
| `web` | `npm ci`, the generated client `web/src/api/schema.d.ts` is up to date, lint, build |
| `e2e` | placeholder until the e2e suite (`tests/e2e`) exists; the Compose stack is in `deploy/dev` |

A breaking API change fails `contracts`. A contract pull request that a person approved carries the
label `contract-change`; then the diff step is skipped (design section 18.1).

`.github/workflows/nightly.yml` runs the mutation check (with a Postgres service), a progress run of the suites with
`RUUM_REQUIRE_IMPLEMENTED=1`, and placeholders for the staging e2e suite and the restore test.

## Run the checks on your machine

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
cargo deny check
cargo test -p ruum-testkit --test isolation --test permissions -- --nocapture   # the report
RUUM_REQUIRE_IMPLEMENTED=1 cargo test -p ruum-testkit --test isolation          # pending = fail
eval "$(scripts/dev.sh env)"                                                     # DATABASE_URL
RUUM_SUITE_BACKEND=postgres cargo test -p ruum-testkit --test isolation --test permissions -- --nocapture
MUTATION_STRICT=1 crates/ruum-testkit/scripts/mutation_check.sh
```

## The security harness (`crates/ruum-testkit`)

| File | What it is |
|---|---|
| `src/contract.rs` | reads every operation (method, path, `operationId`, public or not) from `contracts/openapi.yaml` |
| `src/backend.rs` | the backend of the suites: in-memory services or the Postgres stores (`RUUM_SUITE_BACKEND`) |
| `src/fixtures.rs` | two tenants A and B with the same tree, a principal for every role, `seed`, and the bridge user (`seed_bridge`) |
| `src/client.rs` | sends a request as any principal through the `ruum-api` router, in process |
| `src/requests.rs` | the standard request of every operation against one tenant, with a valid body |
| `src/isolation.rs` | the tenant isolation suite: one registered case per operation |
| `src/permissions.rs` | the permission matrix: one rule per operation, plus the contract-wide `Users only` rules |
| `src/matrix.rs` | the user-only operations (kept equal to `x-ruum-user-only` in the contract by a test), the share-link operations, and the extra cases of the step 1 contract review |
| `permission_matrix.md` | the matrix as a table (principal x operation), generated from the rules |
| `scripts/mutation_check.sh`, `mutations/` | the mutation check |

### Backends

The suites run on the backend that `RUUM_SUITE_BACKEND` names. The report names it in its title,
for example `tenant isolation suite [postgres (memory authz)]`.

| Variable | Values | What |
|---|---|---|
| `RUUM_SUITE_BACKEND` | `memory` (default), `postgres` | the in-memory services, or the Postgres stores |
| `RUUM_SUITE_AUTHZ` | `memory` (default), `spicedb` | the authz engine of the Postgres backend; `spicedb` uses `RUUM_SPICEDB_PRESHARED_KEY` or `SPICEDB_PRESHARED_KEY` |
| `RUUM_SUITE_RLS` | `on` (default), `off` | `off` turns `FORCE ROW LEVEL SECURITY` off in the suite databases |
| `RUUM_SUITE_JOBS` | a number; default 1 (memory), 8 (Postgres) | worlds that run at the same time |

With `postgres`, the suites need `DATABASE_URL` (`eval "$(scripts/dev.sh env)"`) and a role that
may create databases. Each test makes a fresh template database `ruum_suite_<time>_<random>_tpl`
and applies `migrations/` to it. The isolation suite gives every case a database of its own (a
copy of the template), so only tenants A and B exist there. The permission suites share one copy
per run; each world seeds new tenants and users. The stores are wired as in `ruum-server`
(`crates/ruum-server/src/wiring.rs`); chunks stay in memory, and each world has a new in-memory
authz engine. The databases are dropped at the end of the test. A later run drops the databases
of runs that stopped early, after two hours.

Row-level security: the policies are FORCEd, so they hold the table owner too, unless the role is a
superuser. The `ruum` role of the laptop stack is no superuser. A query that forgets its
`tenant_id` filter is then still held by the policy (defense in depth). `RUUM_SUITE_RLS=off` turns
that layer off, so the suites test the filters of the queries alone; the tenant-filter mutations
use it.

On the laptop stack, one Postgres run takes about 30 s (isolation) and 6 min (permissions).

### Outcomes

Each check is **passed**, **pending** or **failed**. A handler that answers `501 not_implemented` is
pending. The suites pass with pending checks, so they can run from the first day. Set
`RUUM_REQUIRE_IMPLEMENTED=1` to count pending checks as failures; the report then lists every
operation that is not built yet. A data leak, a server error, a missing route or a wrong status is a
failure in both modes.

An operation of the contract without a registered case or rule fails the suite. So a new endpoint
must come with its cases.

**Planned operations** (decision 0004-21). An operation marked `x-ruum-status: planned` in
`contracts/openapi.yaml` is specified but not built. It still needs its standard request
(`requests.rs`), its isolation case and its permission rule, and it is listed in
`matrix::PLANNED_OPERATIONS` (a test keeps that list equal to the marks). While its route is
missing or answers `501`, its checks are **planned**: they pass in both modes, also with
`RUUM_REQUIRE_IMPLEMENTED=1`. A data leak in any answer (a marker of tenant A, or a node the
principal cannot see in a refusal or a `501`) still fails. The task that builds the operation
removes the mark and the list entry; `tests/contract.rs` (`every_operation_is_routed`) fails when
a planned operation answers anything but a missing route or `501`, so a built operation cannot
keep the mark. The MCP server does not offer tools of planned operations.

### The tenant isolation suite

The rule: a principal of tenant B acting on a resource of tenant A gets `404`, never data. For each
operation, the case sends probes as tenant B's owner:

- A's resource IDs with B's `Ruum-Tenant` header: `404`.
- A's resource IDs with A's `Ruum-Tenant` header (B claims to be in A): `403` or `404`.
- The bridge user (`fixtures::seed_bridge`: a member of B and a guest of A with `manage` on A's
  `Projects`) sends A's resource IDs with B's `Ruum-Tenant` header: the same status as B's owner,
  and no data of A. The authz engine lets the bridge act on A's nodes, so only the tenant checks
  of the stores (the `tenant_id` filter of every query) give the `404`. A control check
  (`(fixture)`) makes sure that the bridge does read A's plan in tenant A. The bridge also lists,
  searches and reads the whole feed in tenant B: no data of A.
- Extra probes for IDs of A in the body or in a second path parameter (move into A's folder, attach
  A's tag, A's version under B's file, and so on).
- Lists, search and the changes feed in B's own tenant: any answer, but no data of A.

Every answer is searched for the markers of tenant A (IDs, names, emails, tokens, and the name
prefix `alpha-`) that the request did not send. For writes, a read by tenant A's principal must give the same answer before and after
the probe.

### The permission matrix

The principals of tenant A: `anonymous`, `owner`, `admin`, `member`, `guest`, guests with one grant
on `Projects` (`guest_view` to `guest_manage`), the bearer of a `view` share link on `Projects`
(`share_link`), and three agents. Roles are `view < comment < edit < manage`; `share` is a
permission (decision 0001-1, `Who::can_share`): `manage` always, `edit` where the node setting
`editors_can_share` is true (the fixture sets it to false on `Archive`). Members manage the team
tree through the team root and, by the default tenant policy `members_can_share`, share there
too. `agent` acts for `member` with scope
`Projects` and `max_role: view` (the narrow agent). `agent_out` has scope `Private`.
`agent_of_guest_view` has a wide scope but acts for `guest_view`, so it never gets more than `view`.

An agent's right is `user right ∩ agent scope` (design 3.3), computed in `Who::role_on`. A
principal that cannot see a node gets `404`; one that can see it but lacks the role gets `403`.
List operations also check that nodes the principal cannot see are not in the answer.

Two rules from the `Users only` section of the contract hold for every operation
(`permissions::contract_wide`): an agent gets `403 user_required` on every operation marked
`x-ruum-user-only`, even when its user is allowed; a share-link bearer gets `403 user_required`
on everything except the public operations and the node, content, version and diff reads of
`matrix::SHARE_LINK_OPERATIONS`.

When you change a rule, run
`RUUM_UPDATE_MATRIX=1 cargo test -p ruum-testkit --test permissions` and review the diff of
`permission_matrix.md`. Rules with "assumption" or "open question" in the note are readings of the
contract that the architect must confirm. Decision 0002-5 settled all such rules of step 1; their
notes say "decided (0002-...)".

### Seeding: what the auth area must do

While the core services are stubs, `fixtures::seed` makes only IDs and placeholder bearer tokens.
When the identity, tenants, nodes and sharing areas exist, `seed` must create both tenants, the
principals, the grants and the resources through `Services`, and issue real credentials (tokens,
sessions, agent key exchange). It is the only function to change. Until then, an implemented
handler that checks the token answers `401`, and the suites report that as a failure that points to
`seed`.

### How to add a case for a new endpoint

1. Add the contract change (`contracts/openapi.yaml`).
2. Add the standard request in `src/requests.rs`.
3. Add the isolation case in `isolation::case` (usually `addressed(...)` or `own_tenant(...)`, plus
   probes for IDs of the other tenant in the body).
4. Add the rule in `permissions::rule`, then regenerate `permission_matrix.md`.

## Property tests

`crates/ruum-core/tests/properties/` holds the property tests of design section 18.2, written
with `proptest`. Each module states its model at the top. The default case counts keep the
whole binary at a few seconds; set `PROPTEST_CASES` to run more of every test.

```sh
cargo test -p ruum-core --test properties
eval "$(scripts/dev.sh env)"                     # adds the SpiceDB comparison
PROPTEST_CASES=1000 cargo test -p ruum-core --test properties feed
```

| Module | Cases | The property |
|---|---|---|
| `authz` | 48, 48, 48 | A random team world built through `Services::in_memory` (tree of folders, files, shortcut and member links; grants to users and to all members; share links; `editors_can_share`; `members_can_share`; agents with a subtree and a highest role). `authz::require` and `authz::role_on` agree with a reference model in the test for every principal, node and permission, including the error (`not_found`, `permission_denied`, `scope_exceeded`). Model-free: an agent never has a permission or role that its user lacks; a share link never has `manage` or `share`; dropping every `collection` tuple changes a role only from `view` to none. A member link is refused exactly when it would close a loop. |
| `authz` (SpiceDB) | 8 | The relationships that the stores wrote give the same answers in SpiceDB as in the in-memory engine (every user and link subject, every node, `view` to `share` and `editors_cannot_share`). Skipped without `SPICEDB_PRESHARED_KEY`; deletes its tuples afterwards. |
| `feed` | 32 | A random sequence of node operations (create, member links, rename, move, copy, trash, restore, purge, grants to a guest added, changed and removed, `editors_can_share`). For the owner, a guest and an agent of the guest: reading from any cursor with any page size gives the entries and resync markers of one big read, each once and in order; an entry on a node the observer sees now is sent; a sent entry is about a node (or a parent of it) that the observer saw at some moment, or is the documented grant rule; markers for grants to the caller and for moves across the edge of the view. |
| `chunking` | 24, 24, 8 | FastCDC (`SMALL_CHUNKING`): chunks cover the data in order with valid sizes and hashes, and join to the same bytes and BLAKE3; after an insert, chunks that end before it keep their offset and chunks that start 4 maximum chunks after it keep their hash; manifests encode and decode; an upload through the content store downloads as the same bytes. |
| `diff` | 256, 256, 256, 256, 128 | Text: the line changes applied to the old text give the new text (after `\r\n` and final-newline normalisation). JSON: the changes applied to the old value give the new value. Identical inputs give no changes (text, also with `\r\n`; JSON, also in another layout; XML; binary). |

When a property fails, `proptest` shrinks the case and saves its seed in
`crates/ruum-core/tests/proptest-regressions/`; the seed runs first on every later run, so keep
these files in git. A real bug gets a fix in a separate `fix:` commit with the shrunk case as a
plain regression test next to the code.

Bugs found so far:

- A member link inside its own target (directly, or through other member links) made a loop of
  `view` in the graph; every check that walked it failed ("check is too deep" in memory, "max
  depth exceeded" in SpiceDB), so the target answered `500`. The stores now refuse such links on
  create, move and restore (`422 validation_failed`) and copy them as shortcuts
  (`nodes::rules::member_link_loop`; regression: `nodes::conformance::member_link_loops`).
- The changes feed gave an agent the `grant.updated` and `grant.removed` entries (and a `lost`
  marker) of its user's grants outside the agent's scope (regression:
  `journal::feed::tests::an_agent_gets_no_grant_entries_outside_its_scope`).

Open question for the architect: by the documented "before" rule, the grantee of a removed or
changed grant gets that entry even when it never saw the node, for example a grant on a node in
the trash (only managers see trashed nodes). The `feed` property accepts this as documented.

## The mutation check

`crates/ruum-testkit/scripts/mutation_check.sh` copies the repository to
`<MUTATION_TARGET_DIR>.work/repo` (default `target/mutation.work/repo`), breaks one security rule per file in `crates/ruum-testkit/mutations/`,
and runs the suites that the mutation names. The suites must fail.

| Result | Meaning |
|---|---|
| `KILLED` | the suites failed, as they must |
| `SURVIVED` | the suites did not notice the break: a hole in the suites |
| `SKIP` | the target file or the pattern does not exist yet (a placeholder) |
| `BROKEN` | the mutated code does not compile: fix the mutation |

While handlers answer `501`, `SURVIVED` and placeholder `SKIP` are warnings. With
`MUTATION_STRICT=1` they fail the run. Step 1 is built: run it strict. The mutations with
`ENV="RUUM_SUITE_BACKEND=postgres ..."` need `DATABASE_URL`.

The tenant-filter mutations (`nodes-tenant-filter`, `sharing-tenant-filter`,
`tags-tenant-filter-postgres`, `versions-tenant-filter-postgres`,
`journal-tenant-filter-postgres`) drop the `tenant_id = $n and` of the Postgres queries and run
the isolation suite on Postgres with `RUUM_SUITE_RLS=off`. Two other layers hide such a break:

- the authz engine: B's owner has no right on A's nodes. Only the bridge user probes reach the
  queries with A's IDs in tenant B.
- row-level security: with it on, the policy holds the query. That layer is tested by running
  the suites with it on, not by these mutations.

The version statements alone are an equivalent mutation: each runs after the file lookup of the
content store (`node_row`) found the file in the caller's tenant, and binds its `node_id`, which
the foreign key ties to the same tenant. So `versions-tenant-filter-postgres` drops the filter of
`node_row` too.

### How to add a mutation

Add a file `crates/ruum-testkit/mutations/<name>.sh`:

```sh
# What the mutation breaks, and the security rule.
DESCRIPTION="node queries drop the tenant filter"
FILE="crates/ruum-core/src/nodes/postgres.rs"   # relative to the repository root
PERL='s/\b(?:\w+\.)?tenant_id\s*=\s*\$\d+\s+and\s+//gi'  # applied with perl -0pi -e; must change FILE
TESTS="-p ruum-testkit --test isolation"         # cargo test arguments that must fail
ENV="RUUM_SUITE_BACKEND=postgres RUUM_SUITE_RLS=off"  # optional: environment of the test runs
PLACEHOLDER=0                                    # 1 while FILE or the pattern does not exist yet
```

Run it alone with `crates/ruum-testkit/scripts/mutation_check.sh <name>`.
If you source a mutation file by hand instead, do it in bash: zsh does not word-split an unquoted `$TESTS`, so `cargo test $TESTS` fails on its arguments and every mutation looks `KILLED`.

## Contract gaps found by the harness

- `listInvitations`, `createInvitation` and `listAudit` have no `404` response in the contract. The
  isolation suite accepts `403` there. A contract change should add `404`, so another tenant's
  admin routes answer like unknown ones.
- `registerAgent` has no `404`; a scope subtree in another tenant is accepted as `403`, `404` or
  `422`.
- `logout` and `listAgents` with an agent caller: settled by decision 0002-5 (`200`; an agent
  sees only itself in `listAgents`). The step 1 contract review settled `listMyIdentities` and
  `listMySessions`: users only.
- `getContentUrl` has no `403` response in the contract; an agent whose user can see the node but
  whose scope leaves it out is accepted as `403` or `404`.
- `oauth_clients` has no `tenant_id` (a registration belongs to no tenant), so the isolation case
  of `getOAuthClient` only checks that the answer holds no data of tenant A.
- The standard upload names the chunking set `fastcdc-2020-16k-32k-64k` (an example of the
  contract); change `requests::CHUNKING` if the server publishes other IDs.

## Browser end-to-end tests (`web/e2e`)

Playwright specs drive the real web UI in Chromium against the real server and the laptop stack.
Nothing is mocked: sign-in goes through Zitadel, files go to Postgres and Garage, rights go to
SpiceDB, mails go to Mailpit.

```sh
scripts/dev.sh up                    # the stack must be healthy
scripts/e2e_browser.sh               # build, seed, server on 8492, Vite on 5182, all specs, stop
scripts/e2e_browser.sh sharing       # extra arguments go to `playwright test`
RUUM_E2E_WEB=preview scripts/e2e_browser.sh   # the built files (`vite build`) instead of Vite dev
```

The script prints its work folder. It holds the server and Vite logs, the HTML report
(`npx playwright show-report <folder>/report`) and a trace of each failed test. In a git worktree,
the script uses the stack settings (`deploy/dev/.env`, `.state`) of the main worktree, or of
`RUUM_DEV_ROOT`. The CI job `e2e-browser` (`.github/workflows/ci.yml`) runs the same script after
`scripts/dev.sh up`; it has not run on GitHub yet.

How the run is set up:

- The server runs with `RUUM_PUBLIC_URL=http://localhost:5182`,
  `RUUM_OIDC_REDIRECT_URI=http://localhost:5182/auth/callback` and
  `RUUM_CONTENT_URL=http://content.localhost:8492` (Chromium sends `*.localhost` to the loopback
  address, so downloads through the content domain work without Caddy).
- `web/e2e/global-setup.ts` adds the callback of port 5182 to the `ruum-web` client in Zitadel
  (idempotent), and makes three new Zitadel users with a verified email and a random password
  (Ann, Ben, Cara) through the management API, as the bootstrap service account of
  `scripts/dev.sh`. The users are in `<work folder>/run.json` (mode 0600).
- Sign-in is the real OIDC flow: the sign-in page of the UI, `loginStart`, the Zitadel login UI
  (login name, password, "Skip" on the 2-factor prompt), `loginCallback`, the session cookie. The
  `setup` project signs in Ann and Ben and keeps their browser state for the other specs.
- `ruum-server seed-dev` runs too, as a check that the stack and the token key work; the browser
  specs use their own users, because the seed users have no Zitadel login.

| Spec | What it checks |
|---|---|
| `auth.setup.e2e.ts` | sign-in through Zitadel, the personal space, `ruum_session` (HttpOnly) and `ruum_csrf`, `return_to`, sign-out |
| `files.e2e.ts` | new folder, a 3 MB upload in many chunks, download through the content domain with the same SHA-256, rename (the name without its extension is selected), move with the folder picker, trash, restore from the Trash page; a second upload of the same bytes sends no chunks |
| `versions.e2e.ts` | a labelled version, "Upload new version", the text diff (`ins`/`del`), restore, the restored content |
| `sharing.e2e.ts` | Ann shares a folder by email, invites Ben as a guest (mail in Mailpit), Ben accepts and sees the folder with "Can view" only (no upload, rename or trash; the server answers `403` to a write), Ann removes the grant and Ben gets `404` |
| `agents.e2e.ts` | register an agent, create a key (shown once), the agent works through the API with the key, its activity, "last used", the actor of the change, "Undo all" |
| `admin.e2e.ts` | a new team, the members table, an invitation as member (mail in Mailpit), the new member, a role change, the audit log |

Rules for new specs: file names end in `.e2e.ts` (Vitest ignores them); find elements by role and
name, as a person or a screen reader does; give every folder a new name (`uniqueName`), because
all specs share Ann's space; wait for the page of a folder (its heading) before you act in it.

### Known gaps found by the browser tests

The specs mark these with `test.fixme`. Each has the steps that must pass once it is fixed; remove
the `fixme` then.

| Gap | Where | Spec |
|---|---|---|
| A cold load of a protected page loses `return_to`: `/v1/me` and `/v1/tenants` both answer `401`, and the second redirect runs `signInPath()` on `/signin`. | `web/src/app/session.tsx` | `auth.setup.e2e.ts` |
| The mail of an invitation links to `/invitations/accept?token=...`; the web UI has no such page ("Page not found"). The specs accept through the API. | `web/src/app/routes.tsx` | `admin.e2e.ts` |
| An email grant to a person who already has an account never binds: a known identity returns before the binding at sign-in, and `addGrant` sends no invitation mail, although its contract says both. | `ruum-core` `identity/postgres.rs`, `sharing/postgres.rs` | `sharing.e2e.ts` |
| An email grant that binds at a first sign-in does not make the person a guest. "Shared with me" lists the folder ("Another tenant", no link), but every read answers `404`. | `ruum-core` sharing and tenants | `sharing.e2e.ts` |
| The name field of "New folder" and "Rename" does not get the focus: React's `autoFocus` runs before `showModal()`, which focuses the close button. | `web/src/components/ui.tsx` | `files.e2e.ts` |
| While a folder loads after a click, the page keeps the toolbar of the previous folder; an upload started then lands in the previous folder. | `web/src/pages/files/FilesPage.tsx` | `files.e2e.ts` |

Smaller gaps that the specs accept as they are (a comment in the spec says so): the share dialog
shows a user grantee by ID (`listGrants` gives no name or email); the file list and "Shared with
me" show an agent or a sharer by the first 8 characters of the ID (no `display_name` on the
actor); the tenant switcher labels every personal tenant "(personal)", also when the user is a
guest there.

### Manual checklist: passkeys, Google and Microsoft

These need real devices or real accounts, so no automatic test covers them. A person runs them
before a release, on the laptop stack, with the web UI on `http://localhost:5182` (or
`https://ruum.localhost`). It is optional. Today every sign-in button opens the Zitadel login
page, because the server does not send the `idp` hint yet (docs/STATUS.md); choose the method there.

Passkey (WebAuthn works on `localhost` without TLS):

1. Sign in once with a password user, open the Zitadel console (`http://localhost:8180/ui/console`),
   go to your user, and add a passwordless (passkey) authenticator: Touch ID, Windows Hello, or a
   security key.
2. Sign out of Ruum. Click "Sign in with a passkey", enter the login name, and choose the passkey.
3. Check: you land on your files; `/v1/me` shows the same user as before; Account > Sign-in
   methods lists the identity.
4. Cancel the passkey prompt once. Check: the Zitadel page offers the password again, and nothing
   breaks in Ruum.

Google:

1. In Google Cloud, make an OAuth client (web application) with the redirect URI
   `http://localhost:8180/ui/login/login/externalidp/callback`.
2. In the Zitadel console, add a Google identity provider with that client ID and secret, and
   allow it in the login policy.
3. Sign out of Ruum. Click "Continue with Google", choose Google on the Zitadel page, sign in.
4. Check: a new account gets a personal space; an email that Google verified links to an existing
   Ruum user with the same verified email (no second account); an email grant to that address
   binds after the first sign-in.

Microsoft (Entra ID):

1. In Entra ID, register an application with the redirect URI
   `http://localhost:8180/ui/login/login/externalidp/callback` and make a client secret.
2. In the Zitadel console, add a Microsoft identity provider (tenant type as needed) and allow it
   in the login policy.
3. Sign out of Ruum. Click "Continue with Microsoft", choose Microsoft on the Zitadel page, sign
   in with a work or school account and with a personal account.
4. Check the same points as for Google. Also check that an account whose email Microsoft does not
   mark as verified does not claim email grants or invitations.

Remove the identity providers from Zitadel when you are done, if the laptop stack is shared.
