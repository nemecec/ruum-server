# Ruum: rules for people and agents

The design is `docs/DESIGN.md`. It is the source of truth. Decisions are in section 17; do not reopen
them. The working method is section 18.

## Layout

- `contracts/`: the contracts (OpenAPI, SpiceDB schema, event JSON Schemas, manifest schemas).
  Change them only in a contract change, never as a side effect of a feature.
- `crates/`: the Rust workspace (core service, API, MCP, CLI, WebDAV, test kit).
- `migrations/`: SQL migrations (sqlx). One migration change at a time.
- `web/`: the web UI (React + TypeScript + Vite). The API client is generated from `contracts/openapi.yaml`.
- `deploy/dev/`: the laptop stack (Docker Compose). `scripts/dev.sh up|down|seed|logs|clean`.
- `deploy/staging/`: staging as code (OpenTofu, Talos, Helm). `deploy/staging/README.md`. Plans are
  read-only (`up.sh`); applying needs `RUUM_STAGING_APPLY=yes`. Images: `deploy/images/`, `.github/workflows/images.yml`.

## Rust rules

- Rust 2024 edition, stable toolchain. `cargo fmt` and `cargo clippy --all-targets -- -D warnings` must pass.
- axum 0.8, tokio, sqlx with runtime queries only (no compile-time `query!` macros), so the code builds
  without a database.
- Every table has `tenant_id`. Every query filters by it. No query joins two tenants.
- IDs are UUIDv7. Times are `timestamptz` / `time::OffsetDateTime` in UTC.
- Every write records `actor` and `on_behalf_of` (section 3.4 of the design).
- Errors in the API: JSON `{ "error": { "code": "snake_case_code", "message": "...", "hint": "..." } }`.
- Module borders are traits (storage, blob store, authz, runtime driver). Each trait has an in-memory
  implementation for unit tests and a real one; one conformance suite tests both.
- Use workspace dependencies (`[workspace.dependencies]` in the root `Cargo.toml`). Do not add a new
  dependency without saying so in the summary of your change.

## Tests

- Database tests use `#[sqlx::test]` against `DATABASE_URL` of the dev stack; each test gets its own
  database, so parallel work does not clash.
- The tenant isolation and permission suites in `crates/ruum-testkit` must pass for every endpoint.
- Do not change acceptance tests to make them pass. If a test looks wrong, stop and say so.

## Writing

Docs and comments: plain English, short sentences, active voice.

## Commits

Author: Neeme Praks <neeme@praks.net> (set in the local git config). Do not add `Co-Authored-By` trailers.
Keep the history linear: no merge commits. Integrate a branch with `git rebase main` and a fast-forward
(`git merge --ff-only`), or as one squashed commit when its own commits are noise.
- CI file: a card may add lines to `.github/workflows/ci.yml` that its own tests need (role, service, variable, suite line, require flag), with a comment naming the card; the reviewer checks them; new jobs, triggers, permissions or secrets need their own card (decision 0008-3).
