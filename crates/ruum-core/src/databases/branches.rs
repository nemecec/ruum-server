//! Branches of a database (cards D-3a and D-3b, card D-3; design 8.4, decisions 18 and
//! 0004-7, 0004-17, 0004-18, 0004-25, 0004-27, 0004-28): `listDatabaseBranches` and
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
//! - Data (card D-3b): `masked` (the default), `schema_only` (no rows) or `full` (real data:
//!   only a person with `manage` on the source, decision 0004-7; an agent gets `403
//!   user_required`, a person without `manage` gets `403`, before anything is made).
//!
//! [`make_branch`] (the provider's task): the engine copy of the source (Postgres drains the
//! source first, decision 0004-27) with the data asked for. A `masked` copy masks what the
//! lens of the source masks ([`super::provider::mask_plan`]: each column of the database's own
//! schema by its class, `personal` when it has none): `personal` becomes the pseudonym of the
//! value under the source's key, as `text`; `secret` becomes `NULL` with its `NOT NULL`
//! dropped; checks that read a masked column are dropped; keys, `UNIQUE`, indexes and foreign
//! keys are built again, and the engine names the ones a collision broke (decision 0004-17).
//! Those names go into the branch's `status.message` and their number into the journal entry,
//! never a value. A `masked` or `schema_only` branch has a pseudonym key of its own
//! (`key_node_id` = the branch's node ID, decision 0004-25); a `full` branch keeps the source's
//! `key_node_id`. Then the lens from the branch's manifest, and in one transaction
//! ([`super::DatabaseStore::insert_branch`]) the `databases` row (`branch_of`, `branch_data`,
//! `branch_at`), copies of the source's applied `database_migrations` rows, copies of the
//! source's `database_columns` rows whose identity comes from the branch's catalog after the
//! copy and the masking, matched by schema, table and column name (decision 0004-28), so a
//! catalog sync of the branch changes nothing and no confirmed column is unconfirmed again,
//! and the journal entry `database.branch_created` on the source (by the principal pair that
//! asked for the branch).
//!
//! An agent writes on a `masked` or `schema_only` branch without a grant: the core reads
//! `branch_data` (`DatabasePermissions::with_grants`, the query API), the "draft branch" rule of
//! section 8.5. A `full` branch is no draft branch: an agent needs `write_live` there.

use super::classes::{ColumnRow, LiveColumn, live_columns, plan, user_schema};
use super::provider::ProviderDeps;
use super::{
    Database, DatabaseRow, DatabaseService, NODE_TYPE, PROVIDER_ID, engine_not_available, in_trash,
    refuse_share_link,
};
use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::authz::{self, Role};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, PrincipalId, TenantId, UserId, VersionId};
use crate::journal::Written;
use crate::nodes::{ChildOrder, NewTyped, NodeKind, OnConflict};
use crate::paging::{Page, PageRequest};
use crate::providers::{
    ActorPair, CreateTypedNodeRequest, DataCopy, ProviderPrincipalKind, ProviderState, Source,
    invalid, node_not_found,
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

/// The most constraint names that a branch's `status.message` lists (the journal entry has the
/// full count).
const MAX_NAMES_IN_MESSAGE: usize = 20;

/// The `status.message` of a ready masked branch or masked copy whose masking left out
/// constraints (decision 0004-17): their names only, at most [`MAX_NAMES_IN_MESSAGE`]; `None`
/// when none was left out.
pub fn left_out_message(left_out: &[String]) -> Option<String> {
    if left_out.is_empty() {
        return None;
    }
    let shown: Vec<&str> = left_out
        .iter()
        .take(MAX_NAMES_IN_MESSAGE)
        .map(String::as_str)
        .collect();
    let more = left_out.len() - shown.len();
    let rest = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    Some(format!(
        "The masked database is ready. The masking left out {} key(s), unique index(es) or foreign \
         key(s) that masked values break: {}{rest}.",
        left_out.len(),
        shown.join(", ")
    ))
}

/// The platform principal pair of a provider request.
fn actor_of(by: ActorPair) -> Actor {
    let kind = match by.actor.kind {
        ProviderPrincipalKind::User => PrincipalKind::User,
        ProviderPrincipalKind::Group => PrincipalKind::Group,
        ProviderPrincipalKind::App | ProviderPrincipalKind::Provider => PrincipalKind::App,
        ProviderPrincipalKind::Agent => PrincipalKind::Agent,
        ProviderPrincipalKind::ShareLink => PrincipalKind::ShareLink,
    };
    Actor {
        actor: PrincipalRef {
            kind,
            id: PrincipalId(by.actor.id),
        },
        on_behalf_of: by.on_behalf_of.map(UserId),
    }
}

/// What [`make_branch`] made.
pub(super) struct MadeBranch {
    /// The key of the branch's lens.
    pub key: PseudonymKey,
    /// The constraints that the masking left out (`table.name`), for the status.
    pub left_out: Vec<String>,
}

/// Makes the branch `target` of `source` in the engine and writes its rows (the provider's
/// task, cards D-3a and D-3b). `version` is the branch's first manifest version; `by` asked for
/// the branch. Leaves no engine database without its row, and no row without its engine
/// database.
pub(super) async fn make_branch(
    deps: &ProviderDeps,
    tenant: TenantId,
    source: NodeId,
    target: NodeId,
    version: VersionId,
    data: DataCopy,
    by: ActorPair,
) -> Result<MadeBranch> {
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
    // Decision 0004-25: a masked or schema_only branch has a key of its own (derived from the
    // tenant key and its node ID), loaded through Q-4 and installed by the engine (decision
    // 0004-30); a full branch keeps its source's key. The source's key computes the pseudonyms
    // of a masked copy, so the branch's base tables hold what a masked read of the source shows.
    let source_key = deps.keys.engine_key(tenant, src.key_node_id).await?;
    let key_node_id = match data {
        DataCopy::Masked | DataCopy::SchemaOnly => Some(target),
        DataCopy::Full => src.key_node_id,
    };
    let key = deps.keys.engine_key(tenant, key_node_id).await?;
    let mut masked_from = None;
    let copy = match data {
        DataCopy::SchemaOnly => CopyData::SchemaOnly,
        DataCopy::Full => CopyData::Full,
        DataCopy::Masked => {
            // The classes of the source as its lens sees them now. The engine refuses the copy
            // when the source's columns change after this catalog was read (`planned_from`);
            // a change of the classes is checked after the copy (below).
            let catalog = engine
                .catalog(&src.database_ref())
                .await
                .map_err(engine_error)?;
            let plan = super::provider::current_mask_plan(
                deps,
                tenant,
                source,
                src.engine,
                engine.kind(),
                &catalog,
            )
            .await?;
            masked_from = Some((catalog, plan.columns.clone()));
            CopyData::Masked(plan)
        }
    };
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
    let rest = async {
        // A class of the source that became stricter between the plan and the engine's copy
        // would leave its values in clear (Kimi review of D-3b, finding 2): the classes are
        // read again now, after the copy, and the branch fails when the plan would differ.
        if let Some((catalog, columns)) = &masked_from {
            super::provider::require_same_mask_plan(
                deps,
                tenant,
                source,
                src.engine,
                engine.kind(),
                catalog,
                columns,
            )
            .await?;
        }
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
        let live = live_columns(&catalog, user_schema(src.engine));
        let columns = branch_columns(
            &deps.columns.list(tenant, source).await?,
            &live,
            version,
            crate::now(),
        );
        // One transaction: the `databases` row, the roles, the copied migration rows and the
        // column rows (review D-3a 1). A catalog sync never sees the row without its columns,
        // and a restart never leaves a row (`ready`) without them.
        deps.store
            .insert_branch(
                &row,
                &created.roles,
                &columns,
                &super::BranchCreated {
                    by: actor_of(by),
                    constraints_left_out: created.left_out.len(),
                },
            )
            .await
    }
    .await;
    if let Err(e) = rest {
        let _ = engine.drop_database(&created.database).await;
        return Err(e);
    }
    Ok(MadeBranch {
        key,
        left_out: created.left_out,
    })
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
        if req.data == DataCopy::Full {
            // Real data: only a person with manage on the source (decision 0004-7).
            if ctx.actor.is_agent() {
                return Err(Error::user_required());
            }
            authz::require(&*d.authz, &*d.nodes, ctx, id, Role::Manage).await?;
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
                data: req.data,
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
