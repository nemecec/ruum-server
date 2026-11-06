# The foundation of step 1

This page says what exists on `main` before the parallel work of step 1 starts
(`docs/DESIGN.md`, section 18.8, tasks 1 to 4), how to run it, and what is still missing. The
work areas are in `docs/STEP1.md`.

## What exists

| Part | Where | State |
|---|---|---|
| Contracts | `contracts/` | OpenAPI (93 operations), SpiceDB schema and its tests, event JSON Schemas, MCP tool list. Reviewed once (commit `151a5fd`); architect decisions in `docs/decisions/0001-step1-contract-decisions.md`. See `contracts/README.md`. |
| Migration | `migrations/0001_core.sql` | All step 1 tables, row-level security, the cross-tenant link trigger. Applies to Postgres 17 of the stack. |
| Rust workspace | `crates/` | `ruum-core` (types, traits, in-memory implementations), `ruum-api` (router; every operation has a route that answers `501`), `ruum-server` (binary), `ruum-mcp`, `ruum-webdav`, `ruum-cli` (stubs), `ruum-testkit`. |
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

## State of the checks

Checked on 2026-11-06 on `main`:

- `cargo build --workspace`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`: pass.
- `cargo test --workspace`: 23 tests pass. Isolation suite: 178 checks, 1 passed, 177 pending.
  Permission matrix: 1246 checks, 14 passed, 1232 pending. No failures.
- `RUUM_REQUIRE_IMPLEMENTED=1 cargo test -p ruum-testkit`: fails, as expected, with 88 pending
  operations (only `health` is built).
- `cargo deny check`: advisories, bans, licenses and sources ok.
- Migration `0001_core.sql` applies to a new database on the stack Postgres (25 tables, plus PostGIS `spatial_ref_sys`;
  extensions `postgis`, `vector`, `pg_trgm`), also through the server's sqlx migrator.
- `scripts/dev.sh status`: all 8 services healthy.
- `npm run build` in `web/`: pass; `gen:api` gives no diff.
- Mutation check: `errors-answer-ok` killed; `authz-require-allows-all` and
  `effective-role-ignores-scope` survive (expected while handlers answer `501`); the two Postgres
  mutations skip (no Postgres code yet).

## Known gaps

- Every handler except `health` answers `501`. No Postgres implementation of any trait exists yet.
- `fixtures::seed` makes placeholder IDs and tokens. The identity, tenants, nodes and sharing
  areas must make it create real data and credentials; until then an implemented handler that
  checks the token fails the suites with a `401` that points to `seed`.
- `scripts/dev.sh seed` creates only the databases; the tenant and user seed comes with the core
  service.
- The Zitadel app `ruum-web` is a public client (PKCE, no secret). The server's sign-in
  (`/v1/auth/login`, `/v1/auth/callback`) may need a confidential client; the identity area
  decides. `RUUM_OIDC_CLIENT_SECRET` is not set by `dev.sh env`.
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
