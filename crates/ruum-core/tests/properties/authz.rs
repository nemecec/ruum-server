//! Permissions with links (design sections 2.3, 3.2 and 3.3; decision 0001-1).
//!
//! A random world is built through the in-memory services, the way the API builds it: a team
//! tenant with an owner, an admin, a member and three guests; a random tree of folders, files,
//! shortcut links and member links; random grants (to users or to all members); random share
//! links; random `editors_can_share` settings; the tenant policy `members_can_share`; and random
//! agents (a user, a scope subtree, a highest role).
//!
//! The reference model, written here from the design and independent of the engine:
//!
//! - Owners, admins and members manage the whole team tree. A guest's role on a node is the
//!   highest grant to the guest on the node or an ancestor. A grant to all members applies to
//!   owners, admins and members, never to guests.
//! - A member link in folder F to target T gives `view` (never more) on T and everything below
//!   it to everyone who can view F, also through other member links (a fixpoint). A shortcut
//!   link gives nothing.
//! - A share link with role r on node N gives r on N and below. It never has `manage` or
//!   `share`.
//! - `editors_can_share` of a node is the value of the nearest node, from the node up, that sets
//!   one; true when none does.
//! - `share`: owners and admins; members while `members_can_share` is on; a `manage` grant; an
//!   `edit` grant while `editors_can_share` is true.
//! - An agent's right is its user's right intersected with its scope: nothing outside the
//!   subtree (by tree ancestry), at most `max_role`, and `share` only with `max_role` `manage`,
//!   or `edit` where `editors_can_share` is true.
//! - `require` answers `not_found` when the principal cannot see the node (or it is outside the
//!   scope subtree), `permission_denied` when it sees the node but lacks the permission, and
//!   `scope_exceeded` when only the scope stops it.

use crate::common::{Team, block_on, new_node};
use proptest::prelude::*;
use proptest::sample::Index;
use ruum_core::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use ruum_core::authz::{
    self, Authz, ObjectRef, Permission, RelationshipUpdate, Role, Scope, SubjectRef,
    memory::MemoryAuthz,
    spicedb::{SpiceDbAuthz, SpiceDbConfig},
};
use ruum_core::nodes::{LinkKind, NodeKind};
use ruum_core::sharing::{Grantee, NewShareLink, ShareLinkRole};
use ruum_core::tenants::{MemberRole, TenantPolicyChange};
use ruum_core::{Error, ErrorCode, NodeId, PrincipalId, ShareLinkId};
use std::collections::{HashMap, HashSet};

/// The tenant roles of users 1..: user 0 is the owner.
const OTHERS: [MemberRole; 5] = [
    MemberRole::Admin,
    MemberRole::Member,
    MemberRole::Guest,
    MemberRole::Guest,
    MemberRole::Guest,
];
const USERS: usize = OTHERS.len() + 1;
/// The grantee index that means "all members".
const ALL_MEMBERS: usize = USERS;

const ROLES: [Role; 4] = [Role::View, Role::Comment, Role::Edit, Role::Manage];
const PERMISSIONS: [Permission; 5] = [
    Permission::View,
    Permission::Comment,
    Permission::Edit,
    Permission::Manage,
    Permission::Share,
];

// ------------------------------------------------------------------------------ the world

#[derive(Clone, Debug)]
enum KindPick {
    Folder,
    File,
    Shortcut(Index),
    Member(Index),
}

#[derive(Clone, Debug)]
struct Spec {
    /// Parent (among the folders made so far, the root first) and kind of each node.
    nodes: Vec<(Index, KindPick)>,
    /// Node, grantee (a user index or [`ALL_MEMBERS`]) and role.
    grants: Vec<(Index, usize, Role)>,
    links: Vec<(Index, ShareLinkRole)>,
    settings: Vec<(Index, Option<bool>)>,
    members_can_share: bool,
    /// User, scope subtree (`None`: the whole tenant) and highest role.
    agents: Vec<(usize, Option<Index>, Role)>,
}

fn role() -> impl Strategy<Value = Role> {
    prop::sample::select(ROLES.to_vec())
}

fn spec() -> impl Strategy<Value = Spec> {
    let kind = prop_oneof![
        3 => Just(KindPick::Folder),
        2 => Just(KindPick::File),
        1 => any::<Index>().prop_map(KindPick::Shortcut),
        2 => any::<Index>().prop_map(KindPick::Member),
    ];
    let link_role = prop::sample::select(vec![
        ShareLinkRole::View,
        ShareLinkRole::Comment,
        ShareLinkRole::Edit,
    ]);
    (
        prop::collection::vec((any::<Index>(), kind), 1..9),
        prop::collection::vec((any::<Index>(), 0..=ALL_MEMBERS, role()), 0..7),
        prop::collection::vec((any::<Index>(), link_role), 0..3),
        prop::collection::vec((any::<Index>(), prop::option::of(any::<bool>())), 0..4),
        any::<bool>(),
        prop::collection::vec((0..USERS, prop::option::of(any::<Index>()), role()), 0..4),
    )
        .prop_map(
            |(nodes, grants, links, settings, members_can_share, agents)| Spec {
                nodes,
                grants,
                links,
                settings,
                members_can_share,
                agents,
            },
        )
}

/// A grantee in the model.
#[derive(Clone, Copy, Debug, PartialEq)]
enum To {
    User(usize),
    AllMembers,
}

/// The world as built, in model terms (node 0 is the root).
struct World {
    team: Team,
    ids: Vec<NodeId>,
    parent: Vec<Option<usize>>,
    /// `(folder, target)` of each member link.
    member_links: Vec<(usize, usize)>,
    grants: Vec<(usize, To, Role)>,
    links: Vec<(usize, ShareLinkId, Role)>,
    settings: HashMap<usize, bool>,
    members_can_share: bool,
    agents: Vec<(usize, Option<usize>, Role)>,
}

async fn build(spec: &Spec) -> World {
    let team = Team::new(&OTHERS).await;
    let owner = team.owner();
    let s = &team.s;
    let mut ids = vec![team.root];
    let mut parent = vec![None];
    let mut folders = vec![0usize];
    let mut member_links = Vec::new();
    for (i, (p, kind)) in spec.nodes.iter().enumerate() {
        let p = folders[p.index(folders.len())];
        let existing = ids.len() - 1; // non-root nodes made so far
        let (kind, link) = match kind {
            KindPick::Folder => (NodeKind::Folder, None),
            KindPick::File => (NodeKind::File, None),
            KindPick::Shortcut(t) | KindPick::Member(t) if existing > 0 => {
                let target = 1 + t.index(existing);
                let lk = match kind {
                    KindPick::Shortcut(_) => LinkKind::Shortcut,
                    _ => LinkKind::Member,
                };
                (NodeKind::Link, Some((lk, target)))
            }
            _ => (NodeKind::Folder, None),
        };
        let name = format!("n{i}");
        let req = new_node(ids[p], &name, kind, link.map(|(k, t)| (k, ids[t])));
        let mut link = link;
        let n = match s.nodes.create(&owner, req).await {
            Ok(n) => n,
            // A member link inside its own target (directly or through other member links)
            // would make a loop of `view`; the store refuses it. Make a shortcut instead.
            Err(Error::Api(a))
                if a.code == ErrorCode::ValidationFailed
                    && matches!(link, Some((LinkKind::Member, _))) =>
            {
                let (_, t) = link.expect("a link");
                assert!(
                    view_reaches(&parent, &member_links, p, t),
                    "a member link from node {p} to node {t} without a loop was refused"
                );
                link = Some((LinkKind::Shortcut, t));
                let req = new_node(ids[p], &name, kind, Some((LinkKind::Shortcut, ids[t])));
                s.nodes.create(&owner, req).await.expect("create shortcut")
            }
            Err(e) => panic!("create node: {e}"),
        };
        if let Some((LinkKind::Member, t)) = link {
            assert!(
                !view_reaches(&parent, &member_links, p, t),
                "a member link from node {p} to node {t} closes a loop"
            );
        }
        if kind == NodeKind::Folder {
            folders.push(ids.len());
        }
        if let Some((LinkKind::Member, t)) = link {
            member_links.push((p, t));
        }
        ids.push(n.value.id);
        parent.push(Some(p));
    }
    let pick = |i: &Index| i.index(ids.len());

    let mut settings = HashMap::new();
    for (n, v) in &spec.settings {
        let n = pick(n);
        s.sharing
            .update_node_sharing(&owner, ids[n], *v, None)
            .await
            .expect("update_node_sharing");
        match v {
            Some(v) => settings.insert(n, *v),
            None => settings.remove(&n),
        };
    }

    let mut grants = Vec::new();
    for (n, who, role) in &spec.grants {
        let n = pick(n);
        let (grantee, to) = if *who == ALL_MEMBERS {
            (Grantee::TenantMembers, To::AllMembers)
        } else {
            (
                Grantee::User {
                    id: team.users[*who].0,
                },
                To::User(*who),
            )
        };
        match s.sharing.add_grant(&owner, ids[n], grantee, *role).await {
            Ok(_) => grants.push((n, to, *role)),
            // The same grantee on the same node twice: the store refuses the second one.
            Err(Error::Api(a)) if a.code == ErrorCode::Conflict => {}
            Err(e) => panic!("add_grant: {e}"),
        }
    }

    s.tenants
        .update(
            &owner,
            None,
            Some(TenantPolicyChange {
                allow_edit_links: Some(true),
                members_can_share: Some(spec.members_can_share),
                ..TenantPolicyChange::default()
            }),
        )
        .await
        .expect("policy");

    let mut links = Vec::new();
    for (n, role) in &spec.links {
        let n = pick(n);
        let l = s
            .sharing
            .create_share_link(
                &owner,
                ids[n],
                NewShareLink {
                    role: *role,
                    expires_at: None,
                    password: None,
                },
            )
            .await
            .expect("create_share_link");
        links.push((n, l.value.id, role.as_role()));
    }

    let agents = spec
        .agents
        .iter()
        .map(|(u, sub, r)| (*u, sub.as_ref().map(pick), *r))
        .collect();
    World {
        team,
        ids,
        parent,
        member_links,
        grants,
        links,
        settings,
        members_can_share: spec.members_can_share,
        agents,
    }
}

/// Whether `goal` is reachable from `from` along the edges that `view` follows: node to
/// parent, and member-link target to the folder of the link. A new member link in F to T closes
/// a loop exactly when T is reachable from F.
fn view_reaches(
    parent: &[Option<usize>],
    links: &[(usize, usize)],
    from: usize,
    goal: usize,
) -> bool {
    let mut seen = HashSet::new();
    let mut stack = vec![from];
    while let Some(x) = stack.pop() {
        if x == goal {
            return true;
        }
        if seen.insert(x) {
            stack.extend(parent[x]);
            stack.extend(links.iter().filter(|(_, t)| *t == x).map(|(f, _)| *f));
        }
    }
    false
}

// ------------------------------------------------------------------------------ the model

/// Whose right a check asks for.
#[derive(Clone, Copy, Debug)]
enum Who {
    User(usize),
    /// Agent index into `World::agents`.
    Agent(usize),
    /// Share link index into `World::links`.
    Link(usize),
}

/// The graph subject of the model (an agent's subject is its user).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Subject {
    User(usize),
    Link(usize),
}

/// The outcome of `require`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Out {
    Ok,
    NotFound,
    Denied,
    ScopeExceeded,
}

struct Model<'a> {
    w: &'a World,
    /// Nodes each subject can view, with member links (memoized).
    views: std::cell::RefCell<HashMap<Subject, HashSet<usize>>>,
}

impl<'a> Model<'a> {
    fn new(w: &'a World) -> Self {
        Self {
            w,
            views: Default::default(),
        }
    }

    /// The node and its ancestors, the node first.
    fn chain(&self, x: usize) -> Vec<usize> {
        let mut out = vec![x];
        let mut cur = x;
        while let Some(p) = self.w.parent[cur] {
            out.push(p);
            cur = p;
        }
        out
    }

    fn member_role(&self, u: usize) -> MemberRole {
        if u == 0 {
            MemberRole::Owner
        } else {
            OTHERS[u - 1]
        }
    }

    fn is_member(&self, u: usize) -> bool {
        self.member_role(u) >= MemberRole::Member
    }

    fn grant_applies(&self, to: To, u: usize) -> bool {
        match to {
            To::User(g) => g == u,
            To::AllMembers => self.is_member(u),
        }
    }

    /// The role from the team, grants and share links, without member links.
    fn direct_role(&self, s: Subject, x: usize) -> Option<Role> {
        let chain = self.chain(x);
        match s {
            Subject::User(u) => {
                if self.is_member(u) {
                    return Some(Role::Manage);
                }
                self.w
                    .grants
                    .iter()
                    .filter(|(n, to, _)| chain.contains(n) && self.grant_applies(*to, u))
                    .map(|(_, _, r)| *r)
                    .max()
            }
            Subject::Link(l) => {
                let (n, _, r) = self.w.links[l];
                chain.contains(&n).then_some(r)
            }
        }
    }

    /// The nodes the subject can view: direct roles, then member links to a fixpoint.
    fn viewable(&self, s: Subject) -> HashSet<usize> {
        if let Some(v) = self.views.borrow().get(&s) {
            return v.clone();
        }
        let n = self.w.ids.len();
        let mut v: HashSet<usize> = (0..n)
            .filter(|x| self.direct_role(s, *x).is_some())
            .collect();
        loop {
            let mut grew = false;
            for &(folder, target) in &self.w.member_links {
                if !v.contains(&folder) {
                    continue;
                }
                for x in 0..n {
                    if !v.contains(&x) && self.chain(x).contains(&target) {
                        v.insert(x);
                        grew = true;
                    }
                }
            }
            if !grew {
                break;
            }
        }
        self.views.borrow_mut().insert(s, v.clone());
        v
    }

    fn graph_role(&self, s: Subject, x: usize) -> Option<Role> {
        self.direct_role(s, x)
            .or_else(|| self.viewable(s).contains(&x).then_some(Role::View))
    }

    fn editors_can_share(&self, x: usize) -> bool {
        self.chain(x)
            .iter()
            .find_map(|n| self.w.settings.get(n).copied())
            .unwrap_or(true)
    }

    fn graph_share(&self, s: Subject, x: usize) -> bool {
        let Subject::User(u) = s else {
            return false;
        };
        let chain = self.chain(x);
        let granted = |role: Role| {
            self.w
                .grants
                .iter()
                .any(|(n, to, r)| *r == role && chain.contains(n) && self.grant_applies(*to, u))
        };
        let manager = self.member_role(u) >= MemberRole::Admin
            || (self.is_member(u) && self.w.members_can_share)
            || granted(Role::Manage);
        manager || (granted(Role::Edit) && self.editors_can_share(x))
    }

    fn subject(&self, who: Who) -> Subject {
        match who {
            Who::User(u) => Subject::User(u),
            Who::Agent(a) => Subject::User(self.w.agents[a].0),
            Who::Link(l) => Subject::Link(l),
        }
    }

    /// The highest role the scope leaves, and whether the node is inside its subtree.
    fn scope(&self, who: Who, x: usize) -> Option<(Role, bool)> {
        match who {
            Who::User(_) => None,
            Who::Agent(a) => {
                let (_, sub, max) = self.w.agents[a];
                Some((max, sub.is_none_or(|s| self.chain(x).contains(&s))))
            }
            // The graph limits a link to its node; only the role is capped.
            Who::Link(l) => Some((self.w.links[l].2, true)),
        }
    }

    fn role_on(&self, who: Who, x: usize) -> Option<Role> {
        let role = self.graph_role(self.subject(who), x);
        match self.scope(who, x) {
            None => role,
            Some((_, false)) => None,
            Some((max, true)) => role.map(|r| r.min(max)),
        }
    }

    fn require(&self, who: Who, x: usize, p: Permission) -> Out {
        let s = self.subject(who);
        let scope = self.scope(who, x);
        if matches!(scope, Some((_, false))) {
            return Out::NotFound;
        }
        let role = self.graph_role(s, x);
        let has = match p {
            Permission::Share => self.graph_share(s, x),
            p => role >= Some(role_of(p)),
        };
        if !has {
            return if p != Permission::View && role.is_some() {
                Out::Denied
            } else {
                Out::NotFound
            };
        }
        let Some((cap, _)) = scope else {
            return Out::Ok;
        };
        let allowed = match p {
            Permission::Share => {
                if matches!(who, Who::Link(_)) {
                    return Out::Denied;
                }
                cap == Role::Manage || (cap == Role::Edit && self.editors_can_share(x))
            }
            p => cap >= role_of(p),
        };
        if allowed { Out::Ok } else { Out::ScopeExceeded }
    }
}

fn role_of(p: Permission) -> Role {
    match p {
        Permission::View => Role::View,
        Permission::Comment => Role::Comment,
        Permission::Edit => Role::Edit,
        Permission::Manage | Permission::Share => Role::Manage,
    }
}

// ------------------------------------------------------------------------------ the engine

fn ctx_of(w: &World, who: Who) -> Context {
    let tenant = w.team.tenant;
    match who {
        Who::User(u) => w.team.ctx_of(u),
        Who::Agent(a) => {
            let (u, sub, max_role) = w.agents[a];
            Context {
                tenant,
                actor: Actor {
                    actor: PrincipalRef {
                        kind: PrincipalKind::Agent,
                        id: PrincipalId(uuid::Uuid::from_u128(a as u128 + 1)),
                    },
                    on_behalf_of: Some(w.team.users[u].0),
                },
                scope: Some(Scope {
                    subtree: sub.map(|s| w.ids[s]),
                    max_role,
                    expires_at: None,
                }),
                idempotency_key: None,
            }
        }
        // As the API builds it from a share-link token (`link_scope`).
        Who::Link(l) => {
            let (n, id, role) = w.links[l];
            Context {
                tenant,
                actor: Actor {
                    actor: PrincipalRef {
                        kind: PrincipalKind::ShareLink,
                        id: PrincipalId(id.0),
                    },
                    on_behalf_of: None,
                },
                scope: Some(Scope {
                    subtree: Some(w.ids[n]),
                    max_role: role,
                    expires_at: None,
                }),
                idempotency_key: None,
            }
        }
    }
}

fn outcome(r: ruum_core::Result<()>) -> Out {
    match r {
        Ok(()) => Out::Ok,
        Err(Error::Api(a)) => match a.code {
            ErrorCode::NotFound => Out::NotFound,
            ErrorCode::PermissionDenied => Out::Denied,
            ErrorCode::ScopeExceeded => Out::ScopeExceeded,
            other => panic!("require: unexpected code {other:?}"),
        },
        Err(e) => panic!("require: {e}"),
    }
}

fn everyone(w: &World) -> Vec<Who> {
    let mut v: Vec<Who> = (0..USERS).map(Who::User).collect();
    v.extend((0..w.agents.len()).map(Who::Agent));
    v.extend((0..w.links.len()).map(Who::Link));
    v
}

/// The engine's answers for every principal, node and permission.
struct Answers {
    require: HashMap<(usize, usize, usize), Out>,
    role: HashMap<(usize, usize), Option<Role>>,
}

async fn ask(w: &World, people: &[Who]) -> Answers {
    let s = &w.team.s;
    let mut a = Answers {
        require: HashMap::new(),
        role: HashMap::new(),
    };
    for (i, who) in people.iter().enumerate() {
        let ctx = ctx_of(w, *who);
        for (x, id) in w.ids.iter().enumerate() {
            for (pi, p) in PERMISSIONS.iter().enumerate() {
                let r = authz::require(s.authz.as_ref(), s.nodes.as_ref(), &ctx, *id, *p).await;
                a.require.insert((i, x, pi), outcome(r));
            }
            let role = authz::role_on(s.authz.as_ref(), s.nodes.as_ref(), &ctx, *id)
                .await
                .expect("role_on");
            a.role.insert((i, x), role);
        }
    }
    a
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// The in-memory engine (`authz::require`, `authz::role_on` over the graph that the stores
    /// wrote) agrees with the reference model for every principal, node and permission.
    #[test]
    fn engine_agrees_with_the_model(spec in spec()) {
        block_on(async {
            let w = build(&spec).await;
            let m = Model::new(&w);
            let people = everyone(&w);
            let a = ask(&w, &people).await;
            for (i, who) in people.iter().enumerate() {
                for x in 0..w.ids.len() {
                    for (pi, p) in PERMISSIONS.iter().enumerate() {
                        let got = a.require[&(i, x, pi)];
                        let want = m.require(*who, x, *p);
                        assert_eq!(got, want, "require({who:?}, node {x}, {p:?})");
                    }
                    assert_eq!(a.role[&(i, x)], m.role_on(*who, x), "role_on({who:?}, node {x})");
                }
            }
        });
    }

    /// Model-free invariants: an agent never gets more than its user (no permission its user
    /// lacks, no higher role); a share link never has `manage` or `share`.
    #[test]
    fn agents_never_exceed_their_user_and_links_never_manage(spec in spec()) {
        block_on(async {
            let w = build(&spec).await;
            let people = everyone(&w);
            let a = ask(&w, &people).await;
            for (i, who) in people.iter().enumerate() {
                for x in 0..w.ids.len() {
                    match who {
                        Who::Agent(ag) => {
                            let user = w.agents[*ag].0;
                            // `people` starts with the users, so a user's index is its number.
                            for (pi, p) in PERMISSIONS.iter().enumerate() {
                                if a.require[&(i, x, pi)] == Out::Ok {
                                    assert_eq!(
                                        a.require[&(user, x, pi)], Out::Ok,
                                        "agent {ag} has {p:?} on node {x}, its user does not"
                                    );
                                }
                            }
                            assert!(a.role[&(i, x)] <= a.role[&(user, x)], "agent {ag}, node {x}");
                        }
                        Who::Link(l) => {
                            assert!(a.role[&(i, x)] <= Some(Role::Edit), "link {l}, node {x}");
                            for p in [Permission::Manage, Permission::Share] {
                                let pi = PERMISSIONS.iter().position(|q| *q == p).unwrap();
                                assert_ne!(a.require[&(i, x, pi)], Out::Ok, "link {l} {p:?} node {x}");
                            }
                        }
                        Who::User(_) => {}
                    }
                }
            }
        });
    }

    /// A member link gives at most `view`: dropping every `collection` tuple from the graph
    /// changes a subject's role on a node only from `view` to none.
    #[test]
    fn member_links_give_at_most_view(spec in spec()) {
        block_on(async {
            let w = build(&spec).await;
            let all = w.team.h.authz.all();
            let without = MemoryAuthz::default();
            without
                .write(
                    all.iter()
                        .filter(|r| r.relation != "collection")
                        .cloned()
                        .map(RelationshipUpdate::Touch)
                        .collect(),
                )
                .await
                .unwrap();
            for subject in subjects(&w) {
                for (x, id) in w.ids.iter().enumerate() {
                    let with = authz::graph_role(w.team.h.authz.as_ref(), &subject, *id).await.unwrap();
                    let plain = authz::graph_role(&without, &subject, *id).await.unwrap();
                    if with != plain {
                        assert_eq!((with, plain), (Some(Role::View), None), "{subject} on node {x}");
                    }
                    // `share` never comes from a member link either.
                    let obj = ObjectRef::node(*id);
                    let s1 = w.team.h.authz.check(&subject, "share", &obj, None).await.unwrap();
                    let s2 = without.check(&subject, "share", &obj, None).await.unwrap();
                    assert_eq!(s1, s2, "share of {subject} on node {x}");
                }
            }
        });
    }
}

/// The plain graph subjects of the world: every user and every share link.
fn subjects(w: &World) -> Vec<SubjectRef> {
    let mut v: Vec<SubjectRef> = w
        .team
        .users
        .iter()
        .map(|(u, _)| SubjectRef::user(*u))
        .collect();
    v.extend(
        w.links
            .iter()
            .map(|(_, id, _)| SubjectRef::object(ObjectRef::new("share_link", id))),
    );
    v
}

// ------------------------------------------------------------------------------ SpiceDB

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    /// The graph that the in-memory stores wrote gives the same answers in SpiceDB (the laptop
    /// stack) as in the in-memory engine: every subject, node and node permission. Skipped
    /// when `SPICEDB_PRESHARED_KEY` is not set (`eval "$(scripts/dev.sh env)"`). The test
    /// deletes its relationships afterwards (all IDs are fresh UUIDs, so runs do not clash).
    #[test]
    fn spicedb_agrees_with_the_memory_engine(spec in spec()) {
        let Some(config) = SpiceDbConfig::from_env() else {
            eprintln!("skipped: SPICEDB_PRESHARED_KEY is not set");
            return Ok(());
        };
        block_on(async {
            let spice = SpiceDbAuthz::connect(config).await.expect("connect to SpiceDB");
            let w = build(&spec).await;
            let tuples = w.team.h.authz.all();
            spice
                .write(tuples.iter().cloned().map(RelationshipUpdate::Touch).collect())
                .await
                .expect("write to SpiceDB");
            let objects: Vec<ObjectRef> = w.ids.iter().map(|id| ObjectRef::node(*id)).collect();
            let mut mismatch = None;
            'outer: for subject in subjects(&w) {
                for perm in ["view", "comment", "edit", "manage", "share", "editors_cannot_share"] {
                    let mem = w.team.h.authz.check_bulk(&subject, perm, &objects).await.unwrap();
                    let sp = spice.check_bulk(&subject, perm, &objects).await.expect("SpiceDB check");
                    if mem != sp {
                        mismatch = Some(format!("{subject} {perm}: memory {mem:?}, SpiceDB {sp:?}"));
                        break 'outer;
                    }
                }
            }
            spice
                .write(tuples.into_iter().map(RelationshipUpdate::Delete).collect())
                .await
                .expect("clean up SpiceDB");
            if let Some(m) = mismatch {
                panic!("{m}");
            }
        });
    }
}
