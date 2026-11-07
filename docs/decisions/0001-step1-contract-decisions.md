# 0001: Step 1 contract decisions

Status: decided by the architect on 2026-11-06. These decisions close the "Open points for the
architect" of `docs/STEP1.md`. Do not reopen them (`docs/DESIGN.md`, section 17).

## 1. Roles follow Google Drive; `share` is a permission

- The roles are `view < comment < edit < manage`. A higher role includes the lower ones. There is
  no `share` role and no `sharer` relation.
- `share` (change grants and share links) is a **permission**:
  - `manage` always has it;
  - `edit` has it while the node's effective setting `editors_can_share` is true. The default is
    true. A manager sets it per node (`updateNodeSharing`: `true`, `false` or `null` to inherit),
    and children inherit it until a child sets its own value. This is Drive's "Editors can change
    permissions and share";
  - in a team tenant, the policy `members_can_share` keeps its meaning for the team root: members
    get `manage` from the team root, and `share` from it only while the policy is true. Owners
    and admins always share;
  - share links never have it. A share link never gives `manage`.
- A caller grants at most its own role on the node: a caller who shares through `edit` grants up
  to `edit`; a manager grants up to `manage`. A share link role is at most the caller's role.
- Agents (scope): with `max_role` `manage` the agent shares where its user shares; with
  `max_role` `edit` only where its user shares and the effective `editors_can_share` is true;
  below `edit` never.
- Changes: `contracts/authz/schema.zed` (new relations `team`, `link_viewer`, `link_commenter`,
  `link_editor`, `editors_share_off`, `editors_share_on`; tenant relation `members_can_share`;
  permission `share` built from them), `schema.test.yaml`, `Role` in `openapi.yaml`, events and
  Rust (`ruum_core::authz::Role`, new `Permission`), the SQL enum `grant_role`, the column
  `nodes.editors_can_share`, the operation `updateNodeSharing` and the event
  `node.sharing_updated`, the Node fields `my_can_share`, `editors_can_share` and
  `editors_can_share_effective`, and the permission matrix of `ruum-testkit`.

## 2. Tenant selection with the `Ruum-Tenant` header

Accepted as designed. Node, tag, upload, agent, change, search and app password calls name the
tenant in `Ruum-Tenant`; tenant administration uses `/v1/tenants/{tenant_id}`. The header is
optional for agent and share-link tokens (bound to one tenant; if sent, it must match) and
required for users.

## 3. Global identity tables; `oauth_clients` without `tenant_id`

Accepted. `users`, `identities` and `sessions` are global by nature; their `tenant_id` is the
user's home (personal) tenant, and the RLS policies also let a tenant see its members.
`oauth_clients` is the one table without `tenant_id`: a registration exists before a user picks a
tenant and holds no tenant data. Only system lookups read it.

## 4. Team root grants

Accepted as decided in the contract review, adapted to decision 1. On the root of a team tenant:
owners and admins get `manager` (`node:<root>#manager@tenant:<id>#administer`); every member
gets `manage` through `node:<root>#team@tenant:<id>`; members share there while
`tenant:<id>#members_can_share@user:*` exists (the policy `members_can_share`, default true). No
tuple names the creator of a node.

**Amended by decision 0003-1 (content roles).** A member gets its *content role* from the team
root, not always `manage`: `team_manager`, `team_editor`, `team_commenter` or `team_viewer` of
`schema.zed`. The default content role is `manage`, so a member without a `tenant#content_*`
tuple manages, as above. Members with `edit` share as editors (while `members_can_share` is
true and the effective `editors_can_share` is true); members with `view` or `comment` never
share from the team root.

## 5. Cross-tenant links, member-link listing, known gaps

- Cross-tenant links are refused in step 1 (database trigger). Accepted.
- Showing member links that point at a node in `listGrants`: later.
- The known gaps listed in `docs/STEP1.md` are accepted for step 1.

## 6. MCP tool list

`contracts/mcp/tools.yaml` lists one tool per step 1 REST operation that makes sense for an
agent: a snake_case `verb_noun` name, the `operationId`, a one-line description for an AI agent,
and the input as a JSON pointer to the OpenAPI operation. Every other operation is in
`not_exposed` with a reason: user-only operations (`x-ruum-user-only`), and OAuth, session and
upload plumbing. `crates/ruum-mcp/tests/tools_contract.rs` checks that every tool maps to an
existing `operationId` and that every operation has a decision.

## 7. WebDAV authentication: app passwords

- WebDAV uses per-user **app passwords** (design 4.2, machine identities). A user creates an app
  password for one tenant, with a name and an optional expiry. It is shown once, stored only as
  SHA-256, starts with `ruum_app_`, and can be revoked.
- An app password is the user, not an agent: every write through it records `actor` = the user
  (`on_behalf_of` null). It has the user's rights in that one tenant, and works only for WebDAV
  (the REST API refuses it).
- WebDAV signs in with HTTP Basic: the user's email and the app password. Agents use WebDAV with
  their agent token (Bearer).
- `removeMember` revokes the member's app passwords for that tenant.
- Changes: the table `app_passwords` (with `tenant_id` and RLS) in `migrations/0001_core.sql`;
  the operations `listAppPasswords`, `createAppPassword` and `revokeAppPassword`
  (`/v1/me/app-passwords`, user-only, tag `auth`, area 4); isolation and matrix cases in
  `ruum-testkit`.
