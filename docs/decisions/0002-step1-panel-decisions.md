# 0002: Step 1 panel decisions

Status: `provisional`, decided by the agent panel on 2026-11-06 (design section 18.9: one
architect agent proposed, two reviewer agents asked for changes, and this record applies those
changes). A person can veto or change any part. Without a veto, the decisions stand.

This record answers the five open questions of step 1 that `docs/STATUS.md` and the permission
matrix listed. Where the reviewers asked for a change, the change is applied. Where they did not
agree, the safer option is taken, and the text says so.

## 1. `addGrant` by user ID

**Decision.** Keep the behaviour of commit `653ee9a`. Change the contract text to match it.

- A grant by user ID reaches only members and guests of the tenant.
- Any other user ID, unknown or a user of another tenant, gets `404 not_found`. No guest is made.
- People outside the tenant come in only by an email grant or an invitation. These bind by a
  secret token or by a verified email (`email_verified`).
- No new error code. The same `not_found` for both cases means that the answer does not tell an
  unknown ID apart from the user of another tenant.
- With `allow_guests: false`, an email grant is accepted only for the verified email of a
  member, admin or owner. The in-memory store refused every email grant in that case; it now
  follows the Postgres store (`TenantStore::is_verified_member_email`).

**Reasons.**

- A guest made from any user ID gave a lookup from user ID to email (`listMembers`), and it put
  the tenant in a stranger's tenant list without consent.
- Google Drive asks for an email, never an internal ID. Binding by token or verified email
  follows design 4.2 ("automatic linking by email alone allows account takeover").
- Later steps need nothing more.

**Changes.** Contract: the text of `addGrant` and `GranteeEmail` (wording only). Code:
`sharing/memory.rs`, `TenantStore::is_verified_member_email` (memory, Postgres, test stores).
Tests: a new case in `sharing::conformance` (memory and Postgres): with `allow_guests: false`,
an email grant to a member's verified email gives `ok`; to an outsider's or a guest's email it
gives `403`. Mutation `sharing-email-grant-policy-memory`. The isolation probe and the mutation
`sharing-grant-by-id-makes-guest` stay.

## 2. May guests own agents in step 1?

**Decision.** Yes, with limits.

- A guest's live agents count against a per-guest limit (`agents::guest_agent_limit`, one
  member's allowance), not against the tenant's agent limit.
- While the tenant policy has `allow_guests: false`, a guest cannot register a new agent:
  `registerAgent` and `oauthConsent` with `new_agent` answer `403 permission_denied`
  (`agents::guest_agents_refused`). Agents that exist stay; an admin can disable them.
- When a guest leaves or is removed, the guest's agents are disabled (`owner_left`, as before).

**Reasons.**

- Rights: a guest's agent has at most the guest's rights intersected with its scope (design 3.3,
  3.4). It cannot get more than the guest has.
- Control: admins see every agent and can disable it (3.4), and the audit log records the actor.
- Product: the first customers build with agents (17.1-2), and MCP is the main channel (17.1-3).
  A consultant who is a guest and uses Claude on a shared folder is a core case. In Google Drive
  an external user can use their own OAuth apps on files shared with them.
- A "no" would push guests to app passwords and WebDAV, which are less auditable than agents.
- The new rule matches invitations, which refuse guests while `allow_guests` is false.

**Cost for later steps (for step 2).** From step 2, agents spend compute and egress budgets
(design 4.3) and create databases and apps that count against the host tenant.

- Agents of guests get a zero compute and egress budget by default. An admin can raise it.
- Guests and their agents cannot create resources that count against the plan unless a tenant
  policy allows it. Step 2's provider contract decision sets that policy.

**Changes.** Contract: the text of `registerAgent` and `oauthConsent` (wording only). Code:
`insert_agent` in `agents/memory.rs` and `agents/postgres.rs`. Tests:
`agents::tests::guests_register_no_agents_while_guests_are_off` (memory and Postgres;
`registerAgent` and the OAuth consent; members still register). Mutations
`agents-guest-register-policy` and `agents-guest-register-policy-postgres`. Matrix: the notes of
`registerAgent` and `oauthConsent` say "decided (0002-2)". No cell changes, because the fixture
tenant allows guests.

## 3. WebDAV on its own host

**Decision.** Now, in the step 1 hardening, before any user sets up a mount.

- Host: `https://dav.ruum.space/` in production, `https://dav.ruum.localhost/` on the laptop.
  WebDAV is served at the path `/`.
- A separate listener, `RUUM_DAV_BIND` (default `127.0.0.1:8492`), with a router that holds only
  WebDAV routes. The WebDAV host has no `/v1`, no `/mcp` and no web UI. `RUUM_DAV_URL` is the
  public URL (default `https://dav.<host>/` for an https public URL, else `http://<bind>/`).
- The platform origin stops serving `/dav` and answers `404`. There is no redirect: Basic
  credentials must not go to the platform origin.
- The WebDAV host never reads and never sets cookies. Its sign-in is the `Authorization` header
  only (Basic with an app password, or an agent token). A session cookie there is no
  credential (`401`), and the router drops any `Set-Cookie`.
- Every answer of the WebDAV host keeps the content safety headers as defence in depth: the
  content CSP (`sandbox`), `nosniff`, `Content-Disposition: attachment` for `GET` and `HEAD`, and
  active types as `application/octet-stream`. So no script runs on the sibling host, where it
  could set cookies for the parent domain.
- The host is `dav.ruum.space`, not on `ruumapps.net`. Users type app passwords into WebDAV
  clients, and on a domain where anyone can host an app a look-alike phishing host is easy to
  make. `ruum.space` is fully under our control.
- `createAppPassword` returns `dav_url`, the URL to mount. The web UI shows it.

**Cookies and CSRF.** `dav.ruum.space` is same-site with `ruum.space`, so `SameSite` does not
keep the two apart. A sibling host can also set cookies with `Domain=ruum.space` ("cookie
tossing"). The CSRF check was a plain double submit (header equals cookie), which a tossed cookie
defeats.

- Shipped now: the CSRF value is bound to the session. It is base64url of SHA-256 over a fixed
  label and the session secret (`identity::csrf_token`). The server sets it as the `ruum_csrf`
  cookie at sign-in, and the check compares the `Ruum-Csrf` header with the value computed from
  the session cookie, not with the `ruum_csrf` cookie. A header and cookie pair that match each
  other but were not issued for the session fail with `403`.
- Later hardening, not in step 1: the cookie names `__Host-ruum_session` and
  `__Host-ruum_csrf` (the prefix forces `Secure`, `Path=/` and no `Domain`). The reviewers
  disagreed on this point: one preferred to keep the names in step 1, the other accepted a
  rename with all its changes. The rename is a breaking contract change (`securitySchemes`
  names `ruum_session`, the login response text names both cookies, and the web client reads
  `ruum_csrf` by name), and design 18.9 lets the panel make only changes that do not break the
  contract. The safer option within the rules is to keep the names and ship the session-bound
  value, which closes the cookie-tossing CSRF hole. With the WebDAV host free of scripts and
  cookies, the remaining risk (a tossed session cookie, that is login CSRF) needs a script on a
  sibling host, which the content headers prevent.

**Reasons.**

- Design 15 puts user content on separate domains; 17.1-1 is the same rule.
- The cost rises later: the mount URL lives in every Finder, Explorer and rclone setup, so a
  move after launch breaks every client.
- Browsers keep Basic credentials per origin, so on the platform origin they were cached next to
  the web UI.
- The content headers are mitigations. They depend on every browser honouring the CSP sandbox
  for every content type.

**Changes.** This amends decision 0001 section 7 (WebDAV authentication): the sign-in rules stay;
only the place changes (from `/dav` on the platform origin to its own host).

- Contract (additive): the info text names the WebDAV host and the session-bound CSRF value;
  `createAppPassword` names the WebDAV host; the optional field `AppPasswordCreated.dav_url`.
- Code: `crates/ruum-server/src/{config.rs,app.rs,main.rs}` (the second listener, `/dav` gone
  from the platform router), `ruum-webdav` (root path, no cookies), `ruum-api` (`dav_url`,
  `ApiConfig::dav_url`, the CSRF check), `ruum-core` (`identity::csrf_token`), the test kit
  fixture (the CSRF value of the member's session).
- Web: `Account.tsx` shows the server address; mocks and the account test; `npm run gen:api`.
- Laptop stack: the Caddy block `dav.ruum.localhost`, `RUUM_DAV_UPSTREAM` (compose,
  `.env.example`), `RUUM_DAV_BIND` and `RUUM_DAV_URL` in `scripts/dev.sh env`; `/dav` left the
  `@server` matchers.
- Scripts and docs: `scripts/e2e_smoke.sh` (WebDAV steps on the WebDAV listener,
  `RUUM_E2E_DAV_PORT`), `crates/ruum-webdav/scripts/smoke.sh`, `docs/DEVSTACK.md`,
  `web/README.md`, `docs/STATUS.md`. `ruum-cli` does not use WebDAV.

**Tests.** `app::tests::the_platform_origin_has_no_webdav` (`PROPFIND /dav/` and others: `404`);
`app::tests::the_dav_host_has_no_api` and `dav.rs` `the_dav_host_serves_only_webdav` (`GET
/v1/me` is `404` on the WebDAV host; `POST /mcp` is `501`, because `POST` is not a WebDAV method,
and never an MCP answer); `the_dav_host_ignores_and_never_sets_cookies` (a session cookie without
`Authorization` gives `401`, no `Set-Cookie`, the CSP is there);
`routes::auth::tests::the_csrf_value_is_bound_to_the_session`;
`routes::auth::tests::a_new_app_password_names_the_dav_host`; `files_are_served_as_sandboxed_attachments`
stays. Mutation `csrf-not-session-bound`. The e2e smoke test checks the WebDAV host, `dav_url`,
no cookie, and `404` for `/dav` on the platform origin.

## 4. SpiceDB freshness across processes

**Decision.** Now. Every check, bulk check and relationship read of a request reads at least as
fresh as the newest relationship write that any process applied for the request's tenant. A
revocation on one server holds at once on every server.

- Each request runs with its own freshness floor (`authz::with_request_freshness`, a task-local
  value). The REST router wraps every request in it; MCP tool calls go through the REST router;
  WebDAV wraps each request.
- `identity::auth::context_for` runs once for every request that acts in a tenant (REST, the
  tenant routes, MCP, WebDAV). It reads the tenant's newest applied ZedToken through
  `Services::freshness` and sets the floor (`authz::load_request_floor`).
- The Postgres implementation, `authz::outbox::PgFreshness`, reads `authz_outbox` in its own
  transaction that sets `ruum.tenant_id` first. `authz_outbox` is under row-level security, so
  `latest_token` on a connection without the tenant sees no row and answers `None`; that is the
  stale path the decision removes, and `PgFreshness` never takes it. The index
  `authz_outbox_token_idx (tenant_id, id desc) where applied_at is not null` serves the query.
- An error of the lookup fails the request (`500`). It is never read as "no token".
- The SpiceDB engine picks the consistency in this order: the explicit token of the call; the
  request floor; the newest token that this process wrote; else `minimizeLatency`. A write of
  the request raises the floor, so the request reads its own writes.
- Outside a request (the outbox worker, `seed-dev`, the content maintenance job) there is no
  floor, as before.
- The floor is the state at the start of the request (plus the request's own writes). A write
  that another request finishes while this one runs may not be seen by this one. That is the
  usual meaning of a concurrent write.
- The in-memory engine is always fresh (`authz::NoFreshness`).

**Reasons.** With more than one server, a removed grant or a revoked share link stayed effective
on the other servers for up to SpiceDB's quantization window (5 seconds by default). Tokens and
the index existed but were not used (`latest_token` had no caller). The cost is one indexed query
per request.

**`seed-dev` writes through the outbox (verified).** `seed-dev` uses the same Postgres stores as
the server (`wiring::services`), and these stores enqueue every relationship update in their
transaction and apply it after the commit (`authz::outbox`). The e2e smoke test used to wait
6 seconds after the seed for the server to see the seed's relationships; the wait is removed,
and the first requests after the seed pass (see the e2e run in `docs/STATUS.md`).

**Tests.**

- `wiring::tests::writes_of_another_server_hold_at_once` (`ruum-server`, Postgres and the SpiceDB
  of the stack): two server instances on one database, each with its own SpiceDB client and
  tokens. Server B creates a folder, grants Bob `view` and revokes the grant, three times; server
  A answers Bob's `GET /v1/nodes/{id}` through its real request path (router, `RequestContext`,
  `context_for`, `PgFreshness`, SpiceDB) with `200` right after each grant and `404` right after
  each revocation.
- `authz::outbox::tests::freshness_reads_the_tenant_token_under_rls`: the token of the tenant
  only; `None` before the apply; a connection without `ruum.tenant_id` sees no row; a lookup on a
  closed pool is an error.
- `routes::auth::tests::a_failed_freshness_lookup_fails_the_request`: a failing lookup gives
  `500` on tenant requests.
- Mutations `authz-ignores-tenant-token` (`context_for` skips the lookup) and
  `authz-spicedb-ignores-request-floor` (the engine ignores the floor).

## 5. The open rules of the permission matrix

Every rule of `crates/ruum-testkit/src/permissions.rs` that was marked "assumption" or "open
question" has a verdict. The notes now say "decided (0002-2)" or "decided (0002-5)". The matrix
in `crates/ruum-testkit/permission_matrix.md` is generated again. Only two rows change cells: the
`any` cells of the agents in `logout` and `listAgents` become `ok`. The server already answered so;
the test now holds it.

The agent columns are `agent` (acts for `member`, scope `Projects`, `max_role: view`),
`agent_out` (acts for `member`, scope `Private`) and `agent_of_guest_view`.

| Row | Operation | Verdict | Cells | Contract wording |
|---|---|---|---|---|
| 24 | `logout` with an agent bearer token | change `any` to `ok` | agents `ok`; others unchanged (share link `403`) | A bearer token (user or agent) has no session: the call ends nothing and answers `200`; the token stays valid until it expires. |
| 36 | `listMembers` | confirm | guests `403`; members and up `ok`; agents follow their user | Members, admins and owners (and their agents). Guests get `permission_denied`: the list holds the emails of everybody in the tenant. |
| 45 | `listAgents` with an agent caller | change `any` to `ok` | agents `ok` | An agent caller sees only itself. |
| 46 | `registerAgent` | confirm (0002-2) | unchanged (users `ok`, agents `403`) | See section 2. |
| 51 | `listAgentKeys` | confirm | owner of the agent and admins `ok`; others `403/404`; agents `403` (user-only) | The contract already said "The owner or a tenant admin". |
| 54 | `listAgentActivity` | confirm | owner and admins `ok`; others and agents (also the agent itself) `403/404` | The owner of the agent or a tenant admin. Agents get `403` or `404`. |
| 59 | `oauthConsent` | confirm (0002-2) | unchanged | The rules of `registerAgent` apply to `new_agent`. |
| 63 | `getNodeByPath` | confirm | `view` on the target is enough, as for `getNode`; the folders on the way need not be visible | Needs `view` on the target; a guest with a grant on `Projects/Garden` resolves that path. |
| 90 | `createTag` | confirm | members and up `ok`; guests `403`; agents need `max_role` of at least `edit` | An agent needs a scope `max_role` of at least `edit`. |
| 91 | `getTag` | confirm | members `ok`; guests and agents only when they see a node with the tag, else `404` | Members see every tag; guests and agents only with a visible tagged node, else `404`. |
| 92 | `updateTag` | confirm | members and up `ok` when they see the tag; agents need `max_role` `edit` | An agent needs `max_role` `edit` and must see the tag. |
| 93 | `deleteTag` | confirm | admins or the creator; agents need `max_role` `edit` | An agent needs `max_role` `edit` and must see the tag. |
| 94 | `listNodesByTag` | confirm | as `getTag` | Who cannot see the tag gets `404`. |

**Reasons.** `logout`: the call is harmless for a bearer token, and a `403` would add a special
case to clients. `listAgents`: an agent may know itself (`getMe` shows it too), and it sees no
other agent. `listMembers`: Google Drive does not show an external user the organisation's
directory. `getNodeByPath`: the same right as `getNode`, which also shows `parent_id`; the caller
supplies the names. Tags: a tag is tenant data; an agent with a read-only scope changes nothing,
and an agent sees a tag only through a node in its scope, so the scope also bounds what tags it
learns.

## 6. Who learns about a node in the trash

Status: `provisional`, an architect decision added on 2026-11-06 after the review of commit
`c9ba107`. It closes the "old place" gap of the changes feed.

**Rule.** A `node.trashed` or `node.purged` entry, and any other entry on a node that is in the
trash or purged now, reaches a subject only when one of these holds:

- (a) the subject could see the node just before it went to the trash: through a grant, a
  membership, or access to an ancestor that it held at that moment;
- (b) the subject could restore the node while it was in the trash: `manage`, capped by an
  agent's scope.

Access that began after the trash never reveals the node. Seeing the old place now is not
enough. For an agent, the node must also be inside its scope subtree.

**Members.** This applies to members too: a membership counts only from the time the user
joined. Members get `manage` on the whole team tree from the team root. So a member who joins
while the node is still in the trash could restore it, and (b) holds: the member sees the node
in the trash list and gets its entries. A member who joins after the purge learns nothing. An
agent of such a member gets the entries only when its scope allows `manage`.

**Members with content roles (decision 0003-1, this wording replaces the paragraph above where
they differ).** A membership counts from `memberships.updated_at` (the join, or the last change
of `role` or `content_role`).

- For rule (a), a membership with **any** content role counts: a view, comment or edit member
  who could see the node just before the trash satisfies (a), like a manager.
- For rule (b), a membership counts only while the member could restore the node:
  `content_role = manage`, or role admin or owner (`TenantStore::manager_since`). An agent of
  the member also needs a scope with `manage`.
- Any change of `content_role` moves `memberships.updated_at`, as a change of `role` does. So
  the date is never too early. The feed may then hold back an entry from a member who had the
  right before the change; it sends less, never more.

**Dates.** The feed uses the dates that the records already have:

- grants: the journal (`grant.added`, `grant.updated`, `grant.removed` and their `seq`);
- the trash time: the `node.trashed` entry that took the node out of the tree (its own or the
  one of an ancestor), until the `node.restored` or `node.purged` entry;
- memberships: `memberships.updated_at`, the join or the last change of the role
  (`TenantStore::member_since`). The journal has no `member.added` entry. A change between
  member, admin and owner also moves this time, so the answer is never too early; the feed may
  then hold back an entry from a member who had it before. The membership time comes from the
  database clock and the journal time from the server clock.

**What stays.** "Now" stays enough: an entry on a node that the subject sees now is sent. A node
in the trash shows now only to those who may restore it, so that is (b). Entries on nodes in the
tree keep the old "before" rules (old place, grantee of a removed grant).

**What does not count yet.** Member links: the journal does not date the access that they give,
so they never satisfy (a) or (b). The feed sends less, never more.

**IDs of gone nodes in entry data** (decision 0003-8, which replaces the open point that stood
here). Entries may carry, in `data`, the IDs of nodes that the subject cannot see, also of gone
nodes (for example `old_parent_id`, `new_parent_id`). IDs are exempt from rules (a) and (b).
Names, paths, tag names, MIME types, sizes and every other attribute of a node that fails (a)
and (b) for the subject never appear. Accepted costs: a UUIDv7 shows when the unseen node was
made (to the millisecond), and the same ID in two entries shows that both concern the same
unseen node.

**Tests.** `journal::feed::tests` (a guest who gets `/a` after `/a/c` was trashed; members who
join during the trash and after the purge; their agents), a journal conformance case for the
memory and Postgres journals, `member_since` in the tenant conformance suite, and the reference
model of the feed property test. Mutations: `feed-gone-node-old-place`,
`feed-gone-node-ancestor-now`, `feed-member-joined-after-purge`.

## Contract diff

All changes to `contracts/openapi.yaml` are wording, plus one optional response field
(`AppPasswordCreated.dav_url`). No path, parameter, required field, security scheme or cookie name
changes. The breaking-change diff (`oasdiff breaking`) against `main` reports nothing.
