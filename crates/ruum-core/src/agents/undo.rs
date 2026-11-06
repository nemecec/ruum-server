//! Agent activity and undo (`listAgentActivity`, `undoAgent`; design sections 3.4 and 11).
//!
//! Every change of an agent is a journal entry with the agent as `actor`, so "undo everything
//! agent X did since 14:00" reads those entries, newest first, and applies the inverse of each
//! one through the store of its area, as the user who undoes (the undo is a set of new changes by
//! the caller). The table of inverses is [`super::undo_action`]; [`invert`] has one arm per event
//! type.
//!
//! An entry is skipped with a reason:
//! - `irreversible`: the event has no inverse (`node.purged`, `share_link.revoked`), or the data
//!   for it is missing (a first version has no version before it);
//! - `changed_later`: another actor changed the same node after the entry, or the inverse
//!   conflicts with the current state (for example the old name is taken);
//! - `permission_denied`: the caller may not do the inverse.
//!
//! # How an area adds the inverse of a new event type
//!
//! 1. Add the event type to `crate::journal::EventType` and its schema to `contracts/events/`
//!    (a contract change).
//! 2. Add the inverse to [`super::undo_action`] (the table of the `undoAgent` description).
//! 3. Add an arm to [`invert`] that calls the store method of the area with the caller's
//!    context and the data of the entry, and returns the `ChangeInfo` of the write. The store
//!    method does its own permission check; its `permission_denied`, `not_found` and
//!    `scope_exceeded` become the skip reason `permission_denied`, other API errors become
//!    `changed_later`.
//! 4. Add a case to the tests of this module.

use super::{AgentId, UndoItem, UndoReport, UndoRequest, UndoSkipReason, undo_action};
use crate::actor::{Context, require_user_actor};
use crate::authz::Role;
use crate::error::{Error, ErrorCode, Result};
use crate::ids::{GrantId, NodeId, ShareLinkId, TagId, VersionId};
use crate::journal::{ChangeInfo, EventType, Journal, JournalEntry};
use crate::nodes::{MoveRequest, OnConflict, RestoreRequest};
use crate::services::Services;
use crate::sharing::Grantee;
use serde_json::Value;
use std::collections::HashMap;
use time::OffsetDateTime;
use uuid::Uuid;

/// The page size of journal reads.
const READ_PAGE: u32 = 1000;
/// The most entries one undo request handles.
pub const MAX_UNDO_ENTRIES: usize = 1000;

/// Entries whose `at` is up to this much older than `since` are read too, as a margin for the
/// clocks of the servers that wrote them (`seq` order and `at` order can differ a little).
const CLOCK_MARGIN_SECONDS: i64 = 300;

/// Reads the journal of `ctx.tenant` after `after` (oldest first).
async fn read_all(
    journal: &dyn Journal,
    ctx: &Context,
    after: Option<i64>,
) -> Result<Vec<JournalEntry>> {
    let mut out = Vec::new();
    let mut after = after;
    loop {
        let page = journal.read(ctx, after, READ_PAGE).await?;
        let Some(last) = page.items.last().map(|e| e.seq) else {
            break;
        };
        let more = page.next_cursor.is_some();
        out.extend(page.items);
        if !more {
            break;
        }
        after = Some(last);
    }
    Ok(out)
}

/// The `after` cursor from which the journal holds every entry at or after `since` (less the
/// clock margin): a binary search over `seq` with reads of one entry, so an undo or an activity
/// read does not read the history before `since`. `None` reads from the start.
async fn after_for_time(
    journal: &dyn Journal,
    ctx: &Context,
    since: OffsetDateTime,
) -> Result<Option<i64>> {
    let since = since - time::Duration::seconds(CLOCK_MARGIN_SECONDS);
    // Invariant: every entry with seq <= lo is before `since`; the first entry after hi is not.
    let mut lo: i64 = 0;
    let mut hi: i64 = journal.latest_seq(ctx).await?;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let page = journal.read(ctx, Some(mid), 1).await?;
        match page.items.first() {
            Some(e) if e.at < since => lo = e.seq,
            _ => hi = mid,
        }
    }
    Ok((lo > 0).then_some(lo))
}

/// The journal entries where `agent` is the actor, newest first, between `since` and `until`.
pub async fn entries_of(
    journal: &dyn Journal,
    ctx: &Context,
    agent: AgentId,
    since: Option<OffsetDateTime>,
    until: Option<OffsetDateTime>,
) -> Result<Vec<JournalEntry>> {
    let after = match since {
        Some(t) => after_for_time(journal, ctx, t).await?,
        None => None,
    };
    let mut v: Vec<JournalEntry> = read_all(journal, ctx, after)
        .await?
        .into_iter()
        .filter(|e| e.actor.actor.id.0 == agent.0)
        .filter(|e| since.is_none_or(|s| e.at >= s))
        .filter(|e| until.is_none_or(|u| e.at <= u))
        .collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.seq));
    Ok(v)
}

/// Undoes what an agent did (`undoAgent`). Users only; the owner of the agent or a tenant admin.
pub async fn undo(
    s: &Services,
    ctx: &Context,
    id: AgentId,
    req: UndoRequest,
) -> Result<UndoReport> {
    require_user_actor(ctx)?;
    // The owner or a tenant admin; others get `not_found` from `get`.
    let agent = s.agents.get(ctx, id).await?;
    if let Some(seqs) = &req.seqs {
        if seqs.is_empty() || seqs.len() > MAX_UNDO_ENTRIES {
            return Err(crate::identity::invalid(
                "seqs must hold 1 to 1000 sequence numbers",
            ));
        }
    } else if req.since.is_none() {
        return Err(crate::identity::invalid(
            "give since (and optionally until), or seqs",
        ));
    }
    if let (Some(a), Some(b)) = (req.since, req.until)
        && a > b
    {
        return Err(crate::identity::invalid("since must be before until"));
    }

    // Only the part of the journal from the first entry that can be undone: the selected
    // entries, and the later entries of other actors (for `changed_later`).
    let after = match (&req.seqs, req.since) {
        (Some(seqs), _) => seqs
            .iter()
            .min()
            .map(|m| m.saturating_sub(1))
            .filter(|m| *m > 0),
        (None, Some(since)) => after_for_time(s.journal.as_ref(), ctx, since).await?,
        (None, None) => None,
    };
    let all = read_all(s.journal.as_ref(), ctx, after).await?;
    let mut entries: Vec<&JournalEntry> = all
        .iter()
        .filter(|e| e.actor.actor.id.0 == agent.id.0)
        .filter(|e| match &req.seqs {
            Some(seqs) => seqs.contains(&e.seq),
            None => req.since.is_none_or(|s| e.at >= s) && req.until.is_none_or(|u| e.at <= u),
        })
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.seq));
    entries.truncate(MAX_UNDO_ENTRIES);

    // For `changed_later`: the newest seq of another actor per node.
    let mut last_other: HashMap<NodeId, i64> = HashMap::new();
    for e in &all {
        if let Some(n) = e.node_id
            && e.actor.actor.id.0 != agent.id.0
        {
            let v = last_other.entry(n).or_insert(e.seq);
            *v = (*v).max(e.seq);
        }
    }

    let mut report = UndoReport {
        dry_run: req.dry_run,
        reverted: Vec::new(),
        skipped: Vec::new(),
        change: None,
    };
    for e in entries {
        let item = |reason| UndoItem {
            seq: e.seq,
            node_id: e.node_id,
            event: e.event,
            reason,
        };
        if undo_action(e.event).is_none() {
            report
                .skipped
                .push(item(Some(UndoSkipReason::Irreversible)));
            continue;
        }
        if e.node_id
            .and_then(|n| last_other.get(&n))
            .is_some_and(|seq| *seq > e.seq)
        {
            report
                .skipped
                .push(item(Some(UndoSkipReason::ChangedLater)));
            continue;
        }
        if req.dry_run {
            report.reverted.push(item(None));
            continue;
        }
        match invert(s, ctx, e).await {
            Ok(Some(change)) => {
                report.change = Some(change);
                report.reverted.push(item(None));
            }
            Ok(None) => report
                .skipped
                .push(item(Some(UndoSkipReason::Irreversible))),
            Err(Error::Api(api)) => {
                let reason = match api.code {
                    ErrorCode::PermissionDenied
                    | ErrorCode::NotFound
                    | ErrorCode::ScopeExceeded
                    | ErrorCode::UserRequired => UndoSkipReason::PermissionDenied,
                    _ => UndoSkipReason::ChangedLater,
                };
                report.skipped.push(item(Some(reason)));
            }
            Err(other) => return Err(other),
        }
    }
    if !req.dry_run {
        s.audit
            .record(
                ctx,
                crate::identity::audit_entry(
                    "agent.undo",
                    "agent",
                    agent.id,
                    serde_json::json!({
                        "reverted": report.reverted.len(),
                        "skipped": report.skipped.len(),
                    }),
                ),
            )
            .await?;
    }
    Ok(report)
}

fn field<'a>(data: &'a Value, key: &str) -> Result<&'a Value> {
    data.get(key)
        .filter(|v| !v.is_null())
        .ok_or_else(|| Error::Internal(format!("journal entry without `{key}`")))
}

fn uuid_field(data: &Value, key: &str) -> Result<Uuid> {
    field(data, key)?
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Error::Internal(format!("journal entry: `{key}` is not a UUID")))
}

fn str_field(data: &Value, key: &str) -> Result<String> {
    field(data, key)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Internal(format!("journal entry: `{key}` is not a string")))
}

fn json_field<T: serde::de::DeserializeOwned>(data: &Value, key: &str) -> Result<T> {
    serde_json::from_value(field(data, key)?.clone())
        .map_err(|e| Error::Internal(format!("journal entry: `{key}`: {e}")))
}

/// The optional UUID `key` (`None` when absent or `null`).
fn opt_uuid(data: &Value, key: &str) -> Option<Uuid> {
    data.get(key)
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
}

/// Applies the inverse of one entry as the caller in `ctx`. `Ok(None)` when the entry has no
/// inverse after all (for example the first version of a file).
pub async fn invert(s: &Services, ctx: &Context, e: &JournalEntry) -> Result<Option<ChangeInfo>> {
    use EventType::*;
    let node = || {
        e.node_id
            .ok_or_else(|| Error::Internal("journal entry without node".into()))
    };
    let d = &e.data;
    let change = match e.event {
        NodeCreated | NodeCopied | NodeRestored => s.nodes.trash(ctx, node()?, None).await?.change,
        NodeRenamed => {
            s.nodes
                .rename(
                    ctx,
                    node()?,
                    str_field(d, "old_name")?,
                    OnConflict::Fail,
                    None,
                )
                .await?
                .change
        }
        NodeMoved => {
            s.nodes
                .move_node(
                    ctx,
                    node()?,
                    MoveRequest {
                        parent_id: NodeId(uuid_field(d, "old_parent_id")?),
                        name: Some(str_field(d, "old_name")?),
                        on_conflict: OnConflict::Fail,
                        if_revision: None,
                    },
                )
                .await?
                .change
        }
        NodeTrashed => {
            s.nodes
                .restore(ctx, node()?, RestoreRequest::default())
                .await?
                .change
        }
        NodePurged | ShareLinkRevoked => return Ok(None),
        VersionCreated | VersionRestored => match opt_uuid(d, "previous_version_id") {
            Some(prev) => {
                s.versions
                    .restore(ctx, node()?, VersionId(prev))
                    .await?
                    .change
            }
            None => return Ok(None),
        },
        VersionLabeled => {
            let old: Option<String> = d
                .get("old_label")
                .and_then(Value::as_str)
                .map(str::to_string);
            s.versions
                .set_label(ctx, node()?, VersionId(uuid_field(d, "version_id")?), old)
                .await?
                .change
        }
        TagAttached => {
            s.tags
                .detach(ctx, node()?, TagId(uuid_field(d, "tag_id")?))
                .await?
                .change
        }
        TagDetached => {
            s.tags
                .attach(ctx, node()?, TagId(uuid_field(d, "tag_id")?))
                .await?
                .change
        }
        GrantAdded => {
            s.sharing
                .remove_grant(ctx, node()?, GrantId(uuid_field(d, "grant_id")?))
                .await?
                .change
        }
        GrantUpdated => {
            let old: Role = json_field(d, "old_role")?;
            s.sharing
                .update_grant(ctx, node()?, GrantId(uuid_field(d, "grant_id")?), old)
                .await?
                .change
        }
        GrantRemoved => {
            let grantee: Grantee = json_field(d, "grantee")?;
            let role: Role = json_field(d, "role")?;
            s.sharing
                .add_grant(ctx, node()?, grantee, role)
                .await?
                .change
        }
        ShareLinkCreated => {
            s.sharing
                .revoke_share_link(ctx, ShareLinkId(uuid_field(d, "share_link_id")?))
                .await?
                .change
        }
        // The sharing area has no store method for `updateNodeSharing` in the `SharingStore`
        // trait yet. When it adds one (for example `update_node_sharing(ctx, node, Option<bool>)`),
        // this arm calls it with `old_editors_can_share`. Until then the entry is skipped.
        NodeSharingUpdated => return Ok(None),
    };
    Ok(Some(change))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::Actor;
    use crate::ids::{TenantId, UserId};
    use crate::paging::{Cursor, Page};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A journal of `n` entries one minute apart, with seq gaps; counts the entries it returns.
    struct FakeJournal {
        entries: Vec<JournalEntry>,
        returned: AtomicUsize,
    }

    impl FakeJournal {
        fn new(n: i64, start: OffsetDateTime) -> Self {
            let t = TenantId::new();
            let actor = Actor::user(UserId::new());
            let entries = (1..=n)
                .map(|i| JournalEntry {
                    tenant_id: t,
                    seq: i * 2,
                    node_id: None,
                    revision: None,
                    event: EventType::NodeCreated,
                    actor,
                    at: start + time::Duration::minutes(i),
                    data: serde_json::json!({}),
                })
                .collect();
            Self {
                entries,
                returned: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl Journal for FakeJournal {
        async fn read(
            &self,
            _ctx: &Context,
            after: Option<i64>,
            limit: u32,
        ) -> Result<Page<JournalEntry>> {
            let items: Vec<JournalEntry> = self
                .entries
                .iter()
                .filter(|e| after.is_none_or(|a| e.seq > a))
                .take(limit as usize)
                .cloned()
                .collect();
            self.returned.fetch_add(items.len(), Ordering::Relaxed);
            let more = items
                .last()
                .is_some_and(|l| self.entries.last().is_some_and(|last| last.seq > l.seq));
            Ok(Page {
                next_cursor: more
                    .then(|| items.last().map(|l| Cursor(l.seq.to_string())))
                    .flatten(),
                items,
            })
        }

        async fn latest_seq(&self, _ctx: &Context) -> Result<i64> {
            Ok(self.entries.last().map_or(0, |e| e.seq))
        }

        async fn node_entries(
            &self,
            _ctx: &Context,
            _node: crate::ids::NodeId,
            _before: i64,
            _events: &[EventType],
            _limit: u32,
        ) -> Result<Vec<JournalEntry>> {
            Ok(Vec::new())
        }
    }

    fn ctx() -> Context {
        Context {
            tenant: TenantId::new(),
            actor: Actor::user(UserId::new()),
            scope: None,
            idempotency_key: None,
        }
    }

    #[tokio::test]
    async fn a_time_window_reads_only_from_its_start() {
        let start = crate::now() - time::Duration::days(30);
        let j = FakeJournal::new(10_000, start);
        let c = ctx();
        // `since` is minute 9000: the read starts within the clock margin before it.
        let since = start + time::Duration::minutes(9000);
        let after = after_for_time(&j, &c, since).await.unwrap().unwrap();
        let first = j.entries.iter().find(|e| e.seq > after).unwrap();
        assert!(first.at < since);
        assert!(first.at >= since - time::Duration::seconds(CLOCK_MARGIN_SECONDS));
        j.returned.store(0, Ordering::Relaxed);
        let entries = read_all(&j, &c, Some(after)).await.unwrap();
        assert!(entries.iter().any(|e| e.at == since));
        assert!(j.returned.load(Ordering::Relaxed) < 1100);
        // Before the first entry: from the start. After the last: nothing to read.
        assert_eq!(after_for_time(&j, &c, start).await.unwrap(), None);
        let late = after_for_time(&j, &c, start + time::Duration::days(60))
            .await
            .unwrap();
        assert_eq!(late, Some(20_000));
    }

    // ------------------------------------------------------------ inverses of area 1

    use crate::nodes::{CopyRequest, NewNode, NodeKind, OnConflict as Oc};

    /// A user with a personal tenant in fresh in-memory services, and the user's context.
    async fn personal() -> (Services, Context, NodeId) {
        let s = Services::in_memory();
        let login = crate::identity::VerifiedLogin {
            issuer: "https://idp.ruum.test".into(),
            subject: format!("undo-{}", Uuid::now_v7()),
            email: Some(format!("undo-{}@example.test", Uuid::now_v7())),
            email_verified: true,
            name: Some("Undo".into()),
            id_token: None,
        };
        let user = s.identity.upsert_from_login(&login).await.unwrap();
        let (tenant, _) = s.tenants.list_for_user(user.id).await.unwrap().remove(0);
        let ctx = Context {
            tenant: tenant.id,
            actor: Actor::user(user.id),
            scope: None,
            idempotency_key: None,
        };
        (s, ctx, tenant.root_node_id)
    }

    fn new_node(parent: NodeId, name: &str, kind: NodeKind) -> NewNode {
        NewNode {
            parent_id: parent,
            name: name.into(),
            kind,
            node_type: None,
            link: None,
            on_conflict: Oc::Fail,
        }
    }

    /// The newest journal entry of the tenant.
    async fn last(s: &Services, ctx: &Context) -> JournalEntry {
        let latest = s.journal.latest_seq(ctx).await.unwrap();
        s.journal
            .read(ctx, Some(latest - 1), 1)
            .await
            .unwrap()
            .items
            .remove(0)
    }

    /// Inverts the newest entry, which must be of type `event`.
    async fn undo_last(s: &Services, ctx: &Context, event: EventType) -> Option<ChangeInfo> {
        let e = last(s, ctx).await;
        assert_eq!(e.event, event);
        invert(s, ctx, &e).await.unwrap()
    }

    #[tokio::test]
    async fn node_events_are_inverted() {
        let (s, ctx, root) = personal().await;
        let n = &s.nodes;

        // node.created -> trash.
        let a = n
            .create(&ctx, new_node(root, "A", NodeKind::Folder))
            .await
            .unwrap()
            .value;
        assert!(undo_last(&s, &ctx, EventType::NodeCreated).await.is_some());
        assert!(n.get(&ctx, a.id).await.unwrap().trashed_at.is_some());

        // node.trashed -> restore.
        assert!(undo_last(&s, &ctx, EventType::NodeTrashed).await.is_some());
        assert!(n.get(&ctx, a.id).await.unwrap().trashed_at.is_none());

        // node.restored -> trash; then restore it again for the next steps.
        assert!(undo_last(&s, &ctx, EventType::NodeRestored).await.is_some());
        n.restore(&ctx, a.id, Default::default()).await.unwrap();

        // node.renamed -> the old name.
        let f = n
            .create(&ctx, new_node(a.id, "f.txt", NodeKind::File))
            .await
            .unwrap()
            .value;
        n.rename(&ctx, f.id, "g.txt".into(), Oc::Fail, None)
            .await
            .unwrap();
        assert!(undo_last(&s, &ctx, EventType::NodeRenamed).await.is_some());
        assert_eq!(n.get(&ctx, f.id).await.unwrap().name, "f.txt");

        // node.moved -> the old parent and name.
        let b = n
            .create(&ctx, new_node(root, "B", NodeKind::Folder))
            .await
            .unwrap()
            .value;
        n.move_node(
            &ctx,
            f.id,
            MoveRequest {
                parent_id: b.id,
                name: Some("h.txt".into()),
                on_conflict: Oc::Fail,
                if_revision: None,
            },
        )
        .await
        .unwrap();
        assert!(undo_last(&s, &ctx, EventType::NodeMoved).await.is_some());
        let back = n.get(&ctx, f.id).await.unwrap();
        assert_eq!((back.parent_id, back.name.as_str()), (Some(a.id), "f.txt"));

        // node.copied -> the copy goes to the trash (with its subtree).
        let copy = n
            .copy(
                &ctx,
                a.id,
                CopyRequest {
                    parent_id: b.id,
                    name: None,
                    on_conflict: Oc::Fail,
                },
            )
            .await
            .unwrap()
            .value;
        assert!(undo_last(&s, &ctx, EventType::NodeCopied).await.is_some());
        assert!(n.get(&ctx, copy.id).await.unwrap().trashed_at.is_some());
        // The node.created entries of the copied children: trashing a node that is already in
        // the trash is a no-op, so undoing them all (newest first) needs no special case.
        let child = s
            .journal
            .read(&ctx, None, 1000)
            .await
            .unwrap()
            .items
            .into_iter()
            .rev()
            .find(|e| {
                e.event == EventType::NodeCreated
                    && e.data.get("copied_from").is_some_and(|v| !v.is_null())
            })
            .unwrap();
        let c = invert(&s, &ctx, &child).await.unwrap().unwrap();
        assert!(c.seq.is_none());

        // node.purged has no inverse.
        n.purge(&ctx, copy.id).await.unwrap();
        assert!(undo_last(&s, &ctx, EventType::NodePurged).await.is_none());
    }

    #[tokio::test]
    async fn tag_events_are_inverted() {
        let (s, ctx, root) = personal().await;
        let f = s
            .nodes
            .create(&ctx, new_node(root, "f.txt", NodeKind::File))
            .await
            .unwrap()
            .value;
        let tag = s
            .tags
            .create(&ctx, "Garden".into(), None)
            .await
            .unwrap()
            .value;

        // tag.attached -> detach.
        s.tags.attach(&ctx, f.id, tag.id).await.unwrap();
        assert!(undo_last(&s, &ctx, EventType::TagAttached).await.is_some());
        assert!(s.tags.tags_of(&ctx, f.id).await.unwrap().is_empty());

        // tag.detached -> attach.
        assert!(undo_last(&s, &ctx, EventType::TagDetached).await.is_some());
        let ids: Vec<TagId> = s
            .tags
            .tags_of(&ctx, f.id)
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, vec![tag.id]);
    }

    #[tokio::test]
    async fn an_inverse_on_a_changed_node_is_refused() {
        let (s, ctx, root) = personal().await;
        let n = &s.nodes;
        let f = n
            .create(&ctx, new_node(root, "f.txt", NodeKind::File))
            .await
            .unwrap()
            .value;
        n.rename(&ctx, f.id, "g.txt".into(), Oc::Fail, None)
            .await
            .unwrap();
        let renamed = last(&s, &ctx).await;
        // The old name is taken meanwhile: the inverse fails with a conflict
        // (`changed_later` in the undo report).
        n.create(&ctx, new_node(root, "F.TXT", NodeKind::File))
            .await
            .unwrap();
        let err = invert(&s, &ctx, &renamed).await.unwrap_err();
        assert_eq!(err.to_api().code, ErrorCode::NameTaken);
    }
}
