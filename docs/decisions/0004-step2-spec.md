# 0004: Step 2 specification decisions (panel reviews)

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
and one SHOULD item; items 1 to 13 below apply them. Reviewer 2 (later steps and swarm
buildability) gave 12 required changes and 5 notes. The first revision saw only reviewer 2's
first item (the same as reviewer 1, item 1). The second revision read both reviews in full and
applied every reviewer 2 item that the branch did not satisfy yet (items 14 to 19 and 23), the UX
contract needs C-15 to C-19 (item 20), and the architect decision on planned operations (items 21
and 22). The third revision applies the re-review of the second revision (reviewer 1: three
required changes; reviewer 2: six required changes and notes): items 24 to 33. Earlier items that
these change say so in place. `ruum-server-wt/panel/step2-spec-v2-changes.md` maps each review
item to its change.

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
  so `SELECT 'guess'` does not give the pseudonym of a guess. The query log and approvals store
  parameters as hidden markers, never as pseudonyms (item 24).
- PostgreSQL Anonymizer is not used in step 2. Its masking rules are security labels that every
  role can read in `pg_seclabels`, and a salt in a setting is readable too. The lens views keep
  the intent of design 8.5 ("masks inside the database, bound to the agent's database role").
  This changes the mechanism that design 8.5 names; a person should confirm it and the design
  text should follow (STEP2 open question 12).
- Collisions: 48 bits; about 0.2 % for 10^6 distinct values of a tenant and about 16 % for
  10^7. Accepted: a collision makes two values look equal; it never shows a real value.

**Accepted risk** (narrowed by items 24 and 25). An agent with a `write_live` grant on real data
can insert a guessed value and read its pseudonym, then compare: an online test of one guess per
write, logged and rate limited, needing a grant a person gave. On a draft branch (`masked` or
`schema_only` data), where an agent writes without a person, the lens uses the branch's own key,
so rows, migrations and `DO` blocks there give pseudonyms that match nothing outside the branch.
None of this gives the key or an offline dictionary attack. Relates to STEP2 open question 4
(pseudonym scope).

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
not parse) and an agent with `manage` sees only its own entries. Parameters were stored as
pseudonyms; item 24 replaces that with hidden markers.

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

## 14. Pooled sessions and app bindings (reviewer 2, items 1 and 3)

**Decision.** Item 1 stands (LOGIN roles per database and kind). In addition: every request runs
in its own transaction and sets options with `SET LOCAL` only; `SET`, `RESET` and
`set_config(..., false)` of any session option are refused (`statement_not_allowed`); a
connection returns to the pool only after `DISCARD ALL`, and a connection whose transaction did
not end cleanly is closed. `dbhost_check.sh` (I-1) tests from sessions that log in as the role,
not from `ruum_dbhost` with `SET ROLE`.

Hiding `secret` values from people without `manage` stays in the engine (the `nosecret` lens),
because reviewer 1 item 3 needs every clause masked and a query-layer filter cannot do that. So
step 3 apps do not use the people's kinds: an app binding gets kinds of its own, `app_read` and
`app_write` (lens `base`, real values and secrets, one pair per binding), added in step 3 as new
values of `database_roles.kind`, with the binding in `database_roles.binding_id` (item 32). The
permission table has rows for app bindings (step 3) and share links (`403`), so the caller
resolution of Q-1 does not change in step 3.

**Reasons.** Session options set with `set_config(..., false)` outlive a request on a pooled
connection. Reviewer 2's alternative (secrets hidden from people in the query layer only) would
reopen reviewer 1's oracle (`WHERE token LIKE 'a%'`) for viewers.

**Changes.** `database_roles` comment, STEP2 "The engine security model" (Postgres 1 and the
kinds table), the permission table, `queryDatabase` ("Statements that never run"), cards I-1,
E-2, Q-1.

## 15. Spike S-1 before the role kinds freeze (reviewer 2, item 2)

**Decision.** New card S-1 (after I-1; C-1's freeze waits for it). PostgreSQL Anonymizer is gone
(item 4), so the spike proves what replaces it: LOGIN roles cannot reach other roles; masked
roles read pseudonyms in every clause through the lens views and cannot read base tables, the key
or `pg_stats` values; `ruum_mask.pseudonym` runs from a view but not by name without schema
`USAGE`; `masked_write` writes plain columns through the view, cannot write masked columns, and
a `WHERE` on a masked column compares the pseudonym (so `rows_affected` tells nothing new);
`SET LOCAL` and `DISCARD ALL` leave nothing on a pooled connection; the SQL and Rust pseudonyms
agree. If a point fails, C-1 changes the kinds or the lens before the freeze.

**Reasons.** The lens design rests on Postgres behaviour that no test has shown yet (`EXECUTE`
without schema `USAGE` through a view, writes through views with computed columns). Freezing
`database_roles` first would make a failure reopen 0002 and the contract.

**Changes.** STEP2 card S-1, C-1 "After" and "Done when", E-2 "After", order.

## 16. The tenant pseudonym key (reviewer 2, item 9)

**Decision.** One random 32-byte key per tenant in the new table `tenant_keys` (0002), stored
only wrapped (AES-256-GCM) by a key-encryption key: an OpenBao transit key in production, a
local KEK file on the laptop (`RUUM_LOCAL_KEK_FILE`, kept and removed together with the platform
database volume), because dev OpenBao is in memory. The key is made on first use (the first
database of the tenant), under the primary key, so two racing creates keep one key. A tenant
export carries it re-wrapped for the target cell, so pseudonyms stay the same after a move
(design 14). It is never rotated in step 2 (masked branches and copies keep matching their
sources); a rotation would be a new `key_version` and is not specified. New card Q-4 owns it.
Items 25 and 30 add the key of a draft branch and the path of a key into the engine.

**Changes.** `tenant_keys` in 0002 (with RLS), STEP2 infrastructure item 5, area 4, card Q-4.

## 17. Branches keep their metadata; masking by type; no `at` (reviewer 2, items 4 and 5)

**Decision.**

- A branch gets copies of the source's `database_columns` rows (confirmed classes kept) and of
  its applied `database_migrations` rows as of `branch_at` (their column identity comes from the
  branch's catalog, item 28). Classes stay as they are, so in a
  masked branch an agent without `read_personal` reads a pseudonym of the pseudonym: intended
  (consistent inside the branch, never the real value; reviewer 2 note e).
- Masking by type: a `personal` column becomes `text` with the pseudonym; a `secret` column
  keeps its type and holds `NULL`, with its `NOT NULL` dropped; `CHECK` constraints that read a
  masked column are dropped; keys, `UNIQUE`, indexes and foreign keys on masked columns are
  built again after the copy (both sides of a foreign key hold the pseudonym of the same value),
  and one that fails because of a pseudonym collision is left out and named in
  `status.message` (names only); triggers are disabled during the copy.
- `CreateDatabaseBranchRequest.at` and the provider `Source` branch `at` are removed for step 2:
  a branch is always of the current state. A branch of an earlier time is a restore into a new
  node followed by a branch, so D-3 needs no PITR (E-4).

**Changes.** `createDatabaseBranch`, `CreateDatabaseBranchRequest`, provider `Source`,
`databases.branch_at` comment, card D-3.

## 18. Children of typed nodes (reviewer 2, item 6)

**Decision.** `TypeDescriptor.child_types` is the only list of children a typed node may have,
made only by the type's own operations, and the platform enforces it for every type:
`createNode`, `createUpload` (`new_file`), `moveNode` and `copyNode` into a typed node give
`409 conflict`; a child that `child_types` lists cannot move away from its parent (`409`);
`copyNode` of a database copies no branch; trash and purge take the children along. A database
has `child_types: [database]`.

**Changes.** The descriptions of `createNode`, `createUpload`, `moveNode`, `copyNode` and
`trashNode` (additive), `TypeDescriptor.child_types`, provider README "Children of a typed node",
cards P-3 and T-1.

## 19. Ownership and order for the swarm (reviewer 2, items 7, 8, 10, 11 and 12)

**Decision.**

- Item 7 was already applied (decision 11); C-1 may now change everything needed to keep main
  green, and C-1 and C-2 land as one branch.
- New card Q-0 makes `crates/ruum-core/src/privacy/mod.rs`: the marker types, the
  `Pseudonymizer` trait and the `MaskingRules` hook (`classes_changed`). K-1 calls the hook; Q-2
  and Q-3 implement it. This removes the K-1 to Q-2 inversion.
- D-1 makes `routes/databases/mod.rs` with `501` stubs for every handler of the area and one
  submodule per later card (`migrations`, `branches`, `backups`), and the same split in
  `ruum-core/src/databases/`. D-2, D-3 and D-4 change only their submodule.
- C-2 is in "After" of D-1, D-2, D-3, D-4, K-1, A-1 and A-2. The `sqlparser` approval (open
  question 9) is a precondition of Q-1 and A-2.
- Item 12 (SQLite sandbox) was mostly applied (decision 2); E-3 now also asserts that an
  authorizer is set on every connection and that every `PRAGMA` that writes is denied, also in
  read mode.

**Changes.** STEP2 shared files, area 3 and 4 "Owns", cards C-1, C-2, Q-0, D-1 to D-4, K-1, A-1,
A-2, Q-1, Q-2, E-3, order.

## 20. UX contract needs C-15 to C-19

**Decision.** All five are taken; no review argues against one.

- **C-15, classes in the migration request.** `DatabaseApproval` gets `subject`
  (`statement`, `migration`, `grant`), `migration_name` and `planned_columns`;
  `decideDatabaseApproval` takes optional `classes`. Changed from the UX proposal (safer):
  `planned_columns` come from parsing `up`, not from a dry run, because a dry run of a migration
  on live takes `ACCESS EXCLUSIVE` locks (the same reason as decision 13 for `estimated_rows`).
  The classes take effect in the same transaction as the approved migration, never before.
- **C-16, web links for the agent's user.** `DatabaseApproval.review_url`; the `hint` of
  `write_grant_required`, `class_read_required` and `user_required` (on `restoreDatabase` and
  `confirmColumnClasses`) is the web URL of the right page. URLs hold IDs (and kind and minutes)
  only: never a reason, a statement or a value (privacy; prompt injection, UX DB-8).
- **C-17, an agent asks for a grant.** New operation `requestDatabaseAccessGrant` (MCP
  `request_database_access_grant`): a pending `DatabaseApproval` with `subject: grant`; only the
  agent's user decides, and the approval writes the grant with the rules and limits of
  `createDatabaseAccessGrant`. A person calling it gets `403` (they give grants directly).
- **C-18, "Waiting for you" across the tenant.** New operation `listPendingDatabaseApprovals`
  (`GET /v1/database-approvals`): the requests the person may decide, or an agent's own.
- **C-19, the agent's user reads what the agent ran.** `listDatabaseQueries` needs `view`; a
  person with `view` sees their own entries and those of their own agents, `manage` all, an
  agent its own. Statements are normalized (decision 9), so no literal reaches them.

Also from the UX notes: the manifest's `extensions` text no longer names Anonymizer, and the MCP
tool `revoke_database_access_grant` is renamed `remove_database_access_grant` (step 1 uses
`remove` for grants; the operation ID stays). C-20 to C-23 wait (not asked for in this round).

**Changes.** `openapi.yaml` (`DatabaseApproval`, `ApprovalSubject`, `PlannedColumn`,
`GrantRequestDetails`, `DecideDatabaseApprovalRequest.classes`,
`RequestDatabaseAccessGrantRequest`, the two new operations, `listDatabaseQueries`, hints),
`database_approvals` (subject `grant`, grant fields, `planned_columns`), `tools.yaml`, the
permission table, cards A-2, D-2, Q-1, T-1, U-1.

## 21. Planned operations (architect decision)

**Decision.** Operations that are specified but not built carry `x-ruum-status: planned` in
`contracts/openapi.yaml`. `ruum-testkit` treats a planned operation like this, also with
`RUUM_REQUIRE_IMPLEMENTED=1`: it must have a standard request and registered isolation and
permission cases (the "every operation has a case" rule stays); its route may be missing or
answer `501`, and those checks are reported as `planned`, not failed; any data leak in an answer
still fails. A build task removes the mark when it builds the operation. A test keeps the list of
planned operations (`matrix::PLANNED_OPERATIONS`) equal to the contract marks. All 24 step 2
operations are planned.

Details of the implementation:

- The scanner of `contract.rs` reads the mark (only the value `planned` is valid; a mark outside
  an operation fails the scan).
- `Outcome::Planned`: it never fails a suite; a failure of any probe of the operation wins over
  it. The leak checks run first: the isolation suite searches every answer for tenant A's
  markers, and the permission suite now also searches `501` and unrouted answers for nodes the
  principal cannot see (stricter than before, for pending operations too).
- `every_operation_is_routed` skips planned operations, and fails when a planned operation
  answers anything but a missing route or `501`, so a built operation cannot keep its mark.
- The fixture has a database node `<prefix>-garden` in `Projects` (placeholder IDs until D-1,
  A-1 and A-2 seed it), so the standard requests address tenant A's resources and the leak checks
  know their markers.
- The same rule for event types: the step 2 types are in `$defs/planned_types` of
  `contracts/events/journal-entry.schema.json`; the envelope test of `ruum-core` keeps
  `EventType` equal to the enum minus that list, and checks that every type has a payload schema.

**Reasons.** The contract of a step lands before its code (design 18.1), and main must stay green
with `RUUM_REQUIRE_IMPLEMENTED=1` (CI runs the security suites strictly). Without a mark, a
contract-first branch either breaks CI or has to weaken the strict mode for every operation.

**Changes.** `openapi.yaml` (the marks, a "Planned operations" section in `info`),
`crates/ruum-testkit` (`contract.rs`, `report.rs`, `matrix.rs`, `fixtures.rs`, `requests.rs`,
`isolation.rs`, `permissions.rs`, `tests/contract.rs`, `permission_matrix.md`),
`crates/ruum-core/src/journal/mod.rs` (test), the event schemas, `docs/TESTING.md`, STEP2
"Planned operations".

## 22. MCP tools of planned operations

**Decision.** Planned operations keep their tool entries in `contracts/mcp/tools.yaml` (so each
tool is designed with its contract), but the server does not offer them: `gen-tools.mjs` writes
`planned: true` for a tool whose operation is planned, and the MCP catalog leaves such tools out
of `tools/list` and answers a call with "unknown tool". When a build task removes the mark and
runs `gen-tools.mjs`, the tool appears.

**Reasons.** An offered tool whose route is missing would fail for every agent and waste its
turns; listing a tool as "planned" in `tools/list` has no place in the MCP protocol. Keeping the
entries avoids a second contract change per operation.

**Changes.** `tools.yaml` header, `gen-tools.mjs`, `crates/ruum-mcp/src/catalog.rs` (and a test),
`tests/mcp_server.rs` (the offered list), `tools.json`.

## 23. Reviewer 2 notes, recorded

- (a) `Source` has no `import` kind and `provider_hook` has no `tenant_imported`. Moving a
  tenant between cells needs both; additive for the API, new work for every provider. To add
  before step 6 (external providers). The pseudonym key is already exportable (item 16).
- (b) Grants are per agent with a 24 h cap. Standing grants per workspace in `project.yaml`
  (design 8.5, 11.1) come in step 5 and need a nullable `agent_id` and a workspace subject.
- (c) `database_queries` is one plain table (decision 13); partitioning stays open question 8.
  A partitioned table needs `(tenant_id, at, id)` as the key, so the change is made in 0002 in
  place if the panel wants it before step 2 ships.
- (d) Branches count toward the plan's database count (they are databases, and parallel preview
  sessions must not get around the limit).
- (e) A masked branch is masked again for agents (a pseudonym of a pseudonym): intended (item
  17).

## 24. Parameters are never stored as pseudonyms (re-review: reviewer 1, item 1)

**Decision.** The query log (`database_queries.params`, `DatabaseQueryLogEntry.params`) and
approvals (`database_approvals.params`, `DatabaseApproval.params`) store each parameter as a
hidden-value marker (`{"$ruum": "hidden", "class": "personal"}`, JSON `null` for a `NULL`), never
as a raw value and never as a pseudonym. Of the reviewer's two options, the safer one is taken:
hidden markers (option b) leak nothing, while a separate log key (option a) still gives an agent
a pseudonym of any value under some key. The approval still binds the values: `statement_hash`
covers the canonical parameters, and the agent sends them again. The Rust pseudonym is used for
SQLite only.

**Reasons.** An agent with `view` reads its own log entries and its own approvals. With
pseudonyms under the tenant key, it could send any value as a parameter and read back that
value's pseudonym, then compare it with masked columns: a read-only pseudonym oracle that
breaks item 4 ("parameters are never pseudonymized").

**Changes.** `openapi.yaml` (`listDatabaseQueries`, `queryDatabase`, the two `params` schemas:
`HiddenValue` or `null`), 0002 comments of `database_approvals` and `database_queries`, STEP2
Postgres item 4, "Both engines", area 4, cards Q-1, A-2, T-2 (a parameter in the log or an
approval never equals the lens pseudonym of the same value), MCP `list_database_queries` text.

## 25. A draft branch has a pseudonym key of its own (re-review: reviewer 1, item 2)

**Decision.** The lens of a database where an agent may write rows or DDL without a person's
approval (a branch with `masked` or `schema_only` data) uses a key derived for that database:
`HMAC-SHA-256(tenant_key, 'db:' || key_node_id)`. New column `databases.key_node_id`: `NULL`
means the tenant key; a masked or schema_only branch sets its own node ID (0002 checks it); a
copy (`copyNode`) and a restore into a new node keep the value of their source, so the key
follows the data (otherwise a person's full copy of a draft branch would give agents
tenant-key pseudonyms of values an agent wrote). A `full` branch keeps its source's key; agents
need `write_live` there. The masking of a copy computes the pseudonyms with the source's key, so
the branch's base tables hold the same pseudonyms that a masked read of the source shows; then
the engine installs the branch's key in `ruum_mask.key` before the branch is `ready`. The
"pseudonym of the pseudonym" rule of item 17 still holds, consistent inside the branch. No SQL
joins a branch with its source, so nothing needs them to match.

**Reasons.** On a draft branch an agent with `edit` applies migrations without approval, and
migrations run as `owner`, so it can write any rows; new columns count as `personal`, and the
lens would show them as tenant-key pseudonyms. That is an offline dictionary for low-entropy
personal data of the live database, without `read_personal`, which item 4's accepted risk
excluded.

**Changes.** 0002 (`databases.key_node_id` and two checks; `tenant_keys` comment), `openapi.yaml`
(`createDatabaseBranch`, `copyNode`, `restoreDatabase`, `Pseudonym`), provider `Source` branch
text, STEP2 Postgres item 4, SQLite item 3, "Both engines", "Accepted risk", area 4, open
question 4, cards Q-0, Q-4, Q-2, Q-3, D-3, D-4, P-3, T-2 (the same value through a draft branch's
lens and its source's lens gives different pseudonyms), MCP `create_database_branch` text, UX
agent rules.

## 26. Agents dry-run migrations only on draft branches (re-review: reviewer 1, item 3)

**Decision.** An agent's `dry_run: true` on a live database or a `full` branch fails with `403
user_required` (hint `dry_run_on_draft_branch`) before anything runs; no approval is made. On a
`masked` or `schema_only` branch it runs as before. A person dry-runs with the rights of the
migration. Of the reviewer's two options (an approval for the dry run, or draft branches only),
the stricter one is taken: an approval for a dry run would need its own hash (so it cannot be
used for the real run) and still lets an agent take `ACCESS EXCLUSIVE` locks on live after one
click; a draft branch gives the agent the same answer with no risk.

**Reasons.** A dry run runs as `owner` and takes the migration's locks (item 5, item 20).

**Changes.** `applyDatabaseMigration`, `ApplyDatabaseMigrationRequest.dry_run`, STEP2 permission
table and "Both engines", cards D-2 and T-1, MCP `apply_database_migration` text, UX agent error
table.

## 27. A TEMPLATE copy drains the source (re-review: reviewer 2, item 1)

**Decision.** PostgreSQL refuses `CREATE DATABASE ... TEMPLATE x` while any session is connected
to `x`, and the platform keeps pooled connections open (items 1, 14). The engine drains the
source: it stops handing out sessions and sets `ALLOW_CONNECTIONS false`; it closes every pooled
connection of every kind (step 3 app pools too); it waits up to 5 seconds for running statements,
then cancels them and ends the remaining sessions; it takes the copy; it allows connections again
and reopens the pools (the reconciler repairs a crash in the middle). Requests during the pause
wait up to 10 seconds, then fail with the retryable `database_not_ready` (hint `retry`). The same
path serves `copyNode` of a database and `source.kind: template`. Decision 18 (small databases
only) keeps the pause to seconds; the cost is stated and accepted. The other option (dump and
restore for a source in use) is not taken: decision 18 names `TEMPLATE`, and a dump of a live
database is slower for the same small sizes.

**Changes.** `createDatabaseBranch`, `copyNode`, `createDatabase` (template), provider `Source`
template text, 0002 `databases` comment, STEP2 Postgres item 7, cards E-2 (done when: a branch
succeeds with open pools of several kinds and a long statement), D-3, P-3.

## 28. Branch column identity comes from the branch (re-review: reviewer 2, item 2)

**Decision.** The copied `database_columns` rows of a branch take their identity (`rel_oid`,
`attnum`, `type_oid`, `data_type`) from the branch's own catalog after the copy and the masking,
matched by schema, table and column name at that moment. The retype of the masking is not a
reset. So after a masked branch, the branch shows no unconfirmed column that was confirmed in the
source, and a catalog sync of the branch changes nothing.

**Reasons.** The masking retypes every `personal` column to `text` (new `type_oid`), and a copy
that is not a `TEMPLATE` copy gets new OIDs; with the source's identity, the first sync would
reset every such column to `retyped` and fill the digest.

**Changes.** `createDatabaseBranch`, provider `Source` branch text, 0002 `databases` comment,
card D-3.

## 29. Exports never hold the key (re-review: reviewer 2, item 3)

**Decision.** A database export leaves out the Ruum schemas (`ruum_mask`, `ruum_masked`,
`ruum_nosecret`), the per-database roles and the grants (`pg_dump --exclude-schema=...
--no-owner --no-privileges`); a SQLite file never holds the key. A restore or import into a new
database builds the Ruum schemas again and installs the key from `tenant_keys` (the tenant export
of design 14 carries the key re-wrapped, item 16). The only plain copies of a key are
`ruum_mask.key` inside the engine databases (owner `mask`) and the per-connection SQLite
registration.

**Reasons.** A plain `pg_dump` includes `ruum_mask.key`: every reader of an export file, also of
an agent's masked export, would get the tenant key and an offline dictionary attack on every
database of the tenant.

**Changes.** `exportTypedNode`, provider README "Export", 0002 `tenant_keys` comment, STEP2
Postgres item 8, cards E-1, E-2, Q-4 (done when), T-2 export cases.

## 30. The key reaches the engine through the trait (re-review: reviewer 2, item 4)

**Decision.** The `DatabaseEngine` trait (E-1) takes the pseudonym key of the database (a key
handle) when it creates a database, copies one (branch, copy, restore into a new node) and opens
a SQLite connection. D-1 runs after Q-4 and loads the tenant key through it; P-3 (copy), D-3
(branch) and D-4 (restore into a new node) do the same for their new databases, with the key of
item 25.

**Reasons.** No card connected the key store (Q-4) with the engine; E-2 and Q-2 could pass with a
fixed test key while production runs with none.

**Changes.** STEP2 area 2, infrastructure item 5, cards E-1, E-2, D-1 (done when: two databases of
one tenant give the same pseudonym for a value, two tenants do not), D-3, D-4, P-3; 0002
`tenant_keys` comment.

## 31. Q-3 owns the SQLite lens (re-review: reviewer 2, item 5)

**Decision.** Q-3 may change `crates/ruum-core/src/privacy/sqlite.rs` and
`crates/ruum-dbengine/src/sqlite/lens.rs`, as Q-2 does for Postgres. E-3 builds the authorizer
hook, the hardening and the `ruum_pseudonym` registration; Q-3 fills in the TEMP lens views and
the restricted-column rules.

**Changes.** Cards E-3 and Q-3.

## 32. App binding roles have a key now (re-review: reviewer 2, item 6)

**Decision.** Option (a): `database_roles` gets `id` (primary key) and a nullable `binding_id`,
with `unique nulls not distinct (tenant_id, node_id, kind, binding_id)` and a check that
`binding_id` is set exactly for the app kinds (`app_read`, `app_write`, not allowed by the
`kind` check until step 3). Pools are per role (database, kind, binding). The engine trait selects
a role by a role reference (database, kind, optional binding). So step 3 adds two kind values and
nothing else at this border. Option (a) is taken over (b) because it costs one column and keeps
0004-14's claim true.

**Changes.** 0002 `database_roles`, STEP2 Postgres item 1 and the kinds table, area 2, the
permission table, card E-1.

## 33. Re-review notes, applied

- Postgres: the pseudonym function has `search_path` `pg_catalog, pg_temp`, and `REVOKE TEMPORARY
  ON DATABASE ... FROM PUBLIC` runs on every database (STEP2 Postgres item 1, infrastructure item
  2, E-2 done when).
- Module declarations: D-1 declares all six submodules of `ruum-core/src/databases/`, Q-1 the
  two masking submodules of `query/`, Q-0 `keys`, `postgres` and `sqlite` in `privacy/`, each as
  an empty file, so no parallel card edits a shared `mod.rs`.
- Planned marks: the integrator resolves the mechanical conflicts of `PLANNED_OPERATIONS`, the
  marks and the generated files by regenerating after the rebase (STEP2 shared files).
- Raw values in a masked branch's history: the point-in-time window of a branch starts at its
  `ready` time (`422` before), so a restore never brings back the values of the copy
  (`createDatabaseBranch`, `restoreDatabase`, D-3, D-4, T-1).
- Step 3 network path: `pg_hba.conf` lines for the app kinds from the app pool come with step 3
  (STEP2 Postgres item 1, infrastructure item 2).
- Branch of a branch: refused with `409 conflict` (open question 10 decided for step 2; D-3, T-1,
  provider `Source`).

## Open

- Design 8.5 names PostgreSQL Anonymizer; item 4 replaces it. A person confirms, and the design
  text follows in a design change.
- Spike S-1 is not run yet; C-1's freeze of the role kinds waits for it (item 15).
- UX contract needs C-20 to C-23 are not in the contract yet.
