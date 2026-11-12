//! Branches of a database (card D-3a, the `schema_only` part of card D-3; design 8.4,
//! decisions 18 and 0004-17, 0004-18, 0004-25, 0004-27, 0004-28): `listDatabaseBranches` and
//! `createDatabaseBranch`.
//!
//! - `list_branches`: `view` on the database; its live child nodes of type `database` (a
//!   branch is a child node of its source, `branch_of` = the source), one page at a time, as
//!   `Database` answers.
//! - `create_branch`: `edit` on the source; a guest, or an agent of a guest, only while the
//!   tenant policy `guests_create_resources` is true (decision 0004-10). The source must be a
//!   live database that is `ready` (else `503 database_not_ready`, hint `retry`), not a branch
//!   (`409 conflict`), not larger than the branch limit (`422 database_too_large`), and the plan
//!   must have room for another branch (`plan_limit_reached`). Then the branch node (a child of
//!   the source, so it inherits the grants) with the source's manifest and `template` naming
//!   the source, and the provider makes the branch in a task ([`make_branch`]). The answer is
//!   `202` with status `provisioning`.
//! - Data: `schema_only` only (no rows). `masked` (the default) and `full` answer `422
//!   validation_failed` (hint `/data`) until card D-3b; an agent that asks for `full` gets `403
//!   user_required`, and a person without `manage` gets `403`, first.
//!
//! [`make_branch`] (the provider's task): the engine copy of the source with `schema_only`
//! data (Postgres drains the source first, decision 0004-27), the branch's own pseudonym key
//! (`key_node_id` = the branch's node ID, decision 0004-25), the lens from the branch's
//! manifest, the `databases` row (`branch_of`, `branch_data`, `branch_at`; the Postgres store
//! copies the source's applied `database_migrations` rows in the same transaction), and copies
//! of the source's `database_columns` rows whose identity comes from the branch's catalog,
//! matched by schema, table and column name (decision 0004-28), so a catalog sync of the branch
//! changes nothing and no confirmed column is unconfirmed again.
//!
//! An agent writes on a `schema_only` branch without a grant: the core reads `branch_data`
//! (`DatabasePermissions::with_grants`, the query API), the "draft branch" rule of section 8.5.

use super::classes::{ColumnRow, LiveColumn, live_columns, plan, user_schema};
use super::provider::ProviderDeps;
use super::{
    Database, DatabaseRow, DatabaseService, NODE_TYPE, PROVIDER_ID, engine_not_available, in_trash,
    refuse_share_link,
};
use crate::actor::Context;
use crate::authz::{self, Role};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId, VersionId};
use crate::journal::Written;
use crate::nodes::{ChildOrder, NewTyped, NodeKind, OnConflict};
use crate::paging::{Page, PageRequest};
use crate::providers::{
    CreateTypedNodeRequest, DataCopy, ProviderState, Source, invalid, node_not_found,
};
use crate::tenants::database_limits;
use ruum_dbengine::{CopyData, EngineError, NewDatabase, PseudonymKey};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;

/// `CreateDatabaseBranchRequest`.
#[derive(Clone, Debug, Deserialize)]
pub struct CreateBranch {
    pub name: String,
    #[serde(default)]
    pub data: DataCopy,
    #[serde(default)]
    pub on_conflict: OnConflict,
}

/// `422 validation_failed`: `masked` and `full` branches come with card D-3b.
pub fn data_not_available() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::ValidationFailed,
            "only schema_only branches are available yet; masked and full branches come later",
        )
        .with_hint("/data: schema_only"),
    )
}

/// `422`: a create of a branch node without its source (the reconciler sends a lost create
/// again with `Source::Empty`). The branch ends `failed`; it never becomes an empty database.
pub fn branch_source_lost() -> Error {
    invalid(
        "the create of a branch was lost; make the branch again",
        "branch_source_lost",
    )
}

/// `409 conflict`: branches are flat children of a live database in step 2.
fn branch_of_branch() -> Error {
    Error::Api(
        ApiError::new(ErrorCode::Conflict, "a branch of a branch is not possible").with_hint(
            "make the branch of the live database (branch_of of this database), or copy this \
             branch first",
        ),
    )
}

/// `422 database_too_large` (decision 18: branches are for small databases only).
fn too_large(size: i64, max: i64) -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::DatabaseTooLarge,
            format!("the database has {size} bytes; a branch can be made of at most {max} bytes"),
        )
        .with_hint("make the database smaller, or copy it instead"),
    )
}

/// An engine error as a platform error: the kind only, never the engine's text in a status.
fn engine_error(e: EngineError) -> Error {
    match e {
        EngineError::NotReady => Error::Api(super::query::not_ready_error()),
        other => Error::Internal(format!("database engine: {other}")),
    }
}

/// The manifest of a new branch: the source's newest manifest (its confirmed `data_classes`
/// included), with `template` naming the source and that version (read-only for clients).
pub fn branch_manifest(source_manifest: &Value, source: NodeId, version: VersionId) -> Value {
    let mut m = source_manifest.clone();
    if let Some(o) = m.as_object_mut() {
        o.insert(
            "template".into(),
            json!({ "node_id": source, "version_id": version }),
        );
    }
    m
}

/// The `database_columns` rows of a new branch: the source's live rows (classes, suggestions
/// and confirmations kept), each with the identity and the type of the branch's column of the
/// same schema, table and column name (decision 0004-28), and confirmed as of the branch's
/// first manifest version; then [`plan`] adds a row for a branch column that the source has no
/// row for, so a catalog sync of the branch right after changes nothing.
pub fn branch_columns(
    source: &[ColumnRow],
    live: &[LiveColumn],
    version: VersionId,
    now: OffsetDateTime,
) -> Vec<ColumnRow> {
    let copied: Vec<ColumnRow> = live
        .iter()
        .filter_map(|l| {
            let r = source
                .iter()
                .find(|r| r.is_live() && r.column == l.column)?;
            let mut r = r.clone();
            r.identity = l.identity;
            r.data_type = l.data_type.clone();
            if let Some(c) = &mut r.confirmed {
                c.version = version;
            }
            Some(r)
        })
        .collect();
    plan(&copied, live, now).rows
}

/// Makes the branch `target` of `source` in the engine and writes its rows (the provider's
/// task, card D-3a). `version` is the branch's first manifest version. Returns the branch's
/// pseudonym key. Leaves no engine database without its row, and no row without its engine
/// database.
pub(super) async fn make_branch(
    deps: &ProviderDeps,
    tenant: TenantId,
    source: NodeId,
    target: NodeId,
    version: VersionId,
    data: DataCopy,
) -> Result<PseudonymKey> {
    let copy = match data {
        DataCopy::SchemaOnly => CopyData::SchemaOnly,
        DataCopy::Masked | DataCopy::Full => return Err(data_not_available()),
    };
    let src = deps
        .store
        .get(tenant, source)
        .await?
        .ok_or_else(|| node_not_found(source))?;
    if src.branch_of.is_some() {
        return Err(branch_of_branch());
    }
    let engine = deps
        .engines
        .get(src.engine)
        .cloned()
        .ok_or_else(|| engine_not_available(src.engine))?;
    // Decision 0004-25: a schema_only branch has a key of its own (derived from the tenant key
    // and its node ID), loaded through Q-4 and installed by the engine (decision 0004-30). The
    // source's key would compute the pseudonyms of a masked copy (card D-3b).
    let source_key = deps.keys.engine_key(tenant, src.key_node_id).await?;
    let key_node_id = Some(target);
    let key = deps.keys.engine_key(tenant, key_node_id).await?;
    let created = engine
        .copy_database(
            &src.database_ref(),
            &source_key,
            &NewDatabase {
                tenant_id: tenant.0,
                node_id: target.0,
                engine_version: src.engine_version.clone(),
                extensions: src.extensions.clone(),
            },
            &copy,
            &key,
        )
        .await
        .map_err(engine_error)?;
    let mut inserted = false;
    let rest = async {
        // The lens of the branch, from its manifest (the source's confirmed classes).
        let manifest = deps
            .store
            .manifest(tenant, version)
            .await?
            .ok_or_else(|| Error::Internal("the manifest of a branch is missing".into()))?;
        let sync = super::query::interim_catalog_sync(src.engine, &manifest);
        let catalog = engine
            .sync_lens(&created.database, &key, &sync)
            .await
            .map_err(engine_error)?;
        let size_bytes = engine
            .size_bytes(&created.database)
            .await
            .map(|n| i64::try_from(n).unwrap_or(i64::MAX))
            .unwrap_or(0);
        let row = DatabaseRow {
            tenant_id: tenant,
            node_id: target,
            engine: src.engine,
            engine_version: src.engine_version.clone(),
            cluster: created.database.cluster.clone(),
            engine_name: created.database.engine_name.clone(),
            branch_of: Some(source),
            branch_data: Some(data),
            key_node_id,
            extensions: src.extensions.clone(),
            size_bytes,
        };
        deps.store.insert(&row, &created.roles).await?;
        inserted = true;
        let live = live_columns(&catalog, user_schema(src.engine));
        let rows = branch_columns(
            &deps.columns.list(tenant, source).await?,
            &live,
            version,
            crate::now(),
        );
        deps.columns.seed(tenant, target, &rows).await
    }
    .await;
    if let Err(e) = rest {
        if inserted && let Err(d) = deps.store.delete(tenant, target).await {
            tracing::warn!(
                %tenant,
                node = %target,
                error = %crate::providers::store::error_text(&d),
                "the row of a failed branch was not deleted"
            );
        }
        let _ = engine.drop_database(&created.database).await;
        return Err(e);
    }
    Ok(key)
}

impl DatabaseService {
    /// `listDatabaseBranches`: the live branches of a database the caller sees (`view`), as
    /// `Database` answers, in name order.
    pub async fn list_branches(
        &self,
        ctx: &Context,
        id: NodeId,
        page: PageRequest,
    ) -> Result<Page<Database>> {
        refuse_share_link(ctx)?;
        self.database_node(ctx, id).await?;
        let children = self
            .deps
            .nodes
            .list_children(ctx, id, ChildOrder::Name, page)
            .await?;
        let mut items = Vec::with_capacity(children.items.len());
        for child in children.items {
            if child.kind != NodeKind::Typed || child.node_type.as_deref() != Some(NODE_TYPE) {
                continue;
            }
            items.push(self.view(ctx, child).await?);
        }
        Ok(Page {
            items,
            next_cursor: children.next_cursor,
        })
    }

    /// `createDatabaseBranch`. See the module docs for the order of the checks.
    pub async fn create_branch(
        &self,
        ctx: &Context,
        id: NodeId,
        req: CreateBranch,
    ) -> Result<Written<Database>> {
        refuse_share_link(ctx)?;
        let d = &self.deps;
        let source = self.database_node(ctx, id).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, id, Role::Edit).await?;
        if source.trashed_at.is_some() {
            return Err(in_trash());
        }
        self.check_guest_policy(ctx).await?;
        match req.data {
            DataCopy::SchemaOnly => {}
            DataCopy::Full => {
                // Real data: only a person with manage on the source (decision 0004-7).
                if ctx.actor.is_agent() {
                    return Err(Error::user_required());
                }
                authz::require(&*d.authz, &*d.nodes, ctx, id, Role::Manage).await?;
                return Err(data_not_available());
            }
            DataCopy::Masked => return Err(data_not_available()),
        }
        let typed = self.typed_row(ctx.tenant, id).await?;
        let row = d.store.get(ctx.tenant, id).await?;
        let ready = super::DatabaseStatus::of(&typed, false).state == ProviderState::Ready;
        let Some(row) = row.filter(|_| ready) else {
            return Err(Error::Api(super::query::not_ready_error()));
        };
        if row.branch_of.is_some() {
            return Err(branch_of_branch());
        }
        let engine = d
            .engines
            .get(row.engine)
            .cloned()
            .ok_or_else(|| engine_not_available(row.engine))?;
        let size = engine
            .size_bytes(&row.database_ref())
            .await
            .map_err(engine_error)?;
        let size = i64::try_from(size).unwrap_or(i64::MAX);
        let max = d.settings.max_branch_source_bytes;
        if size > max {
            return Err(too_large(size, max));
        }
        self.check_branch_plan(ctx.tenant).await?;
        let source_manifest = d
            .store
            .manifest(ctx.tenant, typed.desired_version)
            .await?
            .ok_or_else(|| Error::Internal(format!("database {id} has no manifest")))?;
        let manifest = branch_manifest(&source_manifest, id, typed.desired_version);
        let written = d
            .nodes
            .create_typed_child(
                ctx,
                NewTyped {
                    parent_id: id,
                    name: req.name,
                    node_type: NODE_TYPE.into(),
                    provider: PROVIDER_ID.into(),
                    manifest: manifest.clone(),
                    on_conflict: req.on_conflict,
                },
            )
            .await?;
        let node = written.value.id;
        let version = written
            .value
            .head_version
            .ok_or_else(|| Error::Internal("a new typed node has no version".into()))?;
        d.store.created(&written.value, version, &manifest);
        // Start the copy. A failure here leaves the row `provisioning`; after a restart the
        // reconciler's create of the node is refused (`branch_source_lost`), so it ends
        // `failed`.
        let create = CreateTypedNodeRequest {
            tenant_id: ctx.tenant,
            node_id: node,
            parent_id: Some(id),
            type_name: NODE_TYPE.into(),
            version_id: version,
            manifest,
            source: Source::Branch {
                node_id: id,
                data: DataCopy::SchemaOnly,
            },
            by: ctx.actor.into(),
        };
        let delivery = crate::providers::reconciler::create_delivery_id(ctx.tenant, node, version);
        use crate::providers::Provider;
        match d.provider.create_typed_node(delivery, create).await {
            Ok(reply) => self.store_status(ctx.tenant, reply.into_inner()).await,
            Err(e) => tracing::warn!(
                tenant = %ctx.tenant,
                node = %node,
                error = %crate::providers::store::error_text(&e),
                "createTypedNode of a branch failed"
            ),
        }
        let db = self.view(ctx, written.value).await?;
        Ok(Written {
            value: db,
            change: written.change,
        })
    }

    /// `plan_limit_reached` when the plan's branch count is reached (C-21).
    async fn check_branch_plan(&self, tenant: TenantId) -> Result<()> {
        let d = &self.deps;
        let t = d.tenants.find(tenant).await?;
        let members = d.tenants.member_count(tenant).await?;
        let limits = database_limits(t.plan, members);
        let usage = d.store.usage(tenant).await?;
        match limits.branches {
            Some(max) if usage.branches >= max => Err(Error::Api(
                ApiError::new(
                    ErrorCode::PlanLimitReached,
                    format!(
                        "the plan allows {max} database branch(es); the space has {}",
                        usage.branches
                    ),
                )
                .with_hint("delete a branch or move to a larger plan"),
            )),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
