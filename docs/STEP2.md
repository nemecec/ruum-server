# Step 2 (Databases): work areas, task cards and order

Status: DRAFT for review by the architect panel (design section 18.9). Nothing here is decided
until the panel accepts it; the result is a `provisional` decision record.

This page splits step 2 of `docs/DESIGN.md` (section 16) into work areas and task cards for the
swarm (section 18.3). It follows the decisions of section 17.3:

| # | Decision | What it means for this step |
|---|---|---|
| 14 | Postgres (shared plan) and SQLite | Two engines behind one trait. No other engine. |
| 15 | No access from outside in the first version | **No connection proxy.** Only the HTTP query API (and, in step 3, Ruum apps through their bindings). |
| 16 | Dedicated plans later | Only `shared-small`. |
| 17 | Point-in-time restore for 7 days | WAL archiving of the shared cluster; Litestream-style replication of SQLite; restore points. |
| 18 | Branches for small databases | `CREATE DATABASE ... TEMPLATE` and a file copy, up to a size limit. |
| 19 | The platform runs declared migrations and rolls back on failure | `applyDatabaseMigration`, one transaction, restore point first. |

## The contracts of step 2 (drafts on `f/step2spec`)

| Contract | File |
|---|---|
| Provider contract (typed nodes) | `contracts/providers/README.md`, `contracts/providers/provider.openapi.yaml` |
| Database manifest | `contracts/manifests/database.schema.json` |
| REST API | `contracts/openapi.yaml`, tag `databases` (22 operations, each with `x-ruum-area`) |
| MCP tools | `contracts/mcp/tools.yaml` (18 tools; the 4 user-only operations are not exposed) |
| SpiceDB schema | `contracts/authz/schema.zed` (database permissions and agent rules), `schema.test.yaml` |
| SQL | `migrations/0002_databases.sql` |
| Journal events | not drafted yet: card C-2 |

The step 1 operations are unchanged. `oasdiff breaking` (CI's check, `--fail-on ERR`) reports 0
errors; the warnings are the new values of the response enum `ErrorCode`.

### Operations by area

| Area | Operations |
|---|---|
| `databases` | `createDatabase`, `getDatabase`, `updateDatabaseManifest`, `listDatabaseMigrations`, `applyDatabaseMigration`, `listDatabaseBranches`, `createDatabaseBranch`, `listDatabaseRestorePoints`, `createDatabaseRestorePoint`, `restoreDatabase` (user only) |
| `query` | `queryDatabase`, `getDatabaseSchema`, `listDatabaseQueries` |
| `classes` | `listDatabaseColumns`, `suggestColumnClasses`, `confirmColumnClasses` (user only) |
| `access` | `listDatabaseAccessGrants`, `createDatabaseAccessGrant` (user only), `revokeDatabaseAccessGrant`, `listDatabaseApprovals`, `getDatabaseApproval`, `decideDatabaseApproval` (user only) |

### Permission table for database nodes (design 3.2, 8.4, 8.5)

| Action | Person | Agent (user's right ∩ scope, plus) |
|---|---|---|
| Read-only query | `view` | `view`; `secret` hidden, `personal` as pseudonyms unless `read_personal` grant |
| Read-write query | `edit` | branch: `edit`; live: `edit` + `write_live` grant |
| Destructive statement | as the statement needs | always a pending approval that a person decides |
| See `secret` values | `manage` | never |
| Settings, migrations on live | `manage` | `manage` + `write_live` grant; may not change `data_classes` |
| Migrations on a branch | `edit` | `edit` |
| Branch with `masked` / `schema_only` data | `edit` | `edit` |
| Branch with `full` data | `manage` | never (`403 user_required`) |
| Restore points (name one) | `edit` | `edit` |
| Point-in-time restore | `manage` (user only) | never |
| Suggest column classes | `edit` | `edit` |
| Confirm column classes | `manage` (user only) | never |
| Give a grant to an agent | the agent's user, `edit` (live writes) or `view` (personal values) | never |
| Query log | `manage` | `manage` (parameters are pseudonyms anyway) |
| Share links | no database operation (`403 user_required`, as in step 1) | |

## Shared files (not owned by any area)

As in step 1 (`docs/STEP1.md`), plus:

| Path | What |
|---|---|
| `contracts/providers/`, `contracts/manifests/` | Contract changes only. |
| `crates/ruum-core/src/providers/mod.rs` | The `Provider` trait (module border). Signature changes need the agreement of the `providers` and `databases` areas. |
| `crates/ruum-dbengine/src/lib.rs` | The `DatabaseEngine` trait (module border). |
| `crates/ruum-core/src/privacy/mod.rs` | The marker types and the `Pseudonymizer` trait; step 3 (telemetry) uses them too. |

## The areas

### 1. Providers (typed nodes)

| | |
|---|---|
| Owns | `crates/ruum-core/src/providers/` (trait, registry, reconciler, deliveries, conformance suite, in-memory test provider) |
| Implements | The platform side of `contracts/providers/provider.openapi.yaml`: typed node create, status, the control loop, hook deliveries from the journal (`provider_deliveries`), `checkAccess` for in-process providers. No HTTP adapter for external providers yet (step 6). |
| Tables | `typed_nodes`, `provider_deliveries`, `versions` with encoding `typed_manifest_v1` |
| Touches | Calls into the nodes area for `copyNode`, `trashNode`, `restoreNode`, `purgeNode` of typed nodes through a hook interface (`nodes::TypedHooks`), agreed with area 1 of step 1 (card P-3). |

### 2. Database engines

| | |
|---|---|
| Owns | `crates/ruum-dbengine/` (new crate): the `DatabaseEngine` trait; `memory` (a fake for unit tests: SQLite in memory), `postgres` (the shared data cluster), `sqlite` (files and replication). Catalog reads, role management, branches, restore points, export. |
| Notes | The engine never sees a platform token. It gets the database name, the role kind to use and the statement. Postgres: the platform connects with its own login role `ruum_dbhost` (CREATEDB, CREATEROLE, no superuser), and per request `SET ROLE` to the database's role (`database_roles`). PostgreSQL Anonymizer masks for the `masked_*` roles. SQLite: one file per database on the db host's disk, opened read-only for read queries, replicated to Garage. |

### 3. Databases (control plane)

| | |
|---|---|
| Owns | `crates/ruum-core/src/databases/{mod,manifest,provider,branches,backups,migrations}.rs`, `crates/ruum-api/src/routes/databases.rs` |
| Implements (OpenAPI) | area `databases` |
| Tables | `databases`, `database_roles`, `database_migrations`, `database_restore_points`, `database_restores` |
| Notes | The database provider (`ruum.database`) implements `providers::Provider` on top of `DatabaseEngine`. Manifest validation: the JSON Schema (`contracts/manifests/database.schema.json`, compiled in) plus the provider rules. Plan limits from configuration (`RUUM_PLAN_LIMITS`, section 4.3: count and size per plan; statement timeout, row cap, result size, branch source limit). |

### 4. Query API and privacy filter

| | |
|---|---|
| Owns | `crates/ruum-core/src/databases/query/`, `crates/ruum-core/src/privacy/`, `crates/ruum-api/src/routes/database_query.rs` |
| Implements (OpenAPI) | area `query` |
| Tables | `database_queries` |
| Notes | One statement per call (parse with `sqlparser` to count statements, find destructive ones and the columns a result reads; new dependency, needs approval). Read-only transaction for `mode: read`; `statement_timeout`; row and size caps. Result JSON with the typed markers (`HiddenValue`, `Pseudonym`, `LiteralValue`). Pseudonyms: HMAC-SHA-256 with a per-tenant key (from OpenBao; in tests a fixed key), `p_` + the first 12 hex characters. Postgres: the masking runs in the database (Anonymizer masking rules generated from `database_columns`, applied to the `masked_*` roles); the query layer still checks that every result column has a known class and treats unknown expressions as `personal`. SQLite: masking in this layer, by the origin of each result column (`sqlite3_column_table_name` / `column_name`). The query log stores parameters as pseudonyms. |

### 5. Column classes

| | |
|---|---|
| Owns | `crates/ruum-core/src/databases/classes/`, `crates/ruum-api/src/routes/database_classes.rs` |
| Implements (OpenAPI) | area `classes` |
| Tables | `database_columns` |
| Notes | Catalog sync after each migration and on a schedule (new columns start unconfirmed = `personal`; dropped columns get `dropped_at`). Suggestions store the newest one per column; every suggestion and confirmation goes to the audit log. Confirmation writes a new manifest version (`data_classes`) and regenerates the Anonymizer rules. A rule-based suggester (names such as `email`, `phone`, `password`, `token`, `name`, `address`, `note`) as the default for existing data; agents can do better and send their own. The digest lists unconfirmed columns. |

### 6. Agent access: grants and approvals

| | |
|---|---|
| Owns | `crates/ruum-core/src/databases/access/`, `crates/ruum-api/src/routes/database_access.rs` |
| Implements (OpenAPI) | area `access` |
| Tables | `database_access_grants`, `database_approvals` |
| Contract | `contracts/authz/schema.zed`: `draft_branch`, `live_writer`, `personal_reader`, `agent_write_live`, `agent_read_personal` |
| Notes | A grant writes its tuple through the outbox and records a restore point first (`write_live`). The core checks the tuple AND the row (`expires_at`, `revoked_at`) on every query; the sweeper deletes the tuple at the end and sends `access_changed`. Destructive statements: `statement_hash` = BLAKE3 of the statement and the canonical parameters; an approval is used once, within 15 minutes. |

### 7. Infrastructure (laptop stack)

| | |
|---|---|
| Owns | `deploy/dev/`, `scripts/dev.sh`, `docs/DEVSTACK.md` |
| Notes | See "Infrastructure work" below. |

### 8. MCP and CLI

| | |
|---|---|
| Owns | `crates/ruum-mcp/`, `crates/ruum-cli/` |
| Implements | The 18 database tools of `contracts/mcp/tools.yaml`; `ruum db create|ls|query|schema|branch|migrate|columns|restore-points`. Query results keep the markers as JSON; the CLI prints them as `<hidden: secret>` and `<person 8f3a91>`. Every tool result with rows says that rows are untrusted data. |

### 9. Web UI

| | |
|---|---|
| Owns | `web/` except `web/src/api/schema.d.ts` |
| Implements | "New database" in a folder; the database page (status, settings, size, limits); a query console (read-only by default, markers as chips "hidden: secret", "person 8f3a91"); the column class list with suggestions filled in and one confirm button; branches; restore points with "Restore to a new database" and "Restore in place"; grants (give, see, revoke) and pending approvals with the statement and the row count. UX package first (section 18.4.1, card U-1). |

### 10. Suites (test harness)

| | |
|---|---|
| Owns | `crates/ruum-testkit/` |
| Implements | Isolation and permission cases for the 22 operations (`USER_ONLY_OPERATIONS` gets the 4 new user-only operations); a **privacy suite**: an agent never gets a raw `secret` value or a raw `personal` value without a grant, through any operation (query, schema, columns, query log, approvals, branches with `masked` data, exports); mutation rules for each privacy and grant rule. |

## Infrastructure work

1. **A data cluster with PostgreSQL Anonymizer.** The shared data cluster is a Postgres server of
   its own, not the platform database (a customer database never shares a server with the
   platform metadata, also on the laptop). Laptop: a new compose service `postgres-data`
   (image `ruum-dev/postgres-data:17`, built from `deploy/dev/postgres-data/Dockerfile`:
   Postgres 17 with PostGIS, pgvector and the `postgresql_anonymizer` package from the PGDG or
   Dalibo repository, `session_preload_libraries = 'anon'`), port 5435, a volume, WAL archiving
   on.
2. **Per-database roles.** An init script creates the login role `ruum_dbhost` with `CREATEDB`
   and `CREATEROLE` (no superuser) and the `anon` extension in `template1`. The engine creates,
   per database, the `NOLOGIN` roles of `database_roles` and `GRANT`s them to `ruum_dbhost`, so
   the platform uses `SET ROLE`; `REVOKE CONNECT ... FROM PUBLIC` on every database. `dev.sh env`
   prints `RUUM_DBHOST_URL`. A check script (`scripts/dbhost_check.sh`) proves that one
   database's role cannot connect to another database.
3. **WAL archiving and point-in-time restore for the shared cluster.** pgBackRest (or WAL-G)
   archives WAL and base backups of `postgres-data` to the Garage bucket `ruum-dbbackups`; 7
   days of retention. Restoring one database: restore the cluster backup to a scratch instance at
   the time, dump the one database, load it into a new database (`new_node`) or swap it
   (`in_place`). A script `scripts/db_restore_test.sh` runs it end to end (the monthly restore
   test of section 5.2, nightly in CI).
4. **SQLite storage with replication to Garage.** SQLite files live on the db host's disk
   (`RUUM_SQLITE_DIR`, laptop `deploy/dev/data/sqlite`), one file per database, WAL mode.
   Replication: Litestream (v0.5, S3-compatible, to the Garage bucket `ruum-sqlite`) as a
   compose service that watches the directory; point-in-time restore reads the replica. If
   Litestream cannot watch a directory with files that come and go, the engine runs replication
   itself (open question 6).
5. **No connection proxy** (decision 15). The design's proxy on `<node-id>.db.hel1.ruum.space`
   comes later; nothing in step 2 listens for the Postgres wire protocol from outside.
6. **CI**: the `postgres-data` service in the e2e job; `redocly lint` also for
   `contracts/providers/provider.openapi.yaml`; an `ajv` check of the manifest examples.

## Task cards

IDs: `C` contract, `I` infrastructure, `P` providers, `E` engines, `D` databases, `Q` query and
privacy, `K` classes, `A` access, `M` MCP and CLI, `U` UI, `T` tests. "After" lists the cards that
must be merged first.

```markdown
ID:          C-1
Goal:        Review and freeze the step 2 contracts of f/step2spec (this draft).
Contracts:   contracts/providers/*, contracts/manifests/database.schema.json, the `databases` tag of
             openapi.yaml, schema.zed, mcp/tools.yaml, migrations/0002_databases.sql.
May change:  those files, the generated web/src/api/schema.d.ts and crates/ruum-mcp/src/tools.json.
Must not:    product code.
Done when:   the architect panel accepts (two reviewers); redocly, zed validate, oasdiff (0 errors),
             the migration applies; a provisional decision record 0003 lists what changed.
After:       -
Out of scope: journal event schemas (C-2).
```

```markdown
ID:          C-2
Goal:        Journal event schemas for step 2.
Contracts:   contracts/events/: typed_node.status_changed, database.columns_confirmed,
             database.migration_applied, database.migration_failed, database.branch_created,
             database.restore_started, database.restored, database.restore_failed,
             database.grant_created, database.grant_revoked, database.approval_decided;
             EventType in openapi.yaml; examples.
May change:  contracts/events/, the EventType enum, generated files.
Must not:    product code.
Done when:   every example validates (ajv); oasdiff 0 errors.
After:       C-1
Out of scope: provider webhooks over HTTP (step 6).
```

```markdown
ID:          T-1
Goal:        Acceptance tests and suite cases for all 22 database operations, before the code.
Contracts:   openapi.yaml tag `databases`, schema.zed, tools.yaml.
May change:  crates/ruum-testkit/ (isolation cases, permission matrix rows, USER_ONLY_OPERATIONS),
             crates/ruum-api/tests/acceptance/databases_*.rs (new).
Must not:    product code, contracts.
Done when:   the suites list every new operation; they fail with 501 until built; a person or the
             panel reviewed them.
After:       C-1
Out of scope: the privacy suite (T-2).
```

```markdown
ID:          T-2
Goal:        The privacy suite: an agent never sees a raw secret, nor a raw personal value without
             a read_personal grant, through any operation.
Contracts:   openapi.yaml (HiddenValue, Pseudonym, LiteralValue, QueryResult), design 8.5.
May change:  crates/ruum-testkit/src/privacy.rs (new), its fixtures, mutation rules.
Must not:    product code, contracts.
Done when:   cases for queryDatabase (plain column, expression, join, aggregate, jsonb with $ruum),
             getDatabaseSchema, listDatabaseQueries, getDatabaseApproval, a masked branch, an
             agent copy; unconfirmed columns count as personal; same value -> same pseudonym;
             people without manage never see secrets; a mutation rule per rule.
After:       C-1
Out of scope: telemetry privacy (step 3).
```

```markdown
ID:          I-1
Goal:        The data cluster on the laptop stack with PostgreSQL Anonymizer and per-database roles.
Contracts:   docs/STEP2.md "Infrastructure work" 1 and 2.
May change:  deploy/dev/postgres-data/ (new), deploy/dev/docker-compose.yml, scripts/dev.sh
             (env RUUM_DBHOST_URL, status), scripts/dbhost_check.sh (new), docs/DEVSTACK.md.
Must not:    the platform Postgres service, other services, crates/.
Done when:   dev.sh up/status shows postgres-data healthy; `create extension anon` works in a new
             database; dbhost_check.sh shows that ruum_dbhost can create a database and its roles,
             and that a role of database A cannot connect to database B; nothing listens outside
             127.0.0.1.
After:       -
Out of scope: backups (I-2).
```

```markdown
ID:          I-2
Goal:        WAL archiving and point-in-time restore of one database on the laptop stack.
May change:  deploy/dev/postgres-data/, deploy/dev/garage/ (bucket ruum-dbbackups),
             scripts/db_restore_test.sh (new), docs/DEVSTACK.md, .github/workflows/nightly.yml.
Must not:    crates/.
Done when:   db_restore_test.sh creates a database, writes rows, notes the time, deletes rows,
             restores the one database to the noted time into a new database and finds the rows.
After:       I-1
Out of scope: Hetzner, the second backup provider.
```

```markdown
ID:          I-3
Goal:        SQLite storage with replication to Garage.
May change:  deploy/dev/ (litestream service, bucket ruum-sqlite), scripts/dev.sh,
             scripts/sqlite_restore_test.sh (new), docs/DEVSTACK.md.
Must not:    crates/.
Done when:   a new file in RUUM_SQLITE_DIR is replicated within 10 s; restore to a time works;
             deleting the file locally and restoring brings it back.
After:       -
Out of scope: more than one db host.
```

```markdown
ID:          P-1
Goal:        The Provider trait, the registry and the in-memory test provider.
Contracts:   contracts/providers/provider.openapi.yaml.
May change:  crates/ruum-core/src/providers/{mod,registry,memory,conformance}.rs, lib.rs export
             line (integrator).
Must not:    nodes/, migrations, contracts.
Done when:   the conformance suite (validate, create, apply, status, hooks, provider versions,
             restore, export) passes on the in-memory provider.
After:       C-1
Out of scope: the HTTP adapter for external providers.
```

```markdown
ID:          P-2
Goal:        typed_nodes and provider_deliveries in Postgres; the reconciler and the delivery worker.
Contracts:   migrations/0002_databases.sql, provider.openapi.yaml.
May change:  crates/ruum-core/src/providers/{postgres,reconciler,deliveries}.rs.
Must not:    other modules, contracts.
Done when:   memory and Postgres pass the same suite; a failed delivery is retried in seq order per
             node; a report with an older generation is ignored; tenant isolation of both tables.
After:       P-1
Out of scope: -
```

```markdown
ID:          P-3
Goal:        The nodes area calls the provider for typed nodes: copy, move, rename, trash, restore,
             purge write deliveries in the same transaction as the journal entry.
May change:  crates/ruum-core/src/nodes/ (a TypedHooks interface, agreed with the nodes owner),
             crates/ruum-core/src/providers/.
Must not:    routes, contracts.
Done when:   copying a folder with a database makes a new database node in provisioning and a
             `copied` delivery; trash makes the database suspended; purge deletes it; the trashed
             file suite also covers a database node.
After:       P-2
Out of scope: -
```

```markdown
ID:          E-1
Goal:        The DatabaseEngine trait and the in-memory (SQLite in memory) engine.
May change:  crates/ruum-dbengine/ (new crate), the root Cargo.toml members line (integrator).
Must not:    ruum-core.
Done when:   the engine conformance suite (create, drop, catalog, read-only query, write, timeout,
             row cap, branch, restore point, export) passes on the memory engine.
After:       C-1
Out of scope: masking (Q-2, Q-3).
```

```markdown
ID:          E-2
Goal:        The Postgres engine on the shared data cluster: databases, NOLOGIN roles, SET ROLE,
             read-only transactions, statement_timeout, branches with TEMPLATE, restore points
             (pg_create_restore_point / LSN), catalog, export with pg_dump.
May change:  crates/ruum-dbengine/src/postgres/.
Must not:    other crates.
Done when:   the engine conformance suite passes against postgres-data; a role of one database
             cannot read another; a statement over the timeout is cancelled.
After:       E-1, I-1
Out of scope: point-in-time restore (E-4).
```

```markdown
ID:          E-3
Goal:        The SQLite engine: files, read-only opens, busy timeout, interrupt on timeout, branch by
             file copy (online backup API), catalog, export.
May change:  crates/ruum-dbengine/src/sqlite/.
Done when:   the engine conformance suite passes; one writer at a time; a read never writes.
After:       E-1, I-3
Out of scope: masking (Q-3).
```

```markdown
ID:          E-4
Goal:        Point-in-time restore of one database (Postgres and SQLite), new node and in place.
May change:  crates/ruum-dbengine/src/{postgres,sqlite}/restore.rs.
Done when:   the restore tests of I-2 and I-3 pass through the engine; in place records a restore
             point first and refuses queries while it runs.
After:       E-2, E-3, I-2
```

```markdown
ID:          D-1
Goal:        createDatabase, getDatabase, updateDatabaseManifest: the database provider on the
             engine, manifest validation, plan limits.
Contracts:   openapi.yaml#/paths/~1v1~1databases, ~1v1~1databases~1{node_id}, .../manifest;
             database.schema.json; provider.openapi.yaml.
May change:  crates/ruum-core/src/databases/{mod,manifest,provider}.rs,
             crates/ruum-api/src/routes/databases.rs.
Must not:    migrations, contracts, other modules.
Done when:   T-1 cases for these operations pass on memory and Postgres; an agent cannot change
             data_classes; status goes provisioning -> ready; isolation and permission suites pass.
After:       P-2, E-1 (memory first), E-2 for the Postgres run
```

```markdown
ID:          D-2
Goal:        Migrations: applyDatabaseMigration, listDatabaseMigrations (decision 19).
May change:  crates/ruum-core/src/databases/migrations.rs, routes/databases.rs (these handlers).
Done when:   a failing migration leaves the database as before (422 migration_failed); a name runs
             once; a restore point comes first; new columns arrive unconfirmed with the author's
             suggestions; an agent on live needs write_live.
After:       D-1, K-1
```

```markdown
ID:          D-3
Goal:        Branches: createDatabaseBranch, listDatabaseBranches (masked by default).
May change:  crates/ruum-core/src/databases/branches.rs, routes/databases.rs (these handlers).
Done when:   a masked branch holds pseudonyms for personal columns and NULL for secrets; full needs a
             person with manage; a source over the limit is 422 database_too_large; the branch is a
             child node that inherits grants and has draft_branch.
After:       D-1, Q-2, A-1
```

```markdown
ID:          D-4
Goal:        Restore points and restore: listDatabaseRestorePoints, createDatabaseRestorePoint,
             restoreDatabase.
May change:  crates/ruum-core/src/databases/backups.rs, routes/databases.rs (these handlers).
Done when:   restore to a new node and in place work on Postgres and SQLite inside the window; a
             time outside the window is 422; agents get 403 user_required.
After:       D-1, E-4
```

```markdown
ID:          Q-1
Goal:        queryDatabase for people: one statement, read-only by default, timeout, caps, the
             result JSON, LiteralValue wrapping, the query log; getDatabaseSchema;
             listDatabaseQueries.
May change:  crates/ruum-core/src/databases/query/, crates/ruum-api/src/routes/database_query.rs.
Done when:   T-1 cases pass; two statements are 422; a write in read mode is read_only_query; the
             log never holds a raw parameter.
After:       D-1
Out of scope: agents (Q-2, Q-3).
```

```markdown
ID:          Q-2
Goal:        The privacy filter for Postgres: Anonymizer masking rules from database_columns on the
             masked roles; pseudonyms with the tenant key; secret markers; result column classes.
May change:  crates/ruum-core/src/privacy/, crates/ruum-core/src/databases/query/postgres_mask.rs,
             crates/ruum-dbengine/src/postgres/anon.rs.
Done when:   T-2 passes on Postgres.
After:       Q-1, K-1, E-2
```

```markdown
ID:          Q-3
Goal:        The privacy filter for SQLite, in the query layer (column origin; unknown -> personal).
May change:  crates/ruum-core/src/databases/query/sqlite_mask.rs.
Done when:   T-2 passes on SQLite.
After:       Q-1, K-1, E-3
```

```markdown
ID:          K-1
Goal:        Column classes: catalog sync, listDatabaseColumns, suggestColumnClasses,
             confirmColumnClasses (new manifest version), the rule-based suggester.
May change:  crates/ruum-core/src/databases/classes/, crates/ruum-api/src/routes/database_classes.rs.
Done when:   T-1 cases pass; unconfirmed = personal; only a person confirms; confirmation is a new
             manifest version and an audit entry.
After:       D-1
```

```markdown
ID:          A-1
Goal:        Grants: createDatabaseAccessGrant, listDatabaseAccessGrants, revokeDatabaseAccessGrant;
             the tuples, the sweeper, the restore point before write_live.
May change:  crates/ruum-core/src/databases/access/grants.rs, routes/database_access.rs.
Done when:   only the agent's user grants; an expired or revoked grant gives nothing even before the
             sweeper runs (mutation rule); the agent can give a grant back.
After:       D-1
```

```markdown
ID:          A-2
Goal:        Approvals of destructive statements: detection, 202 with a pending approval,
             decideDatabaseApproval, one use within 15 minutes.
May change:  crates/ruum-core/src/databases/access/approvals.rs, routes/database_access.rs.
Done when:   DROP, TRUNCATE, DELETE/UPDATE without WHERE from an agent never run without an
             approval; another statement or other parameters do not match; a used approval is spent.
After:       Q-1, A-1
```

```markdown
ID:          M-1
Goal:        The 18 MCP tools and `ruum db ...` in the CLI.
May change:  crates/ruum-mcp/, crates/ruum-cli/.
Done when:   tools_contract passes; an MCP smoke test creates a database, queries it and sees
             markers; the agent benchmark gets 3 database tasks (bench/agent/).
After:       D-1, Q-1 (more tools as their operations land)
```

```markdown
ID:          U-1
Goal:        UX package for databases (journeys, IA, wireframes, copy), section 18.4.1.
May change:  docs/ux/databases/ (new).
Done when:   the UX review panel accepts; contract needs found are listed as C-cards.
After:       C-1
```

```markdown
ID:          U-2
Goal:        The database pages of the web UI (see area 9).
May change:  web/ except schema.d.ts.
Done when:   flow tests, visual snapshots and axe pass; markers render as chips.
After:       U-1, D-1, Q-1, K-1, A-1, A-2 (pages land as their operations land)
```

```markdown
ID:          T-3
Goal:        Step 2 end-to-end: scripts/e2e_smoke.sh gets database steps (create, migrate with
             classes, confirm, query as person and as agent, branch, grant, approval, restore).
May change:  scripts/e2e_smoke.sh, crates/ruum-testkit/scripts/mutation_check.sh rules.
Done when:   the smoke test passes on the stack; the full mutation check is strict and green.
After:       all D, Q, K, A cards
```

## Order

1. **Specify** (now): C-1 review; then in parallel C-2, T-1, T-2, U-1, I-1, I-3.
2. **Foundations** in parallel: P-1, E-1; then P-2, E-2 (after I-1), E-3 (after I-3), I-2.
3. **Control plane**: D-1 (memory first), then K-1, Q-1, A-1 in parallel; P-3.
4. **Privacy and agent rules**: Q-2, Q-3, A-2; then D-2, D-3.
5. **Backups**: E-4, D-4.
6. **Clients**: M-1 and U-2 as their operations land.
7. **Harden**: T-3, the security reviewers (two lenses: tenancy and privacy), the agent benchmark
   with database tasks, synthetic users on the database pages.

Migrations go through the queue: 0002 is the only migration of the step; a change to it during
step 2 is made in place (it is not released) by a migration task, and local databases are
recreated.

## Open questions for the panel

1. **Grant expiry in SpiceDB.** SpiceDB 1.40+ supports relationship expiration (`use expiration`).
   The draft keeps the expiry in Postgres (checked by the core, tuple deleted by the sweeper),
   because the in-memory engine's schema reader does not read `with expiration`. Recommended:
   keep it so for step 2; move to `use expiration` when the reader supports it.
2. **People and secrets.** The draft hides `secret` values from people without `manage` too
   (design 8.5 talks only about agents). Recommended: yes, it costs nothing and protects tokens
   and hashes from viewers.
3. **Share links on databases.** The draft gives share-link bearers no database operation.
   Recommended: yes until public projects (section 9.9).
4. **Pseudonym scope.** Per tenant (joins across databases of one tenant work) or per database
   (less linkable)? Recommended: per tenant, as for telemetry (section 9.6).
5. **PITR of one database in a shared cluster.** Restoring one database means a scratch restore
   of the whole cluster and a dump; slow for a large cluster. Recommended: accept for
   `shared-small` (databases up to 10 GB) with a stated restore time; logical per-database
   backups as an extra daily point if the restore test is too slow.
6. **SQLite replication.** Litestream as a sidecar that watches a directory, or replication in the
   engine (a Rust implementation of the Litestream format)? Recommended: Litestream first; check
   that it handles files that appear and disappear.
7. **Writes by people on live databases.** The draft lets a person with `edit` write without a
   grant, and asks for approval of destructive statements only from agents. Should a person's
   destructive statement also ask (a confirm in the UI)? Recommended: UI confirm only, no API
   rule.
8. **Query log retention and size.** One row per query can be large. Recommended: the audit
   retention of the plan, monthly partitions, and no row for repeated identical reads within one
   second (a counter instead).
9. **`sqlparser` as a new dependency** (statement count, destructive statement detection, column
   origin). Recommended: accept (Apache 2.0, widely used).
10. **Branch of a branch.** Allowed? Recommended: no in step 2 (branches are flat children of a
    live database).
11. **Typed node copy by agents.** `copyNode` of a folder with a database by an agent copies with
    masked data. Is that surprising for a person who asked the agent for a copy? Recommended:
    yes, masked, and the copy's `branch_data`-like flag says so in the UI.
