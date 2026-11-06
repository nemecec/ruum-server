//! The changes feed (`GET /v1/changes`, `GET /v1/changes/latest`).
//!
//! The feed reads the journal of the caller's tenant and keeps the entries that the caller may
//! see. Visibility is checked against the state before and after each change:
//!
//! - after: the caller can see the node now (for agents: inside the scope, too; a trashed node
//!   only with `manage`, as `NodeStore::get`);
//! - before: the caller could see the node's old place (`old_parent_id` of a move or a restore,
//!   `parent_id` of a rename or a trash, the last known parent of a purged node), or the caller
//!   was the grantee of a removed or changed grant. For a trash or a purge, also: the caller had
//!   a grant (from the journal: added and not removed before the change) on the node or on one of
//!   its ancestors as the journal knows them (`parent_id` entries), and for an agent the node was
//!   inside the scope.
//!
//! An entry is sent when either holds. A tenant-level entry (`node_id` `null`) is sent to members
//! only (not to guests).
//!
//! When a change alters what the caller sees of a whole subtree, the page also carries a
//! [`ResyncMarker`] for the caller: a grant to the caller (directly or through
//! `tenant_members`) that is added or removed, a member link added, restored, trashed or purged
//! (on the link target), a move across the edge of what the caller sees, and a restore.
//!
//! "Now" is the state at the time of the read, not the state right after the change. This is the
//! step 1 rule; a client that follows the feed sees each change close to when it happens.

use super::{
    Access, EventType, GRANT_EVENTS, Journal, JournalEntry, ResyncMarker, decode_cursor,
    encode_cursor, parent_before,
};
use crate::actor::{Context, PrincipalKind};
use crate::error::{Error, ErrorCode, Result};
use crate::ids::NodeId;
use crate::paging::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use crate::services::Services;
use crate::tenants::MemberRole;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// How many ancestors the feed follows up from a trashed or purged node through the journal.
const MAX_GONE_DEPTH: usize = 64;

/// The longest long poll (`wait`) of the contract, in seconds.
pub const MAX_WAIT_SECONDS: u64 = 30;

/// One call of the changes feed.
#[derive(Clone, Debug, Default)]
pub struct ChangesQuery {
    /// The position to read after (from the client's cursor). `None` starts at the oldest kept
    /// entry.
    pub after: Option<i64>,
    /// The page size; clamped to `1..=MAX_PAGE_SIZE`.
    pub limit: Option<u32>,
    /// How long to wait for new entries when there is nothing to send (long poll). Clamped to
    /// [`MAX_WAIT_SECONDS`].
    pub wait: Duration,
    /// Only changes inside this folder, at any depth.
    pub node: Option<NodeId>,
}

impl ChangesQuery {
    /// The query from the wire parameters `cursor`, `limit`, `wait` and `node`.
    pub fn from_params(
        cursor: Option<&str>,
        limit: Option<u32>,
        wait_seconds: Option<u64>,
        node: Option<NodeId>,
    ) -> Result<Self> {
        Ok(Self {
            after: cursor
                .filter(|c| !c.is_empty())
                .map(decode_cursor)
                .transpose()?,
            limit,
            wait: Duration::from_secs(wait_seconds.unwrap_or(0).min(MAX_WAIT_SECONDS)),
            node,
        })
    }

    fn limit(&self) -> u32 {
        self.limit
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE)
    }
}

/// One page of the changes feed (`ChangesPage` in `openapi.yaml`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangesPage {
    pub items: Vec<JournalEntry>,
    pub resync: Vec<ResyncMarker>,
    /// Always set; the client passes it to the next call.
    pub next_cursor: String,
    pub has_more: bool,
}

/// The newest cursor (`GET /v1/changes/latest`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LatestCursor {
    pub cursor: String,
    pub seq: i64,
}

/// What the feed needs to know about the caller. [`ServicesAccess`] answers from the core
/// services; tests use a fake.
#[async_trait]
pub trait FeedAccess: Send + Sync {
    /// Whether the caller can see the node now (after the scope).
    async fn can_view(&self, node: NodeId) -> Result<bool>;
    /// The ancestors of the node, root first, or `None` when the node no longer exists.
    async fn ancestors(&self, node: NodeId) -> Result<Option<Vec<NodeId>>>;
    /// Whether the caller's user is a member of the tenant (not a guest).
    async fn is_member(&self) -> Result<bool>;
}

/// [`FeedAccess`] from the core services: `NodeStore::get` for visibility (with the scope and the
/// trash rule),
/// `NodeStore::ancestors` and `TenantStore::role_of`.
pub struct ServicesAccess<'a> {
    pub services: &'a Services,
    pub ctx: &'a Context,
}

fn is_hidden(e: &Error) -> bool {
    matches!(
        e,
        Error::Api(a) if matches!(
            a.code,
            ErrorCode::NotFound | ErrorCode::PermissionDenied | ErrorCode::ScopeExceeded
        )
    )
}

#[async_trait]
impl FeedAccess for ServicesAccess<'_> {
    async fn can_view(&self, node: NodeId) -> Result<bool> {
        // `NodeStore::get` checks `view` with the scope, and the trash rule of the nodes area: a
        // trashed node has left the tree, and only those who may restore it (`manage`) still
        // see it. The trash entry itself still reaches the others through its old place
        // ("before").
        match self.services.nodes.get(self.ctx, node).await {
            Ok(_) => Ok(true),
            Err(e) if is_hidden(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn ancestors(&self, node: NodeId) -> Result<Option<Vec<NodeId>>> {
        match self.services.nodes.ancestors(self.ctx, node).await {
            Ok(a) => Ok(Some(a)),
            Err(Error::Api(a)) if a.code == ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn is_member(&self) -> Result<bool> {
        let Some(user) = self.ctx.actor.effective_user() else {
            return Ok(false);
        };
        let role = self.services.tenants.role_of(self.ctx.tenant, user).await?;
        Ok(role.is_some_and(|r| r >= MemberRole::Member))
    }
}

/// Share-link bearers cannot follow the feed (`user_required`, `contracts/openapi.yaml`, section
/// "Users only"); users and agents can.
pub fn require_feed_caller(ctx: &Context) -> Result<()> {
    if ctx.actor.actor.kind == PrincipalKind::ShareLink {
        return Err(Error::user_required());
    }
    Ok(())
}

/// The newest cursor of the tenant.
pub async fn latest_cursor(journal: &dyn Journal, ctx: &Context) -> Result<LatestCursor> {
    require_feed_caller(ctx)?;
    let seq = journal.latest_seq(ctx).await?;
    Ok(LatestCursor {
        cursor: encode_cursor(seq).0,
        seq,
    })
}

/// Memoized answers of [`FeedAccess`] for one call.
struct Memo<'a> {
    access: &'a dyn FeedAccess,
    journal: &'a dyn Journal,
    ctx: &'a Context,
    view: HashMap<NodeId, bool>,
    ancestors: HashMap<NodeId, Option<Vec<NodeId>>>,
    member: Option<bool>,
}

impl Memo<'_> {
    async fn can_view(&mut self, node: NodeId) -> Result<bool> {
        if let Some(v) = self.view.get(&node) {
            return Ok(*v);
        }
        let v = self.access.can_view(node).await?;
        self.view.insert(node, v);
        Ok(v)
    }

    async fn can_view_opt(&mut self, node: Option<NodeId>) -> Result<bool> {
        match node {
            Some(n) => self.can_view(n).await,
            None => Ok(false),
        }
    }

    async fn is_member(&mut self) -> Result<bool> {
        if let Some(m) = self.member {
            return Ok(m);
        }
        let m = self.access.is_member().await?;
        self.member = Some(m);
        Ok(m)
    }

    /// Whether `node` is `folder` or inside it.
    async fn inside(&mut self, node: NodeId, folder: NodeId) -> Result<bool> {
        if node == folder {
            return Ok(true);
        }
        if !self.ancestors.contains_key(&node) {
            let a = self.access.ancestors(node).await?;
            self.ancestors.insert(node, a);
        }
        Ok(self.ancestors[&node]
            .as_ref()
            .is_some_and(|a| a.contains(&folder)))
    }

    /// Whether the grantee in the entry data is the caller: its user, or all members when the
    /// caller is a member. Email grantees are never matched here (the caller's verified emails
    /// bind through the sharing area).
    async fn grantee_is_caller(&mut self, entry: &JournalEntry) -> Result<bool> {
        let grantee = &entry.data["grantee"];
        match grantee["type"].as_str() {
            Some("user") => Ok(self
                .ctx
                .actor
                .effective_user()
                .is_some_and(|u| grantee["id"].as_str() == Some(u.0.to_string().as_str()))),
            Some("tenant_members") => self.is_member().await,
            _ => Ok(false),
        }
    }

    /// The parent before the change; for a purged node, the last known parent.
    async fn old_parent(&mut self, entry: &JournalEntry) -> Result<Option<NodeId>> {
        match (entry.event, entry.node_id) {
            (EventType::NodePurged, Some(n)) => {
                self.journal.last_parent(self.ctx, n, entry.seq).await
            }
            _ => Ok(parent_before(entry)),
        }
    }

    /// Whether the caller had a grant on `node` before `seq`, from the grant entries of the
    /// journal: a grant whose newest entry is `grant.added` or `grant.updated` with the caller
    /// as grantee.
    async fn had_grant(&mut self, node: NodeId, seq: i64) -> Result<bool> {
        let entries = self
            .journal
            .node_entries(self.ctx, node, seq, GRANT_EVENTS, u32::MAX)
            .await?;
        // Newest first: the first entry of each grant is its state before `seq`.
        let mut seen = HashSet::new();
        for e in &entries {
            let id = e.data["grant_id"].as_str().unwrap_or_default().to_string();
            if !seen.insert(id) || e.event == EventType::GrantRemoved {
                continue;
            }
            if self.grantee_is_caller(e).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether the caller could see a trashed or purged node before the change, beyond its old
    /// place: through a grant on the node or on an ancestor (also one that is now gone or in
    /// the trash), or through an ancestor it can still see. The ancestors come from the journal
    /// (`parent_id` of the entries), up to [`MAX_GONE_DEPTH`].
    async fn saw_gone_node(&mut self, node: NodeId, seq: i64) -> Result<bool> {
        let mut chain = vec![node];
        let mut granted = false;
        let mut cur = node;
        for _ in 0..MAX_GONE_DEPTH {
            if !granted && self.had_grant(cur, seq).await? {
                granted = true;
            }
            if cur != node && self.can_view(cur).await? {
                return Ok(true);
            }
            match self.journal.last_parent(self.ctx, cur, seq).await? {
                Some(p) if !chain.contains(&p) => {
                    chain.push(p);
                    cur = p;
                }
                _ => break,
            }
        }
        if !granted {
            return Ok(false);
        }
        // An agent (or another scoped principal) sees only inside its subtree.
        Ok(match self.ctx.scope.as_ref().and_then(|s| s.subtree) {
            Some(root) => chain.contains(&root),
            None => true,
        })
    }

    /// The target of a member link, from the data of its `node.created` entry (the entry
    /// itself, or the newest one on the node before it).
    async fn member_link_target(&mut self, entry: &JournalEntry) -> Result<Option<NodeId>> {
        let from_data = |e: &JournalEntry| {
            (e.data["link_kind"].as_str() == Some("member"))
                .then(|| e.data["link_target"].as_str())
                .flatten()
                .and_then(|s| s.parse::<NodeId>().ok())
        };
        let Some(node) = entry.node_id else {
            return Ok(None);
        };
        if entry.event == EventType::NodeCreated {
            return Ok(from_data(entry));
        }
        let created = self
            .journal
            .node_entries(self.ctx, node, entry.seq, &[EventType::NodeCreated], 1)
            .await?;
        Ok(created.first().and_then(from_data))
    }

    /// Whether the entry concerns the folder `f` or a node inside it.
    async fn in_folder(&mut self, entry: &JournalEntry, f: NodeId) -> Result<bool> {
        let mut candidates: Vec<NodeId> = entry.node_id.into_iter().collect();
        candidates.extend(super::parent_after(entry));
        candidates.extend(self.old_parent(entry).await?);
        for c in candidates {
            if self.inside(c, f).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether the caller sees the entry, and the caller's resync markers for it.
    async fn classify(
        &mut self,
        entry: &JournalEntry,
        markers: &mut Vec<ResyncMarker>,
    ) -> Result<bool> {
        let Some(node) = entry.node_id else {
            return self.is_member().await;
        };
        let marker = |node_id, access| ResyncMarker {
            seq: entry.seq,
            node_id,
            access,
        };
        let now = match entry.event {
            EventType::NodePurged => false,
            _ => self.can_view(node).await?,
        };
        let old_parent = self.old_parent(entry).await?;
        let before_place = self.can_view_opt(old_parent).await?;
        let mut before = before_place;
        match entry.event {
            EventType::GrantAdded | EventType::GrantUpdated => {
                if now && self.grantee_is_caller(entry).await? {
                    markers.push(marker(node, Access::Gained));
                }
                if entry.event == EventType::GrantUpdated {
                    before = before || self.grantee_is_caller(entry).await?;
                }
            }
            EventType::GrantRemoved => {
                let was_grantee = self.grantee_is_caller(entry).await?;
                before = before || was_grantee;
                if was_grantee && !now {
                    markers.push(marker(node, Access::Lost));
                }
            }
            EventType::NodeMoved => {
                if now && !before_place {
                    markers.push(marker(node, Access::Gained));
                } else if before_place && !now {
                    markers.push(marker(node, Access::Lost));
                }
            }
            EventType::NodeRestored => {
                if now {
                    markers.push(marker(node, Access::Gained));
                    if let Some(target) = self.member_link_target(entry).await?
                        && self.can_view(target).await?
                    {
                        markers.push(marker(target, Access::Gained));
                    }
                }
            }
            EventType::NodeCreated => {
                if now
                    && let Some(target) = self.member_link_target(entry).await?
                    && self.can_view(target).await?
                {
                    markers.push(marker(target, Access::Gained));
                }
            }
            EventType::NodeTrashed | EventType::NodePurged => {
                if !now && !before {
                    before = self.saw_gone_node(node, entry.seq).await?;
                }
                if (now || before)
                    && let Some(target) = self.member_link_target(entry).await?
                    && !self.can_view(target).await?
                {
                    markers.push(marker(target, Access::Lost));
                }
            }
            _ => {}
        }
        Ok(now || before)
    }
}

/// One call of the changes feed for the caller in `ctx`.
///
/// Reads one page of the journal after the cursor and keeps what the caller may see. When nothing
/// is left to send and `wait` is set, waits for new entries (long poll) and reads again, until
/// something is sent or the time is up. `next_cursor` is always set, also when `items` is empty,
/// and moves past entries the caller may not see.
pub async fn list_changes(
    journal: &dyn Journal,
    access: &dyn FeedAccess,
    ctx: &Context,
    query: ChangesQuery,
) -> Result<ChangesPage> {
    require_feed_caller(ctx)?;
    let mut memo = Memo {
        access,
        journal,
        ctx,
        view: HashMap::new(),
        ancestors: HashMap::new(),
        member: None,
    };
    if let Some(folder) = query.node
        && !memo.can_view(folder).await?
    {
        return Err(Error::not_found("node"));
    }
    let limit = query.limit();
    let deadline = tokio::time::Instant::now() + query.wait;
    let mut after = query.after;
    loop {
        // One more than the page, to know whether more entries are ready.
        let page = journal.read(ctx, after, limit + 1).await?;
        let read_after = match &page.next_cursor {
            Some(c) if page.items.is_empty() => decode_cursor(&c.0)?,
            _ => after.unwrap_or(0),
        };
        let has_more = page.items.len() > limit as usize;
        let mut entries = page.items;
        entries.truncate(limit as usize);
        let position = entries.last().map_or(read_after, |e| e.seq);

        let mut items = Vec::new();
        let mut resync = Vec::new();
        for entry in entries {
            if let Some(f) = query.node
                && !memo.in_folder(&entry, f).await?
            {
                continue;
            }
            let mut markers = Vec::new();
            if memo.classify(&entry, &mut markers).await? {
                items.push(entry);
            }
            resync.extend(markers);
        }

        let now = tokio::time::Instant::now();
        if items.is_empty() && resync.is_empty() && !has_more && now < deadline {
            // Nothing for the caller yet: wait, then read on from `position`. Access may change
            // while waiting, so start with a fresh memo.
            if journal.wait_for(ctx, position, deadline - now).await? {
                after = Some(position);
                memo.view.clear();
                memo.ancestors.clear();
                memo.member = None;
                continue;
            }
        }
        return Ok(ChangesPage {
            items,
            resync,
            next_cursor: encode_cursor(position).0,
            has_more,
        });
    }
}

#[cfg(test)]
mod tests;
