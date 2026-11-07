//! In-memory sharing store, for unit tests and for running without Docker.
//!
//! It follows the same rules as the Postgres store ([`super::rules`]) and passes the same
//! conformance suite. Relationships go straight to the engine (no outbox in memory); journal
//! entries go to the shared [`MemoryJournal`].
//!
//! Known limits of the in-memory store: a grant to a user from another tenant does not add a
//! guest membership (the `TenantStore` trait has no method for it), and the node revision does not
//! change on `update_node_sharing` (the node store owns it).

use super::rules;
use super::*;
use crate::authz::{self, Authz, Permission, RelationshipUpdate, tuples};
use crate::error::{Error, ErrorCode};
use crate::journal::memory::MemoryJournal;
use crate::journal::{ChangeInfo, EventType, NewEntry};
use crate::mailing::rate::MemoryCounters;
use crate::nodes::NodeStore;
use crate::tenants::mail::Mailer;
use crate::tenants::{MemberRole, TenantStore};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
struct GrantRow {
    tenant: TenantId,
    grant: Grant,
}

#[derive(Clone, Debug)]
struct LinkRow {
    tenant: TenantId,
    link: ShareLink,
    token_hash: Vec<u8>,
    password_hash: Option<String>,
}

#[derive(Clone, Debug)]
struct IndexRow {
    tenant: TenantId,
    user: UserId,
    node: NodeId,
    grant: GrantId,
    role: Role,
    shared_by: Actor,
    shared_at: OffsetDateTime,
}

#[derive(Debug, Default)]
struct State {
    grants: Vec<GrantRow>,
    links: Vec<LinkRow>,
    index: Vec<IndexRow>,
    editors_can_share: HashMap<(TenantId, NodeId), Option<bool>>,
}

/// The in-memory store. It takes the other in-memory modules it works with.
pub struct MemorySharingStore {
    authz: Arc<dyn Authz>,
    nodes: Arc<dyn NodeStore>,
    tenants: Arc<dyn TenantStore>,
    journal: Arc<MemoryJournal>,
    state: Mutex<State>,
    /// Held across a whole grant change (`update_grant`, `remove_grant`), so two concurrent
    /// changes of one grant never both act on the same old role.
    grant_writes: tokio::sync::Mutex<()>,
    /// The mailer and the public URL of share emails ([`MemorySharingStore::set_mail`]).
    /// Without them no share email is sent (`notified: false`).
    mail: std::sync::OnceLock<(Arc<dyn Mailer>, String)>,
    /// The `addGrant` and share mail limits.
    counters: MemoryCounters,
    /// The share mails sent: tenant, recipient (lower case), node and time.
    recent_mails: Mutex<Vec<(TenantId, String, NodeId, OffsetDateTime)>>,
}

impl std::fmt::Debug for MemorySharingStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemorySharingStore").finish_non_exhaustive()
    }
}

impl MemorySharingStore {
    /// The store over the shared in-memory modules (`Services::in_memory` builds them once).
    /// There is no `Default`: a store with its own private modules would write grants to an
    /// engine that no other area checks.
    pub fn new(
        authz: Arc<dyn Authz>,
        nodes: Arc<dyn NodeStore>,
        tenants: Arc<dyn TenantStore>,
        journal: Arc<MemoryJournal>,
    ) -> Self {
        Self {
            authz,
            nodes,
            tenants,
            journal,
            state: Mutex::new(State::default()),
            grant_writes: tokio::sync::Mutex::new(()),
            mail: std::sync::OnceLock::new(),
            counters: MemoryCounters::default(),
            recent_mails: Mutex::new(Vec::new()),
        }
    }

    /// Sends share emails with `mailer`, with links to `public_url`. The in-memory store sends
    /// at once (there is no outbox in memory). Set once; later calls are ignored.
    pub fn set_mail(&self, mailer: Arc<dyn Mailer>, public_url: String) {
        let _ = self.mail.set((mailer, public_url));
    }

    /// Sends the share email of a new grant (see the Postgres store's `queue_share_mail` for
    /// the rules). Returns whether a mail was sent.
    async fn share_mail(
        &self,
        ctx: &Context,
        node: &Node,
        grantee: &Grantee,
        role: Role,
        message: Option<&str>,
    ) -> Result<bool> {
        use crate::mailing::{self as m, ShareFields};
        let Some((mailer, public_url)) = self.mail.get() else {
            return Ok(false);
        };
        let Some(sender) = ctx.actor.effective_user() else {
            return Ok(false);
        };
        let names = self
            .tenants
            .principal_names(ctx.tenant, &[sender.0, grantee_user(grantee)])
            .await?;
        let (to, email_grant) = match grantee {
            Grantee::Email { email } => (email.clone(), true),
            Grantee::User { id } => match names.get(&id.0).and_then(|n| n.email.clone()) {
                Some(to) if !to.is_empty() => (to, false),
                _ => return Ok(false),
            },
            Grantee::TenantMembers => return Ok(false),
        };
        let me = names.get(&sender.0);
        let sender_email = me.and_then(|n| n.email.clone()).unwrap_or_default();
        if to.eq_ignore_ascii_case(&sender_email) {
            return Ok(false);
        }
        let now = crate::now();
        let key = to.to_lowercase();
        {
            let mut recent = self.recent_mails.lock().unwrap_or_else(|e| e.into_inner());
            let since = now - time::Duration::seconds(m::SHARE_MAIL_REPEAT_SECONDS);
            recent.retain(|r| r.3 > since);
            if recent
                .iter()
                .any(|r| r.0 == ctx.tenant && r.1 == key && r.2 == node.id)
            {
                return Ok(false);
            }
            let user_key = m::share_mail_user_key(sender.0);
            let w = m::SHARE_MAIL_WINDOW_SECONDS;
            if self.counters.peek(ctx.tenant, &user_key, w) >= m::SHARE_MAIL_PER_USER
                || self.counters.peek(ctx.tenant, m::SHARE_MAIL_TENANT_KEY, w)
                    >= m::SHARE_MAIL_PER_TENANT
            {
                return Ok(false);
            }
            self.counters.hit(ctx.tenant, &user_key, w);
            self.counters.hit(ctx.tenant, m::SHARE_MAIL_TENANT_KEY, w);
            recent.push((ctx.tenant, key, node.id, now));
        }
        let fields = ShareFields {
            sharer: me.map(|n| n.display_name.clone()).unwrap_or_default(),
            item: node.name.clone(),
            folder: node.kind == crate::nodes::NodeKind::Folder,
            role,
            message: message.map(str::to_string),
            email_grant,
            // The in-memory names carry no verified flag; members' emails are verified there.
            reply_to: (!sender_email.is_empty()).then_some(sender_email),
        };
        let mail = fields.render(&to, public_url, ctx.tenant.0, node.id.0);
        if let Err(e) = mailer.send(mail).await {
            tracing::warn!(error = %e, "the share email was not sent");
        }
        Ok(true)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("sharing lock")
    }

    fn append(
        &self,
        ctx: &Context,
        node: &Node,
        event: EventType,
        data: serde_json::Value,
    ) -> Result<ChangeInfo> {
        let e = self.journal.append(
            ctx,
            NewEntry {
                node_id: Some(node.id),
                revision: Some(node.revision),
                event,
                data,
            },
        )?;
        Ok(ChangeInfo {
            seq: Some(e.seq),
            actor: ctx.actor,
            at: e.at,
        })
    }

    fn grant_subject(tenant: TenantId, grantee: &Grantee) -> Option<authz::SubjectRef> {
        match grantee {
            Grantee::User { id } => Some(tuples::grant_subject_user(*id)),
            Grantee::TenantMembers => Some(tuples::grant_subject_members(tenant)),
            Grantee::Email { .. } => None,
        }
    }

    fn find_grant(&self, tenant: TenantId, node: NodeId, id: GrantId) -> Result<Grant> {
        self.state()
            .grants
            .iter()
            .find(|g| g.tenant == tenant && g.grant.node_id == node && g.grant.id == id)
            .map(|g| g.grant.clone())
            .ok_or_else(|| Error::not_found("grant"))
    }

    /// Binds the email grants of `email` in `tenant` to `user` (invitation accepted, or a verified
    /// email of a trusted provider). Returns the bound grants.
    pub async fn bind_email_grants(
        &self,
        tenant: TenantId,
        email: &str,
        user: UserId,
    ) -> Result<Vec<Grant>> {
        let mut updates = Vec::new();
        let done;
        {
            let mut st = self.state();
            let taken: Vec<NodeId> = st
                .grants
                .iter()
                .filter(|g| g.tenant == tenant && g.grant.grantee == (Grantee::User { id: user }))
                .map(|g| g.grant.node_id)
                .collect();
            let mut bound = Vec::new();
            for g in st.grants.iter_mut().filter(|g| g.tenant == tenant) {
                if let Grantee::Email { email: e } = &g.grant.grantee
                    && e.eq_ignore_ascii_case(email)
                    && !taken.contains(&g.grant.node_id)
                {
                    g.grant.grantee = Grantee::User { id: user };
                    updates.push(RelationshipUpdate::Touch(tuples::grant(
                        g.grant.node_id,
                        g.grant.role,
                        tuples::grant_subject_user(user),
                    )));
                    bound.push(g.grant.clone());
                }
            }
            done = bound.clone();
            for g in bound {
                st.index.push(IndexRow {
                    tenant,
                    user,
                    node: g.node_id,
                    grant: g.id,
                    role: g.role,
                    shared_by: g.created_by,
                    shared_at: g.created_at,
                });
            }
        }
        if !updates.is_empty() {
            self.authz.write(updates).await?;
        }
        Ok(done)
    }
}

#[async_trait]
impl SharingStore for MemorySharingStore {
    async fn list_grants(
        &self,
        ctx: &Context,
        node: NodeId,
        page: PageRequest,
    ) -> Result<Page<Grant>> {
        rules::refuse_share_links(ctx)?;
        authz::require(&*self.authz, &*self.nodes, ctx, node, Permission::View).await?;
        self.nodes.get(ctx, node).await?;
        let mut on: Vec<NodeId> = self.nodes.ancestors(ctx, node).await?;
        on.push(node);
        let after = page
            .cursor
            .as_ref()
            .map(rules::parse_id_cursor)
            .transpose()?;
        let mut items: Vec<Grant> = self
            .state()
            .grants
            .iter()
            .filter(|g| g.tenant == ctx.tenant && on.contains(&g.grant.node_id))
            .filter(|g| after.is_none_or(|a| g.grant.id.0 > a))
            .map(|g| {
                let mut grant = g.grant.clone();
                grant.inherited_from = (grant.node_id != node).then_some(grant.node_id);
                grant
            })
            .collect();
        items.sort_by_key(|g| g.id);
        Ok(page_of(items, page.limit(), |g| g.id.0))
    }

    async fn share(&self, ctx: &Context, node: NodeId, req: NewGrant) -> Result<GrantAdded> {
        use crate::mailing as m;
        let NewGrant {
            grantee,
            role,
            notify,
            message,
        } = req;
        // The limits count every call, also refused ones (decision 0003-2).
        let user = ctx
            .actor
            .effective_user()
            .map(|u| u.0)
            .unwrap_or(ctx.actor.actor.id.0);
        let mine = self.counters.hit(
            ctx.tenant,
            &m::add_grant_user_key(user),
            m::ADD_GRANT_WINDOW_SECONDS,
        );
        let all = self.counters.hit(
            ctx.tenant,
            m::ADD_GRANT_TENANT_KEY,
            m::ADD_GRANT_WINDOW_SECONDS,
        );
        if mine > m::ADD_GRANT_PER_USER || all > m::ADD_GRANT_PER_TENANT {
            return Err(m::add_grant_rate_limited());
        }
        rules::check_grant(&*self.authz, &*self.nodes, ctx, node, role).await?;
        let n = self.nodes.get(ctx, node).await?;
        let email = rules::check_grantee(&grantee)?;
        let grantee = match email {
            Some(email) => Grantee::Email { email },
            None => grantee,
        };
        let policy = self.tenants.find(ctx.tenant).await?.policy;
        let caller_role = match ctx.actor.effective_user() {
            Some(u) => self.tenants.role_of(ctx.tenant, u).await?,
            None => None,
        };
        // (a) and (a'), as in the Postgres store.
        let grantee = match grantee {
            Grantee::Email { email } if is_inside(caller_role) => {
                match self.tenants.verified_person(ctx.tenant, &email).await? {
                    Some(id) => Grantee::User { id },
                    None => Grantee::Email { email },
                }
            }
            Grantee::Email { email } => {
                if !policy.allow_guests {
                    return Err(rules::guests_refused());
                }
                Grantee::Email { email }
            }
            other => other,
        };
        match &grantee {
            Grantee::User { id } => {
                // Only people already in the tenant (see the Postgres store).
                let role = self.tenants.role_of(ctx.tenant, *id).await?;
                if role.is_none() {
                    return Err(rules::no_such_user());
                }
                if !is_inside(role) && !policy.allow_guests {
                    return Err(rules::guests_refused());
                }
            }
            // While guests are off, an email grant reaches only the verified email of a person
            // who is already inside the tenant (the same rule as the Postgres store).
            Grantee::Email { email }
                if !policy.allow_guests
                    && !self
                        .tenants
                        .is_verified_member_email(ctx.tenant, email)
                        .await? =>
            {
                return Err(rules::guests_refused());
            }
            _ => {}
        }
        let grant = Grant {
            id: GrantId::new(),
            node_id: node,
            grantee: grantee.clone(),
            role,
            inherited_from: None,
            created_by: ctx.actor,
            created_at: crate::now(),
        };
        {
            let mut st = self.state();
            let same = |g: &&GrantRow| {
                g.tenant == ctx.tenant
                    && g.grant.node_id == node
                    && match (&g.grant.grantee, &grantee) {
                        (Grantee::Email { email: a }, Grantee::Email { email: b }) => {
                            a.eq_ignore_ascii_case(b)
                        }
                        (a, b) => a == b,
                    }
            };
            if st.grants.iter().any(|g| same(&g)) {
                return Err(Error::api(
                    ErrorCode::Conflict,
                    "this grantee already has a grant on the node",
                ));
            }
            st.grants.push(GrantRow {
                tenant: ctx.tenant,
                grant: grant.clone(),
            });
            if let Grantee::User { id } = grantee {
                st.index.push(IndexRow {
                    tenant: ctx.tenant,
                    user: id,
                    node,
                    grant: grant.id,
                    role,
                    shared_by: ctx.actor,
                    shared_at: grant.created_at,
                });
            }
        }
        if let Some(subject) = Self::grant_subject(ctx.tenant, &grant.grantee) {
            self.authz
                .write(vec![RelationshipUpdate::Touch(tuples::grant(
                    node, role, subject,
                ))])
                .await?;
        }
        let change = self.append(
            ctx,
            &n,
            EventType::GrantAdded,
            rules::grant_added_data(grant.id, &grant.grantee, role),
        )?;
        let notified = if notify {
            self.share_mail(ctx, &n, &grant.grantee, role, message.as_deref())
                .await?
        } else {
            false
        };
        Ok(GrantAdded {
            written: Written {
                value: grant,
                change,
            },
            notified,
        })
    }

    async fn update_grant(
        &self,
        ctx: &Context,
        node: NodeId,
        grant: GrantId,
        role: Role,
    ) -> Result<Written<Grant>> {
        rules::refuse_share_links(ctx)?;
        authz::require(&*self.authz, &*self.nodes, ctx, node, Permission::View).await?;
        let n = self.nodes.get(ctx, node).await?;
        let _write = self.grant_writes.lock().await;
        let old = self.find_grant(ctx.tenant, node, grant)?;
        rules::check_change(&*self.authz, &*self.nodes, ctx, node, old.role, Some(role)).await?;
        let mut updated = old.clone();
        updated.role = role;
        {
            let mut st = self.state();
            for g in st.grants.iter_mut().filter(|g| g.grant.id == grant) {
                g.grant.role = role;
            }
            for i in st.index.iter_mut().filter(|i| i.grant == grant) {
                i.role = role;
            }
        }
        if let Some(subject) = Self::grant_subject(ctx.tenant, &old.grantee) {
            self.authz
                .write(tuples::change_grant(node, old.role, role, subject))
                .await?;
        }
        let change = self.append(
            ctx,
            &n,
            EventType::GrantUpdated,
            json!({
                "grant_id": grant, "grantee": old.grantee,
                "old_role": old.role, "new_role": role,
            }),
        )?;
        Ok(Written {
            value: updated,
            change,
        })
    }

    async fn remove_grant(
        &self,
        ctx: &Context,
        node: NodeId,
        grant: GrantId,
    ) -> Result<Written<()>> {
        rules::refuse_share_links(ctx)?;
        authz::require(&*self.authz, &*self.nodes, ctx, node, Permission::View).await?;
        let n = self.nodes.get(ctx, node).await?;
        let _write = self.grant_writes.lock().await;
        let old = self.find_grant(ctx.tenant, node, grant)?;
        rules::check_change(&*self.authz, &*self.nodes, ctx, node, old.role, None).await?;
        {
            let mut st = self.state();
            st.grants.retain(|g| g.grant.id != grant);
            st.index.retain(|i| i.grant != grant);
        }
        if let Some(subject) = Self::grant_subject(ctx.tenant, &old.grantee) {
            self.authz
                .write(vec![RelationshipUpdate::Delete(tuples::grant(
                    node, old.role, subject,
                ))])
                .await?;
        }
        let change = self.append(
            ctx,
            &n,
            EventType::GrantRemoved,
            json!({ "grant_id": grant, "grantee": old.grantee, "role": old.role }),
        )?;
        Ok(Written { value: (), change })
    }

    async fn list_share_links(
        &self,
        ctx: &Context,
        node: NodeId,
        page: PageRequest,
    ) -> Result<Page<ShareLink>> {
        rules::refuse_share_links(ctx)?;
        authz::require(&*self.authz, &*self.nodes, ctx, node, Permission::Share).await?;
        self.nodes.get(ctx, node).await?;
        let after = page
            .cursor
            .as_ref()
            .map(rules::parse_id_cursor)
            .transpose()?;
        let mut items: Vec<ShareLink> = self
            .state()
            .links
            .iter()
            .filter(|l| l.tenant == ctx.tenant && l.link.node_id == node)
            .filter(|l| after.is_none_or(|a| l.link.id.0 > a))
            .map(|l| l.link.clone())
            .collect();
        items.sort_by_key(|l| l.id);
        Ok(page_of(items, page.limit(), |l| l.id.0))
    }

    async fn create_share_link(
        &self,
        ctx: &Context,
        node: NodeId,
        req: NewShareLink,
    ) -> Result<Written<ShareLink>> {
        rules::check_grant(&*self.authz, &*self.nodes, ctx, node, req.role.as_role()).await?;
        let n = self.nodes.get(ctx, node).await?;
        let policy = self.tenants.find(ctx.tenant).await?.policy;
        rules::check_new_link(&policy, &req, crate::now())?;
        let token = rules::new_link_token();
        let password_hash = req
            .password
            .as_deref()
            .map(rules::hash_password)
            .transpose()?;
        let link = ShareLink {
            id: ShareLinkId::new(),
            node_id: node,
            role: req.role,
            token: None,
            url: None,
            expires_at: req.expires_at,
            has_password: password_hash.is_some(),
            created_by: ctx.actor,
            created_at: crate::now(),
            revoked_at: None,
        };
        self.state().links.push(LinkRow {
            tenant: ctx.tenant,
            link: link.clone(),
            token_hash: rules::hash_token(&token),
            password_hash,
        });
        self.authz
            .write(vec![RelationshipUpdate::Touch(tuples::share_link(
                node, link.id, link.role,
            ))])
            .await?;
        let change = self.append(
            ctx,
            &n,
            EventType::ShareLinkCreated,
            share_link_created(&link),
        )?;
        let mut value = link;
        value.token = Some(token);
        Ok(Written { value, change })
    }

    async fn revoke_share_link(&self, ctx: &Context, link: ShareLinkId) -> Result<Written<()>> {
        rules::refuse_share_links(ctx)?;
        let row = self
            .state()
            .links
            .iter()
            .find(|l| l.tenant == ctx.tenant && l.link.id == link && l.link.revoked_at.is_none())
            .cloned()
            .ok_or_else(|| Error::not_found("share link"))?;
        let node = row.link.node_id;
        authz::require(&*self.authz, &*self.nodes, ctx, node, Permission::Share).await?;
        let n = self.nodes.get(ctx, node).await?;
        for l in self.state().links.iter_mut().filter(|l| l.link.id == link) {
            l.link.revoked_at = Some(crate::now());
        }
        self.authz
            .write(vec![RelationshipUpdate::Delete(tuples::share_link(
                node,
                link,
                row.link.role,
            ))])
            .await?;
        let change = self.append(
            ctx,
            &n,
            EventType::ShareLinkRevoked,
            json!({ "share_link_id": link }),
        )?;
        Ok(Written { value: (), change })
    }

    async fn resolve_share_link(
        &self,
        token: &str,
        password: Option<&str>,
    ) -> Result<(TenantId, Node, ShareLink)> {
        let hash = rules::hash_token(token);
        let row = self
            .state()
            .links
            .iter()
            .find(|l| l.token_hash == hash)
            .cloned()
            .ok_or_else(|| Error::not_found("share link"))?;
        let ctx = rules::link_context(row.tenant, &row.link);
        let policy = self.tenants.find(ctx.tenant).await?.policy;
        if !rules::link_is_open(&policy, &row.link, crate::now()) {
            return Err(Error::not_found("share link"));
        }
        if let Some(phc) = &row.password_hash {
            rules::check_link_password(&hash, phc, password)?;
        }
        let node = self.nodes.get(&ctx, row.link.node_id).await?;
        Ok((row.tenant, node, row.link))
    }

    async fn shared_with_me(&self, ctx: &Context, page: PageRequest) -> Result<Page<SharedItem>> {
        rules::refuse_share_links(ctx)?;
        let Some(user) = ctx.actor.effective_user() else {
            return Ok(Page::empty());
        };
        let after = page
            .cursor
            .as_ref()
            .map(rules::parse_shared_cursor)
            .transpose()?;
        let (mut rows, members_grants): (Vec<IndexRow>, Vec<GrantRow>) = {
            let st = self.state();
            (
                st.index
                    .iter()
                    .filter(|i| i.user == user)
                    .filter(|i| ctx.scope.is_none() || i.tenant == ctx.tenant)
                    .cloned()
                    .collect(),
                st.grants
                    .iter()
                    .filter(|g| g.grant.grantee == Grantee::TenantMembers)
                    .filter(|g| ctx.scope.is_none() || g.tenant == ctx.tenant)
                    .cloned()
                    .collect(),
            )
        };
        // Grants to `tenant#member` are expanded here, from the user's memberships.
        for g in members_grants {
            if is_inside(self.tenants.role_of(g.tenant, user).await?) {
                rows.push(IndexRow {
                    tenant: g.tenant,
                    user,
                    node: g.grant.node_id,
                    grant: g.grant.id,
                    role: g.grant.role,
                    shared_by: g.grant.created_by,
                    shared_at: g.grant.created_at,
                });
            }
        }
        let mut rows = rules::one_per_node(rows, |r| (r.tenant, r.node), |r| r.role);
        rows.sort_by_key(|r| std::cmp::Reverse((r.shared_at, r.node)));
        if let Some((at, node)) = after {
            rows.retain(|r| (r.shared_at, r.node) < (at, node));
        }
        let limit = page.limit() as usize;
        let mut items = Vec::new();
        let mut last = None;
        let mut more = false;
        for r in rows {
            if items.len() == limit {
                more = true;
                break;
            }
            last = Some((r.shared_at, r.node));
            let tctx = Context {
                tenant: r.tenant,
                ..ctx.clone()
            };
            if !authz::in_scope_subtree(&*self.nodes, &tctx, r.node).await? {
                continue;
            }
            match self.nodes.get(&tctx, r.node).await {
                Ok(node) if node.trashed_at.is_none() => items.push(SharedItem {
                    node,
                    tenant_name: self.tenants.find(r.tenant).await?.name,
                    role: r.role,
                    shared_by: r.shared_by,
                    shared_at: r.shared_at,
                }),
                Ok(_) => {}
                Err(Error::Api(e)) if e.code == ErrorCode::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Page {
            items,
            next_cursor: if more {
                last.map(|(at, n)| rules::shared_cursor(at, n))
            } else {
                None
            },
        })
    }

    async fn update_node_sharing(
        &self,
        ctx: &Context,
        node: NodeId,
        editors_can_share: Option<bool>,
        if_revision: Option<i64>,
    ) -> Result<Written<Node>> {
        rules::refuse_share_links(ctx)?;
        authz::require(&*self.authz, &*self.nodes, ctx, node, Permission::Manage).await?;
        let n = self.nodes.get(ctx, node).await?;
        if if_revision.is_some_and(|r| r != n.revision) {
            return Err(Error::api(
                ErrorCode::PreconditionFailed,
                "the node changed since you read it",
            ));
        }
        let old = self
            .state()
            .editors_can_share
            .insert((ctx.tenant, node), editors_can_share)
            .flatten();
        self.authz
            .write(tuples::editors_can_share(node, editors_can_share))
            .await?;
        let change = self.append(
            ctx,
            &n,
            EventType::NodeSharingUpdated,
            json!({ "old_editors_can_share": old, "new_editors_can_share": editors_can_share }),
        )?;
        let node = self.nodes.get(ctx, node).await?;
        Ok(Written {
            value: node,
            change,
        })
    }
}

/// The data of `share_link.created`.
pub(crate) fn share_link_created(link: &ShareLink) -> serde_json::Value {
    let expires = link.expires_at.map(|t| {
        t.format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default()
    });
    json!({
        "share_link_id": link.id,
        "role": link.role,
        "expires_at": expires,
        "has_password": link.has_password,
    })
}

/// One page of items sorted by a UUIDv7 key.
pub(crate) fn page_of<T>(mut items: Vec<T>, limit: u32, key: impl Fn(&T) -> uuid::Uuid) -> Page<T> {
    let limit = limit as usize;
    let more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if more {
        items.last().map(|i| rules::id_cursor(key(i)))
    } else {
        None
    };
    Page { items, next_cursor }
}

/// The user of a user grantee, or the nil UUID.
fn grantee_user(g: &Grantee) -> uuid::Uuid {
    match g {
        Grantee::User { id } => id.0,
        _ => uuid::Uuid::nil(),
    }
}

/// The role of a user in a tenant counts as "inside the tenant" (not a guest).
pub(crate) fn is_inside(role: Option<MemberRole>) -> bool {
    matches!(
        role,
        Some(MemberRole::Member | MemberRole::Admin | MemberRole::Owner)
    )
}

/// The sharing part of `removeMember` and of email binding for the in-memory tenant store (the
/// Postgres tenant store runs the same steps in its own transaction).
#[async_trait]
impl crate::tenants::MemberCleanup for MemorySharingStore {
    /// Removes the user's direct grants in the tenant and revokes the share links that the user
    /// or the user's agents made there. Grants the user made for others stay.
    async fn member_removed(&self, ctx: &Context, user: UserId) -> Result<()> {
        let tenant = ctx.tenant;
        let me = Grantee::User { id: user };
        let made_by = |a: &Actor| {
            a.on_behalf_of == Some(user)
                || (a.actor.kind == crate::actor::PrincipalKind::User && a.actor.id.0 == user.0)
        };
        let (grants, links) = {
            let mut st = self.state();
            let grants: Vec<Grant> = st
                .grants
                .iter()
                .filter(|g| g.tenant == tenant && g.grant.grantee == me)
                .map(|g| g.grant.clone())
                .collect();
            st.grants
                .retain(|g| !(g.tenant == tenant && g.grant.grantee == me));
            st.index.retain(|r| !(r.tenant == tenant && r.user == user));
            let now = crate::now();
            let mut links = Vec::new();
            for l in st.links.iter_mut().filter(|l| {
                l.tenant == tenant && l.link.revoked_at.is_none() && made_by(&l.link.created_by)
            }) {
                l.link.revoked_at = Some(now);
                links.push(l.link.clone());
            }
            (grants, links)
        };
        let mut updates = Vec::new();
        for g in &grants {
            self.journal.append(
                ctx,
                NewEntry {
                    node_id: Some(g.node_id),
                    revision: None,
                    event: EventType::GrantRemoved,
                    data: json!({ "grant_id": g.id, "grantee": g.grantee, "role": g.role }),
                },
            )?;
            updates.push(RelationshipUpdate::Delete(tuples::grant(
                g.node_id,
                g.role,
                tuples::grant_subject_user(user),
            )));
        }
        for l in &links {
            self.journal.append(
                ctx,
                NewEntry {
                    node_id: Some(l.node_id),
                    revision: None,
                    event: EventType::ShareLinkRevoked,
                    data: json!({ "share_link_id": l.id }),
                },
            )?;
            updates.push(RelationshipUpdate::Delete(tuples::share_link(
                l.node_id, l.id, l.role,
            )));
        }
        if !updates.is_empty() {
            self.authz.write(updates).await?;
        }
        Ok(())
    }

    async fn invitation_accepted(&self, ctx: &Context, email: &str, user: UserId) -> Result<()> {
        self.bind_email_grants(ctx.tenant, email, user).await?;
        Ok(())
    }

    /// Binds the email grants of `email` in every tenant (decision 0003-2 (b) and (c)): a
    /// tenant where the user is not a member or guest makes them a guest, only while it allows
    /// guests; otherwise it keeps its email grants.
    async fn verified_email_signed_in(&self, email: &str, user: UserId) -> Result<()> {
        let mut tenants: Vec<TenantId> = self
            .state()
            .grants
            .iter()
            .filter(|g| matches!(&g.grant.grantee, Grantee::Email { email: e } if e.eq_ignore_ascii_case(email)))
            .map(|g| g.tenant)
            .collect();
        tenants.sort();
        tenants.dedup();
        for t in tenants {
            let old = self.tenants.role_of(t, user).await?;
            if old.is_none() && !self.tenants.find(t).await?.policy.allow_guests {
                continue;
            }
            let bound = self.bind_email_grants(t, email, user).await?;
            if old.is_none() && !bound.is_empty() {
                let mut granted_by: Vec<crate::actor::PrincipalRef> = Vec::new();
                for g in &bound {
                    if !granted_by.contains(&g.created_by.actor) {
                        granted_by.push(g.created_by.actor);
                    }
                }
                let ids: Vec<GrantId> = bound.iter().map(|g| g.id).collect();
                self.tenants
                    .add_guest_from_email_grant(
                        t,
                        user,
                        json!({
                            "role": MemberRole::Guest,
                            "via": "email_grant",
                            "grant_ids": ids,
                            "granted_by": granted_by,
                        }),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}
