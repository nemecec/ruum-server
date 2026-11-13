//! Test fixtures: two tenants (A and B) with the same shape, a principal for every role, and the
//! resources that the suites address.
//!
//! Each tenant is a team tenant with this tree and these principals:
//!
//! ```text
//! root
//! ├── Projects/            (F)  grants: guest_view=view, guest_comment=comment, guest_edit=edit,
//! │   │                              guest_manage=manage
//! │   ├── <prefix>-plan.txt (X)  two versions v1 and v2, tag T attached
//! │   ├── Archive/          (S)  editors_can_share = false (editors keep edit, lose share)
//! │   ├── <prefix>-garden   (D)  a database node (SQLite 3), made by `member` (card D-1), with
//! │   │                          the table `plants` (id, latin_name, common_name; two rows)
//! │   ├── <prefix>-planner  (W)  a web app node, made by `member` (card W-1), one live deployment
//! │   ├── <prefix>-bug      (I)  an issue node (step 3, planned)
//! │   ├── <prefix>-kit      (K)  a project template folder (step 3, planned)
//! │   └── <prefix>-old.txt  (Y)  in the trash (its old parent is Projects)
//! └── Private/             (G)  no grants
//! ```
//!
//! | Principal | What it is |
//! |---|---|
//! | `owner`, `admin`, `member` | tenant roles; owners and admins have `manage` (and so `share`) on the root; members have `manage` from the team root (`node:<root>#team@tenant`, content role `manage`, the default), and `share` while the tenant policy `members_can_share` is true (the default) |
//! | `member_view`, `member_comment`, `member_edit` | members with the content role `view`, `comment` or `edit` (decision 0003-1): that role on the whole team tree from the team root; `member_edit` shares where `editors_can_share` is true (not in `Archive`) |
//! | `guest` | a guest without grants |
//! | `guest_view` ... `guest_manage` | guests with one grant on `Projects`; `guest_edit` shares where `editors_can_share` is true (not in `Archive`), `guest_manage` everywhere below `Projects` |
//! | `agent` | the agent of `member`: scope `Projects`, `max_role: view` (the narrow agent) |
//! | `agent_out` | the agent of `member`: scope `Private`, `max_role: manage` |
//! | `agent_of_guest_view` | the agent of `guest_view`: scope everything, `max_role: manage` |
//! | `share_link` | the bearer of the `view` share link on `Projects` (the token of `resolveShareLink`) |
//! | `anonymous` | no credentials |
//!
//! The database D is made by `member` through `createDatabase` (the database provider, card
//! D-1), and `seed` waits until it is `ready`. Then the step "garden tables" makes the table
//! `plants` as the database's owner kind through the engine (wave 4 gate G-1): `id integer
//! primary key, latin_name text not null, common_name text`, with two rows of plant names (no
//! personal data). The standard requests of `queryDatabase`, `suggestColumnClasses` and
//! `confirmColumnClasses` address it. `member` gives `agent` a `read_personal` grant on it for 8
//! hours (card A-1). The pending request of `agent` on it is a placeholder ID until A-2 seeds it
//! (it keeps the field name).
//!
//! Step 3 (docs/STEP3.md, section "Fixtures"): card W-1 seeds the web app W (made by `member`
//! through `createApp`), its live deployment (through the store) and both approvals of the
//! tenant. The IDs of W's role grant, domain, API key, approval and trace, the issue I with its
//! receipt token, the template K and its setup are placeholders until the cards that build
//! their operations seed them (they keep the field names). The seeded W has one live
//! deployment whose manifest declares the roles `member` (default) and `approver`, the entered
//! secret `STRIPE_KEY` and the generated secret `SESSION_KEY`, an external API `orders` with the
//! scopes `orders.read` and `orders.write`, and a binding `db` with role `edit` on D. Both tenants
//! have the platform admin's approval of public apps and custom domains. The pending publish
//! request of W is a request of `agent` (seeded through the store; its `view` scope cannot
//! publish through the API). I is a `report` issue of W in `Projects` (the reporter is not a
//! member, so it has a receipt token); K is a project template whose setup makes a database and
//! an app; the setup made the project `<prefix>-from-kit` in `Projects`.
//!
//! The tag T was made by `member`. The upload sessions were started by `member`. The web session
//! and the WebDAV app password belong to `member`. The invitation is for a new email address with the role `member`. The
//! OAuth client is a registered public client (OAuth clients belong to no tenant); the OAuth
//! request is the signed consent request of `oauthAuthorize` for that client.
//!
//! # Seeding
//!
//! [`seed`] makes the world in the services of an [`AppState`]. While the core services are stubs,
//! it only makes the IDs and placeholder bearer tokens ([`World::seeded`] is `false`); requests
//! with these tokens can only be answered with `501`. When the identity, tenants, nodes and
//! sharing areas exist, `seed` must create every tenant, user, grant and resource through
//! `Services` and issue real credentials (`TokenIssuer`, sessions, agent key exchange). This is
//! the only function to change; the suites stay as they are.

use ruum_api::AppState;
use ruum_core::authz::Role;
use ruum_core::tenants::MemberRole;
use ruum_core::{
    AgentId, AgentKeyId, AppPasswordId, GrantId, InvitationId, NodeId, ShareLinkId, TagId,
    TenantId, UploadId, UserId, VersionId,
};
use std::collections::BTreeMap;
use uuid::Uuid;

/// The principals of a tenant fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Who {
    Anonymous,
    Owner,
    Admin,
    Member,
    MemberView,
    MemberComment,
    MemberEdit,
    Guest,
    GuestView,
    GuestComment,
    GuestEdit,
    GuestManage,
    Agent,
    AgentOut,
    AgentOfGuestView,
    ShareLink,
}

impl Who {
    /// All principals, in the column order of the permission matrix.
    pub const ALL: &'static [Who] = &[
        Who::Anonymous,
        Who::Owner,
        Who::Admin,
        Who::Member,
        Who::MemberView,
        Who::MemberComment,
        Who::MemberEdit,
        Who::Guest,
        Who::GuestView,
        Who::GuestComment,
        Who::GuestEdit,
        Who::GuestManage,
        Who::Agent,
        Who::AgentOut,
        Who::AgentOfGuestView,
        Who::ShareLink,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Who::Anonymous => "anonymous",
            Who::Owner => "owner",
            Who::Admin => "admin",
            Who::Member => "member",
            Who::MemberView => "member_view",
            Who::MemberComment => "member_comment",
            Who::MemberEdit => "member_edit",
            Who::Guest => "guest",
            Who::GuestView => "guest_view",
            Who::GuestComment => "guest_comment",
            Who::GuestEdit => "guest_edit",
            Who::GuestManage => "guest_manage",
            Who::Agent => "agent",
            Who::AgentOut => "agent_out",
            Who::AgentOfGuestView => "agent_of_guest_view",
            Who::ShareLink => "share_link",
        }
    }

    /// The role in the tenant, for users. Agents and anonymous have none.
    pub fn tenant_role(self) -> Option<MemberRole> {
        match self {
            Who::Owner => Some(MemberRole::Owner),
            Who::Admin => Some(MemberRole::Admin),
            Who::Member | Who::MemberView | Who::MemberComment | Who::MemberEdit => {
                Some(MemberRole::Member)
            }
            Who::Guest | Who::GuestView | Who::GuestComment | Who::GuestEdit | Who::GuestManage => {
                Some(MemberRole::Guest)
            }
            _ => None,
        }
    }

    /// The content role of a member (decision 0003-1): `manage` for `member` (the default),
    /// the lower roles for `member_view`, `member_comment` and `member_edit`; none for the
    /// other principals.
    pub fn content_role(self) -> Option<Role> {
        match self {
            Who::Member => Some(Role::Manage),
            Who::MemberView => Some(Role::View),
            Who::MemberComment => Some(Role::Comment),
            Who::MemberEdit => Some(Role::Edit),
            _ => None,
        }
    }

    /// The direct grant on `Projects`, for the guests with a grant.
    pub fn grant_on_projects(self) -> Option<Role> {
        match self {
            Who::GuestView => Some(Role::View),
            Who::GuestComment => Some(Role::Comment),
            Who::GuestEdit => Some(Role::Edit),
            Who::GuestManage => Some(Role::Manage),
            _ => None,
        }
    }

    pub fn is_agent(self) -> bool {
        matches!(self, Who::Agent | Who::AgentOut | Who::AgentOfGuestView)
    }

    /// The bearer of a share-link token.
    pub fn is_share_link(self) -> bool {
        self == Who::ShareLink
    }

    pub fn is_user(self) -> bool {
        self.tenant_role().is_some()
    }

    /// The user an agent acts for.
    pub fn acts_for(self) -> Option<Who> {
        match self {
            Who::Agent | Who::AgentOut => Some(Who::Member),
            Who::AgentOfGuestView => Some(Who::GuestView),
            _ => None,
        }
    }

    /// The scope of an agent: the subtree (`None` = everything) and the highest role.
    pub fn scope(self) -> Option<(Option<Target>, Role)> {
        match self {
            Who::Agent => Some((Some(Target::Projects), Role::View)),
            Who::AgentOut => Some((Some(Target::Private), Role::Manage)),
            Who::AgentOfGuestView => Some((None, Role::Manage)),
            _ => None,
        }
    }

    /// The role of this principal on a node of the fixture tree, after inheritance and, for
    /// agents, after the scope (`user right ∩ agent scope`, design section 3.3).
    pub fn role_on(self, target: Target) -> Option<Role> {
        if let (Some(user), Some((subtree, max_role))) = (self.acts_for(), self.scope()) {
            let inside = subtree.is_none_or(|s| target.is_inside(s));
            if !inside {
                return None;
            }
            return user.role_on(target).map(|r| r.min(max_role));
        }
        match self {
            // Owners and admins: `manager@tenant#administer`; members: their content role
            // through `team@tenant` on the root (decision 0003-1).
            Who::Owner | Who::Admin => Some(Role::Manage),
            Who::Member | Who::MemberView | Who::MemberComment | Who::MemberEdit => {
                self.content_role()
            }
            // The `view` share link on `Projects`.
            Who::ShareLink if target.is_inside(Target::Projects) => Some(Role::View),
            Who::ShareLink => None,
            _ => match self.grant_on_projects() {
                Some(r) if target.is_inside(Target::Projects) => Some(r),
                _ => None,
            },
        }
    }

    /// Whether this principal has the `share` permission on a node of the fixture tree
    /// (`schema.zed`, decision 0001-1): `manage` always shares; `edit` shares where the effective
    /// `editors_can_share` is true; members share from the team root by the default tenant policy
    /// `members_can_share` (edit members as editors, view and comment members never; decision
    /// 0003-1); share links never. For agents: the user shares, and the scope has
    /// `max_role` `manage`, or `edit` where `editors_can_share` is true.
    pub fn can_share(self, target: Target) -> bool {
        if self.is_share_link() {
            return false;
        }
        let Some(role) = self.role_on(target) else {
            return false;
        };
        if let Some(user) = self.acts_for() {
            return user.can_share(target)
                && (role >= Role::Manage || (role == Role::Edit && target.editors_can_share()));
        }
        role >= Role::Manage || (role == Role::Edit && target.editors_can_share())
    }
}

/// The nodes of the fixture tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Root,
    Projects,
    Plan,
    Archive,
    Old,
    Private,
    /// The database node in `Projects` (step 2).
    Database,
    /// The web app node in `Projects` (step 3).
    App,
    /// The issue node in `Projects` (step 3).
    Issue,
    /// The project template folder in `Projects` (step 3).
    Template,
}

impl Target {
    /// The effective node setting `editors_can_share`. `Archive` sets it to false; every other
    /// node inherits the default, true.
    pub fn editors_can_share(self) -> bool {
        self != Target::Archive
    }

    /// Whether this node is `other` or below it.
    pub fn is_inside(self, other: Target) -> bool {
        self == other
            || match self {
                Target::Root => false,
                Target::Projects | Target::Private => other == Target::Root,
                Target::Plan
                | Target::Archive
                | Target::Old
                | Target::Database
                | Target::App
                | Target::Issue
                | Target::Template => other == Target::Projects || other == Target::Root,
            }
    }
}

/// How a principal authenticates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credentials {
    None,
    /// `Authorization: Bearer <token>`.
    Bearer(String),
    /// The web session cookie and the CSRF value (sent as cookie and `Ruum-Csrf` header).
    Session {
        session: String,
        csrf: String,
    },
}

/// One principal of a tenant fixture.
#[derive(Clone, Debug)]
pub struct Principal {
    pub who: Who,
    /// The user ID (users) or the agent ID (agents); nil for anonymous.
    pub id: Uuid,
    pub email: Option<String>,
    pub credentials: Credentials,
}

/// One tenant with its principals and resources. A and B have the same shape.
#[derive(Clone, Debug)]
pub struct TenantFixture {
    /// `alpha` or `bravo`. Names and emails start with it, so a leak is easy to see.
    pub prefix: &'static str,
    pub id: TenantId,
    pub name: String,
    pub principals: BTreeMap<Who, Principal>,
    pub root: NodeId,
    pub projects: NodeId,
    pub plan: NodeId,
    pub plan_name: String,
    pub archive: NodeId,
    pub old: NodeId,
    pub private: NodeId,
    pub v1: VersionId,
    pub v2: VersionId,
    pub tag: TagId,
    pub tag_name: String,
    /// The grant of `view` on `Projects` to `guest_view`.
    pub grant: GrantId,
    /// The `view` share link on `Projects`. Its bearer is the principal `share_link`.
    pub share_link: ShareLinkId,
    pub share_link_token: String,
    /// The database node in `Projects` (SQLite 3, made by `member` through `createDatabase`,
    /// card D-1), and its name.
    pub database: NodeId,
    pub database_name: String,
    /// A `read_personal` grant of `agent` on the database, given by `member` for 8 hours (card
    /// A-1; `write_live` needs an agent scope of `edit`).
    pub database_grant: Uuid,
    /// A pending request of `agent` on the database (card A-2): a grant request for
    /// `read_personal`, 60 minutes. Only `member` (the agent's user) decides it.
    pub database_approval: Uuid,
    /// The web app node in `Projects` and its name (step 3, planned).
    pub app: NodeId,
    pub app_name: String,
    /// The live deployment of the app.
    pub app_deployment: Uuid,
    /// A static site archive (ustar with `app.yaml` and `index.html`) in `Projects`, made by
    /// `member`: the source of the standard `createAppDeployment` request (card W-2s).
    pub site_archive: NodeId,
    pub site_archive_name: String,
    /// A grant of the app role `approver` to `guest_view`.
    pub app_role_grant: Uuid,
    /// A custom domain of the app (`<prefix>-plants.example.test`).
    pub app_domain: Uuid,
    /// A partner API key of the app's `orders` API.
    pub app_api_key: Uuid,
    /// A pending publish request of `agent` on the app.
    pub app_approval: Uuid,
    /// A trace of the app in the telemetry store (32 hex characters).
    pub app_trace: String,
    /// A `report` issue of the app in `Projects`, and its name.
    pub issue: NodeId,
    pub issue_name: String,
    /// The receipt token of the issue's reporter.
    pub issue_receipt_token: String,
    /// A project template folder in `Projects`, and its name.
    pub template: NodeId,
    pub template_name: String,
    /// The setup that made `<prefix>-from-kit` from the template.
    pub template_setup: Uuid,
    /// An upload of an empty file (no chunks), started by `member`, ready to complete.
    pub upload_empty: UploadId,
    /// An upload with one chunk ([`TenantFixture::chunk`]) not sent yet, started by `member`.
    pub upload_chunked: UploadId,
    pub chunk: Vec<u8>,
    pub invitation: InvitationId,
    pub invitation_token: String,
    pub invitation_email: String,
    /// The web session of `member`.
    pub session: Uuid,
    /// The WebDAV app password of `member` in this tenant, and its secret.
    pub app_password: AppPasswordId,
    pub app_password_secret: String,
    /// The API key of the narrow agent (`agent`).
    pub agent_key: AgentKeyId,
    pub agent_key_secret: String,
    /// A registered OAuth client (`registerOAuthClient`), with its redirect URI.
    pub oauth_client_id: String,
    pub oauth_redirect_uri: String,
    /// The signed consent request (`oauthAuthorize`) for that client.
    pub oauth_request: String,
}

impl TenantFixture {
    fn placeholder(prefix: &'static str) -> Self {
        let tenant = TenantId::new();
        // Emails are unique per world: the worlds of a Postgres run share one database, where a
        // verified email belongs to one user.
        let world = &Uuid::new_v4().simple().to_string()[..8];
        let principals = Who::ALL
            .iter()
            .map(|&who| {
                let id = if who == Who::Anonymous {
                    Uuid::nil()
                } else {
                    Uuid::now_v7()
                };
                let credentials = if who == Who::Anonymous {
                    Credentials::None
                } else {
                    // Not valid for any server: replaced by real credentials in `seed`.
                    Credentials::Bearer(format!("ruum_test_placeholder_{prefix}_{}", who.name()))
                };
                let email = who.is_user().then(|| {
                    format!(
                        "{prefix}-{}-{world}@example.test",
                        who.name().replace('_', "-")
                    )
                });
                (
                    who,
                    Principal {
                        who,
                        id,
                        email,
                        credentials,
                    },
                )
            })
            .collect();
        Self {
            prefix,
            id: tenant,
            name: format!("{prefix}-team"),
            principals,
            root: NodeId::new(),
            projects: NodeId::new(),
            plan: NodeId::new(),
            plan_name: format!("{prefix}-secret-plan.txt"),
            archive: NodeId::new(),
            old: NodeId::new(),
            private: NodeId::new(),
            v1: VersionId::new(),
            v2: VersionId::new(),
            tag: TagId::new(),
            tag_name: format!("{prefix}-tag"),
            grant: GrantId::new(),
            share_link: ShareLinkId::new(),
            share_link_token: format!("{prefix}-share-link-token-{}", Uuid::now_v7().simple()),
            database: NodeId::new(),
            database_name: format!("{prefix}-garden"),
            database_grant: Uuid::now_v7(),
            database_approval: Uuid::now_v7(),
            app: NodeId::new(),
            app_name: format!("{prefix}-planner"),
            app_deployment: Uuid::now_v7(),
            site_archive: NodeId::new(),
            site_archive_name: format!("{prefix}-site.tar"),
            app_role_grant: Uuid::now_v7(),
            app_domain: Uuid::now_v7(),
            app_api_key: Uuid::now_v7(),
            app_approval: Uuid::now_v7(),
            app_trace: Uuid::new_v4().simple().to_string(),
            issue: NodeId::new(),
            issue_name: format!("{prefix}-bug"),
            issue_receipt_token: format!("{prefix}-receipt-token-{}", Uuid::now_v7().simple()),
            template: NodeId::new(),
            template_name: format!("{prefix}-kit"),
            template_setup: Uuid::now_v7(),
            upload_empty: UploadId::new(),
            upload_chunked: UploadId::new(),
            chunk: format!("{prefix} chunk content").into_bytes(),
            invitation: InvitationId::new(),
            invitation_token: format!("{prefix}-invitation-token-{}", Uuid::now_v7().simple()),
            invitation_email: format!("{prefix}-invitee-{world}@example.test"),
            session: Uuid::now_v7(),
            app_password: AppPasswordId::new(),
            app_password_secret: format!("ruum_app_{prefix}{}", Uuid::now_v7().simple()),
            agent_key: AgentKeyId::new(),
            agent_key_secret: format!("ruum_agent_{prefix}{}", Uuid::now_v7().simple()),
            oauth_client_id: format!("{prefix}-oauth-client-{}", Uuid::now_v7().simple()),
            oauth_redirect_uri: "http://127.0.0.1:33418/callback".into(),
            oauth_request: format!("{prefix}-oauth-request-{}", Uuid::now_v7().simple()),
        }
    }

    /// A tenant fixture with placeholder IDs and credentials, for unit tests of the suites.
    #[cfg(test)]
    pub(crate) fn placeholder_for_tests(prefix: &'static str) -> Self {
        Self::placeholder(prefix)
    }

    pub fn principal(&self, who: Who) -> &Principal {
        &self.principals[&who]
    }

    pub fn user_id(&self, who: Who) -> UserId {
        assert!(who.is_user(), "{} is not a user", who.name());
        UserId(self.principal(who).id)
    }

    pub fn agent_id(&self, who: Who) -> AgentId {
        assert!(who.is_agent(), "{} is not an agent", who.name());
        AgentId(self.principal(who).id)
    }

    pub fn node(&self, t: Target) -> NodeId {
        match t {
            Target::Root => self.root,
            Target::Projects => self.projects,
            Target::Plan => self.plan,
            Target::Archive => self.archive,
            Target::Old => self.old,
            Target::Private => self.private,
            Target::Database => self.database,
            Target::App => self.app,
            Target::Issue => self.issue,
            Target::Template => self.template,
        }
    }

    /// The BLAKE3 hash of [`TenantFixture::chunk`], as lower case hex.
    pub fn chunk_hash(&self) -> String {
        blake3::hash(&self.chunk).to_hex().to_string()
    }

    /// Strings that belong only to this tenant: IDs, names, emails and secrets. A response to a
    /// principal of another tenant must not hold any of them.
    pub fn markers(&self) -> Vec<String> {
        let mut m = vec![
            self.id.to_string(),
            self.name.clone(),
            self.root.to_string(),
            self.projects.to_string(),
            self.plan.to_string(),
            self.plan_name.clone(),
            self.archive.to_string(),
            self.old.to_string(),
            format!("{}-old.txt", self.prefix),
            self.private.to_string(),
            self.v1.to_string(),
            self.v2.to_string(),
            self.tag.to_string(),
            self.tag_name.clone(),
            self.grant.to_string(),
            self.share_link.to_string(),
            self.share_link_token.clone(),
            self.database.to_string(),
            self.database_name.clone(),
            self.database_grant.to_string(),
            self.database_approval.to_string(),
            self.app.to_string(),
            self.app_name.clone(),
            self.app_deployment.to_string(),
            self.site_archive.to_string(),
            self.site_archive_name.clone(),
            self.app_role_grant.to_string(),
            self.app_domain.to_string(),
            self.app_api_key.to_string(),
            self.app_approval.to_string(),
            self.app_trace.clone(),
            self.issue.to_string(),
            self.issue_name.clone(),
            self.issue_receipt_token.clone(),
            self.template.to_string(),
            self.template_name.clone(),
            self.template_setup.to_string(),
            self.upload_empty.to_string(),
            self.upload_chunked.to_string(),
            self.invitation.to_string(),
            self.invitation_token.clone(),
            self.invitation_email.clone(),
            self.session.to_string(),
            self.app_password.to_string(),
            self.app_password_secret.clone(),
            self.agent_key.to_string(),
            self.agent_key_secret.clone(),
            self.oauth_request.clone(),
        ];
        for p in self.principals.values() {
            if p.who != Who::Anonymous {
                m.push(p.id.to_string());
            }
            if let Some(e) = &p.email {
                m.push(e.clone());
            }
            if let Credentials::Bearer(t) = &p.credentials {
                m.push(t.clone());
            }
        }
        m
    }

    /// Strings of the `Projects` subtree. A principal without `view` there must not see them.
    pub fn projects_markers(&self) -> Vec<String> {
        vec![
            self.projects.to_string(),
            self.plan.to_string(),
            self.plan_name.clone(),
            self.archive.to_string(),
            self.old.to_string(),
            format!("{}-old.txt", self.prefix),
            self.v1.to_string(),
            self.v2.to_string(),
            self.database.to_string(),
            self.database_name.clone(),
            self.app.to_string(),
            self.app_name.clone(),
            self.site_archive.to_string(),
            self.site_archive_name.clone(),
            self.issue.to_string(),
            self.issue_name.clone(),
            self.template.to_string(),
            self.template_name.clone(),
        ]
    }
}

/// The two tenants of a test.
#[derive(Clone, Debug)]
pub struct World {
    pub a: TenantFixture,
    pub b: TenantFixture,
    /// True when [`seed`] created the world in real services with real credentials.
    pub seeded: bool,
    /// The bridge user, made by [`seed_bridge`]: a member of B and a guest of A with `manage`
    /// on A's `Projects`. `None` until then.
    pub bridge: Option<Principal>,
}

/// Makes the fixture world in the services of `state`. See the module comment.
///
/// It creates both tenants through `Services`: users (through the sign-in path), the team
/// tenant, members and guests (through invitations), the tree, versions, the tag, grants, the
/// share link, uploads, the web session, the app password, the agents with their keys and
/// tokens, and the OAuth client and consent request. Every step runs on its own task: a step
/// whose area is not built yet (a `todo!`) keeps the placeholder ID of that resource, so the
/// suites report the operations of that area and not a broken seed. `seeded` is true when the
/// principals have real credentials.
pub async fn seed(state: &AppState) -> World {
    // One freshness floor for the whole seed, as one request: each step reads the writes of the
    // steps before it, also on a SpiceDB engine that other worlds share (decision 0002-4).
    ruum_core::authz::with_request_freshness(async {
        let mut a = TenantFixture::placeholder("alpha");
        let mut b = TenantFixture::placeholder("bravo");
        let ok_a = seed::tenant(state, &mut a).await;
        let ok_b = seed::tenant(state, &mut b).await;
        World {
            a,
            b,
            seeded: ok_a && ok_b,
            bridge: None,
        }
    })
    .await
}

/// Adds the bridge user to a seeded world ([`World::bridge`]): a user who is a member of tenant
/// B and a guest of tenant A with a `manage` grant on A's `Projects` (added as `edit`, then
/// changed). Its email starts with `bravo-`.
///
/// The authz engine lets the bridge act on A's `Projects` subtree, so when it sends A's IDs
/// with B's `Ruum-Tenant` header, only the tenant checks of the stores (the `tenant_id` filter
/// of every query) stand between it and A's data. The isolation suite uses it for that probe.
/// Returns false when a step fails.
pub async fn seed_bridge(state: &AppState, w: &mut World) -> bool {
    // One freshness floor, as in [`seed`].
    let bridge = ruum_core::authz::with_request_freshness(seed::bridge(state, w)).await;
    match bridge {
        Some(p) => {
            w.bridge = Some(p);
            true
        }
        None => false,
    }
}

mod seed {
    //! The steps of [`super::seed`].

    use super::{Credentials, TenantFixture, Who};
    use ruum_api::AppState;
    use ruum_core::actor::{Actor, Context};
    use ruum_core::agents::oauth::{AuthorizationRequest, ClientRegistration, REQUEST_SECONDS};
    use ruum_core::agents::{AgentScope, Budget, NewAgent};
    use ruum_core::authz::{Role, Scope};
    use ruum_core::blobs::{ChunkRef, Hash, NewUpload, SMALL_CHUNKING, UploadTarget};
    use ruum_core::identity::{AccessClaims, TokenType, VerifiedLogin, issue_user_token};
    use ruum_core::nodes::{NewNode, NodeKind, OnConflict};
    use ruum_core::sharing::{Grantee, NewShareLink, ShareLinkRole};
    use ruum_core::{NodeId, Services, TenantId};
    use std::future::Future;

    /// The issuer of the fixture logins.
    const ISSUER: &str = "https://idp.ruum.test";
    /// How long the fixture tokens live.
    const TOKEN_SECONDS: i64 = 3600;

    /// Runs one step on its own task. `None` when the step fails or its area is not built.
    async fn step<T, F>(label: &str, fut: F) -> Option<T>
    where
        F: Future<Output = ruum_core::Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        match tokio::spawn(ruum_core::authz::in_current_request(fut)).await {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                eprintln!("fixtures::seed: {label}: {e}");
                None
            }
            // A panic: the area of this step is not built yet (`todo!`).
            Err(_) => None,
        }
    }

    fn ctx(tenant: TenantId, actor: Actor) -> Context {
        Context {
            tenant,
            actor,
            scope: None,
            idempotency_key: None,
        }
    }

    fn hash(data: &[u8]) -> Hash {
        Hash(*blake3::hash(data).as_bytes())
    }

    /// Uploads `data` as a new file or a new version and returns the node and version IDs.
    async fn upload(
        s: &Services,
        c: &Context,
        target: UploadTarget,
        data: Vec<u8>,
    ) -> ruum_core::Result<(NodeId, ruum_core::VersionId)> {
        let h = hash(&data);
        let up = s
            .transfers
            .create_upload(
                c,
                NewUpload {
                    target,
                    content_type: Some("text/plain".into()),
                    chunking: SMALL_CHUNKING.id.to_string(),
                    size: data.len() as u64,
                    content_hash: h,
                    chunks: vec![ChunkRef {
                        hash: h,
                        size: data.len() as u32,
                    }],
                },
            )
            .await?;
        s.transfers
            .put_chunk(c, up.id, h, axum::body::Bytes::from(data))
            .await?;
        let w = s.transfers.complete_upload(c, up.id).await?;
        Ok((w.value.0.id, w.value.1.id))
    }

    /// Makes the bridge user of [`super::seed_bridge`].
    pub async fn bridge(state: &AppState, w: &super::World) -> Option<super::Principal> {
        let s = state.services.clone();
        let email = format!(
            "{}-bridge-{}@example.test",
            w.b.prefix,
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let login = VerifiedLogin {
            issuer: ISSUER.into(),
            subject: format!("bridge-{}", uuid::Uuid::now_v7()),
            email: Some(email.clone()),
            email_verified: true,
            name: Some(format!("{} bridge", w.b.prefix)),
            id_token: None,
        };
        let s2 = s.clone();
        let user = step("bridge user", async move {
            s2.identity.upsert_from_login(&login).await
        })
        .await?
        .id;
        // A member of B, a guest of A: both through invitations.
        for (t, role) in [
            (&w.b, ruum_core::tenants::MemberRole::Member),
            (&w.a, ruum_core::tenants::MemberRole::Guest),
        ] {
            let (s2, c, email) = (
                s.clone(),
                ctx(t.id, Actor::user(t.user_id(Who::Owner))),
                email.clone(),
            );
            step("bridge joins", async move {
                let (_inv, token) = s2.tenants.invite(&c, email, role, None).await?;
                s2.tenants
                    .accept_invitation(&Actor::user(user), &token)
                    .await
            })
            .await?;
        }
        let (s2, c, node) = (
            s.clone(),
            ctx(w.a.id, Actor::user(w.a.user_id(Who::Owner))),
            w.a.projects,
        );
        // `edit` first, then `manage`: the `grant.updated` entry names the bridge as grantee,
        // so A's feed has an entry for the bridge that B's feed must never show.
        step("bridge grant", async move {
            let g = s2
                .sharing
                .add_grant(&c, node, Grantee::User { id: user }, Role::Edit)
                .await?;
            s2.sharing
                .update_grant(&c, node, g.value.id, Role::Manage)
                .await
        })
        .await?;
        let token = issue_user_token(s.tokens.as_ref(), user, TOKEN_SECONDS).ok()?;
        Some(super::Principal {
            who: Who::Member,
            id: user.0,
            email: Some(email),
            credentials: Credentials::Bearer(token),
        })
    }

    /// Seeds one tenant. Returns true when the principals got real credentials.
    pub async fn tenant(state: &AppState, t: &mut TenantFixture) -> bool {
        let s = state.services.clone();

        // ------------------------------------------------------------ users
        for who in Who::ALL.iter().copied().filter(|w| w.is_user()) {
            let email = t.principal(who).email.clone();
            let login = VerifiedLogin {
                issuer: ISSUER.into(),
                subject: format!("{}-{}-{}", t.prefix, who.name(), uuid::Uuid::now_v7()),
                email: email.clone(),
                email_verified: true,
                name: Some(format!("{} {}", t.prefix, who.name())),
                id_token: None,
            };
            let s2 = s.clone();
            let Some(user) = step("user", async move {
                s2.identity.upsert_from_login(&login).await
            })
            .await
            else {
                return false;
            };
            let p = t.principals.get_mut(&who).expect("every principal");
            p.id = user.id.0;
        }
        let owner = t.user_id(Who::Owner);

        // ------------------------------------------------------------ the team tenant
        let (s2, name) = (s.clone(), t.name.clone());
        let Some(tenant) = step("team tenant", async move {
            s2.tenants.create_team(&Actor::user(owner), name).await
        })
        .await
        else {
            return false;
        };
        t.id = tenant.id;
        t.root = tenant.root_node_id;
        let tid = t.id;
        let owner_ctx = ctx(tid, Actor::user(owner));

        // Members and guests join through invitations.
        for who in Who::ALL
            .iter()
            .copied()
            .filter(|w| w.is_user() && *w != Who::Owner)
        {
            let role = who.tenant_role().expect("a user");
            let content_role = who.content_role();
            let (s2, c, email) = (
                s.clone(),
                owner_ctx.clone(),
                t.principal(who).email.clone().unwrap_or_default(),
            );
            let user = t.user_id(who);
            let joined = step("member", async move {
                let (_inv, token) = s2
                    .tenants
                    .invite_as(&c, email, role, content_role, None)
                    .await?;
                s2.tenants
                    .accept_invitation(&Actor::user(user), &token)
                    .await
            })
            .await;
            if joined.is_none() {
                return false;
            }
        }

        // The open invitation.
        let (s2, c, email) = (s.clone(), owner_ctx.clone(), t.invitation_email.clone());
        if let Some((inv, token)) = step("invitation", async move {
            s2.tenants
                .invite(&c, email, ruum_core::tenants::MemberRole::Member, None)
                .await
        })
        .await
        {
            t.invitation = inv.id;
            t.invitation_token = token;
        }

        // ------------------------------------------------------------ credentials of users
        for who in Who::ALL.iter().copied().filter(|w| w.is_user()) {
            let user = t.user_id(who);
            let token = match issue_user_token(s.tokens.as_ref(), user, TOKEN_SECONDS) {
                Ok(t) => t,
                Err(_) => return false,
            };
            t.principals.get_mut(&who).expect("a user").credentials = Credentials::Bearer(token);
        }
        // The member signs in to the web UI: a session and the CSRF value.
        let member = t.user_id(Who::Member);
        let s2 = s.clone();
        if let Some((session, secret)) = step("session", async move {
            s2.identity
                .create_session(member, Some("ruum-testkit".into()))
                .await
        })
        .await
        {
            t.session = session.id;
            t.principals
                .get_mut(&Who::Member)
                .expect("member")
                .credentials = Credentials::Session {
                csrf: ruum_core::identity::csrf_token(&secret),
                session: secret,
            };
        }
        let member_ctx = ctx(tid, Actor::user(member));

        // The app password of the member.
        let (s2, c, name) = (
            s.clone(),
            member_ctx.clone(),
            format!("{}-webdav", t.prefix),
        );
        if let Some((pw, secret)) = step("app password", async move {
            s2.identity.create_app_password(&c, name, None).await
        })
        .await
        {
            t.app_password = pw.id;
            t.app_password_secret = secret;
        }

        // ------------------------------------------------------------ the tree
        let folder = |parent: NodeId, name: &str| NewNode {
            parent_id: parent,
            name: name.to_string(),
            kind: NodeKind::Folder,
            node_type: None,
            link: None,
            on_conflict: OnConflict::Fail,
        };
        let (s2, c, req) = (s.clone(), owner_ctx.clone(), folder(t.root, "Projects"));
        if let Some(n) = step("Projects", async move { s2.nodes.create(&c, req).await }).await {
            t.projects = n.value.id;
        }
        let (s2, c, req) = (s.clone(), owner_ctx.clone(), folder(t.root, "Private"));
        if let Some(n) = step("Private", async move { s2.nodes.create(&c, req).await }).await {
            t.private = n.value.id;
        }
        let (s2, c, req) = (s.clone(), owner_ctx.clone(), folder(t.projects, "Archive"));
        if let Some(n) = step("Archive", async move { s2.nodes.create(&c, req).await }).await {
            t.archive = n.value.id;
        }
        // Editors keep `edit` on Archive but lose `share` (`updateNodeSharing`).
        let (s2, c, archive) = (s.clone(), owner_ctx.clone(), t.archive);
        step("Archive editors_can_share", async move {
            s2.sharing
                .update_node_sharing(&c, archive, Some(false), None)
                .await
        })
        .await;

        // The database `<prefix>-garden`, made by `member` through the database provider, and
        // ready before the suites start.
        let (s2, c, req) = (
            s.clone(),
            member_ctx.clone(),
            ruum_core::databases::CreateDatabase {
                parent_id: t.projects,
                name: t.database_name.clone(),
                manifest: serde_json::json!({
                    "engine": "sqlite", "version": "3", "plan": "shared-small",
                }),
                source: None,
                on_conflict: OnConflict::Fail,
            },
        );
        if let Some(w) = step(
            "database",
            async move { s2.databases.create(&c, req).await },
        )
        .await
        {
            t.database = w.value.node.id;
            let (s2, c, id) = (s.clone(), member_ctx.clone(), t.database);
            step("database ready", async move {
                for _ in 0..200 {
                    let db = s2.databases.get(&c, id).await?;
                    if db.status.state == ruum_core::providers::ProviderState::Ready {
                        return Ok(());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                Err(ruum_core::Error::Internal(
                    "the fixture database is not ready after 5 s".into(),
                ))
            })
            .await;
            // The table `plants` (wave 4 gate G-1): the standard requests of `queryDatabase`,
            // `suggestColumnClasses` and `confirmColumnClasses` address it. Made as the
            // database's owner kind through the engine of the services; no personal data.
            let (s2, tenant, id) = (s.clone(), t.id, t.database);
            step("garden tables", async move {
                use ruum_core::databases::engine::{
                    RoleKind, RoleRef, StatementMode, StatementRequest,
                };
                let row = s2
                    .databases
                    .deps()
                    .store
                    .get(tenant, id)
                    .await?
                    .ok_or_else(|| ruum_core::Error::Internal("no databases row".into()))?;
                let engine = s2
                    .database_engines
                    .get(row.engine)
                    .cloned()
                    .ok_or_else(|| {
                        ruum_core::Error::Internal("no engine for the fixture database".into())
                    })?;
                let key = s2.tenant_keys.engine_key(tenant, row.key_node_id).await?;
                let role = RoleRef::new(row.database_ref(), RoleKind::Owner)
                    .map_err(|e| ruum_core::Error::Internal(e.to_string()))?;
                for sql in [
                    "create table plants (id integer primary key, latin_name text not null, \
                     common_name text)",
                    "insert into plants (id, latin_name, common_name) values \
                     (1, 'Rosa canina', 'dog rose'), (2, 'Bellis perennis', 'daisy')",
                ] {
                    engine
                        .execute(
                            &role,
                            &key,
                            &StatementRequest {
                                sql: sql.into(),
                                params: vec![],
                                mode: StatementMode::Write,
                                timeout: std::time::Duration::from_secs(30),
                                max_rows: 10,
                                max_bytes: None,
                            },
                        )
                        .await
                        .map_err(|e| ruum_core::Error::Internal(e.to_string()))?;
                }
                Ok(())
            })
            .await;
        }

        // The plan with two versions (by two actors, so they do not coalesce).
        let (s2, c, target, data) = (
            s.clone(),
            owner_ctx.clone(),
            UploadTarget::NewFile {
                parent_id: t.projects,
                name: t.plan_name.clone(),
                on_conflict: OnConflict::Fail,
            },
            format!("{} plan, first draft\n", t.prefix).into_bytes(),
        );
        if let Some((node, v1)) = step(
            "plan v1",
            async move { upload(&s2, &c, target, data).await },
        )
        .await
        {
            t.plan = node;
            t.v1 = v1;
            let (s2, c, target, data) = (
                s.clone(),
                member_ctx.clone(),
                UploadTarget::NewVersion {
                    node_id: node,
                    base_version: Some(v1),
                    coalesce: true,
                },
                format!("{} plan, second draft\n", t.prefix).into_bytes(),
            );
            if let Some((_, v2)) = step(
                "plan v2",
                async move { upload(&s2, &c, target, data).await },
            )
            .await
            {
                t.v2 = v2;
            }
        } else {
            // The blobs area is not built yet: the plan is an empty file without versions, so
            // the node, tag and search operations still have their file (v1 and v2 keep their
            // placeholder IDs).
            let (s2, c, req) = (
                s.clone(),
                owner_ctx.clone(),
                NewNode {
                    parent_id: t.projects,
                    name: t.plan_name.clone(),
                    kind: NodeKind::File,
                    node_type: Some("text/plain".into()),
                    link: None,
                    on_conflict: OnConflict::Fail,
                },
            );
            if let Some(n) = step(
                "plan (empty)",
                async move { s2.nodes.create(&c, req).await },
            )
            .await
            {
                t.plan = n.value.id;
            }
        }
        // The site archive of the standard `createAppDeployment` request (card W-2s).
        if let Ok(site) = ruum_core::apps::archive::pack_tar(&[
            ("app.yaml".to_string(), &b"runtime: static\n"[..]),
            (
                "index.html".to_string(),
                &b"<!doctype html><h1>Plants</h1>\n"[..],
            ),
        ]) {
            let (s2, c, target) = (
                s.clone(),
                member_ctx.clone(),
                UploadTarget::NewFile {
                    parent_id: t.projects,
                    name: t.site_archive_name.clone(),
                    on_conflict: OnConflict::Fail,
                },
            );
            if let Some((node, _)) = step("site archive", async move {
                upload(&s2, &c, target, site).await
            })
            .await
            {
                t.site_archive = node;
            }
        }

        // The old file, in the trash.
        let (s2, c, req) = (
            s.clone(),
            owner_ctx.clone(),
            NewNode {
                parent_id: t.projects,
                name: format!("{}-old.txt", t.prefix),
                kind: NodeKind::File,
                node_type: Some("text/plain".into()),
                link: None,
                on_conflict: OnConflict::Fail,
            },
        );
        if let Some(old) = step("old", async move {
            let n = s2.nodes.create(&c, req).await?;
            s2.nodes.trash(&c, n.value.id, None).await
        })
        .await
        {
            t.old = old.value.id;
        }

        // The web app W `<prefix>-planner`, made by `member` through `createApp` (card W-1),
        // the platform admin's approval of public apps and custom domains for the tenant, and
        // W's live deployment through the store (its manifest of docs/STEP3.md "Fixtures").
        let (s2, c, req) = (
            s.clone(),
            member_ctx.clone(),
            ruum_core::apps::CreateApp {
                parent_id: t.projects,
                name: t.app_name.clone(),
                settings: None,
                on_conflict: OnConflict::Fail,
            },
        );
        if let Some(w) = step("web app", async move { s2.apps.create(&c, req).await }).await {
            t.app = w.value.node.id;
        }
        let s2 = s.clone();
        step("app approvals", async move {
            s2.apps
                .set_tenant_approval(
                    tid,
                    ruum_core::apps::store::ApprovalChange {
                        public_apps: Some(true),
                        custom_domains: Some(true),
                        by: "fixtures@ruum.test".into(),
                    },
                )
                .await
        })
        .await;
        let (s2, c) = (s.clone(), member_ctx.clone());
        let (app, deployment, database, plan, v1) =
            (t.app, t.app_deployment, t.database, t.plan, t.v1);
        if let Some(d) = step("app live deployment", async move {
            use ruum_core::apps::store::{
                Archive, DeploymentSource, DeploymentState, Environment, NewDeployment,
            };
            let manifest = serde_json::json!({
                "runtime": "node22",
                "start": "node server.js",
                "bindings": { "db": { "node": format!("node:{database}"), "role": "edit" } },
                "secrets": {
                    "STRIPE_KEY": { "entered": true },
                    "SESSION_KEY": { "generate": "random-32-bytes" },
                },
                "roles": {
                    "member": { "description": "Can use the planner", "default": true },
                    "approver": { "description": "Can approve orders" },
                },
                "apis": { "orders": {
                    "visibility": "external",
                    "protocol": "http",
                    "spec": "openapi/orders.yaml",
                    "scopes": ["orders.read", "orders.write"],
                    "preview_scopes": ["orders.read"],
                } },
            });
            let check = ruum_core::apps::manifest::validate(
                &manifest,
                &ruum_core::apps::plan::AppPlanLimits::of(ruum_core::tenants::Plan::Team),
            );
            if !check.valid {
                return Err(ruum_core::Error::Internal(format!(
                    "the fixture manifest is not valid: {:?}",
                    check.problems
                )));
            }
            let archive = format!("{deployment} archive");
            s2.apps
                .store()
                .insert_deployment(
                    &c,
                    NewDeployment {
                        id: deployment,
                        app_node_id: app,
                        environment: Environment::Live,
                        state: DeploymentState::Live,
                        short_id: ruum_core::apps::slugs::short_id(),
                        label: Some("first release".into()),
                        draft: None,
                        source: DeploymentSource {
                            kind: "file".into(),
                            node_id: Some(plan),
                            version_id: Some(v1),
                            files: None,
                        },
                        archive: Archive {
                            content_hash: *blake3::hash(archive.as_bytes()).as_bytes(),
                            size: archive.len() as i64,
                            chunks: Vec::new(),
                        },
                        manifest: Some(manifest),
                        stages: serde_json::json!([]),
                        published_from: None,
                        new_chunks: Vec::new(),
                        expires_at: None,
                    },
                )
                .await
        })
        .await
        {
            t.app_deployment = d.id;
        }

        // The tag T, made by the member, on the plan.
        let (s2, c, name, plan) = (s.clone(), member_ctx.clone(), t.tag_name.clone(), t.plan);
        if let Some(tag) = step("tag", async move {
            let tag = s2.tags.create(&c, name, None).await?;
            s2.tags.attach(&c, plan, tag.value.id).await?;
            Ok(tag)
        })
        .await
        {
            t.tag = tag.value.id;
        }

        // The grants of the guests on Projects.
        for who in [
            Who::GuestView,
            Who::GuestComment,
            Who::GuestEdit,
            Who::GuestManage,
        ] {
            let role = who.grant_on_projects().expect("a guest with a grant");
            let (s2, c, node, user) = (s.clone(), owner_ctx.clone(), t.projects, t.user_id(who));
            if let Some(g) = step("grant", async move {
                s2.sharing
                    .add_grant(&c, node, Grantee::User { id: user }, role)
                    .await
            })
            .await
                && who == Who::GuestView
            {
                t.grant = g.value.id;
            }
        }

        // The view share link on Projects, and the token of its bearer.
        let (s2, c, node) = (s.clone(), owner_ctx.clone(), t.projects);
        if let Some(link) = step("share link", async move {
            s2.sharing
                .create_share_link(
                    &c,
                    node,
                    NewShareLink {
                        role: ShareLinkRole::View,
                        expires_at: None,
                        password: None,
                    },
                )
                .await
        })
        .await
        {
            t.share_link = link.value.id;
            if let Some(token) = link.value.token.clone() {
                t.share_link_token = token;
            }
        }
        let mut claims = AccessClaims::new(
            s.tokens.as_ref(),
            TokenType::ShareLink,
            t.share_link.0,
            TOKEN_SECONDS,
        );
        claims.tid = Some(tid);
        claims.share_link = Some(t.share_link);
        claims.scope = Some(Scope {
            subtree: Some(t.projects),
            max_role: Role::View,
            expires_at: None,
        });
        if let Ok(token) = s.tokens.issue(&claims) {
            let p = t.principals.get_mut(&Who::ShareLink).expect("share link");
            p.id = t.share_link.0;
            p.credentials = Credentials::Bearer(token);
        }

        // The uploads of the member: an empty file ready to complete, and one with a chunk to send.
        let empty = hash(b"");
        let (s2, c, parent) = (s.clone(), member_ctx.clone(), t.projects);
        if let Some(up) = step("empty upload", async move {
            s2.transfers
                .create_upload(
                    &c,
                    NewUpload {
                        target: UploadTarget::NewFile {
                            parent_id: parent,
                            name: "empty.txt".into(),
                            on_conflict: OnConflict::Fail,
                        },
                        content_type: Some("text/plain".into()),
                        chunking: SMALL_CHUNKING.id.to_string(),
                        size: 0,
                        content_hash: empty,
                        chunks: vec![],
                    },
                )
                .await
        })
        .await
        {
            t.upload_empty = up.id;
        }
        let chunk = hash(&t.chunk);
        let size = t.chunk.len() as u32;
        let (s2, c, parent) = (s.clone(), member_ctx.clone(), t.projects);
        if let Some(up) = step("chunked upload", async move {
            s2.transfers
                .create_upload(
                    &c,
                    NewUpload {
                        target: UploadTarget::NewFile {
                            parent_id: parent,
                            name: "chunked.txt".into(),
                            on_conflict: OnConflict::Fail,
                        },
                        content_type: Some("text/plain".into()),
                        chunking: SMALL_CHUNKING.id.to_string(),
                        size: u64::from(size),
                        content_hash: chunk,
                        chunks: vec![ChunkRef { hash: chunk, size }],
                    },
                )
                .await
        })
        .await
        {
            t.upload_chunked = up.id;
        }

        // ------------------------------------------------------------ agents
        let mut agents_ok = true;
        for who in [Who::Agent, Who::AgentOut, Who::AgentOfGuestView] {
            let user = who.acts_for().expect("an agent");
            let (subtree, max_role) = who.scope().expect("an agent");
            let scope = AgentScope {
                subtree: subtree.map(|s| t.node(s)),
                max_role,
                expires_at: None,
                budget: Budget::default(),
            };
            let (s2, c) = (s.clone(), ctx(tid, Actor::user(t.user_id(user))));
            let name = format!("{}-{}", t.prefix, who.name().replace('_', "-"));
            let made = step("agent", async move {
                let agent = s2
                    .agents
                    .register(
                        &c,
                        NewAgent {
                            name,
                            description: None,
                            scope,
                        },
                    )
                    .await?;
                let (key, secret) = s2
                    .agents
                    .create_key(&c, agent.id, Some("fixture key".into()), None)
                    .await?;
                let token = s2.agents.exchange_key(&secret).await?;
                Ok((agent, key, secret, token))
            })
            .await;
            let Some((agent, key, secret, token)) = made else {
                agents_ok = false;
                continue;
            };
            let p = t.principals.get_mut(&who).expect("an agent");
            p.id = agent.id.0;
            p.credentials = Credentials::Bearer(token.access_token);
            if who == Who::Agent {
                t.agent_key = key.id;
                t.agent_key_secret = secret;
            }
        }

        // The grant of `agent` on the database, given by `member` (card A-1): `read_personal`
        // for the longest allowed time. Not `write_live`: that kind needs an agent scope of
        // `edit`, and `agent` has `view`. The standard query is a read and the agents' query
        // rules of Q-1 do not read grants, so it changes no other operation's result.
        let (s2, c, db, agent) = (
            s.clone(),
            member_ctx.clone(),
            t.database,
            t.agent_id(Who::Agent),
        );
        if let Some(g) = step("database grant", async move {
            s2.databases
                .create_grant(
                    &c,
                    &*s2.agents,
                    db,
                    ruum_core::databases::access::grants::CreateGrant {
                        kind: ruum_core::databases::access::grants::GrantKind::ReadPersonal,
                        agent_id: agent,
                        duration_minutes: i64::from(
                            ruum_core::databases::access::grants::READ_PERSONAL_MAX_MINUTES,
                        ),
                        reason: Some("fixture grant".into()),
                    },
                )
                .await
        })
        .await
        {
            t.database_grant = g.value.id;
        } else {
            agents_ok = false;
        }

        // The pending request of `agent` on the database (card A-2): a grant request for
        // `read_personal`, 60 minutes, made as the agent, so `getDatabaseApproval` and
        // `decideDatabaseApproval` have a real row without the statement path of the query API.
        let (s2, db, agent, member) = (
            s.clone(),
            t.database,
            t.agent_id(Who::Agent),
            t.user_id(Who::Member),
        );
        if let Some((a, _)) = step("database approval", async move {
            let scope = s2.agents.authenticate(tid, agent).await?.scope.as_scope();
            let c = Context {
                tenant: tid,
                actor: Actor {
                    actor: ruum_core::actor::PrincipalRef {
                        kind: ruum_core::actor::PrincipalKind::Agent,
                        id: agent.0.into(),
                    },
                    on_behalf_of: Some(member),
                },
                scope: Some(scope),
                idempotency_key: None,
            };
            s2.databases
                .request_grant(
                    &c,
                    &*s2.agents,
                    db,
                    ruum_core::databases::access::approvals::RequestGrant {
                        kind: ruum_core::databases::access::grants::GrantKind::ReadPersonal,
                        duration_minutes: 60,
                        reason: Some("fixture request".into()),
                    },
                )
                .await
        })
        .await
        {
            t.database_approval = a.id;
        } else {
            agents_ok = false;
        }

        // ------------------------------------------------------------ OAuth
        let (s2, name, redirect) = (
            s.clone(),
            format!("{}-mcp-client", t.prefix),
            t.oauth_redirect_uri.clone(),
        );
        if let Some(client) = step("OAuth client", async move {
            s2.oauth
                .register_client(ClientRegistration {
                    client_name: name,
                    redirect_uris: vec![redirect],
                    grant_types: vec![],
                    response_types: vec![],
                    token_endpoint_auth_method: None,
                    client_uri: None,
                    logo_uri: None,
                    software_id: None,
                    software_version: None,
                })
                .await
                .map_err(|e| ruum_core::Error::Internal(e.error_description))
        })
        .await
        {
            t.oauth_client_id = client.client_id.clone();
            let s2 = s.clone();
            let req = AuthorizationRequest {
                client_id: client.client_id,
                redirect_uri: t.oauth_redirect_uri.clone(),
                code_challenge: crate::requests::CODE_CHALLENGE.into(),
                state: Some("test-state".into()),
                resource: s.tokens.audience().to_string(),
                expires_at: ruum_core::now()
                    + std::time::Duration::from_secs(REQUEST_SECONDS as u64),
            };
            if let Some(request) = step("OAuth request", async move {
                s2.oauth.start_authorization(req).await
            })
            .await
            {
                t.oauth_request = request;
            }
        }
        agents_ok
    }
}

/// The application state of a new world, on the backend of `RUUM_SUITE_BACKEND`, comes from
/// [`Backend::app_state`] (a shared database per run) or [`Backend::world`] (a database per
/// world); see [`crate::backend`].
pub use crate::backend::Backend;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_rights_are_the_intersection() {
        // The narrow agent: member has edit, scope gives view inside Projects only.
        assert_eq!(Who::Agent.role_on(Target::Plan), Some(Role::View));
        assert_eq!(Who::Agent.role_on(Target::Root), None);
        assert_eq!(Who::Agent.role_on(Target::Private), None);
        // Scope Private with manage: the member manages there through the team root.
        assert_eq!(Who::AgentOut.role_on(Target::Private), Some(Role::Manage));
        assert!(Who::AgentOut.can_share(Target::Private));
        assert_eq!(Who::AgentOut.role_on(Target::Plan), None);
        // A wide scope never lifts the user's right.
        assert_eq!(
            Who::AgentOfGuestView.role_on(Target::Plan),
            Some(Role::View)
        );
        assert_eq!(Who::AgentOfGuestView.role_on(Target::Private), None);
    }

    #[test]
    fn grants_inherit_down_only() {
        assert_eq!(Who::GuestEdit.role_on(Target::Archive), Some(Role::Edit));
        assert_eq!(Who::GuestEdit.role_on(Target::Root), None);
        assert_eq!(Who::Guest.role_on(Target::Plan), None);
        assert_eq!(Who::Member.role_on(Target::Root), Some(Role::Manage));
        assert_eq!(Who::ShareLink.role_on(Target::Plan), Some(Role::View));
        assert_eq!(Who::ShareLink.role_on(Target::Private), None);
        assert_eq!(Who::Admin.role_on(Target::Private), Some(Role::Manage));
    }

    #[test]
    fn share_is_a_permission() {
        // manage always shares; edit shares where editors_can_share is true.
        assert!(Who::GuestManage.can_share(Target::Archive));
        assert!(Who::GuestEdit.can_share(Target::Projects));
        assert!(!Who::GuestEdit.can_share(Target::Archive));
        assert!(!Who::GuestComment.can_share(Target::Projects));
        assert!(Who::Member.can_share(Target::Projects));
        // Share links never share; the narrow agent (max_role view) never shares.
        assert!(!Who::ShareLink.can_share(Target::Projects));
        assert!(!Who::Agent.can_share(Target::Projects));
        // A wide scope never lifts the user's right.
        assert!(!Who::AgentOfGuestView.can_share(Target::Projects));
    }

    #[test]
    fn content_roles_limit_members() {
        // Decision 0003-1: a member's content role is its role on the whole team tree.
        assert_eq!(Who::MemberView.role_on(Target::Private), Some(Role::View));
        assert_eq!(
            Who::MemberComment.role_on(Target::Plan),
            Some(Role::Comment)
        );
        assert_eq!(Who::MemberEdit.role_on(Target::Root), Some(Role::Edit));
        assert!(!Who::MemberView.can_share(Target::Projects));
        assert!(!Who::MemberComment.can_share(Target::Projects));
        // Edit members share as editors: not where editors_can_share is false.
        assert!(Who::MemberEdit.can_share(Target::Projects));
        assert!(!Who::MemberEdit.can_share(Target::Archive));
        assert_eq!(Who::MemberEdit.tenant_role(), Some(MemberRole::Member));
    }
}
