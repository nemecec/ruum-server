# Step 2 (Databases): work areas, task cards and order

Status: DRAFT, revised after the first panel review (design section 18.9). The decisions of the
review are in `docs/decisions/0004-step2-spec.md` (`provisional`). Nothing here is final until
the panel accepts the revision.

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

The step 1 operations keep their shape. `oasdiff breaking` (CI's check, `--fail-on ERR`) reports
0 errors; the warnings are the new values of the response enum `ErrorCode`. Four step 1
descriptions get additive text for typed nodes (decision 0004-6, 0004-7): `putContent` and
`createUpload` refuse a typed node (`409 conflict`), `restoreVersion` of a database follows the
manifest rules, and `copyNode` copies a database with `full` data only for a person with
`manage` on the source. `TenantPolicy` gets the optional flag `guests_create_resources`
(default false, decision 0004-10). Two new error codes: `statement_not_allowed`,
`class_read_required`.

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
| Read-only query | `view`; `secret` masked without `manage` | `view`; every read of `secret` masked, of `personal` as pseudonyms unless `read_personal` grant (also in `WHERE`, `JOIN`, `ORDER BY`, ...) |
| Read-write query | `edit`; may not read or write `secret` columns without `manage` | branch with `masked` / `schema_only` data: `edit`; live or `full` branch: `edit` + `write_live` grant; a write that reads or writes `personal` (without `read_personal`) or `secret` columns: `403 class_read_required` |
| Destructive statement | as the statement needs | after the `write_live` check: a pending approval (`202`) that a person decides; an approval never replaces `write_live` |
| See `secret` values | `manage` | never |
| Settings, migrations on live or `full` branch | `manage` | `manage` + `write_live` grant + an approval per migration; may not change `data_classes` |
| Migrations on a `masked` / `schema_only` branch | `edit` | `edit` |
| Branch with `masked` / `schema_only` data | `edit` | `edit` |
| Branch with `full` data | `manage` | never (`403 user_required`) |
| Copy a database (`copyNode`) | `full` data with `manage` on the source, else `masked` | always `masked` |
| Restore a manifest version (`restoreVersion`) | `manage` | `manage`; may not change `data_classes` |
| Content writes on a typed node (`putContent`, uploads) | `409 conflict` | `409 conflict` |
| Restore points (name one) | `edit` | `edit` |
| Point-in-time restore | `manage` (user only) | never |
| Suggest column classes | `edit` | `edit` |
| Confirm column classes | `manage` (user only) | never |
| Give a grant to an agent | the agent's user, `edit` (live writes) or `view` (personal values) | never |
| Decide an approval | the agent's user or a person with `manage`, with the right the statement needs (user only) | never |
| Query log | `manage`; normalized statements, no literals | `manage`; only its own entries |
| Engine error text | full with `manage`; else sanitized (SQLSTATE, fixed text, object names) | sanitized |
| Create a database, branch, restore into a new node, copy a typed node, as a guest | only with `guests_create_resources` | the same, for an agent of a guest |
| Share links | no database operation (`403 user_required`, as in step 1) | |

The order of checks for an agent's statement: the user's right and the scope, then the
`write_live` grant (`403 write_grant_required`), then the statement rules (`422
statement_not_allowed`, `403 class_read_required`), then the approval of a destructive statement
(`202`).

## Shared files (not owned by any area)

As in step 1 (`docs/STEP1.md`), plus:

| Path | What |
|---|---|
| `contracts/providers/`, `contracts/manifests/` | Contract changes only. |
| `crates/ruum-core/src/providers/mod.rs` | The `Provider` trait (module border). Signature changes need the agreement of the `providers` and `databases` areas. |
| `crates/ruum-dbengine/src/lib.rs` | The `DatabaseEngine` trait (module border). |
| `crates/ruum-core/src/privacy/mod.rs` | The marker types and the `Pseudonymizer` trait; step 3 (telemetry) uses them too. |

## The engine security model (decision 0004)

The query API runs SQL that agents and people write. The engine must hold the tenant border and
the privacy filter of design 8.5 even when that SQL is hostile. The parser checks of the query
layer (`sqlparser`) are a first line only; each rule below holds in the engine itself.

### Postgres

1. **Sessions.** Untrusted SQL (every statement of the query API and every migration) runs ONLY
   in a session whose `session_user` is the database role of the kind the request needs
   (`database_roles`). These roles have `LOGIN` with a short-lived random password (`VALID
   UNTIL` a few minutes ahead) that the platform sets when it opens connections and never
   stores; connections are pooled per (database, kind) and closed before the password ends. Each
   role is a member of no other role, has no ADMIN option, is `NOINHERIT`, and has `CONNECT` on
   its own database only (`REVOKE CONNECT ... FROM PUBLIC` on every database). So `RESET ROLE`,
   `SET ROLE <other>` and `set_config('role', ...)`, also inside a `DO` block or a function,
   cannot reach another role. `pg_hba.conf` allows these roles only from the platform's
   address, with SCRAM.
2. **The admin role.** `ruum_dbhost` (`CREATEDB`, `CREATEROLE`, no superuser) runs only platform
   DDL: create and drop databases and roles, set passwords, build the lens views, grant. It
   never runs untrusted SQL. On PostgreSQL 17 a `CREATEROLE` role gets ADMIN on the roles it
   makes but no `SET` or `INHERIT` membership (`createrole_self_grant` stays empty).
3. **Lenses.** Each role kind reads through one lens: `base` (the tables), `nosecret` (views in
   the schema `ruum_nosecret`, every `secret` column masked) or `masked` (views in the schema
   `ruum_masked`, `personal` columns as pseudonyms and `secret` columns masked). A role has
   `USAGE` only on its lens schema and `pg_catalog`, and `search_path` is its lens schema; it
   has no right on the base tables and no `USAGE` on `public` or any other schema. So every
   read, in any clause, sees the masked value, and functions outside the lens are out of reach.
   The views are owned by the `mask` role (`NOLOGIN`, `SELECT` on the base tables, no session
   ever runs as it). The kinds:

   | Kind | Who | Lens | Writes |
   |---|---|---|---|
   | `read` | people with `view`, agents with `read_personal` | `nosecret` | none |
   | `write` | people with `edit`, agents with `read_personal` (with `write_live` or on a draft branch) | `nosecret` | plain and `personal` columns |
   | `masked_read` | agents without `read_personal` | `masked` | none |
   | `masked_write` | agents without `read_personal` (with `write_live` or on a draft branch) | `masked` | plain columns only |
   | `secret_read` | people with `manage` | `base` | none |
   | `owner` | people with `manage`; migrations | `base` | everything, owns the schema |
   | `mask` | nobody (`NOLOGIN`) | - | owns the views, `ruum_mask` and the key |

   A masked `secret` column is `text`: `NULL` when the value is `NULL`, else a fixed constant;
   the query layer shows the hidden-value marker. A masked `personal` column is `text` holding
   the pseudonym, whatever its type in the table. The lens views are rebuilt (by `ruum_dbhost`,
   in one transaction with the catalog sync) after each migration and each confirmation.
4. **One pseudonym function for both engines.** `pseudonym(v) = 'p_' || first 12 hex characters
   of HMAC-SHA-256(tenant_key, canonical_text(v))`. `canonical_text` is the text form of the
   value with a fixed format that no session setting changes (text as is; numbers in their
   shortest exact form; times as RFC 3339 in UTC; `bytea` and BLOB as lowercase hex). In
   Postgres the function `ruum_mask.pseudonym(text)` is `SECURITY DEFINER`, owned by `mask`,
   with a fixed `search_path`, and computes the HMAC with the built-in `sha256()` from the
   padded keys in the table `ruum_mask.key` (owner `mask`, no grant to anyone; no extension
   needed, so it never clashes with a `pgcrypto` of the manifest). The lens views call it;
   the agent roles have `EXECUTE` on it but no `USAGE` on the schema `ruum_mask`, so they cannot
   call it by name, and they can never read the key. The platform computes the same function in
   Rust (query log parameters, SQLite). Constants and parameters are never pseudonymized.
   Collisions: 12 hex characters are 48 bits; among 10^6 distinct values of one tenant a
   collision has a probability of about 0.2 %, among 10^7 about 16 %. Accepted for step 2: a
   collision makes two values look equal to an agent; it never shows a real value.
5. **No PostgreSQL Anonymizer in step 2.** Its masking rules are security labels that every
   role can read in `pg_seclabels`, and a salt in a setting is readable too, so a masked role
   could learn the pseudonym key. The lens views above do what design 8.5 asks ("masks inside
   the database, bound to the agent's database role") without that leak (decision 0004-4).
6. **Neutral names.** Every role can list `pg_database` and `pg_roles` of the cluster. Database
   and role names are `d_` / `r_` plus 16 random hex characters, never a node or tenant ID.
7. **Side channels.** The agent roles have no `SELECT` on the base tables, so `pg_stats`
   (`most_common_vals`, histograms) shows them nothing; `EXPLAIN` shows plans of the views;
   `pg_stat_statements` is not installed; `pg_stat_activity` shows other sessions' queries only
   to their own role (default). Timing differences of a query over masked values are accepted.

### SQLite

1. **Files.** One directory per tenant below `RUUM_SQLITE_DIR`, with a random name, and one
   file per database, with a random name (`databases.engine_name`). The engine opens exactly
   that file, with `SQLITE_OPEN_NOFOLLOW`, read-only for `mode: read`.
2. **Hardening on every connection.** An authorizer (`sqlite3_set_authorizer`) that denies
   `ATTACH`, `DETACH`, every `PRAGMA` outside a read-only allow-list (`table_info`,
   `table_xinfo`, `index_list`, `index_info`, `foreign_key_list` and the like), and every
   function call of `load_extension`, `readfile`, `writefile` and `ruum_pseudonym` from
   top-level SQL; `SQLITE_LIMIT_ATTACHED = 0`; `SQLITE_DBCONFIG_DEFENSIVE` on;
   `SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION` off; `trusted_schema = OFF`;
   `SQLITE_DBCONFIG_TRUSTED_SCHEMA` off. `VACUUM` (also `VACUUM INTO`) is refused by the parser
   and by the authorizer.
3. **Lenses.** Per connection, the engine creates `TEMP` views with the names of the tables
   (they shadow `main` for unqualified names), with `personal` columns as
   `ruum_pseudonym(...)` and `secret` columns masked, for the lens of the caller. The function
   `ruum_pseudonym` is registered per connection in Rust with the tenant key; the key never
   enters the file. The authorizer denies every read (`SQLITE_READ`) of a `personal` or
   `secret` column of a `main` table that does not come from a Ruum lens view (its sixth
   argument names the view), so `main.users.email` in a `WHERE` fails. Semantics are equal to
   Postgres: every read of a restricted column sees the masked value or fails.
4. **Writes.** A write statement reads the `main` tables directly; the authorizer denies the
   read of a restricted column (`403 class_read_required`), as the query layer does first.

### Both engines

- **Classes follow column identity** (Postgres: relation OID and `attnum`; SQLite: the platform
  parses every DDL statement it runs). A rename, a retype, a table swap or rebuild, and a drop
  followed by an add of the same name return the column to unconfirmed (`personal`).
- **Agent writes** that read or write a restricted column fail with `403 class_read_required`
  (query layer first, engine second). Masked views never let a write copy a real value.
- **Agent migrations on real data** (live, or a `full` branch) run as `owner`, so they always
  wait for an approval by a person (reason `migration`), on top of `write_live`.
- **Errors are sanitized** for agents and for people without `manage`: the SQLSTATE, a fixed
  text per SQLSTATE and object names; never `DETAIL`, `HINT` or `CONTEXT`. Status messages and
  stored migration errors are sanitized for everyone.
- **The query log** stores the normalized statement (literals replaced) and parameters as
  pseudonyms; agents read only their own entries.

### Accepted risk

An agent that may write can insert a guessed value and read its pseudonym, then compare it with
the pseudonyms of a column: an online test of one guess per write, logged, rate limited, and
needing a write right. A `DO` block on a draft branch could call the pseudonym function through
dynamic SQL only if it gets around the missing schema `USAGE`, and gives the same online test.
Neither gives the key or an offline dictionary attack (decision 0004-4; relates to open
question 4).

## The areas

### 1. Providers (typed nodes)

| | |
|---|---|
| Owns | `crates/ruum-core/src/providers/` (trait, registry, reconciler, deliveries, conformance suite, in-memory test provider) |
| Implements | The platform side of `contracts/providers/provider.openapi.yaml`: typed node create, status, the control loop, hook deliveries from the journal (`provider_deliveries`), `checkAccess` for in-process providers. No HTTP adapter for external providers yet (step 6). |
| Tables | `typed_nodes`, `provider_deliveries`, `versions` with encoding `typed_manifest_v1` |
| Touches | Calls into the nodes area for `copyNode`, `trashNode`, `restoreNode`, `purgeNode` of typed nodes through a hook interface (`nodes::TypedHooks`), agreed with area 1 of step 1 (card P-3). The nodes area refuses content writes on typed nodes (`putContent`, upload targets: `409 conflict`) and sends `restoreVersion` of a typed node through the provider's manifest rules. |

### 2. Database engines

| | |
|---|---|
| Owns | `crates/ruum-dbengine/` (new crate): the `DatabaseEngine` trait; `memory` (a fake for unit tests: SQLite in memory), `postgres` (the shared data cluster), `sqlite` (files and replication). Catalog reads, role management, branches, restore points, export. |
| Notes | The engine never sees a platform token. It gets the database, the role kind to use and the statement. Postgres: untrusted SQL runs in sessions whose `session_user` is the per-database LOGIN role of that kind (short-lived password, pooled per database and kind, member of no other role); `ruum_dbhost` runs platform DDL only; the lens views and the pseudonym function (see "The engine security model"). SQLite: one file per database in a per-tenant directory, the authorizer and the hardening flags on every connection, TEMP lens views, opened read-only for read queries, replicated to Garage. |

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
| Notes | One statement per call (parse with `sqlparser` to count statements, find destructive ones, refuse the statements of `statement_not_allowed`, find the restricted columns a write reads or writes (`class_read_required`), and the columns a result reads; new dependency, needs approval). The role kind follows the caller and the grants (table in "The engine security model"). Read-only transaction for `mode: read`; `statement_timeout`; row and size caps. Result JSON with the typed markers (`HiddenValue`, `Pseudonym`, `LiteralValue`). Masking happens in the engine, through the lens, so every clause sees masked values; the query layer only turns masked values into markers, by the class of each result column (the strictest class it reads; unknown -> `personal`; no column read -> `normal`, never masked). Pseudonyms: one function for both engines (HMAC-SHA-256 with the tenant key from OpenBao, in tests a fixed key; `p_` + 12 hex characters). Engine errors are sanitized for agents and people without `manage`. The query log stores the normalized statement (literals replaced) and the parameters as pseudonyms; agents read only their own entries. |

### 5. Column classes

| | |
|---|---|
| Owns | `crates/ruum-core/src/databases/classes/`, `crates/ruum-api/src/routes/database_classes.rs` |
| Implements (OpenAPI) | area `classes` |
| Tables | `database_columns` |
| Notes | Catalog sync after each migration, after each schema change through the query API, and on a schedule (new columns start unconfirmed = `personal`; dropped columns get `dropped_at`). Classes follow column identity (Postgres `rel_oid`/`attnum`/`type_oid`; SQLite by parsing the DDL the platform runs): a rename, retype, table swap or rebuild, or a drop and add of the same name clears the confirmation (`unconfirmed_reason`) and removes the key from `data_classes` (a new manifest version by the system). Suggestions store the newest one per column; every suggestion and confirmation goes to the audit log. Confirmation writes a new manifest version (`data_classes`) and rebuilds the lens views. A rule-based suggester (names such as `email`, `phone`, `password`, `token`, `name`, `address`, `note`) as the default for existing data; agents can do better and send their own. The digest lists unconfirmed columns. |

### 6. Agent access: grants and approvals

| | |
|---|---|
| Owns | `crates/ruum-core/src/databases/access/`, `crates/ruum-api/src/routes/database_access.rs` |
| Implements (OpenAPI) | area `access` |
| Tables | `database_access_grants`, `database_approvals` |
| Contract | `contracts/authz/schema.zed`: `draft_branch`, `live_writer`, `personal_reader`, `agent_write_live`, `agent_read_personal` |
| Notes | A grant writes its tuple through the outbox and records a restore point first (`write_live`). The core checks the tuple AND the row (`expires_at`, `revoked_at`) on every query; the sweeper deletes the tuple at the end and sends `access_changed`. `draft_branch` is written only for branches with `masked` or `schema_only` data. Destructive statements and agent migrations on real data: checked after `write_live` (an approval never replaces it); `statement_hash` = BLAKE3 of the statement and the canonical parameters (a migration: name and `up`); an approval is used once, within 15 minutes. The agent's user or a person with `manage` decides, with the right the statement needs. `estimated_rows` from `EXPLAIN` or a `count(*)` in a read-only transaction, never a test run. |

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
| Implements | Isolation and permission cases for the 22 operations (`USER_ONLY_OPERATIONS` already has the 4 new user-only operations from C-1); a **privacy suite**: an agent never gets a raw `secret` value or a raw `personal` value without a grant, through any operation (query, schema, columns, query log, approvals, migrations, errors, status, branches with `masked` data, copies, exports) and through any clause or side channel; mutation rules for each privacy and grant rule. |

## Infrastructure work

1. **A data cluster.** The shared data cluster is a Postgres server of its own, not the
   platform database (a customer database never shares a server with the platform metadata,
   also on the laptop). Laptop: a new compose service `postgres-data` (image
   `ruum-dev/postgres-data:17`, built from `deploy/dev/postgres-data/Dockerfile`: Postgres 17
   with PostGIS and pgvector; no PostgreSQL Anonymizer, decision 0004-4), port 5435 on
   127.0.0.1, a volume, WAL archiving on, `pg_stat_statements` not installed.
2. **Per-database roles.** An init script creates the admin role `ruum_dbhost` with `CREATEDB`
   and `CREATEROLE` (no superuser), `REVOKE CONNECT ... FROM PUBLIC` and `REVOKE ALL ON SCHEMA
   public FROM PUBLIC` in `template1`, and `pg_hba.conf` lines that allow the per-database
   roles only with SCRAM from the platform's address. The engine creates, per database, the
   roles of `database_roles` (LOGIN, `NOINHERIT`, member of no role, `CONNECT` on their own
   database only; `mask` NOLOGIN). They are NEVER granted to `ruum_dbhost`: untrusted SQL runs
   in a session that logs in as the role itself, with a short-lived password. `dev.sh env`
   prints `RUUM_DBHOST_URL`. A check script (`scripts/dbhost_check.sh`) proves that a role of
   database A cannot connect to database B, and that from a session of a masked role `RESET
   ROLE`, `SET ROLE <owner>`, `SET ROLE ruum_dbhost`, `SELECT set_config('role', ...)` and the
   same inside a `DO` block all fail or leave `current_user` unchanged.
3. **WAL archiving and point-in-time restore for the shared cluster.** pgBackRest (or WAL-G)
   archives WAL and base backups of `postgres-data` to the Garage bucket `ruum-dbbackups`; 7
   days of retention. Restoring one database: restore the cluster backup to a scratch instance at
   the time, dump the one database, load it into a new database (`new_node`) or swap it
   (`in_place`). A script `scripts/db_restore_test.sh` runs it end to end (the monthly restore
   test of section 5.2, nightly in CI).
4. **SQLite storage with replication to Garage.** SQLite files live on the db host's disk
   (`RUUM_SQLITE_DIR`, laptop `deploy/dev/data/sqlite`), one directory per tenant and one file
   per database, both with random names, WAL mode.
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
May change:  those files, the generated web/src/api/schema.d.ts and crates/ruum-mcp/src/tools.json;
             crates/ruum-testkit/src/matrix.rs (USER_ONLY_OPERATIONS only); the copy of the new
             error codes in web/src/api/errors.ts (the type needs one entry per ErrorCode).
Must not:    product code.
Done when:   the architect panel accepts (two reviewers); redocly, zed validate, oasdiff (0 errors),
             the migration applies, ajv for the manifest examples; USER_ONLY_OPERATIONS carries
             the four new user-only operations (confirmColumnClasses, restoreDatabase,
             createDatabaseAccessGrant, decideDatabaseApproval) in the same commit as their
             `x-ruum-user-only` marks, so `matrix::tests::user_only_list_matches_the_contract`
             stays green on main; cargo test of ruum-testkit, ruum-mcp (tools_contract) and the
             error code test of ruum-core pass; the provisional decision record 0004 lists what
             changed.
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
             panel reviewed them. Matrix cases also for: a guest and an agent of a guest on
             createDatabase, createDatabaseBranch, restoreDatabase (new_node) and copyNode of a
             database, with `guests_create_resources` false (403) and true (allowed); the order
             of checks for an agent's destructive statement on live without write_live (403
             write_grant_required, never 202) and with write_live (202); an approved statement
             re-sent after the write_live grant ended (403); who decides an approval (the
             agent's user, a person with manage: 200; another editor: 403; an agent: 403
             user_required); putContent and createUpload (new_version) on a database node (409,
             also with an edit share link); restoreVersion on a database (edit: 403; an agent
             restoring other data_classes: 422); copyNode of a database by a viewer, an editor,
             an agent (masked) and a manager (full); an agent migration on live (202, then 200
             with approval_id); a full branch without draft_branch (agent write: 403
             write_grant_required).
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
             people without manage never see secrets; a mutation rule per rule. Oracle cases,
             each on Postgres and SQLite, that must show no raw value: `count(*) ... WHERE
             email LIKE 'a%'`, `WHERE email = $1` with the real value, `ORDER BY email`,
             `GROUP BY email`, `min(email)`, `length(email)`, a join on a personal column with a
             normal column, the same with a secret column (also for a person without manage).
             Write cases: `UPDATE t SET normal_col = personal_col`, `INSERT ... SELECT
             personal_col`, `RETURNING personal_col`, on live with write_live and on a draft
             branch (403 class_read_required). Rename cases: a migration that renames `email`
             to the name of a dropped confirmed-normal column, renames a table onto a dropped
             table's name, retypes a column, drops and adds a column of the same name (each:
             the column is unconfirmed afterwards). Role cases (Postgres): `RESET ROLE`, `SET
             ROLE <owner>`, `SET ROLE ruum_dbhost`, `set_config('role', ...)` and a DO block
             doing the same (422 statement_not_allowed or no change of current_user; never
             another role's data). File cases (SQLite): an agent of tenant A runs `ATTACH
             DATABASE '<path of a file of tenant B>'`, `VACUUM INTO '<path>'`,
             `load_extension(...)`, a PRAGMA outside the allow-list (all refused). Pseudonym
             cases: a direct call of `ruum_mask.pseudonym` and `ruum_pseudonym` fails; `SELECT
             'guess'` comes back unmasked and is not a pseudonym; the key is not readable from
             any agent role (`ruum_mask.key`, `pg_seclabels`, settings). Leak cases: a unique
             violation and a check violation from an agent and from a viewer (no `DETAIL`, no
             value in `message`), a failed migration in listDatabaseMigrations for a viewer, the
             status message; a person's query with literals in listDatabaseQueries (no literal;
             an agent with manage sees only its own entries). Side channels: `pg_stats` and
             `EXPLAIN` from an agent role show no value.
After:       C-1
Out of scope: telemetry privacy (step 3).
```

```markdown
ID:          I-1
Goal:        The data cluster on the laptop stack with per-database LOGIN roles (no Anonymizer).
Contracts:   docs/STEP2.md "Infrastructure work" 1 and 2.
May change:  deploy/dev/postgres-data/ (new), deploy/dev/docker-compose.yml, scripts/dev.sh
             (env RUUM_DBHOST_URL, status), scripts/dbhost_check.sh (new), docs/DEVSTACK.md.
Must not:    the platform Postgres service, other services, crates/.
Done when:   dev.sh up/status shows postgres-data healthy; dbhost_check.sh shows that ruum_dbhost
             can create a database and its roles, that a role of database A cannot connect to
             database B, that a per-database role is a member of no role, and that from a
             session of that role `RESET ROLE`, `SET ROLE <other role>`, `SELECT
             set_config('role', '<other role>', false)` and the same in a DO block cannot reach
             another role (current_user stays the same or the statement fails); nothing
             listens outside 127.0.0.1.
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
             `copied` delivery with `data: full` only when the caller is a person with manage on
             the source, else `masked` (a viewer and an editor who copy get masked; an agent gets
             masked); a guest without guests_create_resources gets 403; trash makes the database
             suspended; purge deletes it; putContent and upload targets on a typed node give
             409 (the step 1 code in nodes/memory.rs that allows content on typed nodes is
             changed in both stores); restoreVersion of a typed node goes through the provider's
             manifest rules (manage, validation, data_classes); the trashed file suite also
             covers a database node.
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
Goal:        The Postgres engine on the shared data cluster: databases with neutral names, the
             per-database LOGIN roles with short-lived passwords and a pool per (database,
             kind), the lens views and the pseudonym function (owner `mask`), read-only
             transactions, statement_timeout, branches with TEMPLATE (masked: pseudonyms, non-text
             personal columns retyped to text), restore points (pg_create_restore_point / LSN),
             catalog with column identity (rel_oid, attnum, type_oid), sanitized errors, export
             with pg_dump.
May change:  crates/ruum-dbengine/src/postgres/.
Must not:    other crates.
Done when:   the engine conformance suite passes against postgres-data; a role of one database
             cannot read another; from a query, RESET ROLE, SET ROLE and set_config('role')
             cannot reach another role (also in a DO block); ruum_dbhost never runs a caller's
             statement (a test hook asserts session_user for every untrusted statement); a
             masked role cannot read a base table, `ruum_mask.key` or call
             `ruum_mask.pseudonym` by name; the Rust and the SQL pseudonym agree on a fixed
             list of values of every common type; a statement over the timeout is cancelled.
After:       E-1, I-1
Out of scope: point-in-time restore (E-4).
```

```markdown
ID:          E-3
Goal:        The SQLite engine: files in per-tenant directories, read-only opens, busy timeout,
             interrupt on timeout, the authorizer and the hardening flags, the TEMP lens views
             and `ruum_pseudonym`, branch by file copy (online backup API), catalog, export.
May change:  crates/ruum-dbengine/src/sqlite/.
Done when:   the engine conformance suite passes; one writer at a time; a read never writes; on
             every connection the authorizer denies ATTACH, DETACH, PRAGMA outside the
             allow-list, load_extension, VACUUM (also INTO) and reads of restricted main columns
             outside a lens view; SQLITE_LIMIT_ATTACHED is 0, SQLITE_DBCONFIG_DEFENSIVE is on,
             trusted_schema is OFF; an isolation case: an agent of tenant A attaches tenant B's
             file (refused, nothing read).
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
             data_classes; status goes provisioning -> ready; a guest needs
             guests_create_resources; a contract test keeps the OpenAPI DatabaseManifest and
             contracts/manifests/database.schema.json equal (keywords, enums, limits; only
             readOnly and descriptions may differ); isolation and permission suites pass.
After:       P-2, E-1 (memory first), E-2 for the Postgres run
```

```markdown
ID:          D-2
Goal:        Migrations: applyDatabaseMigration, listDatabaseMigrations (decision 19).
May change:  crates/ruum-core/src/databases/migrations.rs, routes/databases.rs (these handlers).
Done when:   a failing migration leaves the database as before (422 migration_failed); a name runs
             once; a restore point comes first; new columns arrive unconfirmed with the author's
             suggestions; renamed, retyped, rebuilt and re-added columns arrive unconfirmed; an
             agent on live or a full branch needs write_live and an approval (202, then the same
             name and up with approval_id); the stored error is sanitized for agents and people
             without manage.
After:       D-1, K-1
```

```markdown
ID:          D-3
Goal:        Branches: createDatabaseBranch, listDatabaseBranches (masked by default).
May change:  crates/ruum-core/src/databases/branches.rs, routes/databases.rs (these handlers).
Done when:   a masked branch holds pseudonyms for personal columns (non-text ones retyped to text)
             and NULL for secrets; full needs a person with manage; a source over the limit is 422
             database_too_large; the branch is a child node that inherits grants; only masked and
             schema_only branches have draft_branch; a guest needs guests_create_resources.
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
             statements of statement_not_allowed are 422; the log never holds a raw parameter
             or a literal (normalized statement); an agent lists only its own log entries;
             error messages are sanitized for people without manage.
After:       D-1
Out of scope: agents (Q-2, Q-3).
```

```markdown
ID:          Q-2
Goal:        The privacy filter for Postgres: the lens views from database_columns (nosecret,
             masked), the role kind per caller and grant, class_read_required for writes,
             markers from result column classes, sanitized errors for agents.
May change:  crates/ruum-core/src/privacy/, crates/ruum-core/src/databases/query/postgres_mask.rs,
             crates/ruum-dbengine/src/postgres/lens.rs.
Done when:   T-2 passes on Postgres.
After:       Q-1, K-1, E-2
```

```markdown
ID:          Q-3
Goal:        The privacy filter for SQLite: the TEMP lens views and the authorizer rules from
             database_columns, class_read_required for writes, markers from result column
             classes (column origin; unknown -> personal; no column -> normal).
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
             manifest version and an audit entry; the catalog sync keeps classes by column
             identity and resets renamed, retyped, swapped and re-added columns (T-2 rename
             cases); a reset writes a new manifest version without the key.
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
             approval; the write_live check comes first (403 before 202) and an approval never
             replaces it; only the agent's user or a person with manage, with the needed right,
             decides; estimated_rows never comes from a test run of the statement; another
             statement or other parameters do not match; a used approval is spent.
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
2. **People and secrets.** Decided in 0004-3 (provisional): `secret` values are hidden from
   people without `manage` too, with the same lens semantics as for agents (a filter on a
   secret column sees the masked value).
3. **Share links on databases.** The draft gives share-link bearers no database operation.
   Recommended: yes until public projects (section 9.9).
4. **Pseudonym scope.** Per tenant (joins across databases of one tenant work) or per database
   (less linkable)? Recommended: per tenant, as for telemetry (section 9.6). Either way the key
   stays out of reach of every role that runs untrusted SQL, and the "write a guess, read its
   pseudonym" online test stays an accepted risk (0004-4).
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
   retention of the plan, monthly partitions (not in 0002 yet: one plain table), and no row for
   repeated identical reads within one second (a counter instead). Statements are stored
   normalized (0004-9).
9. **`sqlparser` as a new dependency** (statement count, destructive statement detection, column
   origin). Recommended: accept (Apache 2.0, widely used).
10. **Branch of a branch.** Allowed? Recommended: no in step 2 (branches are flat children of a
    live database).
11. **Typed node copy.** Decided in 0004-7 (provisional): `full` data only for a person with
    `manage` on the source; everyone else (agents, viewers, editors) gets `masked` data, and
    the UI says so.
12. **PostgreSQL Anonymizer.** Design 8.5 names it for the masking inside Postgres. 0004-4
    replaces it with Ruum's lens views (same intent, no readable masking rules or salt). A
    person should confirm, and the wording of design 8.5 should follow in a design change.
