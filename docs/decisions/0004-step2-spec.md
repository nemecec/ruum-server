# 0004: Step 2 specification decisions (first panel review)

Status: `provisional`, decided by the agent panel on 2026-11-06 (design section 18.9: one
architect agent drafted the step 2 contracts on `f/step2spec`, two reviewer agents asked for
changes, and this record applies those changes). A person can veto or change any part. Without
a veto, the decisions stand.

Scope: the step 2 contracts (`contracts/providers/`, `contracts/manifests/database.schema.json`,
the `databases` tag and four step 1 descriptions of `contracts/openapi.yaml`,
`contracts/authz/schema.zed`, `contracts/mcp/tools.yaml`, `migrations/0002_databases.sql`) and
`docs/STEP2.md`. Where the reviewers did not agree, or where a fix had more than one form, the
safer option is taken and the text says so.

Reviewer 1 (contract consistency, tenant isolation, the 8.5 privacy filter) gave 11 MUST items
and one SHOULD item. Of reviewer 2, the orchestrator relayed only the first item (the role
model); it asks for the same change as reviewer 1, item 1. The rest of reviewer 2's text did not
reach this revision; see "Open" at the end.

## 1. Postgres sessions: untrusted SQL never runs as the admin role

**Decision.** Untrusted SQL (every statement of the query API and every migration) runs only in
a session whose `session_user` is the per-database role of the kind the request needs. These
roles have `LOGIN` with a short-lived random password (`VALID UNTIL` a few minutes ahead), set
by the platform when it opens connections and never stored. Connections are pooled per
(database, kind) and closed before the password ends. Each role is a member of no other role,
has no ADMIN option, is `NOINHERIT`, and has `CONNECT` on its own database only. `ruum_dbhost`
(`CREATEDB`, `CREATEROLE`) runs only platform DDL and is never granted the per-database roles.
The query layer also refuses role and session statements (`422 statement_not_allowed`), as a
first line.

**Reasons.** With `SET ROLE` from `ruum_dbhost`, a statement could run `RESET ROLE`, `SET ROLE
<owner>` or `set_config('role', ...)` (also in a `DO` block) and reach every role of every
database, with raw secrets and `CREATEROLE`. A session that logs in as the role itself can only
return to itself. Short-lived passwords keep design 8.4 ("no static passwords").

**Changes.** `database_roles` (comment, kinds, `can_login`), STEP2 "The engine security model",
area 2, infrastructure item 2, cards I-1 and E-2 (done when: `RESET ROLE`, `SET ROLE` and
`set_config('role')` cannot reach another role, also in a `DO` block). T-2 role cases.

## 2. SQLite hardening

**Decision.** One directory per tenant and one file per database, with random names. On every
connection: an authorizer that denies `ATTACH`, `DETACH`, every `PRAGMA` outside a read-only
allow-list, `load_extension`, `readfile`, `writefile`, direct calls of `ruum_pseudonym`, and
reads of restricted columns outside a lens view; `SQLITE_LIMIT_ATTACHED = 0`;
`SQLITE_DBCONFIG_DEFENSIVE`; load extension off; `trusted_schema = OFF`. `VACUUM` (also `VACUUM
INTO`) is refused.

**Reasons.** All files share `RUUM_SQLITE_DIR`: `ATTACH` of another tenant's file is a
cross-tenant read, and `VACUUM INTO` writes any file. Per-tenant directories with random names
are a second wall behind the authorizer.

**Changes.** STEP2 "The engine security model" (SQLite), infrastructure item 4, card E-3 (with
an isolation case: an agent of tenant A attaches tenant B's file), T-2 file cases,
`databases.engine_name` comment, `queryDatabase` description.

## 3. The privacy filter masks every read, not only the output

**Decision.** For a principal without the right to see a class (agents without `read_personal`
for `personal`; agents always and people without `manage` for `secret`), every read of such a
column sees the masked value, in every clause: select list, `WHERE`, `JOIN`, `ORDER BY`, `GROUP
BY`, `HAVING`, aggregates, subqueries, `RETURNING`, `INSERT ... SELECT`. Both engines have the
same semantics. Postgres: the role of the caller is limited to a lens schema of views
(`ruum_nosecret` or `ruum_masked`), with no right on the base tables and no `USAGE` on other
schemas. SQLite: per-connection `TEMP` lens views and an authorizer that denies reads of
restricted `main` columns outside them. Open question 2 of STEP2 (people and secrets) is decided
yes, with the same semantics.

**Reasons.** Output masking alone lets `SELECT count(*) FROM users WHERE email LIKE 'a%'` read a
value bit by bit. A lens in the engine closes every clause at once; the parser alone cannot.

**Changes.** `queryDatabase`, `ResultColumn`, `DataClass` text, STEP2 areas 2 and 4, cards Q-2,
Q-3, T-2 oracle cases (`WHERE`, `ORDER BY`, `GROUP BY`, `LIKE`, `min`, `length`, joins).

## 4. One pseudonym function; the key stays out of reach; no PostgreSQL Anonymizer

**Decision.**

- `pseudonym(v) = 'p_' || first 12 hex of HMAC-SHA-256(tenant_key, canonical_text(v))`, one
  definition for both engines and for the platform (Rust). `canonical_text` has a fixed format
  that no session setting changes.
- Postgres: `ruum_mask.pseudonym(text)` is `SECURITY DEFINER`, owned by the `NOLOGIN` role
  `mask`, and reads the padded keys from `ruum_mask.key` (owner `mask`, no grants). It uses the
  built-in `sha256()`, so no extension is needed. The lens views (owner `mask`) call it. Agent
  roles have `EXECUTE` on it but no `USAGE` on `ruum_mask`, so they cannot call it by name and
  never read the key. SQLite: `ruum_pseudonym` is registered per connection in Rust; the key
  never enters the file; direct calls are denied by the authorizer.
- Masked `personal` columns are `text` holding the pseudonym, in lens views and in masked
  branches and copies (a non-text column is retyped to `text`; its check constraints are
  dropped in the copy). `ResultColumn.type` says `text`.
- Expressions that read no column (constants, parameters) are `normal` and never pseudonymized,
  so `SELECT 'guess'` does not give the pseudonym of a guess.
- PostgreSQL Anonymizer is not used in step 2. Its masking rules are security labels that every
  role can read in `pg_seclabels`, and a salt in a setting is readable too. The lens views keep
  the intent of design 8.5 ("masks inside the database, bound to the agent's database role").
  This changes the mechanism that design 8.5 names; a person should confirm it and the design
  text should follow (STEP2 open question 12).
- Collisions: 48 bits; about 0.2 % for 10^6 distinct values of a tenant and about 16 % for
  10^7. Accepted: a collision makes two values look equal; it never shows a real value.

**Accepted risk.** An agent that may write can insert a guessed value and read its pseudonym,
then compare: an online test of one guess per write, logged and rate limited, needing a write
right. A `DO` block on a draft branch that gets around the missing schema `USAGE` gives the
same online test. Neither gives the key or an offline dictionary attack. Relates to STEP2 open
question 4 (pseudonym scope).

**Changes.** STEP2 "The engine security model" items 3 to 5 and "Accepted risk", infrastructure
item 1 (no `anon`), cards I-1, E-2, Q-2, D-3, T-2 pseudonym cases; `DataCopy`, `ResultColumn`,
`createDatabaseBranch`, provider `DataCopy`.

## 5. Agent writes and schema changes cannot relabel or copy restricted data

**Decision.**

- (a) A write statement (`INSERT`, `UPDATE`, `DELETE`, `MERGE`, also in a `WITH`) that reads or
  writes a column the caller may not see fails with `403 class_read_required` (new error code):
  in the target list, `SET`, `WHERE`, a subquery, `RETURNING` or the `SELECT` of an `INSERT`.
  The engine enforces it a second time (Postgres: those columns are computed in the lens views,
  so they are not writable, and reads see masked values; SQLite: the authorizer). Safer option
  taken: refuse, instead of letting writes see masked values, so a write never overwrites real
  values with pseudonyms either.
- (b) Classes follow column identity. Postgres: `rel_oid`, `attnum`, `type_oid` in
  `database_columns`. SQLite: the platform parses every DDL statement it runs and resets every
  column of a table that a statement renamed, rebuilt or dropped and created again. A rename, a
  retype, a table swap or rebuild, and a drop followed by an add of the same name clear the
  confirmation (`unconfirmed_reason`) and remove the key from `data_classes` (a new manifest
  version by the system).
- `draft_branch` is written only for branches with `masked` or `schema_only` data. On a `full`
  branch an agent needs `write_live`, as on live.
- Beyond the review (safer option): an agent's migration on real data (live, or a `full`
  branch) always waits for a person's approval (reason `migration`, `202`, then the same name and
  `up` with `approval_id`), on top of `manage` and `write_live`. A migration runs as `owner` with
  every right on the base tables, so the statement rules above cannot hold inside it.

**Changes.** `queryDatabase`, `applyDatabaseMigration` (`202`, `approval_id`),
`createDatabaseBranch`, `ErrorCode`, `DatabaseApproval.reasons` (`migration` instead of
`alter_role`, which role statements no longer need: they are refused), `database_columns`
(identity, `unconfirmed_reason`), `database_approvals` (`subject`, `migration_name`),
`schema.zed` (`draft_branch` comment, a `full` branch case in `schema.test.yaml`), manifest
`data_classes` text, cards K-1, D-2, D-3, A-2, T-2 write and rename cases.

## 6. Step 1 operations on typed nodes

**Decision.** `putContent` and `createUpload` with a `new_version` target on a typed node fail
with `409 conflict`, also for an `edit` share link. `restoreVersion` of a database needs
`manage`, validates the old manifest against today's rules, and refuses a changed
`data_classes` from an agent (`422 validation_failed`). A person who restores other
`data_classes` confirms them (audit entry); keys that name no live column are dropped.

**Reasons.** The step 1 contract allowed content writes on any node (`nodes/memory.rs` allows
them on typed nodes), and `restoreVersion` needed only `edit`: both bypassed the manifest rules.
The text is additive and lives in the step 1 descriptions, where clients read it.

**Changes.** The descriptions of `putContent`, `createUpload` and `restoreVersion`; provider
README "Change"; cards P-3 (code fix in both stores) and T-1.

## 7. Copying a database never raises the copier's rights

**Decision.** `copyNode` of a database copies `full` data only when the caller is a person with
`manage` on the source. Everyone else (agents, people with `view` or `edit`) gets `masked`
data. The platform sets `source.data`; the provider follows it. STEP2 open question 11 is
decided.

**Reasons.** `copyNode` needs only `view` on the source. A viewer who copies into a folder of
their own becomes manager of the copy and would read its secrets.

**Changes.** `copyNode` description, provider README hook table, `deliverHook` table and
the copy variant of `Source`, card P-3 done-when, T-1 cases.

## 8. Engine error text is sanitized

**Decision.** For agents and for people without `manage`, `query_failed` and migration errors
carry the SQLSTATE, a fixed text per SQLSTATE and object names only, never `DETAIL`, `HINT` or
`CONTEXT`. `DatabaseMigration.error` is sanitized for the same callers. The status `message`
and provider status `message`/`detail` never hold row values, for anyone.

**Reasons.** Postgres `DETAIL` carries row values (`Key (email)=(...)`, `Failing row contains
(...)`), and `listDatabaseMigrations` and the status need only `view`.

**Changes.** `QueryFailed`, `queryDatabase`, `listDatabaseMigrations`, `applyDatabaseMigration`,
`DatabaseMigration.error`, `DatabaseStatus.message`, provider `ProviderStatus`, README rules,
cards Q-1, D-2, E-2, T-2 leak cases.

## 9. The query log holds no literals; agents read only their own entries

**Decision.** Both reviewer options are taken (safer): the log stores the normalized statement
(every literal replaced by `$L1`, `$L2`, ... after a parse; `<unparsed statement>` when it does
not parse) and the parameters as pseudonyms, and an agent with `manage` sees only its own
entries.

**Changes.** `listDatabaseQueries`, `DatabaseQueryLogEntry.statement`, `database_queries`
comment (also: the table is one plain table; the partition remark was wrong), the MCP tool
text, cards Q-1 and T-2.

## 10. Guests and resources that count against the host plan (closes 0002-2)

**Decision.** A new optional `TenantPolicy` flag `guests_create_resources` (default false).
While it is false, a guest or an agent of a guest gets `403 permission_denied` (hint
`guests_create_resources`) on `createDatabase`, `createDatabaseBranch`, `restoreDatabase` with
`target: new_node`, and `copyNode` of a typed node.

**Changes.** `TenantPolicy`, `createDatabase` (the rule in one place, referenced from the
others), `createDatabaseBranch`, `restoreDatabase`, `copyNode`, provider README "Guests and the
host plan", cards D-1, D-3, P-3, T-1 matrix cases.

## 11. CI stays green when the contract lands

**Decision.** The commit that adds the four `x-ruum-user-only` marks also adds the four names to
`USER_ONLY_OPERATIONS` in `crates/ruum-testkit/src/matrix.rs` (`confirmColumnClasses`,
`restoreDatabase`, `createDatabaseAccessGrant`, `decideDatabaseApproval`). C-1 may change that
list and the error copy of the new codes in `web/src/api/errors.ts` (its type needs an entry per
`ErrorCode`).

**Changes.** `matrix.rs`, card C-1.

## 12. Pinned for T-1 (SHOULD)

- (a) Order of checks for an agent's statement: the user's right and the scope, then
  `write_live` (`403 write_grant_required`), then the statement rules (`422
  statement_not_allowed`, `403 class_read_required`), then the approval of a destructive
  statement (`202`). An approval never replaces `write_live`; a re-sent approved statement runs
  every check again.
- (b) Who decides an approval: the user the agent acts for, or a person with `manage` on the
  database; in both cases with the right the statement needs (`edit` for data, `manage` for
  schema and migrations). Safer option: not "any person with the right", so an editor who is a
  stranger to the agent cannot approve its `DELETE`.
- (c) Names: `query_read`, `query_write`, `change_schema` and the new `read_secret` (=
  `manage`, never true for an agent) are permissions in `schema.zed`; `write_live` maps to
  `agent_write_live` and `read_personal` to `agent_read_personal`. Stated in
  `DatabasePermissions`, `CheckRequest.permission`, the provider README and `schema.zed`.
- (d) The OpenAPI `DatabaseManifest` now has the same keywords, enums and limits as
  `database.schema.json` (`additionalProperties: false`, the `extensions` enum and `maxItems`,
  `propertyNames`, `maxLength`, the engine/version `if`/`then`). Card D-1 owns the contract test
  that keeps the two equal; only `readOnly` and descriptions may differ.

## 13. Non-blocking notes, applied

- Neutral database and role names (`d_` / `r_` + 16 random hex), because every role can list
  `pg_database` and `pg_roles`.
- Side channels in T-2: `pg_stats` (no `SELECT` on base tables, so no stats), `EXPLAIN`;
  `pg_stat_statements` is not installed. Timing differences are accepted.
- `database_queries` is not partitioned; the comment says so.
- Pseudonym collisions are stated (item 4).
- `estimated_rows` comes from `EXPLAIN` or a `count(*)` in a read-only transaction, never from a
  test run of `DROP` or `TRUNCATE` (which takes `ACCESS EXCLUSIVE` locks on live).

## Open

- Reviewer 2's items after the first one did not reach this revision (the relayed text ends in
  the middle of its first item). The panel should check that its remaining items are covered.
- Design 8.5 names PostgreSQL Anonymizer; item 4 replaces it. A person confirms, and the design
  text follows in a design change.
