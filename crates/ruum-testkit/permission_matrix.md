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
policy (`members_can_share`).

| operation | anonymous | owner | admin | member | guest | guest_view | guest_comment | guest_edit | guest_manage | agent | agent_out | agent_of_guest_view | share_link | rule |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `health` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public (security: []) |
| `getJwks` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public (security: []) |
| `getProtectedResourceMetadata` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public (security: []) |
| `getAuthorizationServerMetadata` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public (security: []) |
| `loginStart` | any | any | any | any | any | any | any | any | any | any | any | any | any | public (security: []) |
| `loginCallback` | any | any | any | any | any | any | any | any | any | any | any | any | any | public (security: []) |
| `logout` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | any | any | any | 403 | sessions; open question: an agent bearer token |
| `getMe` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | every signed-in principal, agents too |
| `listMyIdentities` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only (agents and share links: 403 user_required) |
| `listMySessions` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only (agents and share links: 403 user_required) |
| `endSession` | 401 | 404 | 404 | ok | 404 | 404 | 404 | 404 | 404 | 403 | 403 | 403 | 403 | users only; only the user of the session (member), others do not see it |
| `listAppPasswords` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only; the caller's own app passwords in the tenant (members and guests) |
| `createAppPassword` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only; the caller's own app passwords in the tenant (members and guests) |
| `revokeAppPassword` | 401 | 404 | 404 | ok | 404 | 404 | 404 | 404 | 404 | 403 | 403 | 403 | 403 | users only; only the user who made it (member), others do not see it |
| `listTenants` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | about the caller |
| `createTenant` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only |
| `getTenant` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | members and guests (listTenants shows guest tenants) |
| `updateTenant` | 401 | ok | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin or owner |
| `listMembers` | 401 | ok | ok | ok | 403 | 403 | 403 | 403 | 403 | ok | ok | 403 | 403 | assumption: members and up; guests get 403 |
| `updateMember` | 401 | ok | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin |
| `removeMember` | 401 | ok | ok | 403 | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin, or the member removes themselves (the target is guest) |
| `listInvitations` | 401 | ok | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin |
| `createInvitation` | 401 | ok | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin |
| `revokeInvitation` | 401 | ok | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin |
| `acceptInvitation` | 401 | any | any | any | any | any | any | any | any | 403 | 403 | 403 | 403 | users only, with the token (the answer depends on membership) |
| `getLimits` | 401 | ok | ok | ok | 403 | 403 | 403 | 403 | 403 | ok | ok | 403 | 403 | any member can read them; guests get 403 |
| `listAudit` | 401 | ok | ok | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | 403 | users only; Needs admin |
| `listAgents` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | any | any | any | 403 | the caller's agents (admins: all); open question: an agent caller |
| `registerAgent` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only; assumption: guests may register agents |
| `getAgent` | 401 | ok | ok | ok | 404 | 404 | 404 | 404 | 404 | 403/404 | 403/404 | 403/404 | 403 | the owner or a tenant admin; others get 404 |
| `updateAgent` | 401 | 403 | 403 | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; only the owner of the agent |
| `disableAgent` | 401 | ok | ok | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; the owner or a tenant admin |
| `enableAgent` | 401 | 403 | 403 | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; only the owner of the agent |
| `listAgentKeys` | 401 | ok | ok | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; assumption: the owner or a tenant admin |
| `createAgentKey` | 401 | 403 | 403 | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; only the owner of the agent |
| `revokeAgentKey` | 401 | ok | ok | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; the owner or a tenant admin |
| `listAgentActivity` | 401 | ok | ok | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | assumption: the owner or a tenant admin |
| `undoAgent` | 401 | ok | ok | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403 | 403 | 403 | 403 | users only; the owner or a tenant admin |
| `registerOAuthClient` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public; a registration gives no access (consent does) |
| `getOAuthClient` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only; the public metadata of a client, for the consent screen |
| `oauthAuthorize` | any | any | any | any | any | any | any | any | any | any | any | any | any | public; a redirect to the consent screen, or a 400 page for an unknown client |
| `oauthConsent` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | 403 | 403 | 403 | 403 | users only; creates an agent of the caller in the tenant; assumption: guests may, as for registerAgent |
| `exchangeToken` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public; the agent key in the body is the credential |
| `getRoot` | 401 | ok | ok | ok | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | 403/404 | view on the root; the root is not secret (403) |
| `createNode` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit on the parent |
| `getNodeByPath` | 401 | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | assumption: view on the target is enough, as for getNode |
| `getNode` | 401 | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view; 404 when the caller cannot see it |
| `purgeNode` | 401 | ok | ok | 403 | 404 | 403 | 403 | 403 | 403 | 403 | 403/404 | 403 | 403 | Needs manage, and admin on a team plan (admin_restore_days > trash_days) |
| `listChildren` | 401 | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | view on the folder |
| `renameNode` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit on the node |
| `moveNode` | 401 | ok | ok | ok | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage on the node and edit on the new parent |
| `copyNode` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs view on the node and edit on the new parent |
| `trashNode` | 401 | ok | ok | ok | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage |
| `restoreNode` | 401 | ok | ok | ok | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage (inherited from the old parent) |
| `listTrash` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | the trashed roots the caller can manage |
| `emptyTrash` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | purges only the trashed roots the caller may purge (purgeNode); others stay |
| `createUpload` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | edit on the parent folder for a new file |
| `getChunkingParams` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | every principal of the tenant (share links: edit links only) |
| `getUpload` | 401 | 404 | 404 | ok | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 403 | only the actor that started the upload (member) |
| `abortUpload` | 401 | 404 | 404 | ok | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 403 | only the actor that started the upload (member) |
| `putChunk` | 401 | 404 | 404 | ok | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 403 | only the actor that started the upload (member) |
| `completeUpload` | 401 | 404 | 404 | ok | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 404 | 403 | only the actor that started the upload (member) |
| `downloadContent` | 401 | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view |
| `putContent` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit |
| `getContentUrl` | 401 | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view |
| `listVersions` | 401 | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view |
| `getVersion` | 401 | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view |
| `restoreVersion` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit |
| `setVersionLabel` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit |
| `diffVersions` | 401 | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view |
| `listDiffPlugins` | 401 | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | ok | Needs view |
| `listTags` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | members see all tags; guests only tags on nodes they can see |
| `createTag` | 401 | ok | ok | ok | 403 | 403 | 403 | 403 | 403 | 403 | ok | 403 | 403 | any member, not guests; assumption: agents need max_role edit |
| `getTag` | 401 | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 404 | ok | 403 | assumption: members, and principals who can see a node with the tag |
| `updateTag` | 401 | ok | ok | ok | 404 | 403 | 403 | 403 | 403 | 403 | 404 | 403 | 403 | any member, not guests; assumption: agents need max_role edit |
| `deleteTag` | 401 | ok | ok | ok | 404 | 403 | 403 | 403 | 403 | 403 | 404 | 403 | 403 | tenant admins or the creator (member); assumption: agents need max_role edit |
| `listNodesByTag` | 401 | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 404 | ok | 403 | assumption: members, and principals who can see a node with the tag |
| `listNodeTags` | 401 | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view on the node |
| `attachTag` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit on the node |
| `detachTag` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs edit on the node |
| `listGrants` | 401 | ok | ok | ok | 404 | ok | ok | ok | ok | ok | 403/404 | ok | 403 | Needs view |
| `addGrant` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the node (manage, or edit with editors_can_share) |
| `updateGrant` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the node (manage, or edit with editors_can_share) |
| `removeGrant` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the node (manage, or edit with editors_can_share) |
| `updateNodeSharing` | 401 | ok | ok | ok | 404 | 403 | 403 | 403 | ok | 403 | 403/404 | 403 | 403 | Needs manage (Archive: the node with editors_can_share false) |
| `listShareLinks` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the node (manage, or edit with editors_can_share) |
| `createShareLink` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the node (manage, or edit with editors_can_share) |
| `revokeShareLink` | 401 | ok | ok | ok | 404 | 403 | 403 | ok | ok | 403 | 403/404 | 403 | 403 | Needs share on the node (manage, or edit with editors_can_share) |
| `resolveShareLink` | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | public; the share link token in the body is the credential |
| `listSharedWithMe` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | the roots of grants to the caller; for agents only inside the scope |
| `listChanges` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | only entries on nodes the caller can see (agents: inside the scope) |
| `getLatestCursor` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | every signed-in principal of the tenant |
| `searchNodes` | 401 | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | ok | 403 | only nodes the caller can see |
