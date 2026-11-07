//! In-memory tag store, for unit tests and for running without Docker. It follows the same
//! rules as the Postgres store ([`super::rules`]) and passes the same conformance suite.

use super::rules::{self, TagCaller};
use super::*;
use crate::authz::{self, Authz, Role};
use crate::journal::memory::MemoryJournal;
use crate::journal::{ChangeInfo, EventType, NewEntry};
use crate::nodes::memory::{MemoryNodeStore, no_change};
use crate::nodes::rules as node_rules;
use crate::tenants::TenantStore;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, Default)]
struct State {
    tags: HashMap<TagId, Tag>,
    /// `(tenant, tag, node)`.
    links: BTreeSet<(TenantId, TagId, NodeId)>,
}

/// The in-memory store.
pub struct MemoryTagStore {
    authz: Arc<dyn Authz>,
    nodes: Arc<MemoryNodeStore>,
    journal: Arc<MemoryJournal>,
    tenants: Arc<dyn TenantStore>,
    state: Mutex<State>,
}

impl std::fmt::Debug for MemoryTagStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryTagStore").finish_non_exhaustive()
    }
}

impl MemoryTagStore {
    pub fn new(
        authz: Arc<dyn Authz>,
        nodes: Arc<MemoryNodeStore>,
        journal: Arc<MemoryJournal>,
        tenants: Arc<dyn TenantStore>,
    ) -> Self {
        Self {
            authz,
            nodes,
            journal,
            tenants,
            state: Mutex::new(State::default()),
        }
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("tags lock")
    }

    /// The live nodes of the tenant that have the tag (for search).
    pub(crate) fn tagged(&self, tenant: TenantId, tag: TagId) -> HashSet<NodeId> {
        let st = self.st();
        st.links
            .range((tenant, tag, NodeId(uuid::Uuid::nil()))..)
            .take_while(|(t, g, _)| *t == tenant && *g == tag)
            .map(|(_, _, n)| *n)
            .filter(|n| self.nodes.live_node(tenant, *n).is_some())
            .collect()
    }

    async fn caller(&self, ctx: &Context) -> Result<TagCaller> {
        let access = match ctx.actor.effective_user() {
            Some(u) => self.tenants.access_of(ctx.tenant, u).await?,
            None => None,
        };
        Ok(TagCaller::new(
            ctx,
            access.map(|a| a.0),
            access.and_then(|a| a.1),
        ))
    }

    /// `(tag, node)` for every tag on a live node of the tenant.
    fn live_pairs(&self, tenant: TenantId) -> Vec<(TagId, NodeId)> {
        let links: Vec<(TagId, NodeId)> = self
            .st()
            .links
            .iter()
            .filter(|(t, _, _)| *t == tenant)
            .map(|(_, g, n)| (*g, *n))
            .collect();
        links
            .into_iter()
            .filter(|(_, n)| self.nodes.live_node(tenant, *n).is_some())
            .collect()
    }

    /// The tag, if the caller sees it.
    async fn visible(&self, ctx: &Context, id: TagId) -> Result<(Tag, TagCaller)> {
        let tag = self
            .st()
            .tags
            .get(&id)
            .filter(|t| t.tenant_id == ctx.tenant)
            .cloned()
            .ok_or_else(rules::tag_not_found)?;
        let caller = self.caller(ctx).await?;
        if !caller.sees_all {
            let pairs: Vec<(TagId, NodeId)> = self
                .live_pairs(ctx.tenant)
                .into_iter()
                .filter(|(g, _)| *g == id)
                .collect();
            let seen = rules::visible_tags(&*self.authz, &*self.nodes, ctx, &pairs).await?;
            if !seen.contains(&id) {
                return Err(rules::tag_not_found());
            }
        }
        Ok((tag, caller))
    }

    fn append(
        &self,
        ctx: &Context,
        node: &Node,
        event: EventType,
        tag: &Tag,
    ) -> Result<ChangeInfo> {
        let e = self.journal.append(
            ctx,
            NewEntry {
                node_id: Some(node.id),
                revision: Some(node.revision),
                event,
                data: rules::tag_data(tag),
            },
        )?;
        Ok(ChangeInfo {
            seq: Some(e.seq),
            actor: ctx.actor,
            at: e.at,
        })
    }

    fn tag_of(&self, tenant: TenantId, id: TagId) -> Result<Tag> {
        self.st()
            .tags
            .get(&id)
            .filter(|t| t.tenant_id == tenant)
            .cloned()
            .ok_or_else(rules::tag_not_found)
    }
}

#[async_trait]
impl TagStore for MemoryTagStore {
    async fn list(&self, ctx: &Context, page: PageRequest) -> Result<Page<Tag>> {
        let after = page
            .cursor
            .as_ref()
            .map(node_rules::parse_key_cursor)
            .transpose()?;
        let caller = self.caller(ctx).await?;
        let seen = if caller.sees_all {
            None
        } else {
            let pairs = self.live_pairs(ctx.tenant);
            Some(rules::visible_tags(&*self.authz, &*self.nodes, ctx, &pairs).await?)
        };
        let mut tags: Vec<Tag> = self
            .st()
            .tags
            .values()
            .filter(|t| t.tenant_id == ctx.tenant)
            .filter(|t| seen.as_ref().is_none_or(|s| s.contains(&t.id)))
            .cloned()
            .collect();
        let key = |t: &Tag| (rules::name_key(&t.name), t.id.0);
        tags.sort_by_key(key);
        if let Some(a) = after {
            tags.retain(|t| key(t) > a);
        }
        let limit = page.limit() as usize;
        let more = tags.len() > limit;
        tags.truncate(limit);
        let next_cursor = if more {
            tags.last()
                .map(|t| node_rules::key_cursor(&rules::name_key(&t.name), t.id.0))
        } else {
            None
        };
        Ok(Page {
            items: tags,
            next_cursor,
        })
    }

    async fn create(
        &self,
        ctx: &Context,
        name: String,
        color: Option<String>,
    ) -> Result<Written<Tag>> {
        let name = rules::validate_name(&name)?;
        let color = rules::validate_color(color)?;
        self.caller(ctx).await?.check_write(ctx)?;
        let mut st = self.st();
        let key = rules::name_key(&name);
        if st
            .tags
            .values()
            .any(|t| t.tenant_id == ctx.tenant && rules::name_key(&t.name) == key)
        {
            return Err(rules::name_taken(&name));
        }
        let tag = Tag {
            id: TagId::new(),
            tenant_id: ctx.tenant,
            name,
            color,
            created_by: ctx.actor,
            created_at: crate::now(),
        };
        st.tags.insert(tag.id, tag.clone());
        Ok(Written {
            value: tag,
            change: no_change(ctx),
        })
    }

    async fn get(&self, ctx: &Context, id: TagId) -> Result<Tag> {
        Ok(self.visible(ctx, id).await?.0)
    }

    async fn update(&self, ctx: &Context, id: TagId, input: TagInput) -> Result<Written<Tag>> {
        let name = input
            .name
            .as_deref()
            .map(rules::validate_name)
            .transpose()?;
        let color = input.color.map(rules::validate_color).transpose()?;
        let (_, caller) = self.visible(ctx, id).await?;
        caller.check_write(ctx)?;
        let mut st = self.st();
        if let Some(n) = &name {
            let key = rules::name_key(n);
            if st
                .tags
                .values()
                .any(|t| t.tenant_id == ctx.tenant && t.id != id && rules::name_key(&t.name) == key)
            {
                return Err(rules::name_taken(n));
            }
        }
        let tag = st
            .tags
            .get_mut(&id)
            .filter(|t| t.tenant_id == ctx.tenant)
            .ok_or_else(rules::tag_not_found)?;
        if let Some(n) = name {
            tag.name = n;
        }
        if let Some(c) = color {
            tag.color = c;
        }
        Ok(Written {
            value: tag.clone(),
            change: no_change(ctx),
        })
    }

    async fn delete(&self, ctx: &Context, id: TagId) -> Result<()> {
        let (tag, caller) = self.visible(ctx, id).await?;
        caller.check_delete(ctx, &tag)?;
        let mut st = self.st();
        let nodes: Vec<NodeId> = st
            .links
            .iter()
            .filter(|(t, g, _)| *t == ctx.tenant && *g == id)
            .map(|(_, _, n)| *n)
            .collect();
        for n in &nodes {
            st.links.remove(&(ctx.tenant, id, *n));
            if let Some(node) = self.nodes.live_node(ctx.tenant, *n) {
                self.append(ctx, &node, EventType::TagDetached, &tag)?;
            }
        }
        st.tags.remove(&id);
        Ok(())
    }

    async fn attach(&self, ctx: &Context, node: NodeId, tag: TagId) -> Result<Written<()>> {
        authz::require(&*self.authz, &*self.nodes, ctx, node, Role::Edit).await?;
        let n = self
            .nodes
            .live_node(ctx.tenant, node)
            .ok_or_else(node_rules::node_not_found)?;
        let tag = self.tag_of(ctx.tenant, tag)?;
        let mut st = self.st();
        if !st.links.insert((ctx.tenant, tag.id, node)) {
            return Ok(Written {
                value: (),
                change: no_change(ctx),
            });
        }
        let change = self.append(ctx, &n, EventType::TagAttached, &tag)?;
        Ok(Written { value: (), change })
    }

    async fn detach(&self, ctx: &Context, node: NodeId, tag: TagId) -> Result<Written<()>> {
        authz::require(&*self.authz, &*self.nodes, ctx, node, Role::Edit).await?;
        let n = self
            .nodes
            .live_node(ctx.tenant, node)
            .ok_or_else(node_rules::node_not_found)?;
        let tag = self.tag_of(ctx.tenant, tag)?;
        let mut st = self.st();
        if !st.links.remove(&(ctx.tenant, tag.id, node)) {
            return Ok(Written {
                value: (),
                change: no_change(ctx),
            });
        }
        let change = self.append(ctx, &n, EventType::TagDetached, &tag)?;
        Ok(Written { value: (), change })
    }

    async fn tags_of(&self, ctx: &Context, node: NodeId) -> Result<Vec<Tag>> {
        authz::require(&*self.authz, &*self.nodes, ctx, node, Role::View).await?;
        self.nodes
            .live_node(ctx.tenant, node)
            .ok_or_else(node_rules::node_not_found)?;
        let st = self.st();
        let mut tags: Vec<Tag> = st
            .links
            .iter()
            .filter(|(t, _, n)| *t == ctx.tenant && *n == node)
            .filter_map(|(_, g, _)| st.tags.get(g).cloned())
            .collect();
        tags.sort_by_key(|t| (rules::name_key(&t.name), t.id.0));
        Ok(tags)
    }

    async fn nodes_with(&self, ctx: &Context, tag: TagId, page: PageRequest) -> Result<Page<Node>> {
        let after = page
            .cursor
            .as_ref()
            .map(node_rules::parse_key_cursor)
            .transpose()?;
        self.visible(ctx, tag).await?;
        let mut ids: Vec<NodeId> = self.tagged(ctx.tenant, tag).into_iter().collect();
        ids.sort();
        if let Some((_, a)) = after {
            ids.retain(|n| n.0 > a);
        }
        let limit = page.limit() as usize;
        let more = ids.len() > limit;
        ids.truncate(limit);
        let next_cursor = if more {
            ids.last().map(|n| node_rules::key_cursor("", n.0))
        } else {
            None
        };
        let nodes: Vec<Node> = ids
            .iter()
            .filter_map(|n| self.nodes.live_node(ctx.tenant, *n))
            .collect();
        let items = self.nodes.present(ctx, nodes, Some(Role::View)).await?;
        Ok(Page { items, next_cursor })
    }
}
