//! Time-limited grants of agents on a database (design section 8.5; card A-1):
//! `createDatabaseAccessGrant`, `listDatabaseAccessGrants` and `revokeDatabaseAccessGrant`, the
//! SpiceDB tuples, the sweeper, and the restore point before a `write_live` grant.
//!
//! A grant is a row of `database_access_grants` and a SpiceDB tuple on the database node:
//! `live_writer` for `write_live`, `personal_reader` for `read_personal`, with the agent as the
//! subject (`contracts/authz/schema.zed`). The row is the truth; the tuple follows it through the
//! outbox ([`crate::authz::outbox`]).
//!
//! The rules:
//!
//! - `createDatabaseAccessGrant` ([`DatabaseService::create_grant`]): users only (`403
//!   user_required` for agents and share links); `view` on the database (else `404`); only the
//!   user the agent acts for (`403`, also for an unknown or disabled agent, so the answer tells
//!   nothing about other people's agents); at most 24 hours for `write_live` and 8 hours for
//!   `read_personal` (`422`); `write_live` needs `edit` of the user and an agent scope of
//!   `edit` or higher (`403`); the database must be inside the agent's scope subtree (`403`),
//!   because a grant outside the scope gives nothing. There is no kind for `secret`: secret
//!   values are never granted. For `write_live` the engine first records a restore point
//!   ([`ruum_dbengine::DatabaseEngine::create_restore_point`]); then ONE transaction writes the
//!   `database_restore_points` row (reason `before_write_grant`, actor, `on_behalf_of`), the
//!   grant row with its `restore_point_id`, the tuple through the outbox and the journal entry
//!   `database.grant_created`.
//! - `listDatabaseAccessGrants` ([`DatabaseService::list_grants`]): `view`; an agent sees only
//!   its own grants. The names of the agent and the giver are filled (decision 0004-35 c).
//! - `revokeDatabaseAccessGrant` ([`DatabaseService::revoke_grant`]): the user who gave it, a
//!   person with `manage`, or the agent that holds it (it gives the grant back). It sets
//!   `revoked_*`, writes `database.grant_revoked`, and, when no other live grant of the same
//!   agent and kind holds the tuple, deletes the tuple through the outbox and enqueues the
//!   provider delivery `access_changed` for the database node. A grant that has ended already
//!   is `404 not_found`, as a revoked link is for `revokeShareLink` (the contract lists no
//!   `409`). On Postgres, the writes of one tuple `(tenant, node, agent, kind)` are serialized
//!   (an advisory lock), so the tuple follows the rows also under concurrent writes.
//!
//! The check for later cards is [`active`] (and [`DatabaseService::active_grant`]): a grant
//! counts only when its row is live (`starts_at <= now < expires_at`, `revoked_at` null) AND the
//! tuple holds (`agent_write_live` / `agent_read_personal`). So an expired or revoked grant gives
//! nothing even before the sweeper deletes its tuple. Q-2, A-2 and D-2 call it.
//!
//! The sweeper ([`DatabaseService::sweep_grants_at`], [`run_sweeper`] on the server) ends the
//! grants whose time ran out: `database.grant_revoked` with cause `expired` (actor: the user who
//! gave the grant, as nobody else acted), and, when no other live grant holds the tuple, the
//! tuple delete through the outbox and `access_changed`. The journal entry is the mark that a
//! grant was swept, so a second run does nothing (idempotent); it looks back
//! [`SWEEP_WINDOW_DAYS`] days.
//!
//! [`grant_prompt_url`] builds the web link of the prefilled grant prompt, the `hint` of `403
//! write_grant_required` (decision 0004-20, C-16; UX DB-8). Q-2 and D-2 use it.

use super::super::{DatabaseRow, DatabaseService, in_trash, refuse_share_link};
use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::agents::AgentStore;
use crate::authz::{self, Authz, ObjectRef, Relationship, RelationshipUpdate, Role, SubjectRef};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{AgentId, NodeId, TenantId, UserId};
use crate::journal::{ChangeInfo, EventType, NewEntry, Written};
use crate::paging::{Cursor, Page, PageRequest};
use crate::providers::store::NewDelivery;
use crate::providers::{DeliveryId, HookDelivery, HookKind};
use async_trait::async_trait;
use ruum_dbengine::RestorePointMark;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

/// The longest `write_live` grant, in minutes (24 hours).
pub const WRITE_LIVE_MAX_MINUTES: u32 = 1440;
/// The longest `read_personal` grant, in minutes (8 hours).
pub const READ_PERSONAL_MAX_MINUTES: u32 = 480;
/// How far back the sweeper looks for grants whose time ran out.
pub const SWEEP_WINDOW_DAYS: i64 = 7;
/// How many grants one sweeper round ends at most.
pub const SWEEP_LIMIT: usize = 500;
/// How often the server's sweeper runs.
pub const SWEEP_EVERY: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------- the model

/// `DatabaseAccessGrantKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    /// Writes to the live database.
    WriteLive,
    /// Real `personal` values.
    ReadPersonal,
}

impl GrantKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GrantKind::WriteLive => "write_live",
            GrantKind::ReadPersonal => "read_personal",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "write_live" => Ok(GrantKind::WriteLive),
            "read_personal" => Ok(GrantKind::ReadPersonal),
            other => Err(Error::Internal(format!("bad grant kind {other}"))),
        }
    }

    /// The longest grant of this kind, in minutes.
    pub fn max_minutes(self) -> u32 {
        match self {
            GrantKind::WriteLive => WRITE_LIVE_MAX_MINUTES,
            GrantKind::ReadPersonal => READ_PERSONAL_MAX_MINUTES,
        }
    }

    /// The relation on the database node that a grant of this kind writes.
    pub fn relation(self) -> &'static str {
        match self {
            GrantKind::WriteLive => "live_writer",
            GrantKind::ReadPersonal => "personal_reader",
        }
    }

    /// The permission on the database node that checks a grant of this kind.
    pub fn permission(self) -> &'static str {
        match self {
            GrantKind::WriteLive => "agent_write_live",
            GrantKind::ReadPersonal => "agent_read_personal",
        }
    }
}

/// Why a grant ended (`cause` of `database.grant_revoked`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndCause {
    /// A person ended it.
    Revoked,
    /// The agent gave it back.
    GivenBack,
    /// Its time ran out (the sweeper).
    Expired,
}

impl EndCause {
    pub fn as_str(self) -> &'static str {
        match self {
            EndCause::Revoked => "revoked",
            EndCause::GivenBack => "given_back",
            EndCause::Expired => "expired",
        }
    }
}

/// `DatabaseAccessGrant`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DatabaseAccessGrant {
    pub id: Uuid,
    #[serde(skip)]
    pub tenant_id: TenantId,
    pub node_id: NodeId,
    pub kind: GrantKind,
    pub agent_id: AgentId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_display_name: Option<String>,
    /// The user who gave the grant (the agent acts for this user).
    pub granted_by: UserId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted_by_display_name: Option<String>,
    pub reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub starts_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
    pub revoked_by: Option<PrincipalRef>,
    /// The user the revoking principal acted for (`revoked_obo`); not in the contract.
    #[serde(skip)]
    pub revoked_on_behalf_of: Option<UserId>,
    /// `write_live`: the restore point recorded before the grant started.
    pub restore_point_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl DatabaseAccessGrant {
    /// Whether the row is live at `at`: started, not expired, not revoked. The one place of
    /// this rule in memory ([`MemoryGrantStore`]); [`PgGrantStore`] has the same in SQL.
    pub fn is_live(&self, at: OffsetDateTime) -> bool {
        self.revoked_at.is_none() && self.starts_at <= at && at < self.expires_at
    }
}

/// The body of `createDatabaseAccessGrant` (`CreateDatabaseAccessGrantRequest`).
#[derive(Clone, Debug, Deserialize)]
pub struct CreateGrant {
    pub kind: GrantKind,
    pub agent_id: AgentId,
    pub duration_minutes: i64,
    #[serde(default)]
    pub reason: Option<String>,
}

/// A grant to write ([`GrantStore::create`]). The permission checks are done.
#[derive(Clone, Debug)]
pub struct NewGrant {
    pub node: NodeId,
    pub kind: GrantKind,
    pub agent: AgentId,
    pub reason: Option<String>,
    pub starts_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    /// `write_live`: the restore point the engine recorded before (required for that kind).
    pub restore_point: Option<RestorePointMark>,
}

/// A row of `database_restore_points` (D-4 lists them later and keeps this shape).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestorePointRow {
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub node_id: NodeId,
    pub label: Option<String>,
    /// `before_write_grant` for the points of this module.
    pub reason: String,
    pub at: OffsetDateTime,
    pub wal_lsn: Option<String>,
    pub actor: Actor,
}

/// The filter of [`GrantStore::list`].
#[derive(Clone, Copy, Debug, Default)]
pub struct GrantFilter {
    /// Only the grants of this agent (an agent lists its own).
    pub agent: Option<AgentId>,
    /// Only the grants that are live at this time (`active=true`).
    pub live_at: Option<OffsetDateTime>,
}

/// What one sweeper round did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepRound {
    /// Grants marked as ended (`database.grant_revoked`, cause `expired`).
    pub ended: usize,
    /// Tuples deleted (no other live grant of the agent and kind held them).
    pub tuples_deleted: usize,
}

/// Times in Postgres have microseconds; the stores keep the same precision everywhere, so a row
/// reads back equal to what was written.
/// Whether a grant still needs its tuple at `at`: it is not revoked and has not expired. Unlike
/// [`DatabaseAccessGrant::is_live`] it does not ask `starts_at <= at`: a grant starts when it is
/// created, so a grant that starts after `at` was created while the end at `at` waited for the
/// lock of the tuple. It is live now, and its tuple must stay (review A-1 R-1).
fn holds_tuple_at(g: &DatabaseAccessGrant, at: OffsetDateTime) -> bool {
    g.revoked_at.is_none() && g.expires_at > at
}

fn micros(t: OffsetDateTime) -> OffsetDateTime {
    t.replace_nanosecond(t.nanosecond() / 1000 * 1000)
        .unwrap_or(t)
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

/// The tuple of a grant: `node:<db>#live_writer@agent:<id>` or `#personal_reader`.
pub fn tuple(node: NodeId, kind: GrantKind, agent: AgentId) -> Relationship {
    Relationship {
        object: ObjectRef::node(node),
        relation: kind.relation().into(),
        subject: agent_subject(agent),
    }
}

/// The agent as a subject of the graph (the subject of the grant checks).
pub fn agent_subject(agent: AgentId) -> SubjectRef {
    SubjectRef::object(ObjectRef::new("agent", agent))
}

/// The `data` of `database.grant_created`.
fn created_data(g: &DatabaseAccessGrant) -> Value {
    json!({
        "grant_id": g.id,
        "kind": g.kind.as_str(),
        "agent_id": g.agent_id,
        "expires_at": rfc3339(g.expires_at),
        "restore_point_id": g.restore_point_id,
        "approval_id": null,
    })
}

/// The `data` of `database.grant_revoked`.
fn revoked_data(g: &DatabaseAccessGrant, cause: EndCause) -> Value {
    json!({
        "grant_id": g.id,
        "kind": g.kind.as_str(),
        "agent_id": g.agent_id,
        "cause": cause.as_str(),
    })
}

/// The delivery ID of the `access_changed` hook of a grant's end: the same for every retry, so
/// the provider gets the hook once. A UUIDv7 in form (the time bits of the grant ID) with bits
/// from SHA-256 of the tenant, the grant and the cause.
pub fn access_changed_delivery_id(tenant: TenantId, grant: Uuid, cause: EndCause) -> DeliveryId {
    let mut h = Sha256::new();
    h.update(b"ruum.grant.access_changed\0");
    h.update(tenant.0.as_bytes());
    h.update(grant.as_bytes());
    h.update(cause.as_str().as_bytes());
    let digest = h.finalize();
    let mut b = [0u8; 16];
    b[..6].copy_from_slice(&grant.as_bytes()[..6]);
    b[6..].copy_from_slice(&digest[..10]);
    b[6] = (b[6] & 0x0f) | 0x70;
    b[8] = (b[8] & 0x3f) | 0x80;
    DeliveryId(Uuid::from_bytes(b))
}

/// The `access_changed` delivery for the database node of a grant that ended (provider
/// contract, hook table), so the provider closes the sessions that lost access.
fn access_changed(
    g: &DatabaseAccessGrant,
    cause: EndCause,
    seq: i64,
    by: Actor,
    at: OffsetDateTime,
) -> NewDelivery {
    NewDelivery {
        delivery_id: access_changed_delivery_id(g.tenant_id, g.id, cause),
        provider: super::super::PROVIDER_ID.into(),
        hook: HookDelivery {
            hook: HookKind::AccessChanged,
            tenant_id: g.tenant_id,
            node_id: Some(g.node_id),
            seq: Some(seq),
            target_node_id: None,
            data: revoked_data(g, cause).as_object().cloned(),
            by: by.into(),
            at,
        },
    }
}

fn change_of(e: &crate::journal::JournalEntry) -> ChangeInfo {
    ChangeInfo {
        seq: Some(e.seq),
        actor: e.actor,
        at: e.at,
    }
}

/// A grant that has ended already is `404`, as a revoked share link is for `revokeShareLink`:
/// the contract of `revokeDatabaseAccessGrant` has no `409` (review A-1 R-2).
fn grant_ended() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::NotFound,
            "no live database access grant with this ID; it has ended already",
        )
        .with_hint("list the grants with active=true"),
    )
}

fn grant_not_found() -> Error {
    Error::not_found("database access grant")
}

// ---------------------------------------------------------------------------- the link

/// The web URL of the database page with the write-grant prompt prefilled (`?give=...`,
/// `docs/ux/databases/screens.md` S6, README DB-8), for the `hint` of `403
/// write_grant_required` (decision 0004-20, C-16). It holds the tenant, the database and the
/// agent IDs, the kind and the minutes only, never a reason, a statement or a value: the types
/// of the arguments allow nothing else. `minutes` is capped to the limit of the kind (at least
/// 1); the person still decides in the prompt.
pub fn grant_prompt_url(
    public_url: &str,
    tenant: TenantId,
    node: NodeId,
    agent: AgentId,
    kind: GrantKind,
    minutes: u32,
) -> String {
    let minutes = minutes.clamp(1, kind.max_minutes());
    format!(
        "{}/t/{tenant}/item/{node}?give={}&agent={agent}&minutes={minutes}",
        public_url.trim_end_matches('/'),
        kind.as_str(),
    )
}

// ---------------------------------------------------------------------------- the store

/// The rows of `database_access_grants` (and the restore points of `write_live` grants). Every
/// method reads and writes the rows of one tenant, except the sweeper's scan
/// ([`GrantStore::expired_unended`]). The writes take the relationship engine: the memory store
/// writes the tuple at once, the Postgres store enqueues it in the transaction of the row and
/// applies it after the commit.
#[async_trait]
pub trait GrantStore: Send + Sync {
    /// Writes in one transaction: the restore point row (when `new.restore_point` is set), the
    /// grant row, the tuple (touch) and `database.grant_created`. `ctx` is the giver. `404`
    /// when the node is not in the tenant.
    async fn create(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        new: NewGrant,
    ) -> Result<Written<DatabaseAccessGrant>>;

    /// A grant of a database of the tenant.
    async fn get(
        &self,
        tenant: TenantId,
        node: NodeId,
        id: Uuid,
    ) -> Result<Option<DatabaseAccessGrant>>;

    /// The grants of a database, newest first (by ID, a UUIDv7). The cursor is the ID of the
    /// last item.
    async fn list(
        &self,
        tenant: TenantId,
        node: NodeId,
        filter: GrantFilter,
        page: PageRequest,
    ) -> Result<Page<DatabaseAccessGrant>>;

    /// A live grant of the agent and kind on the database at `at` (the newest), if any. The row
    /// only: [`active`] also checks the tuple.
    async fn live(
        &self,
        tenant: TenantId,
        node: NodeId,
        agent: AgentId,
        kind: GrantKind,
        at: OffsetDateTime,
    ) -> Result<Option<DatabaseAccessGrant>>;

    /// Ends a live grant at `at`: sets `revoked_*` to `ctx`'s principals, writes
    /// `database.grant_revoked` with `cause`, and, when no other grant of the agent and kind is
    /// live, deletes the tuple and enqueues `access_changed`; all in one transaction. `404`
    /// when the grant is not on that database of the tenant, and also when it has ended.
    async fn revoke(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        node: NodeId,
        id: Uuid,
        cause: EndCause,
        at: OffsetDateTime,
    ) -> Result<Written<DatabaseAccessGrant>>;

    /// The sweeper's scan, across tenants: grants not revoked whose `expires_at` is at or
    /// before `at` (and after `at` minus [`SWEEP_WINDOW_DAYS`]) and that have no
    /// `database.grant_revoked` entry yet. Oldest first, at most `limit`.
    async fn expired_unended(
        &self,
        at: OffsetDateTime,
        limit: usize,
    ) -> Result<Vec<DatabaseAccessGrant>>;

    /// Ends one expired grant (the sweeper): `database.grant_revoked` with cause `expired`, by
    /// the user who gave the grant, and, when no other grant of the agent and kind is live at
    /// `at`, the tuple delete and `access_changed`; in one transaction. Does nothing when the
    /// grant has an end entry already or was revoked. Returns `None` when it did nothing, else
    /// whether it deleted the tuple.
    async fn end_expired(
        &self,
        authz: &dyn Authz,
        grant: &DatabaseAccessGrant,
        at: OffsetDateTime,
    ) -> Result<Option<bool>>;

    /// A restore point row of the tenant.
    async fn restore_point(&self, tenant: TenantId, id: Uuid) -> Result<Option<RestorePointRow>>;
}

/// Pages a list that is sorted newest first; the cursor is the ID of the last item.
fn page_of(mut items: Vec<DatabaseAccessGrant>, limit: usize) -> Page<DatabaseAccessGrant> {
    let next = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|g| Cursor(g.id.to_string()))
    } else {
        None
    };
    Page {
        items,
        next_cursor: next,
    }
}

fn parse_cursor(page: &PageRequest) -> Result<Option<Uuid>> {
    page.cursor
        .as_ref()
        .map(|c| {
            c.0.parse::<Uuid>().map_err(|_| {
                Error::Api(
                    ApiError::new(ErrorCode::BadRequest, "the cursor is not valid")
                        .with_hint("use next_cursor of the previous page"),
                )
            })
        })
        .transpose()
}

// ---------------------------------------------------------------------------- memory

/// The in-memory store. It writes the journal entries through the in-memory journal, the
/// deliveries into the in-memory typed node store, and the tuples straight to the engine.
pub struct MemoryGrantStore {
    nodes: Arc<crate::nodes::memory::MemoryNodeStore>,
    journal: Arc<crate::journal::memory::MemoryJournal>,
    typed: Arc<crate::providers::store::MemoryTypedNodeStore>,
    state: Mutex<MemGrants>,
    /// Held across a whole write, so two writes never act on the same old state (the tuple
    /// write is async).
    writes: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct MemGrants {
    grants: Vec<DatabaseAccessGrant>,
    points: Vec<RestorePointRow>,
}

impl MemoryGrantStore {
    pub fn new(
        nodes: Arc<crate::nodes::memory::MemoryNodeStore>,
        journal: Arc<crate::journal::memory::MemoryJournal>,
        typed: Arc<crate::providers::store::MemoryTypedNodeStore>,
    ) -> Self {
        Self {
            nodes,
            journal,
            typed,
            state: Mutex::new(MemGrants::default()),
            writes: tokio::sync::Mutex::new(()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemGrants> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn revision(&self, tenant: TenantId, node: NodeId) -> Result<i64> {
        self.nodes
            .peek(node)
            .filter(|n| n.tenant_id == tenant)
            .map(|n| n.revision)
            .ok_or_else(|| Error::not_found("database"))
    }

    fn has_end_entry(&self, g: &DatabaseAccessGrant) -> bool {
        let id = g.id.to_string();
        self.journal.entries(g.tenant_id).iter().any(|e| {
            e.event == EventType::DatabaseGrantRevoked
                && e.data["grant_id"].as_str() == Some(id.as_str())
        })
    }

    /// Another grant of the tuple that still needs it at `at`: see [`holds_tuple_at`].
    fn other_live(&self, g: &DatabaseAccessGrant, at: OffsetDateTime) -> bool {
        self.lock().grants.iter().any(|o| {
            o.tenant_id == g.tenant_id
                && o.node_id == g.node_id
                && o.agent_id == g.agent_id
                && o.kind == g.kind
                && o.id != g.id
                && holds_tuple_at(o, at)
        })
    }

    /// The end of a grant: the journal entry, and the tuple delete and the delivery when no
    /// other live grant holds the tuple. Returns the entry and whether the tuple went.
    async fn end(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        g: &DatabaseAccessGrant,
        cause: EndCause,
        at: OffsetDateTime,
    ) -> Result<(crate::journal::JournalEntry, bool)> {
        let revision = self.revision(g.tenant_id, g.node_id)?;
        let delete = !self.other_live(g, at);
        if delete {
            authz
                .write(vec![RelationshipUpdate::Delete(tuple(
                    g.node_id, g.kind, g.agent_id,
                ))])
                .await?;
        }
        let e = self.journal.append(
            ctx,
            NewEntry {
                node_id: Some(g.node_id),
                revision: Some(revision),
                event: EventType::DatabaseGrantRevoked,
                data: revoked_data(g, cause),
            },
        )?;
        if delete {
            self.typed
                .enqueue(access_changed(g, cause, e.seq, ctx.actor, e.at))?;
        }
        Ok((e, delete))
    }
}

#[async_trait]
impl GrantStore for MemoryGrantStore {
    async fn create(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        new: NewGrant,
    ) -> Result<Written<DatabaseAccessGrant>> {
        let _w = self.writes.lock().await;
        let revision = self.revision(ctx.tenant, new.node)?;
        let point = new.restore_point.as_ref().map(|m| RestorePointRow {
            id: Uuid::now_v7(),
            tenant_id: ctx.tenant,
            node_id: new.node,
            label: None,
            reason: "before_write_grant".into(),
            at: micros(m.at),
            wal_lsn: m.wal_lsn.clone(),
            actor: ctx.actor,
        });
        let g = DatabaseAccessGrant {
            id: Uuid::now_v7(),
            tenant_id: ctx.tenant,
            node_id: new.node,
            kind: new.kind,
            agent_id: new.agent,
            agent_display_name: None,
            granted_by: ctx
                .actor
                .effective_user()
                .ok_or_else(Error::user_required)?,
            granted_by_display_name: None,
            reason: new.reason,
            starts_at: micros(new.starts_at),
            expires_at: micros(new.expires_at),
            revoked_at: None,
            revoked_by: None,
            revoked_on_behalf_of: None,
            restore_point_id: point.as_ref().map(|p| p.id),
            created_at: micros(new.starts_at),
        };
        authz
            .write(vec![RelationshipUpdate::Touch(tuple(
                g.node_id, g.kind, g.agent_id,
            ))])
            .await?;
        {
            let mut st = self.lock();
            if let Some(p) = point {
                st.points.push(p);
            }
            st.grants.push(g.clone());
        }
        let e = self.journal.append(
            ctx,
            NewEntry {
                node_id: Some(g.node_id),
                revision: Some(revision),
                event: EventType::DatabaseGrantCreated,
                data: created_data(&g),
            },
        )?;
        Ok(Written {
            value: g,
            change: change_of(&e),
        })
    }

    async fn get(
        &self,
        tenant: TenantId,
        node: NodeId,
        id: Uuid,
    ) -> Result<Option<DatabaseAccessGrant>> {
        Ok(self
            .lock()
            .grants
            .iter()
            .find(|g| g.tenant_id == tenant && g.node_id == node && g.id == id)
            .cloned())
    }

    async fn list(
        &self,
        tenant: TenantId,
        node: NodeId,
        filter: GrantFilter,
        page: PageRequest,
    ) -> Result<Page<DatabaseAccessGrant>> {
        let after = parse_cursor(&page)?;
        let limit = page.limit() as usize;
        let mut items: Vec<DatabaseAccessGrant> = self
            .lock()
            .grants
            .iter()
            .filter(|g| g.tenant_id == tenant && g.node_id == node)
            .filter(|g| filter.agent.is_none_or(|a| g.agent_id == a))
            .filter(|g| filter.live_at.is_none_or(|at| g.is_live(at)))
            .filter(|g| after.is_none_or(|c| g.id < c))
            .cloned()
            .collect();
        items.sort_by_key(|g| std::cmp::Reverse(g.id));
        items.truncate(limit + 1);
        Ok(page_of(items, limit))
    }

    async fn live(
        &self,
        tenant: TenantId,
        node: NodeId,
        agent: AgentId,
        kind: GrantKind,
        at: OffsetDateTime,
    ) -> Result<Option<DatabaseAccessGrant>> {
        Ok(self
            .lock()
            .grants
            .iter()
            .filter(|g| {
                g.tenant_id == tenant
                    && g.node_id == node
                    && g.agent_id == agent
                    && g.kind == kind
                    && g.is_live(at)
            })
            .max_by_key(|g| g.id)
            .cloned())
    }

    async fn revoke(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        node: NodeId,
        id: Uuid,
        cause: EndCause,
        at: OffsetDateTime,
    ) -> Result<Written<DatabaseAccessGrant>> {
        let _w = self.writes.lock().await;
        let at = micros(at);
        let mut g = self
            .get(ctx.tenant, node, id)
            .await?
            .ok_or_else(grant_not_found)?;
        if !g.is_live(at) {
            return Err(grant_ended());
        }
        g.revoked_at = Some(at);
        g.revoked_by = Some(ctx.actor.actor);
        g.revoked_on_behalf_of = ctx.actor.on_behalf_of;
        {
            let mut st = self.lock();
            if let Some(row) = st
                .grants
                .iter_mut()
                .find(|r| r.tenant_id == ctx.tenant && r.id == id)
            {
                *row = g.clone();
            }
        }
        let (e, _) = self.end(ctx, authz, &g, cause, at).await?;
        Ok(Written {
            value: g,
            change: change_of(&e),
        })
    }

    async fn expired_unended(
        &self,
        at: OffsetDateTime,
        limit: usize,
    ) -> Result<Vec<DatabaseAccessGrant>> {
        let since = at - time::Duration::days(SWEEP_WINDOW_DAYS);
        let mut due: Vec<DatabaseAccessGrant> = self
            .lock()
            .grants
            .iter()
            .filter(|g| g.revoked_at.is_none() && g.expires_at <= at && g.expires_at > since)
            .cloned()
            .collect();
        due.retain(|g| !self.has_end_entry(g));
        due.sort_by_key(|g| (g.expires_at, g.id));
        due.truncate(limit);
        Ok(due)
    }

    async fn end_expired(
        &self,
        authz: &dyn Authz,
        grant: &DatabaseAccessGrant,
        at: OffsetDateTime,
    ) -> Result<Option<bool>> {
        let _w = self.writes.lock().await;
        let Some(g) = self.get(grant.tenant_id, grant.node_id, grant.id).await? else {
            return Ok(None);
        };
        if g.revoked_at.is_some() || g.expires_at > at || self.has_end_entry(&g) {
            return Ok(None);
        }
        let ctx = sweeper_ctx(&g);
        let (_, deleted) = self.end(&ctx, authz, &g, EndCause::Expired, at).await?;
        Ok(Some(deleted))
    }

    async fn restore_point(&self, tenant: TenantId, id: Uuid) -> Result<Option<RestorePointRow>> {
        Ok(self
            .lock()
            .points
            .iter()
            .find(|p| p.tenant_id == tenant && p.id == id)
            .cloned())
    }
}

/// The context of the sweeper's entry: the user who gave the grant, in its tenant.
fn sweeper_ctx(g: &DatabaseAccessGrant) -> Context {
    Context {
        tenant: g.tenant_id,
        actor: Actor::user(g.granted_by),
        scope: None,
        idempotency_key: None,
    }
}

// ---------------------------------------------------------------------------- Postgres

/// The Postgres store. Every statement filters by `tenant_id` and runs in a transaction that
/// sets the tenant (row-level security), except the sweeper's scan, which runs as the system.
pub struct PgGrantStore {
    pool: sqlx::PgPool,
}

impl PgGrantStore {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

macro_rules! grant_columns {
    () => {
        "id, tenant_id, node_id, kind::text as kind, agent_id, granted_by, reason, starts_at, \
         expires_at, revoked_at, revoked_kind, revoked_by, revoked_obo, restore_point_id, \
         created_at"
    };
}

fn grant_of(r: &sqlx::postgres::PgRow) -> Result<DatabaseAccessGrant> {
    use sqlx::Row;
    let kind: String = r.try_get("kind")?;
    let revoked_kind: Option<PrincipalKind> = r.try_get("revoked_kind")?;
    let revoked_by: Option<Uuid> = r.try_get("revoked_by")?;
    Ok(DatabaseAccessGrant {
        id: r.try_get("id")?,
        tenant_id: TenantId(r.try_get("tenant_id")?),
        node_id: NodeId(r.try_get("node_id")?),
        kind: GrantKind::parse(&kind)?,
        agent_id: AgentId(r.try_get("agent_id")?),
        agent_display_name: None,
        granted_by: UserId(r.try_get("granted_by")?),
        granted_by_display_name: None,
        reason: r.try_get("reason")?,
        starts_at: r.try_get("starts_at")?,
        expires_at: r.try_get("expires_at")?,
        revoked_at: r.try_get("revoked_at")?,
        revoked_by: match (revoked_kind, revoked_by) {
            (Some(kind), Some(id)) => Some(PrincipalRef {
                kind,
                id: id.into(),
            }),
            _ => None,
        },
        revoked_on_behalf_of: r.try_get::<Option<Uuid>, _>("revoked_obo")?.map(UserId),
        restore_point_id: r.try_get("restore_point_id")?,
        created_at: r.try_get("created_at")?,
    })
}

async fn node_revision(
    conn: &mut sqlx::PgConnection,
    tenant: TenantId,
    node: NodeId,
) -> Result<i64> {
    let r: Option<i64> =
        sqlx::query_scalar("select revision from nodes where tenant_id = $1 and id = $2 for share")
            .bind(tenant.0)
            .bind(node.0)
            .fetch_optional(conn)
            .await?;
    r.ok_or_else(|| Error::not_found("database"))
}

/// Serializes the grant writes of one tuple `(tenant, node, agent, kind)` until the end of the
/// caller's transaction (`pg_advisory_xact_lock` on a hash of the key). `create`, `revoke` and
/// `end_expired` take it first, so the "other live grant" check of one write always sees the
/// committed result of the other: two revokes at the same time cannot both keep the tuple, and
/// a create during a revoke cannot lose its tuple (review A-1 R-1). The outbox rows of the
/// writes then get their IDs in commit order, and the outbox applies them in that order. A hash
/// collision of two keys only serializes more.
async fn lock_tuple(
    conn: &mut sqlx::PgConnection,
    tenant: TenantId,
    node: NodeId,
    agent: AgentId,
    kind: GrantKind,
) -> Result<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!(
            "database_access_grant:{}:{}:{}:{}",
            tenant.0,
            node.0,
            agent.0,
            kind.as_str()
        ))
        .execute(conn)
        .await?;
    Ok(())
}

/// Another grant of the tuple that still needs it at `at` (see [`holds_tuple_at`]).
async fn other_live_pg(
    conn: &mut sqlx::PgConnection,
    g: &DatabaseAccessGrant,
    at: OffsetDateTime,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "select exists (select 1 from database_access_grants where tenant_id = $1 \
         and node_id = $2 and agent_id = $3 and kind = $4::database_grant_kind and id <> $5 \
         and revoked_at is null and expires_at > $6)",
    )
    .bind(g.tenant_id.0)
    .bind(g.node_id.0)
    .bind(g.agent_id.0)
    .bind(g.kind.as_str())
    .bind(g.id)
    .bind(at)
    .fetch_one(conn)
    .await?)
}

/// The Postgres end of a grant in the caller's transaction: the journal entry, and the tuple
/// delete (outbox) and the delivery when no other live grant holds the tuple. Returns the
/// entry, the outbox row (if any) and whether the tuple goes.
async fn end_pg(
    conn: &mut sqlx::PgConnection,
    ctx: &Context,
    g: &DatabaseAccessGrant,
    cause: EndCause,
    at: OffsetDateTime,
) -> Result<(crate::journal::JournalEntry, Option<i64>)> {
    let revision = node_revision(conn, g.tenant_id, g.node_id).await?;
    let delete = !other_live_pg(conn, g, at).await?;
    let e = crate::journal::postgres::append(
        conn,
        ctx,
        NewEntry {
            node_id: Some(g.node_id),
            revision: Some(revision),
            event: EventType::DatabaseGrantRevoked,
            data: revoked_data(g, cause),
        },
    )
    .await?;
    let mut ob = None;
    if delete {
        ob = Some(
            crate::authz::outbox::enqueue(
                conn,
                g.tenant_id,
                &[RelationshipUpdate::Delete(tuple(
                    g.node_id, g.kind, g.agent_id,
                ))],
            )
            .await?,
        );
        crate::providers::postgres::enqueue(
            conn,
            &access_changed(g, cause, e.seq, ctx.actor, e.at),
        )
        .await?;
    }
    Ok((e, ob))
}

async fn has_end_entry_pg(conn: &mut sqlx::PgConnection, g: &DatabaseAccessGrant) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "select exists (select 1 from journal where tenant_id = $1 and node_id = $2 \
         and type = 'database.grant_revoked' and data->>'grant_id' = $3)",
    )
    .bind(g.tenant_id.0)
    .bind(g.node_id.0)
    .bind(g.id.to_string())
    .fetch_one(conn)
    .await?)
}

#[async_trait]
impl GrantStore for PgGrantStore {
    async fn create(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        new: NewGrant,
    ) -> Result<Written<DatabaseAccessGrant>> {
        let granted_by = ctx
            .actor
            .effective_user()
            .ok_or_else(Error::user_required)?;
        let mut tx = crate::sharing::postgres::begin(&self.pool, ctx).await?;
        lock_tuple(&mut tx, ctx.tenant, new.node, new.agent, new.kind).await?;
        let revision = node_revision(&mut tx, ctx.tenant, new.node).await?;
        let point = match &new.restore_point {
            Some(m) => {
                let id = Uuid::now_v7();
                sqlx::query(
                    "insert into database_restore_points (id, tenant_id, node_id, reason, at, \
                     wal_lsn, actor_kind, actor, on_behalf_of) \
                     values ($1, $2, $3, 'before_write_grant', $4, $5::pg_lsn, $6, $7, $8)",
                )
                .bind(id)
                .bind(ctx.tenant.0)
                .bind(new.node.0)
                .bind(micros(m.at))
                .bind(m.wal_lsn.as_deref())
                .bind(ctx.actor.actor.kind)
                .bind(ctx.actor.actor.id.0)
                .bind(ctx.actor.on_behalf_of.map(|u| u.0))
                .execute(&mut *tx)
                .await?;
                Some(id)
            }
            None => None,
        };
        let id = Uuid::now_v7();
        let row = sqlx::query(concat!(
            "insert into database_access_grants (id, tenant_id, node_id, kind, agent_id, \
             granted_by, reason, starts_at, expires_at, restore_point_id, created_at) \
             values ($1, $2, $3, $4::database_grant_kind, $5, $6, $7, $8, $9, $10, $8) \
             returning ",
            grant_columns!()
        ))
        .bind(id)
        .bind(ctx.tenant.0)
        .bind(new.node.0)
        .bind(new.kind.as_str())
        .bind(new.agent.0)
        .bind(granted_by.0)
        .bind(new.reason.as_deref())
        .bind(micros(new.starts_at))
        .bind(micros(new.expires_at))
        .bind(point)
        .fetch_one(&mut *tx)
        .await?;
        let g = grant_of(&row)?;
        let e = crate::journal::postgres::append(
            &mut tx,
            ctx,
            NewEntry {
                node_id: Some(g.node_id),
                revision: Some(revision),
                event: EventType::DatabaseGrantCreated,
                data: created_data(&g),
            },
        )
        .await?;
        let ob = crate::authz::outbox::enqueue(
            &mut tx,
            ctx.tenant,
            &[RelationshipUpdate::Touch(tuple(
                g.node_id, g.kind, g.agent_id,
            ))],
        )
        .await?;
        tx.commit().await?;
        crate::authz::outbox::apply_after_commit(&self.pool, authz, ctx.tenant, &[ob]).await;
        Ok(Written {
            value: g,
            change: change_of(&e),
        })
    }

    async fn get(
        &self,
        tenant: TenantId,
        node: NodeId,
        id: Uuid,
    ) -> Result<Option<DatabaseAccessGrant>> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(concat!(
            "select ",
            grant_columns!(),
            " from database_access_grants where tenant_id = $1 and node_id = $2 and id = $3"
        ))
        .bind(tenant.0)
        .bind(node.0)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(grant_of).transpose()
    }

    async fn list(
        &self,
        tenant: TenantId,
        node: NodeId,
        filter: GrantFilter,
        page: PageRequest,
    ) -> Result<Page<DatabaseAccessGrant>> {
        let after = parse_cursor(&page)?;
        let limit = page.limit() as usize;
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let rows = sqlx::query(concat!(
            "select ",
            grant_columns!(),
            " from database_access_grants where tenant_id = $1 and node_id = $2 \
             and ($3::uuid is null or agent_id = $3) \
             and ($4::timestamptz is null or (revoked_at is null and starts_at <= $4 \
                  and expires_at > $4)) \
             and ($5::uuid is null or id < $5) \
             order by id desc limit $6"
        ))
        .bind(tenant.0)
        .bind(node.0)
        .bind(filter.agent.map(|a| a.0))
        .bind(filter.live_at)
        .bind(after)
        .bind(limit as i64 + 1)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let items = rows.iter().map(grant_of).collect::<Result<Vec<_>>>()?;
        Ok(page_of(items, limit))
    }

    async fn live(
        &self,
        tenant: TenantId,
        node: NodeId,
        agent: AgentId,
        kind: GrantKind,
        at: OffsetDateTime,
    ) -> Result<Option<DatabaseAccessGrant>> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(concat!(
            "select ",
            grant_columns!(),
            " from database_access_grants where tenant_id = $1 and node_id = $2 \
             and agent_id = $3 and kind = $4::database_grant_kind \
             and revoked_at is null and starts_at <= $5 and expires_at > $5 \
             order by id desc limit 1"
        ))
        .bind(tenant.0)
        .bind(node.0)
        .bind(agent.0)
        .bind(kind.as_str())
        .bind(at)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(grant_of).transpose()
    }

    async fn revoke(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        node: NodeId,
        id: Uuid,
        cause: EndCause,
        at: OffsetDateTime,
    ) -> Result<Written<DatabaseAccessGrant>> {
        let at = micros(at);
        let mut tx = crate::sharing::postgres::begin(&self.pool, ctx).await?;
        // The agent and the kind of a grant never change, so read them first, then take the
        // lock of the tuple, then lock and read the row again.
        let key: Option<(Uuid, String)> = sqlx::query_as(
            "select agent_id, kind::text from database_access_grants \
             where tenant_id = $1 and node_id = $2 and id = $3",
        )
        .bind(ctx.tenant.0)
        .bind(node.0)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let (agent, kind) = key.ok_or_else(grant_not_found)?;
        lock_tuple(
            &mut tx,
            ctx.tenant,
            node,
            AgentId(agent),
            GrantKind::parse(&kind)?,
        )
        .await?;
        let row = sqlx::query(concat!(
            "select ",
            grant_columns!(),
            " from database_access_grants where tenant_id = $1 and node_id = $2 and id = $3 \
             for update"
        ))
        .bind(ctx.tenant.0)
        .bind(node.0)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(grant_not_found)?;
        let g = grant_of(&row)?;
        if !g.is_live(at) {
            return Err(grant_ended());
        }
        let row = sqlx::query(concat!(
            "update database_access_grants set revoked_at = $3, revoked_kind = $4, \
             revoked_by = $5, revoked_obo = $6 where tenant_id = $1 and id = $2 returning ",
            grant_columns!()
        ))
        .bind(ctx.tenant.0)
        .bind(id)
        .bind(at)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .fetch_one(&mut *tx)
        .await?;
        let g = grant_of(&row)?;
        let (e, ob) = end_pg(&mut tx, ctx, &g, cause, at).await?;
        tx.commit().await?;
        if let Some(ob) = ob {
            crate::authz::outbox::apply_after_commit(&self.pool, authz, ctx.tenant, &[ob]).await;
        }
        Ok(Written {
            value: g,
            change: change_of(&e),
        })
    }

    async fn expired_unended(
        &self,
        at: OffsetDateTime,
        limit: usize,
    ) -> Result<Vec<DatabaseAccessGrant>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await?;
        let rows = sqlx::query(concat!(
            "select ",
            grant_columns!(),
            " from database_access_grants g where g.revoked_at is null and g.expires_at <= $1 \
             and g.expires_at > $2 and not exists (select 1 from journal j \
                 where j.tenant_id = g.tenant_id and j.node_id = g.node_id \
                 and j.type = 'database.grant_revoked' and j.data->>'grant_id' = g.id::text) \
             order by g.expires_at, g.id limit $3"
        ))
        .bind(at)
        .bind(at - time::Duration::days(SWEEP_WINDOW_DAYS))
        .bind(limit as i64)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(grant_of).collect()
    }

    async fn end_expired(
        &self,
        authz: &dyn Authz,
        grant: &DatabaseAccessGrant,
        at: OffsetDateTime,
    ) -> Result<Option<bool>> {
        let ctx = sweeper_ctx(grant);
        let mut tx = crate::sharing::postgres::begin(&self.pool, &ctx).await?;
        lock_tuple(
            &mut tx,
            grant.tenant_id,
            grant.node_id,
            grant.agent_id,
            grant.kind,
        )
        .await?;
        let row = sqlx::query(concat!(
            "select ",
            grant_columns!(),
            " from database_access_grants where tenant_id = $1 and node_id = $2 and id = $3 \
             for update"
        ))
        .bind(grant.tenant_id.0)
        .bind(grant.node_id.0)
        .bind(grant.id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let g = grant_of(&row)?;
        if g.revoked_at.is_some() || g.expires_at > at || has_end_entry_pg(&mut tx, &g).await? {
            return Ok(None);
        }
        let (_, ob) = end_pg(&mut tx, &ctx, &g, EndCause::Expired, at).await?;
        tx.commit().await?;
        if let Some(ob) = ob {
            crate::authz::outbox::apply_after_commit(&self.pool, authz, g.tenant_id, &[ob]).await;
        }
        Ok(Some(ob.is_some()))
    }

    async fn restore_point(&self, tenant: TenantId, id: Uuid) -> Result<Option<RestorePointRow>> {
        use sqlx::Row;
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(
            "select id, tenant_id, node_id, label, reason::text as reason, at, \
             wal_lsn::text as wal_lsn, actor_kind, actor, on_behalf_of \
             from database_restore_points where tenant_id = $1 and id = $2",
        )
        .bind(tenant.0)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let Some(r) = row else {
            return Ok(None);
        };
        Ok(Some(RestorePointRow {
            id: r.try_get("id")?,
            tenant_id: TenantId(r.try_get("tenant_id")?),
            node_id: NodeId(r.try_get("node_id")?),
            label: r.try_get("label")?,
            reason: r.try_get("reason")?,
            at: r.try_get("at")?,
            wal_lsn: r.try_get("wal_lsn")?,
            actor: Actor {
                actor: PrincipalRef {
                    kind: r.try_get("actor_kind")?,
                    id: r.try_get::<Uuid, _>("actor")?.into(),
                },
                on_behalf_of: r.try_get::<Option<Uuid>, _>("on_behalf_of")?.map(UserId),
            },
        }))
    }
}

// ---------------------------------------------------------------------------- the check

/// The grant check for later cards (Q-2, A-2, D-2): the live grant of `agent` and `kind` on the
/// database at `at`, or `None`. A grant counts only when its row is live AND its tuple holds in
/// the graph (`agent_write_live` / `agent_read_personal`, subject the agent). An expired or
/// revoked grant gives nothing, also while the sweeper has not deleted its tuple yet, and a
/// tuple without a live row gives nothing either.
pub async fn active(
    store: &dyn GrantStore,
    authz: &dyn Authz,
    tenant: TenantId,
    node: NodeId,
    agent: AgentId,
    kind: GrantKind,
    at: OffsetDateTime,
) -> Result<Option<DatabaseAccessGrant>> {
    let Some(g) = store.live(tenant, node, agent, kind, at).await? else {
        return Ok(None);
    };
    let holds = authz
        .check(
            &agent_subject(agent),
            kind.permission(),
            &ObjectRef::node(node),
            None,
        )
        .await?;
    Ok(holds.then_some(g))
}

// ---------------------------------------------------------------------------- the service

fn only_the_agents_user() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PermissionDenied,
            "only the user the agent acts for may give it a grant",
        )
        .with_hint("ask the agent's user to give the grant"),
    )
}

fn validation(message: &str, hint: &str) -> Error {
    Error::Api(ApiError::new(ErrorCode::ValidationFailed, message).with_hint(hint))
}

impl DatabaseService {
    /// `createDatabaseAccessGrant`. See the module docs for the rules and their order.
    /// `agents` is the agent store of the services (the agent's user and scope).
    pub async fn create_grant(
        &self,
        ctx: &Context,
        agents: &dyn AgentStore,
        node: NodeId,
        req: CreateGrant,
    ) -> Result<Written<DatabaseAccessGrant>> {
        let user = crate::actor::require_user_actor(ctx)?;
        let d = self.deps();
        let db = self.database_node(ctx, node).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, node, Role::View).await?;
        // Only the user the agent acts for. An unknown or disabled agent gets the same answer,
        // so the call tells nothing about other people's agents.
        let agent = match agents.authenticate(ctx.tenant, req.agent_id).await {
            Ok(a) if a.owner_id == user && a.tenant_id == ctx.tenant => a,
            _ => return Err(only_the_agents_user()),
        };
        let max = req.kind.max_minutes();
        if req.duration_minutes < 1 || req.duration_minutes > i64::from(max) {
            return Err(validation(
                &format!("a {} grant lasts 1 to {max} minutes", req.kind.as_str()),
                "/duration_minutes",
            ));
        }
        if req.reason.as_ref().is_some_and(|r| r.chars().count() > 500) {
            return Err(validation("reason has at most 500 characters", "/reason"));
        }
        if db.trashed_at.is_some() {
            return Err(in_trash());
        }
        let now = crate::now();
        if agent.scope.expires_at.is_some_and(|t| t <= now) {
            return Err(Error::Api(
                ApiError::new(ErrorCode::PermissionDenied, "the agent's scope has expired")
                    .with_hint("extend the agent's scope first"),
            ));
        }
        if let Some(root) = agent.scope.subtree
            && root != node
            && !d.nodes.ancestors(ctx, node).await?.contains(&root)
        {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    "the database is outside the agent's scope",
                )
                .with_hint("widen the agent's scope to the database first"),
            ));
        }
        // Both kinds need a ready database: the grant row refers to its `databases` row (a
        // foreign key), so a database that is still provisioning, or whose create failed, gets
        // the same `409` on both backends instead of a `500` on Postgres (review A-1 R-3).
        self.ready_database(ctx.tenant, node).await?;
        let restore_point = match req.kind {
            GrantKind::ReadPersonal => None,
            GrantKind::WriteLive => {
                authz::require(&*d.authz, &*d.nodes, ctx, node, Role::Edit).await?;
                if agent.scope.max_role < Role::Edit {
                    return Err(Error::Api(
                        ApiError::new(
                            ErrorCode::PermissionDenied,
                            "write_live needs an agent scope of edit or higher",
                        )
                        .with_hint("give the agent the scope edit first"),
                    ));
                }
                Some(self.restore_point_before_grant(ctx.tenant, node).await?)
            }
        };
        let new = NewGrant {
            node,
            kind: req.kind,
            agent: agent.id,
            reason: req.reason,
            starts_at: now,
            expires_at: now + time::Duration::minutes(req.duration_minutes),
            restore_point,
        };
        let mut w = d.grants.create(ctx, &*d.authz, new).await?;
        self.fill_grant_names(ctx.tenant, std::slice::from_mut(&mut w.value))
            .await;
        Ok(w)
    }

    /// The `databases` row of a ready database, else `409` "not ready".
    async fn ready_database(&self, tenant: TenantId, node: NodeId) -> Result<DatabaseRow> {
        self.deps().store.get(tenant, node).await?.ok_or_else(|| {
            Error::Api(
                ApiError::new(ErrorCode::Conflict, "the database is not ready yet")
                    .with_hint("retry when the database is ready"),
            )
        })
    }

    /// The engine's restore point before a `write_live` grant. The database must be ready
    /// (its `databases` row exists).
    async fn restore_point_before_grant(
        &self,
        tenant: TenantId,
        node: NodeId,
    ) -> Result<RestorePointMark> {
        let d = self.deps();
        let row = self.ready_database(tenant, node).await?;
        let engine = d
            .engines
            .get(row.engine)
            .ok_or_else(|| super::super::engine_not_available(row.engine))?;
        engine
            .create_restore_point(&row.database_ref())
            .await
            .map_err(|e| {
                // The engine text stays in the log (decision 0004-8).
                tracing::warn!(%tenant, %node, error = %e, "the restore point before a grant failed");
                Error::api(
                    ErrorCode::Unavailable,
                    "the database could not record a restore point; try again",
                )
            })
    }

    /// `listDatabaseAccessGrants`: `view`; an agent lists only its own grants.
    pub async fn list_grants(
        &self,
        ctx: &Context,
        node: NodeId,
        active: bool,
        page: PageRequest,
    ) -> Result<Page<DatabaseAccessGrant>> {
        refuse_share_link(ctx)?;
        let d = self.deps();
        self.database_node(ctx, node).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, node, Role::View).await?;
        let filter = GrantFilter {
            agent: ctx
                .actor
                .is_agent()
                .then_some(AgentId(ctx.actor.actor.id.0)),
            live_at: active.then(crate::now),
        };
        let mut p = d.grants.list(ctx.tenant, node, filter, page).await?;
        self.fill_grant_names(ctx.tenant, &mut p.items).await;
        Ok(p)
    }

    /// `revokeDatabaseAccessGrant`: the user who gave it, a person with `manage`, or the agent
    /// that holds it (cause `given_back`).
    pub async fn revoke_grant(
        &self,
        ctx: &Context,
        node: NodeId,
        id: Uuid,
    ) -> Result<Written<DatabaseAccessGrant>> {
        refuse_share_link(ctx)?;
        let d = self.deps();
        self.database_node(ctx, node).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, node, Role::View).await?;
        let g = d
            .grants
            .get(ctx.tenant, node, id)
            .await?
            .ok_or_else(grant_not_found)?;
        let holder = ctx.actor.is_agent() && ctx.actor.actor.id.0 == g.agent_id.0;
        let cause = if holder {
            EndCause::GivenBack
        } else if ctx.actor.is_user() {
            let giver = ctx.actor.actor.id.0 == g.granted_by.0;
            if !giver {
                authz::require(&*d.authz, &*d.nodes, ctx, node, Role::Manage).await?;
            }
            EndCause::Revoked
        } else {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    "only the user who gave the grant, a person with manage or the agent that \
                     holds it may end it",
                )
                .with_hint("ask the agent's user"),
            ));
        };
        let mut w = d
            .grants
            .revoke(ctx, &*d.authz, node, id, cause, crate::now())
            .await?;
        self.fill_grant_names(ctx.tenant, std::slice::from_mut(&mut w.value))
            .await;
        Ok(w)
    }

    /// [`active`] now, over the stores of this service.
    pub async fn active_grant(
        &self,
        tenant: TenantId,
        node: NodeId,
        agent: AgentId,
        kind: GrantKind,
    ) -> Result<Option<DatabaseAccessGrant>> {
        self.active_grant_at(tenant, node, agent, kind, crate::now())
            .await
    }

    /// [`active`] at `at` (tests look past the end of a grant).
    pub async fn active_grant_at(
        &self,
        tenant: TenantId,
        node: NodeId,
        agent: AgentId,
        kind: GrantKind,
        at: OffsetDateTime,
    ) -> Result<Option<DatabaseAccessGrant>> {
        let d = self.deps();
        active(&*d.grants, &*d.authz, tenant, node, agent, kind, at).await
    }

    /// One sweeper round now.
    pub async fn sweep_grants(&self) -> Result<SweepRound> {
        self.sweep_grants_at(crate::now()).await
    }

    /// One sweeper round at `at`: ends every grant whose time ran out by then (see the module
    /// docs). A failure of one grant is logged and does not stop the others.
    pub async fn sweep_grants_at(&self, at: OffsetDateTime) -> Result<SweepRound> {
        let d = self.deps();
        let mut round = SweepRound::default();
        for g in d.grants.expired_unended(at, SWEEP_LIMIT).await? {
            match d.grants.end_expired(&*d.authz, &g, at).await {
                Ok(Some(deleted)) => {
                    round.ended += 1;
                    round.tuples_deleted += usize::from(deleted);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    tenant = %g.tenant_id,
                    grant = %g.id,
                    error = %e,
                    "the grant sweeper could not end a grant; the next round tries again"
                ),
            }
        }
        Ok(round)
    }

    /// Fills `agent_display_name` and `granted_by_display_name` with the resolver of decision
    /// 0003-5 (names of the tenant only). Names are optional: a failed lookup leaves them out.
    async fn fill_grant_names(&self, tenant: TenantId, grants: &mut [DatabaseAccessGrant]) {
        if grants.is_empty() {
            return;
        }
        let mut ids: Vec<Uuid> = grants
            .iter()
            .flat_map(|g| [g.agent_id.0, g.granted_by.0])
            .collect();
        ids.sort();
        ids.dedup();
        match self.deps().tenants.principal_names(tenant, &ids).await {
            Ok(names) => {
                for g in grants {
                    g.agent_display_name = names.get(&g.agent_id.0).map(|n| n.display_name.clone());
                    g.granted_by_display_name =
                        names.get(&g.granted_by.0).map(|n| n.display_name.clone());
                }
            }
            Err(e) => tracing::warn!(%tenant, error = %e, "grant names: lookup failed"),
        }
    }
}

/// The server's sweeper: a round every [`SWEEP_EVERY`], each in a task of its own, so a panic
/// ends the round, not the loop.
pub async fn run_sweeper(service: Arc<DatabaseService>) {
    loop {
        let s = service.clone();
        match tokio::spawn(async move { s.sweep_grants().await }).await {
            Ok(Ok(round)) if round != SweepRound::default() => {
                tracing::info!(?round, "grant sweeper round");
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "the grant sweeper failed a round"),
            Err(e) => tracing::error!(error = %e, "a grant sweeper round panicked"),
        }
        tokio::time::sleep(SWEEP_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    //! The link, one conformance suite of the store on memory and Postgres, the service rules
    //! on the in-memory services, the Postgres transaction and tenant rules (`#[sqlx::test]`),
    //! and the restore point on the Postgres engine (`RUUM_DBHOST_URL`; skipped without it,
    //! except in CI).

    use super::*;
    use crate::agents::{Agent, AgentScope, Budget, NewAgent};
    use crate::authz::memory::MemoryAuthz;
    use crate::databases::tests::{World, ctx, settled, world};
    use crate::databases::{CreateDatabase, DatabaseParts, Engines, install};
    use crate::nodes::OnConflict;
    use crate::providers::ProviderState;
    use crate::providers::store::TypedNodeStore;

    fn code<T: std::fmt::Debug>(r: Result<T>) -> (u16, String) {
        match r {
            Err(Error::Api(e)) => (e.code.http_status(), e.code.as_str().to_string()),
            other => panic!("expected an API error, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------------ the link

    #[test]
    fn grant_prompt_url_holds_ids_kind_and_minutes_only() {
        let (t, n, a) = (TenantId::new(), NodeId::new(), AgentId::new());
        let url = grant_prompt_url("https://ruum.localhost/", t, n, a, GrantKind::WriteLive, 60);
        assert_eq!(
            url,
            format!("https://ruum.localhost/t/{t}/item/{n}?give=write_live&agent={a}&minutes=60")
        );
        let (path, query) = url
            .strip_prefix("https://ruum.localhost")
            .unwrap()
            .split_once('?')
            .unwrap();
        assert_eq!(path, format!("/t/{t}/item/{n}"));
        let pairs: Vec<(&str, &str)> = query
            .split('&')
            .map(|p| p.split_once('=').unwrap())
            .collect();
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["give", "agent", "minutes"], "no other field");
        // Every value is an ID, a kind or a number: nothing a person or an agent wrote.
        for (k, v) in pairs {
            let ok = match k {
                "give" => v == "write_live" || v == "read_personal",
                "agent" => v.parse::<Uuid>().is_ok(),
                "minutes" => v.parse::<u32>().is_ok(),
                _ => false,
            };
            assert!(ok, "{k}={v}");
        }
        // Minutes are capped to the limits of the kind.
        assert!(
            grant_prompt_url("https://x", t, n, a, GrantKind::ReadPersonal, 10_000)
                .ends_with("minutes=480")
        );
        assert!(
            grant_prompt_url("https://x", t, n, a, GrantKind::WriteLive, 10_000)
                .ends_with("minutes=1440")
        );
        assert!(
            grant_prompt_url("https://x", t, n, a, GrantKind::WriteLive, 0).ends_with("minutes=1")
        );
    }

    #[test]
    fn delivery_ids_are_stable_per_grant_and_cause() {
        let (t, g) = (TenantId::new(), Uuid::now_v7());
        let id = access_changed_delivery_id(t, g, EndCause::Revoked);
        assert_eq!(id, access_changed_delivery_id(t, g, EndCause::Revoked));
        assert_ne!(id, access_changed_delivery_id(t, g, EndCause::Expired));
        assert_ne!(
            id,
            access_changed_delivery_id(TenantId::new(), g, EndCause::Revoked)
        );
        assert_eq!(id.0.get_version_num(), 7);
    }

    // ------------------------------------------------------------------------ conformance

    /// What the conformance suite works on: a database `node` of `tenant`, its user `user`, an
    /// agent of that user, and another tenant.
    struct Setup {
        tenant: TenantId,
        user: UserId,
        node: NodeId,
        agent: AgentId,
        other: TenantId,
    }

    async fn holds(authz: &dyn Authz, agent: AgentId, kind: GrantKind, node: NodeId) -> bool {
        authz
            .check(
                &agent_subject(agent),
                kind.permission(),
                &ObjectRef::node(node),
                None,
            )
            .await
            .unwrap()
    }

    fn new_grant(
        s: &Setup,
        kind: GrantKind,
        start: OffsetDateTime,
        minutes: i64,
        mark: Option<RestorePointMark>,
    ) -> NewGrant {
        NewGrant {
            node: s.node,
            kind,
            agent: s.agent,
            reason: Some("conformance".into()),
            starts_at: start,
            expires_at: start + time::Duration::minutes(minutes),
            restore_point: mark,
        }
    }

    /// One conformance suite for [`MemoryGrantStore`] and [`PgGrantStore`].
    async fn conformance(
        store: &dyn GrantStore,
        authz: &dyn Authz,
        typed: &dyn TypedNodeStore,
        s: Setup,
    ) {
        let c = ctx(s.tenant, s.user);
        let t0 = micros(crate::now());
        let delivered = |id: Uuid, cause: EndCause| {
            let d = access_changed_delivery_id(s.tenant, id, cause);
            async move { typed.delivery(s.tenant, d).await.unwrap() }
        };

        // A read_personal grant: the row, the tuple and a journal entry.
        let w1 = store
            .create(
                &c,
                authz,
                new_grant(&s, GrantKind::ReadPersonal, t0, 60, None),
            )
            .await
            .unwrap();
        let g1 = w1.value.clone();
        assert!(w1.change.seq.is_some());
        assert_eq!(g1.granted_by, s.user);
        assert_eq!(
            (g1.kind, g1.agent_id, g1.node_id),
            (GrantKind::ReadPersonal, s.agent, s.node)
        );
        assert_eq!((g1.restore_point_id, g1.revoked_at), (None, None));
        assert_eq!(g1.expires_at - g1.starts_at, time::Duration::minutes(60));
        assert!(holds(authz, s.agent, GrantKind::ReadPersonal, s.node).await);
        assert!(!holds(authz, s.agent, GrantKind::WriteLive, s.node).await);
        assert_eq!(
            store.get(s.tenant, s.node, g1.id).await.unwrap(),
            Some(g1.clone())
        );

        // Another tenant sees nothing of it.
        assert_eq!(store.get(s.other, s.node, g1.id).await.unwrap(), None);
        let none = store
            .list(
                s.other,
                s.node,
                GrantFilter::default(),
                PageRequest::default(),
            )
            .await
            .unwrap();
        assert!(none.items.is_empty());
        assert_eq!(
            store
                .live(s.other, s.node, s.agent, GrantKind::ReadPersonal, t0)
                .await
                .unwrap(),
            None
        );

        // A write_live grant with its restore point row.
        let mark = RestorePointMark {
            at: t0,
            wal_lsn: None,
        };
        let g2 = store
            .create(
                &c,
                authz,
                new_grant(&s, GrantKind::WriteLive, t0, 30, Some(mark)),
            )
            .await
            .unwrap()
            .value;
        let rp = g2.restore_point_id.expect("write_live has a restore point");
        let row = store
            .restore_point(s.tenant, rp)
            .await
            .unwrap()
            .expect("the row");
        assert_eq!(row.reason, "before_write_grant");
        assert_eq!((row.node_id, row.at, row.actor), (s.node, t0, c.actor));
        assert_eq!(store.restore_point(s.other, rp).await.unwrap(), None);

        // The list: newest first, in pages, with filters.
        let p1 = store
            .list(
                s.tenant,
                s.node,
                GrantFilter::default(),
                PageRequest {
                    cursor: None,
                    limit: Some(1),
                },
            )
            .await
            .unwrap();
        assert_eq!(p1.items, vec![g2.clone()]);
        let p2 = store
            .list(
                s.tenant,
                s.node,
                GrantFilter::default(),
                PageRequest {
                    cursor: p1.next_cursor.clone(),
                    limit: Some(1),
                },
            )
            .await
            .unwrap();
        assert_eq!((p2.items, p2.next_cursor), (vec![g1.clone()], None));
        let other_agent = store
            .list(
                s.tenant,
                s.node,
                GrantFilter {
                    agent: Some(AgentId::new()),
                    live_at: None,
                },
                PageRequest::default(),
            )
            .await
            .unwrap();
        assert!(other_agent.items.is_empty());
        let live_later = store
            .list(
                s.tenant,
                s.node,
                GrantFilter {
                    agent: Some(s.agent),
                    live_at: Some(t0 + time::Duration::minutes(45)),
                },
                PageRequest::default(),
            )
            .await
            .unwrap();
        assert_eq!(live_later.items, vec![g1.clone()]);

        // Live rows, and the check with the tuple: an expired grant gives nothing although the
        // tuple still holds (no sweeper ran).
        let at = |m: i64| t0 + time::Duration::minutes(m);
        assert_eq!(
            store
                .live(s.tenant, s.node, s.agent, GrantKind::ReadPersonal, t0)
                .await
                .unwrap(),
            Some(g1.clone())
        );
        let act = |kind, when| active(store, authz, s.tenant, s.node, s.agent, kind, when);
        assert_eq!(
            act(GrantKind::ReadPersonal, t0).await.unwrap(),
            Some(g1.clone())
        );
        assert_eq!(act(GrantKind::ReadPersonal, at(61)).await.unwrap(), None);
        assert!(holds(authz, s.agent, GrantKind::ReadPersonal, s.node).await);
        assert_eq!(act(GrantKind::WriteLive, at(31)).await.unwrap(), None);
        assert_eq!(
            act(GrantKind::ReadPersonal, t0 - time::Duration::minutes(1))
                .await
                .unwrap(),
            None,
            "not before it starts"
        );

        // A revoke while another grant of the kind is live keeps the tuple and sends nothing.
        let g3 = store
            .create(
                &c,
                authz,
                new_grant(&s, GrantKind::ReadPersonal, t0, 120, None),
            )
            .await
            .unwrap()
            .value;
        let r = store
            .revoke(&c, authz, s.node, g1.id, EndCause::Revoked, crate::now())
            .await
            .unwrap();
        assert!(r.change.seq.is_some());
        assert!(r.value.revoked_at.is_some());
        assert_eq!(r.value.revoked_by, Some(c.actor.actor));
        assert_eq!(
            store.get(s.tenant, s.node, g1.id).await.unwrap(),
            Some(r.value.clone())
        );
        assert!(holds(authz, s.agent, GrantKind::ReadPersonal, s.node).await);
        assert!(delivered(g1.id, EndCause::Revoked).await.is_none());
        assert_eq!(
            code(
                store
                    .revoke(&c, authz, s.node, g1.id, EndCause::Revoked, crate::now())
                    .await
            ),
            (404, "not_found".into()),
            "an ended grant is 404, as a revoked share link (review A-1 R-2)"
        );
        assert_eq!(
            code(
                store
                    .revoke(
                        &ctx(s.other, s.user),
                        authz,
                        s.node,
                        g3.id,
                        EndCause::Revoked,
                        crate::now()
                    )
                    .await
            )
            .0,
            404
        );
        // The revoked grant gives nothing, also while the tuple holds for another grant.
        assert_eq!(
            act(GrantKind::ReadPersonal, at(90)).await.unwrap(),
            Some(g3.clone())
        );
        assert_eq!(act(GrantKind::ReadPersonal, at(121)).await.unwrap(), None);

        // The last live grant's revoke deletes the tuple and sends access_changed.
        store
            .revoke(&c, authz, s.node, g3.id, EndCause::GivenBack, crate::now())
            .await
            .unwrap();
        assert!(!holds(authz, s.agent, GrantKind::ReadPersonal, s.node).await);
        let d = delivered(g3.id, EndCause::GivenBack)
            .await
            .expect("access_changed");
        assert_eq!((d.hook, d.node_id), (HookKind::AccessChanged, Some(s.node)));
        assert_eq!(act(GrantKind::ReadPersonal, t0).await.unwrap(), None);

        // The sweeper ends the expired write_live grant once.
        assert!(store.expired_unended(at(29), 10).await.unwrap().is_empty());
        let due = store.expired_unended(at(31), 10).await.unwrap();
        assert_eq!(due.iter().map(|g| g.id).collect::<Vec<_>>(), vec![g2.id]);
        assert!(holds(authz, s.agent, GrantKind::WriteLive, s.node).await);
        assert_eq!(
            store.end_expired(authz, &due[0], at(31)).await.unwrap(),
            Some(true)
        );
        assert!(!holds(authz, s.agent, GrantKind::WriteLive, s.node).await);
        assert!(delivered(g2.id, EndCause::Expired).await.is_some());
        assert_eq!(
            store.end_expired(authz, &due[0], at(31)).await.unwrap(),
            None
        );
        assert!(store.expired_unended(at(31), 10).await.unwrap().is_empty());
        // Revoked grants are never swept.
        assert!(store.expired_unended(at(200), 10).await.unwrap().is_empty());
        // The sweeper looks back a limited window only.
        assert!(
            store
                .expired_unended(at(60 * 24 * (SWEEP_WINDOW_DAYS + 1)), 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    // ------------------------------------------------------------------------ memory

    async fn agent_of(w: &World, user: UserId, max_role: Role, subtree: Option<NodeId>) -> Agent {
        w.s.agents
            .register(
                &ctx(w.team.id, user),
                NewAgent {
                    name: format!("agent {}", Uuid::now_v7().simple()),
                    description: None,
                    scope: AgentScope {
                        subtree,
                        max_role,
                        expires_at: None,
                        budget: Budget::default(),
                    },
                },
            )
            .await
            .unwrap()
    }

    fn agent_ctx(a: &Agent) -> Context {
        Context {
            tenant: a.tenant_id,
            actor: Actor {
                actor: PrincipalRef {
                    kind: PrincipalKind::Agent,
                    id: a.id.0.into(),
                },
                on_behalf_of: Some(a.owner_id),
            },
            scope: Some(a.scope.as_scope()),
            idempotency_key: None,
        }
    }

    async fn database(w: &World, name: &str) -> NodeId {
        let a = ctx(w.team.id, w.alice);
        let id = w
            .s
            .databases
            .create(
                &a,
                CreateDatabase {
                    parent_id: w.folder,
                    name: name.into(),
                    manifest: json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" }),
                    source: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
        assert_eq!(
            settled(&w.s, &a, id).await.status.state,
            ProviderState::Ready
        );
        id
    }

    #[tokio::test]
    async fn memory_store_conformance() {
        let w = world().await;
        let node = database(&w, "garden").await;
        let agent = agent_of(&w, w.alice, Role::Edit, None).await;
        let other =
            w.s.identity
                .upsert_from_login(&crate::identity::VerifiedLogin {
                    issuer: "https://test.ruum.localhost".into(),
                    subject: format!("frank-{}", Uuid::now_v7()),
                    email: Some(format!("frank-{}@example.test", Uuid::now_v7().simple())),
                    email_verified: true,
                    name: Some("Frank".into()),
                    id_token: None,
                })
                .await
                .unwrap()
                .home_tenant_id;
        let d = w.s.databases.deps();
        conformance(
            &*d.grants,
            &*d.authz,
            &*d.typed,
            Setup {
                tenant: w.team.id,
                user: w.alice,
                node,
                agent: agent.id,
                other,
            },
        )
        .await;
    }

    fn create_req(kind: GrantKind, agent: AgentId, minutes: i64) -> CreateGrant {
        CreateGrant {
            kind,
            agent_id: agent,
            duration_minutes: minutes,
            reason: Some("tests".into()),
        }
    }

    /// The rules of `createDatabaseAccessGrant` on the in-memory services.
    #[tokio::test]
    async fn only_the_agents_user_grants_within_the_limits() {
        let w = world().await;
        let node = database(&w, "garden").await;
        let s = &w.s;
        let alice = ctx(w.team.id, w.alice);
        let bob = ctx(w.team.id, w.bob);
        let a_edit = agent_of(&w, w.alice, Role::Edit, None).await;
        let a_view = agent_of(&w, w.alice, Role::View, None).await;
        let a_bob = agent_of(&w, w.bob, Role::Edit, None).await;
        let create = |c: Context, kind, agent, minutes| async move {
            s.databases
                .create_grant(&c, &*s.agents, node, create_req(kind, agent, minutes))
                .await
        };
        // Another person (Bob has edit on the database) may not grant Alice's agent.
        assert_eq!(
            code(create(bob.clone(), GrantKind::ReadPersonal, a_edit.id, 60).await),
            (403, "permission_denied".into())
        );
        // An unknown agent gets the same answer.
        assert_eq!(
            code(create(alice.clone(), GrantKind::ReadPersonal, AgentId::new(), 60).await).0,
            403
        );
        // Agents never grant (users only), also to themselves.
        assert_eq!(
            code(create(agent_ctx(&a_edit), GrantKind::ReadPersonal, a_edit.id, 60).await),
            (403, "user_required".into())
        );
        // The limits.
        for (kind, minutes) in [
            (GrantKind::ReadPersonal, 481),
            (GrantKind::WriteLive, 1441),
            (GrantKind::ReadPersonal, 0),
        ] {
            assert_eq!(
                code(create(alice.clone(), kind, a_edit.id, minutes).await),
                (422, "validation_failed".into()),
                "{kind:?} {minutes}"
            );
        }
        // write_live needs an agent scope of edit.
        assert_eq!(
            code(create(alice.clone(), GrantKind::WriteLive, a_view.id, 60).await).0,
            403
        );
        // read_personal for the longest time; the names are filled.
        let rp = create(alice.clone(), GrantKind::ReadPersonal, a_view.id, 480)
            .await
            .unwrap()
            .value;
        assert_eq!(rp.restore_point_id, None);
        assert_eq!(rp.agent_display_name.as_deref(), Some(a_view.name.as_str()));
        assert!(rp.granted_by_display_name.is_some());
        // write_live records a restore point first.
        let wl = create(alice.clone(), GrantKind::WriteLive, a_edit.id, 1440)
            .await
            .unwrap()
            .value;
        let point = wl.restore_point_id.expect("a restore point");
        let row = s
            .databases
            .deps()
            .grants
            .restore_point(w.team.id, point)
            .await
            .unwrap()
            .expect("the restore point row");
        assert_eq!(row.reason, "before_write_grant");
        assert_eq!(row.actor, alice.actor);
        // Bob's own agent: Bob gives it a grant (he has view and edit).
        create(bob.clone(), GrantKind::WriteLive, a_bob.id, 30)
            .await
            .unwrap();
        // Alice's journal has the two entries of her grants.
        let created = w
            .handles
            .journal
            .entries(w.team.id)
            .into_iter()
            .filter(|e| e.event == EventType::DatabaseGrantCreated)
            .count();
        assert_eq!(created, 3);
    }

    /// Review A-1 R-3: a database that is not ready (no `databases` row: still provisioning,
    /// or its create failed) refuses both kinds with `409`, before the store is called. On
    /// Postgres the grant row's foreign key to `databases` would otherwise fail as a `500`.
    #[tokio::test]
    async fn a_database_that_is_not_ready_refuses_both_kinds() {
        let w = world().await;
        let node = database(&w, "garden").await;
        let s = &w.s;
        let alice = ctx(w.team.id, w.alice);
        let a = agent_of(&w, w.alice, Role::Edit, None).await;
        s.databases
            .deps()
            .store
            .delete(w.team.id, node)
            .await
            .unwrap()
            .expect("the databases row");
        for kind in [GrantKind::ReadPersonal, GrantKind::WriteLive] {
            assert_eq!(
                code(
                    s.databases
                        .create_grant(&alice, &*s.agents, node, create_req(kind, a.id, 30))
                        .await
                ),
                (409, "conflict".into()),
                "{kind:?}"
            );
        }
        let rows = s
            .databases
            .list_grants(&alice, node, false, PageRequest::default())
            .await
            .unwrap();
        assert!(rows.items.is_empty(), "no grant row: {:?}", rows.items);
    }

    /// The check, the sweeper and the revoke rules on the in-memory services.
    #[tokio::test]
    async fn expired_or_revoked_grants_give_nothing_before_the_sweeper() {
        let w = world().await;
        let node = database(&w, "garden").await;
        let s = &w.s;
        let alice = ctx(w.team.id, w.alice);
        let a = agent_of(&w, w.alice, Role::Edit, None).await;
        let other = agent_of(&w, w.bob, Role::Edit, None).await;
        let t = w.team.id;
        let give = |kind, minutes| {
            let alice = alice.clone();
            async move {
                s.databases
                    .create_grant(&alice, &*s.agents, node, create_req(kind, a.id, minutes))
                    .await
                    .unwrap()
                    .value
            }
        };
        let later = |m: i64| crate::now() + time::Duration::minutes(m);
        let rp = GrantKind::ReadPersonal;

        // Expiry: nothing after the end, while the tuple still holds.
        let g = give(GrantKind::WriteLive, 1).await;
        assert_eq!(
            s.databases
                .active_grant(t, node, a.id, GrantKind::WriteLive)
                .await
                .unwrap()
                .map(|x| x.id),
            Some(g.id)
        );
        assert_eq!(
            s.databases
                .active_grant_at(t, node, a.id, GrantKind::WriteLive, later(2))
                .await
                .unwrap(),
            None
        );
        assert!(holds(&*s.authz, a.id, GrantKind::WriteLive, node).await);
        // The sweeper deletes the tuple once and writes one entry.
        let round = s.databases.sweep_grants_at(later(2)).await.unwrap();
        assert_eq!(
            round,
            SweepRound {
                ended: 1,
                tuples_deleted: 1
            }
        );
        assert!(!holds(&*s.authz, a.id, GrantKind::WriteLive, node).await);
        assert_eq!(
            s.databases.sweep_grants_at(later(3)).await.unwrap(),
            SweepRound::default()
        );
        let expired: Vec<_> = w
            .handles
            .journal
            .entries(t)
            .into_iter()
            .filter(|e| e.event == EventType::DatabaseGrantRevoked)
            .collect();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].data["cause"], "expired");
        assert_eq!(expired[0].actor, Actor::user(w.alice));

        // Revoke: nothing after it, also while another grant keeps the tuple.
        let long = give(rp, 120).await;
        let short = give(rp, 1).await;
        s.databases
            .revoke_grant(&alice, node, long.id)
            .await
            .unwrap();
        assert!(
            holds(&*s.authz, a.id, rp, node).await,
            "short still holds the tuple"
        );
        assert_eq!(
            s.databases
                .active_grant(t, node, a.id, rp)
                .await
                .unwrap()
                .map(|x| x.id),
            Some(short.id)
        );
        assert_eq!(
            s.databases
                .active_grant_at(t, node, a.id, rp, later(30))
                .await
                .unwrap(),
            None,
            "the long grant is revoked, the short one has ended"
        );

        // Who may end a grant.
        let bob = ctx(t, w.bob);
        let g = give(rp, 60).await;
        assert_eq!(
            code(s.databases.revoke_grant(&bob, node, g.id).await),
            (403, "permission_denied".into()),
            "Bob has edit, did not give it and does not hold it"
        );
        assert_eq!(
            code(
                s.databases
                    .revoke_grant(&agent_ctx(&other), node, g.id)
                    .await
            )
            .0,
            403,
            "another agent"
        );
        let back = s
            .databases
            .revoke_grant(&agent_ctx(&a), node, g.id)
            .await
            .unwrap();
        assert_eq!(
            back.value.revoked_by.map(|p| p.kind),
            Some(PrincipalKind::Agent)
        );
        let last = w.handles.journal.entries(t).pop().unwrap();
        assert_eq!(last.data["cause"], "given_back");
        assert_eq!(
            code(s.databases.revoke_grant(&alice, node, g.id).await),
            (404, "not_found".into())
        );
        assert_eq!(
            code(s.databases.revoke_grant(&alice, node, Uuid::now_v7()).await).0,
            404
        );
        assert_eq!(
            s.databases
                .active_grant(t, node, a.id, rp)
                .await
                .unwrap()
                .map(|x| x.id),
            Some(short.id)
        );

        // An agent lists only its own grants; a person lists all.
        let all = s
            .databases
            .list_grants(&alice, node, false, PageRequest::default())
            .await
            .unwrap();
        assert_eq!(all.items.len(), 4);
        let own = s
            .databases
            .list_grants(&agent_ctx(&other), node, false, PageRequest::default())
            .await
            .unwrap();
        assert!(own.items.is_empty());
        let mine = s
            .databases
            .list_grants(&agent_ctx(&a), node, true, PageRequest::default())
            .await
            .unwrap();
        // Live now: the short grant (and the write_live grant, which the sweeper ended only in
        // the future of this test).
        assert!(
            mine.items
                .iter()
                .all(|g| g.agent_id == a.id && g.revoked_at.is_none())
        );
        assert_eq!(
            mine.items
                .iter()
                .filter(|g| g.kind == rp)
                .map(|g| g.id)
                .collect::<Vec<_>>(),
            vec![short.id]
        );
    }

    // ------------------------------------------------------------------------ Postgres

    /// A tenant with a user, a ready database node (its `databases` row) and an agent of the
    /// user, as the system writes them.
    async fn seed_pg(pool: &sqlx::PgPool) -> (TenantId, UserId, NodeId, AgentId) {
        let (t, u, root, node, v, agent) = (
            TenantId::new(),
            UserId::new(),
            NodeId::new(),
            NodeId::new(),
            crate::ids::VersionId::new(),
            AgentId::new(),
        );
        let manifest = json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" });
        let (bytes, hash) = crate::nodes::typed_manifest_bytes(&manifest);
        let mut tx = pool.begin().await.unwrap();
        for sql in [
            "set local ruum.system = 'on'",
            "set constraints all deferred",
        ] {
            sqlx::query(sql).execute(&mut *tx).await.unwrap();
        }
        sqlx::query(
            "insert into tenants (id, name, kind, plan, root_node_id, created_kind, created_by) \
             values ($1, 't', 'team', 'team', $2, 'user', $3)",
        )
        .bind(t)
        .bind(root)
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "insert into users (id, tenant_id, email, email_verified, display_name) \
             values ($1, $2, $3, true, 'x')",
        )
        .bind(u.0)
        .bind(t)
        .bind(format!("{}@example.test", u.0))
        .execute(&mut *tx)
        .await
        .unwrap();
        for (id, parent, kind, ty, head) in [
            (root, None, "folder", None, None),
            (
                node,
                Some(root),
                "typed",
                Some(crate::databases::NODE_TYPE),
                Some(v),
            ),
        ] {
            sqlx::query(
                "insert into nodes (id, tenant_id, parent_id, name, kind, type, head_version, \
                 owner_id, created_kind, created_by, updated_kind, updated_by) \
                 values ($1, $2, $3, $4, $5::node_kind, $6, $7, $8, 'user', $8, 'user', $8)",
            )
            .bind(id)
            .bind(t)
            .bind(parent)
            .bind(id.to_string())
            .bind(kind)
            .bind(ty)
            .bind(head)
            .bind(u.0)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        sqlx::query(
            "insert into versions (id, tenant_id, node_id, manifest, encoding, size_bytes, \
             content_hash, actor_kind, actor) values ($1, $2, $3, $4, 'typed_manifest_v1', $5, \
             $6, 'user', $7)",
        )
        .bind(v)
        .bind(t)
        .bind(node)
        .bind(&bytes)
        .bind(bytes.len() as i64)
        .bind(hash.to_vec())
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        crate::providers::postgres::insert_typed_node(
            &mut tx,
            &crate::providers::store::NewTypedNode {
                tenant_id: t,
                node_id: node,
                provider: crate::databases::PROVIDER_ID.into(),
                type_name: crate::databases::NODE_TYPE.into(),
                desired_version: v,
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "insert into agents (id, tenant_id, owner_id, name, scope_max_role, created_kind, \
             created_by) values ($1, $2, $3, 'agent', 'edit', 'user', $3)",
        )
        .bind(agent.0)
        .bind(t)
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        crate::databases::PgDatabaseStore::new(pool.clone())
            .insert(
                &crate::databases::DatabaseRow {
                    tenant_id: t,
                    node_id: node,
                    engine: ruum_dbengine::EngineKind::Sqlite,
                    engine_version: "3".into(),
                    cluster: "memory".into(),
                    engine_name: format!("d_{}", &Uuid::now_v7().simple().to_string()[16..]),
                    branch_of: None,
                    branch_data: None,
                    key_node_id: None,
                    extensions: Vec::new(),
                    size_bytes: 4096,
                },
                &[],
            )
            .await
            .unwrap();
        (t, u, node, agent)
    }

    use crate::databases::DatabaseStore as _;

    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store_conformance(pool: sqlx::PgPool) {
        let (t, u, node, agent) = seed_pg(&pool).await;
        let (other, ..) = seed_pg(&pool).await;
        conformance(
            &PgGrantStore::new(pool.clone()),
            &MemoryAuthz::default(),
            &crate::providers::postgres::PgTypedNodeStore::new(pool.clone()),
            Setup {
                tenant: t,
                user: u,
                node,
                agent,
                other,
            },
        )
        .await;
    }

    /// The Postgres store keeps every row with its tenant, with row-level security off, so only
    /// the tenant filters of its statements hold (the mutation
    /// `databases-grants-tenant-filter-postgres` breaks one).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_grant_store_filters_by_tenant_without_rls(pool: sqlx::PgPool) {
        for table in ["database_access_grants", "database_restore_points"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "alter table {table} no force row level security"
            )))
            .execute(&pool)
            .await
            .unwrap();
        }
        let store = PgGrantStore::new(pool.clone());
        let authz = MemoryAuthz::default();
        let (a, u, node, agent) = seed_pg(&pool).await;
        let (b, ..) = seed_pg(&pool).await;
        let now = micros(crate::now());
        let g = store
            .create(
                &ctx(a, u),
                &authz,
                NewGrant {
                    node,
                    kind: GrantKind::WriteLive,
                    agent,
                    reason: None,
                    starts_at: now,
                    expires_at: now + time::Duration::minutes(5),
                    restore_point: Some(RestorePointMark {
                        at: now,
                        wal_lsn: None,
                    }),
                },
            )
            .await
            .unwrap()
            .value;
        assert!(store.get(a, node, g.id).await.unwrap().is_some());
        assert_eq!(store.get(b, node, g.id).await.unwrap(), None);
        let page = store
            .list(b, node, GrantFilter::default(), PageRequest::default())
            .await
            .unwrap();
        assert!(page.items.is_empty());
        assert_eq!(
            store
                .live(b, node, agent, GrantKind::WriteLive, now)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .restore_point(b, g.restore_point_id.unwrap())
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            code(
                store
                    .revoke(
                        &ctx(b, u),
                        &authz,
                        node,
                        g.id,
                        EndCause::Revoked,
                        crate::now()
                    )
                    .await
            )
            .0,
            404
        );
    }

    /// Row-level security: a transaction of another tenant sees no grant and no restore point.
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_rls_hides_the_rows_of_other_tenants(pool: sqlx::PgPool) {
        let store = PgGrantStore::new(pool.clone());
        let (a, u, node, agent) = seed_pg(&pool).await;
        let (b, ..) = seed_pg(&pool).await;
        let now = micros(crate::now());
        store
            .create(
                &ctx(a, u),
                &MemoryAuthz::default(),
                NewGrant {
                    node,
                    kind: GrantKind::WriteLive,
                    agent,
                    reason: None,
                    starts_at: now,
                    expires_at: now + time::Duration::minutes(5),
                    restore_point: Some(RestorePointMark {
                        at: now,
                        wal_lsn: None,
                    }),
                },
            )
            .await
            .unwrap();
        for (tenant, want) in [(a, 1i64), (b, 0)] {
            let mut tx = crate::journal::postgres::begin_tenant(&pool, tenant)
                .await
                .unwrap();
            for table in ["database_access_grants", "database_restore_points"] {
                let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "select count(*) from {table}"
                )))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
                assert_eq!(n, want, "{table} as tenant {tenant}");
            }
        }
    }

    async fn count_system(pool: &sqlx::PgPool, sql: &str, tenant: TenantId) -> i64 {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
            .bind(tenant.0)
            .fetch_one(&mut *tx)
            .await
            .unwrap()
    }

    /// The restore point row, the grant row, the outbox tuple and the journal entry are one
    /// transaction: all of them, or none.
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_write_live_is_one_transaction(pool: sqlx::PgPool) {
        let store = PgGrantStore::new(pool.clone());
        let authz = MemoryAuthz::default();
        let (a, u, node, agent) = seed_pg(&pool).await;
        let now = micros(crate::now());
        let grant = |agent| NewGrant {
            node,
            kind: GrantKind::WriteLive,
            agent,
            reason: None,
            starts_at: now,
            expires_at: now + time::Duration::minutes(5),
            restore_point: Some(RestorePointMark {
                at: now,
                wal_lsn: None,
            }),
        };
        let counts = || async {
            (
                count_system(&pool, "select count(*) from database_restore_points where tenant_id = $1", a).await,
                count_system(&pool, "select count(*) from database_access_grants where tenant_id = $1", a).await,
                count_system(&pool, "select count(*) from authz_outbox where tenant_id = $1", a).await,
                count_system(
                    &pool,
                    "select count(*) from journal where tenant_id = $1 and type = 'database.grant_created'",
                    a,
                )
                .await,
            )
        };
        // An agent that does not exist fails the grant row (foreign key): nothing stays.
        assert!(
            store
                .create(&ctx(a, u), &authz, grant(AgentId::new()))
                .await
                .is_err()
        );
        assert_eq!(counts().await, (0, 0, 0, 0));
        let g = store
            .create(&ctx(a, u), &authz, grant(agent))
            .await
            .unwrap()
            .value;
        assert_eq!(counts().await, (1, 1, 1, 1));
        let linked = count_system(
            &pool,
            &format!(
                "select count(*) from database_access_grants g join database_restore_points p \
                 on p.tenant_id = g.tenant_id and p.id = g.restore_point_id \
                 where g.tenant_id = $1 and p.reason = 'before_write_grant' and g.id = '{}'",
                g.id
            ),
            a,
        )
        .await;
        assert_eq!(linked, 1);
        let updates: Value = {
            let mut tx = crate::journal::postgres::begin_tenant(&pool, a)
                .await
                .unwrap();
            sqlx::query_scalar("select updates from authz_outbox where tenant_id = $1")
                .bind(a.0)
                .fetch_one(&mut *tx)
                .await
                .unwrap()
        };
        let want = serde_json::to_value(vec![RelationshipUpdate::Touch(tuple(
            node,
            GrantKind::WriteLive,
            agent,
        ))])
        .unwrap();
        assert_eq!(updates, want);
        assert!(
            holds(&authz, agent, GrantKind::WriteLive, node).await,
            "applied after the commit"
        );
    }

    /// Review A-1 R-1: two revokes at the same time of the two live grants of one tuple. Each
    /// must see the other's commit, so the second one deletes the tuple. Without the lock of the
    /// tuple, most rounds left the tuple with no live grant, for ever (the sweeper skips revoked
    /// rows).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_concurrent_revokes_leave_no_tuple(pool: sqlx::PgPool) {
        let store = PgGrantStore::new(pool.clone());
        let authz = MemoryAuthz::default();
        let (t, u, node, agent) = seed_pg(&pool).await;
        let (rounds, mut leaks) = (20, 0);
        for _ in 0..rounds {
            let now = micros(crate::now());
            let mk = || NewGrant {
                node,
                kind: GrantKind::ReadPersonal,
                agent,
                reason: None,
                starts_at: now,
                expires_at: now + time::Duration::minutes(60),
                restore_point: None,
            };
            let c = ctx(t, u);
            let g1 = store.create(&c, &authz, mk()).await.unwrap().value;
            let g2 = store.create(&c, &authz, mk()).await.unwrap().value;
            let (r1, r2) = tokio::join!(
                store.revoke(&c, &authz, node, g1.id, EndCause::Revoked, crate::now()),
                store.revoke(&c, &authz, node, g2.id, EndCause::Revoked, crate::now()),
            );
            r1.unwrap();
            r2.unwrap();
            if holds(&authz, agent, GrantKind::ReadPersonal, node).await {
                leaks += 1;
                authz
                    .write(vec![RelationshipUpdate::Delete(tuple(
                        node,
                        GrantKind::ReadPersonal,
                        agent,
                    ))])
                    .await
                    .unwrap();
            }
        }
        assert_eq!(
            leaks, 0,
            "{leaks} of {rounds} rounds left the tuple with no live grant"
        );
    }

    /// Review A-1 R-1: a create during a revoke of the same tuple. The new grant is live, so
    /// its tuple must stay, whichever write takes the lock first; and after the new grant is
    /// revoked too, the tuple goes.
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_create_during_revoke_keeps_the_tuple(pool: sqlx::PgPool) {
        let store = PgGrantStore::new(pool.clone());
        let authz = MemoryAuthz::default();
        let (t, u, node, agent) = seed_pg(&pool).await;
        let c = ctx(t, u);
        let mk = || {
            let now = micros(crate::now());
            NewGrant {
                node,
                kind: GrantKind::ReadPersonal,
                agent,
                reason: None,
                starts_at: now,
                expires_at: now + time::Duration::minutes(60),
                restore_point: None,
            }
        };
        let (rounds, mut lost) = (20, 0);
        for _ in 0..rounds {
            let a = store.create(&c, &authz, mk()).await.unwrap().value;
            let (r, b) = tokio::join!(
                store.revoke(&c, &authz, node, a.id, EndCause::Revoked, crate::now()),
                store.create(&c, &authz, mk()),
            );
            r.unwrap();
            let b = b.unwrap().value;
            if !holds(&authz, agent, GrantKind::ReadPersonal, node).await {
                lost += 1;
            }
            store
                .revoke(&c, &authz, node, b.id, EndCause::Revoked, crate::now())
                .await
                .unwrap();
            assert!(
                !holds(&authz, agent, GrantKind::ReadPersonal, node).await,
                "no live grant is left, so the tuple goes"
            );
        }
        assert_eq!(
            lost, 0,
            "{lost} of {rounds} rounds left the new live grant without its tuple"
        );
    }

    // ------------------------------------------------------------------------ the engine

    /// `write_live` on the Postgres engine: the engine's restore point (with its LSN) comes
    /// before the grant.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn write_live_takes_a_restore_point_on_the_postgres_engine() {
        let required = std::env::var_os("CI").is_some()
            || std::env::var_os("RUUM_DBENGINE_REQUIRE_POSTGRES").is_some();
        let Some(config) = ruum_dbengine::postgres::PostgresConfig::from_env() else {
            assert!(
                !required,
                "RUUM_DBHOST_URL is not set: this test needs the data cluster"
            );
            eprintln!("skipped: RUUM_DBHOST_URL is not set");
            return;
        };
        let engine = match ruum_dbengine::postgres::PostgresEngine::connect(config).await {
            Ok(e) => Arc::new(e) as Arc<dyn ruum_dbengine::DatabaseEngine>,
            Err(e) => {
                assert!(!required, "the data cluster: {e}");
                eprintln!("skipped: the data cluster does not answer: {e}");
                return;
            }
        };
        let mut w = world().await;
        let parts = DatabaseParts {
            engines: Engines {
                postgres: Some(engine.clone()),
                sqlite: None,
            },
            keys: w.s.tenant_keys.clone(),
            store: w.handles.databases.clone(),
            typed: w.handles.typed.clone(),
            columns: w.s.databases.deps().columns.clone(),
            masking: w.s.databases.deps().masking.clone(),
            grants: w.s.databases.deps().grants.clone(),
            queries: Arc::new(crate::databases::query::MemoryQueryLog::new()),
        };
        install(&mut w.s, parts).unwrap();
        let a = ctx(w.team.id, w.alice);
        let node = w
            .s
            .databases
            .create(
                &a,
                CreateDatabase {
                    parent_id: w.folder,
                    name: "pg garden".into(),
                    manifest: json!({ "engine": "postgres", "version": "17", "plan": "shared-small" }),
                    source: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
        assert_eq!(
            settled(&w.s, &a, node).await.status.state,
            ProviderState::Ready
        );
        let agent = agent_of(&w, w.alice, Role::Edit, None).await;
        let g =
            w.s.databases
                .create_grant(
                    &a,
                    &*w.s.agents,
                    node,
                    create_req(GrantKind::WriteLive, agent.id, 60),
                )
                .await
                .unwrap()
                .value;
        let row =
            w.s.databases
                .deps()
                .grants
                .restore_point(w.team.id, g.restore_point_id.unwrap())
                .await
                .unwrap()
                .unwrap();
        assert!(row.wal_lsn.is_some(), "the Postgres engine gives an LSN");
        assert!(row.at <= g.starts_at + time::Duration::seconds(1));
        let db = w
            .handles
            .databases
            .get(w.team.id, node)
            .await
            .unwrap()
            .unwrap();
        let _ = engine.drop_database(&db.database_ref()).await;
    }
}
