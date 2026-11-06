//! The Postgres tag store: tables `tags` and `node_tags`, and the journal in the same
//! transaction. Permission checks on nodes run on the engine before the transaction.

use super::rules::{self, TagCaller};
use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::authz::{self, Authz, Role};
use crate::error::Error;
use crate::ids::{PrincipalId, UserId};
use crate::journal::postgres::{append, begin_tenant};
use crate::journal::{ChangeInfo, EventType, NewEntry};
use crate::nodes::memory::no_change;
use crate::nodes::postgres::{load, node_of};
use crate::nodes::{NodeStore, rules as node_rules};
use crate::tenants::MemberRole;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};
use std::sync::Arc;
use uuid::Uuid;

const TAG_COLS: &str =
    "id, tenant_id, name, color, created_at, created_kind, created_by, created_obo";

fn tag_of(row: &PgRow) -> Result<Tag> {
    Ok(Tag {
        id: TagId(row.try_get("id")?),
        tenant_id: TenantId(row.try_get("tenant_id")?),
        name: row.try_get("name")?,
        color: row.try_get("color")?,
        created_by: Actor {
            actor: PrincipalRef {
                kind: row.try_get::<PrincipalKind, _>("created_kind")?,
                id: PrincipalId(row.try_get::<Uuid, _>("created_by")?),
            },
            on_behalf_of: row.try_get::<Option<Uuid>, _>("created_obo")?.map(UserId),
        },
        created_at: row.try_get("created_at")?,
    })
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("23505"))
}

async fn load_tag(conn: &mut PgConnection, tenant: TenantId, id: TagId) -> Result<Tag> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {TAG_COLS} from tags where tenant_id = $1 and id = $2"
    )))
    .bind(tenant.0)
    .bind(id.0)
    .fetch_optional(conn)
    .await?
    .ok_or_else(rules::tag_not_found)?;
    tag_of(&row)
}

/// `(tag, node)` for the tags on live nodes of the tenant (only `tag` when given).
async fn live_pairs(
    conn: &mut PgConnection,
    tenant: TenantId,
    tag: Option<TagId>,
) -> Result<Vec<(TagId, NodeId)>> {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select nt.tag_id, nt.node_id from node_tags nt \
         join nodes n on n.tenant_id = nt.tenant_id and n.id = nt.node_id \
         where nt.tenant_id = $1 and n.trashed_at is null \
         and ($2::uuid is null or nt.tag_id = $2)",
    )
    .bind(tenant.0)
    .bind(tag.map(|t| t.0))
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(t, n)| (TagId(t), NodeId(n)))
        .collect())
}

/// The Postgres store.
pub struct PgTagStore {
    pool: PgPool,
    authz: Arc<dyn Authz>,
    nodes: Arc<dyn NodeStore>,
}

impl std::fmt::Debug for PgTagStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgTagStore").finish_non_exhaustive()
    }
}

impl PgTagStore {
    pub fn new(pool: PgPool, authz: Arc<dyn Authz>, nodes: Arc<dyn NodeStore>) -> Self {
        Self { pool, authz, nodes }
    }

    async fn caller(&self, ctx: &Context) -> Result<TagCaller> {
        let role = match ctx.actor.effective_user() {
            Some(u) => {
                let mut c = begin_tenant(&self.pool, ctx.tenant).await?;
                let r: Option<MemberRole> = sqlx::query_scalar(
                    "select role from memberships where tenant_id = $1 and user_id = $2",
                )
                .bind(ctx.tenant.0)
                .bind(u.0)
                .fetch_optional(&mut *c)
                .await?;
                c.commit().await?;
                r
            }
            None => None,
        };
        Ok(TagCaller::new(ctx, role))
    }

    async fn visible(&self, ctx: &Context, id: TagId) -> Result<(Tag, TagCaller)> {
        let mut c = begin_tenant(&self.pool, ctx.tenant).await?;
        let tag = load_tag(&mut c, ctx.tenant, id).await?;
        let pairs = live_pairs(&mut c, ctx.tenant, Some(id)).await?;
        c.commit().await?;
        let caller = self.caller(ctx).await?;
        if !caller.sees_all
            && !rules::visible_tags(&*self.authz, &*self.nodes, ctx, &pairs)
                .await?
                .contains(&id)
        {
            return Err(rules::tag_not_found());
        }
        Ok((tag, caller))
    }

    /// Attaches or detaches in one transaction; `None` when nothing changed.
    async fn link(
        &self,
        ctx: &Context,
        node: NodeId,
        tag: TagId,
        attach: bool,
    ) -> Result<Written<()>> {
        authz::require(&*self.authz, &*self.nodes, ctx, node, Role::Edit).await?;
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let n = load(&mut tx, ctx.tenant, node, false).await?.node;
        if n.trashed_at.is_some() {
            return Err(node_rules::node_not_found());
        }
        let tag = load_tag(&mut tx, ctx.tenant, tag).await?;
        let changed = if attach {
            sqlx::query(
                "insert into node_tags (tenant_id, node_id, tag_id, created_kind, created_by, \
                 created_obo) values ($1, $2, $3, $4, $5, $6) on conflict do nothing",
            )
            .bind(ctx.tenant.0)
            .bind(node.0)
            .bind(tag.id.0)
            .bind(ctx.actor.actor.kind)
            .bind(ctx.actor.actor.id.0)
            .bind(ctx.actor.on_behalf_of.map(|u| u.0))
            .execute(&mut *tx)
            .await?
            .rows_affected()
        } else {
            sqlx::query(
                "delete from node_tags where tenant_id = $1 and node_id = $2 and tag_id = $3",
            )
            .bind(ctx.tenant.0)
            .bind(node.0)
            .bind(tag.id.0)
            .execute(&mut *tx)
            .await?
            .rows_affected()
        };
        if changed == 0 {
            tx.commit().await?;
            return Ok(Written {
                value: (),
                change: no_change(ctx),
            });
        }
        let e = append(
            &mut tx,
            ctx,
            NewEntry {
                node_id: Some(node),
                revision: Some(n.revision),
                event: if attach {
                    EventType::TagAttached
                } else {
                    EventType::TagDetached
                },
                data: rules::tag_data(&tag),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(Written {
            value: (),
            change: ChangeInfo {
                seq: Some(e.seq),
                actor: ctx.actor,
                at: e.at,
            },
        })
    }
}

#[async_trait]
impl TagStore for PgTagStore {
    async fn list(&self, ctx: &Context, page: PageRequest) -> Result<Page<Tag>> {
        let after = page
            .cursor
            .as_ref()
            .map(node_rules::parse_key_cursor)
            .transpose()?;
        let caller = self.caller(ctx).await?;
        let limit = page.limit() as usize;
        let mut c = begin_tenant(&self.pool, ctx.tenant).await?;
        let seen: Option<Vec<Uuid>> = if caller.sees_all {
            None
        } else {
            let pairs = live_pairs(&mut c, ctx.tenant, None).await?;
            let seen = rules::visible_tags(&*self.authz, &*self.nodes, ctx, &pairs).await?;
            Some(seen.into_iter().map(|t| t.0).collect())
        };
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {TAG_COLS}, lower(name) as name_key from tags where tenant_id = $1 \
             and ($2::uuid[] is null or id = any($2)) \
             and ($3::text is null or (lower(name) collate \"C\", id) > ($3::text collate \"C\", $4::uuid)) \
             order by lower(name) collate \"C\", id limit $5"
        )))
        .bind(ctx.tenant.0)
        .bind(seen)
        .bind(after.as_ref().map(|(k, _)| k.clone()))
        .bind(after.as_ref().map(|(_, i)| *i))
        .bind((limit + 1) as i64)
        .fetch_all(&mut *c)
        .await?;
        c.commit().await?;
        let mut items = Vec::new();
        let mut last = None;
        for r in rows.iter().take(limit) {
            let t = tag_of(r)?;
            last = Some((r.try_get::<String, _>("name_key")?, t.id.0));
            items.push(t);
        }
        let next_cursor = if rows.len() > limit {
            last.map(|(k, i)| node_rules::key_cursor(&k, i))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
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
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let id = TagId::new();
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into tags (id, tenant_id, name, color, created_kind, created_by, created_obo) \
             values ($1, $2, $3, $4, $5, $6, $7) returning {TAG_COLS}"
        )))
        .bind(id.0)
        .bind(ctx.tenant.0)
        .bind(&name)
        .bind(&color)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                rules::name_taken(&name)
            } else {
                Error::Database(e)
            }
        })?;
        tx.commit().await?;
        Ok(Written {
            value: tag_of(&row)?,
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
        let set_color = input.color.is_some();
        let color = input
            .color
            .map(rules::validate_color)
            .transpose()?
            .flatten();
        let (_, caller) = self.visible(ctx, id).await?;
        caller.check_write(ctx)?;
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update tags set name = coalesce($3, name), \
             color = case when $4 then $5 else color end, updated_at = now() \
             where tenant_id = $1 and id = $2 returning {TAG_COLS}"
        )))
        .bind(ctx.tenant.0)
        .bind(id.0)
        .bind(&name)
        .bind(set_color)
        .bind(&color)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                rules::name_taken(name.as_deref().unwrap_or_default())
            } else {
                Error::Database(e)
            }
        })?
        .ok_or_else(rules::tag_not_found)?;
        tx.commit().await?;
        Ok(Written {
            value: tag_of(&row)?,
            change: no_change(ctx),
        })
    }

    async fn delete(&self, ctx: &Context, id: TagId) -> Result<()> {
        let (tag, caller) = self.visible(ctx, id).await?;
        caller.check_delete(ctx, &tag)?;
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let nodes: Vec<(Uuid, i64, Option<time::OffsetDateTime>)> = sqlx::query_as(
            "delete from node_tags nt using nodes n \
             where nt.tenant_id = $1 and nt.tag_id = $2 \
             and n.tenant_id = nt.tenant_id and n.id = nt.node_id \
             returning nt.node_id, n.revision, n.trashed_at",
        )
        .bind(ctx.tenant.0)
        .bind(id.0)
        .fetch_all(&mut *tx)
        .await?;
        for (node, revision, trashed) in nodes {
            if trashed.is_some() {
                continue;
            }
            append(
                &mut tx,
                ctx,
                NewEntry {
                    node_id: Some(NodeId(node)),
                    revision: Some(revision),
                    event: EventType::TagDetached,
                    data: rules::tag_data(&tag),
                },
            )
            .await?;
        }
        sqlx::query("delete from tags where tenant_id = $1 and id = $2")
            .bind(ctx.tenant.0)
            .bind(id.0)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn attach(&self, ctx: &Context, node: NodeId, tag: TagId) -> Result<Written<()>> {
        self.link(ctx, node, tag, true).await
    }

    async fn detach(&self, ctx: &Context, node: NodeId, tag: TagId) -> Result<Written<()>> {
        self.link(ctx, node, tag, false).await
    }

    async fn tags_of(&self, ctx: &Context, node: NodeId) -> Result<Vec<Tag>> {
        authz::require(&*self.authz, &*self.nodes, ctx, node, Role::View).await?;
        let mut c = begin_tenant(&self.pool, ctx.tenant).await?;
        let n = load(&mut c, ctx.tenant, node, false).await?.node;
        if n.trashed_at.is_some() {
            return Err(node_rules::node_not_found());
        }
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {} from tags t join node_tags nt on nt.tenant_id = t.tenant_id \
             and nt.tag_id = t.id where t.tenant_id = $1 and nt.node_id = $2 \
             order by lower(t.name) collate \"C\", t.id",
            TAG_COLS
                .split(", ")
                .map(|c| format!("t.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        )))
        .bind(ctx.tenant.0)
        .bind(node.0)
        .fetch_all(&mut *c)
        .await?;
        c.commit().await?;
        rows.iter().map(tag_of).collect()
    }

    async fn nodes_with(&self, ctx: &Context, tag: TagId, page: PageRequest) -> Result<Page<Node>> {
        let after = page
            .cursor
            .as_ref()
            .map(node_rules::parse_key_cursor)
            .transpose()?;
        self.visible(ctx, tag).await?;
        let limit = page.limit() as usize;
        let mut c = begin_tenant(&self.pool, ctx.tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {} from nodes n join node_tags nt on nt.tenant_id = n.tenant_id \
             and nt.node_id = n.id where n.tenant_id = $1 and nt.tag_id = $2 \
             and n.trashed_at is null and ($3::uuid is null or n.id > $3) \
             order by n.id limit $4",
            crate::nodes::postgres::COLUMNS
        )))
        .bind(ctx.tenant.0)
        .bind(tag.0)
        .bind(after.map(|(_, i)| i))
        .bind((limit + 1) as i64)
        .fetch_all(&mut *c)
        .await?;
        c.commit().await?;
        let more = rows.len() > limit;
        let nodes = rows
            .iter()
            .take(limit)
            .map(|r| Ok(node_of(r)?.node))
            .collect::<Result<Vec<Node>>>()?;
        let next_cursor = if more {
            nodes.last().map(|n| node_rules::key_cursor("", n.id.0))
        } else {
            None
        };
        let items = node_rules::visible(&*self.authz, &*self.nodes, ctx, nodes, Role::View).await?;
        Ok(Page { items, next_cursor })
    }
}
