//! The rules of typed nodes in the tree (card P-3; decisions 0004-6, 0004-7 and 0004-18;
//! `contracts/providers/README.md`, "Children of a typed node" and "Lifecycle hooks"). Both
//! node stores use these functions, so they follow one rule.

use super::rules;
use super::{Node, NodeKind, NodeStore, TypedHooks};
use crate::actor::Context;
use crate::authz::{Authz, Role};
use crate::error::{Error, Result};
use crate::ids::NodeId;
use crate::providers::store::NewDelivery;
use crate::providers::{DataCopy, DeliveryId, HookDelivery, HookKind};
use serde_json::Value;
use std::collections::HashSet;
use time::OffsetDateTime;

/// `409 conflict`: a typed node is not a folder; only its type's own operations make its
/// children (`TypeDescriptor.child_types`, decision 0004-18).
pub fn not_a_folder() -> Error {
    rules::conflict(
        "the node is a typed node, not a folder; nothing goes inside it",
        "choose a folder; the children of a typed node come only from its own operations \
         (for a database: createDatabaseBranch)",
    )
}

/// `409 conflict`: a child of a typed node (a database branch) stays with its parent.
pub fn child_stays() -> Error {
    rules::conflict(
        "the node belongs to the typed node above it and cannot move away from it",
        "move or copy the typed node above it instead",
    )
}

/// `409 conflict`: a typed node whose type has no provider that takes copies.
pub fn copy_not_available() -> Error {
    rules::conflict(
        "this typed node cannot be copied: its provider does not take copies here",
        "ask the operator; the node itself is unchanged",
    )
}

/// The `409` of a new parent that is a typed node, else the step 1 rule (`422` for a file or a
/// link).
pub fn check_parent(parent: &Node, message: &str) -> Result<()> {
    match parent.kind {
        NodeKind::Folder => Ok(()),
        NodeKind::Typed => Err(not_a_folder()),
        _ => Err(rules::invalid(message)),
    }
}

/// True when `node` is a typed node.
pub fn is_typed(node: &Node) -> bool {
    node.kind == NodeKind::Typed
}

/// The nodes of a copy: `all` is the live subtree of the source, parents before children. A
/// copy does not descend into a typed node: its children (database branches) stay behind
/// (decision 0004-18, "a copy of a database copies no branch").
pub fn copy_sources(all: Vec<Node>) -> Vec<Node> {
    let mut typed: HashSet<NodeId> = HashSet::new();
    let mut skipped: HashSet<NodeId> = HashSet::new();
    let mut out = Vec::with_capacity(all.len());
    for (i, n) in all.into_iter().enumerate() {
        let below_typed = i > 0
            && n.parent_id
                .is_some_and(|p| typed.contains(&p) || skipped.contains(&p));
        if below_typed {
            skipped.insert(n.id);
            continue;
        }
        if is_typed(&n) {
            typed.insert(n.id);
        }
        out.push(n);
    }
    out
}

/// The data of a copy of a typed node (decision 0004-7): `full` only when the caller is a
/// person acting for themselves with `manage` on the source (after the scope); `masked` for
/// everyone else (agents, apps, people with `view` or `edit`). A viewer who copies into a
/// folder of their own manages the copy, so a full copy would give them what they could not
/// read.
pub async fn copy_data(
    authz: &dyn Authz,
    nodes: &dyn NodeStore,
    ctx: &Context,
    source: NodeId,
) -> Result<DataCopy> {
    if !ctx.actor.is_user() {
        return Ok(DataCopy::Masked);
    }
    let view = rules::caller_view(authz, nodes, ctx, &[source]).await?;
    Ok(if view.first().and_then(|v| v.role) == Some(Role::Manage) {
        DataCopy::Full
    } else {
        DataCopy::Masked
    })
}

/// The name of a copy's data in the `copied` hook (`data.data`).
pub fn data_name(d: DataCopy) -> &'static str {
    match d {
        DataCopy::Masked => "masked",
        DataCopy::SchemaOnly => "schema_only",
        DataCopy::Full => "full",
    }
}

/// The data of a copy that a `copied` hook carries; `masked` when it is missing or unknown, so a
/// broken payload never copies real data.
pub fn data_of(hook: &HookDelivery) -> DataCopy {
    match hook
        .data
        .as_ref()
        .and_then(|d| d.get("data"))
        .and_then(Value::as_str)
    {
        Some("full") => DataCopy::Full,
        Some("schema_only") => DataCopy::SchemaOnly,
        _ => DataCopy::Masked,
    }
}

/// A typed node as the hooks need it: its ID and its type.
pub fn typed_ref(node: &Node) -> Option<(NodeId, String)> {
    if is_typed(node) {
        node.node_type.clone().map(|t| (node.id, t))
    } else {
        None
    }
}

/// One delivery of `hook` for the typed node `node` of type `type_name`, from the journal entry
/// `seq` (its `data`). For `copied`, `target` is the new node and the data of the copy. `None`
/// when the type's provider does not take the hook.
#[allow(clippy::too_many_arguments)]
pub fn delivery(
    hooks: &dyn TypedHooks,
    ctx: &Context,
    hook: HookKind,
    node: NodeId,
    type_name: &str,
    seq: i64,
    at: OffsetDateTime,
    entry: &Value,
    target: Option<(NodeId, DataCopy)>,
) -> Option<NewDelivery> {
    let provider = hooks.provider_for(type_name, hook)?;
    let mut data = entry.as_object().cloned().unwrap_or_default();
    if let Some((_, d)) = target {
        data.insert("data".into(), Value::String(data_name(d).into()));
    }
    Some(NewDelivery {
        delivery_id: DeliveryId::new(),
        provider,
        hook: HookDelivery {
            hook,
            tenant_id: ctx.tenant,
            node_id: Some(node),
            seq: Some(seq),
            target_node_id: target.map(|(t, _)| t),
            data: Some(data),
            by: ctx.actor.into(),
            at,
        },
    })
}

/// The deliveries of `hook` for the typed nodes `typed` (from [`typed_ref`]), from one journal
/// entry. None without hooks or without a journal entry.
pub fn deliveries(
    hooks: Option<&dyn TypedHooks>,
    ctx: &Context,
    hook: HookKind,
    typed: &[(NodeId, String)],
    seq: Option<i64>,
    at: OffsetDateTime,
    entry: &Value,
) -> Vec<NewDelivery> {
    let (Some(hooks), Some(seq)) = (hooks, seq) else {
        return Vec::new();
    };
    typed
        .iter()
        .filter_map(|(n, t)| delivery(hooks, ctx, hook, *n, t, seq, at, entry, None))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::Actor;
    use crate::ids::{TenantId, UserId};

    fn node(id: NodeId, parent: Option<NodeId>, kind: NodeKind) -> Node {
        let actor = Actor::user(UserId::new());
        Node {
            id,
            tenant_id: TenantId::new(),
            parent_id: parent,
            name: "n".into(),
            kind,
            node_type: (kind == NodeKind::Typed).then(|| "database".to_string()),
            link_kind: None,
            link_target: None,
            head_version: None,
            owner_id: UserId::new(),
            size_bytes: 0,
            revision: 1,
            trashed_at: None,
            created_at: crate::now(),
            updated_at: crate::now(),
            created_by: actor,
            updated_by: actor,
            my_role: None,
            my_can_share: None,
            editors_can_share: None,
            editors_can_share_effective: None,
        }
    }

    #[test]
    fn a_copy_stops_at_typed_nodes() {
        let (f, db, branch, deep, file) = (
            NodeId::new(),
            NodeId::new(),
            NodeId::new(),
            NodeId::new(),
            NodeId::new(),
        );
        let all = vec![
            node(f, None, NodeKind::Folder),
            node(db, Some(f), NodeKind::Typed),
            node(file, Some(f), NodeKind::File),
            node(branch, Some(db), NodeKind::Typed),
            node(deep, Some(branch), NodeKind::Typed),
        ];
        let ids: Vec<NodeId> = copy_sources(all).iter().map(|n| n.id).collect();
        assert_eq!(ids, vec![f, db, file]);
        // A copy of the database alone keeps only the database.
        let alone = vec![
            node(db, Some(f), NodeKind::Typed),
            node(branch, Some(db), NodeKind::Typed),
        ];
        let ids: Vec<NodeId> = copy_sources(alone).iter().map(|n| n.id).collect();
        assert_eq!(ids, vec![db]);
    }

    #[test]
    fn a_broken_or_missing_copy_data_is_masked() {
        let mut h = HookDelivery {
            hook: HookKind::Copied,
            tenant_id: TenantId::new(),
            node_id: Some(NodeId::new()),
            seq: Some(1),
            target_node_id: Some(NodeId::new()),
            data: None,
            by: Actor::user(UserId::new()).into(),
            at: crate::now(),
        };
        assert_eq!(data_of(&h), DataCopy::Masked);
        for (v, want) in [
            (Value::String("full".into()), DataCopy::Full),
            (Value::String("schema_only".into()), DataCopy::SchemaOnly),
            (Value::String("masked".into()), DataCopy::Masked),
            (Value::String("FULL".into()), DataCopy::Masked),
            (Value::Bool(true), DataCopy::Masked),
        ] {
            let mut m = serde_json::Map::new();
            m.insert("data".into(), v);
            h.data = Some(m);
            assert_eq!(data_of(&h), want);
        }
    }
}
