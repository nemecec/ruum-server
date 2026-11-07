# Digest 2026-11-07 (afternoon): C-8 to C-11, the C-1 acceptance, cards E-2, Q-4 and P-2

Short report for the person (design section 18.9). Agent panels decided everything here. Each
decision is `provisional`, and you can veto any part of it. `main` is not pushed.

## What changed on `main`

Six branches were rebased onto `main`, checked on the build server, pushed and checked in
GitHub CI, then fast-forwarded, in this order. The history is linear: there is no merge commit
and no co-author line.

- **`w/fixes`** (web UI): when the server says that an upload's name is taken, but the list on
  the screen did not show that name yet, the upload comes back to the conflict question. It
  never replaces or renames a file silently. A new version starts from the head that the
  server has now. Agent names wrap like text at phone width.
- **`f/ux-c8-c11`** (decision 0003 item 6):
  - C-8: a request can send `coalesce: false`, and the web UI always does. So an upload from
    the web UI always makes a new version (migration `0010_upload_coalesce.sql`).
  - C-9: `previewInvitation` and an accept page that names the space, the inviter, the role and
    the end date.
  - C-10: Shared with me names the space of each item.
  - C-11: the undo question says who changed the item later.
- **`c/c1-acceptance`** (decision 0004-35): card C-1 is accepted, and the step 2 contracts are
  frozen. From now on, a contract change must be additive.
- **`s2/e-2`**: the Postgres engine of `ruum-dbengine` on the shared data cluster.
- **`s2/q-4`**: the tenant pseudonym key, stored only in wrapped form (a local key-encryption
  key on the laptop, OpenBao transit in production).
- **`s2/p-2`**: typed nodes and provider deliveries in Postgres, the delivery worker and the
  reconciler.

Fixes made at the integration:

- Three web files of `f/ux-c8-c11` conflicted with `w/fixes`. The result keeps both: the
  `name_taken` round trip and the fresh head of `w/fixes`, and C-8's removal of the
  coalescing UI.
- `tools.json` was regenerated after the C-1 contract commit.
- A browser test helper in `web/e2e/sharing.e2e.ts` now opens the folder details by keyboard.
  The uploads panel and the toast covered the button, and one test failed in 5 of 7 CI runs.
  No assertion changed.
- The tenant-key mutation of review Q-4 is now in the mutation check.

New dependency: `aes-gcm` 0.11.1 (card Q-4). Details are in [STATUS.md](../STATUS.md).

## Test numbers (`main`, build server)

- `cargo fmt`, `cargo clippy -D warnings`: pass.
- `cargo test --workspace`: 565 passed, 0 failed, 1 ignored (needs SMTP). This morning: 467.
- Security suites with `RUUM_REQUIRE_IMPLEMENTED=1`, on memory and on Postgres: tenant isolation
  265 passed and 77 planned (342 checks), permission matrix 1552 passed and 384 planned (1936
  checks), trashed file suite 96 of 96. 0 failed.
- Mutation check (strict): 51 of 51 mutations killed (45 this morning), none survived or
  skipped.
- `scripts/e2e_smoke.sh`: 59 of 59 checks passed, against the build agent's own Postgres. The
  shared stack Postgres of the build server still has an old migration 0005, so the test fails
  there at start, as this morning.
- Web: lint, build, 289 Vitest tests (280 this morning) and the e2e type check pass.
- `oasdiff breaking` from the morning `main` (`1b344d6`): 0 breaking changes.
- GitHub CI: green on `w/fixes` and on each rebased branch (`integ/...`). Some jobs needed a
  re-run (see Risks).

## Decisions and reviews

- [0003: step 1 follow-ups](../decisions/0003-step1-followups.md), item 6: it is now built and
  merged. The build of `f/ux-c8-c11` and `w/fixes` has no panel review file.
- [0004: step 2 specification](../decisions/0004-step2-spec.md), new item 35: the C-1
  acceptance. Reviewer 1 **accepted with two required changes**. Reviewer 2 **requested four
  changes**. All six changes are additive and are applied. Where the reviewers disagreed, the
  record took the safer option, for example `database_not_ready` became a `503` with
  `Retry-After`.
- Card reviews (in `ruum-server-wt/panel/`):
  - E-2: **changes requested** for one blocker (B-1): during a copy, a tenant's own rules and
    triggers ran as `ruum_dbhost`, which can grant roles of other tenants. This is fixed on the
    branch: the steps that change tenant tables run as the copy's owner, with the tenant
    triggers, rules and forced row-level security parked. New hostile-copy tests cover it. The
    reviewer did not review the fix again; the integrator read it against the review.
  - Q-4: **accepted**, no blocker.
  - P-2: **rejected** for one finding: a `purged` delivery stopped after a `4xx`, so a provider
    could keep the data of a deleted node. This is fixed on the branch: `purged` and
    `tenant_deleted` retry until they succeed, and a new suite case tests this on both stores.
    The reviewer did not review the fix again.
- The non-blocking notes go to later cards: D-1 wires the engine, the keys and the worker into
  `Services`; Q-2, I-2, P-3, the tenant export of design 14 and the step 6 HTTP adapter take
  the rest. STATUS.md lists them.

## Risks

- **Flaky CI.** The unit job failed twice when it downloaded the PostgreSQL apt key
  (`curl: (56)`). In the browser job, the C-8 test of `versions.e2e.ts` failed once, and the
  visual `pseudo-locale › share` failed once. Each re-run passed.
- **Fixes without a second review.** The E-2 B-1 fix and the P-2 finding 1 fix are tested,
  but no reviewer has looked at them again.
- **OpenBao is not in CI.** The OpenBao test returns early there and reports `ok`.
- **Stale builds on the build server.** `run.sh` keeps the file times, so cargo can reuse old
  test binaries after a rebase. At this integration, a stale binary failed the testkit once.
  The checks now touch the sources before cargo runs. Two full builds at once also ran out of
  memory once.
- No branch was force-pushed. The rebased branches went to GitHub as `integ/...` for CI and
  were deleted after the fast-forward. On GitHub, the old branch names still point to the
  commits from before the rebase, and `main` there is not updated.
- The Estonian text of C-9 to C-11 is machine translated.

## Linux visual references

None waits for a refresh. The browser job is green on `integ/s2-p-2`, which has the same web
tree as `main`.

## Waiting for a person

- A veto or confirmation of decision 0003 item 6 as built, and of 0004-35.
- Approval of the new dependency `aes-gcm`. `sqlparser`, `rusqlite` and the web dependencies
  of the morning still wait too.
- A check of the Estonian translation.
- OpenBao in CI and production key handling (decision 0004-16).
- A push of `main`.
