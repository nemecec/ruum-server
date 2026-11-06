//! The tenant isolation suite (`docs/DESIGN.md`, sections 15 and 18.2).
//!
//! The rule: a principal of tenant B acting on a resource of tenant A gets `404`, never data.
//!
//! The suite reads every operation of `contracts/openapi.yaml` and needs a registered case for
//! each one in [`case`]. An operation without a case fails the suite, so every new endpoint must
//! add one. A case is a list of probes. Each probe sends one request as a principal of tenant B,
//! and checks:
//!
//! 1. the status ([`Expect`]);
//! 2. that the response (body and headers) holds no marker of tenant A (IDs, names, emails,
//!    secrets), except the ones the request itself sent;
//! 3. for writes, that a read of tenant A's resource by tenant A's principal gives the same answer
//!    before and after the probe (the write did not happen).
//!
//! Every case runs in a fresh world ([`crate::fixtures::seed`]), so one case cannot spoil another.
//!
//! # How to add a case for a new operation
//!
//! 1. Add the standard request of the operation in [`crate::requests::standard`].
//! 2. Add an arm in [`case`]. Most operations that address a resource of a tenant use
//!    [`addressed`]; lists in the caller's own tenant use [`own_tenant`]. Add extra probes for
//!    IDs of the other tenant in the body or in a second path parameter.
//! 3. Add the rule of the operation in [`crate::permissions::rule`].

use crate::client::{self, RequestSpec, Response};
use crate::fixtures::{self, Principal, TenantFixture, Who, World};
use crate::report::{Outcome, Report};
use crate::requests::standard;
use axum::http::{Method, StatusCode};
use serde_json::json;

/// The status a probe must get.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    /// Exactly `404`. The rule for every resource of another tenant.
    NotFound,
    /// `403` or `404`. Only where the contract has no `404` for the operation (a contract gap,
    /// listed in `docs/TESTING.md`) or for a tenant header that names a tenant the caller is not
    /// in.
    Denied,
    /// `403`, `404` or `422`. Only for an ID of the other tenant inside a request body where the
    /// contract has no `404`.
    Rejected,
    /// Any answer below `500` except `401`, as long as it holds no data of the other tenant. For
    /// lists and actions in the caller's own tenant.
    NoLeak,
}

impl Expect {
    fn accepts(self, s: StatusCode) -> bool {
        let c = s.as_u16();
        match self {
            Expect::NotFound => c == 404,
            Expect::Denied => c == 403 || c == 404,
            Expect::Rejected => c == 403 || c == 404 || c == 422,
            Expect::NoLeak => c < 500 && c != 401,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Expect::NotFound => "404",
            Expect::Denied => "403 or 404",
            Expect::Rejected => "403, 404 or 422",
            Expect::NoLeak => "any status below 500 except 401, without data of tenant A",
        }
    }
}

/// One request of a case, sent as a principal of tenant B.
#[derive(Clone, Debug)]
pub struct Probe {
    pub label: String,
    /// The principal of tenant B that sends the request.
    pub actor: Who,
    pub request: RequestSpec,
    pub expect: Expect,
    /// A read by a principal of tenant A whose answer must not change because of the probe.
    pub unchanged: Option<(Who, RequestSpec)>,
}

impl Probe {
    pub fn new(label: impl Into<String>, actor: Who, request: RequestSpec, expect: Expect) -> Self {
        Self {
            label: label.into(),
            actor,
            request,
            expect,
            unchanged: None,
        }
    }

    pub fn unchanged(mut self, who: Who, read: RequestSpec) -> Self {
        self.unchanged = Some((who, read));
        self
    }
}

/// The registered case of one operation.
#[derive(Clone, Debug)]
pub struct IsolationCase {
    /// Why the probes are what they are, for operations that are not plain addressed ones.
    pub note: &'static str,
    pub probes: Vec<Probe>,
}

fn std_req(op: &str, t: &TenantFixture) -> RequestSpec {
    standard(op, t).unwrap_or_else(|| panic!("no standard request for {op}"))
}

/// The probes of an operation that addresses a resource of tenant A:
///
/// - tenant B's owner sends the standard request with A's IDs and B's tenant header: `404`;
/// - for operations with a tenant header, the same request with A's tenant header (B's owner
///   claims to be in A): `403` or `404`.
///
/// `read` names the read operation (as A's owner, or `reader`) that must not change.
pub fn addressed(op: &str, w: &World, expect: Expect, read: Option<(&str, Who)>) -> Vec<Probe> {
    let req = std_req(op, &w.a);
    let has_header = req.tenant.is_some();
    let unchanged = read.map(|(r, who)| (who, std_req(r, &w.a)));
    let mut probes = Vec::new();
    let mut p = Probe::new(
        "B's owner, A's resource",
        Who::Owner,
        req.clone().with_tenant(has_header.then_some(w.b.id)),
        expect,
    );
    p.unchanged = unchanged.clone();
    probes.push(p);
    if has_header {
        let mut p = Probe::new(
            "B's owner claims tenant A in Ruum-Tenant",
            Who::Owner,
            req,
            Expect::Denied,
        );
        p.unchanged = unchanged;
        probes.push(p);
    }
    probes
}

/// The probes of an operation that acts in the caller's own tenant (lists, search, creating):
///
/// - tenant B's owner sends the standard request against B: no data of A in the answer;
/// - for operations with a tenant header, the same request with A's tenant header: `403`/`404`.
pub fn own_tenant(op: &str, w: &World, read: Option<(&str, Who)>) -> Vec<Probe> {
    let req = std_req(op, &w.b);
    let has_header = req.tenant.is_some();
    let unchanged = read.map(|(r, who)| (who, std_req(r, &w.a)));
    let mut probes = Vec::new();
    let mut p = Probe::new(
        "B's owner in tenant B",
        Who::Owner,
        req.clone(),
        Expect::NoLeak,
    );
    p.unchanged = unchanged.clone();
    probes.push(p);
    if has_header {
        let mut p = Probe::new(
            "B's owner claims tenant A in Ruum-Tenant",
            Who::Owner,
            req.tenant(w.a.id),
            Expect::Denied,
        );
        p.unchanged = unchanged;
        probes.push(p);
    }
    probes
}

fn plain(note: &'static str, probes: Vec<Probe>) -> Option<IsolationCase> {
    Some(IsolationCase { note, probes })
}

/// The registered isolation case of every operation. `None` means "no case": the suite fails.
pub fn case(op: &str, w: &World) -> Option<IsolationCase> {
    use Expect::*;
    let (a, b) = (&w.a, &w.b);
    let o = Who::Owner;
    let addressed_case = |e: Expect, read: Option<(&str, Who)>| {
        plain("addressed resource of tenant A", addressed(op, w, e, read))
    };
    let node_get = |t: &TenantFixture, n: ruum_core::NodeId, rest: &str| {
        RequestSpec::get(format!("/v1/nodes/{n}{rest}")).tenant(t.id)
    };
    match op {
        // ------------------------------------------------------------ system and auth
        "health"
        | "getJwks"
        | "getProtectedResourceMetadata"
        | "getAuthorizationServerMetadata"
        | "loginStart"
        | "loginCallback" => plain(
            "public; no tenant data. The answer must not hold data of tenant A",
            vec![Probe::new("B's owner", o, std_req(op, b), NoLeak)],
        ),
        "logout" => plain(
            "B's owner signs out; A's sessions stay valid",
            vec![
                Probe::new("B's owner signs out", o, std_req(op, b), NoLeak)
                    .unchanged(Who::Member, std_req("getMe", a)),
            ],
        ),
        "getMe" | "listMyIdentities" | "listMySessions" | "listTenants" | "listSharedWithMe" => {
            plain(
                "about the caller only",
                vec![Probe::new("B's owner", o, std_req(op, b), NoLeak)],
            )
        }
        "endSession" => addressed_case(NotFound, Some(("listMySessions", Who::Member))),

        // ------------------------------------------------------------ tenants
        "createTenant" => plain(
            "makes a new tenant for the caller",
            vec![Probe::new("B's owner", o, std_req(op, b), NoLeak)],
        ),
        "getTenant" | "listMembers" | "getLimits" => addressed_case(NotFound, None),
        "updateTenant" => addressed_case(NotFound, Some(("getTenant", o))),
        "updateMember" | "removeMember" => {
            let mut p = addressed(op, w, NotFound, Some(("listMembers", o)));
            let method = std_req(op, a).method;
            let mut mixed = RequestSpec::new(
                method,
                format!("/v1/tenants/{}/members/{}", b.id, a.user_id(Who::Guest)),
            );
            mixed.body = std_req(op, a).body;
            p.push(
                Probe::new("B's owner, A's user under tenant B", o, mixed, NotFound)
                    .unchanged(o, std_req("listMembers", a)),
            );
            plain("addressed resource of tenant A", p)
        }
        // The contract has no 404 for these; see docs/TESTING.md.
        "listInvitations" | "listAudit" => plain(
            "contract gap: no 404 response in the contract, so 403 is accepted",
            addressed(op, w, Denied, None),
        ),
        "createInvitation" => plain(
            "contract gap: no 404 response in the contract, so 403 is accepted",
            addressed(op, w, Denied, Some(("listInvitations", o))),
        ),
        "revokeInvitation" => {
            let mut p = addressed(op, w, NotFound, Some(("listInvitations", o)));
            p.push(
                Probe::new(
                    "B's owner, A's invitation under tenant B",
                    o,
                    RequestSpec::new(
                        Method::DELETE,
                        format!("/v1/tenants/{}/invitations/{}", b.id, a.invitation),
                    ),
                    NotFound,
                )
                .unchanged(o, std_req("listInvitations", a)),
            );
            plain("addressed resource of tenant A", p)
        }
        "acceptInvitation" => plain(
            "invitation tokens are bearer secrets by design (sent by email); the probe checks \
             that an unknown token is 404 and that B's own token leaks nothing of A",
            vec![
                Probe::new(
                    "B's owner, unknown token",
                    o,
                    RequestSpec::post("/v1/invitations/accept")
                        .json(json!({ "token": "no-such-invitation-token" })),
                    NotFound,
                ),
                Probe::new("B's owner, B's token", o, std_req(op, b), NoLeak),
            ],
        ),

        // ------------------------------------------------------------ agents
        "listAgents" => plain("list in the caller's tenant", own_tenant(op, w, None)),
        "registerAgent" => {
            let mut p = own_tenant(op, w, Some(("listAgents", o)));
            p.push(
                Probe::new(
                    "B's member, scope subtree in tenant A",
                    Who::Member,
                    RequestSpec::post("/v1/agents").tenant(b.id).json(json!({
                        "name": "bravo-agent-into-alpha",
                        "scope": { "subtree": a.projects, "max_role": "view" },
                    })),
                    Rejected,
                )
                .unchanged(o, std_req("listAgents", a)),
            );
            plain(
                "creates in the caller's tenant; a scope in tenant A is refused",
                p,
            )
        }
        "getAgent" | "listAgentKeys" | "listAgentActivity" => addressed_case(NotFound, None),
        "updateAgent" => {
            let mut p = addressed(op, w, NotFound, Some(("getAgent", o)));
            p.push(Probe::new(
                "B's member moves its agent's scope into tenant A",
                Who::Member,
                RequestSpec::new(
                    Method::PATCH,
                    format!("/v1/agents/{}", b.agent_id(Who::Agent)),
                )
                .tenant(b.id)
                .json(json!({ "scope": { "subtree": a.projects, "max_role": "view" } })),
                Rejected,
            ));
            plain("addressed resource of tenant A", p)
        }
        "disableAgent" | "enableAgent" => addressed_case(NotFound, Some(("getAgent", o))),
        "createAgentKey" => addressed_case(NotFound, Some(("listAgentKeys", o))),
        "revokeAgentKey" => {
            let mut p = addressed(op, w, NotFound, Some(("listAgentKeys", o)));
            p.push(
                Probe::new(
                    "B's owner, A's key under B's agent",
                    o,
                    RequestSpec::new(
                        Method::DELETE,
                        format!("/v1/agents/{}/keys/{}", b.agent_id(Who::Agent), a.agent_key),
                    )
                    .tenant(b.id),
                    NotFound,
                )
                .unchanged(o, std_req("listAgentKeys", a)),
            );
            plain("addressed resource of tenant A", p)
        }
        "undoAgent" => addressed_case(NotFound, Some(("listChanges", o))),
        "registerOAuthClient" | "oauthAuthorize" => plain(
            "OAuth clients belong to no tenant; B's registration or authorization leaks nothing \
             of A",
            vec![Probe::new(
                "anonymous, B's client",
                Who::Anonymous,
                std_req(op, b),
                NoLeak,
            )],
        ),
        "getOAuthClient" => plain(
            "the public metadata of a client, which belongs to no tenant; the answer must not \
             hold data of tenant A",
            vec![Probe::new("B's owner", o, std_req(op, b), NoLeak)],
        ),
        "oauthConsent" => {
            let into_a = std_req(op, b).json(json!({
                "request": b.oauth_request,
                "approve": true,
                "tenant_id": a.id,
                "new_agent": {
                    "name": "bravo-oauth-agent-in-alpha",
                    "scope": { "subtree": a.projects, "max_role": "view" },
                },
            }));
            let bind_a = std_req(op, b).json(json!({
                "request": b.oauth_request,
                "approve": true,
                "tenant_id": b.id,
                "agent_id": a.agent_id(Who::Agent),
            }));
            plain(
                "the consent creates or binds an agent in the caller's tenant only",
                vec![
                    Probe::new("B's owner creates an agent in tenant A", o, into_a, Denied)
                        .unchanged(o, std_req("listAgents", a)),
                    Probe::new("B's owner binds A's agent", o, bind_a, NotFound)
                        .unchanged(o, std_req("getAgent", a)),
                    Probe::new("B's owner, B's consent", o, std_req(op, b), NoLeak),
                ],
            )
        }
        "exchangeToken" => plain(
            "a key gives a token bound to the key's tenant: B's exchange leaks nothing of A, and \
             B's agent token cannot act in tenant A",
            vec![
                Probe::new("B's key exchange", Who::Anonymous, std_req(op, b), NoLeak),
                Probe::new(
                    "B's agent claims tenant A",
                    Who::Agent,
                    std_req("getRoot", b).tenant(a.id),
                    Denied,
                ),
                Probe::new(
                    "B's agent reads A's node with A's tenant header",
                    Who::Agent,
                    std_req("getNode", a),
                    Denied,
                ),
            ],
        ),

        // ------------------------------------------------------------ nodes
        "getRoot" | "listTrash" | "listTags" | "getLatestCursor" | "getChunkingParams" => {
            plain("in the caller's tenant", own_tenant(op, w, None))
        }
        "createNode" => {
            let mut p = addressed(op, w, NotFound, Some(("listChildren", o)));
            p.push(Probe::new(
                "B's owner links to A's file from B's folder",
                o,
                RequestSpec::post("/v1/nodes").tenant(b.id).json(json!({
                    "parent_id": b.projects, "name": "link to alpha", "kind": "link",
                    "link": { "kind": "shortcut", "target": a.plan },
                })),
                NotFound,
            ));
            plain("parent or link target in tenant A", p)
        }
        "getNodeByPath" => plain(
            "the path resolves in the caller's tenant only: A's file name finds nothing in B",
            vec![
                Probe::new(
                    "B's owner asks for A's path in tenant B",
                    o,
                    RequestSpec::get(format!("/v1/nodes/by-path/Projects/{}", a.plan_name))
                        .tenant(b.id),
                    NotFound,
                ),
                Probe::new(
                    "B's owner claims tenant A in Ruum-Tenant",
                    o,
                    std_req(op, a),
                    Denied,
                ),
            ],
        ),
        "getNode" | "listChildren" | "downloadContent" | "listVersions" | "listDiffPlugins"
        | "listNodeTags" | "listGrants" | "listShareLinks" | "getContentUrl" => {
            addressed_case(NotFound, None)
        }
        "purgeNode" => addressed_case(NotFound, Some(("listTrash", o))),
        "emptyTrash" => plain(
            "empties the caller's own trash; A's trash stays",
            own_tenant(op, w, Some(("listTrash", o))),
        ),
        "renameNode" | "trashNode" => addressed_case(NotFound, Some(("getNode", o))),
        "moveNode" | "copyNode" => {
            let mut p = addressed(op, w, NotFound, Some(("getNode", o)));
            p.push(
                Probe::new(
                    "B's owner, B's file into A's folder",
                    o,
                    RequestSpec::post(format!(
                        "/v1/nodes/{}/{}",
                        b.plan,
                        if op == "moveNode" { "move" } else { "copy" }
                    ))
                    .tenant(b.id)
                    .json(json!({ "parent_id": a.archive })),
                    NotFound,
                )
                .unchanged(o, node_get(a, a.archive, "/children")),
            );
            plain("node or new parent in tenant A", p)
        }
        "restoreNode" => {
            let mut p = addressed(op, w, NotFound, Some(("listTrash", o)));
            p.push(
                Probe::new(
                    "B's owner restores B's file into A's folder",
                    o,
                    RequestSpec::post(format!("/v1/nodes/{}/restore", b.old))
                        .tenant(b.id)
                        .json(json!({ "parent_id": a.projects })),
                    NotFound,
                )
                .unchanged(o, std_req("listChildren", a)),
            );
            plain("node or new parent in tenant A", p)
        }

        // ------------------------------------------------------------ uploads and content
        "createUpload" => {
            let mut p = addressed(op, w, NotFound, Some(("listChildren", o)));
            p.push(Probe::new(
                "B's owner, new version of A's file",
                o,
                RequestSpec::post("/v1/uploads").tenant(b.id).json(json!({
                    "target": "new_version", "node_id": a.plan, "size": 0,
                    "content_hash": crate::requests::EMPTY_HASH,
                    "chunking": crate::requests::CHUNKING, "chunks": [],
                })),
                NotFound,
            ));
            plain("target in tenant A", p)
        }
        "getUpload" => addressed_case(NotFound, None),
        "abortUpload" | "putChunk" => addressed_case(NotFound, Some(("getUpload", Who::Member))),
        "completeUpload" => addressed_case(NotFound, Some(("listChildren", o))),
        "putContent" => addressed_case(NotFound, Some(("listVersions", o))),

        // ------------------------------------------------------------ versions and diff
        "getVersion" | "restoreVersion" => {
            let read = (op == "restoreVersion").then_some(("listVersions", o));
            let mut p = addressed(op, w, NotFound, read);
            let method = std_req(op, a).method;
            let rest = if op == "restoreVersion" {
                "/restore"
            } else {
                ""
            };
            p.push(Probe::new(
                "B's owner, A's version under B's file",
                o,
                RequestSpec::new(
                    method,
                    format!("/v1/nodes/{}/versions/{}{rest}", b.plan, a.v1),
                )
                .tenant(b.id),
                NotFound,
            ));
            plain("addressed resource of tenant A", p)
        }
        "setVersionLabel" => addressed_case(NotFound, Some(("listVersions", o))),
        "diffVersions" => {
            let mut p = addressed(op, w, NotFound, None);
            p.push(Probe::new(
                "B's owner diffs B's file against A's version",
                o,
                node_get(b, b.plan, &format!("/diff?from={}&to={}", a.v1, b.v2)),
                Rejected,
            ));
            plain("addressed resource of tenant A", p)
        }

        // ------------------------------------------------------------ tags
        "createTag" => plain(
            "creates in the caller's tenant",
            own_tenant(op, w, Some(("listTags", o))),
        ),
        "getTag" | "listNodesByTag" => addressed_case(NotFound, None),
        "updateTag" | "deleteTag" => addressed_case(NotFound, Some(("getTag", o))),
        "attachTag" | "detachTag" => {
            let read = if op == "attachTag" {
                node_get(a, a.archive, "/tags")
            } else {
                std_req("listNodeTags", a)
            };
            let mut p = addressed(op, w, NotFound, None);
            for x in p.iter_mut() {
                x.unchanged = Some((o, read.clone()));
            }
            let method = std_req(op, a).method;
            p.push(Probe::new(
                "B's owner, B's node with A's tag",
                o,
                RequestSpec::new(
                    method.clone(),
                    format!("/v1/nodes/{}/tags/{}", b.plan, a.tag),
                )
                .tenant(b.id),
                NotFound,
            ));
            p.push(
                Probe::new(
                    "B's owner, A's node with B's tag",
                    o,
                    RequestSpec::new(method, format!("/v1/nodes/{}/tags/{}", a.plan, b.tag))
                        .tenant(b.id),
                    NotFound,
                )
                .unchanged(o, std_req("listNodeTags", a)),
            );
            plain("node or tag in tenant A", p)
        }

        // ------------------------------------------------------------ sharing
        "addGrant" => addressed_case(NotFound, Some(("listGrants", o))),
        "updateGrant" | "removeGrant" => {
            let mut p = addressed(op, w, NotFound, Some(("listGrants", o)));
            let std = std_req(op, a);
            let mut mixed = RequestSpec::new(
                std.method,
                format!("/v1/nodes/{}/grants/{}", b.projects, a.grant),
            )
            .tenant(b.id);
            mixed.body = std.body;
            p.push(
                Probe::new("B's owner, A's grant under B's folder", o, mixed, NotFound)
                    .unchanged(o, std_req("listGrants", a)),
            );
            plain("addressed resource of tenant A", p)
        }
        "createShareLink" | "revokeShareLink" => {
            addressed_case(NotFound, Some(("listShareLinks", o)))
        }
        "resolveShareLink" => plain(
            "share link tokens are bearer secrets by design; the probe checks that an unknown \
             token is 404 and that B's own link leaks nothing of A",
            vec![
                Probe::new(
                    "B's owner, unknown token",
                    o,
                    RequestSpec::post("/v1/share-links/resolve")
                        .json(json!({ "token": "no-such-share-link-token" })),
                    NotFound,
                ),
                Probe::new(
                    "anonymous, B's token",
                    Who::Anonymous,
                    std_req(op, b),
                    NoLeak,
                ),
                Probe::new(
                    "B's share-link bearer claims tenant A",
                    Who::ShareLink,
                    std_req("getNode", b).tenant(a.id),
                    Denied,
                ),
                Probe::new(
                    "B's share-link bearer reads A's node with A's tenant header",
                    Who::ShareLink,
                    std_req("getNode", a),
                    Denied,
                ),
            ],
        ),

        // ------------------------------------------------------------ changes and search
        "listChanges" => {
            let mut p = own_tenant(op, w, None);
            p.push(Probe::new(
                "B's owner filters by A's node",
                o,
                RequestSpec::get(format!("/v1/changes?node={}", a.plan)).tenant(b.id),
                NoLeak,
            ));
            plain("the feed of the caller's tenant", p)
        }
        "searchNodes" => {
            let mut p = own_tenant(op, w, None);
            p.push(Probe::new(
                "B's owner searches for A's file name",
                o,
                RequestSpec::get(format!("/v1/search?q={}", a.plan_name)).tenant(b.id),
                NoLeak,
            ));
            p.push(Probe::new(
                "B's owner searches within A's folder",
                o,
                RequestSpec::get(format!("/v1/search?q=plan&within={}", a.projects)).tenant(b.id),
                NoLeak,
            ));
            plain("search in the caller's tenant", p)
        }

        _ => None,
    }
}

/// Runs one probe in `app` and returns its outcome.
pub async fn run_probe(
    app: &axum::Router,
    w: &World,
    fallback: &Response,
    probe: &Probe,
) -> Outcome {
    let actor: &Principal = w.b.principal(probe.actor);
    let before = match &probe.unchanged {
        Some((who, read)) => Some(client::send(app, read, w.a.principal(*who)).await),
        None => None,
    };
    let res = client::send(app, &probe.request, actor).await;
    let after = match &probe.unchanged {
        Some((who, read)) => Some(client::send(app, read, w.a.principal(*who)).await),
        None => None,
    };

    let mut problems: Vec<String> = Vec::new();
    if client::is_unrouted(&res, fallback) {
        return Outcome::Failed(format!(
            "{}: no handler answers this method and path (the route is missing)",
            probe.request.label()
        ));
    }
    // A leak is a failure even when the handler is otherwise unfinished.
    let sent = probe.request.sent_text();
    let text = res.text();
    let leaked: Vec<String> =
        w.a.markers()
            .into_iter()
            .filter(|m| !sent.contains(m.as_str()) && text.contains(m.as_str()))
            .collect();
    if !leaked.is_empty() {
        problems.push(format!("the answer holds data of tenant A: {leaked:?}"));
    }
    let pending = res.is_not_implemented();
    if !pending {
        if res.status.is_server_error() {
            problems.push(format!("server error {}", res.status));
        } else if res.status == StatusCode::UNAUTHORIZED && probe.actor != Who::Anonymous {
            problems.push(format!(
                "401: tenant B's principal was not authenticated{}",
                if w.seeded {
                    ""
                } else {
                    " (fixtures::seed still makes placeholder credentials; implement it)"
                }
            ));
        } else if !probe.expect.accepts(res.status) {
            problems.push(format!(
                "{} got {}, expected {}",
                probe.request.label(),
                res.status,
                probe.expect.describe()
            ));
        }
    }
    if let (Some(b), Some(a)) = (&before, &after) {
        if b.is_not_implemented() || a.is_not_implemented() {
            // The read is not built yet; the write cannot be checked.
            if !pending && problems.is_empty() {
                return Outcome::Pending;
            }
        } else if b.status != a.status || b.body != a.body {
            problems.push(format!(
                "tenant A's state changed: {} gave {} before and {} after",
                probe
                    .unchanged
                    .as_ref()
                    .map(|u| u.1.label())
                    .unwrap_or_default(),
                b.status,
                a.status
            ));
        }
    }
    if !problems.is_empty() {
        Outcome::Failed(problems.join("; "))
    } else if pending {
        Outcome::Pending
    } else {
        Outcome::Passed
    }
}

/// Runs the whole suite and returns the report.
pub async fn run() -> Report {
    let mut report = Report::new("tenant isolation suite");
    let ops = crate::contract::operations();

    // Registration: every contract operation needs a case.
    let probe_world = fixtures::seed(&fixtures::app_state()).await;
    for op in &ops {
        if case(&op.id, &probe_world).is_none() {
            report.missing.push(op.id.clone());
        }
    }

    for op in &ops {
        let state = fixtures::app_state();
        let w = fixtures::seed(&state).await;
        let Some(c) = case(&op.id, &w) else { continue };
        let app = client::app(state);
        let fallback = client::fallback_fingerprint(&app, w.b.principal(Who::Owner)).await;
        if c.probes.is_empty() {
            report.add(
                &op.id,
                "case",
                Outcome::Failed("the case has no probes".into()),
            );
        }
        for p in &c.probes {
            let outcome = run_probe(&app, &w, &fallback, p).await;
            report.add(&op.id, &p.label, outcome);
        }
    }
    report
}
