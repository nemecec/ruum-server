//! Shared helpers: a team tenant in [`Services::in_memory`] with users of given roles.

use ruum_core::actor::{Actor, Context};
use ruum_core::identity::VerifiedLogin;
use ruum_core::nodes::{LinkKind, LinkSpec, NewNode, NodeKind, OnConflict};
use ruum_core::services::MemoryHandles;
use ruum_core::tenants::MemberRole;
use ruum_core::{NodeId, Services, TenantId, UserId};
use std::future::Future;

/// Runs a future to the end on a fresh single-thread runtime (proptest bodies are sync).
pub fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(f)
}

/// A team tenant made through the services, like the sign-in and invitation paths do.
pub struct Team {
    pub s: Services,
    pub h: MemoryHandles,
    pub tenant: TenantId,
    pub root: NodeId,
    /// The users; index 0 is the owner who made the tenant.
    pub users: Vec<(UserId, MemberRole)>,
}

impl Team {
    /// A team whose owner is user 0, then one user per entry of `others`, who join through an
    /// invitation with that role.
    pub async fn new(others: &[MemberRole]) -> Team {
        let (s, h) = Services::in_memory_with_handles();
        let mut users = Vec::new();
        for i in 0..=others.len() {
            let login = VerifiedLogin {
                issuer: "https://idp.ruum.test".into(),
                subject: format!("u{i}-{}", uuid::Uuid::now_v7()),
                email: Some(format!(
                    "u{i}-{}@example.test",
                    uuid::Uuid::now_v7().simple()
                )),
                email_verified: true,
                name: Some(format!("user {i}")),
                id_token: None,
            };
            let user = s.identity.upsert_from_login(&login).await.expect("user");
            users.push((user.id, MemberRole::Owner));
        }
        let owner = users[0].0;
        let tenant = s
            .tenants
            .create_team(&Actor::user(owner), "props".into())
            .await
            .expect("team");
        let owner_ctx = user_ctx(tenant.id, owner);
        for (i, role) in others.iter().enumerate() {
            let (user, _) = users[i + 1];
            let email = format!("join{i}-{}@example.test", uuid::Uuid::now_v7().simple());
            let (_, token) = s
                .tenants
                .invite(&owner_ctx, email, *role, None)
                .await
                .expect("invite");
            s.tenants
                .accept_invitation(&Actor::user(user), &token)
                .await
                .expect("accept");
            users[i + 1].1 = *role;
        }
        Team {
            s,
            h,
            tenant: tenant.id,
            root: tenant.root_node_id,
            users,
        }
    }

    pub fn owner(&self) -> Context {
        user_ctx(self.tenant, self.users[0].0)
    }

    pub fn ctx_of(&self, user: usize) -> Context {
        user_ctx(self.tenant, self.users[user].0)
    }
}

pub fn user_ctx(tenant: TenantId, user: UserId) -> Context {
    Context {
        tenant,
        actor: Actor::user(user),
        scope: None,
        idempotency_key: None,
    }
}

/// A request to make a node of `kind` (a link when `link` is set).
pub fn new_node(
    parent: NodeId,
    name: &str,
    kind: NodeKind,
    link: Option<(LinkKind, NodeId)>,
) -> NewNode {
    NewNode {
        parent_id: parent,
        name: name.to_string(),
        kind: if link.is_some() { NodeKind::Link } else { kind },
        node_type: None,
        link: link.map(|(kind, target)| LinkSpec { kind, target }),
        on_conflict: OnConflict::Fail,
    }
}
