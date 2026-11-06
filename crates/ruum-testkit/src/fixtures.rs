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
//! │   └── <prefix>-old.txt  (Y)  in the trash (its old parent is Projects)
//! └── Private/             (G)  no grants
//! ```
//!
//! | Principal | What it is |
//! |---|---|
//! | `owner`, `admin`, `member` | tenant roles; owners and admins have `manage` (and so `share`) on the root; members have `manage` from the team root (`node:<root>#team@tenant`), and `share` while the tenant policy `members_can_share` is true (the default) |
//! | `guest` | a guest without grants |
//! | `guest_view` ... `guest_manage` | guests with one grant on `Projects`; `guest_edit` shares where `editors_can_share` is true (not in `Archive`), `guest_manage` everywhere below `Projects` |
//! | `agent` | the agent of `member`: scope `Projects`, `max_role: view` (the narrow agent) |
//! | `agent_out` | the agent of `member`: scope `Private`, `max_role: manage` |
//! | `agent_of_guest_view` | the agent of `guest_view`: scope everything, `max_role: manage` |
//! | `share_link` | the bearer of the `view` share link on `Projects` (the token of `resolveShareLink`) |
//! | `anonymous` | no credentials |
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
            Who::Member => Some(MemberRole::Member),
            Who::Guest | Who::GuestView | Who::GuestComment | Who::GuestEdit | Who::GuestManage => {
                Some(MemberRole::Guest)
            }
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
            // Owners and admins: `manager@tenant#administer`; members: `team@tenant` on the root.
            Who::Owner | Who::Admin | Who::Member => Some(Role::Manage),
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
    /// `members_can_share`; share links never. For agents: the user shares, and the scope has
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
                Target::Plan | Target::Archive | Target::Old => {
                    other == Target::Projects || other == Target::Root
                }
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
                let email = who
                    .is_user()
                    .then(|| format!("{prefix}-{}@example.test", who.name().replace('_', "-")));
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
            upload_empty: UploadId::new(),
            upload_chunked: UploadId::new(),
            chunk: format!("{prefix} chunk content").into_bytes(),
            invitation: InvitationId::new(),
            invitation_token: format!("{prefix}-invitation-token-{}", Uuid::now_v7().simple()),
            invitation_email: format!("{prefix}-invitee@example.test"),
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
}

/// Makes the fixture world in the services of `state`. See the module comment.
///
/// It is async because the real version writes through `Services`.
pub async fn seed(_state: &AppState) -> World {
    // TODO(identity, tenants, nodes, sharing areas): create both tenants through
    // `state.services` and replace the placeholder credentials with real ones. Then set
    // `seeded: true`.
    World {
        a: TenantFixture::placeholder("alpha"),
        b: TenantFixture::placeholder("bravo"),
        seeded: false,
    }
}

/// The application state the suites run against: in-memory services.
///
/// A Postgres backend (`#[sqlx::test]` database) comes when the Postgres implementations of the
/// core traits exist; then this function picks it when `DATABASE_URL` is set.
pub fn app_state() -> AppState {
    AppState::in_memory()
}

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
}
