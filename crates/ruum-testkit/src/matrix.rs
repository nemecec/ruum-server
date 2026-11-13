//! The permission matrix: the cases that the permission suite must run for every endpoint, from
//! the contract review of step 1. The test harness task turns each case into a test against the
//! API router; until then this list is the definition of done for those cases.

/// The operations that only a signed-in user may call (`x-ruum-user-only: true` in
/// `contracts/openapi.yaml`). For each one the suite calls the endpoint with an agent token whose
/// user is allowed (owner and tenant admin, scope `max_role: manage`, no subtree) and with a
/// share-link token, and expects `403 user_required` both times.
pub const USER_ONLY_OPERATIONS: &[&str] = &[
    "listMyIdentities",
    "listMySessions",
    "updateMyPreferences",
    "endSession",
    "listAppPasswords",
    "createAppPassword",
    "revokeAppPassword",
    "createTenant",
    "updateTenant",
    "updateMember",
    "removeMember",
    "listInvitations",
    "createInvitation",
    "revokeInvitation",
    "acceptInvitation",
    "listAudit",
    "registerAgent",
    "updateAgent",
    "disableAgent",
    "enableAgent",
    "listAgentKeys",
    "createAgentKey",
    "revokeAgentKey",
    "undoAgent",
    "getOAuthClient",
    "oauthConsent",
    // Step 2 (databases, docs/STEP2.md card C-1).
    "confirmColumnClasses",
    "restoreDatabase",
    "createDatabaseAccessGrant",
    "decideDatabaseApproval",
    // Step 3 (docs/STEP3.md card C-3).
    "setAppSecret",
    "addAppDomain",
    "decideAppApproval",
    "createAppApiKey",
    "confirmAppTelemetryFields",
    "computeAppPseudonym",
    "updateProjectPublication",
    "createIssueRevealRequest",
];

/// The batch operations (decision 0003-7): `200` with one result per item, each with the status
/// that the single operation would answer. The suites send one item and judge its status
/// ([`crate::client::send`]); the rules are those of `moveNode` and `attachTag`.
pub const BATCH_OPERATIONS: &[&str] = &["moveNodes", "attachTagToNodes"];

/// Whether `method` and `uri` are a call of a batch operation of the contract.
pub fn is_batch(method: &str, uri: &str) -> bool {
    use std::sync::OnceLock;
    static BATCH: OnceLock<Vec<crate::contract::Operation>> = OnceLock::new();
    BATCH
        .get_or_init(|| {
            crate::contract::operations()
                .into_iter()
                .filter(|o| BATCH_OPERATIONS.contains(&o.id.as_str()))
                .collect()
        })
        .iter()
        .any(|o| o.method == method && o.matches(uri))
}

/// The operations that are specified but not built (`x-ruum-status: planned` in
/// `contracts/openapi.yaml`, decision 0004-21). The suites need a standard request and the
/// isolation and permission cases for each of them, like for any operation; while its route is
/// missing or answers `501`, its checks are reported as planned, not failed, also with
/// `RUUM_REQUIRE_IMPLEMENTED=1`. A data leak in any answer still fails. The task that builds an
/// operation removes its mark in the contract and its name here (a test keeps the two equal).
pub const PLANNED_OPERATIONS: &[&str] = &[
    // Step 2 (databases, docs/STEP2.md).
    "listDatabaseMigrations",
    "applyDatabaseMigration",
    "listDatabaseRestorePoints",
    "createDatabaseRestorePoint",
    "restoreDatabase",
    // Step 3 (web apps, issues, templates; docs/STEP3.md).
    "compareAppDeployment",
    "listAppSecrets",
    "setAppSecret",
    "rotateAppSecret",
    "listAppDomains",
    "addAppDomain",
    "removeAppDomain",
    "listAppRoles",
    "listAppRoleGrants",
    "addAppRoleGrant",
    "removeAppRoleGrant",
    "listAppApprovals",
    "getAppApproval",
    "decideAppApproval",
    "listAppApiKeys",
    "createAppApiKey",
    "revokeAppApiKey",
    "searchAppLogs",
    "getAppMetrics",
    "getAppTrace",
    "listAppTelemetryFields",
    "confirmAppTelemetryFields",
    "computeAppPseudonym",
    "getProjectPublication",
    "updateProjectPublication",
    "listIssues",
    "createIssue",
    "getIssue",
    "updateIssue",
    "listIssueEvents",
    "createIssueRevealRequest",
    "resolveIssueReceipt",
    "decideIssueReveal",
    "listTemplates",
    "createTemplate",
    "getTemplate",
    "instantiateTemplate",
    "getTemplateSetup",
];

/// The operations that a share-link bearer may call (the `Users only` section of
/// `contracts/openapi.yaml`): the node, content, version and diff reads, inside the link's
/// subtree. An `edit` link may also call `putContent` and the upload operations; the fixture link
/// is a `view` link, so they are not listed. Every other operation that needs sign-in answers
/// `403 user_required` to a share-link bearer.
pub const SHARE_LINK_OPERATIONS: &[&str] = &[
    "getRoot",
    "getNode",
    "getNodeByPath",
    "listChildren",
    "downloadContent",
    "getContentUrl",
    "listVersions",
    "getVersion",
    "diffVersions",
    "listDiffPlugins",
];

/// One case of the permission suite.
#[derive(Clone, Copy, Debug)]
pub struct Case {
    pub name: &'static str,
    pub given: &'static str,
    pub expect: &'static str,
}

/// Cases beyond the role-by-endpoint grid.
pub const CASES: &[Case] = &[
    Case {
        name: "agent_cannot_escalate",
        given: "an agent of a tenant admin calls each of USER_ONLY_OPERATIONS, including \
                updateAgent on itself with scope {subtree: null, max_role: manage} and \
                createAgentKey for itself",
        expect: "403 user_required; the agent row and its keys are unchanged",
    },
    Case {
        name: "member_link_same_tenant",
        given: "a user with share on file X in tenant A creates a member link to X in a folder \
                of tenant B (Ruum-Tenant: B)",
        expect: "404 not_found; no node and no collection tuple is written",
    },
    Case {
        name: "remove_member_cleans_up",
        given: "member M has a direct edit grant on folder F and made a share link on F, then \
                an admin removes M",
        expect: "the grant is gone (grant.removed), the link is revoked (share_link.revoked), \
                 M gets 404 on F and the link token resolves to 404",
    },
    Case {
        name: "team_member_manages",
        given: "a team member (not admin) moves and trashes a file another member made",
        expect: "200; with members_can_share false, addGrant by the same member is 403",
    },
    Case {
        name: "editors_can_share",
        given: "guest_edit (edit grant on Projects) calls addGrant with role edit on Projects, \
                addGrant with role manage on Projects, and addGrant on Archive \
                (editors_can_share false); then a manager sets editors_can_share null on Archive \
                and guest_edit calls addGrant on Archive again",
        expect: "201, 403 (a caller grants at most its own role), 403, then 201; a share link \
                 with role edit never shares (addGrant with its token: 403 user_required)",
    },
    Case {
        name: "app_password_is_the_user",
        given: "member makes an app password for tenant A and writes a file through /dav with \
                HTTP Basic (member's email + the password); then uses it against /v1 and against \
                tenant B; then an admin removes member",
        expect: "the journal entry has actor = member (kind user, on_behalf_of null); /v1 \
                 refuses it (401); tenant B refuses it; after removeMember it is revoked (401)",
    },
    Case {
        name: "token_audience_and_type",
        given: "a Ruum-signed token with another aud, or with a typ other than agent, user or \
                share_link",
        expect: "401 unauthenticated",
    },
    Case {
        name: "share_link_token_reads_only",
        given: "a share-link token calls a node read inside the link's subtree, a node read \
                outside it, and listTenants",
        expect: "200, 404, 403 user_required",
    },
];

#[cfg(test)]
mod tests {
    use super::{PLANNED_OPERATIONS, USER_ONLY_OPERATIONS};

    const SPEC: &str = include_str!("../../../contracts/openapi.yaml");
    const MARK: &str = "      x-ruum-user-only: true\n";

    /// The operationId of the operation that holds the mark at `pos`.
    fn operation_at(pos: usize) -> &'static str {
        let before = &SPEC[..pos];
        let start = before
            .rfind("operationId: ")
            .expect("an operationId before the mark")
            + "operationId: ".len();
        let end = start + SPEC[start..].find('\n').unwrap();
        &SPEC[start..end]
    }

    #[test]
    fn user_only_list_matches_the_contract() {
        let mut marked: Vec<&str> = SPEC
            .match_indices(MARK)
            .map(|(i, _)| operation_at(i))
            .collect();
        let mut listed: Vec<&str> = USER_ONLY_OPERATIONS.to_vec();
        marked.sort_unstable();
        listed.sort_unstable();
        assert_eq!(marked, listed);
    }

    #[test]
    fn planned_list_matches_the_contract() {
        let ops = crate::contract::parse(SPEC).expect("the contract scans");
        let mut marked: Vec<&str> = ops
            .iter()
            .filter(|o| o.planned)
            .map(|o| o.id.as_str())
            .collect();
        let mut listed: Vec<&str> = PLANNED_OPERATIONS.to_vec();
        marked.sort_unstable();
        listed.sort_unstable();
        assert_eq!(marked, listed);
    }
}
