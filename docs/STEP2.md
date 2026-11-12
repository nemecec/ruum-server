# Step 2 (Databases): work areas, task cards and order

Status: DRAFT, revised three times after panel reviews (design section 18.9): once for
reviewer 1, again for every item of reviewer 2 and the UX contract needs C-15 to C-19, and a
third time for the re-review of both reviewers (decisions 0004-24 to 0004-33). Spike S-1 passed
and its result is applied (decision 0004-34): the role kinds are frozen. The architect panel
accepted card C-1 on 2026-11-07 with the changes of decision 0004-35, which are applied: the step
2 contracts are frozen (a later change is additive, in a `contract:` commit).
The decisions are in `docs/decisions/0004-step2-spec.md` (`provisional`).

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
| REST API | `contracts/openapi.yaml`, tag `databases` (24 operations, each with `x-ruum-area` and `x-ruum-status: planned`) |
| MCP tools | `contracts/mcp/tools.yaml` (20 tools, not offered while their operations are planned; the 4 user-only operations are not exposed) |
| SpiceDB schema | `contracts/authz/schema.zed` (database permissions and agent rules), `schema.test.yaml` |
| SQL | `migrations/0004_databases.sql` |
| Journal events | `contracts/events/` (11 step 2 types, planned: `$defs/planned_types` in `journal-entry.schema.json`), card C-2 |

The step 1 operations keep their shape. `oasdiff breaking` (CI's check, `--fail-on ERR`) reports
0 errors; the warnings are the new values of the response enum `ErrorCode`. Four step 1
descriptions get additive text for typed nodes (decision 0004-6, 0004-7): `putContent` and
`createUpload` refuse a typed node (`409 conflict`), `restoreVersion` of a database follows the
manifest rules, and `copyNode` copies a database with `full` data only for a person with
`manage` on the source. `TenantPolicy` gets the optional flag `guests_create_resources`
(default false, decision 0004-10). Two new error codes: `statement_not_allowed`,
`class_read_required`.

### Planned operations (decision 0004-21)

Every step 2 operation carries `x-ruum-status: planned` in `contracts/openapi.yaml` until it is
built. `ruum-testkit` treats a planned operation like this, also with
`RUUM_REQUIRE_IMPLEMENTED=1`: it must have a standard request and registered isolation and
permission cases (the "every operation has a case" rule stays); its route may be missing or answer
`501`, and those checks are reported as `planned`, not failed; any data leak in an answer still
fails. `matrix::PLANNED_OPERATIONS` lists them, and a test keeps the list equal to the marks.
**The card that builds an operation removes its mark and its list entry** (and runs
`gen-tools.mjs`, so its MCP tool appears); `every_operation_is_routed` fails when a planned
operation answers anything but a missing route or `501`. The same holds for the step 2 event
types (`$defs/planned_types` in `contracts/events/journal-entry.schema.json`): the card that writes
the first entry of a type adds it to `EventType` and removes it from that list.

### Operations by area

| Area | Operations |
|---|---|
| `databases` | `createDatabase`, `getDatabase`, `updateDatabaseManifest`, `listDatabaseMigrations`, `applyDatabaseMigration`, `listDatabaseBranches`, `createDatabaseBranch`, `listDatabaseRestorePoints`, `createDatabaseRestorePoint`, `restoreDatabase` (user only) |
| `query` | `queryDatabase`, `getDatabaseSchema`, `listDatabaseQueries` |
| `classes` | `listDatabaseColumns`, `suggestColumnClasses`, `confirmColumnClasses` (user only) |
| `access` | `listDatabaseAccessGrants`, `createDatabaseAccessGrant` (user only), `revokeDatabaseAccessGrant`, `requestDatabaseAccessGrant` (agents), `listDatabaseApprovals`, `listPendingDatabaseApprovals`, `getDatabaseApproval`, `decideDatabaseApproval` (user only) |

### Permission table for database nodes (design 3.2, 8.4, 8.5)

| Action | Person | Agent (user's right ∩ scope, plus) |
|---|---|---|
| Read-only query | `view`; `secret` masked without `manage` | `view`; every read of `secret` masked, of `personal` as pseudonyms unless `read_personal` grant (also in `WHERE`, `JOIN`, `ORDER BY`, ...) |
| Read-write query | `edit`; may not read or write `secret` columns without `manage` | branch with `masked` / `schema_only` data: `edit`; live or `full` branch: `edit` + `write_live` grant; a write that reads or writes `personal` (without `read_personal`) or `secret` columns: `403 class_read_required` |
| Destructive statement | as the statement needs | after the `write_live` check: a pending approval (`202`) that a person decides; an approval never replaces `write_live` |
| See `secret` values | `manage` | never |
| Settings, migrations on live or `full` branch | `manage` | `manage` + `write_live` grant + an approval per migration; may not change `data_classes` |
| Migrations on a `masked` / `schema_only` branch | `edit` | `edit` |
| Dry run of a migration (`dry_run: true`) | as for the migration | only on a `masked` / `schema_only` branch; on live or a `full` branch `403 user_required` (hint `dry_run_on_draft_branch`), nothing runs, no approval |
| Branch with `masked` / `schema_only` data | `edit` | `edit` |
| Branch with `full` data | `manage` | never (`403 user_required`) |
| Branch of a branch | `409 conflict` | `409 conflict` |
| Copy a database (`copyNode`) | `full` data with `manage` on the source, else `masked` | always `masked` |
| Restore a manifest version (`restoreVersion`) | `manage` | `manage`; may not change `data_classes` |
| Content writes on a typed node (`putContent`, uploads) | `409 conflict` | `409 conflict` |
| Restore points (name one) | `edit` | `edit` |
| Point-in-time restore | `manage` (user only) | never |
| Suggest column classes | `edit` | `edit` |
| Confirm column classes | `manage` (user only) | never |
| Give a grant to an agent | the agent's user, `edit` (live writes) or `view` (personal values) | never |
| Ask for a grant (`requestDatabaseAccessGrant`) | `403` (a person gives it directly) | `view`; a pending request that only its user decides |
| Decide an approval | the agent's user or a person with `manage`, with the right the statement needs (user only); a grant request: only the agent's user | never |
| Confirm classes in a migration approval (`classes`) | `manage` (user only) | never |
| "Waiting for you" across the tenant | the requests the person may decide | its own requests |
| Query log | `manage`: all entries; `view`: own entries and those of their own agents; normalized statements, no literals, parameters as hidden markers | `view`; only its own entries |
| Engine error text | full with `manage`; else sanitized (SQLSTATE, fixed text, object names) | sanitized |
| Create a database, branch, restore into a new node, copy a typed node, as a guest | only with `guests_create_resources` | the same, for an agent of a guest |
| Share links | no database operation (`403 user_required`, as in step 1) | |
| Children of a database node | only its branches (`createDatabaseBranch`); create, upload, move or copy into it: `409`; move a branch away: `409` | the same |
| App binding (step 3) | | its own engine role kinds per binding (`app_read`, `app_write`: real values, secrets included; `database_roles.binding_id`), never the people's kinds (decisions 0004-14, 0004-32) |

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
| `crates/ruum-core/src/privacy/mod.rs` | The marker types, the `Pseudonymizer` trait and the `MaskingRules` hook (`classes_changed`); made by card Q-0; step 3 (telemetry) uses them too. |
| `crates/ruum-api/src/routes/databases/mod.rs`, `crates/ruum-core/src/databases/mod.rs` | Made by card D-1 with `501` stubs for every handler of the `databases` area and one submodule per card (`migrations`, `branches`, `backups`); later cards change only their submodule. D-1 also declares every submodule of `ruum-core/src/databases/` that a later card fills (`classes`, `query`, `access` and the three above) as an empty file, so no parallel card edits `mod.rs` (decision 0004-33). |
| `crates/ruum-core/src/databases/query/mod.rs` | Made by card Q-1; it also declares the empty `postgres_mask` and `sqlite_mask` submodules that Q-2 and Q-3 fill. |

**Planned marks and generated files.** Parallel cards each remove a line of `PLANNED_OPERATIONS`
and a mark in `openapi.yaml`, and regenerate `tools.json` and `schema.d.ts`. These conflicts are
mechanical: the integrator resolves them after the rebase by keeping both removals and running
the generators again (`gen-tools.mjs`, `npm run gen:api`), never by hand-merging generated files.

## The engine security model (decision 0004)

The query API runs SQL that agents and people write. The engine must hold the tenant border and
the privacy filter of design 8.5 even when that SQL is hostile. The parser checks of the query
layer (`sqlparser`) are a first line only; each rule below holds in the engine itself.

### Postgres

1. **Sessions.** Untrusted SQL (every statement of the query API and every migration) runs ONLY
   in a session whose `session_user` is the database role of the kind the request needs
   (`database_roles`). These roles have `LOGIN` with a short-lived random password (`VALID
   UNTIL` a few minutes ahead) that the platform sets when it opens connections and never
   stores; connections are pooled per role, that is per (database, kind, binding; the binding
   is empty except for the app kinds of step 3) and closed before the password ends. Each
   role is a member of no other role, has no ADMIN option, is `NOINHERIT`, and has `CONNECT` on
   its own database only (`REVOKE CONNECT ... FROM PUBLIC` on every database). So `RESET ROLE`,
   `SET ROLE <other>` and `set_config('role', ...)`, also inside a `DO` block or a function,
   cannot reach another role. `REVOKE TEMPORARY ON DATABASE ... FROM PUBLIC` on every database,
   so no role makes temporary objects. `pg_hba.conf` allows these roles only from the platform's
   address, with SCRAM (step 3 widens this to the app pool for the app kinds). **Pooled sessions:** each request runs in its own transaction and sets
   its options with `SET LOCAL` only (`statement_timeout`, read-only); `SET`, `RESET` and
   `set_config(..., false)` of session options are refused (`statement_not_allowed`); a
   connection goes back to the pool only after `DISCARD ALL`, and one whose transaction did not
   end cleanly is closed. Transaction-control statements (`BEGIN`, `START TRANSACTION`, `COMMIT`,
   `END`, `ROLLBACK`, `SAVEPOINT`, `RELEASE`, `PREPARE TRANSACTION`) are refused too, in queries
   and in each statement of a migration's `up` (`statement_not_allowed`), because they end the
   transaction that holds the `SET LOCAL` options and the rollback of a migration; after each
   untrusted statement the engine checks that its transaction is still open, else it fails the
   request and closes the connection (decision 0004-34 d). A role can change its own session
   defaults and password (`ALTER ROLE CURRENT_USER ...`, also from a `DO` block), so before it
   opens connections for a role, `ruum_dbhost` runs `ALTER ROLE <role> RESET ALL`, sets the
   fixed settings (`search_path`) and a new password with `VALID UNTIL`; after login the engine
   checks that `pg_db_role_setting` holds only those settings (decision 0004-34 c). The same
   login-role path serves step 3 apps (decision 15), with kinds of their own per binding (see
   the table below).
2. **The admin role.** `ruum_dbhost` (`CREATEDB`, `CREATEROLE`, no superuser) runs only platform
   DDL: create and drop databases and roles, set passwords, build the lens views, grant. It
   never runs untrusted SQL. On PostgreSQL 17 a `CREATEROLE` role gets ADMIN on the roles it
   makes but no `SET` or `INHERIT` membership (`createrole_self_grant` stays empty). To make
   objects owned by `mask` (PostgreSQL 16 and later need `SET` membership for `OWNER TO` and
   `SET ROLE`), `ruum_dbhost` grants itself `mask` `WITH INHERIT FALSE, SET TRUE` and builds the
   key, the functions and the lens views under `SET ROLE mask`. It holds no membership in a LOGIN
   kind; the LOGIN kinds are members of nothing, so no untrusted session reaches `mask` or
   `ruum_dbhost` (decision 0004-34 a).
3. **Lenses.** Each role kind reads through one lens: `base` (the tables), `nosecret` (views in
   the schema `ruum_nosecret`, every `secret` column masked) or `masked` (views in the schema
   `ruum_masked`, `personal` columns as pseudonyms and `secret` columns masked). A role has
   `USAGE` only on its lens schema, `extensions` and `pg_catalog`, and `search_path` is its lens
   schema and `extensions`; it has no right on the base tables and no `USAGE` on `public` or any
   other schema. So every read, in any clause, sees the masked value, and functions outside the
   lens are out of reach. The views are owned by the `mask` role (`NOLOGIN`; `SELECT`,
   `INSERT`, `UPDATE` and `DELETE` on the base tables, because an auto-updatable view needs the
   view owner's rights; no untrusted session ever runs as it). `ruum_dbhost` owns the schemas
   (`public`, `ruum_mask`, `ruum_masked`, `ruum_nosecret`, `extensions`); the `owner` kind has
   `USAGE` and `CREATE` on `public` only and owns the tables, not the schema, so a migration
   cannot give a lens kind `USAGE` on `public`; it has no `CREATE` on the database (no schema,
   no extension). The base-table rights of `mask` and `secret_read` come from `ALTER DEFAULT
   PRIVILEGES` that the platform runs once in an `owner` session; a migration that revokes them
   breaks the lens closed (decision 0004-34 b). Extensions of the manifest are created by
   `ruum_dbhost` in the schema `extensions` (decision 0004-34 g). The kinds:

   | Kind | Who | Lens | Writes |
   |---|---|---|---|
   | `read` | people with `view`, agents with `read_personal` | `nosecret` | none |
   | `write` | people with `edit`, agents with `read_personal` (with `write_live` or on a draft branch) | `nosecret` | plain and `personal` columns |
   | `masked_read` | agents without `read_personal` | `masked` | none |
   | `masked_write` | agents without `read_personal` (with `write_live` or on a draft branch) | `masked` | plain columns only |
   | `secret_read` | people with `manage` | `base` | none |
   | `owner` | people with `manage`; migrations | `base` | everything, owns the schema |
   | `mask` | nobody (`NOLOGIN`) | - | owns the views, `ruum_mask` and the key |
   | `app_read`, `app_write` (step 3) | one app binding each (`database_roles.binding_id`) | `base` | as the binding says; real values and secrets, because an app checks its own password hashes and tokens |

   Hiding `secret` values from PEOPLE without `manage` is done by the engine (the `nosecret`
   lens), as reviewer 1 asked; an app binding never uses the people's kinds, so step 3 does not
   need to change the people's rule (decision 0004-14). Spike S-1 confirmed the kinds; they are
   frozen (decision 0004-34).

   A masked `secret` column is `text`: `NULL` when the value is `NULL`, else a fixed constant;
   the query layer shows the hidden-value marker. A masked `personal` column is `text` holding
   the pseudonym, whatever its type in the table. The lens views are rebuilt (by `ruum_dbhost`,
   in one transaction with the catalog sync) after each migration and each confirmation.
4. **One pseudonym function for both engines.** `pseudonym(v) = 'p_' || first 12 hex characters
   of HMAC-SHA-256(tenant_key, canonical_text(v))`. `canonical_text` is the text form of the
   value with a fixed format that no session setting changes: text as is; integers in decimal;
   `numeric` without trailing fraction zeros; floats in the shortest form that reads back
   exactly, in the layout of PostgreSQL `float8out` (fixed notation for a decimal exponent in
   [-4, 15), else `d.ddde+XX`; `NaN`, `Infinity`, `-0`); `timestamptz` and `timestamp` as
   `YYYY-MM-DDTHH:MM:SS[.ffffff without trailing zeros]Z` in UTC; `date` as `YYYY-MM-DD`;
   `bytea` and BLOB as lowercase hex without `\x`; `boolean` as `true` / `false`; `uuid` in
   lowercase (decision 0004-34 e). In Postgres the canonical text is made INSIDE the function,
   never by a `::text` cast in the view (a cast follows the session's `TimeZone`, `DateStyle`,
   `extra_float_digits` and `bytea_output`): there is one overload `ruum_mask.pseudonym(<type>)`
   per type, each `SECURITY DEFINER`, owned by `mask`, with `search_path` set to `pg_catalog,
   pg_temp` and fixed output settings, and it computes the HMAC with the built-in `sha256()` from
   the padded keys in the table `ruum_mask.key` (owner `mask`, no grant to anyone; no extension
   needed, so it never clashes with a `pgcrypto` of the manifest). The lens views call it; the
   agent roles have `EXECUTE` on `pseudonym` (not on its helpers) but no `USAGE` on the schema
   `ruum_mask`, so they cannot call it by name, and they can never read the key (spike S-1
   showed both).
   The key is the pseudonym key of the database (decision 0004-25): the tenant's key
   (`tenant_keys`, card Q-4) for a live database and a `full` branch, and a key derived for the
   database, HMAC-SHA-256(tenant key, `db:` || node ID), for a branch with `masked` or
   `schema_only` data (`databases.key_node_id`; a copy or restore keeps its source's key). The
   platform gives the engine that key when it creates the database (decision 0004-30). The
   platform computes the same function in Rust for SQLite. Constants and parameters are never
   pseudonymized, and the query log and approvals store parameters as hidden markers, never as
   pseudonyms (decision 0004-24).
   Collisions: 12 hex characters are 48 bits; among 10^6 distinct values of one tenant a
   collision has a probability of about 0.2 %, among 10^7 about 16 %. Accepted for step 2: a
   collision makes two values look equal to an agent; it never shows a real value.
5. **No PostgreSQL Anonymizer in step 2.** Its masking rules are security labels that every
   role can read in `pg_seclabels`, and a salt in a setting is readable too, so a masked role
   could learn the pseudonym key. The lens views above do what design 8.5 asks ("masks inside
   the database, bound to the agent's database role") without that leak (decision 0004-4).
6. **Neutral names.** Every role can list `pg_database` and `pg_roles` of the cluster. Database
   and role names are `d_` / `r_` plus 16 random hex characters, never a node or tenant ID.
7. **Copies with TEMPLATE** (branches, `copyNode`, templates; decision 0004-27). PostgreSQL
   refuses `CREATE DATABASE ... TEMPLATE` while any session is connected to the source, idle
   ones included, and the platform keeps pooled connections open on purpose. So the engine
   drains the source: it stops handing out sessions and sets `ALLOW_CONNECTIONS false`; it
   closes every pooled connection of the source, of every kind (step 3 app pools included); it
   waits up to 5 seconds for running statements, then cancels them and ends the remaining
   sessions; it takes the copy; it allows connections again and reopens the pools (the
   reconciler does the same after a crash in the middle). New requests wait up to 10 seconds,
   then fail with the retryable `503 database_not_ready` (hint `retry`). Branches are for small
   databases (decision 18, the plan's branch limit), so the pause is a few seconds; this cost is
   accepted. A masked copy then masks with the source's key and installs the key of the new
   database in `ruum_mask.key` before it is `ready`.
8. **Exports** (decision 0004-29). A `pg_dump` export leaves out `ruum_mask` (the key),
   `ruum_masked`, `ruum_nosecret`, the roles and the grants (`--exclude-schema`, `--no-owner`,
   `--no-privileges`). A restore or import into a new database builds the Ruum schemas again
   and installs the key from `tenant_keys`.
9. **Side channels.** The agent roles have no `SELECT` on the base tables, so `pg_stats`
   (`most_common_vals`, histograms) shows them nothing; `EXPLAIN` shows plans of the views;
   `pg_stat_statements` is not installed; `pg_stat_activity` shows other sessions' queries only
   to their own role (default). Timing differences of a query over masked values are accepted.
   PostgreSQL leaves row values out of the `DETAIL` of a constraint error for a role without
   `SELECT` on the base table (spike S-1); the sanitizing of errors stays as a second line.
10. **Migrations and the lens views** (decision 0004-34 f). A lens view depends on the columns it
   reads: `ALTER COLUMN ... TYPE` and `DROP COLUMN` fail while it exists, and `DROP TABLE ...
   CASCADE` drops it. So before a migration `ruum_dbhost` drops the lens views of the database;
   the migration runs in an `owner` session; then the catalog sync and the rebuild of the lens
   views run in one `ruum_dbhost` transaction. While the views are gone, the lens kinds fail
   closed (the database is `updating`; requests wait, then get the retryable
   `503 database_not_ready`). A failed migration rebuilds the views from the unchanged catalog.
   Before each statement of `up`, the engine sets its own `statement_timeout` and
   `lock_timeout` again, and it enforces a wall-clock limit for the whole migration with a
   protocol cancel request, then closes the connection (decision 0004-35), so a `SET LOCAL` or a
   `set_config` in a `DO` block cannot lift the limits.

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
   `ruum_pseudonym` is registered per connection in Rust with the pseudonym key of the
   database (the tenant key, or the derived key of a `masked` / `schema_only` branch; the
   platform hands it to the engine when it opens the connection); the key never enters the
   file. The authorizer denies every read (`SQLITE_READ`) of a `personal` or
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
- **The query log** stores the normalized statement (literals replaced) and each parameter as a
  hidden marker, never a pseudonym; approvals store parameters the same way; agents read only
  their own entries (decision 0004-24).
- **Draft branches have their own pseudonym key** (decision 0004-25). On a branch with `masked`
  or `schema_only` data an agent writes rows and runs migrations without a person, so the
  branch's lens uses a key derived for the branch: a value written there never gives its
  live-database pseudonym. No SQL joins a branch with its source, so nothing needs the two to
  match.
- **Agents dry-run migrations only on draft branches** (decision 0004-26): a dry run runs as
  `owner` and takes the migration's locks.

### Accepted risk

An agent with a `write_live` grant on a live database (or a `full` branch) can insert a guessed
value and read its pseudonym, then compare it with the pseudonyms of a column: an online test
of one guess per write on real data, logged, rate limited, and needing a grant that a person
gave. On a draft branch, where an agent writes without a person, the lens uses the branch's own
key (decision 0004-25), so rows written there, a `DO` block that gets around the missing schema
`USAGE`, and a migration that runs as `owner` all give pseudonyms that match nothing outside
the branch. Parameters are never stored as pseudonyms (decision 0004-24). None of these gives
the key or an offline dictionary attack (decision 0004-4; relates to open question 4).

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
| Notes | The engine never sees a platform token. It gets the database, a role reference (database, kind, optional binding; decision 0004-32) and the statement, and, when it creates a database or opens a SQLite connection, the pseudonym key of that database (decision 0004-30). Postgres: untrusted SQL runs in sessions whose `session_user` is the per-database LOGIN role of that kind (short-lived password, pooled per role, member of no other role); `ruum_dbhost` runs platform DDL only; the lens views and the pseudonym function (see "The engine security model"). SQLite: one file per database in a per-tenant directory, the authorizer and the hardening flags on every connection, TEMP lens views, opened read-only for read queries, replicated to Garage. |

### 3. Databases (control plane)

| | |
|---|---|
| Owns | `crates/ruum-core/src/databases/{mod,manifest,provider,branches,backups,migrations}.rs`, `crates/ruum-api/src/routes/databases/{mod,migrations,branches,backups}.rs` (D-1 makes `mod.rs` with `501` stubs; each later card changes only its submodule) |
| Implements (OpenAPI) | area `databases` |
| Tables | `databases`, `database_roles`, `database_migrations`, `database_restore_points`, `database_restores` |
| Notes | The database provider (`ruum.database`) implements `providers::Provider` on top of `DatabaseEngine`. Manifest validation: the JSON Schema (`contracts/manifests/database.schema.json`, compiled in) plus the provider rules. Plan limits from configuration (`RUUM_PLAN_LIMITS`, section 4.3: count and size per plan; statement timeout, row cap, result size, branch source limit). |

### 4. Query API and privacy filter

| | |
|---|---|
| Owns | `crates/ruum-core/src/databases/query/`, `crates/ruum-core/src/privacy/` (`mod.rs` by Q-0, `keys.rs` by Q-4), `crates/ruum-api/src/routes/databases/query.rs` |
| Implements (OpenAPI) | area `query` |
| Tables | `database_queries` |
| Notes | One statement per call (parse with `sqlparser` to count statements, find destructive ones, refuse the statements of `statement_not_allowed`, find the restricted columns a write reads or writes (`class_read_required`), and the columns a result reads; new dependency, needs approval). The role kind follows the caller and the grants (table in "The engine security model"). Read-only transaction for `mode: read`; `statement_timeout`; row and size caps. Result JSON with the typed markers (`HiddenValue`, `Pseudonym`, `LiteralValue`). Masking happens in the engine, through the lens, so every clause sees masked values; the query layer only turns masked values into markers, by the class of each result column (the strictest class it reads; unknown -> `personal`; no column read -> `normal`, never masked). Pseudonyms: one function for both engines (HMAC-SHA-256 with the pseudonym key of the database: the tenant key of `tenant_keys`, wrapped by a key in OpenBao, card Q-4, or the key derived from it for a `masked` / `schema_only` branch; in tests a fixed key; `p_` + 12 hex characters). The query log: a person with `view` reads their own entries and those of their own agents; `manage` reads all; agents read their own. Engine errors are sanitized for agents and people without `manage`. The query log stores the normalized statement (literals replaced) and each parameter as a hidden marker, never a pseudonym (decision 0004-24); agents read only their own entries. |

### 5. Column classes

| | |
|---|---|
| Owns | `crates/ruum-core/src/databases/classes/`, `crates/ruum-api/src/routes/databases/classes.rs` |
| Implements (OpenAPI) | area `classes` |
| Tables | `database_columns` |
| Notes | Catalog sync after each migration, after each schema change through the query API, and on a schedule (new columns start unconfirmed = `personal`; dropped columns get `dropped_at`). Classes follow column identity (Postgres `rel_oid`/`attnum`/`type_oid`; SQLite by parsing the DDL the platform runs): a rename, retype, table swap or rebuild, or a drop and add of the same name clears the confirmation (`unconfirmed_reason`) and removes the key from `data_classes` (a new manifest version by the system). Suggestions store the newest one per column; every suggestion and confirmation goes to the audit log. Confirmation writes a new manifest version (`data_classes`) and rebuilds the lens views. A rule-based suggester (names such as `email`, `phone`, `password`, `token`, `name`, `address`, `note`) as the default for existing data; agents can do better and send their own. The digest lists unconfirmed columns. |

### 6. Agent access: grants and approvals

| | |
|---|---|
| Owns | `crates/ruum-core/src/databases/access/`, `crates/ruum-api/src/routes/databases/access.rs` |
| Implements (OpenAPI) | area `access` |
| Tables | `database_access_grants`, `database_approvals` (subjects `statement`, `migration`, `grant`) |
| Contract | `contracts/authz/schema.zed`: `draft_branch`, `live_writer`, `personal_reader`, `agent_write_live`, `agent_read_personal` |
| Notes | A grant writes its tuple through the outbox and records a restore point first (`write_live`). The core checks the tuple AND the row (`expires_at`, `revoked_at`) on every query; the sweeper deletes the tuple at the end and sends `access_changed`. `draft_branch` is written only for branches with `masked` or `schema_only` data. Destructive statements and agent migrations on real data: checked after `write_live` (an approval never replaces it); `statement_hash` = BLAKE3 of the statement and the canonical parameters (a migration: name and `up`); an approval is used once, within 15 minutes. The agent's user or a person with `manage` decides, with the right the statement needs. `estimated_rows` from `EXPLAIN` or a `count(*)` in a read-only transaction, never a test run. A migration request carries `planned_columns` (from parsing `up`; nothing runs before the approval), and the person may confirm their classes in the decision (`classes`), applied in the same transaction as the migration (UX C-15). An agent may ask for a grant (`requestDatabaseAccessGrant`, UX C-17); only its user decides, and the approval makes the grant. `listPendingDatabaseApprovals` is the "Waiting for you" list across the tenant (UX C-18). Every request has a `review_url`, and the hints of `write_grant_required`, `class_read_required` and `user_required` carry the web URL of the right page (UX C-16; IDs only, never values). |

### 7. Infrastructure (laptop stack)

| | |
|---|---|
| Owns | `deploy/dev/`, `scripts/dev.sh`, `docs/DEVSTACK.md` |
| Notes | See "Infrastructure work" below. |

### 8. MCP and CLI

| | |
|---|---|
| Owns | `crates/ruum-mcp/`, `crates/ruum-cli/` |
| Implements | The 20 database tools of `contracts/mcp/tools.yaml` (each appears when its operation loses the planned mark); `ruum db create|ls|query|schema|branch|migrate|columns|restore-points`. Query results keep the markers as JSON; the CLI prints them as `<hidden: secret>` and `<person 8f3a91>`. Every tool result with rows says that rows are untrusted data. |

### 9. Web UI

| | |
|---|---|
| Owns | `web/` except `web/src/api/schema.d.ts` |
| Implements | "New database" in a folder; the database page (status, settings, size, limits); a query console (read-only by default, markers as chips "hidden: secret", "person 8f3a91"); the column class list with suggestions filled in and one confirm button; branches; restore points with "Restore to a new database" and "Restore in place"; grants (give, see, revoke) and pending approvals with the statement and the row count. UX package first (section 18.4.1, card U-1). |

### 10. Suites (test harness)

| | |
|---|---|
| Owns | `crates/ruum-testkit/` |
| Implements | Isolation and permission cases for the 24 operations (the standard requests, the isolation cases and the permission rules exist on `f/step2spec`, reported as `planned` until built; `USER_ONLY_OPERATIONS` has the 4 user-only operations and `PLANNED_OPERATIONS` the 24 planned ones); a **privacy suite**: an agent never gets a raw `secret` value or a raw `personal` value without a grant, through any operation (query, schema, columns, query log, approvals, migrations, errors, status, branches with `masked` data, copies, exports) and through any clause or side channel; mutation rules for each privacy and grant rule. |

## Infrastructure work

1. **A data cluster.** The shared data cluster is a Postgres server of its own, not the
   platform database (a customer database never shares a server with the platform metadata,
   also on the laptop). Laptop: a new compose service `postgres-data` (image
   `ruum-dev/postgres-data:17`, built from `deploy/dev/postgres-data/Dockerfile`: Postgres 17
   with PostGIS and pgvector; no PostgreSQL Anonymizer, decision 0004-4), port 5435 on
   127.0.0.1, a volume, WAL archiving on, `pg_stat_statements` not installed,
   `max_prepared_transactions = 0`. `template1` holds no extension (the platform image puts
   PostGIS there; the data cluster must not), and the control files of `postgis` and `vector`
   say `trusted = true`, so `ruum_dbhost` can create them (decision 0004-34 g).
2. **Per-database roles.** An init script creates the admin role `ruum_dbhost` with `CREATEDB`
   and `CREATEROLE` (no superuser), `REVOKE CONNECT, TEMPORARY ON DATABASE ... FROM PUBLIC` for
   `postgres` and `template1`, `REVOKE ALL ON SCHEMA public FROM PUBLIC` in `template1`, and `pg_hba.conf` lines that allow the per-database
   roles only with SCRAM from the platform's address. The engine creates, per database, the
   roles of `database_roles` (LOGIN, `NOINHERIT`, member of no role, `CONNECT` on their own
   database only; `mask` NOLOGIN), and runs `REVOKE TEMPORARY ON DATABASE ... FROM PUBLIC` on
   each database. Step 3 adds `pg_hba.conf` lines for the app kinds from the app pool's
   addresses (decision 15). They are NEVER granted to `ruum_dbhost`: untrusted SQL runs
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
5. **The key-encryption key for tenant keys** (card Q-4; the platform hands the engine the
   key of each database, decision 0004-30). Production: an OpenBao transit key
   `ruum-tenant-keys` (OpenBao with persistent storage). Laptop: OpenBao runs in dev mode and
   loses its data on a restart, so the KEK is a local file (`RUUM_LOCAL_KEK_FILE`, made by
   `dev.sh up` next to the platform database volume and removed by `dev.sh clean`, so wrapped
   keys and their KEK live and die together). `dev.sh env` prints it.
6. **No connection proxy** (decision 15). The design's proxy on `<node-id>.db.hel1.ruum.space`
   comes later; nothing in step 2 listens for the Postgres wire protocol from outside.
7. **CI**: the `postgres-data` service in the e2e job; `redocly lint` also for
   `contracts/providers/provider.openapi.yaml`; an `ajv` check of the manifest examples.

## Task cards

IDs: `C` contract, `S` spike, `I` infrastructure, `P` providers, `E` engines, `D` databases, `Q`
query and privacy, `K` classes, `A` access, `M` MCP and CLI, `U` UI, `T` tests. "After" lists the
cards that must be merged first. Every card that builds an operation also removes its
`x-ruum-status: planned` mark and its `PLANNED_OPERATIONS` entry, regenerates
`crates/ruum-mcp/src/tools.json`, and seeds what its standard request needs in
`crates/ruum-testkit/src/fixtures.rs` (the fixture field names stay); a card that writes the
first entry of an event type adds it to `EventType` and removes it from `$defs/planned_types`. These
files are not listed again under "May change".

```markdown
ID:          C-1
Goal:        Review and freeze the step 2 contracts of f/step2spec (this draft).
Contracts:   contracts/providers/*, contracts/manifests/database.schema.json, the `databases` tag of
             openapi.yaml, schema.zed, mcp/tools.yaml, migrations/0004_databases.sql.
May change:  those files, the generated web/src/api/schema.d.ts and crates/ruum-mcp/src/tools.json;
             crates/ruum-testkit/ (USER_ONLY_OPERATIONS, PLANNED_OPERATIONS, the planned outcome,
             the database fixture fields, standard requests, isolation cases and permission
             rules of the 24 operations, permission_matrix.md); the planned flag of
             crates/ruum-mcp (gen-tools.mjs, catalog.rs, its tests); the envelope test of
             crates/ruum-core/src/journal/mod.rs ($defs/planned_types); docs/TESTING.md; the copy of
             the new error codes in web/src/api/errors.ts (the type needs one entry per
             ErrorCode). Everything needed so that main stays green.
Must not:    product code (handlers, stores, engines).
Done when:   the architect panel accepts (two reviewers); redocly, zed validate, oasdiff (0 errors),
             the migration applies, ajv for the manifest and event examples; USER_ONLY_OPERATIONS
             carries the four new user-only operations (confirmColumnClasses, restoreDatabase,
             createDatabaseAccessGrant, decideDatabaseApproval) in the same commit as their
             `x-ruum-user-only` marks, so `matrix::tests::user_only_list_matches_the_contract`
             stays green on main; every step 2 operation is `x-ruum-status: planned` with a
             standard request and isolation and permission cases; cargo test --workspace and
             RUUM_REQUIRE_IMPLEMENTED=1 cargo test -p ruum-testkit (memory and postgres) pass;
             the provisional decision record 0004 lists what changed; spike S-1 is done and its
             result is in 0004 before the database_roles kinds are frozen (done: 0004-34; the
             kinds do not change); the description of queryDatabase and applyDatabaseMigration
             lists the transaction-control statements under "Statements that never run"
             (0004-34 d).
Status:      ACCEPTED (2026-11-07, branch c/c1-acceptance): both reviewers accepted with
             changes (c1-acceptance-review-1.md, c1-acceptance-review-2.md); the changes are
             applied (decision 0004-35: transaction control and the rules of `up`, unqualified
             table names, names next to principal IDs, `503 DatabaseNotReady`).
After:       S-1 (for the freeze only; the review can start before)
Out of scope: journal event schemas (C-2).
```

```markdown
ID:          C-2
Goal:        Journal event schemas for step 2 (drafted on f/step2spec with C-1; this card is done
             when C-1 merges).
Contracts:   contracts/events/: typed_node.status_changed, database.columns_confirmed,
             database.migration_applied, database.migration_failed, database.branch_created,
             database.restore_started, database.restored, database.restore_failed,
             database.grant_created, database.grant_revoked, database.approval_decided;
             EventType in openapi.yaml; examples.
May change:  contracts/events/, the EventType enum, generated files.
Must not:    product code.
Done when:   every example validates (ajv); oasdiff 0 errors; the types are in `$defs/planned_types`
             of the envelope and the envelope test of ruum-core keeps EventType equal to the
             built types. Payloads hold names, IDs and counts only, never row values.
After:       C-1
Out of scope: provider webhooks over HTTP (step 6).
```

```markdown
ID:          S-1
Status:      DONE (2026-11-07, branch s2/spike): 225 checks passed on PostgreSQL 17.11; the
             kinds stand; the fixes are decision 0004-34 (a) to (g).
Goal:        Spike: prove the Postgres engine security model before the role kinds freeze.
Contracts:   docs/STEP2.md "The engine security model"; database_roles in 0004.
May change:  spikes/step2-roles/ (new, scripts and a short README), docs/decisions/0004 (result).
Must not:    crates/, contracts (a contract change that the result needs goes to C-1).
Done when:   on the postgres-data cluster of I-1, a script shows: (1) a LOGIN role of each kind,
             member of no role, cannot reach another role with RESET ROLE, SET ROLE,
             set_config('role'), also in a DO block or a function; (2) a masked role reads
             personal columns through the `ruum_masked` views as pseudonyms in every clause
             (WHERE, JOIN, ORDER BY, GROUP BY, aggregates), and cannot read the base tables,
             `ruum_mask.key` or `pg_stats` values; (3) `ruum_mask.pseudonym` (SECURITY DEFINER,
             owner `mask`) runs from a view but not by name without schema USAGE; (4)
             `masked_write`: an UPDATE or INSERT of plain columns through the view works, a write
             of a masked column fails, a WHERE on a masked column compares the pseudonym, and
             rows_affected tells nothing beyond what the masked read tells; (5) SET LOCAL and
             DISCARD ALL leave no session option on a pooled connection; (6) the SQL and the
             Rust pseudonym agree on text, numbers, times and bytea. If a point fails, the result
             names the change to the kinds or the lens, and C-1 takes it before the freeze.
After:       I-1
Out of scope: SQLite (E-3), performance.
```

```markdown
ID:          T-1
Goal:        Acceptance tests and the extra suite cases for all 24 database operations, before the
             code (the standard requests, isolation cases and permission rules of the 24
             operations already exist from C-1, reported as planned).
Contracts:   openapi.yaml tag `databases`, schema.zed, tools.yaml.
May change:  crates/ruum-testkit/ (isolation cases, permission matrix rows, the extra cases),
             crates/ruum-api/tests/acceptance/databases_*.rs (new).
Must not:    product code, contracts.
Done when:   the acceptance tests exist and are planned or pending until built; a person or the
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
             write_grant_required); children of a database (createNode, createUpload new_file,
             moveNode and copyNode into it: 409; moveNode of a branch away: 409; copyNode of a
             database copies no branch; trash and purge take the branches along); a grant request
             (agent: 201, again: 200 with the same request; a person: 403; decided by the agent's
             user: the grant exists; by another manager: 403); a migration approval with classes
             (the classes are confirmed after the migration ran, not before; an agent cannot send
             classes: 403 user_required); the query log for a person with view (own and own
             agents' entries only) and for an agent (own only); listPendingDatabaseApprovals
             (a person sees what they may decide, an agent its own); an agent's dry run
             (`dry_run: true`) on live and on a full branch (403 user_required, also with
             write_live; nothing ran, no approval made, no lock taken) and on a masked branch
             (200); a branch of a branch (409 conflict); restoreDatabase of a branch to a time
             before it was ready (422). From decision 0004-35: `public.t` (Postgres) and `main.t`
             (SQLite) in queryDatabase and in `up` are 422 statement_not_allowed with hint
             `unqualified_names`, for a person with manage, a viewer and an agent; an `up` with
             `BEGIN` or `COMMIT` is 422 with hint `remove_transaction_control` and writes no
             restore point, migration row or journal entry; `SET LOCAL statement_timeout` in a
             query and in `up` is 422; database_not_ready is always `503` with `Retry-After`
             (queryDatabase, getDatabaseSchema, applyDatabaseMigration, createDatabaseBranch,
             createDatabaseRestorePoint, restoreDatabase).
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
             `EXPLAIN` from an agent role show no value. Pseudonym oracle cases (decisions
             0004-24, 0004-25): a parameter in listDatabaseQueries and in getDatabaseApproval
             is a hidden marker, never a pseudonym, and never equals the lens pseudonym of the
             same value; the same value read through a masked or schema_only branch's lens
             and through its source's lens gives different pseudonyms (rows the agent wrote
             into the branch, a migration that inserts rows, a DO block on the branch); a copy
             of such a branch keeps the branch's key. Export cases (decision 0004-29): an
             export of a database (by a person and for an agent; full and masked) holds no
             key bytes and no `ruum_mask`, `ruum_masked` or `ruum_nosecret` object.
After:       C-1
Out of scope: telemetry privacy (step 3).
```

```markdown
ID:          Q-0
Goal:        The privacy border: crates/ruum-core/src/privacy/mod.rs with the marker types
             (HiddenValue, Pseudonym, LiteralValue), the Pseudonymizer trait (one function for
             both engines, keyed by the pseudonym key of a database: the tenant key, or the key
             derived for `databases.key_node_id`), and the MaskingRules hook
             (classes_changed(node): the classes of a database changed; rebuild what depends on
             them). It also declares the empty submodules `keys`, `postgres` and `sqlite`, so
             Q-4, Q-2 and Q-3 never edit mod.rs.
Contracts:   openapi.yaml (the markers), docs/STEP2.md "The engine security model" item 4.
May change:  crates/ruum-core/src/privacy/mod.rs, empty privacy/{keys,postgres,sqlite}.rs, the
             lib.rs export line (integrator).
Must not:    databases/, engines, contracts.
Done when:   the marker types serialize exactly as the contract schemas; the Rust pseudonym
             matches fixed test vectors (text, numbers, times, bytea; canonical_text); the
             derived key HMAC-SHA-256(tenant key, `db:` || node ID) matches a test vector; the
             canonical_text rules of decision 0004-34 (e) hold, with the values of
             spikes/step2-roles/values.tsv and the SQL output of the spike as test vectors, and
             the card lists the types that use the generic overload; an
             in-memory MaskingRules records calls, so K-1 can call the hook and Q-2 and Q-3
             implement it without touching each other's files.
After:       C-1
Out of scope: the key store (Q-4), the lens views (Q-2, Q-3).
```

```markdown
ID:          Q-4
Goal:        The tenant pseudonym key: tenant_keys (0004), the key-encryption key behind a
             KeyWrapper trait (OpenBao transit in production; a local KEK file on the laptop and
             in tests), the key made on first use, exported and imported with the tenant; the
             pseudonym key of a database (`database_key(tenant, key_node_id)`: the tenant key,
             or the key derived for a masked / schema_only branch, decision 0004-25).
Contracts:   migrations/0004_databases.sql (tenant_keys), decision 0004-16.
May change:  crates/ruum-core/src/privacy/keys.rs, scripts/dev.sh (RUUM_LOCAL_KEK_FILE in
             up/env/clean), docs/DEVSTACK.md.
Must not:    other modules, contracts.
Done when:   two racing first uses keep one key (primary key); the key is stored wrapped only:
             no plain key in any platform table or log, and the only plain copies are
             `ruum_mask.key` inside the engine databases (owner `mask`) and the per-connection
             SQLite registration; a restart of the laptop stack keeps the same
             pseudonyms (KEK file persists; OpenBao dev mode does not matter); a tenant export
             carries the key re-wrapped for the target and an import gives the same pseudonyms;
             the key is never rotated (a test asserts key_version 1); tenant isolation of the
             table (RLS and the tenant filter).
After:       Q-0
Out of scope: key rotation (not specified).
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
             another role (current_user stays the same or the statement fails), each tested from
             a session that LOGS IN as the per-database role (not from ruum_dbhost with SET
             ROLE); nothing listens outside 127.0.0.1; template1 holds no extension; ruum_dbhost
             can create every extension of the manifest allow-list (postgis and vector marked
             trusted); max_prepared_transactions is 0 (decision 0004-34 g, d). The checks of
             spikes/step2-roles/spike.sh sections 1, 7 and 8 are a starting point.
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
Contracts:   migrations/0004_databases.sql, provider.openapi.yaml.
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
             covers a database node; children of a typed node follow `child_types` of the
             provider descriptor for every type: createNode, createUpload (new_file), moveNode
             and copyNode into a database node give 409, moveNode of a branch away from its
             source gives 409, copyNode of a database copies no branch, trash and purge take the
             branches along (each gets its hook); a copy keeps the source's key_node_id, loads
             the key through Q-4 and the engine installs it in the new database (decision
             0004-30); a Postgres copy takes the drained TEMPLATE path of E-2.
After:       P-2, Q-4
Out of scope: -
```

```markdown
ID:          E-1
Goal:        The DatabaseEngine trait and the in-memory (SQLite in memory) engine.
May change:  crates/ruum-dbengine/ (new crate), the root Cargo.toml members line (integrator).
Must not:    ruum-core.
Done when:   the engine conformance suite (create, drop, catalog, read-only query, write, timeout,
             row cap, branch, restore point, export) passes on the memory engine; the trait
             takes the pseudonym key of the database (a key handle, never read back) when it
             creates a database, copies one (branch, copy, restore into a new node) and opens
             a SQLite connection (decision 0004-30); it selects a role by a role reference
             (database, kind, optional binding), never by a bare kind, so step 3's app
             bindings do not change the border (decision 0004-32); its export leaves out the
             Ruum schemas, roles and grants (decision 0004-29).
After:       C-1
Out of scope: masking (Q-2, Q-3).
```

```markdown
ID:          E-2
Goal:        The Postgres engine on the shared data cluster: databases with neutral names, the
             per-database LOGIN roles with short-lived passwords and a pool per role
             (database, kind, binding), the lens views and the pseudonym function (owner `mask`), read-only
             transactions, statement_timeout, branches with TEMPLATE (masked: pseudonyms, non-text
             personal columns retyped to text), restore points (pg_create_restore_point / LSN),
             catalog with column identity (rel_oid, attnum, type_oid), sanitized errors, export
             with pg_dump (without the Ruum schemas, roles and grants); pooled sessions with SET
             LOCAL and DISCARD ALL; the drain of a source before a TEMPLATE copy (decision
             0004-27); the pseudonym key of each database in `ruum_mask.key` (decision 0004-30).
May change:  crates/ruum-dbengine/src/postgres/.
Must not:    other crates.
Done when:   the engine conformance suite passes against postgres-data; a role of one database
             cannot read another; from a query, RESET ROLE, SET ROLE and set_config('role')
             cannot reach another role (also in a DO block); ruum_dbhost never runs a caller's
             statement (a test hook asserts session_user for every untrusted statement); a
             masked role cannot read a base table, `ruum_mask.key` or call
             `ruum_mask.pseudonym` by name; the Rust and the SQL pseudonym agree on a fixed
             list of values of every common type; a statement over the timeout is cancelled;
             after a request, a pooled connection holds no session option (`SHOW` of
             statement_timeout, role and search_path equal a fresh session), and a connection
             whose transaction failed is closed; S-1's script passes against the engine; a
             branch of a database succeeds while pooled connections of several kinds are open
             and while a long statement runs (cancelled after the bound; requests during the
             pause wait, then get database_not_ready), and connections work again afterwards;
             a masked branch masks with the source's key and holds the branch's key in
             `ruum_mask.key` before it is ready; a pg_dump export holds no `ruum_mask`,
             `ruum_masked` or `ruum_nosecret` object, no role and no grant, and a restore of it
             into a new database installs the key handed in; the pseudonym function has
             `search_path` `pg_catalog, pg_temp`, and no per-database role has TEMPORARY.
             From S-1 (decision 0004-34): ruum_dbhost holds SET-only membership in `mask` and
             none in a LOGIN kind; ruum_dbhost owns every schema and the owner kind owns only
             tables; one `ruum_mask.pseudonym(<type>)` overload per type with fixed settings
             (the Rust and SQL pseudonyms agree under hostile session settings); before opening
             connections for a role, ruum_dbhost resets its settings and password, and a
             connection whose role has other settings is closed; after each untrusted statement
             the transaction is still open, else the connection is closed; the lens views are
             dropped before a migration and rebuilt with the catalog sync after it (also after a
             failed one); extensions live in the schema `extensions`, reachable by every kind.
             From the C-1 acceptance (decision 0004-35): before each statement of a migration's
             `up`, the engine sets statement_timeout and lock_timeout again (a `SET LOCAL
             statement_timeout = 0` or a `set_config('statement_timeout', '0', true)` in a DO
             block in one statement does not lift the timeout of the next), and a wall-clock
             limit for the whole migration ends it with a protocol cancel request and closes the
             connection; the one-time ALTER DEFAULT PRIVILEGES also grants USAGE on sequences to
             the write and masked_write kinds (a conformance case inserts through both write
             lenses into a `serial` table and a `generated always as identity` table).
After:       E-1, I-1, S-1 (done), Q-0
Out of scope: point-in-time restore (E-4).
```

```markdown
ID:          E-3
Goal:        The SQLite engine: files in per-tenant directories, read-only opens, busy timeout,
             interrupt on timeout, the authorizer hook and the hardening flags, the
             registration of `ruum_pseudonym` with the key handed in, branch by file copy
             (online backup API), catalog, export. Q-3 fills in the TEMP lens views and the
             restricted-column rules of the authorizer (the same split as E-2 and Q-2).
May change:  crates/ruum-dbengine/src/sqlite/ except sqlite/lens.rs (Q-3; E-3 makes it empty).
Done when:   the engine conformance suite passes; one writer at a time; a read never writes; on
             every connection an authorizer is set (a test asserts it) and denies ATTACH, DETACH,
             PRAGMA outside the read-only allow-list (every PRAGMA that writes, also in read
             mode), load_extension (and the C API extension loading is off), VACUUM (also INTO)
             (the denial of reads of restricted main columns outside a lens view is Q-3's);
             SQLITE_LIMIT_ATTACHED is 0, SQLITE_DBCONFIG_DEFENSIVE is on, trusted_schema is
             OFF; an isolation case: an agent of tenant A attaches tenant B's file (refused,
             nothing read); the engine sets `PRAGMA defer_foreign_keys = ON` itself in every
             migration transaction, so a table rebuild works without `PRAGMA foreign_keys`
             (decision 0004-35).
After:       E-1, I-3, Q-0
Out of scope: the TEMP lens views and the restricted-column rules (Q-3).
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
             crates/ruum-core/src/databases/{migrations,branches,backups}.rs and the empty
             module files classes/mod.rs, query/mod.rs, access/mod.rs (declared only),
             crates/ruum-api/src/routes/databases/mod.rs (new),
             crates/ruum-api/src/routes/databases/{migrations,branches,backups}.rs (new, stubs
             only), the fixture database in crates/ruum-testkit/src/fixtures.rs.
Must not:    migrations, contracts, other modules.
Done when:   every handler of the `databases` area exists, the ones of later cards as `501` stubs
             in their own submodule (routes/databases/{migrations,branches,backups}.rs, and the
             same split in crates/ruum-core/src/databases/), so D-2, D-3 and D-4 never edit the
             same file; the fixture seeds the database node `<prefix>-garden` in Projects; T-1
             cases for these operations pass on memory and Postgres; an agent cannot change
             data_classes; status goes provisioning -> ready; a guest needs
             guests_create_resources; a contract test keeps the OpenAPI DatabaseManifest and
             contracts/manifests/database.schema.json equal (keywords, enums, limits; only
             readOnly and descriptions may differ); isolation and permission suites pass;
             databases/mod.rs declares all six submodules (migrations, branches, backups,
             classes, query, access), so K-1, Q-1 and A-1 never edit it; createDatabase loads
             the tenant key through Q-4 and the engine installs it (decision 0004-30): two
             databases of one tenant give the same pseudonym for the same value, two tenants
             do not. No card of step 2 reads the manifest's `migrations` path: do not build a
             folder reader (step 4 adds a new property for migrations in a repo, decision
             0004-35).
After:       P-2, E-1 (memory first), E-2 for the Postgres run, Q-4, C-2
```

```markdown
ID:          D-2
Goal:        Migrations: applyDatabaseMigration, listDatabaseMigrations (decision 19).
May change:  crates/ruum-core/src/databases/migrations.rs, routes/databases/migrations.rs.
Done when:   a failing migration leaves the database as before (422 migration_failed); a name runs
             once; a restore point comes first; new columns arrive unconfirmed with the author's
             suggestions; renamed, retyped, rebuilt and re-added columns arrive unconfirmed; an
             agent on live or a full branch needs write_live and an approval (202, then the same
             name and up with approval_id); the stored error is sanitized for agents and people
             without manage; an approved migration confirms the classes the person sent in the
             decision in the same transaction (entries for columns that do not exist afterwards
             are ignored); an agent's dry run on live or a full branch is 403 user_required
             before anything runs or is locked, and no approval is made; on a masked or
             schema_only branch it runs (decision 0004-26); an `up` with a transaction-control
             statement is 422 statement_not_allowed (hint remove_transaction_control, the
             statement number in `message`; no restore point, migration row or journal entry),
             and so is an `up` with `SET`, `SET LOCAL`, `RESET` or `set_config`; `CREATE
             SCHEMA` and `CREATE EXTENSION` in `up` fail on the engine rights (422
             migration_failed, the database as before; decision 0004-35); a migration that retypes or drops a column
             read by a lens view works (the views are dropped first and rebuilt after, also
             after a failure; decision 0004-34 f).
After:       D-1, K-1, A-2, C-2
```

```markdown
ID:          D-3
Goal:        Branches: createDatabaseBranch, listDatabaseBranches (masked by default).
May change:  crates/ruum-core/src/databases/branches.rs, routes/databases/branches.rs.
Done when:   a masked branch holds pseudonyms for personal columns (non-text ones retyped to text)
             and NULL for secrets; full needs a person with manage; a source over the limit is 422
             database_too_large; the branch is a child node that inherits grants; only masked and
             schema_only branches have draft_branch; a guest needs guests_create_resources; the
             branch has copies of the source's database_columns rows (confirmed classes kept) and
             of its applied database_migrations rows as of branch_at, so a branch neither counts
             every column as unconfirmed nor runs 0001_... again; masking by type: personal ->
             text pseudonym, secret -> NULL with NOT NULL dropped, CHECK constraints that read a
             masked column dropped, keys, UNIQUE, indexes and foreign keys built again and left
             out (named in status.message, no values) when a pseudonym collision breaks them,
             triggers disabled during the copy; a branch is always of the current state (no
             `at`; restore first for an earlier time); the copy drains the source first (E-2's
             done-when: a branch of a database in use succeeds; decision 0004-27); the copied
             database_columns rows take their identity (rel_oid, attnum, type_oid, data_type)
             from the branch's catalog after the copy and the masking, matched by schema, table
             and column name, so after a masked branch listDatabaseColumns of the branch shows
             no unconfirmed column that was confirmed in the source, and a catalog sync of the
             branch changes nothing (decision 0004-28); a masked or schema_only branch has
             key_node_id = its node ID and its lens pseudonyms differ from the source's for the
             same value (decision 0004-25); a branch of a branch is 409 conflict; the restore
             window of a branch starts at its ready time.
After:       D-1, Q-2, Q-4, A-1, C-2
```

```markdown
ID:          D-4
Goal:        Restore points and restore: listDatabaseRestorePoints, createDatabaseRestorePoint,
             restoreDatabase.
May change:  crates/ruum-core/src/databases/backups.rs, routes/databases/backups.rs.
Done when:   restore to a new node and in place work on Postgres and SQLite inside the window; a
             time outside the window is 422 (for a branch the window starts at its ready time);
             agents get 403 user_required (hint: the web URL of the Backups tab); a restore
             into a new node keeps the source's key_node_id and loads the key through Q-4, and
             the engine installs it (decision 0004-30).
After:       D-1, E-4, Q-4, C-2
```

```markdown
ID:          Q-1
Goal:        queryDatabase for people: one statement, read-only by default, timeout, caps, the
             result JSON, LiteralValue wrapping, the query log; getDatabaseSchema;
             listDatabaseQueries.
May change:  crates/ruum-core/src/databases/query/ except postgres_mask.rs and sqlite_mask.rs
             (Q-1 declares them empty in query/mod.rs; Q-2 and Q-3 fill them),
             crates/ruum-api/src/routes/databases/query.rs.
Done when:   T-1 cases pass; two statements are 422; a write in read mode is read_only_query; the
             statements of statement_not_allowed are 422 (also SET, SET LOCAL, RESET and
             set_config of any option, and transaction-control statements with hint
             remove_transaction_control, decision 0004-34 d); `public.t` and `main.t` are 422
             with hint `unqualified_names`, for people and agents, and the server never
             rewrites a name (decision 0004-35); database_not_ready is 503 with Retry-After; the log
             never holds a raw parameter, a pseudonym of a parameter
             or a literal (normalized statement; parameters as hidden markers, decision
             0004-24); an agent lists only its own log entries; a person with view lists own
             entries and those of their own agents, a person with manage all; error messages are
             sanitized for people without manage.
Precondition: the sqlparser dependency is approved (open question 9).
After:       D-1, Q-0
Out of scope: agents (Q-2, Q-3).
```

```markdown
ID:          Q-2
Goal:        The privacy filter for Postgres: the lens views from database_columns (nosecret,
             masked), the role kind per caller and grant, class_read_required for writes,
             markers from result column classes, sanitized errors for agents.
May change:  crates/ruum-core/src/privacy/postgres.rs (the MaskingRules implementation),
             crates/ruum-core/src/databases/query/postgres_mask.rs,
             crates/ruum-dbengine/src/postgres/lens.rs.
Done when:   T-2 passes on Postgres; classes_changed rebuilds the lens views in one transaction
             with the catalog sync; the lens of a masked or schema_only branch uses the branch's
             key (decision 0004-25).
After:       Q-1, K-1, E-2
```

```markdown
ID:          Q-3
Goal:        The privacy filter for SQLite: the TEMP lens views and the authorizer rules from
             database_columns, class_read_required for writes, markers from result column
             classes (column origin; unknown -> personal; no column -> normal).
May change:  crates/ruum-core/src/databases/query/sqlite_mask.rs,
             crates/ruum-core/src/privacy/sqlite.rs (the MaskingRules implementation),
             crates/ruum-dbengine/src/sqlite/lens.rs (the TEMP lens views and the
             restricted-column rules of the authorizer, on E-3's hook; decision 0004-31).
Done when:   T-2 passes on SQLite; the lens of a masked or schema_only branch uses the branch's
             key.
After:       Q-1, K-1, E-3
```

```markdown
ID:          K-1
Goal:        Column classes: catalog sync, listDatabaseColumns, suggestColumnClasses,
             confirmColumnClasses (new manifest version), the rule-based suggester.
May change:  crates/ruum-core/src/databases/classes/, crates/ruum-api/src/routes/databases/classes.rs.
Done when:   T-1 cases pass; unconfirmed = personal; only a person confirms; confirmation is a new
             manifest version and an audit entry; the catalog sync keeps classes by column
             identity and resets renamed, retyped, swapped and re-added columns (T-2 rename
             cases); a reset writes a new manifest version without the key; every change of
             classes calls MaskingRules::classes_changed (Q-0; an in-memory implementation
             records the calls in the tests; the lens views themselves are Q-2 and Q-3);
             database_columns, getDatabaseSchema and the lens views cover the schema `public`
             (SQLite `main`) only, never the Ruum schemas or `extensions`; the sync transaction
             overwrites or deletes a dropped column's row when a column of the same name comes
             back (drop and add, rename onto the name), so the primary key does not collide
             (decision 0004-35).
After:       D-1, Q-0, C-2
```

```markdown
ID:          A-1
Goal:        Grants: createDatabaseAccessGrant, listDatabaseAccessGrants, revokeDatabaseAccessGrant;
             the tuples, the sweeper, the restore point before write_live.
May change:  crates/ruum-core/src/databases/access/grants.rs, routes/databases/access.rs.
Done when:   only the agent's user grants; an expired or revoked grant gives nothing even before the
             sweeper runs (mutation rule); the agent can give a grant back; the hint of
             write_grant_required is the web URL of the prefilled grant prompt (IDs, kind and
             minutes only).
After:       D-1, C-2
```

```markdown
ID:          A-2
Goal:        Approvals: destructive statements (detection, 202 with a pending approval), migration
             requests with planned_columns and classes, grant requests
             (requestDatabaseAccessGrant), decideDatabaseApproval, listPendingDatabaseApprovals,
             one use within 15 minutes.
May change:  crates/ruum-core/src/databases/access/approvals.rs, routes/databases/access.rs.
Done when:   DROP, TRUNCATE, DELETE/UPDATE without WHERE from an agent never run without an
             approval; the write_live check comes first (403 before 202) and an approval never
             replaces it; only the agent's user or a person with manage, with the needed right,
             decides; a grant request only the agent's user, and approving it writes the grant
             (A-1's rules and limits) in the same transaction; planned_columns come from parsing
             `up` (nothing runs before the approval); classes in a decision need manage and take
             effect only with the migration (D-2); estimated_rows never comes from a test run of
             the statement; another statement or other parameters do not match; a used approval
             is spent; params of an approval are hidden markers, never pseudonyms (decision
             0004-24); every request has a review_url (IDs only); listPendingDatabaseApprovals
             lists what the caller may decide (agents: their own) and leaves out databases the
             caller cannot see; two racing grant requests of one agent for one database and kind
             make one row (a row lock until 0004 gets the partial unique index, decision
             0004-35); a pending approval expires after 24 hours and an approved one 15 minutes
             after the decision if unused (an approved grant request is final); an agent's
             `data_classes` in a migration are stored as suggestions, and only a person with
             `manage` confirms classes in a decision (0004-40 h).
Precondition: the sqlparser dependency is approved (open question 9).
After:       Q-1, A-1, K-1, C-2
```

```markdown
ID:          M-1
Goal:        The 20 MCP tools and `ruum db ...` in the CLI.
May change:  crates/ruum-mcp/, crates/ruum-cli/.
Done when:   tools_contract passes; a tool appears in tools/list exactly when its operation is no
             longer planned; an MCP smoke test creates a database, queries it and sees
             markers; the agent benchmark gets 3 database tasks (bench/agent/).
After:       D-1, Q-1 (more tools as their operations land)
```

```markdown
ID:          U-1
Goal:        UX package for databases (journeys, IA, wireframes, copy), section 18.4.1.
May change:  docs/ux/databases/ (new).
Done when:   the UX review panel accepts; contract needs found are listed as C-cards. (The
             package is on f/step2spec; C-15 to C-19 are in the contract, decision 0004-20;
             C-20 to C-23 wait.)
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

1. **Specify** (now): C-1 and C-2 review (one branch); S-1 is done (it ran on the build
   server's Postgres 17 before I-1, decision 0004-34); in parallel T-1, T-2, U-1, I-1, I-3.
2. **Foundations** in parallel: P-1, E-1, Q-0; then P-2, Q-4, E-2 (after I-1, S-1), E-3 (after
   I-3), I-2.
3. **Control plane**: D-1 (memory first), then K-1, Q-1, A-1 in parallel; P-3.
4. **Privacy and agent rules**: Q-2, Q-3, A-2; then D-2, D-3.
5. **Backups**: E-4, D-4.
6. **Clients**: M-1 and U-2 as their operations land.
7. **Harden**: T-3, the security reviewers (two lenses: tenancy and privacy), the agent benchmark
   with database tasks, synthetic users on the database pages.

Migrations go through the queue: 0004 is the only migration of the step; a change to it during
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
   (less linkable)? Recommended: per tenant, as for telemetry (section 9.6), with one exception
   that is decided (0004-25): a branch with `masked` or `schema_only` data uses a key derived
   for the branch, because agents write there without a person. Either way the key stays out of
   reach of every role that runs untrusted SQL, and the "write a guess, read its pseudonym"
   online test on real data stays an accepted risk that needs a `write_live` grant (0004-4).
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
   retention of the plan, monthly partitions (not in 0004 yet: one plain table), and no row for
   repeated identical reads within one second (a counter instead). Statements are stored
   normalized (0004-9).
9. **`sqlparser` as a new dependency** (statement count, destructive statement detection, column
   origin, the normalized query log, planned_columns). Recommended: accept (Apache 2.0, widely
   used). Its approval is a precondition of Q-1 and A-2.
10. **Branch of a branch.** Decided for step 2 (0004-33): no, `409 conflict` (branches are flat
    children of a live database), with a T-1 case. Branches count toward the plan's database
    count (0004-23 d).
11. **Typed node copy.** Decided in 0004-7 (provisional): `full` data only for a person with
    `manage` on the source; everyone else (agents, viewers, editors) gets `masked` data, and
    the UI says so.
12. **PostgreSQL Anonymizer.** Design 8.5 names it for the masking inside Postgres. 0004-4
    replaces it with Ruum's lens views (same intent, no readable masking rules or salt). A
    person should confirm, and the wording of design 8.5 should follow in a design change.
