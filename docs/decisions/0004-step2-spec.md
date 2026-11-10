# 0004: Step 2 specification decisions (panel reviews)

Status: `provisional`, decided by the agent panel on 2026-11-06 (design section 18.9: one
architect agent drafted the step 2 contracts on `f/step2spec`, two reviewer agents asked for
changes, and this record applies those changes). A person can veto or change any part. Without
a veto, the decisions stand.

Scope: the step 2 contracts (`contracts/providers/`, `contracts/manifests/database.schema.json`,
the `databases` tag and four step 1 descriptions of `contracts/openapi.yaml`,
`contracts/authz/schema.zed`, `contracts/mcp/tools.yaml`, `migrations/0004_databases.sql`) and
`docs/STEP2.md`. Where the reviewers did not agree, or where a fix had more than one form, the
safer option is taken and the text says so.

Migration number: the draft was written as `0002_databases.sql`. At the integration on 2026-11-07,
decision 0003 had already taken 0002 and 0003 (`principal_names`, `agent_names_unique`), so the
file is now `0004_databases.sql`, and "0004" in this record and in `docs/STEP2.md` means that
migration. The content did not change.

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

Item 34 records spike S-1. Item 35 records the acceptance of card C-1 (two architect reviewers,
2026-11-07) and the changes it applied.

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
`database_roles` first would make a failure reopen 0004 and the contract.

**Changes.** STEP2 card S-1, C-1 "After" and "Done when", E-2 "After", order.

## 16. The tenant pseudonym key (reviewer 2, item 9)

**Decision.** One random 32-byte key per tenant in the new table `tenant_keys` (0004), stored
only wrapped (AES-256-GCM) by a key-encryption key: an OpenBao transit key in production, a
local KEK file on the laptop (`RUUM_LOCAL_KEK_FILE`, kept and removed together with the platform
database volume), because dev OpenBao is in memory. The key is made on first use (the first
database of the tenant), under the primary key, so two racing creates keep one key. A tenant
export carries it re-wrapped for the target cell, so pseudonyms stay the same after a move
(design 14). It is never rotated in step 2 (masked branches and copies keep matching their
sources); a rotation would be a new `key_version` and is not specified. New card Q-4 owns it.
Items 25 and 30 add the key of a draft branch and the path of a key into the engine.

**Changes.** `tenant_keys` in 0004 (with RLS), STEP2 infrastructure item 5, area 4, card Q-4.

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
  A partitioned table needs `(tenant_id, at, id)` as the key, so the change is made in 0004 in
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
`HiddenValue` or `null`), 0004 comments of `database_approvals` and `database_queries`, STEP2
Postgres item 4, "Both engines", area 4, cards Q-1, A-2, T-2 (a parameter in the log or an
approval never equals the lens pseudonym of the same value), MCP `list_database_queries` text.

## 25. A draft branch has a pseudonym key of its own (re-review: reviewer 1, item 2)

**Decision.** The lens of a database where an agent may write rows or DDL without a person's
approval (a branch with `masked` or `schema_only` data) uses a key derived for that database:
`HMAC-SHA-256(tenant_key, 'db:' || key_node_id)`. New column `databases.key_node_id`: `NULL`
means the tenant key; a masked or schema_only branch sets its own node ID (0004 checks it); a
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

**Changes.** 0004 (`databases.key_node_id` and two checks; `tenant_keys` comment), `openapi.yaml`
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
template text, 0004 `databases` comment, STEP2 Postgres item 7, cards E-2 (done when: a branch
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

**Changes.** `createDatabaseBranch`, provider `Source` branch text, 0004 `databases` comment,
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

**Changes.** `exportTypedNode`, provider README "Export", 0004 `tenant_keys` comment, STEP2
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
one tenant give the same pseudonym for a value, two tenants do not), D-3, D-4, P-3; 0004
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

**Changes.** 0004 `database_roles`, STEP2 Postgres item 1 and the kinds table, area 2, the
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

## 34. Spike S-1: the result (card S-1)

**Result.** The spike passed. `spikes/step2-roles/spike.sh` ran on PostgreSQL 17.11 (the build
server, 2026-11-07): 225 checks passed, 0 failed. All six points of card S-1 hold: (1) no LOGIN
kind reaches another role, also in a `DO` block or a function; (2) the masked kinds read
pseudonyms in every clause and cannot read the base tables, `ruum_mask.key` or `pg_stats`
values; (3) `ruum_mask.pseudonym` runs from a view but not by name; (4) `masked_write` writes
plain columns only, and a `WHERE` on a masked column compares the pseudonym; (5) `SET LOCAL` and
`DISCARD ALL` leave no session option; (6) the SQL and the Rust pseudonym agree on 29 values
under hostile session settings. **The role kinds of `database_roles` do not change and are now
frozen** (no contract change). The spike also found six gaps in how the model is built. Each
one is fixed below, and the safer option is taken where there was a choice. The output is in
`ruum-server-wt/panel/step2-spike-s1.md`.

- **(a) Objects owned by `mask`.** Since PostgreSQL 16, `ALTER ... OWNER TO mask` and `SET ROLE
  mask` need `SET` membership in `mask`; the `ADMIN` that `CREATEROLE` gives is not enough.
  Decision: `ruum_dbhost` grants itself `mask` `WITH INHERIT FALSE, SET TRUE` when it makes a
  database, and builds the key, the functions and the lens views under `SET ROLE mask`. It holds
  no membership in any LOGIN kind. The direction is safe: the LOGIN kinds are members of nothing,
  so no untrusted session reaches `mask` or `ruum_dbhost`. Item 1's "never granted to
  `ruum_dbhost`" now means the LOGIN kinds.
- **(b) Schema ownership.** `ruum_dbhost` (the database owner) owns `public` and the `ruum_*`
  schemas. The `owner` kind gets `USAGE` and `CREATE` on `public` only and owns the tables, not
  the schema, so an untrusted migration cannot grant `USAGE` on `public` to a lens kind (the
  spike shows that this grant gives nothing). The `owner` kind gets no `CREATE` on the database,
  so it makes no schema and no extension. The base-table rights of `mask` (`SELECT`, `INSERT`,
  `UPDATE`, `DELETE`: an auto-updatable view needs the view owner's rights on the table) and of
  `secret_read` (`SELECT`) come from `ALTER DEFAULT PRIVILEGES` that the platform runs once in an
  `owner` session. A migration that revokes them breaks the lens closed (queries fail).
- **(c) A role can change its own settings and password.** Any LOGIN role can run `ALTER ROLE
  CURRENT_USER SET <option>` (the value reaches every new connection of that kind, and `DISCARD
  ALL` does not reset it) and `ALTER ROLE CURRENT_USER PASSWORD`. The query layer already refuses
  `ALTER ROLE`, but a `DO` block gets around the parser. Decision: before it opens connections for
  a role, `ruum_dbhost` runs `ALTER ROLE <role> RESET ALL`, sets the fixed settings again
  (`search_path`) and a new password with `VALID UNTIL`; after login the engine checks that
  `pg_db_role_setting` holds only the fixed settings, else it closes the connection and repeats
  the reset. A password the role chose itself stops working at the next reset, and `pg_hba.conf`
  allows these roles only from the platform's address.
- **(d) Transaction control ends the platform's transaction.** A `COMMIT` (also `END`,
  `ROLLBACK`, `BEGIN`, `START TRANSACTION`, `SAVEPOINT`, `RELEASE`, `PREPARE TRANSACTION`)
  ends the transaction that holds the `SET LOCAL` options: the next statement runs without the
  timeout. In a migration's `up`, a `COMMIT` breaks the one-transaction rollback of decision 19
  (the spike leaves a table behind after a failed migration). Decision: transaction-control
  statements are added to "Statements that never run" (`422 statement_not_allowed`) for queries
  and for each statement of a migration's `up`; as the engine line, after each untrusted
  statement the engine checks that its transaction is still open (else it fails the request and
  closes the connection). A `COMMIT` inside a `DO` block already fails inside the platform's
  transaction, and `max_prepared_transactions` stays 0 on the data cluster. This changes the
  text of `queryDatabase` and `applyDatabaseMigration`: card C-1 takes it (additive).
- **(e) The canonical text is made inside the function.** With `ruum_mask.pseudonym(text)` and a
  `::text` cast in the view, the pseudonym of a `timestamptz` changes with the session's
  `TimeZone` (also `DateStyle`, `extra_float_digits`, `bytea_output`). Decision: one overload
  `ruum_mask.pseudonym(<type>)` per type, each `SECURITY DEFINER` and computing
  `ruum_mask.canonical(<type>)` with fixed function settings (`search_path`, `timezone = UTC`,
  `extra_float_digits = 1`). The rules, which the Rust side must follow exactly: text as is;
  integers in decimal; `numeric` without trailing fraction zeros (`trim_scale`); floats in the
  shortest form that reads back exactly, in the layout of PostgreSQL `float8out` (fixed notation
  for a decimal exponent in [-4, 15), else `d.ddde+XX`; `NaN`, `Infinity`, `-0`); `timestamptz`
  and `timestamp` as `YYYY-MM-DDTHH:MM:SS[.ffffff without trailing zeros]Z` in UTC; `date` as
  `YYYY-MM-DD`; `bytea` as lowercase hex without `\x`; `boolean` as `true` / `false`; `uuid`
  in lowercase. A `personal` column of another built-in base type (item 37: any other type is
  masked like `secret`) uses a generic overload over `::text` with
  every output setting fixed (`DateStyle`, `IntervalStyle`, `TimeZone`, `extra_float_digits`,
  `bytea_output`); card Q-0 lists those types. Only the agent kinds get `EXECUTE`, on
  `pseudonym` only (not on the HMAC helper).
- **(f) Lens views and migrations.** A lens view depends on the columns it reads, so `ALTER
  COLUMN ... TYPE` and `DROP COLUMN` fail while it exists, and `DROP TABLE ... CASCADE` by the
  `owner` kind drops it. A rename works (the view follows the column). Decision: before a
  migration, `ruum_dbhost` drops the lens views of the database (`SET ROLE mask`); the migration
  runs in an `owner` session; then the catalog sync and the rebuild of the lens views run in one
  `ruum_dbhost` transaction (item 1 already says "after each migration"). These are three
  transactions, because the migration runs as another role on another connection. While the
  views are gone, the lens kinds fail closed (the database is `updating`; requests wait, then get
  the retryable `database_not_ready`). A failed migration rebuilds the views from the unchanged
  catalog.
- **(g) Extensions.** `ruum_dbhost` (no superuser) can create seven of the nine extensions of
  the manifest allow-list; `postgis` and `vector` are not trusted extensions, so it cannot. A lens
  kind reaches extension functions only with `USAGE` on their schema. The stack's Postgres image
  also installs PostGIS in `template1`, so every new database gets `public.spatial_ref_sys`.
  Decision: extensions are created by `ruum_dbhost` in the schema `extensions` (owner
  `ruum_dbhost`), with `USAGE` for every kind and `search_path` `<lens schema>, extensions`; the
  data cluster image (card I-1) marks `postgis` and `vector` as trusted (`trusted = true` in their
  control files), which is safe here because only `ruum_dbhost` has `CREATE` on a database, and
  keeps `template1` free of extensions.
- **(h) Confirmed as specified.** PostgreSQL itself leaves row values out of the `DETAIL` of a
  `CHECK` or `UNIQUE` violation for a role without `SELECT` on the base table (all lens kinds);
  the `owner` kind sees them. The sanitizing of item 8 stays (defence in depth). `pg_stats`
  shows the lens kinds nothing of the base tables. `createrole_self_grant` is empty on
  PostgreSQL 17, and `ruum_dbhost` holds only `ADMIN` (no `INHERIT`, no `SET`) on the LOGIN kinds.
- **Not covered by the spike:** `pg_hba.conf` (card I-1), SQLite (E-3), performance, the
  statement text of another session of the same kind in `pg_stat_activity` (visible to the
  same role by default; every session of a kind has the same data rights, so it shows nothing
  that kind may not read; recorded here, not tested).

**Changes.** STEP2 "The engine security model" (Postgres items 1 to 4, a new item 10),
"Infrastructure work" items 1 and 2, cards C-1, S-1, I-1, E-2, Q-0, Q-1 and D-2;
`spikes/step2-roles/`.

## 35. Card C-1 accepted: the acceptance reviews (card C-1)

**Outcome.** Status `provisional`, decided by the agent panel on 2026-11-07 (design 18.9). Two
architect reviewers read the step 2 contracts on `main` (`1b344d6`):
`ruum-server-wt/panel/c1-acceptance-review-1.md` (contract consistency, tenant isolation, the
8.5 privacy filter, the spike) **accepted with two required changes**, and
`ruum-server-wt/panel/c1-acceptance-review-2.md` (buildability for the next cards and later
steps) **requested four changes and accepts when they land**. Neither asked for a re-review.
Branch `c/c1-acceptance` applies every required change in one `contract:` commit; all of them
are additive (description text, new optional properties, a new shared response and new `503`
responses on planned operations): `oasdiff breaking` has 0 errors, and the regenerated
`web/src/api/schema.d.ts` and `crates/ruum-mcp/src/tools.json` change only by these. With this
commit card C-1 is accepted and the step 2 contracts are frozen; a later change is additive.
No change to migration 0004, `schema.zed`, the manifest schema or the event schemas.

Where the two reviews differ, the safer option is taken:

- **Options inside a migration.** Reviewer 1 refuses `SET LOCAL` and `set_config` of every
  option and has the platform set its options again before each statement of `up`. Reviewer 2
  keeps `SET LOCAL` (so a migration can lower `lock_timeout`) and asks for a wall-clock limit
  in the engine. Decision: both lines of reviewer 1, and the wall-clock limit of reviewer 2 as
  a third line. A migration cannot change a limit; a later additive request field can lower
  `lock_timeout` if people need it.
- **The hint of a refusal.** Reviewer 1 puts the statement number in `hint`, reviewer 2 a fixed
  hint code. Decision: the codes `remove_transaction_control` and `unqualified_names` in `hint`
  (agents can act on a fixed code), and the statement number in `message`.
- **`database_not_ready`.** Reviewer 1 keeps it in the text of `queryDatabase`; reviewer 2 makes
  it a `503` with `Retry-After` on every operation that reaches the engine. Decision: the `503`
  (a `400` tells clients not to retry).
- **Qualified table names.** Both refuse `public.x` and `main.x` rather than rewrite them (a
  rewrite is a second place where a name can resolve to a base table).

**The changes.**

- **(a) Transaction control and the rules of `up`** (reviewer 1 R1, reviewer 2 R1; completes
  item 34 d, b and g). `queryDatabase` "Statements that never run" (on both engines) adds
  transaction control (`BEGIN`, `START TRANSACTION`, `COMMIT`, `END`, `ROLLBACK`, `ABORT`,
  `SAVEPOINT`, `RELEASE`, `ROLLBACK TO`, `PREPARE TRANSACTION`, `COMMIT PREPARED`, `ROLLBACK
  PREPARED`; hint `remove_transaction_control`), and "`SET`, `RESET` or `set_config(...,
  false)` of any session option" becomes "`SET` (also `SET LOCAL`), `RESET` and `set_config` of
  any option". `applyDatabaseMigration` gets "Statements that never run in `up`" (the same
  rules, transaction control included; `422 statement_not_allowed`, the statement number in
  `message`; nothing ran, and no restore point, migration row or journal entry is written; the
  platform sets `statement_timeout` and `lock_timeout` again before each statement) and "What
  `up` may create" (tables, views, functions and types in `public`; no schema and no
  extension; extensions from the manifest in the schema `extensions`). `DatabaseState` and
  `ProviderState`: `updating` also covers a migration. `queryDatabase` "Not ready": during a
  migration, every kind except the owner's waits up to 10 seconds, then gets `503
  database_not_ready` (item 34 f). MCP: `apply_database_migration` says "no BEGIN/COMMIT, no
  SET, no CREATE SCHEMA or CREATE EXTENSION"; `query_database` says "no BEGIN, COMMIT or SET".
- **(b) Unqualified table names** (reviewer 1 R2, reviewer 2 R2; decision 0004-3: the same
  semantics in both engines). After item 34 b the lens kinds have no `USAGE` on `public`, so
  `public.plants` failed with a bare `42501` for agents and viewers and worked for managers;
  SQLite `main.plants` passed the TEMP lens views. `queryDatabase` gets "Table names": write
  names without a schema; `public.` (Postgres) and `main.` (SQLite) are refused for every
  caller (`422 statement_not_allowed`, hint `unqualified_names`); the server never rewrites a
  name; `pg_catalog`, `information_schema` and `extensions` may be named. `getDatabaseSchema`
  and the tool texts of `get_database_schema` and `query_database` say so.
  `ResultColumn.source.schema` keeps `public`, so it matches the `data_classes` keys. Q-1 owns
  the parser rule; T-1 and T-2 add the cases.
- **(c) Names next to principal IDs** (reviewer 2 R3; decision 0003-5, UX C-22). Optional
  `type: string` properties, absent when no name is known (the pattern of
  `on_behalf_of_display_name`): `DatabaseAccessGrant.agent_display_name` and
  `granted_by_display_name`, `DatabaseApproval.decided_by_display_name`,
  `DatabaseColumn.confirmed_by_display_name`. The resolver of decision 0003-5 fills them; A-1,
  A-2 and K-1 build them.
- **(d) `503 DatabaseNotReady`** (reviewer 2 R4). A new shared response: `503`, header
  `Retry-After`, body `Error` with `database_not_ready` and hint `retry`. It is on
  `queryDatabase`, `getDatabaseSchema`, `applyDatabaseMigration`, `createDatabaseBranch`,
  `createDatabaseRestorePoint` and `restoreDatabase`. `QueryFailed` no longer names
  `database_not_ready`; `createDatabaseBranch` names the `503`. All six operations are planned,
  so nothing built changes.

**Notes recorded for the named cards** (non-blocking; STEP2 carries the card rules):

- E-2: the platform's options set again before each statement of `up`, a wall-clock limit with
  a protocol cancel request, and `USAGE` on sequences for the write kinds (an `INSERT` through a
  write lens into a `serial` table needs `nextval`). E-3: the engine sets `PRAGMA
  defer_foreign_keys = ON` in every migration transaction (a SQLite table rebuild cannot turn
  foreign keys off inside a transaction).
- K-1: the catalog covers `public` (SQLite `main`) only, so PostGIS's
  `extensions.spatial_ref_sys` never shows up as unconfirmed `personal` columns; a dropped
  column's row is overwritten or deleted when the name comes back.
- A-2: one pending grant request per agent, database and kind; a row lock until 0004 next
  changes in place and gets `unique (tenant_id, node_id, requested_by, grant_kind) where status
  = 'pending' and subject = 'grant'`.
- Q-2: decide whether the lens also wraps user views and how triggers that name other tables
  behave through the write lens (a conformance case: an `updated_at` trigger). If the lens does
  not wrap views, `getDatabaseSchema` marks objects the caller cannot query.
- Q-2 and D-3: make the copy and the lens agree on empty `secret` values (suggested: every
  non-`NULL` value hidden in the lens, `''` becomes `NULL` in a masked copy); the design change
  of "Open" says so.
- D-2: a migration's `updating` writes no `typed_node.status_changed` (recommended;
  `database.migration_*` covers it). D-1: no folder reader for the manifest's `migrations`
  (step 4 adds a property for migrations in a repo; never widen the string to a `oneOf`).
- UX C-20 becomes its own operation later (for example `checkDatabaseStatement`), never `check:
  true` on `queryDatabase`; C-21 adds optional `databases` objects to `Limits` and `Usage` before
  D-1 starts, with the branch quota apart from the database count; C-23 is its own card after
  A-1 (optional `Database.access_summary`); an optional `DatabaseApproval.database_name` comes
  with C-21 or A-2.
- Text fixes for the next change of each file: the `Pseudonym` example with 12 hex characters
  and "a keyed hash of the value's canonical text"; the `schema.zed` comment on `manage`; the
  shared `ValidationFailed` text naming the step 2 codes.
- Later steps (additive then): step 3 a hook kind `binding_changed`, a gallery source kind, a
  manifest list `schemas` for frameworks that need their own schema or extension; step 4 the
  branch quota and a migrations source in a repo; step 5 nullable `agent_id` and `session_id`
  for workspace grants; 14 (cells) the provider `Source` kind `import` and the hook
  `tenant_imported` before the first tenant moves between cells.

**Changes.** `contracts/openapi.yaml` (`getDatabaseSchema`, `queryDatabase`,
`applyDatabaseMigration`, `createDatabaseBranch`, `createDatabaseRestorePoint`,
`restoreDatabase`, `QueryFailed`, `DatabaseNotReady`, `DatabaseState`, `DatabaseAccessGrant`,
`DatabaseApproval`, `DatabaseColumn`), `contracts/providers/provider.openapi.yaml`
(`ProviderState`), `contracts/mcp/tools.yaml`, the regenerated `crates/ruum-mcp/src/tools.json`
and `web/src/api/schema.d.ts`; STEP2 status, "The engine security model" (Postgres items 7 and
10), cards C-1, T-1, E-2, E-3, D-1, D-2, Q-1, K-1 and A-2.

## Open

- Design 8.5 names PostgreSQL Anonymizer; item 4 replaces it. A person confirms, and the design
  text follows in a design change (with the empty `secret` values of item 35).
- Spike S-1 passed (item 34). The role kinds are frozen. C-1 took the text change of item 34 (d)
  (item 35 a); card C-1 is accepted.
- The `database_roles` comment of migration 0004 predates item 34 (owner of the schemas,
  `ruum_dbhost` membership in `mask`, `USAGE` on `extensions`, items 34 c and d), and one line
  holds both `key_node_id` and `extensions`. STEP2 "The engine security model" wins. Fix the
  file in the same in-place change as the next SQL change of 0004 (an edit alone changes the
  sqlx checksum and forces a fresh database on every stack).
- UX contract needs C-20 to C-23 are not in the contract yet (item 35 records the form each
  takes).

## 36. Dependencies approved by the person (2026-11-08)

The person approved two new dependencies (design 18.4: a new dependency needs approval):

- `sqlparser` (Apache-2.0), for the query path (cards Q-1 and A-2). This closes open question 9:
  Q-1 may start once D-1 is merged.
- `aes-gcm` 0.11.1 (Apache-2.0 OR MIT, RustCrypto), already used by card Q-4 for the tenant
  pseudonym key.

Status: decided (not provisional).

## 37. Tenant-defined types never reach a definer function (review-dbhost-2 item 2, backlog S-1, S-4)

Status: `provisional` (design 18.9); it carries out decision 0005 B-2 of the person, which is
not provisional.

**Problem.** `ruum_mask.pseudonym_other(anyelement)` is `SECURITY DEFINER` (owner `mask`) and
ran `v::text`. A tenant owns its own types, so it may `CREATE CAST (its_type AS text) WITH
FUNCTION ...`. Cast lookup does not use `search_path`, so the fixed path of the function did not
help: the tenant's function ran as `mask` each time a masked reader read the column, and also
in step 2 of a masked copy. As `mask` it could read the key and change `ruum_mask` objects
(`review-dbhost-2.md` item 2, proved on the data cluster).

**Decision.**

- (a) **The masking rule** (0005 B-2). A `personal` column whose declared type is not a
  built-in base type is masked like `secret`. A built-in base type is a type of `pg_catalog`
  with an OID below 16384 and `typtype = 'b'` (arrays of such types included). Every other
  type is "tenant-defined" here: enums, domains (also over a built-in type), composites,
  ranges and multiranges (also the built-in ones), arrays of those, and the types of
  extensions. The masked lens shows the hidden constant (`NULL` stays `NULL`; the null test is
  `num_nulls`, so it never looks inside a composite; a `secret` column uses the same form). The query layer turns it into
  `{"$ruum": "hidden", "class": "personal"}`. In a masked copy such a column becomes `text`
  and holds `NULL`. Agents cannot join or group on such a column: every value is the same
  constant. This replaces "a domain uses the overload of its base type" of item 34 (e) and
  the list of generic types of card Q-0 for every type that is not a built-in base type.
- (b) **Only built-in base types reach a definer function.** The lens and the masked copy
  call a pseudonym function only for a column whose declared type is a built-in base type; any
  other column, or one whose type the engine cannot read, gets the hidden form (fail closed).
  `pseudonym_other` is PL/pgSQL and first checks the type of its argument; for any other type
  it raises `42804` before any conversion, so a direct call (tenant code in step 2 of a masked
  copy, while the copy's owner holds `EXECUTE`) cannot reach a tenant cast. A tenant cannot add
  a cast between two built-in types (`CREATE CAST` needs the ownership of one of them), so the
  remaining conversions are those of PostgreSQL itself.
- (c) **Fixed search path and qualified bodies.** Every function of `ruum_mask` keeps
  `search_path = pg_catalog, pg_temp`; the bodies of `hmac12` and `pseudonym_other` also
  qualify their functions and operators (`operator(pg_catalog.||)`), so they resolve nothing
  outside `pg_catalog` even if a setting of the function were changed.
- (d) **No tenant code runs as `mask`; every build makes `mask`'s objects again.** The
  protection is (b): no tenant code ever runs as `mask`. The rest is defence in depth.
  `mask` holds `USAGE` only on `ruum_mask`, `ruum_masked` and `ruum_nosecret`. `ruum_dbhost`,
  the owner of the schemas, grants it `CREATE` inside the transaction that makes the functions
  or builds the lens views, and revokes it before the commit. This blocks only `CREATE OR
  REPLACE` and new objects (review-masking-1 F1, review-masking-2 F-2): `mask` still owns the
  functions, the key and the views, and an owner needs no `CREATE` to replace the `_RETURN` rule
  of a view, to `ALTER FUNCTION`, to add a trigger, to change the key or to grant rights. So
  every build of the lens (each migration, `sync_lens`, step 3 of a copy, and a restore in
  place) drops the three lens schemas, makes the functions, the key table (the key is kept or
  replaced) and the views again, and takes back every default privilege that `mask` gives to
  another role. A change that code as `mask` made, and the objects of the code from before
  this fix, do not survive the next build; a restore point from before the fix comes back
  repaired (review-masking-2 F-1). A build refuses to go on when an object outside the lens
  schemas depends on an object inside them: only tenant code in step 2 of a masked copy, while
  the copy's owner holds `USAGE` on `ruum_mask`, could make one, so such a copy is refused
  (review-masking-1 F2). The role kinds are frozen (item 34), so no new role kind was added:
  `mask` stays the owner of the functions, the key and the views. A role that lens sessions
  never run as, as the owner of the functions and the key (review-dbhost-2), needs a later
  decision.
- (e) **Tests** (backlog S-4). `crates/ruum-dbengine/src/postgres/tests/tenant_types.rs`, on
  the data cluster: a tenant with an enum, domains (one with a check, one over the enum), a
  composite, a range, an array of the enum, casts of each to `text`, an implicit cast from
  `integer` to its enum, an operator, a view and default privileges. Every tenant function
  marks a session setting and tries the role grants of `hostile_copy`. The masked reader sees
  the hidden constant, no tenant function runs, no role gains a membership; a direct call of
  `pseudonym_other` with each tenant type fails; the full, schema-only and masked copies, a
  restore and `sync_lens` grant nothing and leave `mask` without `CREATE`; a pooled admin
  connection keeps its search path after `SET` and `RESET ALL` / `DISCARD ALL`
  (review-dbhost item 3). The state from before the fix and changes that need only ownership
  do not survive `sync_lens` or a restore in place of a restore point in that state; a masked
  copy whose step 2 leaves a view on `ruum_mask.key` is refused; a secret composite whose
  fields are all `NULL` shows the constant; a domain over `text` is hidden by the guards of the
  lens and of the masked copy on their own. Mutations `dbengine-mask-tenant-type-pseudonym`,
  `dbengine-mask-other-no-type-check`, `dbengine-mask-keeps-create`,
  `dbengine-mask-copy-tenant-type-pseudonym`, `dbengine-mask-build-keeps-mask-objects`,
  `dbengine-mask-restore-keeps-snapshot-objects`, `dbengine-mask-foreign-dependents-ignored`
  and `dbengine-mask-secret-is-null`.

**Effect on pseudonyms.** A built-in base type keeps its pseudonym (the canonical text did not
change). A personal column of a built-in range type or of an extension type (for example
`int4range`, `geometry`, `vector`) now shows the hidden constant instead of a pseudonym.

**Changes.** `crates/ruum-dbengine/src/postgres/lens.rs`, `setup.rs`, `copy.rs`; the doc of the
generic overload in `crates/ruum-core/src/privacy/mod.rs`; the tests and mutations above.
Fix records: `ruum-server-wt/panel/masking-fix.md` and `masking-fix-2.md` (the two security
reviews `review-masking-1.md` and `review-masking-2.md`).

## 38. Masked sessions never run tenant code on live data (backlog S-21, review-Q-2-3 R-1)

Status: `provisional` (design 18.9). The architect proposal `ruum-server-wt/panel/s21-proposal.md`
(option B of four) and its two reviews (`s21-review-1.md`, security: changes requested;
`s21-review-2.md`, use and cost: accepted with changes). Every required change of both reviews
is applied. Where they differ, the safer option is taken and the text says so. Fix record:
`ruum-server-wt/panel/q2-s21-fix.md`.

**Problem.** The lens decides what a statement may read. But a session also starts tenant code:
functions in views, triggers, rules, defaults, policies. That code runs on the base tables and
sees the real rows, whatever lens the session has, and it can copy a value into a column the
session may read. Each Q-2 review found one more such path (P-1: triggers; R-1: a function
behind a view in a row write). A list of paths is never complete.

**Decision.**

- (a) **The rule.** A masked session never runs tenant code in a writable transaction on a
  live database. A *masked session* is any session whose lens hides a value that tenant code can
  see: the kinds `masked_write` (agents without `read_personal`) and `write` (agents with
  `read_personal`, people with `edit` but not `manage`; lens `nosecret`, so secrets stay
  hidden). Only `owner` (people with `manage`, migrations) and, in step 3, the app kinds (lens
  `base`) are outside the rule. Agents with `read_personal` are masked sessions (both reviews:
  the proposal had exempted them, which reopened P-1 for secrets).
- (b) **Tenant code** is any object of the tenant that Postgres may run during a statement:
  functions, procedures and aggregates (any language, volatility or security: a `STABLE`
  function can call a `VOLATILE` one), triggers (also with a built-in function, also disabled),
  rules other than a view's own `_RETURN`, row-level security policies, relations of a kind
  outside the known list (a foreign table) and event triggers. Casts, operators, defaults,
  generated expressions, domain checks and range types need a tenant function to run tenant
  code, so the function clause covers them. An object is the tenant's when the database's
  `owner` role owns it or it is in a schema that role may create in, so a later grant cannot open
  a gap (review 1 L-2). The functions of allow-listed extensions are in schema `extensions`,
  which the tenant does not own and cannot create in. The check is strict: **any** tenant code
  counts, not only code that depends on a restricted column (review 1 L-1: trigger functions are
  never SQL-standard, so a dependency walk would count every trigger anyway). Review 2 N-3
  suggested scoping the `write` kind to databases with a `secret` column; the stricter form is
  taken, and the scope stays an option for later.
  **Built-in functions are code too, unless they are on the allow-list** (review-Q-2-s21-2 R-2,
  review-Q-2-harden H-1). A built-in function can carry state out of an expression: `set_config`,
  `setseed` with `random()`, advisory locks, sequences and large objects. A list of such functions
  is never complete, so the rule is the other way round. Code that runs on the real rows (a view
  of the tenant, a policy, a `CHECK` constraint including a domain's, an index expression or
  predicate, a generated column) may call only: a built-in function that Postgres does not mark
  `VOLATILE` and that is named in `ROW_FUNCTIONS` (`crates/ruum-dbengine/src/postgres/allow.rs`:
  aggregates and window functions, math, strings, dates and times, arrays, ranges, JSON and text
  search), the function of a built-in operator, and the function of a built-in cast. A function of
  the tenant or of an extension, a session setting, a lock, a random seed, a sequence, a large
  object, and a function that runs a query given as text are not on the list. Built-in functions
  are pinned, so `pg_depend` does not show them; the engine reads the stored node tree of the
  object. Such an object counts as tenant code for writes (kind `unlisted_functions`; a
  column default is not scanned, it sees no row).
- (c) **Three lines, each fails closed.**
  1. The query layer refuses a row write (`INSERT`, `UPDATE`, `DELETE`, `MERGE`, a
     data-modifying `WITH`) of a masked session when the database holds tenant code:
     `501 not_implemented` with the hint code `tenant_code_on_live` (review 2 R-4: the shape
     of the destructive statements until card A-2, and a fixed hint code, 0004-35; no new
     error code, so no contract change). Nothing runs.
  2. The engine refuses the same in the request's own transaction, for every writable
     transaction of `masked_write` and `write`, whatever the query layer thought the statement
     was: once before the statement (a clear refusal) and again after it, before `COMMIT`
     (review 1 M-2, review 2 N-6). DDL that commits before the statement's locks is seen by the
     second check; DDL after them waits for this transaction; the rollback takes back what the
     code wrote. Only effects outside the transaction stay (a sequence number, timing). This
     closes review-Q-2-3 N-2 and S-24's second half. Error: `422 statement_not_allowed`, hint
     `tenant_code_on_live`.
  3. A masked session's statement that writes no row runs read-only, also in mode `write`
     (before: agents only; now people with `edit` too).
  The proposal's "code fingerprint" is dropped: the check after the statement closes the race
  without it (review 2 N-6).
- (d) **Reads.** A masked read may run a view or a policy of the tenant only when everything it
  calls is on the allow-list of (b). A relation whose view or policy reaches a function outside
  the list (and every view that reads such a relation) is not part of the masked lens: both lens
  schemas hold a *refusal view* of the same name and columns, and a statement that reads it gets
  `422 statement_not_allowed` with the hint `tenant_code_on_live` (the planner raises it before
  any tenant code runs; a person with `manage` and the owner kind still read the real view). This
  closes review-Q-2-s21-2 R-1 (a tenant function that writes a session setting) and R-2 and
  review-Q-2-harden H-1 (built-in functions that take a lock, set a seed or use a sequence).
  The functions that the statement calls itself are held by the cluster: the set-up revokes
  `EXECUTE` on every `VOLATILE` function of `pg_catalog` from `PUBLIC`, except `nextval`,
  `currval`, `lastval`, `gen_random_uuid`, `uuidv7`, `clock_timestamp`, `timeofday`, `random`,
  the `pg_sleep` family and `pg_cancel_backend` / `pg_terminate_backend` (none of them carries a
  value from one expression to another; the last two end only sessions of the caller's own
  role, which the engine does when it drains a copy). Postgres
  marks a function `IMMUTABLE` or `STABLE` only when it cannot write. The `owner` kind keeps
  every function, and the engine refuses a database where another kind may call a revoked one.
  Postgres checks the right of a function in a view against the reader, so the line holds inside
  views, and for a function that runs a query given as text. The pooled connection is reset with
  `DISCARD ALL` after every use, so no session state outlives a lease. **Accepted read-path
  limit:** an error or its timing can reveal one fixed fact per row, and a masked read still
  evaluates the built-in expressions of the lens itself. Review 1 M-1 option 1 (evaluate tenant
  views over the lens tables) stays open as a later hardening.
- (e) **Draft branches.** The rule lets tenant code run freely on a draft branch with `masked` or
  `schema_only` data: its rows are pseudonyms and `NULL` secrets, so tenant code learns nothing
  beyond a masked read (review 2 N-4 checked this). Draft branches are not built yet, and a
  masked *copy* of a node is not a draft branch (people may add real rows to it later). So today
  every database counts as live. The card that builds draft branches marks the engine copy of a
  branch at its creation, and both lines then skip it.
- (f) **The ways on.** Until card A-2, a person with `manage` makes the change. A-2 turns the
  `501` into a `202` with a new `DatabaseApproval.reasons` value `tenant_code` (additive). That
  approval (review 1 H-2, review 2 R-4): is always a person's, and no access policy can grant it
  in advance (like the migration approval, 0004-5); counts as a real-data access of the agent,
  logged as such; and says so in the person's words, without "trigger", "function" or "branch"
  ("This database runs its own logic on changes, and that logic can copy real data to places
  the agent can read"). A data change from a branch is a migration with DML, which runs as
  `owner` and already waits for a person. There is no "publish a data change" (review 2 R-4).
- (g) **Residual, stated.** The rule removes the masked session as the starter of tenant code. It
  does not stop tenant code that others start: an app's users, a person with `manage`, a job. A
  trigger can still copy a personal value into a column confirmed `normal`, where a masked
  agent reads it later (review 1 M-3, review 2 R-3). Tenant code that copies data gives its
  target columns the class of what it reads: the agent that writes the code suggests that class
  in its migration, and the `privacy` stage of the pipeline (design 11.2) checks it. That stage
  is a card of step 4 or 5. A narrow case also stays: tenant code made between the two engine
  checks that removes every trace of tenant code before the second check (it would need to drop
  itself while it runs).
- (h) **Answers to the panel's open items.** S-23 (editors without `manage`): decided, the strict
  form. Platform helpers (an `updated_at` trigger the platform installs: owned by a platform role,
  in a schema the tenant cannot write, a pinned body, known by owner and OID; review 1 L-1, review
  2 N-2): wanted, not in this cut; a card before step 3 templates use a trigger. An agent should
  see before it plans that live writes need a person (review 2 N-1): an additive field of
  `getDatabase` later, with A-2.

**Tests.** `crates/ruum-core/src/databases/query/postgres_mask_tests.rs`
`masked_sessions_never_run_tenant_code_on_live`: one hostile case per kind (a trigger; a disabled
trigger with a built-in function; a rule; a policy; the R-1 function behind a view; a `STABLE`
function over a `VOLATILE` one; a definer function behind a view; a volatile default; an
aggregate over a built-in function), each for an agent, an agent with `read_personal` and a
person with `edit`, three or more write forms each; `501` with `tenant_code_on_live`, no row
changed and nothing copied; a control write after each case; the engine line for both kinds; a
person with `manage` still writes. `crates/ruum-dbengine/src/postgres/tests/tenant_code.rs`: each
clause of the check on its own (and the extensions' functions never count); the engine line
before and after the statement, with a trigger made between the two checks (a sequence shows
that the statement ran and was rolled back); a guard over every catalog column that points to a
function or holds an expression, so a Postgres upgrade that adds one fails. The test database of
the hostile tests holds no tenant code by default; P-2 is now tested through a function in a
view (masked sessions reach tenant code only in reads). Mutations: `databases-query-agent-side-effects`,
`databases-query-tenant-code-write-kind`, `databases-query-agent-read-only`,
`dbengine-masked-write-side-effects` (the check before), `dbengine-tenant-code-post-check`,
`dbengine-tenant-code-write-kind`, `dbengine-tenant-code-functions`,
`dbengine-tenant-code-triggers`, `dbengine-tenant-code-disabled`, `dbengine-side-effects-rules`,
`dbengine-tenant-code-policies`. The clauses for relations and event triggers have no mutation:
a test cannot make a foreign table (no allow-listed foreign data wrapper) or an event trigger
(superuser only).

**Changes.** `crates/ruum-dbengine/src/postgres/depends.rs` (`TENANT_CODE_SQL` replaces
`SIDE_EFFECTS_SQL`), `session.rs`, `mod.rs`, `lib.rs` (`DatabaseEngine::tenant_code_on_live`
replaces `write_side_effects`); `crates/ruum-core/src/databases/query/mod.rs`; the tests and
mutations above; design 8.5.

**Hardening of the read path (review-Q-2-s21-2 R-1 and R-2, review-Q-2-harden H-1).** Items (b)
and (d) above carry the rule. Fix record: `ruum-server-wt/panel/q2-harden.md`. Code:
`crates/ruum-dbengine/src/postgres/allow.rs` (the two lists), `depends.rs` (`RootFacts::unlisted`,
`tenant_code_sql`), `lens.rs` (`policy_edges`, `code_relations`, `refusal_view_sql`), `setup.rs`
(`grants_sql`, `check_volatile_revoked`), `errors.rs`, `deploy/dev/postgres-data/initdb/10-ruum-data.sh`,
`scripts/dbhost_check.sh`. Tests: `crates/ruum-dbengine/src/postgres/tests/allow_list.rs` (one per
rule: statements, views and policies, writes, the check of the cluster set-up, the reset of the
pooled connection), the new cases of `tenant_code.rs`, `errors_from_inside_a_routine_are_marked`
in `security.rs`, and `masked_reads_of_code_outside_the_allow_list_are_refused` in
`postgres_mask_tests.rs` (the S1, U2, V and H1 attacks as statements, for an agent and for Bob).
Mutations: `dbengine-allow-list-views`, `-built-ins`, `-names`, `-policies`, `-writes`,
`dbengine-refusal-view-dropped`, `dbengine-volatile-unchecked`, `dbengine-volatile-granted-to-public`,
`dbengine-pool-keeps-session-state`. **Consequences.** A view or policy that calls a function of an
extension (`citext` operators, `pgcrypto`, `postgis`) is outside the list until a panel decision
names the functions; a person with `manage` and the agent's migration path are not affected. A
column default is not scanned. A `CHECK` of a domain is scanned for writes, not for a cast to the
domain inside a view (backlog). The tests of P-2 and F-1 to F-5 that read a view with a function of
the tenant as a masked session now expect the refusal.
