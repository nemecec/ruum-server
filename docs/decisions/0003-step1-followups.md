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

The proposal's order of work is item 5, then 1, 2, 3, then 4, 6, 7 and 8. Item 5 is built
(branch `f/followups`), and item 1 is built (branch `f/content-role`), each in full. Items 2
and 3 are built on the server (branch `f/email-grants`, stacked on `f/content-role`). Items 4
and 7 are built, and item 8 is recorded, on branch `f/audit-mcp`. Item 6 is decided here and
not built yet. The web UI parts of items 1, 2 and 3 are cards in
`ruum-server-wt/panel/ui-followups.md`, built on the design system.

| Item | Decision | Built |
|---|---|---|
| 1. A content role for members | below | **yes**, server, contract and tests (branch `f/content-role`); the UI is a card |
| 2. C-12: email grants bind | below | **yes**, server (branch `f/email-grants`) |
| 3. C-13: share and invitation emails | below | **yes**, server (branch `f/email-grants`); web UI on the card |
| 4. C-14: sign-ins in the audit log | below | **yes** (branch `f/audit-mcp`) |
| 5. C-7: names of actors | below | **yes** (branch `f/followups`) |
| 6. C-8 to C-11 | below | no |
| 7. MCP and benchmark fixes, safe stop | below | **yes** (branch `f/audit-mcp`; fixes 1 and 2 by item 5, fix 3 deferred on purpose) |
| 8. IDs of gone nodes in entry data | below | **recorded** (branch `f/audit-mcp`: 0002-6, the contract text, a feed test) |

Migrations are numbered in the order they are made, not by the numbers of the proposal (which
assumed item 1 first): item 5 made `0002_principal_names.sql` and
`0003_agent_names_unique.sql`, decision 0004 took `0004_databases.sql`, item 1 made
`0005_member_content_role.sql`, items 2 and 3 made `0006_email_grants_and_mail.sql`, and
`f/audit-mcp` made `0007_session_tenants.sql` (item 4) and `0008_nodes_type_index.sql` (item
7); it was written as 0005 and 0006 and renumbered at its rebase (the SQL did not change). The
next items take the next free numbers.

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

**Choices made while building (each the safer or the non-breaking option):**

- `Member.content_role` and `Invitation.content_role` are `ContentRole` or `null`, and the
  server always sends them, but they are **not** in `required`. v2 1.3 said "required in
  responses". The web UI builds `Member` and `Invitation` objects in its mock API, and the web
  UI may not change in this change (it is being rebuilt); a required property would break its
  build. The property can become required later without a breaking change.
- A content role for a role other than `member` (in `updateMember` or `createInvitation`) is
  `422 validation_failed`, the code the server uses for invalid fields (v2 said
  `invalid_request`, which is not an `ErrorCode`).
- The schema writes `team_manager = member - (content_viewer + content_commenter +
  content_editor)`: the same set as the chained `-` of v2 1.4, with one parenthesis, so the
  in-memory reader (operators left to right) and SpiceDB read it the same way.
- `node#edit`, `comment` and `view` get `team->team_editor`, `team_commenter` and `team_viewer`
  (v2 1.4). `sharing_member` (the team root's `share`) is now `team_manager &
  members_can_share`; edit members share through `sharing_team_editor` inside
  `sharing_editor`, so `editors_can_share` applies to them.
- Rule (b) of 0002-6 does not "use `manage` through the authz check as today" (v2 1.5 assumed
  so; the feed reads the membership date, not the graph). The feed now asks
  `TenantStore::manager_since`: `memberships.updated_at` only for owners, admins and members
  with `content_role = manage`. Rule (a) still uses `member_since` (any content role).
- An accepted invitation always moves `memberships.updated_at`, as before (also when the role
  and the content role stay): the date is never too early.
- The audit entry `member.role_changed` gets `old_content_role` and `new_content_role`;
  `invitation.created` and `invitation.accepted` get `content_role`. A change of the content
  role alone is a `member.role_changed` entry.
- `TenantStore` gets `update_member` (role and content role), `invite_as` (with a content
  role), `manager_since` and `access_of` (role and content role, for the tag rules).
  `set_member_role` and `invite` stay and keep the content role, or use the default.

**Changes (item 1).** Contract (additive; `oasdiff breaking` against `main`: nothing): the
schema `ContentRole`, the optional `content_role` of `Member`, `Invitation`,
`CreateInvitationRequest` and the `updateMember` body, `422` on `updateMember` and
`createInvitation`, the wording of `addGrant`, `GranteeTenantMembers` and
`TenantPolicy.members_can_share`. `contracts/authz/schema.zed` (relations `content_viewer`,
`content_commenter`, `content_editor`; permissions `team_manager`, `team_editor`,
`team_commenter`, `team_viewer`, `sharing_team_editor`; the rebuild mapping in the header) and
`schema.test.yaml` (tenants `gamma`, `delta`). Migration `0005_member_content_role.sql`. Code:
`tenants::relations` (the content tuple), `tenants::rules` (`content_role_after`,
`invitation_content_role`, `accepted_content_role`, `edits_team_content`), the memory and
Postgres tenant stores (`update_member`, `invite_as`, `accept_invitation`, `remove_member`,
`manager_since`, `access_of`), `journal::feed` (rule (b)), `tags::rules` (create, update and
the creator's delete need content role `edit` or more), `ruum-api` `routes/tenants.rs`. The
test kit has three new principals, `member_view`, `member_comment` and `member_edit`
(`fixtures.rs`, the matrix columns of `permissions.rs`, `permission_matrix.md`).

**Tests (item 1).** `schema.test.yaml` (authz conformance on memory and SpiceDB: each content
role, an edit grant for a view member, a `tenant#member` grant for view and comment members,
edit members without `members_can_share` and where `editors_can_share` is false); tenant
conformance on memory and Postgres: `content_roles_keep_the_invariant` (member(view), admin,
member (default), guest, member(edit), removed: the content role and the tuples after each
step, the CHECK constraints in SQL), `accepting_an_invitation_sets_the_content_role` (a new
person, a guest, a member keeps the higher role, an admin stays at `null`) and
`member_since_dates_the_membership` (a content role change moves the date; `manager_since`);
`relations::tests::content_roles_map_to_one_tuple`, `rules::tests` (content roles);
`journal::feed::tests::content_roles_count_for_rule_a_and_only_manage_for_rule_b`;
`tags::rules::tests::tag_writes_need_an_edit_content_role`;
`agents::undo::tests::a_view_member_cannot_undo_into_a_write`; the permission matrix with the
three new columns (memory, Postgres, SpiceDB): `member_view` and `member_comment` get `403` on
every write, `member_edit` writes content, cannot move, trash, restore or purge, and shares only
outside `Archive`. Mutations `team-viewer-gets-manage`, `content-role-not-cleared`,
`content-role-keeps-member-since`, `accept-ignores-content-role`.

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

## Items 2 and 3: how they are built

**Code.** `sharing::SharingStore::share` (`addGrant` with `notify` and `message`; `add_grant`
is `share` without a mail), memory and Postgres. `mailing::rate` (fixed-window counters in
`rate_counters`, and in memory), `mailing::outbox` (the mail outbox: `queue`, the worker
`run_worker`, `run_once`, `clean`), `mailing::{ShareFields, InvitationFields}`.
`tenants::mail`: `Mail.html` and `Mail.reply_to`, `multipart/alternative`, RFC 2047 subjects,
`share_mail`, `invitation_mail`, `header_text`, `message_text`, `html_escape`, `check_address`.
`IdentityStore::bind_email_grants(user, session, force)` (Postgres: `sessions.bind_checked_at`;
memory: the sharing store's `verified_email_signed_in`). `tenants::postgres::
bind_email_grants_on_sign_in` makes the guest (`member.added` with `via: email_grant`,
`grant_ids`, `granted_by`). `TenantStore::verified_person` and
`TenantStore::add_guest_from_email_grant` (memory). `PgTenantStore::invite` queues the mail;
`accept_invitation` looks the token up in `invitation_tokens`. `ruum-api`: `addGrant` answers
`notified`; the sign-in (`loginCallback`) binds with `force`; `getMe` and `listSharedWithMe`
bind for session cookies (`routes::auth::bind_for_session`); `listSharedWithMe` takes
`check_email_grants`. `ruum-server` starts the mail worker next to the sweeper (every 5
seconds, and at once when a change commits a mail).

**Contract** (additive; `oasdiff breaking` against `main`: nothing): the `addGrant` text (rules
(a), (a'), (b), (c), the share email, the limits) and its `429`; `AddGrantRequest.notify` and
`message` texts; `GrantWrite.notified`; the `GranteeEmail` text; the `getMe` and
`listSharedWithMe` texts and the `check_email_grants` parameter; the `createInvitation` text;
`may_add_guest` in the `grant.added` event schema; the `add_grant` tool text (`tools.json`
regenerated); `web/src/api/schema.d.ts` regenerated.

**Choices made while building** (each the safer or the non-breaking option):

- `may_add_guest: true` is in the data of every `grant.added` of an email grant (journal, and
  so the audit log), not computed per address. Every stored email grant may add a guest
  (callers who may list members get a user grant for the persons of the tenant; while guests
  are off an email grant to an outsider is refused), so a feed reader, also the guest who
  shared, learns nothing about the address. The event schema gets the optional property.
- The share mail row holds the mail's fields, not the rendered mail; the worker renders it with
  its public URL. The row still holds no secret.
- No share mail goes to the caller's own address, or to a user grantee without a verified
  email. The sharer's name is the effective user's name (an agent shares in its user's name).
- `TenantStore::invite` still returns a token to callers inside the server (the seed, the
  tests). It is stored as a hash in `invitation_tokens` and goes into no mail; the worker mints
  the mailed token. Both work until the invitation ends.
- `invitation_tokens` rows of an ended invitation are deleted 7 days after it ended, so a later
  `previewInvitation` (item 6) can still answer `accepted`, `revoked` or `expired` for a week.
- The accept link keeps `?token=`: the current accept page reads only the query, and the web UI
  is out of scope here. Moving the token into the fragment (`#token=`) is on the UI card; the
  server change is only `tenants::memory::accept_url`.
- The invitation mail names no content role yet: item 1 is not built.
- `session_tenants` (item 4) does not exist yet, so its clean-up rule waits for item 4.
- The in-memory stores send mail at once (they have no outbox, as they have no authz outbox).
- The `addGrant` limit counts in the sharing stores, before every other check, in its own
  committed transaction; MCP tools and the CLI go through the same path.
- Until the web UI card is built, the share dialog keeps sending `notify: false` and its own
  `createInvitation`, so the web UI sends no share email and its flow does not change.

**Tests.** `sharing::conformance` on memory and Postgres: `email_grants` (rules (a), (a'),
agents, `allow_guests: false` gives a guest the same `403` for every email), `share_emails`
(the mail, its link, message, escaping, no token; `notify: false`; email grants; no mail for
`tenant_members`; one mail per recipient and node in 10 minutes), `add_grant_limits` (60 calls,
refused calls count, an agent shares its user's counter, another user is not limited, a second
store on the same database shares the counter), `share_mail_limits` (the 51st grant in an hour
is made without a mail); `postgres_mail_outbox_worker` (retry, give-up and cleared payload, a
removed grant drops the mail, a grant bound before the send still sends, the 7-day clean-up,
old counter windows); `tenants::tests::email_grants` (a web session binds and makes a guest with
`member.added`, the 60-second throttle and `force`, another user's or an ended session binds
nothing, an unverified email and a tenant without guests bind nothing, a revoked invitation
sends no mail and mints no token, the clean-up of ended invitations' tokens);
`invitations_bind_by_token` (the mail names role and end date; the mailed token works and the
accept ends it); `tenants::mail::tests` (CR/LF and control characters, cut names, escaping, no
link from the message, multipart, encoded subjects, unsafe addresses); `ruum-api`
`email_grants_bind_in_web_sessions_only` (sign-in binds and makes a guest, the throttle of
`getMe`, `check_email_grants`, a bearer token never binds, `notified`);
`scripts/e2e_smoke.sh` (a share email reaches the Mailpit API with the folder link and no
token). Mutations `guest-add-grant-resolves-email` (memory, `-postgres`),
`guest-email-grant-oracle` (Postgres, `-memory`), `email-bind-any-session`,
`email-grant-binds-unverified`, `email-bind-no-guest-policy`,
`mail-outbox-ignores-removed-grant`; `sharing-email-grant-policy-memory` now breaks the
lookup of rule (a), because its old form became equivalent once rule (a) resolves members'
emails first.

## 4. C-14: sign-ins in the audit log

**Decision.** As v2 4.1 to 4.3: `auth.login` in the home tenant at sign-in, synchronous and
fail-closed; one `auth.login` with `via: session_first_use` at the first use of a session in
another tenant where the user is a member, admin or owner (`session_tenants` marker); no row
and no marker for guests; `auth.login_failed` only in the home tenant and only for a known
user; the unused audit helpers wired.

**Review outcome.** No required change. Reviewer 1 note: `data.session_id` is the
`sessions.id` row ID, never derived from the cookie secret. Reviewer 2 #5: `session_tenants`
rows are deleted when their session ends or expires (see item 3).

**Built** (branch `f/audit-mcp`). Choices made while building, each the safer or the simpler
option:

- `loginCallback` writes `auth.login` (`data.method`, `data.session_id`, IP, user agent) in the
  home tenant after the session row exists. When the write fails, the server ends that session
  and answers `500` without cookies.
- The first use is recorded by the `RequestContext` extractor and by `tenant_context` (the
  `/v1/tenants/{tenant_id}` routes), so every tenant request with a session cookie passes it.
  `AuditLog::record_first_use` writes the marker and the row in one transaction. A cache in the
  process (`SessionsSeen`) keeps the pairs that are done; a guest is not kept, so a later
  promotion to member is recorded.
- The client IP comes from the connection (or the last `X-Forwarded-For` address that a local
  proxy added, as `limits.rs`), for every audit entry of a request. WebDAV records only the user
  agent: its listener does not pass the peer address to the auth code.
- No path refuses a known user after the identity provider answered (a deleted user's identity
  is not found, and the sign-in makes or links a user), so `auth.login_failed` stays unused in
  step 1. Failures without a known user are a `tracing` event (`target: ruum::security`,
  `event: auth.login_failed`, a reason code). There is no metrics registry yet, so the counter
  `ruum_auth_login_failed_total` is a later card.
- `endSession` also writes `auth.logout` (the helper says "ended a session").
- `agent.key_used` is written in the transaction of the exchange; `agent.key_refused` (reasons
  `revoked`, `expired`, `agent_disabled`, `scope_expired`) is committed before the error. An
  unknown key writes nothing (it names no tenant). `agent.token_issued` is written in the
  transaction that rotates the refresh token, so a failed write issues nothing.
- `auth.app_password_used`: at most one row per password per hour, checked against the audit
  table itself in the insert statement (`last_used_at` is still updated on every use, for the
  web UI).
- The sweeper (`identity::sweep`), not the mail outbox worker of item 3, deletes the markers of
  sessions that ended, expired or are gone: it already sweeps sessions.

Tests: the audit conformance case `the_first_use_of_a_session_is_recorded_once` (memory,
Postgres); `routes::auth::tests::sign_ins_are_audited_in_the_home_tenant_and_at_first_use`,
`a_failed_sign_in_audit_fails_the_sign_in`, `a_failed_first_use_audit_fails_the_request`; the
key, token and app password rows in `agents::tests` and `identity::tests` (memory, Postgres);
the sweeper test. Mutations `login-not-audited`, `guest-session-audited-in-host`.

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

**Built** (branch `f/audit-mcp`):

- Tool descriptions (`move_node`, `copy_node`, `rename_node`, `create_node`: `name_taken` and
  `on_conflict: rename`; `search_nodes`: `type`, not `kind`, for "all PDFs"; `get_me`: what
  agents cannot do) and the user-only sentence in the MCP `INSTRUCTIONS`.
- Fix 4: `searchNodes` takes `type` (exact, or a prefix that ends in `/`, ignoring case) and `q`
  is optional when `type`, `tag` or `within` is given (then ordered by name); with none of the
  four, `422 validation_failed`. Migration `0008_nodes_type_index.sql` (the index above).
- Fix 5: `moveNodes` (`POST /v1/nodes/move`) and `attachTagToNodes` (`POST /v1/tags/{tag_id}/nodes`),
  1 to 100 items, `200` with one `BatchItemResult` per item: `status` is what the single
  operation would answer, then its body or `error`. Not atomic; the `Idempotency-Key` covers the
  call. Tools `move_nodes` and `attach_tag_to_nodes`. The isolation and permission suites send
  one-item batches and judge the item's status (`ruum_testkit::client::send`,
  `matrix::BATCH_OPERATIONS`); `tests/batch.rs` checks mixed batches.
- Fix 6: a middleware gives every `403 user_required` the hint "ask the user to do this in the
  Ruum web UI at `<RUUM_PUBLIC_URL><page>`", the page by the path of the operation (`/agents`,
  `/members`, `/audit`, `/settings`, `/account`). The web UI must route these top-level paths
  to the page of the current space (card in `ruum-server-wt/panel/ui-followups.md`).
- The safe stop: `bench/agent/lib/outcome.mjs` (outcomes `pass`, `safe_stop`, `fail`,
  `harmful`; a tenant snapshot before and after the agent), `harm: true` on the checks of data
  loss, wrong access and refused actions, the report's `gate` (pass rate, harmful = 0), and the
  "Scoring" section of `docs/BENCHMARK.md`. The benchmark was not run again.
- Per-item rate counting of batches (7.3) is not built: step 1 has no rate limit on these
  writes, and the shared counters come with item 2.

## 8. IDs of gone nodes in entry data

**Decision.** As v2 8: IDs are acceptable; names and every other attribute of a node that fails
rules (a) and (b) never appear. The accepted costs (UUIDv7 creation time, linking) are
recorded. The rule replaces the "What does not count yet" paragraph of 0002-6.

**Review outcome.** No required change (reviewer 1: "Item 8's rule and its accepted costs are
recorded").

**Recorded** (branch `f/audit-mcp`): the rule replaces the open point of 0002-6 in
`0002-step1-panel-decisions.md`; one sentence in `listChanges` and in the `data` of
`contracts/events/journal-entry.schema.json`; the feed test
`journal::feed::tests::a_move_out_of_a_gone_folder_shows_its_id_and_never_its_name`. Not done:
the assertion in the feed property test that no name of a node that fails (a) and (b) appears,
and the mutation `feed-leaks-gone-parent-name` (v2 8.6); they stay on the card of item 8.

## Later cards

From v2 7.7 and the reviews: an agent-facing undo preview; the rate limits of `limits.rs` in
the shared counters; an email match for admin invitations; a view link in the share email if
real users ask; the GDPR deletion job (`principal_names`, `mail_outbox`); per-person caps for
the production mail sender; the metric `ruum_auth_login_failed_total` (item 4, when a metrics
registry exists); the property test and mutation of item 8. From items 2 and 3: the web UI card
(share dialog, item page states, accept link in the fragment), the clean-up of
`session_tenants` with item 4, and the content role in the invitation mail with item 1.
