//! The changes feed (`GET /v1/changes`, `GET /v1/changes/latest`).
//!
//! The feed reads the journal of the caller's tenant and keeps the entries that the caller may
//! see. Visibility is checked against the state before and after each change:
//!
//! - after: the caller can see the node now (for agents: inside the scope, too; a trashed node
//!   only with `manage`, as `NodeStore::get`);
//! - before: the caller could see the node's old place (`old_parent_id` of a move or a restore,
//!   `parent_id` of a rename or a trash, the last known parent of a purged node), or the caller
//!   was the grantee of a removed or changed grant.
//!
//! An entry is sent when either holds. A tenant-level entry (`node_id` `null`) is sent to members
//! only (not to guests).
//!
//! The trash (decision 0002-6): a `node.trashed` or `node.purged` entry, and any entry on a node
//! that is in the trash or purged now, is sent only when the caller sees the node now (in the
//! trash: only with `manage`), or could see it just before it went to the trash, or could restore
//! it while it was in the trash. "Could" uses dated access only: grants from the journal, and a
//! membership from `TenantStore::member_since` (members `manage` the team tree). `manage` is
//! capped by an agent's scope, and for an agent the node must be inside its subtree. Seeing the
//! old place now is not enough: access that began after the trash never reveals the node.
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
use crate::authz::Role;
use crate::error::{Error, ErrorCode, Result};
use crate::ids::NodeId;
use crate::paging::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use crate::services::Services;
use crate::tenants::MemberRole;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use time::OffsetDateTime;

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
    /// Since when the caller's user is a member, admin or owner (not a guest), or `None`
    /// (`TenantStore::member_since`). Members see the whole team tree through the team root
    /// (`manage`); the feed counts that access only from this time (decision 0002-6).
    async fn member_since(&self) -> Result<Option<OffsetDateTime>>;
}

/// [`FeedAccess`] from the core services: `NodeStore::get` for visibility (with the scope and the
/// trash rule),
/// `NodeStore::ancestors`, `TenantStore::role_of` and `TenantStore::member_since`.
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
        // see it. Who else learns of a trash follows decision 0002-6 (`Memo::may_learn`).
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

    async fn member_since(&self) -> Result<Option<OffsetDateTime>> {
        let Some(user) = self.ctx.actor.effective_user() else {
            return Ok(None);
        };
        self.services
            .tenants
            .member_since(self.ctx.tenant, user)
            .await
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
    member_since: Option<Option<OffsetDateTime>>,
    gone: HashMap<NodeId, Option<Gone>>,
}

/// The time a node spent in the trash, from the journal: the `node.trashed` entry that took it
/// out of the tree (its own, or the one of an ancestor), and the entry that ended it (a restore
/// or a purge), `None` while it is still in the trash.
#[derive(Clone)]
struct Gone {
    trashed: JournalEntry,
    ended: Option<JournalEntry>,
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

    async fn member_since(&mut self) -> Result<Option<OffsetDateTime>> {
        if let Some(m) = self.member_since {
            return Ok(m);
        }
        let m = self.access.member_since().await?;
        self.member_since = Some(m);
        Ok(m)
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

    /// The caller's grants on `node` in the window `since..before`, from the grant entries of
    /// the journal: for each grant whose state is `grant.added` or `grant.updated` with the
    /// caller's user as grantee, the `seq` of the entry that set the state, whether that state
    /// held already at `since` (the grant's newest entry before `since`) or began inside the
    /// window, and its role. A grant that was removed
    /// has no state. `tenant_members` grants are left out: the membership itself gives more
    /// (`manage` from the team root), dated by [`FeedAccess::member_since`].
    async fn grant_states(
        &mut self,
        node: NodeId,
        since: i64,
        before: i64,
    ) -> Result<Vec<(i64, bool, Role)>> {
        let entries = self
            .journal
            .node_entries(self.ctx, node, before, GRANT_EVENTS, u32::MAX)
            .await?;
        let user = self.ctx.actor.effective_user().map(|u| u.0.to_string());
        // Newest first: the first entry of a grant before `since` is its state at `since`;
        // older ones do not count.
        let mut settled = HashSet::new();
        let mut out = Vec::new();
        for e in &entries {
            let id = e.data["grant_id"].as_str().unwrap_or_default().to_string();
            if settled.contains(&id) {
                continue;
            }
            let at_since = e.seq < since;
            if at_since {
                settled.insert(id);
            }
            let grantee = &e.data["grantee"];
            let mine = grantee["type"].as_str() == Some("user")
                && user.is_some()
                && grantee["id"].as_str() == user.as_deref();
            if e.event == EventType::GrantRemoved || !mine {
                continue;
            }
            // An unknown role counts as the lowest one.
            let role = serde_json::from_value(e.data["role"].clone()).unwrap_or(Role::View);
            out.push((e.seq, at_since, role));
        }
        Ok(out)
    }

    /// The ancestors of a node as the journal knows them before `seq` (`parent_id` of the
    /// entries), the node first, up to [`MAX_GONE_DEPTH`].
    async fn gone_chain(&mut self, node: NodeId, seq: i64) -> Result<Vec<NodeId>> {
        let mut chain = vec![node];
        let mut cur = node;
        while chain.len() < MAX_GONE_DEPTH {
            match self.journal.last_parent(self.ctx, cur, seq).await? {
                Some(p) if !chain.contains(&p) => {
                    chain.push(p);
                    cur = p;
                }
                _ => break,
            }
        }
        Ok(chain)
    }

    /// Whether a chain from [`Self::gone_chain`] is inside the scope subtree of an agent (or
    /// another scoped principal); always true without a subtree.
    fn chain_in_scope(&self, chain: &[NodeId]) -> bool {
        match self.ctx.scope.as_ref().and_then(|s| s.subtree) {
            Some(root) => chain.contains(&root),
            None => true,
        }
    }

    /// The trash time of a trash or purge entry: for a trash, until the next restore or purge
    /// of the node, or a purge of an ancestor; for a purge, from the trash before it (the purge itself when the journal has
    /// none) until the purge.
    async fn window_of(&mut self, entry: &JournalEntry, node: NodeId) -> Result<Gone> {
        const ENDS: &[EventType] = &[EventType::NodeRestored, EventType::NodePurged];
        Ok(match entry.event {
            EventType::NodePurged => {
                let trashed = self
                    .journal
                    .node_entries(self.ctx, node, entry.seq, &[EventType::NodeTrashed], 1)
                    .await?
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| entry.clone());
                Gone {
                    trashed,
                    ended: Some(entry.clone()),
                }
            }
            _ => {
                let later = self
                    .journal
                    .node_entries(self.ctx, node, i64::MAX, ENDS, u32::MAX)
                    .await?;
                // Newest first: the last one after the trash is the first after it.
                let mut ended = later.into_iter().rfind(|e| e.seq > entry.seq);
                // A purge of an ancestor ends the trash time too.
                let chain = self.gone_chain(node, i64::MAX).await?;
                for x in &chain[1..] {
                    let purges = self
                        .journal
                        .node_entries(self.ctx, *x, i64::MAX, &[EventType::NodePurged], 1)
                        .await?;
                    if let Some(p) = purges.into_iter().next()
                        && p.seq > entry.seq
                        && ended.as_ref().is_none_or(|e| p.seq < e.seq)
                    {
                        ended = Some(p);
                    }
                }
                Gone {
                    trashed: entry.clone(),
                    ended,
                }
            }
        })
    }

    /// Whether the node is out of the tree now (in the trash, itself or below a trashed folder,
    /// or purged), from the journal, and since which trash. The node itself and each ancestor as
    /// the journal knows them count; the earliest trash that has not ended took the node out.
    async fn gone_now(&mut self, node: NodeId) -> Result<Option<Gone>> {
        if let Some(g) = self.gone.get(&node) {
            return Ok(g.clone());
        }
        const STATES: &[EventType] = &[
            EventType::NodeTrashed,
            EventType::NodeRestored,
            EventType::NodePurged,
        ];
        let chain = self.gone_chain(node, i64::MAX).await?;
        let mut out: Option<Gone> = None;
        for x in chain {
            let newest = self
                .journal
                .node_entries(self.ctx, x, i64::MAX, STATES, 1)
                .await?;
            let g = match newest.first() {
                Some(e) if e.event == EventType::NodeTrashed => Gone {
                    trashed: e.clone(),
                    ended: None,
                },
                Some(e) if e.event == EventType::NodePurged => self.window_of(e, x).await?,
                _ => continue,
            };
            out = Some(match out {
                // The earliest trash, and the earliest end (a purge above ends it too).
                Some(o) => Gone {
                    trashed: if g.trashed.seq < o.trashed.seq {
                        g.trashed
                    } else {
                        o.trashed
                    },
                    ended: match (o.ended, g.ended) {
                        (Some(a), Some(b)) => Some(if a.seq < b.seq { a } else { b }),
                        (a, b) => a.or(b),
                    },
                },
                None => g,
            });
        }
        self.gone.insert(node, out.clone());
        Ok(out)
    }

    /// Decision 0002-6: whether the caller may learn about a node for the trash time `g`. Either
    /// it could see the node just before the trash, or it could restore the node while it was in
    /// the trash. Access that began after the trash never counts.
    ///
    /// - Just before the trash: a grant to the caller's user (any role) on the node or on an
    ///   ancestor at that moment, or a membership that began before the trash. For an agent the
    ///   node must be inside its scope subtree then.
    /// - In the trash: `manage` from a grant on the node or an ancestor, or from a membership
    ///   (the team root), at some moment before the end of the trash time; for an agent only
    ///   when its scope allows `manage` and the node is inside its subtree.
    ///
    /// Member links do not count here: the journal does not date their access.
    async fn may_learn(&mut self, node: NodeId, g: &Gone) -> Result<bool> {
        let t = &g.trashed;
        let member_since = self.member_since().await?;

        let chain_then = self.gone_chain(node, t.seq).await?;
        if self.chain_in_scope(&chain_then) {
            if member_since.is_some_and(|m| m <= t.at) {
                return Ok(true);
            }
            for cur in &chain_then {
                if !self.grant_states(*cur, t.seq, t.seq).await?.is_empty() {
                    return Ok(true);
                }
            }
        }

        let cap = self.ctx.scope.as_ref().map_or(Role::Manage, |s| s.max_role);
        if cap < Role::Manage {
            return Ok(false);
        }
        let end = g.ended.as_ref().map_or(i64::MAX, |e| e.seq);
        let chain_end = self.gone_chain(node, end).await?;
        // A member until the end (or now) manages the whole team tree; the scope must hold the
        // node at the end. (At the trash itself, (a) above already answered.)
        let member_in_time = match (member_since, &g.ended) {
            (Some(m), Some(e)) => m <= e.at,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if member_in_time && self.chain_in_scope(&chain_end) {
            return Ok(true);
        }
        // A `manage` grant counts at the moment it held: the grant's node must be an ancestor
        // of the node then, and for an agent the node inside its subtree then.
        let mut candidates = chain_end.clone();
        candidates.extend(chain_then.iter().filter(|c| !chain_end.contains(c)));
        for cur in candidates {
            let states = self.grant_states(cur, t.seq, end).await?;
            for (seq, at_since, role) in states {
                if role < Role::Manage {
                    continue;
                }
                let chain_at = if at_since {
                    chain_then.clone()
                } else {
                    self.gone_chain(node, seq + 1).await?
                };
                if chain_at.contains(&cur) && self.chain_in_scope(&chain_at) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Whether the caller held the grant of a grant entry on `node`: it is the grantee, and for
    /// an agent (or another scoped principal) the node is inside the scope subtree. The user's
    /// grant outside the subtree is not the agent's business (design 3.3).
    async fn held_grant(&mut self, entry: &JournalEntry, node: NodeId) -> Result<bool> {
        if !self.grantee_is_caller(entry).await? {
            return Ok(false);
        }
        let Some(root) = self.ctx.scope.as_ref().and_then(|s| s.subtree) else {
            return Ok(true);
        };
        if self.inside(node, root).await? {
            return Ok(true);
        }
        // A node that is gone: its ancestors as the journal knows them.
        let mut cur = node;
        let mut chain = vec![node];
        for _ in 0..MAX_GONE_DEPTH {
            match self.journal.last_parent(self.ctx, cur, entry.seq).await? {
                Some(p) if p == root => return Ok(true),
                Some(p) if !chain.contains(&p) => {
                    chain.push(p);
                    cur = p;
                }
                _ => break,
            }
        }
        Ok(false)
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
                    before = before || self.held_grant(entry, node).await?;
                }
            }
            EventType::GrantRemoved => {
                let was_grantee = self.held_grant(entry, node).await?;
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
                // Decision 0002-6: a trash or a purge reaches only those who could see the node
                // just before the trash or could restore it, whatever they see now of its old
                // place.
                if !now {
                    let g = self.window_of(entry, node).await?;
                    before = self.may_learn(node, &g).await?;
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
        // Decision 0002-6: any other entry on a node that is out of the tree now (in the trash
        // or purged) follows the same rule. "Now" stays enough: a node in the trash shows now
        // only to those who may restore it.
        if !now
            && before
            && !matches!(entry.event, EventType::NodeTrashed | EventType::NodePurged)
            && let Some(g) = self.gone_now(node).await?
            && !self.may_learn(node, &g).await?
        {
            markers.clear();
            return Ok(false);
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
        member_since: None,
        gone: HashMap::new(),
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
                memo.member_since = None;
                memo.gone.clear();
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
