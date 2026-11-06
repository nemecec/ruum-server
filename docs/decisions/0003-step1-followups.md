# 0003: Step 1 follow-ups

Status: `provisional`, decided by the agent panel on 2026-11-06 (design section 18.9). The
architect wrote two proposals (`followups-proposal-v1.md`, `followups-proposal-v2.md`); two
reviewers reviewed each one (`followups-review-1.md`, `followups-review-2.md`,
`followups-review-v2-1.md`, `followups-review-v2-2.md`). Neither reviewer accepted v2 as it
was; each listed required changes. This record is v2 with every required change of both v2
reviews applied. Where the reviews did not agree, the safer option is taken, and the text says
why. A person can veto or change any part. Without a veto, the decisions stand.

The panel files are in `ruum-server-wt/panel/` (outside the repository).

## Implementation status

The proposal's order of work is item 5, then 1, 2, 3, then 4, 6, 7 and 8. This change
implements **item 5** in full. Items 1, 2, 3, 4, 6, 7 and 8 are decided here and not built yet;
each is a separate change, in that order. No item is half built.

| Item | Decision | Built |
|---|---|---|
| 1. A content role for members | below | no |
| 2. C-12: email grants bind | below | no |
| 3. C-13: share and invitation emails | below | no |
| 4. C-14: sign-ins in the audit log | below | no |
| 5. C-7: names of actors | below | **yes** (branch `f/followups`) |
| 6. C-8 to C-11 | below | no |
| 7. MCP and benchmark fixes, safe stop | below | only the fixes 1 and 2 of BENCHMARK.md (item 5) and the `list_changes` tool text |
| 8. IDs of gone nodes in entry data | below | no |

Migrations are numbered in the order they are made, not by the numbers of the proposal (which
assumed item 1 first): item 5 made `0002_principal_names.sql` and
`0003_agent_names_unique.sql`. The next items take the next free numbers.

## 1. A content role for team members

**Decision.** As v2 1.1 to 1.10: `MemberRole` does not change; a new field
`content_role: view | comment | edit | manage` applies only to `role: member`, default
`manage`; owners and admins always manage the team tree and have `content_role: null`; guests
have `null`. Storage invariant: `content_role` is not null if and only if `role = 'member'`
(CHECK constraint on `memberships` and `invitations`); the rebuild mapping goes into the
`schema.zed` header. SpiceDB: `team_manager = member - content_viewer - content_commenter -
content_editor`, then `team_editor`, `team_commenter`, `team_viewer`; edit members share only
while `members_can_share` and the effective `editors_can_share` are on. Feed wording of 0002-6
as v2 1.5 (rule (a): any content role; rule (b): manage only; every change of `content_role`
moves `memberships.updated_at`). A grant to `tenant#member` gives its role to every member,
view members included (v2 1.6), and the UI says so.

**Review outcome.** Both v2 reviews required changes:

- Reviewer 1 #2: `accept_invitation` is a transition. It writes `content_role` and its tuple
  through `role_relations` in the same transaction: a new person, or a guest who becomes a
  member, gets the invitation's content role; an existing member keeps the higher of the two
  content roles (acceptance never lowers rights, as `accepted_role`); an admin or owner stays
  at `null`. Tenant conformance cases for the three paths; mutation `accept-ignores-content-role`.
- Reviewer 2 #4: a "Can view" member still sees the whole team tree; only what they can change
  is limited. This record, the Members page and the invite form say so ("Can view: sees
  everything in *Space*, changes nothing. To show only some folders, make them a guest."). The
  S2 hint that sends people to Guest stays.

## 2. C-12: email grants bind

**Decision.** As v2 2.1 to 2.6: (a) a caller whose effective user is a member, admin or owner
gets a user grant for the verified email of a person of the tenant; (a') a guest caller (or an
agent of a guest) always gets an email grant; (b) an email grant binds by verified email only
in an interactive user session (sign-in, or `getMe` / `listSharedWithMe` with the session
cookie), never for agent tokens, app passwords, share-link tokens or bearer tokens; (c) a bind
makes a guest only while `allow_guests` is true. Audit `grant.added` with `may_add_guest` and
`member.added` with `via: email_grant`. Rate limit of `addGrant`: 60 per 10 minutes per user
and tenant, 600 per tenant, in Postgres.

**Review outcome.**

- Reviewer 1 #1: while `allow_guests` is false, a guest caller (or an agent of a guest) gets
  the same answer for every email grant: always `403 guests_refused`, so a member's email and
  an unknown email cannot be told apart. Sharing conformance case on memory and Postgres;
  mutation rule.
- Reviewer 2 #1, the 60-second throttle: the `404` path of the item page forces one bind check
  that the throttle does not stop (a `listSharedWithMe` call with a parameter that skips the
  throttle, for session cookies only; agents and tokens stay excluded). Test: a member opens a
  guest's share within 60 seconds of a `getMe` and gets the item.
- Reviewer 1 note (taken as part of the safer design): refused `addGrant` calls count against
  the rate limit too, and the counter is written outside the grant's transaction.

## 3. C-13: share and invitation emails

**Decision.** As v2 3.1 to 3.10: shares create no invitations (an email grant binds only by
verified email, rule 2(b)); share mails carry no token; the server mails no share-link tokens
(the view link without an account is a parked product question); invitation mails name the
space, inviter, role, content role and end date; `multipart/alternative`; header-injection and
message rules of v2 3.3; a mail outbox in Postgres written in the grant's transaction, with a
worker; invitation tokens never stored in the outbox (`invitation_tokens`); mail goes only to
Mailpit, production sending stays with a person.

**Review outcome.**

- Reviewer 2 #1, the link: the share mail links to the existing routes
  `/t/<tenant_id>/item/<id>` (file) or `/t/<tenant_id>/files/<id>` (folder), not `/n/<id>`.
  The item page on `404` from a share mail says "If this was shared with another email address,
  sign in with that address, or ask the person who shared it" (no sharer, no item name), and
  that a new account's email must be verified first. Tests: e2e with Mailpit (an outsider
  creates an account with that address and lands on the item); a person signed in with
  another address sees the "another address" state.
- Reviewer 2 #2: the share dialog stops calling `createInvitation` and drops the "Email them an
  invitation" box in the same change; "Notify people" and the share email replace it. The copy
  for non-admin sharers changes to the 3.10 wording (and never tells a member to "make an
  account"). When the answer has `notified: false`, the dialog says "Ruum did not email them
  this time; they find it under Shared with me." Web flow test.
- Reviewer 1 #3 (a): `mail_outbox` has no grant key; before sending, the worker checks that a
  grant on `node_id` still exists for the recipient (an email grant for `lower(to_email)`, or a
  user grant of the user whose verified email is `to_email`). A removed grant drops the mail; a
  grant bound before the send still sends. Tests for both.
- Reviewer 1 #3 (b): for invitation rows the worker sends and mints a token only while the
  invitation is pending; a revoke before the send means no mail and no token. "End all its
  tokens" means "no longer accepted", not "deleted" (reviewer 1 note on C-9), so
  `previewInvitation` can still show `revoked`, `accepted` or `expired`.
- Retention, where the reviews differ. Reviewer 1 #3 (c) asks to clear `payload` also when the
  worker gives up, to delete sent or given-up rows after a fixed window "for example 7 days",
  and to delete a user's outbox rows at GDPR deletion. Reviewer 2 #5 asks to delete them after
  30 days "enough for support". **Decision: 7 days**, the safer option: the rows hold email
  addresses and the personal message (DESIGN 15, GDPR), and the 10-minute de-duplicate check
  needs only minutes. `payload` is cleared on success and on give-up; GDPR deletion deletes the
  user's rows.
- Reviewer 2 #5, clean-up of the other new tables, run by the outbox worker with one test per
  table: `session_tenants` rows go when their session ends or expires; `invitation_tokens` rows
  go when their invitation is accepted, revoked or expired, after `previewInvitation` no longer
  needs them (the invitation row keeps the state); `rate_counters` lose old windows.
- Reviewer 1 note: per-tenant caps do not stop one person with many free personal tenants.
  Acceptable while mail goes only to Mailpit; it goes on the card for the production mail
  sender, which stays with a person.

## 4. C-14: sign-ins in the audit log

**Decision.** As v2 4.1 to 4.3: `auth.login` in the home tenant at sign-in, synchronous and
fail-closed; one `auth.login` with `via: session_first_use` at the first use of a session in
another tenant where the user is a member, admin or owner (`session_tenants` marker); no row
and no marker for guests; `auth.login_failed` only in the home tenant and only for a known
user; the unused audit helpers wired.

**Review outcome.** No required change. Reviewer 1 note: `data.session_id` is the
`sessions.id` row ID, never derived from the cookie secret. Reviewer 2 #5: `session_tenants`
rows are deleted when their session ends or expires (see item 3).

## 5. C-7: names of actors (built)

**Decision.**

- Every answer that names a principal carries its name: `PrincipalRef.display_name` (journal
  entries, versions, `created_by` and `updated_by`, grants, Shared with me, audit, agent
  activity), the grantee's `display_name`, and a new optional `on_behalf_of_display_name` next
  to every `on_behalf_of`.
- Users show their current display name, agents their name. Anyone who may see the answer may
  see the name, as in Drive.
- Resolution order, per tenant, one batch per answer: current members and guests (`users`),
  agents of the tenant (`agents`), then the tenant's snapshot `principal_names` of people who
  left, else no name. The snapshot is written when a membership ends, in the same transaction.
  No lookup crosses tenants; the resolver runs under the tenant's row-level security.
- Agents look like people only by name: answers always carry `actor.kind`, and for an agent
  `on_behalf_of` and `on_behalf_of_display_name`. The web UI shows "*Agent* (agent of *User*)"
  with the agent mark. The MCP `list_changes` description says to tell agents apart by
  `actor.kind`.
- Agent names are unique per owner and tenant, ignoring case. A clash gets " (2)", " (3)", ...
  (no new `409`); the base name is cut so the result fits 100 characters (reviewer 2 note).
  Migration `0003_agent_names_unique.sql` numbers existing duplicates first, then adds the
  unique index `agents_owner_name_key`.
- `listChanges` gets the optional filters `actor` (principal ID) and `since` (timestamp). They
  apply after the visibility rules, so they never show more.
- GDPR deletion: the deletion job deletes the user's `principal_names` rows in every tenant
  and blanks `users.display_name`; answers then carry no name. "A removed person keeps the last
  name" holds only until a GDPR deletion.

**Review outcome.** Both v2 reviews accepted item 5 for their lens (reviewer 1: "the name
snapshot does not change RLS. GDPR deletion leads to `null`"). Reviewer 1 note: the GDPR job
needs a system lookup per cell to find the tenants that hold a removed person's snapshot; "no
system lookup across tenants" holds for the name resolver only.

**Choices made while building (each the safer or the non-breaking option):**

- `display_name` and `on_behalf_of_display_name` stay `type: string` and are **absent** when no
  name is known, instead of `null`. Widening a response type to `null` is a breaking change
  under `oasdiff`.
- `on_behalf_of_display_name` is added to every schema that has `on_behalf_of` (`ActorPair`,
  `JournalEntry`, `Change`, `Version`, `AuditEntry`, `Me`), not only to `ActorPair` and
  `JournalEntry`, so the rule "next to every `on_behalf_of`" has no exception. The journal
  entry event schema gets the same optional property.
- `GranteeUser.email` is filled only for callers who may share the node **and** whose
  effective user is a member, admin or owner (who may list the members' emails anyway). v2 said
  "callers who have `share`"; a guest with `share` would then read members' emails from the
  grant list, which is the lookup that 0002-5 row 36 keeps from guests.
- Share-link bearers get no names. They are anonymous visitors.
- One middleware in `ruum-api` (`names.rs`) fills the names for every operation, also for MCP
  tools, which run through the same router. It takes the tenant of a principal from the
  nearest enclosing object (`tenant_id`, or its `node.tenant_id`), else from the request; so
  Shared with me, which spans tenants, names each sharer in the item's tenant.
- There is no GDPR deletion job yet. The job's card must delete `principal_names` rows (and,
  with item 3, `mail_outbox` rows).

**Changes.** Contract (additive, `oasdiff breaking` against `main`: nothing): the optional
response properties above, the `actor` and `since` parameters of `listChanges`, the wording of
`PrincipalRef` and `GranteeUser`, the event schema, the `list_changes` tool text
(`tools.json` regenerated). Migrations `0002_principal_names.sql`,
`0003_agent_names_unique.sql`. Code: `TenantStore::principal_names` (memory, Postgres) and the
snapshot at `remove_member`; `agents::unique_agent_name` (registration, OAuth consent,
rename); `ruum-api` `names.rs`, `routes/sharing.rs` (`may_see_grantee_emails`),
`routes/changes.rs` and `journal::feed::ChangesFilter`; web `useNames.ts`, `names.tsx`,
`SharedPage.tsx`. Bench task `undo_agent_two_bots`.

**Tests.** `tenants::tests::principal_names_follow_the_tenant` and
`agents::tests::agent_names_are_unique_per_owner` (memory and Postgres);
`agents::tests::the_migration_numbers_duplicate_agent_names`; `names::tests` (ruum-api);
`ruum-testkit` `tests/names.rs` on the suite backend (getNode, listChanges with and without
filters, listVersions, listGrants as a member and as a guest who may share, Shared with me,
the audit log, a share-link bearer, tenant B, an agent named like its user, a removed member);
web flow test "marks an agent whose name looks like a person". Mutations
`names-cross-tenant-memory`, `names-ignore-item-tenant`, `names-grantee-email-for-guests`,
`names-for-share-link-bearers`.

## 6. C-8 to C-11

**Decision.** As v2 6: C-8 `coalesce` on `UploadTargetNewVersion` and `putContent`; C-9
`previewInvitation` (`POST /v1/invitations/preview`, token in the body, per-IP limit, masked
email, state); C-10 `SharedItem.tenant_name`; C-11 `UndoItem.changed_by`.

**Review outcome.** No required change. Reviewer 1 note: `previewInvitation` shows ended
invitations, so ended tokens are "no longer accepted", not deleted (see item 3).

## 7. MCP fixes, benchmark fixes and the safe stop

**Decision.** As v2 7.1 to 7.7: tool descriptions; BENCHMARK fixes 1 and 2 by item 5, fix 3
(agent-facing undo preview) deferred on purpose, fixes 4 (search `type`), 5 (batch
`moveNodes`, `attachTagToNodes`) and 6 (`user_required` hint names the page) decided; a third
benchmark outcome `safe_stop` with no credit, and a gate of pass rate plus harmful outcomes = 0.

**Review outcome.** Both reviews required the same fix of 7.4: `nodes` has no `mime_type`
column; the MIME type is `nodes.type` (`Node.type` in the contract). The migration is
`create index nodes_type_idx on nodes (tenant_id, lower(type) text_pattern_ops) where trashed_at
is null` (reviewer 2: `text_pattern_ops`, so the prefix match `like 'image/%'` can use it), and
the `searchNodes` text says that `type` matches `Node.type`, so the filter keeps working for
typed nodes later. Reviewer 2 note: the `search_nodes` tool text says which of `kind` and `type`
an agent uses for "all PDFs".

## 8. IDs of gone nodes in entry data

**Decision.** As v2 8: IDs are acceptable; names and every other attribute of a node that fails
rules (a) and (b) never appear. The accepted costs (UUIDv7 creation time, linking) are
recorded. The rule replaces the "What does not count yet" paragraph of 0002-6.

**Review outcome.** No required change (reviewer 1: "Item 8's rule and its accepted costs are
recorded").

## Later cards

From v2 7.7 and the reviews: an agent-facing undo preview; the rate limits of `limits.rs` in
the shared counters; an email match for admin invitations; a view link in the share email if
real users ask; the GDPR deletion job (`principal_names`, `mail_outbox`); per-person caps for
the production mail sender.
