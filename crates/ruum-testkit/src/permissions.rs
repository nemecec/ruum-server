//! The permission matrix (`docs/DESIGN.md`, sections 3.2, 3.3 and 18.2).
//!
//! For every operation, [`rule`] says what each principal of the fixture tenant may do with the
//! standard request of [`crate::requests::standard`]. The rules come from:
//!
//! - the roles of section 3.2 (`view < comment < edit < manage`, inherited down the tree, grants
//!   only add rights), the `share` permission of decision 0001-1 (`manage` always; `edit` where
//!   `editors_can_share` is true; [`crate::fixtures::Who::can_share`]) and the "Needs ..." lines
//!   of `contracts/openapi.yaml`;
//! - the tenant roles (`guest < member < admin < owner`) of the contract;
//! - the agent rule of section 3.3: the agent's right is the user's right intersected with the
//!   agent's scope ([`crate::fixtures::Who::role_on`]);
//! - the `Users only` section of the contract: agents and share-link bearers get
//!   `403 user_required` on the operations marked `x-ruum-user-only`, and share-link bearers may
//!   call only the reads of [`crate::matrix::SHARE_LINK_OPERATIONS`] ([`contract_wide`]).
//!
//! The rules expand to a table (principal x operation), which is kept in
//! `crates/ruum-testkit/permission_matrix.md` for review. A test fails when the table and the
//! rules disagree; `RUUM_UPDATE_MATRIX=1` writes the file again. Change a rule only with the
//! architect (design section 18.4); a rule marked "assumption" in its note is a reading of the
//! contract that a person should confirm.

use crate::client::{self, Response};
use crate::fixtures::{self, Target, TenantFixture, Who};
use crate::matrix::{SHARE_LINK_OPERATIONS, USER_ONLY_OPERATIONS};
use crate::report::{Outcome, Report};
use crate::requests::standard;
use axum::http::StatusCode;
use futures::stream::{self, StreamExt};
use ruum_core::authz::Role;
use ruum_core::tenants::MemberRole;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::OnceLock;

/// What one principal must get for one operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// A `2xx` or `3xx` answer.
    Allow,
    /// `403`: the principal can see the resource but lacks the right (or the agent scope).
    Forbidden,
    /// `404`: the principal cannot see the resource, so its existence must not leak.
    Hidden,
    /// `403` or `404`.
    Denied,
    /// `401`: no credentials.
    Unauthenticated,
    /// Not decided by the contract; any answer below `500`. Listed as an open question.
    Either,
}

impl Access {
    pub fn accepts(self, s: StatusCode) -> bool {
        let c = s.as_u16();
        match self {
            Access::Allow => (200..400).contains(&c),
            Access::Forbidden => c == 403,
            Access::Hidden => c == 404,
            Access::Denied => c == 403 || c == 404,
            Access::Unauthenticated => c == 401,
            Access::Either => c < 500,
        }
    }

    /// The short form in the matrix file.
    pub fn code(self) -> &'static str {
        match self {
            Access::Allow => "ok",
            Access::Forbidden => "403",
            Access::Hidden => "404",
            Access::Denied => "403/404",
            Access::Unauthenticated => "401",
            Access::Either => "any",
        }
    }
}

/// The rule of one operation.
pub struct Rule {
    /// Where the rule comes from, and the assumptions in it.
    pub note: &'static str,
    pub access: Box<dyn Fn(Who) -> Access>,
    /// A successful answer may show the markers of this node only to principals with at least
    /// this role on it (lists, search, the changes feed).
    pub hide: Option<(Target, Role)>,
}

impl Rule {
    fn new(note: &'static str, f: impl Fn(Who) -> Access + 'static) -> Self {
        Self {
            note,
            access: Box::new(f),
            hide: None,
        }
    }

    fn hide(mut self, t: Target, r: Role) -> Self {
        self.hide = Some((t, r));
        self
    }
}

/// How agents are treated by a rule that is not about nodes.
#[derive(Clone, Copy, Debug)]
enum Agents {
    /// Like the user they act for.
    FollowUser,
    /// Like the user, and only with `max_role` of at least `edit` (tenant-level writes).
    FollowUserWrite,
    Allow,
    Forbidden,
    Denied,
}

fn agents(policy: Agents, who: Who, user_rule: &dyn Fn(Who) -> Access) -> Access {
    let user = who.acts_for().expect("an agent");
    match policy {
        Agents::FollowUser => user_rule(user),
        Agents::FollowUserWrite => {
            let a = user_rule(user);
            let (_, max_role) = who.scope().expect("an agent");
            if a == Access::Allow && max_role < Role::Edit {
                Access::Forbidden
            } else {
                a
            }
        }
        Agents::Allow => Access::Allow,
        Agents::Forbidden => Access::Forbidden,
        Agents::Denied => Access::Denied,
    }
}

/// Open to everybody, also without credentials.
fn public(access: Access) -> Rule {
    Rule::new("public (security: [])", move |_| access)
}

/// Any signed-in principal.
fn signed_in(note: &'static str, a: Agents) -> Rule {
    Rule::new(note, move |who| {
        if who == Who::Anonymous {
            Access::Unauthenticated
        } else if who.is_agent() {
            agents(a, who, &|_| Access::Allow)
        } else {
            Access::Allow
        }
    })
}

/// Users only; agents get `a`.
fn user_rule(note: &'static str, a: Agents, f: impl Fn(Who) -> Access + Copy + 'static) -> Rule {
    Rule::new(note, move |who| {
        if who == Who::Anonymous {
            Access::Unauthenticated
        } else if who.is_agent() {
            agents(a, who, &f)
        } else {
            f(who)
        }
    })
}

/// At least `min` in the tenant; lower roles get `403`.
fn tenant_role(note: &'static str, min: MemberRole, a: Agents) -> Rule {
    user_rule(note, a, move |who| match who.tenant_role() {
        Some(r) if r >= min => Access::Allow,
        _ => Access::Forbidden,
    })
}

/// At least `role` on `target`, after the agent scope. A principal that cannot see the node gets
/// `404`; one that can see it but lacks the role gets `403`. An agent whose user can see the node
/// but whose scope leaves it out gets `403` (`scope_exceeded`) or `404`.
fn node(note: &'static str, target: Target, role: Role) -> Rule {
    node2(note, target, role, None)
}

fn node2(note: &'static str, target: Target, role: Role, second: Option<(Target, Role)>) -> Rule {
    Rule::new(note, move |who| {
        if who == Who::Anonymous {
            return Access::Unauthenticated;
        }
        let ok = |t: Target, r: Role| who.role_on(t).is_some_and(|x| x >= r);
        if ok(target, role) && second.is_none_or(|(t, r)| ok(t, r)) {
            return Access::Allow;
        }
        if who.role_on(target).is_some() {
            return Access::Forbidden;
        }
        let user_sees = who.acts_for().is_some_and(|u| u.role_on(target).is_some());
        if user_sees || target == Target::Root {
            // The root is not secret (the contract answers 403 for it).
            Access::Denied
        } else {
            Access::Hidden
        }
    })
}

/// The `share` permission on `target` ([`Who::can_share`]). A principal that cannot see the node
/// gets `404`; one that can see it but cannot share gets `403`.
fn node_share(note: &'static str, target: Target) -> Rule {
    let sees = node("", target, Role::View).access;
    Rule::new(note, move |who| match sees(who) {
        Access::Allow if who.can_share(target) => Access::Allow,
        Access::Allow => Access::Forbidden,
        other => other,
    })
}

/// The owner of the narrow agent (`member`), and tenant admins when `admins` is true.
fn agent_owner(note: &'static str, admins: bool, hidden_from_others: bool) -> Rule {
    user_rule(note, Agents::Denied, move |who| match who {
        Who::Member => Access::Allow,
        Who::Owner | Who::Admin if admins => Access::Allow,
        // Admins see every agent of the tenant, so they get 403, not 404.
        Who::Owner | Who::Admin => Access::Forbidden,
        _ if hidden_from_others => Access::Hidden,
        _ => Access::Denied,
    })
}

/// Only the actor that started the upload (`member`) sees it.
fn upload_creator(note: &'static str) -> Rule {
    Rule::new(note, |who| match who {
        Who::Anonymous => Access::Unauthenticated,
        Who::Member => Access::Allow,
        _ => Access::Hidden,
    })
}

/// Who can see the tag T (it is on `plan`): every member, and others with `view` on `plan`.
fn sees_tag(who: Who) -> bool {
    if who.is_agent() {
        who.role_on(Target::Plan).is_some()
    } else {
        who.tenant_role().is_some_and(|r| r >= MemberRole::Member)
            || who.role_on(Target::Plan).is_some()
    }
}

fn tag_rule(note: &'static str, may_write: fn(Who) -> bool, write: bool) -> Rule {
    Rule::new(note, move |who| {
        if who == Who::Anonymous {
            return Access::Unauthenticated;
        }
        if !sees_tag(who) {
            return Access::Hidden;
        }
        if !write {
            return Access::Allow;
        }
        let allowed = match who.acts_for() {
            Some(user) => may_write(user) && who.scope().is_some_and(|(_, r)| r >= Role::Edit),
            None => may_write(who),
        };
        if allowed {
            Access::Allow
        } else {
            Access::Forbidden
        }
    })
}

/// The public operations of the contract (`security: []`).
fn public_operations() -> &'static BTreeSet<String> {
    static PUBLIC: OnceLock<BTreeSet<String>> = OnceLock::new();
    PUBLIC.get_or_init(|| {
        crate::contract::operations()
            .into_iter()
            .filter(|o| o.public)
            .map(|o| o.id)
            .collect()
    })
}

/// The rules that hold for every operation (the `Users only` section of the contract), on top
/// of the rule of the operation:
///
/// - an agent calling an operation marked `x-ruum-user-only` gets `403 user_required`, even when
///   its user would be allowed;
/// - a share-link bearer gets `403 user_required` on every operation that needs sign-in, except
///   the reads of [`SHARE_LINK_OPERATIONS`] (there the role of the link decides).
fn contract_wide(op: &str, mut r: Rule) -> Rule {
    let user_only = USER_ONLY_OPERATIONS.contains(&op);
    let link_may_call = SHARE_LINK_OPERATIONS.contains(&op) || public_operations().contains(op);
    let base = r.access;
    r.access = Box::new(move |who| {
        if (who.is_agent() && user_only) || (who.is_share_link() && !link_may_call) {
            Access::Forbidden
        } else {
            base(who)
        }
    });
    r
}

/// The rule of every operation. `None` means "no rule": the suite fails.
pub fn rule(op: &str) -> Option<Rule> {
    operation_rule(op).map(|r| contract_wide(op, r))
}

/// The rule of one operation, before [`contract_wide`].
fn operation_rule(op: &str) -> Option<Rule> {
    use Target::*;
    let r = match op {
        // ------------------------------------------------------------ system and auth
        "health"
        | "getJwks"
        | "getProtectedResourceMetadata"
        | "getAuthorizationServerMetadata" => public(Access::Allow),
        "loginStart" | "loginCallback" => public(Access::Either),
        "exchangeToken" => Rule::new(
            "public; the agent key in the body is the credential",
            |_| Access::Allow,
        ),
        "resolveShareLink" => Rule::new(
            "public; the share link token in the body is the credential",
            |_| Access::Allow,
        ),
        "registerOAuthClient" => Rule::new(
            "public; a registration gives no access (consent does)",
            |_| Access::Allow,
        ),
        "oauthAuthorize" => Rule::new(
            "public; a redirect to the consent screen, or a 400 page for an unknown client",
            |_| Access::Either,
        ),
        "getOAuthClient" => user_rule(
            "users only; the public metadata of a client, for the consent screen",
            Agents::Forbidden,
            |_| Access::Allow,
        ),
        "oauthConsent" => user_rule(
            "users only; creates an agent of the caller in the tenant; guests may, not while \
             allow_guests is false (decided 0002-2)",
            Agents::Forbidden,
            |_| Access::Allow,
        ),
        "logout" => signed_in(
            "ends the caller's web session; a bearer token (agents too) has none: 200, ends \
             nothing (decided 0002-5)",
            Agents::Allow,
        ),
        "getMe" => signed_in("every signed-in principal, agents too", Agents::Allow),
        "listMyIdentities" | "listMySessions" => signed_in(
            "users only (agents and share links: 403 user_required)",
            Agents::Forbidden,
        ),
        "listAppPasswords" | "createAppPassword" => user_rule(
            "users only; the caller's own app passwords in the tenant (members and guests)",
            Agents::Forbidden,
            |_| Access::Allow,
        ),
        "revokeAppPassword" => user_rule(
            "users only; only the user who made it (member), others do not see it",
            Agents::Forbidden,
            |who| {
                if who == Who::Member {
                    Access::Allow
                } else {
                    Access::Hidden
                }
            },
        ),
        "endSession" => user_rule(
            "users only; only the user of the session (member), others do not see it",
            Agents::Forbidden,
            |who| {
                if who == Who::Member {
                    Access::Allow
                } else {
                    Access::Hidden
                }
            },
        ),

        // ------------------------------------------------------------ tenants
        "listTenants" => signed_in("about the caller", Agents::Allow),
        "createTenant" => signed_in("users only", Agents::Forbidden),
        "getTenant" => tenant_role(
            "members and guests (listTenants shows guest tenants)",
            MemberRole::Guest,
            Agents::FollowUser,
        ),
        "updateTenant" => tenant_role(
            "users only; Needs admin or owner",
            MemberRole::Admin,
            Agents::Forbidden,
        ),
        "listMembers" => tenant_role(
            "members and up; guests get 403, the list holds emails (decided 0002-5)",
            MemberRole::Member,
            Agents::FollowUser,
        ),
        "updateMember" => tenant_role(
            "users only; Needs admin",
            MemberRole::Admin,
            Agents::Forbidden,
        ),
        "removeMember" => user_rule(
            "users only; Needs admin, or the member removes themselves (the target is guest)",
            Agents::Forbidden,
            |who| match who {
                Who::Owner | Who::Admin | Who::Guest => Access::Allow,
                _ => Access::Forbidden,
            },
        ),
        "listInvitations" | "createInvitation" | "revokeInvitation" | "listAudit" => tenant_role(
            "users only; Needs admin",
            MemberRole::Admin,
            Agents::Forbidden,
        ),
        "acceptInvitation" => user_rule(
            "users only, with the token (the answer depends on membership)",
            Agents::Forbidden,
            |_| Access::Either,
        ),
        "getLimits" => tenant_role(
            "any member can read them; guests get 403",
            MemberRole::Member,
            Agents::FollowUser,
        ),

        // ------------------------------------------------------------ agents
        "listAgents" => signed_in(
            "the caller's agents (admins: all); an agent sees only itself (decided 0002-5)",
            Agents::Allow,
        ),
        "registerAgent" => user_rule(
            "users only; guests may, not while allow_guests is false (decided 0002-2)",
            Agents::Forbidden,
            |_| Access::Allow,
        ),
        "getAgent" => agent_owner("the owner or a tenant admin; others get 404", true, true),
        "listAgentActivity" => agent_owner(
            "the owner or a tenant admin; agents get 403/404 (decided 0002-5)",
            true,
            false,
        ),
        "listAgentKeys" => agent_owner(
            "users only; the owner or a tenant admin (decided 0002-5)",
            true,
            false,
        ),
        "disableAgent" | "revokeAgentKey" | "undoAgent" => {
            agent_owner("users only; the owner or a tenant admin", true, false)
        }
        "updateAgent" | "enableAgent" | "createAgentKey" => {
            agent_owner("users only; only the owner of the agent", false, false)
        }

        // ------------------------------------------------------------ nodes
        "getRoot" => node(
            "view on the root; the root is not secret (403)",
            Root,
            Role::View,
        ),
        "createNode" => node("Needs edit on the parent", Projects, Role::Edit),
        "getNodeByPath" => node(
            "view on the target is enough, as for getNode (decided 0002-5)",
            Plan,
            Role::View,
        ),
        "getNode" => node(
            "Needs view; 404 when the caller cannot see it",
            Plan,
            Role::View,
        ),
        "listChildren" => node("view on the folder", Projects, Role::View),
        "renameNode" => node("Needs edit on the node", Plan, Role::Edit),
        "moveNode" => node2(
            "Needs manage on the node and edit on the new parent",
            Plan,
            Role::Manage,
            Some((Archive, Role::Edit)),
        ),
        "copyNode" => node2(
            "Needs view on the node and edit on the new parent",
            Plan,
            Role::View,
            Some((Archive, Role::Edit)),
        ),
        "trashNode" => node("Needs manage", Plan, Role::Manage),
        "restoreNode" => node(
            "Needs manage (inherited from the old parent)",
            Old,
            Role::Manage,
        ),
        "purgeNode" => {
            let needs_manage = node("", Old, Role::Manage).access;
            Rule::new(
                "Needs manage, and admin on a team plan (admin_restore_days > trash_days)",
                move |who| match needs_manage(who) {
                    Access::Allow => {
                        let user = who.acts_for().unwrap_or(who);
                        if user.tenant_role() >= Some(MemberRole::Admin) {
                            Access::Allow
                        } else {
                            Access::Forbidden
                        }
                    }
                    other => other,
                },
            )
        }
        "listTrash" => signed_in("the trashed roots the caller can manage", Agents::Allow)
            .hide(Old, Role::Manage),
        "emptyTrash" => signed_in(
            "purges only the trashed roots the caller may purge (purgeNode); others stay",
            Agents::Allow,
        )
        .hide(Old, Role::Manage),

        // ------------------------------------------------------------ uploads and content
        "createUpload" => node(
            "edit on the parent folder for a new file",
            Projects,
            Role::Edit,
        ),
        "getUpload" | "abortUpload" | "putChunk" | "completeUpload" => {
            upload_creator("only the actor that started the upload (member)")
        }
        "getChunkingParams" => signed_in(
            "every principal of the tenant (share links: edit links only)",
            Agents::Allow,
        ),
        "downloadContent" | "getContentUrl" => node("Needs view", Plan, Role::View),
        "putContent" => node("Needs edit", Plan, Role::Edit),

        // ------------------------------------------------------------ versions and diff
        "listVersions" | "getVersion" | "diffVersions" | "listDiffPlugins" => {
            node("Needs view", Plan, Role::View)
        }
        "restoreVersion" | "setVersionLabel" => node("Needs edit", Plan, Role::Edit),

        // ------------------------------------------------------------ tags
        "listTags" => signed_in(
            "members see all tags; guests only tags on nodes they can see",
            Agents::Allow,
        ),
        "createTag" => tenant_role(
            "any member, not guests; agents need max_role edit (decided 0002-5)",
            MemberRole::Member,
            Agents::FollowUserWrite,
        ),
        "getTag" | "listNodesByTag" => tag_rule(
            "members, and principals who can see a node with the tag (decided 0002-5)",
            |_| true,
            false,
        ),
        "updateTag" => tag_rule(
            "any member, not guests; agents need max_role edit (decided 0002-5)",
            |w| w.tenant_role().is_some_and(|r| r >= MemberRole::Member),
            true,
        ),
        "deleteTag" => tag_rule(
            "tenant admins or the creator (member); agents need max_role edit (decided 0002-5)",
            |w| matches!(w, Who::Owner | Who::Admin | Who::Member),
            true,
        ),
        "listNodeTags" => node("Needs view on the node", Plan, Role::View),
        "attachTag" => node("Needs edit on the node", Archive, Role::Edit),
        "detachTag" => node("Needs edit on the node", Plan, Role::Edit),

        // ------------------------------------------------------------ sharing
        "listGrants" => node("Needs view", Projects, Role::View),
        "addGrant" | "updateGrant" | "removeGrant" | "listShareLinks" | "createShareLink"
        | "revokeShareLink" => node_share(
            "Needs share on the node (manage, or edit with editors_can_share)",
            Projects,
        ),
        "updateNodeSharing" => node(
            "Needs manage (Archive: the node with editors_can_share false)",
            Archive,
            Role::Manage,
        ),
        "listSharedWithMe" => signed_in(
            "the roots of grants to the caller; for agents only inside the scope",
            Agents::Allow,
        )
        .hide(Projects, Role::View),

        // ------------------------------------------------------------ changes and search
        "listChanges" => signed_in(
            "only entries on nodes the caller can see (agents: inside the scope)",
            Agents::Allow,
        )
        .hide(Projects, Role::View),
        "getLatestCursor" => signed_in("every signed-in principal of the tenant", Agents::Allow),
        "searchNodes" => {
            signed_in("only nodes the caller can see", Agents::Allow).hide(Plan, Role::View)
        }

        _ => return None,
    };
    Some(r)
}

fn markers_of(t: &TenantFixture, target: Target) -> Vec<String> {
    match target {
        Target::Projects | Target::Root => t.projects_markers(),
        Target::Plan => vec![
            t.plan.to_string(),
            t.plan_name.clone(),
            t.v1.to_string(),
            t.v2.to_string(),
        ],
        Target::Old => vec![t.old.to_string(), format!("{}-old.txt", t.prefix)],
        Target::Archive => vec![t.archive.to_string()],
        Target::Private => vec![t.private.to_string()],
    }
}

/// Checks one answer against the rule.
pub fn evaluate(
    rule: &Rule,
    who: Who,
    t: &TenantFixture,
    seeded: bool,
    sent: &str,
    res: &Response,
    fallback: &Response,
) -> Outcome {
    if client::is_unrouted(res, fallback) {
        return Outcome::Failed("no handler answers this method and path".into());
    }
    let expected = (rule.access)(who);
    let text = res.text();
    let leaks = |markers: Vec<String>| -> Vec<String> {
        markers
            .into_iter()
            .filter(|m| !sent.contains(m.as_str()) && text.contains(m.as_str()))
            .collect()
    };
    let mut problems = Vec::new();
    // A refusal never shows the hidden part of the tree.
    if res.status.is_client_error() && who.role_on(Target::Projects).is_none() {
        let l = leaks(t.projects_markers());
        if !l.is_empty() {
            problems.push(format!(
                "the refusal shows data the principal cannot see: {l:?}"
            ));
        }
    }
    if res.is_not_implemented() {
        return if problems.is_empty() {
            Outcome::Pending
        } else {
            Outcome::Failed(problems.join("; "))
        };
    }
    if res.status.is_server_error() {
        problems.push(format!("server error {}", res.status));
    } else if res.status == StatusCode::UNAUTHORIZED
        && expected != Access::Unauthenticated
        && expected != Access::Either
    {
        problems.push(format!(
            "401, expected {}{}",
            expected.code(),
            if seeded {
                ""
            } else {
                " (fixtures::seed still makes placeholder credentials; implement it)"
            }
        ));
    } else if !expected.accepts(res.status) {
        problems.push(format!("got {}, expected {}", res.status, expected.code()));
    }
    if let Some((target, role)) = rule.hide
        && res.status.is_success()
        && who.role_on(target).is_none_or(|r| r < role)
    {
        let l = leaks(markers_of(t, target));
        if !l.is_empty() {
            problems.push(format!(
                "the answer shows nodes the principal cannot see: {l:?}"
            ));
        }
    }
    if problems.is_empty() {
        Outcome::Passed
    } else {
        Outcome::Failed(problems.join("; "))
    }
}

/// Runs the whole matrix on the backend of `RUUM_SUITE_BACKEND`.
pub async fn run() -> Report {
    let backend = fixtures::Backend::start().await;
    let report = run_on(&backend).await;
    backend.finish().await;
    report
}

/// Runs the whole matrix on `backend`: every operation as every principal, each in a fresh
/// world.
pub async fn run_on(backend: &fixtures::Backend) -> Report {
    let mut report = Report::new(&format!("permission matrix suite [{}]", backend.name()));
    let ops = crate::contract::operations();
    for op in &ops {
        if rule(&op.id).is_none() {
            report.missing.push(op.id.clone());
        }
    }
    let jobs: Vec<(&str, Who)> = ops
        .iter()
        .filter(|op| rule(&op.id).is_some())
        .flat_map(|op| Who::ALL.iter().map(move |&who| (op.id.as_str(), who)))
        .collect();
    let rows: Vec<(&str, Who, Outcome)> = stream::iter(jobs)
        .map(|(op, who)| async move {
            let rule = rule(op).expect("filtered above");
            let state = backend.app_state().await;
            let w = fixtures::seed(&state).await;
            let app = client::app(state);
            let Some(req) = standard(op, &w.a) else {
                return (op, who, Outcome::Failed("no standard request".into()));
            };
            let fallback = client::fallback_fingerprint(&app, w.a.principal(Who::Owner)).await;
            let res = client::send(&app, &req, w.a.principal(who)).await;
            let outcome = evaluate(
                &rule,
                who,
                &w.a,
                w.seeded,
                &req.sent_text(),
                &res,
                &fallback,
            );
            (op, who, outcome)
        })
        .buffered(backend.jobs())
        .collect()
        .await;
    for (op, who, outcome) in rows {
        report.add(op, who.name(), outcome);
    }
    report
}

/// The operations that read the content or the versions of a file, checked on a trashed file by
/// [`run_trashed`].
pub const TRASHED_FILE_READS: &[&str] = &[
    "downloadContent",
    "getContentUrl",
    "listVersions",
    "getVersion",
    "diffVersions",
];

/// The rule for a read of a trashed file (the trash rule of the nodes area, as `getNode`): a
/// trashed node has left the tree, so only principals with `manage` on it (those who may
/// restore it) read it; everyone else gets `404`, also with `view` or `edit`.
pub fn trashed_read_rule(op: &'static str) -> Rule {
    contract_wide(
        op,
        Rule::new(
            "the trash rule: manage on the trashed file, else 404",
            |who| {
                if who == Who::Anonymous {
                    Access::Unauthenticated
                } else if who.role_on(Target::Plan).is_some_and(|r| r >= Role::Manage) {
                    Access::Allow
                } else {
                    Access::Hidden
                }
            },
        ),
    )
}

/// The trashed-file suite (security review of 2026-11-06): the owner trashes the plan (a file
/// with versions v1 and v2), then every principal sends the standard requests of
/// [`TRASHED_FILE_READS`] and of `listChanges`.
///
/// - The reads follow [`trashed_read_rule`]. Before the fix, a principal with `view` or `edit`
///   (not `manage`) still downloaded the trashed file, got a content URL and listed its
///   versions for the whole trash period, although `getNode` answered `404`.
/// - The changes feed shows the entries on the trashed file (its versions) only to principals
///   with `manage` on it; the others still get the `node.trashed` entry through the old place.
pub async fn run_trashed() -> Report {
    let backend = fixtures::Backend::start().await;
    let report = run_trashed_on(&backend).await;
    backend.finish().await;
    report
}

/// [`run_trashed`] on `backend`.
pub async fn run_trashed_on(backend: &fixtures::Backend) -> Report {
    let mut report = Report::new(&format!("trashed file suite [{}]", backend.name()));
    let mut ops: Vec<&'static str> = TRASHED_FILE_READS.to_vec();
    ops.push("listChanges");
    let jobs: Vec<(&'static str, Who)> = ops
        .into_iter()
        .flat_map(|op| Who::ALL.iter().map(move |&who| (op, who)))
        .collect();
    let rows: Vec<(&'static str, Who, Outcome)> = stream::iter(jobs)
        .map(|(op, who)| async move { (op, who, trashed_check(backend, op, who).await) })
        .buffered(backend.jobs())
        .collect()
        .await;
    for (op, who, outcome) in rows {
        report.add(op, who.name(), outcome);
    }
    report
}

/// One check of [`run_trashed`]: `who` sends the standard request of `op` after the owner
/// trashed the plan, in a fresh world.
async fn trashed_check(backend: &fixtures::Backend, op: &'static str, who: Who) -> Outcome {
    let state = backend.app_state().await;
    let w = fixtures::seed(&state).await;
    let t = &w.a;
    let owner = ruum_core::actor::Context {
        tenant: t.id,
        actor: ruum_core::actor::Actor::user(t.user_id(Who::Owner)),
        scope: None,
        idempotency_key: None,
    };
    // The owner trashes the plan as one request does: with the tenant's freshness floor
    // (decision 0002-4), so a shared SpiceDB engine reads the seed of this world.
    let services = &state.services;
    let trashed = ruum_core::authz::with_request_freshness(async {
        ruum_core::authz::load_request_floor(&*services.freshness, t.id).await?;
        services.nodes.trash(&owner, t.plan, None).await
    })
    .await;
    if let Err(e) = trashed {
        return Outcome::Failed(format!("trash: {e:?}"));
    }
    let app = client::app(state);
    let Some(req) = standard(op, t) else {
        return Outcome::Failed("no standard request".into());
    };
    let fallback = client::fallback_fingerprint(&app, t.principal(Who::Owner)).await;
    let res = client::send(&app, &req, t.principal(who)).await;
    if op == "listChanges" {
        trashed_feed_outcome(who, t, &res)
    } else {
        evaluate(
            &trashed_read_rule(op),
            who,
            t,
            w.seeded,
            &req.sent_text(),
            &res,
            &fallback,
        )
    }
}

/// The changes feed after the plan went to the trash: the version IDs of the plan show only to
/// principals with `manage` on it, and they do show to them (so the check is not empty).
fn trashed_feed_outcome(who: Who, t: &TenantFixture, res: &Response) -> Outcome {
    let expected = match who {
        Who::Anonymous => Access::Unauthenticated,
        Who::ShareLink => Access::Forbidden,
        _ => Access::Allow,
    };
    if !expected.accepts(res.status) {
        return Outcome::Failed(format!("got {}, expected {}", res.status, expected.code()));
    }
    if !res.status.is_success() {
        return Outcome::Passed;
    }
    let text = res.text();
    let versions = [t.v1.to_string(), t.v2.to_string()];
    let shown: Vec<&String> = versions
        .iter()
        .filter(|v| text.contains(v.as_str()))
        .collect();
    let manages = who.role_on(Target::Plan).is_some_and(|r| r >= Role::Manage);
    if manages && shown.len() != versions.len() {
        return Outcome::Failed(format!(
            "a manager of the trashed file misses its versions in the feed ({} of 2)",
            shown.len()
        ));
    }
    if !manages && !shown.is_empty() {
        return Outcome::Failed(format!(
            "the feed shows the versions of a trashed file to a principal without manage: {shown:?}"
        ));
    }
    Outcome::Passed
}

/// The matrix as a Markdown table, for `permission_matrix.md`.
pub fn render_matrix() -> String {
    let ops = crate::contract::operations();
    let mut s = String::new();
    let _ = writeln!(
        s,
        "# Permission matrix\n\n\
         Generated from the rules in `crates/ruum-testkit/src/permissions.rs` by\n\
         `RUUM_UPDATE_MATRIX=1 cargo test -p ruum-testkit --test permissions`. Do not edit by hand.\n\n\
         Each cell is what the principal must get for the standard request of the operation\n\
         (`crates/ruum-testkit/src/requests.rs`) in tenant A. `ok` = 2xx/3xx, `403`, `404`,\n\
         `403/404` = either, `401`, `any` = not decided by the contract (open question).\n\
         The principals are described in `crates/ruum-testkit/src/fixtures.rs`. The narrow agent\n\
         `agent` acts for `member` with scope `Projects` and `max_role: view`. `share_link` is the\n\
         bearer of a `view` share link on `Projects`. Roles are `view < comment < edit < manage`;\n\
         `share` is a permission: `manage` always, `edit` where `editors_can_share` is true (not\n\
         in `Archive`). Members manage the team tree and share there by the default tenant\n\
         policy (`members_can_share`).\n"
    );
    let _ = write!(s, "| operation |");
    for who in Who::ALL {
        let _ = write!(s, " {} |", who.name());
    }
    let _ = writeln!(s, " rule |");
    let _ = write!(s, "|---|");
    for _ in Who::ALL {
        let _ = write!(s, "---|");
    }
    let _ = writeln!(s, "---|");
    for op in &ops {
        let _ = write!(s, "| `{}` |", op.id);
        match rule(&op.id) {
            Some(r) => {
                for &who in Who::ALL {
                    let _ = write!(s, " {} |", (r.access)(who).code());
                }
                let _ = writeln!(s, " {} |", r.note);
            }
            None => {
                for _ in Who::ALL {
                    let _ = write!(s, " ? |");
                }
                let _ = writeln!(s, " NO RULE |");
            }
        }
    }
    s
}

/// The path of the committed matrix.
pub fn matrix_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("permission_matrix.md")
}
