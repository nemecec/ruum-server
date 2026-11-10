# Permission matrix

Generated from the rules in `crates/ruum-testkit/src/permissions.rs` by
`RUUM_UPDATE_MATRIX=1 cargo test -p ruum-testkit --test permissions`. Do not edit by hand.

Each cell is what the principal must get for the standard request of the operation
(`crates/ruum-testkit/src/requests.rs`) in tenant A. `ok` = 2xx/3xx, `403`, `404`,
`403/404` = either, `401`, `any` = not decided by the contract (open question).
The principals are described in `crates/ruum-testkit/src/fixtures.rs`. The narrow agent
`agent` acts for `member` with scope `Projects` and `max_role: view`. `share_link` is the
bearer of a `view` share link on `Projects`. Roles are `view < comment < edit < manage`;
`share` is a permission: `manage` always, `edit` where `editors_can_share` is true (not
in `Archive`). Members manage the team tree and share there by the default tenant
policy (`members_can_share`). `member_view`, `member_comment` and `member_edit` are
members with a lower content role (decision 0003-1): that role on the whole team tree.

| operation | anonymous | owner | admin | member | member_view | member_comment | member_edit | guest | guest_view | guest_comment | guest_edit | guest_manage | agent | agent_out | agent_of_guest_view | share_link | rule |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `health` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public (security: []) |
| `getJwks` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public (security: []) |
| `getProtectedResourceMetadata` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public (security: []) |
| `getAuthorizationServerMetadata` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public (security: []) |
| `loginStart` | any | any | any | any | any | any | any | any | any | any | any | any | any | any | any | any | public (security: []) |
| `loginCallback` | any | any | any | any | any | any | any | any | any | any | any | any | any | any | any | any | public (security: []) |
| `logout` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | ends the caller's web session; a bearer token (agents too) has none: 200, ends nothing (decided 0002-5) |
| `getMe` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | every signed-in principal, agents too |
| `updateMyPreferences` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only; the caller's own preferences (agents and share links: 403 user_required) |
| `listMyIdentities` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only (agents and share links: 403 user_required) |
| `listMySessions` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only (agents and share links: 403 user_required) |
| `endSession` | 401 | 404 | 404 | ok | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 403 | 403 | 403 | 403 | users only; only the user of the session (member), others do not see it |
| `listAppPasswords` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only; the caller's own app passwords in the tenant (members and guests) |
| `createAppPassword` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only; the caller's own app passwords in the tenant (members and guests) |
| `revokeAppPassword` | 401 | 404 | 404 | ok | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 403 | 403 | 403 | 403 | users only; only the user who made it (member), others do not see it |
| `listTenants` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | about the caller |
| `createTenant` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only |
| `getTenant` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | members and guests (listTenants shows guest tenants) |
| `updateTenant` | 401 | ok | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin or owner |
| `listMembers` | 401 | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | 403 | ok | ok | 403 | 403 | members and up; guests get 403, the list holds emails (decided 0002-5) |
| `updateMember` | 401 | ok | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin |
| `removeMember` | 401 | ok | ok | 403 | 403 | 403 | 403 | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin, or the member removes themselves (the target is guest) |
| `listInvitations` | 401 | ok | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin |
| `createInvitation` | 401 | ok | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin |
| `revokeInvitation` | 401 | ok | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin |
| `acceptInvitation` | 401 | any | any | any | any | any | any | any | any | any | any | any | 403 | 403 | 403 | 403 | users only, with the token (the answer depends on membership) |
| `previewInvitation` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public; the invitation token in the body is the credential, nothing changes |
| `getLimits` | 401 | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | 403 | ok | ok | 403 | 403 | any member can read them; guests get 403 |
| `listAudit` | 401 | ok | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin |
| `listAgents` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | the caller's agents (admins: all); an agent sees only itself (decided 0002-5) |
| `registerAgent` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only; guests may, not while allow_guests is false (decided 0002-2) |
| `getAgent` | 401 | ok | ok | ok | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 403/404 | 403/404 | 403/404 | 403 | the owner or a tenant admin; others get 404 |
| `updateAgent` | 401 | 403 | 403 | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; only the owner of the agent |
| `disableAgent` | 401 | ok | ok | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; the owner or a tenant admin |
| `enableAgent` | 401 | 403 | 403 | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; only the owner of the agent |
| `listAgentKeys` | 401 | ok | ok | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; the owner or a tenant admin (decided 0002-5) |
| `createAgentKey` | 401 | 403 | 403 | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; only the owner of the agent |
| `revokeAgentKey` | 401 | ok | ok | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; the owner or a tenant admin |
| `listAgentActivity` | 401 | ok | ok | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | the owner or a tenant admin; agents get 403/404 (decided 0002-5) |
| `undoAgent` | 401 | ok | ok | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; the owner or a tenant admin |
| `registerOAuthClient` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public; a registration gives no access (consent does) |
| `getOAuthClient` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only; the public metadata of a client, for the consent screen |
| `oauthAuthorize` | any | any | any | any | any | any | any | any | any | any | any | any | any | any | any | any | public; a redirect to the consent screen, or a 400 page for an unknown client |
| `oauthConsent` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only; creates an agent of the caller in the tenant; guests may, not while allow_guests is false (decided 0002-2) |
| `exchangeToken` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public; the agent key in the body is the credential |
| `getRoot` | 401 | ok | ok | ok | ok | ok | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | view on the root; the root is not secret (403) |
| `createNode` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit on the parent |
| `moveNodes` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | batch of moveNode (decision 0003-7): the status of the item; manage on the node and edit on the new parent |
| `getNodeByPath` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | view on the target is enough, as for getNode (decided 0002-5) |
| `getNode` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view; 404 when the caller cannot see it |
| `purgeNode` | 401 | ok | ok | 403 | 403 | 403 | 403 | 404 | 403 | 403 | 403 | 403 | 403 | 403/404 | 403 | 403 | Needs manage, and admin on a team plan (admin_restore_days > trash_days) |
| `listChildren` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | view on the folder |
| `renameNode` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit on the node |
| `moveNode` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage on the node and edit on the new parent |
| `copyNode` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs view on the node and edit on the new parent |
| `trashNode` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage |
| `restoreNode` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage (inherited from the old parent) |
| `listTrash` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | the trashed roots the caller can manage |
| `emptyTrash` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | purges only the trashed roots the caller may purge (purgeNode); others stay |
| `createUpload` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | edit on the parent folder for a new file |
| `getChunkingParams` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | every principal of the tenant (share links: edit links only) |
| `getUpload` | 401 | 404 | 404 | ok | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 403 | only the actor that started the upload (member) |
| `abortUpload` | 401 | 404 | 404 | ok | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 403 | only the actor that started the upload (member) |
| `putChunk` | 401 | 404 | 404 | ok | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 403 | only the actor that started the upload (member) |
| `completeUpload` | 401 | 404 | 404 | ok | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 403 | only the actor that started the upload (member) |
| `downloadContent` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view |
| `putContent` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit |
| `getContentUrl` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view |
| `listVersions` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view |
| `getVersion` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view |
| `restoreVersion` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit |
| `setVersionLabel` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit |
| `diffVersions` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view |
| `listDiffPlugins` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view |
| `listTags` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | members see all tags; guests only tags on nodes they can see |
| `createTag` | 401 | ok | ok | ok | 403 | 403 | ok | 403 | 403 | 403 | 403 | 403 | 403 | ok | 403 | 403 | members with content role edit or manage, admins, owners; not guests, view or comment members (decision 0003-1); agents need max_role edit (decided 0002-5) |
| `getTag` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 404 | ok | 403 | members, and principals who can see a node with the tag (decided 0002-5) |
| `updateTag` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | 403 | 403 | 403 | 404 | 403 | 403 | members with content role edit or manage, admins, owners; not guests, view or comment members (decision 0003-1); agents need max_role edit (decided 0002-5) |
| `deleteTag` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | 403 | 403 | 404 | 403 | 403 | tenant admins or the creator (member, while it can edit: decision 0003-1); agents need max_role edit (decided 0002-5) |
| `listNodesByTag` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 404 | ok | 403 | members, and principals who can see a node with the tag (decided 0002-5) |
| `attachTagToNodes` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | batch of attachTag (decision 0003-7): the status of the item; edit on the node |
| `listNodeTags` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the node |
| `attachTag` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit on the node |
| `detachTag` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit on the node |
| `listGrants` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view |
| `addGrant` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the node (manage, or edit with editors_can_share) |
| `updateGrant` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the node (manage, or edit with editors_can_share) |
| `removeGrant` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the node (manage, or edit with editors_can_share) |
| `updateNodeSharing` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage (Archive: the node with editors_can_share false) |
| `listShareLinks` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the node (manage, or edit with editors_can_share) |
| `createShareLink` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the node (manage, or edit with editors_can_share) |
| `revokeShareLink` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the node (manage, or edit with editors_can_share) |
| `resolveShareLink` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public; the share link token in the body is the credential |
| `listSharedWithMe` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | the roots of grants to the caller; for agents only inside the scope |
| `listChanges` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | only entries on nodes the caller can see (agents: inside the scope) |
| `getLatestCursor` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | every signed-in principal of the tenant |
| `searchNodes` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | only nodes the caller can see |
| `createDatabase` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | 403 | 403 | 403 | 403/404 | 403 | 403 | Needs edit on the parent folder (agents: max_role edit); guests and agents of guests need guests_create_resources (default false: 403) |
| `getDatabase` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the database |
| `updateDatabaseManifest` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage (settings); an agent may not change data_classes (unchanged here) |
| `getDatabaseSchema` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the database |
| `queryDatabase` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | mode read needs view (agents read masked values); the standard query is a read |
| `listDatabaseQueries` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view; entries are filtered (manage: all; view: own and own agents'; agents: own) (decision 0004-20, UX C-19) |
| `listDatabaseColumns` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the database |
| `suggestColumnClasses` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit on the database |
| `confirmColumnClasses` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403 | 403 | 403 | users only; Needs manage |
| `listDatabaseMigrations` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the database |
| `applyDatabaseMigration` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | live database: Needs manage; an agent also needs write_live and an approval (no agent of the fixture has manage on the database) |
| `listDatabaseBranches` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the database |
| `createDatabaseBranch` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | 403 | 403 | 403 | 403/404 | 403 | 403 | masked branch: Needs edit; guests and agents of guests need guests_create_resources (default false: 403) |
| `listDatabaseRestorePoints` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the database |
| `createDatabaseRestorePoint` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit on the database |
| `restoreDatabase` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs manage; new_node by a guest needs guests_create_resources |
| `listDatabaseAccessGrants` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view; an agent sees only its own grants |
| `createDatabaseAccessGrant` | 401 | 403 | 403 | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; only the user the agent acts for (member; read_personal needs view) |
| `revokeDatabaseAccessGrant` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | ok | 403/404 | 403 | 403 | the user who gave it (member), a person with manage, or the agent that holds it (agent) |
| `requestDatabaseAccessGrant` | 401 | 403 | 403 | 403 | 403 | 403 | 403 | 404 | 403 | 403 | 403 | 403 | ok | 403/404 | ok | 403 | agents only, with view on the database; a person gets 403 (decision 0004-20, UX C-17) |
| `listPendingDatabaseApprovals` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | the requests the caller may decide (agents: their own), across the tenant; only on databases the caller can see |
| `listDatabaseApprovals` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the database |
| `getDatabaseApproval` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | 404 | 403 | Needs view; an agent sees only its own requests (the request is agent's), others get 404 |
| `decideDatabaseApproval` | 401 | 403 | 403 | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; the fixture request is a grant request of agent, which only the agent's user (member) decides (decision 0004-20 C-17); a statement or a migration: the agent's user or a person with manage, with the right it needs |
| `createApp` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | 403 | 403 | 403 | 403/404 | 403 | 403 | Needs edit on the parent folder (agents: max_role edit); guests and agents of guests need guests_create_resources (default false: 403) |
| `getApp` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the app |
| `updateAppSettings` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage; the standard request changes telemetry_mode (agents may; access is users only) |
| `listAppDeployments` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the app |
| `createAppDeployment` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | previews: Needs edit on the app and view on the source file |
| `getAppDeployment` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the app |
| `publishAppDeployment` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit; a person publishes at once (200), an agent gets a pending approval (202); the narrow agents of the fixture have view only |
| `compareAppDeployment` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | telemetry reads need edit on the app (logs can hold personal data, design 9.6) |
| `listAppSecrets` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit (names and states only) |
| `setAppSecret` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403 | 403 | 403 | users only; Needs manage |
| `rotateAppSecret` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage on the app |
| `listAppDomains` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the app |
| `addAppDomain` | 401 | ok | ok | 403 | 403 | 403 | 403 | 404 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs manage, and in a team tenant an owner or admin (the fixture tenants have custom domains approved) |
| `removeAppDomain` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage on the app |
| `listAppRoles` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the app |
| `listAppRoleGrants` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the app |
| `addAppRoleGrant` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the app (or the grants_roles role; no fixture principal holds it) |
| `removeAppRoleGrant` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the app (or the grants_roles role; no fixture principal holds it) |
| `listAppApprovals` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit on the app (pending binding changes and migration names, review R14); an agent sees only its own requests |
| `getAppApproval` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit (review R14); an agent sees only its own requests (the request is agent's), others get 404 |
| `decideAppApproval` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403 | 403 | 403 | users only; the agent's user (member) or a person with manage on the app, with edit on the app |
| `listAppApiKeys` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage on the app |
| `createAppApiKey` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403 | 403 | 403 | users only; Needs manage |
| `revokeAppApiKey` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage on the app |
| `searchAppLogs` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | telemetry reads need edit on the app (logs can hold personal data, design 9.6) |
| `getAppMetrics` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | telemetry reads need edit on the app (logs can hold personal data, design 9.6) |
| `getAppTrace` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | telemetry reads need edit on the app (logs can hold personal data, design 9.6) |
| `listAppTelemetryFields` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | telemetry reads need edit on the app (logs can hold personal data, design 9.6) |
| `confirmAppTelemetryFields` | 401 | ok | ok | ok | 403 | 403 | 403 | 404 | 403 | 403 | 403 | ok | 403 | 403 | 403 | 403 | users only; Needs manage |
| `computeAppPseudonym` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403 | 403 | 403 | users only; Needs edit |
| `getProjectPublication` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the project folder |
| `updateProjectPublication` | 401 | ok | ok | 403 | 403 | 403 | 403 | 404 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs manage on the project and its items, and in a team tenant an owner or admin (the fixture tenants have public apps approved) |
| `listIssues` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | only issues the caller can see (agents: inside the scope) |
| `createIssue` | 401 | ok | ok | ok | 403 | ok | ok | 404 | 403 | ok | ok | ok | 403 | 403/404 | 403 | 403 | Needs comment on the folder (decision 0006, open question 7); the standard request sets no assignee, status or links |
| `getIssue` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view; report attachments need edit |
| `updateIssue` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit on the issue |
| `listIssueEvents` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit on the issue and on its app (both in Projects) |
| `createIssueRevealRequest` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403 | 403 | 403 | users only; Needs edit |
| `resolveIssueReceipt` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public; the receipt token in the body is the credential, nothing changes |
| `decideIssueReveal` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public; the receipt token in the body is the credential (the reporter may change a choice at any time) |
| `listTemplates` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | the gallery and the tenant templates the caller can see |
| `createTemplate` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs view on the source (Archive) and edit on the parent (Projects) |
| `getTemplate` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | tenant template: Needs view |
| `instantiateTemplate` | 401 | ok | ok | ok | 403 | 403 | ok | 404 | 403 | 403 | 403 | 403 | 403 | 403/404 | 403 | 403 | Needs view on the template and edit on the parent; the fixture template makes a database and an app, so guests and agents of guests need guests_create_resources |
| `getTemplateSetup` | 401 | ok | ok | ok | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the new project folder (in Projects) |
