//! The standard request of every operation against the resources of one tenant fixture.
//!
//! Both suites start from these requests: the permission matrix sends them as each principal of
//! the same tenant; the isolation suite sends them as a principal of the other tenant. Bodies are
//! valid for the contract, so a real handler gets past validation and must decide on access.
//!
//! A new operation in `contracts/openapi.yaml` needs a new arm here (the suites fail until it
//! has one).

use crate::client::RequestSpec;
use crate::fixtures::{TenantFixture, Who};
use axum::http::Method;
use serde_json::json;

/// BLAKE3 of the empty input.
pub const EMPTY_HASH: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

/// The chunking parameter set that the standard upload names (an example ID of `ChunkingParams`
/// in the contract). The real server publishes its sets through `getChunkingParams`; if this ID
/// is not among them, change it here.
pub const CHUNKING: &str = "fastcdc-2020-16k-32k-64k";

/// A PKCE `S256` code challenge (43 characters).
pub const CODE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

/// A fixed time in the past, for `since` parameters.
pub const SINCE: &str = "2026-01-01T00:00:00Z";

/// A time inside the point-in-time window of a database made by the fixture (the restore of
/// `restoreDatabase`). The window is 7 days; card D-4 replaces this with a time from the
/// fixture database's window when it seeds one.
pub const SINCE_RECENT: &str = "2026-11-06T00:00:00Z";

/// The standard request of `op` against tenant `t`. `None` if the operation is unknown here.
pub fn standard(op: &str, t: &TenantFixture) -> Option<RequestSpec> {
    let tid = t.id;
    let agent = t.agent_id(Who::Agent);
    let node = |n: ruum_core::NodeId, rest: &str| format!("/v1/nodes/{n}{rest}");
    let db = |rest: &str| format!("/v1/databases/{}{rest}", t.database);
    let plan = t.plan;
    let r = match op {
        // ------------------------------------------------------------ system and auth
        "health" => RequestSpec::get("/healthz"),
        "getJwks" => RequestSpec::get("/.well-known/jwks.json"),
        "getProtectedResourceMetadata" => {
            RequestSpec::get("/.well-known/oauth-protected-resource")
        }
        "getAuthorizationServerMetadata" => {
            RequestSpec::get("/.well-known/oauth-authorization-server")
        }
        "loginStart" => RequestSpec::get("/v1/auth/login?return_to=%2F"),
        "loginCallback" => RequestSpec::get("/v1/auth/callback?code=test-code&state=test-state"),
        "logout" => RequestSpec::post("/v1/auth/logout"),
        "getMe" => RequestSpec::get("/v1/me"),
        "listMyIdentities" => RequestSpec::get("/v1/me/identities"),
        "listMySessions" => RequestSpec::get("/v1/me/sessions"),
        "updateMyPreferences" => RequestSpec::new(Method::PATCH, "/v1/me/preferences")
            .json(json!({ "locale": "et" })),
        "endSession" => RequestSpec::new(Method::DELETE, format!("/v1/me/sessions/{}", t.session)),
        "listAppPasswords" => RequestSpec::get("/v1/me/app-passwords").tenant(tid),
        "createAppPassword" => RequestSpec::post("/v1/me/app-passwords")
            .tenant(tid)
            .json(json!({ "name": format!("{}-finder", t.prefix) })),
        "revokeAppPassword" => RequestSpec::new(
            Method::DELETE,
            format!("/v1/me/app-passwords/{}", t.app_password),
        )
        .tenant(tid),

        // ------------------------------------------------------------ tenants
        "listTenants" => RequestSpec::get("/v1/tenants"),
        "createTenant" => {
            RequestSpec::post("/v1/tenants").json(json!({ "name": format!("{}-new-team", t.prefix) }))
        }
        "getTenant" => RequestSpec::get(format!("/v1/tenants/{tid}")),
        "updateTenant" => RequestSpec::new(Method::PATCH, format!("/v1/tenants/{tid}"))
            .json(json!({ "name": format!("{}-renamed", t.prefix) })),
        "listMembers" => RequestSpec::get(format!("/v1/tenants/{tid}/members")),
        "updateMember" => RequestSpec::new(
            Method::PATCH,
            format!("/v1/tenants/{tid}/members/{}", t.user_id(Who::Guest)),
        )
        .json(json!({ "role": "member" })),
        "removeMember" => RequestSpec::new(
            Method::DELETE,
            format!("/v1/tenants/{tid}/members/{}", t.user_id(Who::Guest)),
        ),
        "listInvitations" => RequestSpec::get(format!("/v1/tenants/{tid}/invitations")),
        "createInvitation" => RequestSpec::post(format!("/v1/tenants/{tid}/invitations"))
            .json(json!({ "email": format!("{}-second-invitee@example.test", t.prefix), "role": "member" })),
        "revokeInvitation" => RequestSpec::new(
            Method::DELETE,
            format!("/v1/tenants/{tid}/invitations/{}", t.invitation),
        ),
        "acceptInvitation" => RequestSpec::post("/v1/invitations/accept")
            .json(json!({ "token": t.invitation_token })),
        "getLimits" => RequestSpec::get(format!("/v1/tenants/{tid}/limits")),
        "listAudit" => RequestSpec::get(format!("/v1/tenants/{tid}/audit")),

        // ------------------------------------------------------------ agents
        "listAgents" => RequestSpec::get("/v1/agents").tenant(tid),
        "registerAgent" => RequestSpec::post("/v1/agents").tenant(tid).json(json!({
            "name": format!("{}-new-agent", t.prefix),
            "scope": { "subtree": t.projects, "max_role": "view" },
        })),
        "getAgent" => RequestSpec::get(format!("/v1/agents/{agent}")).tenant(tid),
        "updateAgent" => RequestSpec::new(Method::PATCH, format!("/v1/agents/{agent}"))
            .tenant(tid)
            .json(json!({ "name": format!("{}-renamed-agent", t.prefix) })),
        "disableAgent" => RequestSpec::post(format!("/v1/agents/{agent}/disable")).tenant(tid),
        "enableAgent" => RequestSpec::post(format!("/v1/agents/{agent}/enable")).tenant(tid),
        "listAgentKeys" => RequestSpec::get(format!("/v1/agents/{agent}/keys")).tenant(tid),
        "createAgentKey" => RequestSpec::post(format!("/v1/agents/{agent}/keys"))
            .tenant(tid)
            .json(json!({ "name": "second key" })),
        "revokeAgentKey" => RequestSpec::new(
            Method::DELETE,
            format!("/v1/agents/{agent}/keys/{}", t.agent_key),
        )
        .tenant(tid),
        "listAgentActivity" => {
            RequestSpec::get(format!("/v1/agents/{agent}/activity")).tenant(tid)
        }
        "undoAgent" => RequestSpec::post(format!("/v1/agents/{agent}/undo"))
            .tenant(tid)
            .json(json!({ "since": SINCE, "dry_run": true })),
        "registerOAuthClient" => RequestSpec::post("/v1/oauth/register").json(json!({
            "client_name": format!("{}-mcp-client", t.prefix),
            "redirect_uris": [t.oauth_redirect_uri],
        })),
        "getOAuthClient" => RequestSpec::get(format!("/v1/oauth/clients/{}", t.oauth_client_id)),
        "oauthAuthorize" => RequestSpec::get(format!(
            "/v1/oauth/authorize?response_type=code&client_id={}&redirect_uri={}\
             &code_challenge={CODE_CHALLENGE}&code_challenge_method=S256&state=test-state",
            t.oauth_client_id,
            urlencode(&t.oauth_redirect_uri),
        )),
        "oauthConsent" => RequestSpec::post("/v1/oauth/consent").json(json!({
            "request": t.oauth_request,
            "approve": true,
            "tenant_id": tid,
            "new_agent": {
                "name": format!("{}-oauth-agent", t.prefix),
                "scope": { "subtree": t.projects, "max_role": "view" },
            },
        })),
        "exchangeToken" => RequestSpec::post("/v1/oauth/token").form(format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange\
             &subject_token={}\
             &subject_token_type=urn%3Aruum%3Aparams%3Aoauth%3Atoken-type%3Aapi-key",
            t.agent_key_secret
        )),

        // ------------------------------------------------------------ nodes
        "getRoot" => RequestSpec::get("/v1/nodes/root").tenant(tid),
        "createNode" => RequestSpec::post("/v1/nodes").tenant(tid).json(json!({
            "parent_id": t.projects, "name": "new folder", "kind": "folder",
        })),
        "getNodeByPath" => {
            RequestSpec::get(format!("/v1/nodes/by-path/Projects/{}", t.plan_name)).tenant(tid)
        }
        "getNode" => RequestSpec::get(node(plan, "")).tenant(tid),
        "listChildren" => RequestSpec::get(node(t.projects, "/children")).tenant(tid),
        "renameNode" => RequestSpec::post(node(plan, "/rename"))
            .tenant(tid)
            .json(json!({ "name": format!("{}-renamed.txt", t.prefix) })),
        "moveNode" => RequestSpec::post(node(plan, "/move"))
            .tenant(tid)
            .json(json!({ "parent_id": t.archive })),
        "moveNodes" => RequestSpec::post("/v1/nodes/move").tenant(tid).json(json!({
            "items": [{ "node_id": plan, "parent_id": t.archive }],
        })),
        "copyNode" => RequestSpec::post(node(plan, "/copy"))
            .tenant(tid)
            .json(json!({ "parent_id": t.archive })),
        "trashNode" => RequestSpec::post(node(plan, "/trash")).tenant(tid),
        "restoreNode" => RequestSpec::post(node(t.old, "/restore"))
            .tenant(tid)
            .json(json!({})),
        "purgeNode" => RequestSpec::new(Method::DELETE, node(t.old, "")).tenant(tid),
        "listTrash" => RequestSpec::get("/v1/trash").tenant(tid),
        "emptyTrash" => RequestSpec::post("/v1/trash/empty").tenant(tid),

        // ------------------------------------------------------------ uploads and content
        "createUpload" => RequestSpec::post("/v1/uploads").tenant(tid).json(json!({
            "target": "new_file",
            "parent_id": t.projects,
            "name": "uploaded.txt",
            "size": 0,
            "content_hash": EMPTY_HASH,
            "chunking": CHUNKING,
            "chunks": [],
        })),
        "getChunkingParams" => {
            RequestSpec::get("/v1/uploads/params?size=1048576&content_type=text%2Fplain")
                .tenant(tid)
        }
        "getUpload" => RequestSpec::get(format!("/v1/uploads/{}", t.upload_chunked)).tenant(tid),
        "abortUpload" => RequestSpec::new(
            Method::DELETE,
            format!("/v1/uploads/{}", t.upload_chunked),
        )
        .tenant(tid),
        "putChunk" => RequestSpec::new(
            Method::PUT,
            format!("/v1/uploads/{}/chunks/{}", t.upload_chunked, t.chunk_hash()),
        )
        .tenant(tid)
        .bytes(t.chunk.clone()),
        "completeUpload" => {
            RequestSpec::post(format!("/v1/uploads/{}/complete", t.upload_empty)).tenant(tid)
        }
        "downloadContent" => RequestSpec::get(node(plan, "/content")).tenant(tid),
        "getContentUrl" => RequestSpec::get(node(plan, "/content-url")).tenant(tid),
        "putContent" => RequestSpec::new(Method::PUT, node(plan, "/content"))
            .tenant(tid)
            .header("content-type", "text/plain")
            .bytes(b"new content".to_vec()),

        // ------------------------------------------------------------ versions and diff
        "listVersions" => RequestSpec::get(node(plan, "/versions")).tenant(tid),
        "getVersion" => RequestSpec::get(node(plan, &format!("/versions/{}", t.v1))).tenant(tid),
        "restoreVersion" => {
            RequestSpec::post(node(plan, &format!("/versions/{}/restore", t.v1))).tenant(tid)
        }
        "setVersionLabel" => {
            RequestSpec::new(Method::PUT, node(plan, &format!("/versions/{}/label", t.v1)))
                .tenant(tid)
                .json(json!({ "label": "first draft" }))
        }
        "diffVersions" => {
            RequestSpec::get(node(plan, &format!("/diff?from={}&to={}", t.v1, t.v2))).tenant(tid)
        }
        "listDiffPlugins" => RequestSpec::get(node(plan, "/diff/plugins")).tenant(tid),

        // ------------------------------------------------------------ tags
        "listTags" => RequestSpec::get("/v1/tags").tenant(tid),
        "createTag" => RequestSpec::post("/v1/tags")
            .tenant(tid)
            .json(json!({ "name": format!("{}-new-tag", t.prefix) })),
        "getTag" => RequestSpec::get(format!("/v1/tags/{}", t.tag)).tenant(tid),
        "updateTag" => RequestSpec::new(Method::PATCH, format!("/v1/tags/{}", t.tag))
            .tenant(tid)
            .json(json!({ "name": format!("{}-renamed-tag", t.prefix) })),
        "deleteTag" => {
            RequestSpec::new(Method::DELETE, format!("/v1/tags/{}", t.tag)).tenant(tid)
        }
        "listNodesByTag" => RequestSpec::get(format!("/v1/tags/{}/nodes", t.tag)).tenant(tid),
        "listNodeTags" => RequestSpec::get(node(plan, "/tags")).tenant(tid),
        "attachTag" => {
            RequestSpec::new(Method::PUT, node(t.archive, &format!("/tags/{}", t.tag))).tenant(tid)
        }
        "attachTagToNodes" => RequestSpec::post(format!("/v1/tags/{}/nodes", t.tag))
            .tenant(tid)
            .json(json!({ "node_ids": [t.archive] })),
        "detachTag" => {
            RequestSpec::new(Method::DELETE, node(plan, &format!("/tags/{}", t.tag))).tenant(tid)
        }

        // ------------------------------------------------------------ sharing
        "listGrants" => RequestSpec::get(node(t.projects, "/grants")).tenant(tid),
        "addGrant" => RequestSpec::post(node(t.projects, "/grants"))
            .tenant(tid)
            .json(json!({
                "grantee": { "type": "user", "id": t.user_id(Who::Guest) },
                "role": "view",
                "notify": false,
            })),
        "updateGrant" => RequestSpec::new(
            Method::PATCH,
            node(t.projects, &format!("/grants/{}", t.grant)),
        )
        .tenant(tid)
        .json(json!({ "role": "comment" })),
        "removeGrant" => RequestSpec::new(
            Method::DELETE,
            node(t.projects, &format!("/grants/{}", t.grant)),
        )
        .tenant(tid),
        "updateNodeSharing" => RequestSpec::new(Method::PATCH, node(t.archive, "/sharing"))
            .tenant(tid)
            .json(json!({ "editors_can_share": null })),
        "listShareLinks" => RequestSpec::get(node(t.projects, "/share-links")).tenant(tid),
        "createShareLink" => RequestSpec::post(node(t.projects, "/share-links"))
            .tenant(tid)
            .json(json!({ "role": "view" })),
        "revokeShareLink" => RequestSpec::new(
            Method::DELETE,
            format!("/v1/share-links/{}", t.share_link),
        )
        .tenant(tid),
        "resolveShareLink" => RequestSpec::post("/v1/share-links/resolve")
            .json(json!({ "token": t.share_link_token })),
        "listSharedWithMe" => RequestSpec::get("/v1/shared-with-me"),

        // ------------------------------------------------------------ changes and search
        "listChanges" => RequestSpec::get("/v1/changes").tenant(tid),
        "getLatestCursor" => RequestSpec::get("/v1/changes/latest").tenant(tid),
        "searchNodes" => RequestSpec::get("/v1/search?q=secret-plan").tenant(tid),

        // ------------------------------------------------------------ databases (step 2, planned)
        "createDatabase" => RequestSpec::post("/v1/databases").tenant(tid).json(json!({
            "parent_id": t.projects,
            "name": "new database",
            "manifest": { "engine": "sqlite", "version": "3", "plan": "shared-small" },
        })),
        "getDatabase" => RequestSpec::get(db("")).tenant(tid),
        "updateDatabaseManifest" => RequestSpec::new(Method::PUT, db("/manifest"))
            .tenant(tid)
            .json(json!({
                "manifest": { "engine": "sqlite", "version": "3", "plan": "shared-small" },
                "label": "same settings",
            })),
        "getDatabaseSchema" => RequestSpec::get(db("/schema")).tenant(tid),
        "queryDatabase" => RequestSpec::post(db("/query"))
            .tenant(tid)
            .json(json!({ "sql": "select count(*) from plants", "mode": "read" })),
        "listDatabaseQueries" => RequestSpec::get(db("/queries")).tenant(tid),
        "listDatabaseColumns" => RequestSpec::get(db("/columns")).tenant(tid),
        "suggestColumnClasses" => RequestSpec::post(db("/columns/suggestions"))
            .tenant(tid)
            .json(json!({ "suggestions": [{
                "schema": "main", "table": "plants", "column": "latin_name",
                "class": "normal", "reason": "botanical names, not about people",
            }] })),
        "confirmColumnClasses" => RequestSpec::post(db("/columns/confirm"))
            .tenant(tid)
            .json(json!({ "columns": [{
                "schema": "main", "table": "plants", "column": "latin_name", "class": "normal",
            }] })),
        "listDatabaseMigrations" => RequestSpec::get(db("/migrations")).tenant(tid),
        "applyDatabaseMigration" => RequestSpec::post(db("/migrations"))
            .tenant(tid)
            .json(json!({
                "name": "0002_add_plant_notes",
                "up": "alter table plants add column notes text",
                "data_classes": { "main.plants.notes": "personal" },
            })),
        "listDatabaseBranches" => RequestSpec::get(db("/branches")).tenant(tid),
        "createDatabaseBranch" => RequestSpec::post(db("/branches"))
            .tenant(tid)
            .json(json!({ "name": "garden preview", "data": "masked" })),
        "listDatabaseRestorePoints" => RequestSpec::get(db("/restore-points")).tenant(tid),
        "createDatabaseRestorePoint" => RequestSpec::post(db("/restore-points"))
            .tenant(tid)
            .json(json!({ "label": "before import" })),
        "restoreDatabase" => RequestSpec::post(db("/restore")).tenant(tid).json(json!({
            "at": SINCE_RECENT, "target": "new_node", "name": "garden restored",
        })),
        "listDatabaseAccessGrants" => RequestSpec::get(db("/access-grants")).tenant(tid),
        "createDatabaseAccessGrant" => RequestSpec::post(db("/access-grants"))
            .tenant(tid)
            .json(json!({
                "kind": "read_personal", "agent_id": agent, "duration_minutes": 60,
                "reason": "match customer names",
            })),
        "revokeDatabaseAccessGrant" => {
            RequestSpec::new(Method::DELETE, db(&format!("/access-grants/{}", t.database_grant)))
                .tenant(tid)
        }
        "requestDatabaseAccessGrant" => RequestSpec::post(db("/access-grant-requests"))
            .tenant(tid)
            .json(json!({
                "kind": "read_personal", "duration_minutes": 60,
                "reason": "match customer names",
            })),
        "listPendingDatabaseApprovals" => RequestSpec::get("/v1/database-approvals").tenant(tid),
        "listDatabaseApprovals" => RequestSpec::get(db("/approvals")).tenant(tid),
        "getDatabaseApproval" => {
            RequestSpec::get(db(&format!("/approvals/{}", t.database_approval))).tenant(tid)
        }
        "decideDatabaseApproval" => RequestSpec::post(db(&format!(
            "/approvals/{}/decision",
            t.database_approval
        )))
        .tenant(tid)
        .json(json!({ "decision": "approve", "comment": "ok, the old rows can go" })),

        _ => return None,
    };
    Some(r)
}

/// Percent-encodes a query parameter value (RFC 3986 unreserved characters stay).
pub fn urlencode(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
