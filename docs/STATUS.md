# Status of step 1

This page says what works on `main` after the wave B merge, the security fixes of 2026-11-06,
the panel decisions of `docs/decisions/0002-step1-panel-decisions.md`, the quality wave (wave
C), the UX fixes of the web UI (`q/uxfix`), the synthetic-user fixes (`q/synthfix`), the
names of actors (decision 0003, `f/followups`), the step 2 specification (decision 0004,
`f/step2spec`) and the morning integration of 2026-11-07 (decision 0003 items 1 to 4 and 7, spike
S-1, the first step 2 cards, the design system and localisation of the web UI), the afternoon
integration of 2026-11-07 (decision 0003 item 6, the C-1 acceptance, cards E-2, Q-4 and P-2),
the evening integration (run on 2026-11-08: the browser tests on the build server, sign-in
providers, the second E-2 fix, cards D-1, T-1 and T-2), the night integration of 2026-11-08
(`f/followups-4`, the new voice and Estonian of `w/voice`, cards K-1 and A-1; run on
2026-11-09), the integration of 2026-11-09 (the RUSTSEC-2026-0459 fix, `f/masking-hardening`,
decision 0005 B-3 and B-1, card Q-1), the evening integration of 2026-11-09 (the C-8 race, a data
cluster per agent, K-1 C-2, the step 2 error codes and query rule 10, the brand directions; run
on 2026-11-10), how to run it, the test counts and the known gaps. The work areas are in `docs/STEP1.md`. The foundation and wave A are in
`docs/FOUNDATION.md`.

## What is merged

Wave A: `swarm/authz`, `swarm/journal`, `swarm/identity`, `swarm/webui`.
Wave B: `swarm/nodes`, `swarm/blobs`, `swarm/webdav`, `swarm/mcp`, `swarm/webui2`.
Wave C (2026-11-06, rebased onto `main` and fast-forwarded, in this order): `q/decisions`,
`q/pgsuites`, `q/props`, `q/browser`, `q/bench`, `q/uxdesign`.

| Branch | What it adds |
|---|---|
| `q/decisions` | Decision record 0002 and its implementation (see "Panel decisions" below). |
| `q/pgsuites` | The isolation, permission and trashed file suites also run on the Postgres stores (`RUUM_SUITE_BACKEND=postgres`, optional `RUUM_SUITE_AUTHZ=spicedb`, `RUUM_SUITE_RLS=off`); Postgres tenant-filter mutations; the mutation check runs strict. |
| `q/props` | Property tests (`crates/ruum-core/tests/properties`: authz, changes feed, chunking, diff) and two fixes they found: a member link can no longer form a loop of `view` (`rules::member_link_loop`, `422`), and the changes feed keeps an agent's grant entries inside its scope. |
| `q/browser` | Playwright tests in `web/e2e` against the real server, Zitadel and Mailpit (`scripts/e2e_browser.sh`), the browser driver for agents (`scripts/ux/drive.mjs`), the CI job `e2e-browser`. |
| `q/bench` | The first agent benchmark over MCP (`bench/agent/`, 8 step 1 tasks; first run 7 of 8 passed; `docs/BENCHMARK.md`). |
| `q/uxdesign` | The UX package (`docs/ux/`: personas, journeys, IA, design system, copy, agent experience, gap analysis), provisional. |

UX fixes (2026-11-06, `git merge --no-ff q/uxfix`, no conflicts; only `web/` and `docs/ux/`
changed): the blockers and most majors of the UX panel. Details and contract needs C-7 to C-11
are in `docs/ux/decisions.md`.

| Area | What changed in the web UI |
|---|---|
| Shared with me | Every row opens the item (`/t/<space>/files/<id>`, `/t/<space>/item/<id>`); the space and the sharer by name, never an ID; a space that is not in the person's list opens in a guest shell. |
| Share links | `/s#token` visitor page (password, ended link, subfolders, Download and Preview with the link's bearer token; the token never goes into a URL). |
| Spaces, invitations | "Start a team space..." in the switcher and on Account; `/invitations/accept` (sign-in keeps `return_to`, one click joins); a personal space offers only Guest. "Send an email" is off (`notify: false`) until the server sends share emails. |
| Agents | `/t/<space>/agents/connect` (MCP address, steps for Claude Desktop, ChatGPT, Claude Code); consent defaults to one folder and 90 days; Pause/Resume, access summary, activity with item names. |
| Files | Names instead of IDs (from `listMembers`, `listAgents`), agent marks in Changed, a coalescing warning before "Upload new version", one upload panel with a conflict question per batch, selection with bulk Move and Trash (Undo), sorting, error copy for every `ErrorCode` (`api/errors.ts`), welcome card and empty states. |
| Tests | Flow tests for share links, invitations, uploads, selection, errors; `web/e2e/visual.e2e.ts` (88 snapshots on the mock UI, darwin only) and `web/e2e/a11y.e2e.ts` (axe, WCAG 2.2 A/AA); the 4 UI `test.fixme` of the browser tests now run. |

Synthetic-user fixes (2026-11-06, `q/synthfix` fast-forwarded onto `main`, no conflicts; only
`web/`, `docs/ux/decisions.md` and `scripts/ux/README.md` changed; no server or contract change):
the 2 blockers and the majors that two synthetic users (design 18.9) found. Decisions S1 to S6
and contract needs C-12 to C-14 are in `docs/ux/decisions.md`.

| Area | What changed in the web UI |
|---|---|
| Share dialog | A share to a member or guest goes by user ID (`addGrant {type: "user"}`), so it reaches the account at once; the access from the space role ("Everyone in *Space* except guests: Can manage") is listed and a weaker share is called out; one "Email them an invitation" box for a person outside, plus "Use a link instead"; own row read-only; "added by". The mock API keeps email grants as email grants, like the server. |
| Upload conflict | "Save as a new version" is the default and first names the person's own recent unnamed version ("Before the upload of HH:MM"), so nothing is lost. |
| Activity log | Access, role, sharing rule, link and member entries name the item (a link), the person and the old and new values (from `AuditEntry.data`). |
| Smaller | Change to Guest asks first; sharing rule inherit shows the parent's value and "Saved."; version times; "Keep forever..."; compare summary; image icon; sign-in labels. |

Night integration (2026-11-07, rebased onto `main` and fast-forwarded, in this order; linear
history, no merge commit):

| Branch | What it adds |
|---|---|
| `f/followups` | Decision record 0003 (step 1 follow-ups, `provisional`; items 1 to 8 decided) and its item 5 (C-7, names of actors): migrations `0002_principal_names.sql` and `0003_agent_names_unique.sql`; `display_name` and `on_behalf_of_display_name` in every answer (middleware `ruum-api/src/names.rs`, MCP included); unique agent names per owner (" (2)"); `listChanges` filters `actor` and `since`; the web UI shows server names, an agent as "*Agent* (agent of *User*)"; bench task `09_undo_agent_two_bots`. |
| `f/step2spec` | The step 2 (Databases) specification and decision record 0004 (`provisional`, items 1 to 33): `contracts/providers/`, `contracts/manifests/database.schema.json`, 24 database operations in `contracts/openapi.yaml` marked `x-ruum-status: planned`, `schema.zed` relations for databases, step 2 event schemas, `migrations/0004_databases.sql` (draft), `docs/STEP2.md` (task cards), `docs/ux/databases/`. The test kit reports the checks of planned operations as planned (also with `RUUM_REQUIRE_IMPLEMENTED=1`) and still fails on leaks; the MCP server offers no tool of a planned operation. |

Fixes made during the night integration:

- The rebase of `f/step2spec` renamed its migration `0002_databases.sql` to `0004_databases.sql`
  (0002 and 0003 went to decision 0003); the SQL did not change. `docs/STEP2.md` and decision
  0004 say "0004" now.
- `crates/ruum-mcp/src/tools.json` and `web/src/api/schema.d.ts` were regenerated at each
  rebased commit (both branches changed `openapi.yaml` and `tools.yaml`; the YAML merged without
  conflicts).

Content role of team members (decision 0003-1, branch `f/content-role`): members get a
content role `view | comment | edit | manage` (default `manage`) on the team tree.
Migration `0005_member_content_role.sql` (`memberships.content_role`, `invitations.content_role`,
CHECK: set if and only if `role = member`); `schema.zed` relations `content_viewer`,
`content_commenter`, `content_editor` and permissions `team_manager`, `team_editor`,
`team_commenter`, `team_viewer`; optional `content_role` in `updateMember`, `createInvitation`,
`Member` and `Invitation`; accepting an invitation never lowers a content role; rule (b) of
0002-6 counts only managers (`TenantStore::manager_since`); tag writes need content role `edit`.
The permission matrix has three new columns (`member_view`, `member_comment`, `member_edit`).
The web UI part is built on the design system (`w/design-system`, see below).

Morning integration (2026-11-07, each branch rebased onto `main`, checked on the build server,
pushed and green in GitHub CI, then fast-forwarded, in this order; linear history, no merge
commit, no co-author line):

| Branch | What it adds | Review |
|---|---|---|
| `f/content-role` | Decision 0003-1: a content role for team members (above). | accepted (`review-f1.md`) |
| `f/email-grants` | Decision 0003-2 and 0003-3: an email grant binds at sign-in to the verified email; share and invitation emails through the mail outbox (`0006_email_grants_and_mail.sql`, worker, rate counters); `addGrant` answers `notified`. | accepted (`review-f23.md`) |
| `f/audit-mcp` | Decision 0003-4 (C-14: `auth.login` and credential use in the audit log, `0007_session_tenants.sql`), 0003-7 (`searchNodes` `type`, `moveNodes`, `attachTagToNodes`, tool texts, `user_required` hints, the benchmark outcome `safe_stop` and harm checks, `0008_nodes_type_index.sql`), 0003-8 recorded. | accepted (`review-f47.md`) |
| `s2/spike` | Spike S-1 (`spikes/step2-roles/`): the Postgres engine security model holds, 225 checks, 0 failed; the role kinds of `database_roles` are frozen; decision 0004-34 (a) to (g) fixes six gaps in how the model is built; `docs/STEP2.md` follows. | spike result (`step2-spike-s1.md`) |
| `s2/q-0` | Card Q-0: `ruum_core::privacy` (marker types, `Pseudonymizer`, `MaskingRules` hook, canonical text with the spike's test vectors), plus the doc fix of review note 1 (`int2`/`int4` resolve to the `double precision` overload). | accepted (`review-Q-0.md`) |
| `s2/p-1` | Card P-1: `ruum_core::providers` (the `Provider` trait, the registry, the in-memory provider, the conformance suite); the flaky export case of the review counts per tenant now. | rejected for one flaky test, fixed on the branch (`review-P-1.md`) |
| `s2/e-1` | Card E-1: the new crate `ruum-dbengine` (the `DatabaseEngine` trait, the SQLite memory engine, the engine conformance suite); a failed `COMMIT` rolls back (case `deferred_constraint`). New dependency: `rusqlite` (`bundled`, `hooks`). | changes requested (B-1), fixed on the branch (`review-E-1.md`) |
| `s2/i-1` | Card I-1: the data cluster `postgres-data` (Postgres 17.11, PostGIS, pgvector, no Anonymizer) with per-database LOGIN roles, `scripts/dbhost_check.sh`, `dev.sh` env and status. | accepted (`review-I-1.md`) |
| `w/design-system` | Design 12.5 and 12.4: tokens (W3C format, Style Dictionary), the component library `web/src/ui` with Ladle and lint rules, every screen on it; FormatJS localisation (English, machine-translated Estonian, pseudo-locale), `updateMyPreferences` and `Me.preferences` (`0009_user_locale.sql`); the UX panel fixes; the web UI of decision 0003 items 1 to 4 and 7; the invitation token in the URL fragment. New web dependencies: `react-intl`, `@formatjs/icu-messageformat-parser`, Style Dictionary, stylelint, Ladle. | UX panel accepted with changes, fixed on the branch (`ux-heuristics.md`, `ux-accessibility.md`, `ux-consistency.md`) |

The review files are in `ruum-server-wt/panel/`. Every decision is `provisional` (design 18.9).

Fixes made during the morning integration:

- Migrations renumbered at the rebase (the SQL did not change): `f/email-grants` 0005 to
  `0006_email_grants_and_mail.sql`, `f/audit-mcp` 0005 and 0006 to `0007_session_tenants.sql`
  and `0008_nodes_type_index.sql`, `w/design-system` 0005 to `0009_user_locale.sql`.
- `f/email-grants` was taken in the form that `w/design-system` had already rebased onto
  `f/content-role` (the same four commits, adapted to the content role); `w/design-system` was
  rebased without its copies of those commits.
- `f/audit-mcp`: the sign-in audit row comes first (it fails closed), then the email grants
  bind; the identity test area keeps both the audit log and the in-memory authz engine.
  `permission_matrix.md` was regenerated for `moveNodes` and `attachTagToNodes` with the
  content-role columns.
- `tools.json` and `web/src/api/schema.d.ts` were regenerated where the contract commits met.
- `s2/p-1` and `s2/q-0` both add a `pub mod` line to `ruum-core/src/lib.rs`: both kept.
- The CI job `browser end-to-end` failed once on `integ/audit-mcp` and `integ/s2-e-1` on
  `versions.e2e.ts` "a name conflict on upload ..." (the known upload race of the design system
  report); the re-run passed. `w/design-system` changes that test's flow.
- The rebased branches are on GitHub as `integ/email-grants`, `integ/audit-mcp`,
  `integ/s2-spike`, `integ/s2-q-0`, `integ/s2-p-1`, `integ/s2-e-1`, `integ/s2-i-1` and
  `integ/design-system` (the old branch names keep the pre-rebase commits; no force push).

Afternoon integration (2026-11-07, each branch rebased onto `main`, checked on the build server,
pushed and green in GitHub CI, then fast-forwarded, in this order; linear history, no merge
commit, no co-author line):

| Branch | What it adds | Review |
|---|---|---|
| `w/fixes` | Web UI: an upload whose name the server says is taken (`name_taken`, a name the list did not show yet) comes back to the conflict question, and a new version starts from the head the server has now; agent names wrap like text at phone width. Linux and darwin references of the changed phone screens. | no panel review file (UI fixes) |
| `f/ux-c8-c11` | Decision 0003-6 (C-8 to C-11): `coalesce` on new versions and `putContent` (the web UI always sends `false`), `0010_upload_coalesce.sql`; `previewInvitation` (public, rate limited per IP) and the accept and sign-in pages that use it; `SharedItem.tenant_name`; `UndoItem.changed_by`. Localised in en and et. | built from decision 0003-6; no review file of the build |
| `c/c1-acceptance` | Decision 0004-35: card C-1 is accepted and the step 2 contracts are frozen (additive changes only from now on). One `contract:` commit with the required changes of both reviews: the 0004-34 rules in `queryDatabase` and `applyDatabaseMigration`, unqualified table names, names next to principal IDs, `database_not_ready` as `503` with `Retry-After`. | accepted with changes, applied (`c1-acceptance-review-1.md`, `c1-acceptance-review-2.md`) |
| `s2/e-2` | Card E-2: the Postgres engine of `ruum-dbengine` on the shared data cluster (pool, sessions, setup, lens, catalog, copy, export, split). The copy steps that change tenant tables run as the copy's owner in its own session, with tenant triggers, rules and forced RLS parked; hostile copy tests. CI builds `postgres-data` and installs `pg_dump` 17. | changes requested (B-1, cross-tenant role escalation through tenant rules and triggers during a copy), fixed on the branch (`review-E-2.md`); the re-review (`re-review-e2.md`) was not complete. Its check of hand-over steps 1 and 3 found a second escalation: `ruum_dbhost` sessions searched `public`, so a tenant operator (`oid = integer`, `name !~ name`) ran as `ruum_dbhost` in step 1, step 3 and every lens rebuild. Fixed on `f/rr-fix-e2`: every `ruum_dbhost` session has `search_path = pg_catalog, pg_temp`; hostile test and mutation `dbengine-dbhost-search-path` (the nightly job starts `postgres-data`). Needs a second review. |
| `s2/q-4` | Card Q-4: the tenant pseudonym key (`tenant_keys`), wrapped with AES-256-GCM by a local key-encryption key (`RUUM_LOCAL_KEK_FILE`) or OpenBao transit; export and import re-wrap; `database_key`. `dev.sh env` sets the local KEK file. New dependency: `aes-gcm` 0.11.1. | accepted (`review-Q-4.md`) |
| `s2/p-2` | Card P-2: `typed_nodes` and `provider_deliveries` in Postgres, the delivery worker (per-node and per-tenant order, backoff, "never twice") and the reconciler; three Postgres mutations. `purged` and `tenant_deleted` are retried until they succeed. | rejected for one finding (a `purged` delivery ended on a `4xx`), fixed on the branch (`review-P-2.md`) |

Fixes made during the afternoon integration:

- `f/ux-c8-c11` on `w/fixes`: `FilesPage.tsx`, `uploads.tsx` and `useUploads.ts` conflicted.
  Kept from `w/fixes`: the `name_taken` round trip (`askAgain`, `onNameTaken`, `nameKey`) and
  the fresh head before a new version (`refreshNode`). Kept from C-8: no coalescing UI (no
  `recent`, no version name in the conflict dialog, no "replaced" toast) and
  `newVersionTarget` with `coalesce: false`.
- `c/c1-acceptance`: `tools.json` regenerated (`gen-tools.mjs`) and `schema.d.ts` checked
  (`npm run gen:api`, no change) after the rebase onto the C-8 contract.
- `web/e2e/sharing.e2e.ts`: two tests open a folder's details by keyboard after an upload.
  The uploads panel and the upload toast covered the Details button when the random folder name
  sorted near the bottom; "a person signed in with another address ..." failed in 5 of 7 CI
  runs. No assertion changed (commit on `s2/p-2`).
- The mutation rule of review-Q-4 note 1 (`privacy-tenant-keys-tenant-filter-postgres`) is in
  `crates/ruum-testkit/mutations/` (review-P-2 finding 9).
- CI: the unit job of `integ/s2-e-2` and `integ/s2-p-2` failed once each while downloading the
  PGDG key (`curl: (56)`); the re-run passed. The browser job failed once on
  the C-8 test of `versions.e2e.ts` ("Upload new version" always makes a new version: the
  older version downloaded the newer content) and once on the visual `pseudo-locale › share` (2 % of pixels);
  each re-run passed.
- The rebased branches were pushed as `integ/f-ux-c8-c11`, `integ/c-c1-acceptance`,
  `integ/s2-e-2`, `integ/s2-q-4` and `integ/s2-p-2` (no force push) and deleted from GitHub
  after the fast-forward (each is part of `main`). The old branch names on GitHub keep the
  pre-rebase commits. `w/fixes` needed no rebase.

Evening integration (run on 2026-11-08, each branch rebased onto `main`, checked on the build
server with `cargo test --workspace`, both suite backends with `RUUM_REQUIRE_IMPLEMENTED=1` and,
after `f/buildbox-browser`, `scripts/e2e_browser.sh`, pushed and green in GitHub CI, then
fast-forwarded, in this order; linear history, no merge commit, no co-author line):

| Branch | What it adds | Review |
|---|---|---|
| `f/buildbox-browser` | The build server gets Node.js 22 (pinned, SHA-256 checked) and Playwright's Chromium; `run.sh` gives each agent a dev root (`RUUM_DEV_ROOT`, `RUUM_DEV_SH`) with its own Postgres and SpiceDB and free ports for both e2e scripts; `setup.sh --fresh-platform-db`. | no panel review file (tooling) |
| `f/sign-in-providers` | `scripts/zitadel_idps.sh` (`dev.sh idps [check]`, run by `dev.sh up`): Google and Microsoft (tenant `common`) in the laptop Zitadel when their keys are present, and a mock OIDC provider (`dev.sh up --sso-mock`, compose profile `sso-mock`); `web/e2e/sso.e2e.ts` signs in through the mock from start to end, and the CI browser job requires it; the status of the sending domain in `docs/setup/ACCOUNTS.md`. | no panel review file |
| `f/rr-fix-e2` | Every `ruum_dbhost` session has `search_path = pg_catalog, pg_temp`: before, a tenant operator in `public` (`oid = integer`, `name !~ name`) ran as `ruum_dbhost` in hand-over steps 1 and 3 and in every lens rebuild (found by checking the question of `re-review-e2.md`). Hostile tests, mutation `dbengine-dbhost-search-path`; the nightly mutation job starts `postgres-data`. | no review of the fix yet (`re-review-e2.md` was not complete) |
| `s2/d-1` | Card D-1: `createDatabase`, `getDatabase` and `updateDatabaseManifest` on the engines (the three `planned` marks removed); contract C-21 (`Limits.databases`, `Usage.databases`); `Services` wires the engines, the tenant keys, the delivery worker (each round in its own task) and the reconciler; the fixture database `<prefix>-garden`; `listVersions` of a database node lists its manifest versions; a stale manifest check is `409`; a database in the trash shows `suspended` (`in_trash`); four mutations. New dependencies in `ruum-core`: `jsonschema` (workspace) and the workspace crate `ruum-dbengine`. | changes requested (B-1 `listVersions`, B-2 a T-1 case), B-1 fixed, accepted by the second review once B-2 is fixed and the panel names the wave exception for `crates/ruum-api/src/routes/versions.rs` (`review-D-1.md`, `review-D-1-fix.md`, `review-D-1-2.md`) |
| `s2/t-1` | Card T-1: 42 acceptance cases of the 24 database operations (`tests/databases_*.rs`), 12 isolation probes, the meta checks (`databases_meta`). The 11 D-1 cases pass on memory and Postgres; the other 31 stay planned or wait for a card. | accepted (`review-T-1.md`) |
| `s2/t-2` | Card T-2: the privacy suite (`src/privacy.rs`, `tests/privacy.rs`, 56 cases, 99 runs, all planned until the operations and the privacy filters land; 24 planned mutations). | changes requested (R-1 to R-3), fixed, accepted by the second review (`review-T-2.md`, `review-T-2-fix.md`, `review-T-2-2.md`) |

Fixes made during the evening integration:

- `s2/t-1`, review-D-1 B-2: the case `guests_create_a_database_only_with_the_policy` gave
  `guest_view` a second grant on Projects (it already has `view` there), and step 1 answers
  `409`. The setup now makes a folder in Projects as the owner and grants `manage` there; the
  databases go into that folder, and `createNode` is in the case's operation list. No assertion
  changed. This is the fix that `review-D-1-2.md` proved in a scratch copy. D-1 was
  fast-forwarded after the rebased T-1 passed the 11 d1 cases on memory and Postgres, in CI and
  on the build server.
- The wave exception of review D-1 B-1 (`crates/ruum-api/src/routes/versions.rs`, about 15
  lines of dispatch, outside D-1's "May change") is taken as both D-1 reviews recommend. It is
  `provisional`: no separate panel record names it.
- CI (review-T-1 N-1, review-T-2 N-1): the memory and Postgres suite jobs also run
  `databases_d1`, `_guests`, `_agents`, `_nodes`, `_statements` and `privacy`; the unit job runs
  `databases_meta`. `docs/TESTING.md` has a section on the T-1 cases (review-T-2 N-6).
- `main` got documentation commits from outside this run during the integration. `s2/t-2`
  was rebased once more onto the first two (decision 0004-36, design 9.10); CI on the rebased
  commit is green. The final checks ran on `b4a63e8`; the commits after it change only
  `docs/DESIGN.md` and `docs/ux/copy.md`.
- On the build server, the four `ruum-dbengine` export tests need a PostgreSQL 17 `pg_dump`
  (review-D-1 N-7: the server has 16). The checks set `RUUM_PG_DUMP` to a wrapper that runs the
  `pg_dump` of the `ruum-dev/postgres-data:17` image, as CI sets its own 17 client.
  `f/followups-4`: `setup.sh` now installs the PostgreSQL 17 client (PGDG), and the server has
  it, so a plain run passes without `RUUM_PG_DUMP`.
- The rebased branches were pushed as `integ/f-sign-in-providers`, `integ/f-rr-fix-e2`,
  `integ/s2-d-1`, `integ/s2-t-1`, `integ/s2-t-2` and `integ/s2-t-2-r` (no force push) and
  deleted from GitHub after the fast-forward. `f/buildbox-browser` needed no rebase.

Night integration (2026-11-08, run into 2026-11-09; each branch rebased onto `main`, checked on
the build server with `cargo test --workspace` and both suite backends with
`RUUM_REQUIRE_IMPLEMENTED=1` and `scripts/e2e_browser.sh`, pushed as `integ/...` and green in
GitHub CI, then fast-forwarded, in this order; linear history, no merge commit, no co-author
line):

| Branch | What it adds | Review |
|---|---|---|
| `f/followups-4` | re-review-P-2 F-1 to F-3: a delete-hook delivery whose payload does not read is retried (by the hook column), a provider panic is `500 internal` with the backoff and no longer holds a delivery, `DeliveryWorker::run` survives a panic, two mutations; the flaky log tests of Q-4 (a child process and a global test subscriber); `setup.sh` installs the PostgreSQL 17 client. | no panel review file (follow-ups of reviewed findings) |
| `w/voice` | The English source text and the invitation and share emails in the voice of `docs/ux/copy.md`; `web/src/locales/context/en.json` (description, tone and length budget of each of the 834 messages); `docs/ux/glossary.md`, `docs/ux/style-guides/et.md`, `docs/ux/i18n.md`; the translation check in `npm run lint` (`web/scripts/check-translations.mjs`: ICU, keys, placeholders, plurals, glossary, a status with the hash of the English text); Estonian translated anew (all `machine`); visual references of the new copy. | no panel review file; the UX review panel has not looked at it |
| `s2/k-1` | Card K-1: `listDatabaseColumns`, `suggestColumnClasses`, `confirmColumnClasses` (three `planned` marks removed, `database.columns_confirmed` written), the catalog sync (on read and every 10 minutes), the rule-based suggester, the memory and Postgres column stores with one conformance suite, five mutations; gate G-1 (the fixture table `plants`); a `user_required` hint that starts with `<public_url>/t/` is kept (C-16). | accepted with conditions C-1 to C-3 (`review-K-1.md`); C-3 done at the merge; one review only |
| `s2/a-1` | Card A-1: `createDatabaseAccessGrant`, `listDatabaseAccessGrants`, `revokeDatabaseAccessGrant` (three marks removed, `database.grant_created` and `database.grant_revoked` written), the tuples, the grant sweeper, the restore point before `write_live`, the fixture grant (`read_personal` for 8 hours); writes of one tuple serialized on Postgres by an advisory lock; six mutations. | changes requested (R-1 to R-3), fixed, accepted by the second review (`review-A-1.md`, `review-A-1-fix.md`, `review-A-1-2.md`) |

Not merged:

- `s2/q-1` (card Q-1): the second review requested changes (`review-Q-1-2.md` S-1:
  `set_config` runs through `query_to_xml` and `ts_stat`). The fix is an engine revoke outside
  Q-1's files (next cards). The branch and its worktree `ruum-server-wt/s2q1` stay.
- `f/dbhost-review`: no commit past `main`, and the second review did not accept
  (`review-dbhost-2.md`: a confirmed hole, item 2). The worktree `ruum-server-wt/secfix` stays.

Fixes made during the night integration:

- The rebase of A-1 onto K-1 joined the shared lines (fields `columns`, `masking`, `grants` in
  that order; both spawn lines of `main.rs`; MCP tool count 57; both event payloads in
  `routes/changes/tests.rs`; the envelope exemptions in `journal/mod.rs`; the fixture comment)
  and generated `tools.json` again. Five `DatabaseParts` literals in tests that git merged
  without a conflict lacked the other card's fields; CI found them on `integ/s2-a-1`, and the
  fix went into A-1's implementation commit (`integ/s2-a-1-r`).
- CI: both suite jobs run `databases_classes_api` and `databases_grants_api` (review-K-1 C-3,
  review-A-1 N-6).
- `docs/STEP2.md`: the route files of Q-1, K-1, A-1 and A-2 are
  `crates/ruum-api/src/routes/databases/{query,classes,access}.rs` (wave decision 4).
- The K-1 wave exceptions that review-K-1 confirms (`ruum-api/src/error.rs`, the four places a
  new `EventType` touches, the `DatabaseParts` test literals) are taken as `provisional`.
- On the build server, three cold workspace builds at once ran out of memory (the linker was
  killed). The checks then ran two at a time with `CARGO_BUILD_JOBS=8` and
  `CARGO_PROFILE_DEV_DEBUG=line-tables-only`, each with its own data cluster (`pgdata-<key>`,
  `RUUM_DBENGINE_REQUIRE_POSTGRES=1`).
- The rebased branches were pushed as `integ/f-followups-4`, `integ/w-voice`, `integ/s2-k-1`,
  `integ/s2-a-1` and `integ/s2-a-1-r` (no force push) and deleted from GitHub after the
  fast-forward.

Integration of 2026-11-09 evening (run 6, done on 2026-11-10; each branch rebased onto `main`,
checked on the build server with `cargo test --workspace`, both suite backends with
`RUUM_REQUIRE_IMPLEMENTED=1` and `scripts/e2e_browser.sh`, pushed as `integ6/...-2` and green in
GitHub CI, then fast-forwarded, in this order; linear history, no merge commit, no co-author
line):

| Branch | What it adds | Review |
|---|---|---|
| `w/c8-race` | Backlog 2026-11-09 T-13: the versions panel reads the node again with the first page of versions, so "Current", the restore Undo and the compare dialog use the real head right after an upload; `usePagedApi` keeps the first page (additive); a flow test that holds the children reload; the C-8 browser test asserts the first row is the one "Current" row. | accepted (`review-c8-r6.md`; notes: the Vitest flake T-19, the Chromium network flake T-18) |
| `f/pgdata-per-agent` | Backlog 2026-11-08 B-2: every `run.sh` run of a tree with `deploy/dev/postgres-data` builds `ruum-buildbox-pgdata:<key>` from the tree's own folder and starts `pgdata-<key>` (its own port, `RUUM_DBHOST_URL`, `DBHOST_CHECK_CONTAINER`, `RUUM_DBENGINE_REQUIRE_POSTGRES=1`); a changed folder gives a new image and an empty cluster; `--stop` removes container, volume and image. | accepted (`review-dc-r6.md`, notes N-1 to N-6) |
| `f/k1c2-errors` | Backlog 2026-11-08 P-2 (K-1 C-2), P-3 and 2026-11-09 S-12: a name that does not fit the class store gets no row and counts as personal; the nine step 2 error codes in `ErrorCode`, `sqlstate` and `Retry-After` in `ApiError`, Q-1's `Problem` is `ApiError`; query rule 10 refuses writes to and schema changes of catalog relations; a data-modifying `WITH` is a write. One additive contract change (the rule in the `queryDatabase` text). | accepted (`review-srv-r6.md`, notes N-1 to N-5) |
| `w/brand-directions` | Three brand directions for the person (`docs/ux/brand/directions.html`, `palettes.mjs`, `build.mjs`, `README.md`): Paper and spruce, Marker, Graphite; 252 contrast pairs pass. The person chose Graphite (decision 0005 B-5) before the merge. | docs only, no panel review file |

Notes of this integration:

- `main` moved during the run (the voice panel integration, three commits), so the stack was
  rebased twice. The first rebase was pushed as `integ6/c8-race`, `integ6/pgdata-per-agent`,
  `integ6/k1c2-errors` and `integ6/brand-directions`; the second, which is on `main`, as the
  same names with `-2`. The Rust, contract, migration and deploy trees of the two rebases are
  the same, so the Rust checks of the first rebase hold; the browser tests ran again on each
  tip of the second.
- All checks used the new per-agent data cluster (`f/pgdata-per-agent`'s `run.sh`).
- The browser test `oauth.e2e.ts` failed in 2 of 4 full runs of the first rebase (`page.goto`
  after the app's callback: `net::ERR_ABORTED`) and passed alone and on the second rebase
  (backlog 2026-11-09 T-20). One more full run on `w/brand-directions` failed once (output
  trimmed) and passed on the re-run. CI was green on all eight branches.

Integration of 2026-11-09 (each branch rebased onto `main`, checked on the build server with
`cargo test --workspace`, both suite backends with `RUUM_REQUIRE_IMPLEMENTED=1` and
`scripts/e2e_browser.sh`, pushed as `integ2/...` and green in GitHub CI, then fast-forwarded, in
this order; linear history, no merge commit, no co-author line):

| Branch | What it adds | Review |
|---|---|---|
| `d/rusqlite-0.41` | RUSTSEC-2026-0459 (SQLite before 3.54 bundled in `libsqlite3-sys` 0.37, CVE-2026-119375): `rusqlite` 0.41.1 with `libsqlite3-sys` 0.39.1 (SQLite 3.54.1). `sqlx-sqlite` 0.9.0, which the lock holds through sqlx but Ruum never builds, asks for `libsqlite3-sys` < 0.38, and Cargo allows one crate that links `sqlite3`; a `[patch.crates-io]` puts an empty stand-in with the same name, version and feature names in its place (`third_party/sqlx-sqlite-stub`, excluded from the workspace). Remove it when a released sqlx accepts `libsqlite3-sys` 0.39. API changes: `Connection::authorizer` takes `&mut self`; `Statement::columns` and `columns_with_metadata` return iterators. | no panel review (a dependency fix); `cargo deny check` passes |
| `f/masking-hardening` | Decision 0005 B-2 and 0004-37 (backlog S-1, S-4, B-12): tenant-defined types never reach a `ruum_mask` definer function (only built-in base types are converted inside one; others are masked as `secret`); every lens build drops and makes the three lens schemas, the functions and the key table of `mask` again (the key is kept), after a `pg_depend` check; `restore_in_place` rebuilds the lens of the restored copy before the swap; `mask` holds `CREATE` only inside the build transaction; hostile-copy and tenant-type tests; new mutations. | review 1 accepted with notes F1 and F2 (`review-masking-1.md`); review 2 did not accept (`review-masking-2.md` F-1: the state from before the fix survived a build and `restore_in_place` brought it back); fixed (`masking-fix-2.md`); review 3 accepted (`review-masking-3.md`) |
| `f/untrusted-agent-text` | Decision 0005 B-3 (backlog S-6): 52 marks `x-ruum-content-trust: untrusted` in `contracts/openapi.yaml` on fields that hold text an agent can write and others read, and a section "Untrusted text" in the API description; `untrusted_output` on 62 of 70 MCP tools, whose descriptions say to treat the text as data, never as instructions; the same rule once in the MCP server instructions; `crates/ruum-mcp/tests/content_trust.rs`. No wire change. | no panel review file |
| `w/et-ruum` | Decision 0005 B-1 in the Estonian UI: rule 9 of `web/scripts/check-translations.mjs` (no Estonian sentence starts with the word "ruum"; no more capital "Ruum" than the English has "Ruum"; a warning when both stand side by side); 12 Estonian messages rephrased; a new key `shared.space`; `docs/ux/style-guides/et.md` section 2, the glossary and `i18n.md`. | no panel review file; the Estonian is still `machine` |
| `s2/q-1` (head `s2/q-1-fix2`) | Card Q-1: `queryDatabase`, `getDatabaseSchema` and `listDatabaseQueries` for people (three `planned` marks removed): the statement rules (`sqlparser` 0.63.0), read-only mode, the role kind per caller, plan limits, the query log; agents fail closed until Q-2. Fix 2 (backlog S-2, S-3): the data cluster set-up revokes `pg_catalog.set_config` from `PUBLIC` in `template1` and in each data database, the engine refuses a database where `PUBLIC` can still run it (`check_set_config`), and only the owner kind runs `set_config`; the names middleware leaves the rows of a query result alone; Unicode-escaped names are refused. CI's suite jobs run `databases_query_api` (review-Q-1 R-5). | review 1 and 2 requested changes (`review-Q-1.md`, `review-Q-1-2.md` S-1); fixed twice; review 3 accepted (`review-Q-1-3.md`, notes N-A and N-B) |

Fixes made during this integration:

- The first try of this integration (keys `integ-a`, `integ-b`, `integ-c`) stopped: SSH to the
  build server timed out after a change of the laptop's IP, and RUSTSEC-2026-0459 failed
  `cargo-deny` on every branch, `main` included. The advisory fix landed first; the stack was
  rebased onto it once more.
- At the rebase of Q-1 onto the masking fix (first try, kept): `tools.json` generated again;
  step 3 of a hand-over in `copy.rs` runs `setup::rebuild_lens`, then `setup::check_set_config`;
  the hit marker of the masking tests (`public.mark`) called `set_config`, which the masked
  reader and `mask` may no longer run, so `mark` hands `path:current_user` to `public.hit`, a
  `SECURITY DEFINER` function of the owner kind. At the rebase onto the advisory fix: one
  `Cargo.lock` conflict (Q-1's `sqlparser` next to the removed `sqlite-wasm-rs`).
- The build server's shared data cluster runs the set-up of Q-1 now (`git pull` of the base
  copy to the Q-1 tip, `scripts/dev.sh up`; `set_config` in `template1` has no `PUBLIC`
  right). Every laptop stack needs `scripts/dev.sh up` too, or the engine refuses every
  database with a message that names the set-up.
- `cargo-deny` 0.20.2 is installed on the build server (`~/.cargo/bin`); `setup.sh` does not
  install it yet (backlog B-16).
- The rebased branches were pushed as `integ2/masking-hardening`, `integ2/untrusted-agent-text`,
  `integ2/et-ruum` and `integ2/s2-q-1` (no force push), and `d/rusqlite-0.41`.

The digest for the person is `docs/digests/2026-11-09-evening.md` (earlier: `docs/digests/2026-11-09.md`, `docs/digests/2026-11-08.md`, `docs/digests/2026-11-07-evening.md`,
`docs/digests/2026-11-07-afternoon.md`, `docs/digests/2026-11-07.md`, `docs/digests/2026-11-06-night.md`, `docs/digests/2026-11-06.md`,
`docs/digests/2026-11-06-evening.md`). The worktrees of the merged branches are removed; the
branches stay.

Fixes made during the `q/uxfix` integration:

- Two flow tests (`shell.test.tsx` "starts a team space from the space switcher",
  `invitation.test.tsx` "joins the space with one click") read the space switcher before React
  Router committed the new location (a transition) and failed about one full run in three. The
  same assertions now run in `waitFor`.
- Sign out raced a sign-in redirect: the server ends the session first, a call still under way
  answered `401`, and the `401` listener sent the browser to `/signin?return_to=...` while
  `browser.assign(logout_url)` started. The e2e test "sign out ends the session" failed with
  `net::ERR_ABORTED` (3 of 8 repeats). `api/client.ts` now skips the sign-in listeners while the
  person signs out (`setSigningOut`); a `401` from logout lands on `/signin`. Two flow tests;
  the e2e test waits for the end of the chain (`/signin`).

Fixes made during the integration:

- `scripts/dev.sh env` printed `RUUM_DAV_BIND=127.0.0.1:` (no port): the heredoc used a
  variable that was set inside the heredoc. It now prints `127.0.0.1:8492` (from
  `RUUM_DAV_UPSTREAM`).
- `scripts/e2e_browser.sh` gives the WebDAV listener its own port (`RUUM_E2E_BROWSER_DAV_PORT`,
  default 8496). It inherited `RUUM_DAV_BIND` from `dev.sh env`, the same port as the server.
- The Postgres suite backend (`crates/ruum-testkit/src/backend.rs`) wires `PgFreshness`, as
  `ruum-server` does since decision 0002-4. The seed (`fixtures::seed`, `seed_bridge`) and the
  trash step of the trashed file suite run with a request freshness floor;
  `authz::in_current_request` carries the floor into the tasks that the seed spawns. Without
  this, worlds that run at the same time on one SpiceDB engine overwrote the process token, and
  the SpiceDB run failed 11 to 19 trashed checks and 45 matrix checks (setup steps answered
  `404`).
- `web/e2e`: two timing races. `files.e2e.ts` matched any `status` element while the folder
  showed `Loading...`; `auth.setup.e2e.ts` navigated while the sign-out redirect chain still ran.

All 93 step 1 operations of `contracts/openapi.yaml` are built. Of the 24 step 2 database
operations, 12 are built (cards D-1, K-1, A-1 and Q-1); the other 12 are marked `x-ruum-status:
planned` (decision 0004-21).

| Area | What works |
|---|---|
| 1. Nodes, tags, search | The tree (create, get, by path, children, rename, move, copy, trash, restore, purge, empty trash), shortcut and member links, tags, name search with the trigram index. Memory and Postgres stores, one conformance suite. |
| 2. Blobs, versions, diff | Chunked uploads (FastCDC parameter sets, BLAKE3), downloads with `Range`, `HEAD` and `If-None-Match`, signed content URLs, `putContent`, versions (coalescing, thinning, retention, labels, restore), diff plugins (text, JSON, XML, image, binary), chunk garbage collection. Memory and Postgres stores, chunks in memory or S3 (Garage). |
| 3. Authz, sharing | `authz::require`, grants, share links, `updateNodeSharing`, Shared with me. In-memory engine and SpiceDB, the outbox. |
| 4. Identity, tenants, agents | Sign-in with Zitadel (PKCE), sessions with CSRF, tenants, members, invitations, plans and limits, agents and API keys, token exchange, OAuth 2.1 for MCP clients, app passwords, undo, the sweeper. |
| 5. Journal, audit | The changes feed (cursor, long poll, resync markers), the audit log (JSON and NDJSON). |
| 6. MCP, CLI | `/mcp` (stateless streamable HTTP, OAuth-protected, one tool per entry of `contracts/mcp/tools.yaml`). The `ruum` CLI with an agent API key. |
| 7. WebDAV | Its own host (`https://dav.ruum.localhost/`, listener `RUUM_DAV_BIND`, default `127.0.0.1:8492`; decision 0002-3): PROPFIND, GET with ranges, PUT (a version), MKCOL, MOVE, COPY, DELETE (to the trash), LOCK, quota properties. HTTP Basic with an app password, or an agent token. |
| 8. Web UI | All step 1 pages, chunked resumable uploads in the browser, diff view, previews through the content domain. |

### Links between the areas (integration)

- `Services::in_memory`: the node store is the content store's `versions::HeadUpdate` (a new
  version sets `head_version` and `size_bytes`), and the content store is the node store's
  `nodes::ContentCopy` (a copied file gets one version with the source's head, shared chunks,
  its own `version.created`; a purge forgets the versions).
- Postgres: `copyNode` calls `blobs::postgres::copy_head` in its own transaction, after the
  node entries.
- `ruum-server` wiring (`crates/ruum-server/src/wiring.rs`): without `DATABASE_URL` everything
  is in memory. With it, every store is Postgres, the authz engine is SpiceDB (when a preshared
  key is set) with the outbox worker, chunks go to S3/Garage (`RUUM_S3_*` or `S3_*`), and
  sign-in uses Zitadel (`RUUM_OIDC_ISSUER`, `RUUM_OIDC_CLIENT_ID`). The log says
  `authz engine: SpiceDB` and `stores: Postgres`.

## Security findings and fixes (2026-11-06)

| Finding | Severity | Fix | Regression test |
|---|---|---|---|
| WebDAV `GET` served user files on the platform origin (`/dav` shares the origin of the web UI and `/v1`) with the stored type (`text/html`, `image/svg+xml`) and no CSP, `nosniff` or `Content-Disposition`. A script in an uploaded file ran as the platform (session riding with the readable `ruum_csrf` cookie). Reported twice. | must fix | `ruum-webdav` `content_safety`: every `/dav` answer carries `Content-Security-Policy` = `blobs::signed::CONTENT_CSP` (`sandbox; ...`) and `X-Content-Type-Options: nosniff`; a successful `GET`/`HEAD` also carries `Content-Disposition: attachment` with the file name, and every type that is not passive (`blobs::signed::is_passive_content_type`: plain text, raster images, audio, video, PDF, JSON, archives) goes out as `application/octet-stream`. `CONTENT_CSP` and `disposition_header` moved from `routes/blobs.rs` to `ruum_core::blobs::signed`, so REST and WebDAV share them. | `crates/ruum-webdav/tests/dav.rs` `files_are_served_as_sandboxed_attachments` (`.html`, `.svg`, `.xhtml`, `.xml`, `.js`; `GET`, `HEAD`, range); mutation `webdav-content-safety` |
| Content and version reads ignored the trash rule: a viewer or editor (not `manage`) of a trashed file still downloaded it, got a content URL and listed its versions, although `getNode` answered `404`. The changes feed had the same gap. | should fix | `blobs::logic::require_trashed_read`: `open_content`, `content_grant`, `list_filtered` and `get` of versions (Postgres and memory; `diffVersions` goes through them and `NodeStore::get`) need `manage` on a trashed file and answer `404` otherwise. The feed's `ServicesAccess::can_view` uses `NodeStore::get`, so entries on a trashed node reach only those who may restore it. A `node.trashed` or `node.purged` entry, and any entry on a node in the trash or purged, reaches only those who could see the node just before the trash or could restore it; access that began after the trash (also a membership) never reveals it (decision 0002-6). | `ruum-testkit` `trashed_file_reads` (`permissions::run_trashed`: every principal, `downloadContent`, `getContentUrl`, `listVersions`, `getVersion`, `diffVersions`, `listChanges` after the plan is trashed; 78 checks); the content conformance suite (memory with a node store that does not check the trash, and Postgres); mutations `blobs-trashed-read-postgres`, `blobs-trashed-read-memory`, `feed-trashed-node-visible` |
| A guest without grants registered agents (`registerAgent`, `oauthConsent` with `new_agent`) that counted against the host tenant's agent limit, so a guest could block owners and members (`plan_limit_reached`). | should fix | A guest's agents are counted apart: each guest may own `agents::guest_agent_limit(plan)` live agents (one member's allowance), and the tenant's limit counts only the agents of owners, admins and members (`agents::postgres::MEMBER_AGENT`). `getLimits` usage counts the same agents. Guests may still register agents, as the permission matrix assumes. | `agents::tests::guests_cannot_use_up_the_tenant_agent_limit` (memory and Postgres); mutations `agents-guest-uses-tenant-limit`, `agents-guest-uses-tenant-limit-postgres` |

## Panel decisions (2026-11-06, decision 0002, `provisional`)

| Question | Decision | Tests |
|---|---|---|
| 1. `addGrant` by user ID | Members and guests of the tenant only; any other ID is `404`, no guest is made. Contract text fixed. With `allow_guests: false` the in-memory store now accepts an email grant to a member's verified email, as Postgres does. | `sharing::conformance` (memory, Postgres); mutation `sharing-email-grant-policy-memory` |
| 2. Guests and agents | Guests may own agents (own limit), but register none while `allow_guests` is false (`registerAgent`, `oauthConsent` with `new_agent`: `403`). | `agents::tests::guests_register_no_agents_while_guests_are_off`; mutations `agents-guest-register-policy`, `agents-guest-register-policy-postgres` |
| 3. WebDAV host | WebDAV moved to its own host and listener; the platform origin answers `404` for `/dav`; the WebDAV host has no API, never reads or sets cookies, keeps the content headers. `createAppPassword` returns `dav_url`. The CSRF value is bound to the session (cookie tossing). Cookie names stay (`__Host-` later). | `app::tests::*`, `dav.rs` `the_dav_host_*`, `the_csrf_value_is_bound_to_the_session`, `a_new_app_password_names_the_dav_host`; mutation `csrf-not-session-bound`; e2e |
| 4. SpiceDB freshness | Every tenant request reads at least as fresh as the tenant's newest outbox token (`PgFreshness`, under RLS); a lookup error fails the request. | `wiring::tests::writes_of_another_server_hold_at_once` (two servers, SpiceDB), `freshness_reads_the_tenant_token_under_rls`, `a_failed_freshness_lookup_fails_the_request`; mutations `authz-ignores-tenant-token`, `authz-spicedb-ignores-request-floor`; e2e without the 6 s wait |
| 5. Matrix rules | Every "assumption" and "open question" rule decided; `logout` and `listAgents` for agents: `any` becomes `ok`. Contract wording for these operations. | permission matrix (1209 checks) |
| 6. The trash in the changes feed | A trash or purge entry, and any entry on a node in the trash or purged, reaches a subject only when it could see the node just before the trash, or could restore it (`manage`, capped by an agent's scope) while it was in the trash. Access that began after the trash never counts; memberships are dated by `memberships.updated_at` (`TenantStore::member_since`). | `journal::feed::tests`, journal and tenant conformance (memory and Postgres), the feed property test; mutations `feed-gone-node-old-place`, `feed-gone-node-ancestor-now`, `feed-member-joined-after-purge` |

## How to run it

The cargo settings of this repository (one shared cache):

```sh
export CARGO_TARGET_DIR=$PWD/target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=false CARGO_PROFILE_TEST_DEBUG=false
```

### Stack and server

```sh
scripts/dev.sh up                 # Postgres, Garage, Zitadel, SpiceDB, ...; see docs/DEVSTACK.md
scripts/dev.sh status             # all services healthy
eval "$(scripts/dev.sh env)"
cargo run -p ruum-server          # http://127.0.0.1:8490 and https://ruum.localhost (Caddy);
                                  # WebDAV: http://127.0.0.1:8492 and https://dav.ruum.localhost
```

Set these too, or tokens and content URLs end at each restart:

- `RUUM_TOKEN_SIGNING_KEY`: base64url of 32 random bytes
  (`openssl rand 32 | openssl base64 -A | tr '+/' '-_' | tr -d '='`).
- `RUUM_CONTENT_SIGNING_KEY`: base64 of 32 random bytes (`openssl rand -base64 32`).

Other settings: `RUUM_REQUEST_TIMEOUT_SECONDS` (default 120), `RUUM_DAV_BIND` (the WebDAV
listener, default `127.0.0.1:8492`), `RUUM_DAV_URL` (the public URL of the WebDAV host, default
`https://dav.<host>/` for an https public URL, else `http://<RUUM_DAV_BIND>/`),
`RUUM_DAV_SPOOL_DIR` (WebDAV upload spool), `RUUM_PLAN_LIMITS`, `SMTP_URL`, `RUUM_MAIL_FROM`.

### Test users without a browser

```sh
target/debug/ruum-server seed-dev /tmp/seed.env   # same env as the server, same signing key
source /tmp/seed.env
```

It refuses to run unless `RUUM_PUBLIC_URL` is a loopback or `.localhost` origin. It makes a team
tenant with Alice (owner), Bob (a guest without grants), Carol (her own tenant), an agent of
Alice with an API key, and an app password of Alice. The file holds 1-hour user tokens
(`ALICE_TOKEN`, ...), `AGENT_KEY`, `APP_PASSWORD`, `TENANT_A`, `TENANT_B` and the user IDs. The
server sees the seed's relationships at once: each request reads at least as fresh as its
tenant's newest outbox token (decision 0002-4).

### End-to-end smoke test

```sh
scripts/e2e_smoke.sh              # the stack must be up; RUUM_E2E_PORT (default 8491),
                                  # RUUM_E2E_DAV_PORT (default 8493), RUUM_DEV_SH (for worktrees)
```

It builds and starts the server against the stack, seeds, and checks 59 steps: CLI login,
folders, a 3 MB upload in 73 chunks and a download with the same SHA-256, a named version and a
second version, a copy, a share to Bob and his access, Shared with me, the changes feed, MCP
(`initialize`, `tools/list`, `tools/call list_children`), WebDAV on its own host (PROPFIND, PUT,
GET, `dav_url`, no cookie, no API there, `404` for `/dav` on the platform origin) and tenant
isolation (Carol gets `404`). There is no wait after the seed. Last run on 2026-11-06 (branch
`q/decisions`): `PASSED: 52 checks`.

### CLI

```sh
cargo build -p ruum-cli
target/debug/ruum --url http://127.0.0.1:8490 login --key ruum_agent_...
target/debug/ruum mkdir -p /Projects/Garden
target/debug/ruum upload plan.md /Projects/Garden/
target/debug/ruum ls /Projects/Garden
target/debug/ruum versions /Projects/Garden/plan.md
target/debug/ruum share /Projects --user <user id> --role view
target/debug/ruum changes
```

`--json` prints the API's JSON. The credentials are in `~/.config/ruum/credentials.json`.

### MCP

`POST /mcp` with `Authorization: Bearer <agent access token>` and
`Accept: application/json, text/event-stream`. Without a token it answers `401` with
`WWW-Authenticate: Bearer resource_metadata=".../.well-known/oauth-protected-resource"`, so MCP
clients run the OAuth flow (dynamic registration, consent in the web UI). For scripts: exchange
an agent API key at `POST /v1/oauth/token` (`grant_type=urn:ietf:params:oauth:grant-type:token-exchange`,
`subject_token_type=urn:ruum:params:oauth:token-type:api-key`). A tool's request body goes in the
argument `body` (see `crates/ruum-mcp/src/tools.json`).

### WebDAV

`https://dav.ruum.localhost/` or `http://127.0.0.1:8492/` (its own host, decision 0002-3), HTTP
Basic with the user's email and an app password (`POST /v1/me/app-passwords`, one tenant; the
answer's `dav_url` is the URL to mount). Agents send their access token as Bearer. The platform
origin has no `/dav`. `crates/ruum-webdav/scripts/smoke.sh` has Finder-like curl requests.

### Web UI

```sh
cd web
npm ci
npm run dev                       # mock API, no server needed
npm run dev:server                # proxies the API to the server on 8490 (see web/README.md)
```

For Zitadel sign-in through Vite, start the server with
`RUUM_OIDC_REDIRECT_URI=http://localhost:5173/auth/callback`.

## Checks (2026-11-10, `main` after the integration of 2026-11-09 evening, on the build server)

`main` is `4b15efc` before this documentation commit. The checks ran with
`scripts/buildbox/run.sh` of `main`, which now gives each key its own data cluster
(`pgdata-<key>`, `RUUM_DBENGINE_REQUIRE_POSTGRES=1`):

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`: pass.
- `cargo test --workspace`: 734 passed, 0 failed, 2 ignored (724 before; the new tests of
  `f/k1c2-errors`).
- `RUUM_REQUIRE_IMPLEMENTED=1 cargo test -p ruum-testkit` with `RUUM_SUITE_BACKEND=memory`:
  tenant isolation 354 checks (314 passed, 40 planned), permission matrix 1936 checks (1744
  passed, 192 planned), trashed file suite 96/96, database cases d1 11/11, grants 7/7, agents
  13, guests 3 and statements 7 planned, nodes 5 planned and 3 pending (card P-3), privacy
  suite 99 planned; 0 pending (501), 0 failed. With `postgres`: pass, 0 failed (the summary
  lines were cut from the log).
- `MUTATION_STRICT=1 crates/ruum-testkit/scripts/mutation_check.sh`: 90 of 90 mutations killed, none survived, skipped or broken (88 before; new: `databases-classes-long-name-stored` and `databases-query-catalog-write-allowed`), 2 h 48 min with another agent's check on the server.
- `scripts/e2e_smoke.sh`: `PASSED: 59 checks`.
- `scripts/e2e_browser.sh`: 172 passed, 1 skipped (`sso.e2e.ts`), on each of the four tips of
  the second rebase; `oauth.e2e.ts` is flaky on the build server (backlog 2026-11-09 T-20).
- GitHub CI: green on `integ6/c8-race`, `integ6/pgdata-per-agent`, `integ6/k1c2-errors`,
  `integ6/brand-directions` and on the same names with `-2` (the tips on `main`).

### Checks of the integration of 2026-11-09 (earlier)


`main` is `24d192b` before this documentation commit; it is the tree of `integ2/s2-q-1`. The
checks ran with `scripts/buildbox/run.sh <key> <worktree> -- ...` (own Postgres and SpiceDB)
and a data cluster of their own (`pgdata-<key>` from the tree's `deploy/dev/postgres-data`, in
`RUUM_DBHOST_URL`, with `RUUM_DBENGINE_REQUIRE_POSTGRES=1` and
`RUUM_PG_DUMP=/usr/lib/postgresql/17/bin/pg_dump`), detached on the server (`setsid nohup`, log
in `~/integ10-<key>.log`), four trees at once with `CARGO_BUILD_JOBS=4`. Each first touches the
sources (`run.sh` keeps file times).

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`: pass.
- `cargo test --workspace`: 724 passed, 0 failed, 2 ignored (660 before; `tenants/mail.rs`
  needs SMTP; Q-1's `sqlite_double_quoted_words_are_names_only` waits for DQS off, backlog
  2026-11-08 S-7).
- `RUUM_REQUIRE_IMPLEMENTED=1 cargo test -p ruum-testkit` with `RUUM_SUITE_BACKEND=memory` and
  `postgres` (the same numbers on both): tenant isolation 354 checks (314 passed, 40 planned;
  before 304 / 50), permission matrix 1936 checks (1744 passed, 192 planned; before 1696 /
  240), trashed file suite 96/96; database cases d1 11/11 and grants 7/7 passed, agents 13,
  guests 3 and statements 7 planned, nodes 5 planned and 3 pending (card P-3); privacy suite 99
  planned; `databases_classes_api`, `databases_grants_api` and `databases_query_api` pass; 0
  pending (501), 0 failed. The SpiceDB variant was not run; CI runs memory authz.
- `MUTATION_STRICT=1 crates/ruum-testkit/scripts/mutation_check.sh` (with
  `MUTATION_TARGET_DIR` under the key's target): 88 of 88 mutations killed, none survived, skipped or broken (69 before; new: eight of
  the masking fix, one names and nine query mutations of Q-1, and `dbengine-set-config-unchecked`).
- `scripts/e2e_smoke.sh`: `PASSED: 59 checks`.
- `scripts/e2e_browser.sh`: 172 passed, 1 skipped (`sso.e2e.ts`: the shared stack runs no mock
  provider; CI starts it and requires the test), on each of the four tips.
- `cargo deny check` (on the advisory fix, build server, `cargo-deny` 0.20.2): advisories,
  bans, licenses and sources ok. CI's `cargo-deny` job is green on every `integ2/...` branch.
- Each tip on its own, build server: `f/masking-hardening` 670 unit tests, `f/untrusted-agent-text`
  674, `w/et-ruum` 674, `s2/q-1` 724; both suite backends and the browser tests green on each.
  The advisory fix alone: 660 unit tests, clippy and `cargo deny check` pass.
- GitHub CI: green on `d/rusqlite-0.41` (run 42630881821), `integ2/masking-hardening`
  (42634877658), `integ2/untrusted-agent-text` (42634883345) and `integ2/et-ruum`
  (42634887154) at the first run. `integ2/s2-q-1` (42634892612): the browser job failed once
  on the flaky `versions.e2e.ts` C-8 test (172 passed, 1 failed) and passed on the re-run of the
  failed job; every other job green at the first run.
- Known flaky: `versions.e2e.ts` "Upload new version ... (C-8)" (cause found: a stale node marks
  the wrong version "Current"; backlog 2026-11-09 T-13),
  `sharing::conformance::postgres_add_grant_limits_conform` and the visual `pseudo-locale >
  share`. The last two did not fail in this run.
- Not run at this integration: `web/` on the laptop (the browser job and `e2e_browser.sh` run the
  web build; `w/et-ruum` ran `npm ci && npm run lint && npm run build && npm test` on the build
  server, 306 tests), `oasdiff` (CI runs it).

## Next cards (2026-11-10, from `docs/backlog/2026-11-08.md` and `docs/backlog/2026-11-09.md`)

Small, clear fixes that need no person (design 20.3). The IDs are those of the backlogs (the
date tells which), which have the evidence. Each card goes through the normal reviews and gates.
Cards 1 to 4 of the earlier list are merged (integration of 2026-11-09 evening, run 6): the C-8
race (2026-11-09 T-13, `w/c8-race`), the data cluster per agent (2026-11-08 B-2,
`f/pgdata-per-agent`: every `run.sh` run of a current tree gets its own `pgdata-<key>`), K-1
C-2 and the step 2 error codes with query rule 10 (2026-11-08 P-2, P-3; 2026-11-09 S-12,
`f/k1c2-errors`). The brand directions (`w/brand-directions`, `docs/ux/brand/`) are merged too.
In this order:

1. **Security follow-ups** (2026-11-08 S-7; 2026-11-09 S-8, S-9, S-11, S-15, S-16, S-17): DQS
   off in the bundled SQLite (un-ignore the planned Q-1 test); masked copy with a view on a
   personal column; no `USAGE` on `ruum_mask` for the copy's owner; names only in
   `PrincipalRef` fields; the remaining untrusted marks; rule 10 walks the query of
   `CREATE TABLE ... AS` and `CREATE VIEW ... AS`; one line on "names, not relations".
2. **Flaky tests and connection drops** (2026-11-09 T-12, T-18 to T-20; 2026-11-08 T-4 to
   T-6): first the list of known flaky tests in `docs/TESTING.md` (T-6, raised: three new ones
   in run 6, among them `oauth.e2e.ts`, which failed in 2 of 4 full browser runs of this
   integration); cap the connections of the dbengine Postgres suites; `mutation_check.sh`
   prints why a mutant died.
3. **Brand tokens for Graphite** (2026-11-09 U-1 to U-4; decision 0005 B-5, design 12.6): the
   missing tokens with today's values (title font, icon stroke, control edge and shadow), the
   favicon and wordmark assets, self-hosted IBM Plex. With the visual designer's Graphite work
   (`w/graphite-theme` is open).
4. **CI and harness** (2026-11-08 T-1, T-2, T-10; 2026-11-09 U-5).
5. **Build server and mutation check** (2026-11-08 B-1, B-3 to B-7, the `docs/TESTING.md` half
   of S-5; 2026-11-09 B-13 to B-16, B-18 to B-20): `--detach`, file touch, a full-check lock,
   `--perl`, `cargo-deny` in `setup.sh`, a sweep of abandoned keys.
6. **Method** (2026-11-08 M-1 to M-8; 2026-11-09 M-14 to M-19, M-21, M-22): templates and
   workflow; tasks name the base commit.
7. **Translation** (2026-11-08 L-3 to L-5; 2026-11-09 L-10, L-12): a reviewer agent step for
   Estonian, visual tests in Estonian, localised emails, the voice panel follow-ups.

With card Q-2: 2026-11-09 P-15 (`QueryError` becomes `Error`).

Waiting for the architect panel: 2026-11-08 P-1 (K-1 C-1, before Q-2), P-4, P-5, P-8 (the
`isolation.rs` change of Q-1 is merged; confirm or veto), P-9; 2026-11-09 S-10 (owner-made
`SET` and definer functions), S-14 (show agent-written text in the web UI), review-Q-1-3 N-A
(the owner kind changes options); 2026-11-09 P-13 (rule 10 and own `pg_*` tables), P-14 (why a
long-named column stays personal), L-11 (a size limit in the error answer). Waiting for the person: see
`docs/digests/2026-11-09-evening.md`.

## Known gaps

- **Step 1 follow-ups** (decision 0003, `provisional`): items 1 (content role), 2 (C-12), 3
  (C-13), 4 (C-14), 5 (C-7) and 7 are built and merged, server and web UI; item 8 is recorded.
  Item 6 (C-8 to C-11) is merged (server, contract, web UI; migration
  `0010_upload_coalesce.sql`). The rate limit of `previewInvitation` is per process (`limits.rs`). Not built of 4, 7 and 8: the metric
  `ruum_auth_login_failed_total` (no metrics registry), the IP of WebDAV app password rows, the
  property test and mutation of item 8. The benchmark was not run with the new tools.
- **Mail.** Mail goes only to the relay of `SMTP_URL` (Mailpit on the laptop). Production
  sending stays with a person (design 18.9). There is no GDPR deletion job yet; it must delete
  a user's `mail_outbox` rows (and `principal_names`).

- **Step 2** (decision 0004, `provisional`): the specification, spike S-1 (passed; the role
  kinds are frozen), the foundation cards Q-0, P-1, E-1 and I-1, the C-1 acceptance (0004-35:
  the step 2 contracts are frozen, later changes are additive) and cards E-2 (Postgres engine),
  Q-4 (tenant pseudonym key) and P-2 (typed node store, delivery worker, reconciler) are
  merged, and so are cards D-1 (`createDatabase`, `getDatabase`, `updateDatabaseManifest`;
  `Services` wires the engines, the tenant keys, the delivery worker and the reconciler), T-1
  (database acceptance cases), T-2 (privacy suite), K-1 (column classes), A-1 (grants) and Q-1
  (the query API for people, 2026-11-09). The other 12 database operations count as planned in
  the suites; the privacy suite is all planned until the operations and the filters of Q-2 and
  Q-3 land. Q-1 notes open: review-Q-1-3 N-A (the owner kind still changes options; the panel
  records the intended reading). N-B is fixed: query rule 10 refuses writes to the catalog
  (`f/k1c2-errors`, merged 2026-11-09 evening; follow-ups backlog 2026-11-09 S-16, S-17, P-13). The fix of `f/rr-fix-e2` (`ruum_dbhost` search path) had two
  reviews: the first was not complete (`review-dbhost.md`), the second did not accept
  (`review-dbhost-2.md`): the search-path part holds, but **a tenant cast on a tenant type runs
  as `mask` inside `ruum_mask.pseudonym_other`, reads the HMAC key and can replace
  `ruum_mask.hmac12`** (item 2, proved on the data cluster; no agent reads through the lens
  yet), and items 1 and 3 have no test. The fix of item 2 and the tests of items 1 and 3 are on
  branch `f/masking-hardening` (decision 0004-37), **merged on 2026-11-09** after three reviews
  (`review-masking-3.md` accepted; notes N1 to N4: a database from before the fix keeps its
  key, no repair path for an old foreign dependent, test placement, mutation thread count). K-1 conditions open: C-1 (the column store and the
  manifest's `data_classes` drift apart after a person's `updateDatabaseManifest` or
  `restoreVersion`; before Q-2); C-2 is fixed (`f/k1c2-errors`, merged 2026-11-09 evening: a
  name over 63 characters gets no class row and counts as personal); notes N-1 to N-6. A-1 notes N-1 to N-5 and N-7; the create codes
  `409`/`503` are a contract gap for the panel. Open notes for later cards: review-D-1 N-2 to N-5, N-8, N-9 (plan check not
  atomic, two server processes, `getVersion`/`restoreVersion` of a manifest version);
  review-T-1 N-3 to N-7; review-T-2 N-5 (no public export operation: the panel decides) and
  N-8 (E-3, Q-2 and Q-3 each remove their own entry of `SQLITE_WAITS_FOR` /
  `FILTER_WAITS_FOR` in `crates/ruum-testkit/src/privacy.rs`); re-review-p2 F-1 (an unreadable
  payload still ends a `purged` delivery in Postgres; asked for before D-1 starts the worker,
  and D-1 now starts it), F-2 (memory store: a provider panic holds a delivery for ever) and
  F-3 (a mutation for `must_succeed`); all three are fixed on `f/followups-4`. OpenBao is not in CI: `openbao_transit_wrapper_round_trip` returns early and
  reports `ok` there (review-Q-4 note 2). `wrapper_from_env_needs_a_kek` asserts nothing where
  `dev.sh env` sets `RUUM_LOCAL_KEK_FILE` (review-Q-4 note 6). Notes for later cards:
  review-E-2 N-3 to N-6 (Q-2, I-2), review-Q-4 notes 3 to 5 (tenant export of design 14, D-1),
  review-P-2 findings 2 to 8 (D-1, P-3, step 6 HTTP adapter, tenant deletion).
  `migrations/0004_databases.sql` is a draft, but it is applied like any migration (by
  `sqlx::test`, the suites, and the server at start while `RUUM_MIGRATE` is on, the default), so
  a change to it before step 2 is built needs a fresh database. Open: card C-1 takes the text
  change of 0004-34 (d) (done in 0004-35); the PostgreSQL
  Anonymizer change of design 8.5 (a person confirms); UX contract needs C-20 to C-23; the
  notes of the card reviews for later cards (Q-2: `int2`/`int4`, `real`, BC dates; P-2: agent
  copies, delivery keys per tenant; I-2 and later: review-I-1 findings 1 and 2).
  `scripts/dbhost_check.sh` needs the `postgres-data` service and was not run at this
  integration.
- **Linux visual snapshots.** The 47 Linux references of the morning were refreshed from CI
  (`1b344d6`); `w/fixes` and `f/ux-c8-c11` brought their own Linux references from CI. The
  browser job is green on `integ/s2-t-2-r` (the web tree of `main`), so no Linux reference
  waits for a refresh. The visual `pseudo-locale › share`
  failed once (2 % of pixels) and passed on the re-run.
- **Flaky browser tests.** `versions.e2e.ts` "a name conflict on upload ..." (morning) and the
  C-8 test of "Upload new version" (once this afternoon: the
  version without "Current" downloaded the newer content, probably a stale Versions list) can
  fail in CI; a re-run passed.
- **One SpiceDB for all worktrees.** The SpiceDB of the laptop stack holds one schema. After a
  branch with new relations (here `f/step2spec`) wrote its schema and relationships, a branch
  without those relations cannot write its schema (`cannot delete relation ...`), and its SpiceDB
  tests fail (`spicedb_authz_conforms`, `spicedb_agrees_with_the_memory_engine`,
  `writes_of_another_server_hold_at_once`). This hit `f/followups` before the step 2 merge; on
  `main` they pass.
- **Cookie names.** The session and CSRF cookies keep their names (`ruum_session`,
  `ruum_csrf`); the `__Host-` prefix is a later hardening, because the rename breaks the
  contract (decision 0002-3). The CSRF value is bound to the session. The REST download
  (`downloadContent`) keeps the stored type under its CSP sandbox; only WebDAV maps active types
  to `application/octet-stream`.
- **Caddy on a running stack.** The new `dav.ruum.localhost` block needs the Caddy container to
  be recreated (it reads the Caddyfile at start; `RUUM_DAV_UPSTREAM` is new in compose).
- **Signed content URLs and the trash.** A content URL made before the file went to the trash
  works until it expires (at most `CONTENT_URL_SECONDS`, 5 minutes).
- **Mutation check.** By default the script builds into `target/mutation`; pass
  `MUTATION_TARGET_DIR=$PWD/target` to share the cache.
- **Product gaps found by the browser tests** (`test.fixme`, see `docs/TESTING.md`): the 4 web
  UI gaps (dialog focus, upload race, cold-load `return_to`, no `/invitations/accept` page) are
  fixed by `q/uxfix`. The server gaps behind the two sharing `test.fixme` are closed in
  `f/email-grants` (a bind makes the person a guest; every sign-in and web session binds;
  `addGrant` queues a share email); the tests stay `fixme` until the web UI card sends
  `notify: true`. `docs/TESTING.md` still lists the fixed UI
  gaps and has no rows for `visual.e2e.ts` and `a11y.e2e.ts`.
- **Web UI after `q/uxfix`** (`docs/ux/decisions.md`): names come from the server since
  decision 0003-5 (C-7: `display_name` and `on_behalf_of_display_name` in
  every answer; agents show as "*Agent* (agent of *User*)"); an unlisted space is named from
  Shared with me (C-10, branch `f/ux-c8-c11`); uploads from the web UI never coalesce (C-8).
  Not done: the session-ended dialog (UX-09), pausing
  uploads offline, the "Changed by agents" filter (C-2), the "Shared" mark on rows (C-1), the
  share dialog people picker, grouping agent activity by runs, version day groups, Shift-range
  selection, the grid view, the app shell rail (UX-17), UX-20. The visual snapshots exist only
  for darwin; a Linux CI job needs its own reviewed set.
- **After `q/synthfix`** (`docs/ux/decisions.md`): agents (MCP, CLI) and guests who share by
  email still make email grants that never reach a member (C-12); `addGrant` sends no share
  email and the invitation email names no role or end date (C-13); no code calls
  `audit::events::sign_in`, so no space logs sign-ins (C-14). (The accept page names the space,
  the inviter and the role since C-9, branch `f/ux-c8-c11`.) A Member manages everything in a team space; a view-only member role is a
  product question for the architect panel. No folder upload control. (C-14 is built on
  `f/audit-mcp`, decision 0003-4.)
- **SpiceDB freshness inside a request.** A request reads at least as fresh as its tenant's
  newest token at its start and its own writes (decision 0002-4). Work that a handler spawns on
  another task (unless it uses `authz::in_current_request`), the outbox worker and `seed-dev`
  have no request floor and read at the process token. Concurrent writers outside a request can
  overwrite the process token with an older one; the suites hit this before the integration fix.
- **Coalescing.** Saves by the same actor within 10 minutes merge into the head version (design
  5.3), also through the CLI, WebDAV and MCP, unless the request sends `coalesce: false` (C-8;
  the web UI always does). Name a version (`setVersionLabel`) to keep it. The
  WebDAV ETag of a file is `"<head version>-<revision>"`, because a merged save keeps the version
  ID.
- **Request timeout.** `RUUM_REQUEST_TIMEOUT_SECONDS` (default 120) also covers the body of a
  large WebDAV `PUT` or `putContent` on a slow link. Chunked uploads are not affected.
- In-memory `TenantStore::usage` reports 0 bytes and 0 nodes (Postgres reports real usage).
- Passkeys are only in the manual checklist of `docs/TESTING.md`. Google and Microsoft are set up in
  the laptop Zitadel by `scripts/dev.sh` when their keys are present, and the flow through an external
  provider is tested with the mock OIDC provider (`web/e2e/sso.e2e.ts`); a real Google and Microsoft
  sign-in still needs a person (`docs/DEVSTACK.md`, "Sign-in providers").
- The WebDAV example `dav_fake_server` keeps its name but now runs on `Services::in_memory`.
- CI (`.github/workflows/`) runs on the pushed branches; `main` itself is not pushed.
- Older gaps of `docs/FOUNDATION.md` still apply (`node.sharing_updated` is not undone, the
  `idp` hint of `loginStart` is not sent, the in-memory `Arc` cycle).

## Dependencies changed on 2026-11-09

- `rusqlite` 0.39.0 to 0.41.1 (`libsqlite3-sys` 0.37.0 to 0.39.1), for RUSTSEC-2026-0459.
- `sqlx-sqlite` 0.9.0 is replaced by an empty path crate (`third_party/sqlx-sqlite-stub`,
  `[patch.crates-io]`); no code of it is built. Remove it with a sqlx release that accepts
  `libsqlite3-sys` 0.39.
- `sqlparser` 0.63.0 (Apache-2.0, card Q-1, decision 0004-36), default features off, `std`.

## Dependencies added in the evening integration (2026-11-08)

- `ruum-core` now uses the workspace dependency `jsonschema` (the manifest check of card D-1)
  and the workspace crate `ruum-dbengine`. No new crate from outside the workspace.

## Dependencies added on 2026-11-07 (afternoon)

- `aes-gcm` 0.11.1 (workspace, ruum-core; Apache-2.0 OR MIT; `default-features = false`,
  `aes` and `alloc`): the local key-encryption key of card Q-4 (decision 0004-16). The person
  approved it (decision 0004-36), and `sqlparser` too. `ruum-dbengine` now also uses the workspace dependencies `sqlx` and
  `tracing` (card E-2).

## Dependencies added in wave B

- `unicode-normalization` (workspace, nodes area: NFC names).
- ruum-core uses the existing workspace dependencies `similar`, `quick-xml` and `image` (diff
  plugins). ruum-mcp, ruum-cli and ruum-webdav use workspace dependencies only (`rmcp`, `clap`,
  `dav-server`, ...; see their `Cargo.toml`). Web: `@noble/hashes` (BLAKE3 in the browser).
