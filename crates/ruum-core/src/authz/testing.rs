//! Test helpers of the authz and sharing modules.

use crate::actor::{Actor, Context};
use crate::error::{Error, Result};
use crate::ids::{NodeId, TenantId, UserId};
use crate::journal::{ChangeInfo, Written};
use crate::nodes::*;
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;

/// A node store that knows the tree shape and the nodes (for `ancestors` and `get`). `get` does
/// not check permissions; it filters by tenant and hides nothing else.
#[derive(Default)]
pub(crate) struct TreeNodes {
    pub parents: RwLock<HashMap<NodeId, NodeId>>,
    pub nodes: RwLock<HashMap<NodeId, Node>>,
}

impl TreeNodes {
    pub fn add(&self, node: NodeId, parent: NodeId) {
        self.parents.write().unwrap().insert(node, parent);
    }

    /// Adds a folder node to the tree.
    pub fn folder(
        &self,
        tenant: TenantId,
        id: NodeId,
        parent: Option<NodeId>,
        name: &str,
        owner: UserId,
    ) {
        if let Some(p) = parent {
            self.add(id, p);
        }
        let now = crate::now();
        self.nodes.write().unwrap().insert(
            id,
            Node {
                id,
                tenant_id: tenant,
                parent_id: parent,
                name: name.to_string(),
                kind: NodeKind::Folder,
                node_type: None,
                link_kind: None,
                link_target: None,
                head_version: None,
                owner_id: owner,
                size_bytes: 0,
                revision: 1,
                trashed_at: None,
                created_at: now,
                updated_at: now,
                created_by: Actor::user(owner),
                updated_by: Actor::user(owner),
                my_role: None,
                my_can_share: None,
                editors_can_share: None,
                editors_can_share_effective: None,
            },
        );
    }
}

#[async_trait]
impl NodeStore for TreeNodes {
    async fn root(&self, _: &Context) -> Result<Node> {
        unimplemented!()
    }
    async fn create(&self, _: &Context, _: NewNode) -> Result<Written<Node>> {
        unimplemented!()
    }
    async fn get(&self, ctx: &Context, id: NodeId) -> Result<Node> {
        self.nodes
            .read()
            .unwrap()
            .get(&id)
            .filter(|n| n.tenant_id == ctx.tenant)
            .cloned()
            .ok_or_else(|| Error::not_found("node"))
    }
    async fn get_by_path(&self, _: &Context, _: &str) -> Result<Node> {
        unimplemented!()
    }
    async fn list_children(
        &self,
        _: &Context,
        _: NodeId,
        _: ChildOrder,
        _: PageRequest,
    ) -> Result<Page<Node>> {
        unimplemented!()
    }
    async fn rename(
        &self,
        _: &Context,
        _: NodeId,
        _: String,
        _: OnConflict,
        _: Option<i64>,
    ) -> Result<Written<Node>> {
        unimplemented!()
    }
    async fn move_node(&self, _: &Context, _: NodeId, _: MoveRequest) -> Result<Written<Node>> {
        unimplemented!()
    }
    async fn copy(&self, _: &Context, _: NodeId, _: CopyRequest) -> Result<Written<Node>> {
        unimplemented!()
    }
    async fn trash(&self, _: &Context, _: NodeId, _: Option<i64>) -> Result<Written<Node>> {
        unimplemented!()
    }
    async fn restore(&self, _: &Context, _: NodeId, _: RestoreRequest) -> Result<Written<Node>> {
        unimplemented!()
    }
    async fn list_trash(&self, _: &Context, _: bool, _: PageRequest) -> Result<Page<Node>> {
        unimplemented!()
    }
    async fn purge(&self, _: &Context, _: NodeId) -> Result<Written<()>> {
        unimplemented!()
    }
    async fn empty_trash(&self, _: &Context) -> Result<(u32, Option<ChangeInfo>)> {
        unimplemented!()
    }
    async fn path_of(&self, _: &Context, _: NodeId) -> Result<String> {
        unimplemented!()
    }
    async fn ancestors(&self, _: &Context, id: NodeId) -> Result<Vec<NodeId>> {
        let p = self.parents.read().unwrap();
        let mut out = Vec::new();
        let mut cur = id;
        while let Some(&parent) = p.get(&cur) {
            out.push(parent);
            cur = parent;
        }
        out.reverse();
        Ok(out)
    }
}

use crate::tenants::*;

/// A tenant store that knows roles and policies (for `get` and `role_of`).
#[derive(Default)]
pub(crate) struct TestTenants {
    pub roles: RwLock<HashMap<(TenantId, UserId), MemberRole>>,
    pub policies: RwLock<HashMap<TenantId, TenantPolicy>>,
    /// Verified emails of users, for `is_verified_member_email`.
    pub verified_emails: RwLock<HashMap<UserId, String>>,
}

#[async_trait]
impl TenantStore for TestTenants {
    async fn list_for_user(&self, _: UserId) -> Result<Vec<(Tenant, MemberRole)>> {
        unimplemented!()
    }
    async fn create_team(&self, _: &Actor, _: String) -> Result<Tenant> {
        unimplemented!()
    }
    async fn get(&self, ctx: &Context) -> Result<Tenant> {
        self.find(ctx.tenant).await
    }
    async fn find(&self, tenant: TenantId) -> Result<Tenant> {
        Ok(Tenant {
            id: tenant,
            name: "test".into(),
            kind: TenantKind::Team,
            plan: Plan::Team,
            region: "hel1".into(),
            root_node_id: NodeId::new(),
            policy: self
                .policies
                .read()
                .unwrap()
                .get(&tenant)
                .cloned()
                .unwrap_or_default(),
            created_at: crate::now(),
        })
    }
    async fn update(
        &self,
        _: &Context,
        _: Option<String>,
        _: Option<TenantPolicyChange>,
    ) -> Result<Tenant> {
        unimplemented!()
    }
    async fn role_of(&self, tenant: TenantId, user: UserId) -> Result<Option<MemberRole>> {
        Ok(self.roles.read().unwrap().get(&(tenant, user)).copied())
    }
    async fn is_verified_member_email(&self, tenant: TenantId, email: &str) -> Result<bool> {
        let roles = self.roles.read().unwrap();
        Ok(self.verified_emails.read().unwrap().iter().any(|(u, e)| {
            e.eq_ignore_ascii_case(email)
                && roles
                    .get(&(tenant, *u))
                    .is_some_and(|r| *r != MemberRole::Guest)
        }))
    }
    async fn verified_person(&self, tenant: TenantId, email: &str) -> Result<Option<UserId>> {
        let roles = self.roles.read().unwrap();
        Ok(self
            .verified_emails
            .read()
            .unwrap()
            .iter()
            .find(|(u, e)| {
                e.eq_ignore_ascii_case(email.trim()) && roles.contains_key(&(tenant, **u))
            })
            .map(|(u, _)| *u))
    }
    async fn principal_names(
        &self,
        tenant: TenantId,
        ids: &[uuid::Uuid],
    ) -> Result<std::collections::HashMap<uuid::Uuid, crate::tenants::PrincipalName>> {
        let roles = self.roles.read().unwrap();
        let emails = self.verified_emails.read().unwrap();
        Ok(ids
            .iter()
            .filter(|id| roles.contains_key(&(tenant, UserId(**id))))
            .map(|id| {
                (
                    *id,
                    crate::tenants::PrincipalName {
                        display_name: format!("User {}", &id.to_string()[..8]),
                        email: emails.get(&UserId(*id)).cloned(),
                        owner: None,
                    },
                )
            })
            .collect())
    }
    async fn list_members(&self, _: &Context, _: PageRequest) -> Result<Page<Member>> {
        unimplemented!()
    }
    async fn access_of(
        &self,
        tenant: TenantId,
        user: UserId,
    ) -> Result<Option<(MemberRole, Option<super::Role>)>> {
        // The fake knows no content roles: its members have the default, `manage`.
        Ok(self.roles.read().unwrap().get(&(tenant, user)).map(|r| {
            (
                *r,
                (*r == MemberRole::Member).then_some(super::Role::Manage),
            )
        }))
    }
    async fn update_member(
        &self,
        _: &Context,
        _: UserId,
        _: MemberRole,
        _: Option<super::Role>,
    ) -> Result<Member> {
        unimplemented!()
    }
    async fn remove_member(&self, _: &Context, _: UserId) -> Result<()> {
        unimplemented!()
    }
    async fn invite_as(
        &self,
        _: &Context,
        _: String,
        _: MemberRole,
        _: Option<super::Role>,
        _: Option<String>,
    ) -> Result<(Invitation, String)> {
        unimplemented!()
    }
    async fn list_invitations(&self, _: &Context, _: PageRequest) -> Result<Page<Invitation>> {
        unimplemented!()
    }
    async fn revoke_invitation(&self, _: &Context, _: crate::ids::InvitationId) -> Result<()> {
        unimplemented!()
    }
    async fn accept_invitation(&self, _: &Actor, _: &str) -> Result<Member> {
        unimplemented!()
    }
    async fn usage(&self, _: &Context) -> Result<Usage> {
        unimplemented!()
    }
}
