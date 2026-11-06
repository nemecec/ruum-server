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
