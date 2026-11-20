//! The Postgres [`AppStore`] (tables `tenant_app_settings`, `apps`, `retired_app_slugs`,
//! `app_deployments` and, for releases, `versions` of migrations 0001 and 0011). Every statement
//! filters by `tenant_id` and runs in a transaction of the tenant (forced row-level security).
//! The tenant slug indexes span tenants; a clash there is a unique violation, never a read of
//! another tenant's row.

use super::*;
use crate::journal::postgres::{append, begin_tenant};
use crate::journal::{EventType, NewEntry};
use sqlx::Row;
use sqlx::postgres::PgRow;

/// The Postgres store.
#[derive(Clone, Debug)]
pub struct PgAppStore {
    pool: sqlx::PgPool,
    /// The pool of the live-switch locks, apart from `pool` (Kimi W-2s run 11, 3): a holder
    /// keeps one of these connections while it does its release work on `pool`, so the locks
    /// never use up the connections that the holders (or other tenants) need. Made on first
    /// use, with the connect options of `pool`.
    locks: std::sync::Arc<std::sync::OnceLock<sqlx::PgPool>>,
    /// The pool of the site locks (backlog W2S-1), apart from `pool` and `locks` for the same
    /// reason: a publish holds its live switch while it takes the site lock.
    site_locks: std::sync::Arc<std::sync::OnceLock<sqlx::PgPool>>,
}

/// At most this many site locks are held at the same time in one process (the deployments
/// that write a site, the publishes that copy an index, the garbage collection).
pub(crate) const SITE_LOCK_CONNECTIONS: u32 = 8;

/// At most this many live switches run at the same time in one process (each holds one
/// connection of the lock pool); more wait, up to the wait of the switch, then get
/// `live_switch_busy`.
pub(crate) const LIVE_SWITCH_CONNECTIONS: u32 = 4;

impl PgAppStore {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self {
            pool,
            locks: std::sync::Arc::default(),
            site_locks: std::sync::Arc::default(),
        }
    }

    fn site_lock_pool(&self) -> &sqlx::PgPool {
        self.site_locks.get_or_init(|| {
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(SITE_LOCK_CONNECTIONS)
                .min_connections(0)
                .idle_timeout(std::time::Duration::from_secs(60))
                .connect_lazy_with((*self.pool.connect_options()).clone())
        })
    }

    fn lock_pool(&self) -> &sqlx::PgPool {
        self.locks.get_or_init(|| {
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(LIVE_SWITCH_CONNECTIONS)
                .min_connections(0)
                .idle_timeout(std::time::Duration::from_secs(60))
                .connect_lazy_with((*self.pool.connect_options()).clone())
        })
    }
}

/// The name of the unique constraint that a statement broke, if it broke one.
fn unique_violation(e: &sqlx::Error) -> Option<String> {
    match e {
        sqlx::Error::Database(d) if d.code().as_deref() == Some("23505") => {
            Some(d.constraint().unwrap_or_default().to_string())
        }
        _ => None,
    }
}

const APP_COLUMNS: &str = "tenant_id, node_id, slug, settings, live_deployment_id, \
    slug_fixed_at, created_at, updated_at";

fn app_row(r: &PgRow) -> Result<AppRow> {
    let settings: Value = r.try_get("settings")?;
    Ok(AppRow {
        tenant_id: r.try_get("tenant_id")?,
        node_id: r.try_get("node_id")?,
        slug: r.try_get("slug")?,
        settings: AppSettings::from_stored(&settings),
        live_deployment_id: r.try_get("live_deployment_id")?,
        slug_fixed_at: r.try_get("slug_fixed_at")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

const DEPLOYMENT_COLUMNS: &str = "tenant_id, id, app_node_id, environment::text as environment, \
    state::text as state, short_id, version_id, label, draft, source_kind, source_node_id, \
    source_version_id, source_files, archive_hash, archive_size, archive_chunks, manifest, stages, \
    check_result, published_from, approval_id, created_kind, created_by, created_obo, created_at, \
    published_at, expires_at";

fn deployment_row(r: &PgRow) -> Result<DeploymentRow> {
    let env: String = r.try_get("environment")?;
    let state: String = r.try_get("state")?;
    let hash: Vec<u8> = r.try_get("archive_hash")?;
    let files: Option<Value> = r.try_get("source_files")?;
    Ok(DeploymentRow {
        tenant_id: r.try_get("tenant_id")?,
        id: r.try_get("id")?,
        app_node_id: r.try_get("app_node_id")?,
        environment: Environment::parse(&env)
            .ok_or_else(|| Error::Internal(format!("unknown environment {env}")))?,
        state: DeploymentState::parse(&state)
            .ok_or_else(|| Error::Internal(format!("unknown deployment state {state}")))?,
        short_id: r.try_get("short_id")?,
        version_id: r.try_get("version_id")?,
        label: r.try_get("label")?,
        draft: r.try_get("draft")?,
        source: DeploymentSource {
            kind: r.try_get("source_kind")?,
            node_id: r.try_get("source_node_id")?,
            version_id: r.try_get("source_version_id")?,
            files: files
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| Error::Internal(format!("source_files: {e}")))?,
        },
        archive: Archive {
            content_hash: hash
                .try_into()
                .map_err(|_| Error::Internal("an archive hash is not 32 bytes".into()))?,
            size: r.try_get("archive_size")?,
            chunks: r.try_get("archive_chunks")?,
        },
        manifest: r.try_get("manifest")?,
        stages: r.try_get("stages")?,
        check_result: r.try_get("check_result")?,
        published_from: r.try_get("published_from")?,
        approval_id: r.try_get("approval_id")?,
        created_by: Actor {
            actor: crate::actor::PrincipalRef {
                kind: r.try_get("created_kind")?,
                id: crate::ids::PrincipalId(r.try_get("created_by")?),
            },
            on_behalf_of: r
                .try_get::<Option<Uuid>, _>("created_obo")?
                .map(crate::ids::UserId),
        },
        created_at: r.try_get("created_at")?,
        published_at: r.try_get("published_at")?,
        expires_at: r.try_get("expires_at")?,
    })
}

async fn read_tenant(
    conn: &mut sqlx::PgConnection,
    tenant: TenantId,
) -> Result<Option<TenantAppSettings>> {
    let row = sqlx::query(
        "select slug, public_apps_approved_at is not null as public_apps, \
         custom_domains_approved_at is not null as custom_domains \
         from tenant_app_settings where tenant_id = $1",
    )
    .bind(tenant)
    .fetch_optional(&mut *conn)
    .await?;
    row.map(|r| {
        Ok(TenantAppSettings {
            tenant_id: tenant,
            slug: r.try_get("slug")?,
            public_apps_approved: r.try_get("public_apps")?,
            custom_domains_approved: r.try_get("custom_domains")?,
        })
    })
    .transpose()
}

/// Inserts the row of a deployment (in the caller's transaction).
async fn insert_row(
    conn: &mut sqlx::PgConnection,
    ctx: &Context,
    d: &NewDeployment,
    version: Option<VersionId>,
    now: OffsetDateTime,
) -> Result<DeploymentRow> {
    crate::blobs::postgres::record_chunks(&mut *conn, ctx.tenant, &d.new_chunks).await?;
    let files = d
        .source
        .files
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|e| Error::Internal(format!("source_files: {e}")))?;
    let release = d.is_release();
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "insert into app_deployments (tenant_id, id, app_node_id, environment, state, \
         short_id, version_id, label, draft, source_kind, source_node_id, source_version_id, \
         source_files, archive_hash, archive_size, archive_chunks, manifest, stages, \
         published_from, created_kind, created_by, created_obo, created_at, published_at, \
         expires_at) values ($1, $2, $3, $4::text::app_environment, \
         $5::text::app_deployment_state, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
         $17, $18, $19, $20, $21, $22, $23, $24, $25) returning {DEPLOYMENT_COLUMNS}"
    )))
    .bind(ctx.tenant)
    .bind(d.id)
    .bind(d.app_node_id)
    .bind(d.environment.as_str())
    .bind(d.state.as_str())
    .bind(&d.short_id)
    .bind(version)
    .bind(d.label.as_deref())
    .bind(d.draft.as_deref())
    .bind(&d.source.kind)
    .bind(d.source.node_id)
    .bind(d.source.version_id)
    .bind(files)
    .bind(d.archive.content_hash.to_vec())
    .bind(d.archive.size)
    .bind(&d.archive.chunks)
    .bind(&d.manifest)
    .bind(&d.stages)
    .bind(d.published_from)
    .bind(ctx.actor.actor.kind)
    .bind(ctx.actor.actor.id.0)
    .bind(ctx.actor.on_behalf_of.map(|u| u.0))
    .bind(now)
    .bind(release.then_some(now))
    .bind(d.expires_at)
    .fetch_one(&mut *conn)
    .await;
    match row {
        Ok(r) => deployment_row(&r),
        Err(e) if unique_violation(&e).is_some() => Err(Error::api(
            crate::error::ErrorCode::Conflict,
            "the deployment exists",
        )),
        Err(e) => Err(e.into()),
    }
}

/// Inserts a release version (encoding `app_release_v1`) in the caller's transaction.
async fn insert_version(conn: &mut sqlx::PgConnection, v: &Version, bytes: &[u8]) -> Result<()> {
    sqlx::query(
        "insert into versions (id, tenant_id, node_id, manifest, encoding, size_bytes, \
         content_hash, content_type, restored_from, label, actor_kind, actor, on_behalf_of, \
         created_at, updated_at) values ($1, $2, $3, $4, 'app_release_v1', $5, $6, $7, $8, $9, \
         $10, $11, $12, $13, $13)",
    )
    .bind(v.id)
    .bind(v.tenant_id)
    .bind(v.node_id)
    .bind(bytes)
    .bind(v.size_bytes)
    .bind(v.content_hash.0.to_vec())
    .bind(v.content_type.as_deref())
    .bind(v.restored_from)
    .bind(v.label.as_deref())
    .bind(v.actor.actor.kind)
    .bind(v.actor.actor.id.0)
    .bind(v.actor.on_behalf_of.map(|u| u.0))
    .bind(v.created_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Makes `id` the live deployment of `app` (the old one `superseded`) and fixes the slug.
async fn switch_live(
    conn: &mut sqlx::PgConnection,
    tenant: TenantId,
    app: NodeId,
    id: Uuid,
    previous: Option<Uuid>,
    now: OffsetDateTime,
) -> Result<()> {
    if let Some(old) = previous {
        sqlx::query(
            "update app_deployments set state = 'superseded' where tenant_id = $1 and id = $2",
        )
        .bind(tenant)
        .bind(old)
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query(
        "update apps set live_deployment_id = $3, slug_fixed_at = coalesce(slug_fixed_at, $4), \
         updated_at = $4 where tenant_id = $1 and node_id = $2",
    )
    .bind(tenant)
    .bind(app)
    .bind(id)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A version row of `versions` (the columns of [`VERSION_COLUMNS`]).
const VERSION_COLUMNS: &str = "id, size_bytes, content_hash, content_type, restored_from, label, \
    actor_kind, actor, on_behalf_of, created_at";

fn version_row(tenant: TenantId, node: NodeId, r: &PgRow) -> Result<Version> {
    let hash: Vec<u8> = r.try_get("content_hash")?;
    let hash: [u8; 32] = hash
        .try_into()
        .map_err(|_| Error::Internal("a stored hash is not 32 bytes".into()))?;
    Ok(Version {
        id: r.try_get("id")?,
        tenant_id: tenant,
        node_id: node,
        size_bytes: r.try_get("size_bytes")?,
        content_hash: crate::blobs::Hash(hash),
        content_type: r.try_get("content_type")?,
        restored_from: r.try_get("restored_from")?,
        label: r.try_get("label")?,
        actor: Actor {
            actor: crate::actor::PrincipalRef {
                kind: r.try_get("actor_kind")?,
                id: crate::ids::PrincipalId(r.try_get("actor")?),
            },
            on_behalf_of: r
                .try_get::<Option<Uuid>, _>("on_behalf_of")?
                .map(crate::ids::UserId),
        },
        created_at: r.try_get("created_at")?,
    })
}

const APPROVAL_COLUMNS: &str = "tenant_id, id, app_node_id, deployment_id, status::text as status, \
    changes, bound_archive_hash, bound_manifest_hash, bound_bindings, requested_kind, \
    requested_by, requested_obo, decided_by, decided_at, comment, expires_at, created_at";

fn approval_row(r: &PgRow) -> Result<ApprovalRow> {
    let h32 = |v: Vec<u8>| -> Result<[u8; 32]> {
        v.try_into()
            .map_err(|_| Error::Internal("a stored hash is not 32 bytes".into()))
    };
    Ok(ApprovalRow {
        tenant_id: r.try_get("tenant_id")?,
        id: r.try_get("id")?,
        app_node_id: r.try_get("app_node_id")?,
        deployment_id: r.try_get("deployment_id")?,
        status: r.try_get("status")?,
        changes: r.try_get("changes")?,
        bound_archive_hash: h32(r.try_get("bound_archive_hash")?)?,
        bound_manifest_hash: h32(r.try_get("bound_manifest_hash")?)?,
        bound_bindings: r.try_get("bound_bindings")?,
        requested_by: Actor {
            actor: crate::actor::PrincipalRef {
                kind: r.try_get("requested_kind")?,
                id: crate::ids::PrincipalId(r.try_get("requested_by")?),
            },
            on_behalf_of: r
                .try_get::<Option<Uuid>, _>("requested_obo")?
                .map(crate::ids::UserId),
        },
        decided_by: r.try_get("decided_by")?,
        decided_at: r.try_get("decided_at")?,
        comment: r.try_get("comment")?,
        expires_at: r.try_get("expires_at")?,
        created_at: r.try_get("created_at")?,
    })
}

async fn begin_system(pool: &sqlx::PgPool) -> Result<sqlx::Transaction<'static, sqlx::Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("select set_config('ruum.system', 'on', true)")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// Appends the entries in the caller's transaction; the change of the first.
async fn append_all(
    conn: &mut sqlx::PgConnection,
    ctx: &Context,
    node: NodeId,
    entries: Vec<(EventType, Value)>,
    at: OffsetDateTime,
) -> Result<ChangeInfo> {
    let mut first = None;
    for (event, data) in entries {
        let e = append(
            conn,
            ctx,
            NewEntry {
                node_id: Some(node),
                revision: None,
                event,
                data,
            },
        )
        .await?;
        first.get_or_insert(ChangeInfo {
            seq: Some(e.seq),
            actor: ctx.actor,
            at: e.at,
        });
    }
    Ok(first.unwrap_or(ChangeInfo {
        seq: None,
        actor: ctx.actor,
        at,
    }))
}

/// The settings row of the tenant, made with the first free candidate when missing. Each try
/// runs under a savepoint, so a clash with another tenant's slug (a unique violation on the
/// global indexes) leaves the transaction usable.
async fn ensure_in(
    conn: &mut sqlx::PgConnection,
    tenant: TenantId,
    candidates: &[String],
) -> Result<TenantAppSettings> {
    if let Some(t) = read_tenant(conn, tenant).await? {
        return Ok(t);
    }
    for slug in candidates {
        sqlx::query("savepoint ruum_tenant_slug")
            .execute(&mut *conn)
            .await?;
        let tried = async {
            sqlx::query("insert into tenant_app_settings (tenant_id, slug) values ($1, $2)")
                .bind(tenant)
                .bind(slug)
                .execute(&mut *conn)
                .await?;
            sqlx::query(
                "insert into retired_app_slugs (tenant_id, kind, slug) values ($1, 'tenant', $2)",
            )
            .bind(tenant)
            .bind(slug)
            .execute(&mut *conn)
            .await?;
            Ok::<(), sqlx::Error>(())
        }
        .await;
        match tried {
            Ok(()) => {
                sqlx::query("release savepoint ruum_tenant_slug")
                    .execute(&mut *conn)
                    .await?;
                return read_tenant(conn, tenant)
                    .await?
                    .ok_or_else(|| Error::Internal("tenant_app_settings row is missing".into()));
            }
            Err(e) => match unique_violation(&e) {
                Some(constraint) => {
                    sqlx::query("rollback to savepoint ruum_tenant_slug")
                        .execute(&mut *conn)
                        .await?;
                    if constraint == "tenant_app_settings_pkey" {
                        // Another request made the row first.
                        if let Some(t) = read_tenant(conn, tenant).await? {
                            return Ok(t);
                        }
                    }
                }
                None => return Err(e.into()),
            },
        }
    }
    Err(no_free_slug())
}

/// The advisory lock key of the site lock of a tenant: 8 bytes of a keyed hash of the tenant
/// (a clash only makes two tenants wait for each other).
fn site_lock_key(tenant: TenantId) -> i64 {
    let mut h = blake3::Hasher::new();
    h.update(b"ruum.apps.static_sites.v1");
    h.update(tenant.0.as_bytes());
    let b = h.finalize();
    let mut k = [0u8; 8];
    k.copy_from_slice(&b.as_bytes()[..8]);
    i64::from_be_bytes(k)
}

/// The advisory lock key of the live switch of one app: 8 bytes of a keyed hash of the tenant
/// and the app (a clash only makes two apps wait for each other).
fn live_switch_key(tenant: TenantId, app: NodeId) -> i64 {
    let mut h = blake3::Hasher::new();
    h.update(b"ruum.apps.live_switch.v1");
    h.update(tenant.0.as_bytes());
    h.update(app.0.as_bytes());
    let b = h.finalize();
    let mut k = [0u8; 8];
    k.copy_from_slice(&b.as_bytes()[..8]);
    i64::from_be_bytes(k)
}

#[async_trait]
impl AppStore for PgAppStore {
    async fn tenant_settings(&self, tenant: TenantId) -> Result<Option<TenantAppSettings>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let t = read_tenant(&mut tx, tenant).await?;
        tx.commit().await?;
        Ok(t)
    }

    async fn ensure_tenant_settings(
        &self,
        tenant: TenantId,
        candidates: &[String],
    ) -> Result<TenantAppSettings> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let t = ensure_in(&mut tx, tenant, candidates).await?;
        tx.commit().await?;
        Ok(t)
    }

    async fn set_approval(
        &self,
        tenant: TenantId,
        candidates: &[String],
        change: ApprovalChange,
    ) -> Result<TenantAppSettings> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        ensure_in(&mut tx, tenant, candidates).await?;
        if let Some(p) = change.public_apps {
            sqlx::query(
                "update tenant_app_settings set \
                 public_apps_approved_at = case when $2 then coalesce(public_apps_approved_at, now()) end, \
                 public_apps_approved_by = case when $2 then $3 end, updated_at = now() \
                 where tenant_id = $1",
            )
            .bind(tenant)
            .bind(p)
            .bind(&change.by)
            .execute(&mut *tx)
            .await?;
        }
        if let Some(d) = change.custom_domains {
            sqlx::query(
                "update tenant_app_settings set \
                 custom_domains_approved_at = case when $2 then coalesce(custom_domains_approved_at, now()) end, \
                 custom_domains_approved_by = case when $2 then $3 end, updated_at = now() \
                 where tenant_id = $1",
            )
            .bind(tenant)
            .bind(d)
            .bind(&change.by)
            .execute(&mut *tx)
            .await?;
        }
        let t = read_tenant(&mut tx, tenant)
            .await?
            .ok_or_else(|| Error::Internal("tenant_app_settings row is missing".into()))?;
        tx.commit().await?;
        Ok(t)
    }

    async fn slug_state(&self, tenant: TenantId, slug: &str, node: NodeId) -> Result<SlugState> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        // `gone`: the app's node is a purge tombstone (in the trash with the tenant root as
        // its trash root, until the provider takes `purged`) or no longer exists. Its row still
        // holds the slug, but the slug is retired at once (review W-2s, 1).
        let holder: Option<(NodeId, bool)> = sqlx::query_as(
            "select a.node_id, not exists (select 1 from nodes n where n.tenant_id = $1 \
             and n.id = a.node_id and not exists (select 1 from nodes r where r.tenant_id = $1 \
             and r.id = n.trash_root and r.parent_id is null)) \
             from apps a where a.tenant_id = $1 and a.slug = $2 and a.node_id <> $3",
        )
        .bind(tenant)
        .bind(slug)
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?;
        let used_by: Option<Option<NodeId>> = sqlx::query_scalar(
            "select app_node_id from retired_app_slugs \
             where tenant_id = $1 and kind = 'app' and slug = $2",
        )
        .bind(tenant)
        .bind(slug)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(match (holder, used_by.flatten()) {
            (Some((_, true)), _) => SlugState::Retired,
            (Some((n, false)), _) => SlugState::InUse(n),
            (None, Some(n)) if n != node => SlugState::Retired,
            _ => SlugState::Free,
        })
    }

    async fn insert_app(&self, ctx: &Context, app: NewApp) -> Result<Option<AppRow>> {
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let settings = serde_json::to_value(app.settings)
            .map_err(|e| Error::Internal(format!("settings: {e}")))?;
        let now = crate::now();
        let inserted = sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into apps (tenant_id, node_id, slug, settings, access, created_at, updated_at) \
             values ($1, $2, $3, $4, $5, $6, $6) returning {APP_COLUMNS}"
        )))
        .bind(ctx.tenant)
        .bind(app.node_id)
        .bind(&app.slug)
        .bind(&settings)
        .bind(app.settings.access.as_str())
        .bind(now)
        .fetch_one(&mut *tx)
        .await;
        let row = match inserted {
            Ok(r) => app_row(&r)?,
            Err(e) => {
                return match unique_violation(&e).as_deref() {
                    Some("apps_tenant_id_slug_key") => Ok(None),
                    Some(_) => Err(Error::api(
                        crate::error::ErrorCode::Conflict,
                        "the app row exists",
                    )),
                    None => Err(e.into()),
                };
            }
        };
        sqlx::query(
            "insert into retired_app_slugs (tenant_id, kind, slug, app_node_id) \
             values ($1, 'app', $2, $3) \
             on conflict (tenant_id, slug) where kind = 'app' do nothing",
        )
        .bind(ctx.tenant)
        .bind(&app.slug)
        .bind(app.node_id)
        .execute(&mut *tx)
        .await?;
        let owner: Option<NodeId> = sqlx::query_scalar(
            "select app_node_id from retired_app_slugs \
             where tenant_id = $1 and kind = 'app' and slug = $2",
        )
        .bind(ctx.tenant)
        .bind(&app.slug)
        .fetch_one(&mut *tx)
        .await?;
        if owner != Some(app.node_id) {
            // Another app had this slug: never given again (the transaction rolls back).
            return Ok(None);
        }
        tx.commit().await?;
        Ok(Some(row))
    }

    async fn get_app(&self, tenant: TenantId, node: NodeId) -> Result<Option<AppRow>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {APP_COLUMNS} from apps where tenant_id = $1 and node_id = $2"
        )))
        .bind(tenant)
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(app_row).transpose()
    }

    async fn count_apps(&self, tenant: TenantId) -> Result<u32> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let n: i64 = sqlx::query_scalar("select count(*) from apps where tenant_id = $1")
            .bind(tenant)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(n.max(0) as u32)
    }

    async fn update_settings(
        &self,
        ctx: &Context,
        node: NodeId,
        expected: &AppSettings,
        settings: &AppSettings,
        data: Value,
    ) -> Result<Option<(AppRow, ChangeInfo)>> {
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        // Lock the row and compare: a write based on an old copy writes nothing.
        let current: Value = sqlx::query_scalar(
            "select settings from apps where tenant_id = $1 and node_id = $2 for update",
        )
        .bind(ctx.tenant)
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("app"))?;
        if AppSettings::from_stored(&current) != *expected {
            tx.rollback().await?;
            return Ok(None);
        }
        let stored = serde_json::to_value(settings)
            .map_err(|e| Error::Internal(format!("settings: {e}")))?;
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update apps set settings = $3, access = $4, updated_at = now() \
             where tenant_id = $1 and node_id = $2 returning {APP_COLUMNS}"
        )))
        .bind(ctx.tenant)
        .bind(node)
        .bind(&stored)
        .bind(settings.access.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("app"))?;
        let row = app_row(&row)?;
        let e = append(
            &mut tx,
            ctx,
            NewEntry {
                node_id: Some(node),
                revision: None,
                event: EventType::AppSettingsChanged,
                data,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(Some((
            row,
            ChangeInfo {
                seq: Some(e.seq),
                actor: ctx.actor,
                at: e.at,
            },
        )))
    }

    async fn remove_app(&self, tenant: TenantId, node: NodeId) -> Result<()> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        for stmt in [
            "update apps set live_deployment_id = null where tenant_id = $1 and node_id = $2",
            // An approval names its deployment (no cascade): it goes first, or a pending
            // approval of an agent would block the purge for ever (review W-2s, Kimi 2).
            "delete from app_approvals where tenant_id = $1 and app_node_id = $2",
            "delete from app_deployments where tenant_id = $1 and app_node_id = $2",
            "delete from apps where tenant_id = $1 and node_id = $2",
            "delete from app_purge_progress where tenant_id = $1 and app_node_id = $2",
        ] {
            sqlx::query(stmt)
                .bind(tenant)
                .bind(node)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn insert_deployment(&self, ctx: &Context, d: NewDeployment) -> Result<DeploymentRow> {
        let tenant = ctx.tenant;
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let previous: Option<Option<Uuid>> = sqlx::query_scalar(
            "select live_deployment_id from apps where tenant_id = $1 and node_id = $2 for update",
        )
        .bind(tenant)
        .bind(d.app_node_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(previous) = previous else {
            return Err(Error::not_found("app"));
        };
        let now = crate::now();
        let release = d.is_release().then(|| release_version(ctx, &d, None, now));
        if let Some((v, bytes)) = &release {
            insert_version(&mut tx, v, bytes).await?;
        }
        let row = insert_row(&mut tx, ctx, &d, release.as_ref().map(|(v, _)| v.id), now).await?;
        if release.is_some() {
            switch_live(&mut tx, tenant, d.app_node_id, d.id, previous, now).await?;
        }
        tx.commit().await?;
        Ok(row)
    }

    async fn get_deployment(
        &self,
        tenant: TenantId,
        node: NodeId,
        id: Uuid,
    ) -> Result<Option<DeploymentRow>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {DEPLOYMENT_COLUMNS} from app_deployments \
             where tenant_id = $1 and app_node_id = $2 and id = $3"
        )))
        .bind(tenant)
        .bind(node)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(deployment_row).transpose()
    }

    async fn list_deployments(
        &self,
        tenant: TenantId,
        node: NodeId,
        environment: Option<Environment>,
        page: PageRequest,
    ) -> Result<Page<DeploymentRow>> {
        let after = page
            .cursor
            .as_ref()
            .map(decode_deployment_cursor)
            .transpose()?;
        let limit = page.limit() as usize;
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {DEPLOYMENT_COLUMNS} from app_deployments \
             where tenant_id = $1 and app_node_id = $2 \
             and ($3::text is null or environment::text = $3) \
             and ($4::timestamptz is null or (created_at, id) < ($4, $5)) \
             order by created_at desc, id desc limit $6"
        )))
        .bind(tenant)
        .bind(node)
        .bind(environment.map(Environment::as_str))
        .bind(after.map(|(at, _)| at))
        .bind(after.map(|(_, id)| id))
        .bind(limit as i64 + 1)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let items = rows
            .iter()
            .map(deployment_row)
            .collect::<Result<Vec<_>>>()?;
        Ok(page_of(items, limit))
    }

    async fn set_deployment_state(
        &self,
        tenant: TenantId,
        id: Uuid,
        state: DeploymentState,
    ) -> Result<bool> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let done = sqlx::query(
            "update app_deployments set state = $3::text::app_deployment_state \
             where tenant_id = $1 and id = $2",
        )
        .bind(tenant)
        .bind(id)
        .bind(state.as_str())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(done.rows_affected() == 1)
    }

    async fn update_stages(&self, tenant: TenantId, id: Uuid, stages: &Value) -> Result<bool> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let done = sqlx::query(
            "update app_deployments set state = 'running', stages = $3 \
             where tenant_id = $1 and id = $2 and state in ('pending', 'running')",
        )
        .bind(tenant)
        .bind(id)
        .bind(stages)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(done.rows_affected() == 1)
    }

    async fn finish_preview(
        &self,
        ctx: &Context,
        id: Uuid,
        stages: &Value,
        check: Option<&Value>,
    ) -> Result<DeploymentState> {
        let tenant = ctx.tenant;
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let me = sqlx::query(
            "select app_node_id, draft, created_at from app_deployments where tenant_id = $1 \
             and id = $2 and environment = 'preview' and state in ('pending', 'running') \
             for update",
        )
        .bind(tenant)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("deployment"))?;
        let app: NodeId = me.try_get("app_node_id")?;
        let draft: Option<String> = me.try_get("draft")?;
        let created: OffsetDateTime = me.try_get("created_at")?;
        // The app row orders the finishes of one app (two previews of one draft).
        sqlx::query("select 1 from apps where tenant_id = $1 and node_id = $2 for update")
            .bind(tenant)
            .bind(app)
            .execute(&mut *tx)
            .await?;
        let mut state = DeploymentState::Ready;
        if let Some(draft) = &draft {
            let newer: bool = sqlx::query_scalar(
                "select exists (select 1 from app_deployments where tenant_id = $1 \
                 and app_node_id = $2 and environment = 'preview' and state = 'ready' \
                 and draft = $3 and id <> $4 and (created_at, id) > ($5, $4))",
            )
            .bind(tenant)
            .bind(app)
            .bind(draft)
            .bind(id)
            .bind(created)
            .fetch_one(&mut *tx)
            .await?;
            if newer {
                state = DeploymentState::Superseded;
            }
            sqlx::query(
                "update app_deployments set state = 'superseded' where tenant_id = $1 \
                 and app_node_id = $2 and environment = 'preview' and state = 'ready' \
                 and draft = $3 and id <> $4 and (created_at, id) < ($5, $4)",
            )
            .bind(tenant)
            .bind(app)
            .bind(draft)
            .bind(id)
            .bind(created)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "update app_deployments set state = $3::text::app_deployment_state, stages = $4, \
             check_result = $5 where tenant_id = $1 and id = $2",
        )
        .bind(tenant)
        .bind(id)
        .bind(state.as_str())
        .bind(stages)
        .bind(check)
        .execute(&mut *tx)
        .await?;
        if state == DeploymentState::Ready {
            append_all(
                &mut tx,
                ctx,
                app,
                vec![(EventType::AppDeployed, preview_deployed_data(id))],
                crate::now(),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(state)
    }

    async fn fail_deployment(
        &self,
        ctx: &Context,
        id: Uuid,
        stages: &Value,
        check: Option<&Value>,
        stage: &str,
        reason: &str,
    ) -> Result<()> {
        let tenant = ctx.tenant;
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(
            "update app_deployments set state = 'failed', stages = $3, check_result = $4 \
             where tenant_id = $1 and id = $2 returning app_node_id, environment::text as env",
        )
        .bind(tenant)
        .bind(id)
        .bind(stages)
        .bind(check)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("deployment"))?;
        let app: NodeId = row.try_get("app_node_id")?;
        let env: String = row.try_get("env")?;
        let env = Environment::parse(&env).unwrap_or(Environment::Preview);
        append_all(
            &mut tx,
            ctx,
            app,
            vec![(
                EventType::AppDeploymentFailed,
                deployment_failed_data(id, env, stage, reason),
            )],
            crate::now(),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn release(&self, ctx: &Context, d: NewDeployment, r: Release) -> Result<Released> {
        if !d.is_release() {
            return Err(Error::Internal(
                "release() of a deployment that is not live".into(),
            ));
        }
        let tenant = ctx.tenant;
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let live: Option<Option<Uuid>> = sqlx::query_scalar(
            "select live_deployment_id from apps where tenant_id = $1 and node_id = $2 for update",
        )
        .bind(tenant)
        .bind(d.app_node_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(live) = live else {
            return Err(Error::not_found("app"));
        };
        if live != r.expected_live {
            return Err(live_changed());
        }
        let previous = match live {
            Some(p) => {
                let v: Option<Option<Uuid>> = sqlx::query_scalar(
                    "select version_id from app_deployments where tenant_id = $1 and id = $2",
                )
                .bind(tenant)
                .bind(p)
                .fetch_optional(&mut *tx)
                .await?;
                Some((p, v.flatten().map(VersionId)))
            }
            None => None,
        };
        let now = crate::now();
        let (version, bytes) = release_version(ctx, &d, r.restored_from, now);
        insert_version(&mut tx, &version, &bytes).await?;
        let row = insert_row(&mut tx, ctx, &d, Some(version.id), now).await?;
        switch_live(&mut tx, tenant, d.app_node_id, d.id, live, now).await?;
        let entries = release_entries(&d, &version, previous, &r);
        let change = append_all(&mut tx, ctx, d.app_node_id, entries, now).await?;
        tx.commit().await?;
        Ok(Released {
            deployment: row,
            version,
            change,
        })
    }

    async fn lock_live_switch(
        &self,
        tenant: TenantId,
        app: NodeId,
        wait: std::time::Duration,
    ) -> Result<LiveSwitchLock> {
        // A transaction-level advisory lock, held by a transaction that stays open until the
        // guard drops (a dropped transaction rolls back and frees the lock). The transaction is
        // on the lock pool, not on the store pool, so a holder never waits for a store
        // connection that another holder keeps. `try` in a loop: a waiting switch holds no
        // connection. The whole acquisition (the connection and the lock) is inside `wait`.
        let key = live_switch_key(tenant, app);
        let deadline = tokio::time::Instant::now() + wait;
        let busy = crate::apps::deployments::live_switch_busy;
        loop {
            let mut tx = tokio::time::timeout_at(deadline, self.lock_pool().begin())
                .await
                .map_err(|_| busy())??;
            let got: bool = tokio::time::timeout_at(
                deadline,
                sqlx::query_scalar("select pg_try_advisory_xact_lock($1)")
                    .bind(key)
                    .fetch_one(&mut *tx),
            )
            .await
            .map_err(|_| busy())??;
            if got {
                return Ok(LiveSwitchLock::held(Box::new(tx)));
            }
            tokio::time::timeout_at(deadline, tx.rollback())
                .await
                .map_err(|_| busy())??;
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(busy());
            }
            tokio::time::sleep_until(deadline.min(now + std::time::Duration::from_millis(50)))
                .await;
        }
    }

    async fn insert_approval(&self, ctx: &Context, a: NewApproval) -> Result<ApprovalRow> {
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into app_approvals (tenant_id, id, app_node_id, deployment_id, changes, \
             bound_archive_hash, bound_manifest_hash, bound_bindings, requested_kind, \
             requested_by, requested_obo, expires_at) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
             returning {APPROVAL_COLUMNS}"
        )))
        .bind(ctx.tenant)
        .bind(a.id)
        .bind(a.app_node_id)
        .bind(a.deployment_id)
        .bind(&a.changes)
        .bind(a.bound_archive_hash.to_vec())
        .bind(a.bound_manifest_hash.to_vec())
        .bind(&a.bound_bindings)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .bind(a.expires_at)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        approval_row(&row)
    }

    async fn list_versions(
        &self,
        tenant: TenantId,
        node: NodeId,
        filter: VersionFilter,
        page: PageRequest,
    ) -> Result<Page<Version>> {
        let after = page
            .cursor
            .as_ref()
            .map(crate::blobs::logic::decode_version_cursor)
            .transpose()?;
        let limit = page.limit() as usize;
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {VERSION_COLUMNS} from versions \
             where tenant_id = $1 and node_id = $2 and encoding = 'app_release_v1' \
             and ($3::principal_kind is null or actor_kind = $3) \
             and ($4::boolean is null or (label is not null) = $4) \
             and ($5::timestamptz is null or (created_at, id) < ($5, $6)) \
             order by created_at desc, id desc limit $7"
        )))
        .bind(tenant)
        .bind(node)
        .bind(filter.actor_kind)
        .bind(filter.labeled)
        .bind(after.map(|(at, _)| at))
        .bind(after.map(|(_, id)| id))
        .bind(limit as i64 + 1)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut items = rows
            .iter()
            .map(|r| version_row(tenant, node, r))
            .collect::<Result<Vec<_>>>()?;
        Ok(version_page(&mut items, limit))
    }

    async fn release_of(
        &self,
        tenant: TenantId,
        node: NodeId,
        version: VersionId,
    ) -> Result<Option<(Version, Uuid)>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {VERSION_COLUMNS}, (select d.id from app_deployments d \
             where d.tenant_id = v.tenant_id and d.version_id = v.id) as deployment_id \
             from versions v where v.tenant_id = $1 and v.node_id = $2 and v.id = $3 \
             and v.encoding = 'app_release_v1'"
        )))
        .bind(tenant)
        .bind(node)
        .bind(version)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let Some(r) = row else {
            return Ok(None);
        };
        let deployment: Option<Uuid> = r.try_get("deployment_id")?;
        match deployment {
            Some(d) => Ok(Some((version_row(tenant, node, &r)?, d))),
            None => Ok(None),
        }
    }

    async fn expire_previews(
        &self,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<ExpiredPreview>> {
        let mut tx = begin_system(&self.pool).await?;
        let rows = sqlx::query(
            "update app_deployments d set state = 'expired' from ( \
               select tenant_id, id from app_deployments \
               where environment = 'preview' and state = 'ready' and expires_at <= $1 \
               order by expires_at limit $2 for update skip locked) e \
             where d.tenant_id = e.tenant_id and d.id = e.id \
             returning d.tenant_id, d.app_node_id, d.id",
        )
        .bind(now)
        .bind(i64::from(limit))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter()
            .map(|r| {
                Ok(ExpiredPreview {
                    tenant_id: r.try_get("tenant_id")?,
                    app_node_id: r.try_get("app_node_id")?,
                    id: r.try_get("id")?,
                })
            })
            .collect()
    }

    async fn static_deployments(&self, tenant: TenantId) -> Result<Vec<StaticDeployment>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let rows = sqlx::query(
            "select id, app_node_id, (environment = 'live' \
             or state in ('pending', 'running', 'ready')) as reachable, \
             case when state in ('pending', 'running') then created_at end as in_flight_since \
             from app_deployments where tenant_id = $1",
        )
        .bind(tenant)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter()
            .map(|r| {
                Ok(StaticDeployment {
                    id: r.try_get("id")?,
                    app_node_id: r.try_get("app_node_id")?,
                    reachable: r.try_get("reachable")?,
                    in_flight_since: r.try_get("in_flight_since")?,
                })
            })
            .collect()
    }

    async fn app_deployment_ids_after(
        &self,
        tenant: TenantId,
        app: NodeId,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<Uuid>> {
        // A range scan of `app_deployments_app_id_idx` (migration 0015).
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "select id from app_deployments where tenant_id = $1 and app_node_id = $2 \
             and ($3::uuid is null or id > $3) order by id limit $4",
        )
        .bind(tenant)
        .bind(app)
        .bind(after)
        .bind(i64::from(limit))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ids)
    }

    async fn static_purge_cursor(&self, tenant: TenantId, app: NodeId) -> Result<Option<Uuid>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let after: Option<Uuid> = sqlx::query_scalar(
            "select static_after from app_purge_progress where tenant_id = $1 and app_node_id = $2",
        )
        .bind(tenant)
        .bind(app)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(after)
    }

    async fn set_static_purge_cursor(
        &self,
        tenant: TenantId,
        app: NodeId,
        after: Uuid,
    ) -> Result<()> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        sqlx::query(
            "insert into app_purge_progress (tenant_id, app_node_id, static_after) \
             values ($1, $2, $3) on conflict (tenant_id, app_node_id) \
             do update set static_after = excluded.static_after, updated_at = now()",
        )
        .bind(tenant)
        .bind(app)
        .bind(after)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn storage_usage(&self, tenant: TenantId) -> Result<StorageUsage> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let r = sqlx::query(
            "select (select coalesce(sum(size_bytes), 0)::bigint from nodes \
               where tenant_id = $1) as files, \
             (select count(*) from memberships where tenant_id = $1 and role <> 'guest') \
               as members",
        )
        .bind(tenant)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let members: i64 = r.try_get("members")?;
        Ok(StorageUsage {
            file_bytes: r.try_get("files")?,
            members: u32::try_from(members).unwrap_or(u32::MAX),
        })
    }

    async fn lock_static_sites(
        &self,
        tenant: TenantId,
        exclusive: bool,
        wait: std::time::Duration,
    ) -> Result<LiveSwitchLock> {
        // As `lock_live_switch`: a transaction-level advisory lock (shared for writers,
        // exclusive for the collection) on a pool of its own, tried in a loop so that a
        // waiting caller holds no connection.
        let key = site_lock_key(tenant);
        let sql = if exclusive {
            "select pg_try_advisory_xact_lock($1)"
        } else {
            "select pg_try_advisory_xact_lock_shared($1)"
        };
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let mut tx = tokio::time::timeout_at(deadline, self.site_lock_pool().begin())
                .await
                .map_err(|_| sites_busy())??;
            let got: bool = tokio::time::timeout_at(
                deadline,
                sqlx::query_scalar(sql).bind(key).fetch_one(&mut *tx),
            )
            .await
            .map_err(|_| sites_busy())??;
            if got {
                return Ok(LiveSwitchLock::held(Box::new(tx)));
            }
            tokio::time::timeout_at(deadline, tx.rollback())
                .await
                .map_err(|_| sites_busy())??;
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(sites_busy());
            }
            tokio::time::sleep_until(deadline.min(now + std::time::Duration::from_millis(50)))
                .await;
        }
    }

    async fn grant_bindings(
        &self,
        ctx: &Context,
        authz: &dyn crate::authz::Authz,
        rows: Vec<NewBinding>,
    ) -> Result<Vec<BindingRow>> {
        use crate::authz::RelationshipUpdate;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let mut made = Vec::new();
        for r in &rows {
            let inserted = sqlx::query(sqlx::AssertSqlSafe(format!(
                "insert into app_bindings (tenant_id, id, app_node_id, deployment_id, \
                 principal_id, name, kind, target_node_id, role, api, scopes, \
                 on_behalf_of_user, granted_kind, granted_by, granted_obo) \
                 values ($1, $2, $3, $4, $5, $6, $7::app_binding_kind, $8, $9, $10, $11, $12, \
                 $13, $14, $15) returning {}",
                binding_columns("")
            )))
            .bind(ctx.tenant)
            .bind(r.id)
            .bind(r.app_node_id)
            .bind(r.deployment_id)
            .bind(r.principal_id)
            .bind(&r.name)
            .bind(r.kind.as_str())
            .bind(r.target_node_id)
            .bind(r.role.map(role_text))
            .bind(&r.api)
            .bind(&r.scopes)
            .bind(r.on_behalf_of_user)
            .bind(ctx.actor.actor.kind)
            .bind(ctx.actor.actor.id.0)
            .bind(ctx.actor.on_behalf_of.map(|u| u.0))
            .fetch_one(&mut *tx)
            .await;
            let row = match inserted {
                Ok(row) => binding_row(&row)?,
                Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("23503") => {
                    return Err(Error::not_found("node"));
                }
                Err(e) => return Err(e.into()),
            };
            append(
                &mut tx,
                ctx,
                NewEntry {
                    node_id: Some(row.app_node_id),
                    revision: None,
                    event: EventType::AppBindingGranted,
                    data: binding_granted_data(&row),
                },
            )
            .await?;
            made.push(row);
        }
        let mut touches = Vec::new();
        for b in &made {
            for t in
                super::super::bindings::principal_tuples(ctx.tenant, b.app_node_id, b.principal_id)
                    .into_iter()
                    .chain(super::super::bindings::binding_tuple(b))
            {
                let u = RelationshipUpdate::Touch(t);
                if !touches.contains(&u) {
                    touches.push(u);
                }
            }
        }
        let ob = crate::authz::outbox::enqueue(&mut tx, ctx.tenant, &touches).await?;
        tx.commit().await?;
        self.apply_outbox(authz, ctx.tenant, ob).await;
        Ok(made)
    }

    async fn revoke_bindings(
        &self,
        ctx: &Context,
        authz: &dyn crate::authz::Authz,
        ids: &[Uuid],
        reason: RevokeReason,
    ) -> Result<Vec<BindingRow>> {
        use crate::authz::RelationshipUpdate;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update app_bindings set revoked_at = now(), revoke_reason = $3 \
             where tenant_id = $1 and id = any($2) and revoked_at is null returning {}",
            binding_columns("")
        )))
        .bind(ctx.tenant)
        .bind(ids)
        .bind(reason.as_str())
        .fetch_all(&mut *tx)
        .await?;
        let mut ended = rows.iter().map(binding_row).collect::<Result<Vec<_>>>()?;
        ended.sort_by_key(|b| b.id);
        let mut deletes = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for b in &ended {
            append(
                &mut tx,
                ctx,
                NewEntry {
                    node_id: Some(b.app_node_id),
                    revision: None,
                    event: EventType::AppBindingRevoked,
                    data: binding_revoked_data(b, reason),
                },
            )
            .await?;
            let Some(t) = super::super::bindings::binding_tuple(b) else {
                continue;
            };
            // A tuple goes only when no other active row of the principal implies it.
            let kept: bool = sqlx::query_scalar(
                "select exists (select 1 from app_bindings where tenant_id = $1 \
                 and principal_id = $2 and target_node_id = $3 and kind <> 'api' \
                 and role = $4 and revoked_at is null)",
            )
            .bind(ctx.tenant)
            .bind(b.principal_id)
            .bind(b.target_node_id)
            .bind(b.role.map(role_text))
            .fetch_one(&mut *tx)
            .await?;
            if !kept && seen.insert(t.clone()) {
                deletes.push(RelationshipUpdate::Delete(t));
            }
        }
        let ob = if deletes.is_empty() {
            None
        } else {
            Some(crate::authz::outbox::enqueue(&mut tx, ctx.tenant, &deletes).await?)
        };
        tx.commit().await?;
        if let Some(ob) = ob {
            self.apply_outbox(authz, ctx.tenant, ob).await;
        }
        Ok(ended)
    }

    async fn list_bindings(&self, tenant: TenantId, q: &BindingQuery) -> Result<Vec<BindingRow>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {} from app_bindings where tenant_id = $1 \
             and ($2::uuid is null or app_node_id = $2) \
             and ($3::uuid is null or principal_id = $3) \
             and ($4::uuid is null or deployment_id = $4) \
             and ($5::uuid is null or target_node_id = $5) \
             and (not $6 or revoked_at is null) \
             and ($8::uuid is null or id > $8) order by id limit $7",
            binding_columns("")
        )))
        .bind(tenant)
        .bind(q.app)
        .bind(q.principal)
        .bind(q.deployment)
        .bind(q.target)
        .bind(q.active_only)
        .bind(i64::from(q.limit))
        .bind(q.after)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(binding_row).collect()
    }

    async fn ended_preview_bindings(
        &self,
        tenant: TenantId,
        app: Option<NodeId>,
        limit: u32,
    ) -> Result<Vec<BindingRow>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {} from app_bindings b where b.tenant_id = $1 \
             and ($2::uuid is null or b.app_node_id = $2) and b.revoked_at is null \
             and b.principal_id <> b.app_node_id \
             and not exists (select 1 from app_deployments d where d.tenant_id = $1 \
               and d.id = b.principal_id and d.state in ('pending', 'running', 'ready')) \
             order by b.id limit $3",
            binding_columns("b.")
        )))
        .bind(tenant)
        .bind(app)
        .bind(i64::from(limit))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(binding_row).collect()
    }

    async fn binding_targets(
        &self,
        tenant: TenantId,
        after: Option<NodeId>,
        limit: u32,
    ) -> Result<Vec<NodeId>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let rows: Vec<NodeId> = sqlx::query_scalar(
            "select distinct target_node_id from app_bindings where tenant_id = $1 \
             and revoked_at is null \
             and ($2::uuid is null or target_node_id > $2) order by target_node_id limit $3",
        )
        .bind(tenant)
        .bind(after)
        .bind(i64::from(limit))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows)
    }

    async fn binding_tenants(&self, after: Option<TenantId>, limit: u32) -> Result<Vec<TenantId>> {
        let mut tx = begin_system(&self.pool).await?;
        let rows: Vec<TenantId> = sqlx::query_scalar(
            "select t.id from tenants t where ($1::uuid is null or t.id > $1) \
             and exists (select 1 from app_bindings b where b.tenant_id = t.id) \
             order by t.id limit $2",
        )
        .bind(after)
        .bind(i64::from(limit))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows)
    }

    async fn delete_app_bindings(
        &self,
        tenant: TenantId,
        authz: &dyn crate::authz::Authz,
        app: NodeId,
        limit: u32,
    ) -> Result<usize> {
        use crate::authz::RelationshipUpdate;
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "delete from app_bindings where tenant_id = $1 and id in (select id from \
             app_bindings where tenant_id = $1 and app_node_id = $2 order by id limit $3) \
             returning {}",
            binding_columns("")
        )))
        .bind(tenant)
        .bind(app)
        .bind(i64::from(limit))
        .fetch_all(&mut *tx)
        .await?;
        let gone = rows.iter().map(binding_row).collect::<Result<Vec<_>>>()?;
        let mut deletes = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for b in &gone {
            let tuples = super::super::bindings::principal_tuples(tenant, app, b.principal_id)
                .into_iter()
                .chain(
                    b.is_active()
                        .then(|| super::super::bindings::binding_tuple(b))
                        .flatten(),
                );
            for t in tuples {
                if seen.insert(t.clone()) {
                    deletes.push(RelationshipUpdate::Delete(t));
                }
            }
        }
        let ob = if deletes.is_empty() {
            None
        } else {
            Some(crate::authz::outbox::enqueue(&mut tx, tenant, &deletes).await?)
        };
        tx.commit().await?;
        if let Some(ob) = ob {
            self.apply_outbox(authz, tenant, ob).await;
        }
        Ok(gone.len())
    }
}

impl PgAppStore {
    /// Applies an outbox row of a binding write after its commit; a failure leaves the row to
    /// the outbox worker (the row and the tuple never stay apart for good).
    async fn apply_outbox(&self, authz: &dyn crate::authz::Authz, tenant: TenantId, id: i64) {
        if let Some(token) =
            crate::authz::outbox::apply_after_commit(&self.pool, authz, tenant, &[id]).await
        {
            crate::authz::raise_request_floor(&token);
        }
    }
}

/// The columns of `app_bindings`, each with `prefix` (`""` or `"b."`).
fn binding_columns(prefix: &str) -> String {
    [
        "tenant_id",
        "id",
        "app_node_id",
        "deployment_id",
        "principal_id",
        "name",
        "kind::text as kind",
        "target_node_id",
        "role",
        "api",
        "scopes",
        "on_behalf_of_user",
        "granted_kind",
        "granted_by",
        "granted_obo",
        "created_at",
        "revoked_at",
        "revoke_reason",
    ]
    .iter()
    .map(|c| format!("{prefix}{c}"))
    .collect::<Vec<_>>()
    .join(", ")
}

/// The stored text of a binding role.
fn role_text(r: crate::authz::Role) -> &'static str {
    match r {
        crate::authz::Role::View => "view",
        crate::authz::Role::Comment => "comment",
        crate::authz::Role::Edit => "edit",
        crate::authz::Role::Manage => "manage",
    }
}

fn binding_row(r: &PgRow) -> Result<BindingRow> {
    let kind: String = r.try_get("kind")?;
    let role: Option<String> = r.try_get("role")?;
    let reason: Option<String> = r.try_get("revoke_reason")?;
    let role = match role.as_deref() {
        None => None,
        Some("view") => Some(crate::authz::Role::View),
        Some("edit") => Some(crate::authz::Role::Edit),
        Some(other) => {
            return Err(Error::Internal(format!("a binding with the role {other}")));
        }
    };
    Ok(BindingRow {
        tenant_id: r.try_get("tenant_id")?,
        id: r.try_get("id")?,
        app_node_id: r.try_get("app_node_id")?,
        deployment_id: r.try_get("deployment_id")?,
        principal_id: r.try_get("principal_id")?,
        name: r.try_get("name")?,
        kind: BindingKind::parse(&kind)
            .ok_or_else(|| Error::Internal(format!("a binding of the kind {kind}")))?,
        target_node_id: r.try_get("target_node_id")?,
        role,
        api: r.try_get("api")?,
        scopes: r.try_get("scopes")?,
        on_behalf_of_user: r.try_get("on_behalf_of_user")?,
        granted_by: Actor {
            actor: crate::actor::PrincipalRef {
                kind: r.try_get("granted_kind")?,
                id: crate::ids::PrincipalId(r.try_get("granted_by")?),
            },
            on_behalf_of: r
                .try_get::<Option<Uuid>, _>("granted_obo")?
                .map(crate::ids::UserId),
        },
        created_at: r.try_get("created_at")?,
        revoked_at: r.try_get("revoked_at")?,
        revoke_reason: match reason {
            None => None,
            Some(s) => Some(
                RevokeReason::parse(&s)
                    .ok_or_else(|| Error::Internal(format!("a binding revoked for {s}")))?,
            ),
        },
    })
}
