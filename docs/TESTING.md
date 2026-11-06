# Testing

This page explains the CI gates and the security test harness. The design rules are in
`docs/DESIGN.md`, sections 15, 18.2 and 18.5.

## CI gates

`.github/workflows/ci.yml` runs on every pull request and on `main`. Each job is a required check.

| Job | What it checks |
|---|---|
| `rust-lint` | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` |
| `rust-test` | the migrations apply to an empty Postgres 17 (twice), unit and conformance tests with `DATABASE_URL` |
| `security-suites` | the tenant isolation suite and the permission matrix (`crates/ruum-testkit`) |
| `cargo-deny` | advisories, licenses, banned crates and sources (`deny.toml`) |
| `contracts` | Redocly lint of `contracts/openapi.yaml`, `zed validate` of the SpiceDB schema, and an `oasdiff` breaking-change diff against the base branch |
| `web` | `npm ci`, the generated client `web/src/api/schema.d.ts` is up to date, lint, build |
| `e2e` | placeholder until the e2e suite (`tests/e2e`) exists; the Compose stack is in `deploy/dev` |

A breaking API change fails `contracts`. A contract pull request that a person approved carries the
label `contract-change`; then the diff step is skipped (design section 18.1).

`.github/workflows/nightly.yml` runs the mutation check, a progress run of the suites with
`RUUM_REQUIRE_IMPLEMENTED=1`, and placeholders for the staging e2e suite and the restore test.

## Run the checks on your machine

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
cargo deny check
cargo test -p ruum-testkit --test isolation --test permissions -- --nocapture   # the report
RUUM_REQUIRE_IMPLEMENTED=1 cargo test -p ruum-testkit --test isolation          # pending = fail
crates/ruum-testkit/scripts/mutation_check.sh
```

## The security harness (`crates/ruum-testkit`)

| File | What it is |
|---|---|
| `src/contract.rs` | reads every operation (method, path, `operationId`, public or not) from `contracts/openapi.yaml` |
| `src/fixtures.rs` | two tenants A and B with the same tree, a principal for every role, and `seed` |
| `src/client.rs` | sends a request as any principal through the `ruum-api` router, in memory |
| `src/requests.rs` | the standard request of every operation against one tenant, with a valid body |
| `src/isolation.rs` | the tenant isolation suite: one registered case per operation |
| `src/permissions.rs` | the permission matrix: one rule per operation, plus the contract-wide `Users only` rules |
| `src/matrix.rs` | the user-only operations (kept equal to `x-ruum-user-only` in the contract by a test), the share-link operations, and the extra cases of the step 1 contract review |
| `permission_matrix.md` | the matrix as a table (principal x operation), generated from the rules |
| `scripts/mutation_check.sh`, `mutations/` | the mutation check |

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
- Extra probes for IDs of A in the body or in a second path parameter (move into A's folder, attach
  A's tag, A's version under B's file, and so on).
- Lists, search and the changes feed in B's own tenant: any answer, but no data of A.

Every answer is searched for the markers of tenant A (IDs, names, emails, tokens) that the request
did not send. For writes, a read by tenant A's principal must give the same answer before and after
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
`target/mutation.work/repo`, breaks one security rule per file in `crates/ruum-testkit/mutations/`,
and runs the suites that the mutation names. The suites must fail.

| Result | Meaning |
|---|---|
| `KILLED` | the suites failed, as they must |
| `SURVIVED` | the suites did not notice the break: a hole in the suites |
| `SKIP` | the target file or the pattern does not exist yet (a placeholder) |
| `BROKEN` | the mutated code does not compile: fix the mutation |

While handlers answer `501`, `SURVIVED` and placeholder `SKIP` are warnings. With
`MUTATION_STRICT=1` they fail the run; turn it on when step 1 is built.

### How to add a mutation

Add a file `crates/ruum-testkit/mutations/<name>.sh`:

```sh
# What the mutation breaks, and the security rule.
DESCRIPTION="node queries drop the tenant filter"
FILE="crates/ruum-core/src/nodes/postgres.rs"   # relative to the repository root
PERL='s/tenant_id\s*=\s*\$\d+\s+AND\s+//g'        # applied with perl -0pi -e; must change FILE
TESTS="-p ruum-testkit --test isolation"         # cargo test arguments that must fail
PLACEHOLDER=1                                    # 1 while FILE or the pattern does not exist yet
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
