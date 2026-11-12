//! The conformance suite of [`AppStore`]: the memory store and the Postgres store pass the same
//! cases (`tests` below runs both; the Postgres run needs `DATABASE_URL`).

use super::*;
use crate::actor::Context;
use crate::ids::UserId;

/// What a case needs from a backend.
#[async_trait]
pub(crate) trait Harness: Send + Sync {
    fn store(&self) -> &dyn AppStore;
    /// A new tenant and a user of it.
    async fn tenant(&self) -> (TenantId, UserId);
    /// A new web app node in the tenant (the typed node rows that `apps` refers to).
    async fn app_node(&self, tenant: TenantId, user: UserId) -> NodeId;
}

fn ctx(tenant: TenantId, user: UserId) -> Context {
    Context {
        tenant,
        actor: Actor::user(user),
        scope: None,
        idempotency_key: None,
    }
}

fn app(node: NodeId, slug: &str) -> NewApp {
    NewApp {
        node_id: node,
        slug: slug.into(),
        settings: AppSettings::default(),
    }
}

fn deployment(app: NodeId, env: Environment, state: DeploymentState) -> NewDeployment {
    NewDeployment {
        id: Uuid::now_v7(),
        app_node_id: app,
        environment: env,
        state,
        short_id: super::super::slugs::short_id(),
        label: Some("plant list".into()),
        draft: None,
        source: DeploymentSource {
            kind: "file".into(),
            node_id: Some(NodeId::new()),
            version_id: Some(VersionId::new()),
            files: None,
        },
        archive: Archive {
            content_hash: [7; 32],
            size: 10,
            chunks: vec![1, 2, 3],
        },
        manifest: Some(serde_json::json!({ "runtime": "static" })),
        stages: serde_json::json!([]),
        published_from: None,
        expires_at: None,
    }
}

/// A unique slug per run (the Postgres runs of one database share the global indexes).
fn unique(base: &str) -> String {
    format!("{base}-{}", &Uuid::now_v7().simple().to_string()[20..])
}

/// Tenant slugs are unique across tenants and made once; approvals start off.
pub(crate) async fn tenant_slugs_span_tenants(h: &dyn Harness) {
    let s = h.store();
    let (a, _) = h.tenant().await;
    let (b, _) = h.tenant().await;
    let garden = unique("garden");
    let other = unique("garden");
    assert!(s.tenant_settings(a).await.unwrap().is_none());
    let ta = s
        .ensure_tenant_settings(a, std::slice::from_ref(&garden))
        .await
        .unwrap();
    assert_eq!(ta.slug, garden);
    assert!(!ta.public_apps_approved && !ta.custom_domains_approved);
    // B cannot take A's slug; it gets the next candidate.
    let tb = s
        .ensure_tenant_settings(b, &[garden.clone(), other.clone()])
        .await
        .unwrap();
    assert_eq!(tb.slug, other);
    // Made once: a second call keeps the slug.
    let again = s.ensure_tenant_settings(a, &[unique("x")]).await.unwrap();
    assert_eq!(again.slug, garden);
    // Every candidate taken: conflict, nothing made.
    let (c, _) = h.tenant().await;
    let e = s
        .ensure_tenant_settings(c, &[garden.clone(), other.clone()])
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 409);
    assert!(s.tenant_settings(c).await.unwrap().is_none());
    // Approvals: per tenant.
    let t = s
        .set_approval(
            a,
            &[],
            ApprovalChange {
                public_apps: Some(true),
                custom_domains: Some(true),
                by: "admin@ruum.test".into(),
            },
        )
        .await
        .unwrap();
    assert!(t.public_apps_approved && t.custom_domains_approved);
    assert!(
        !s.tenant_settings(b)
            .await
            .unwrap()
            .unwrap()
            .public_apps_approved
    );
    let t = s
        .set_approval(
            a,
            &[],
            ApprovalChange {
                public_apps: Some(false),
                custom_domains: None,
                by: "admin@ruum.test".into(),
            },
        )
        .await
        .unwrap();
    assert!(!t.public_apps_approved && t.custom_domains_approved);
}

/// App rows belong to their tenant: another tenant never reads, changes or removes them, and has
/// its own slug space.
pub(crate) async fn apps_stay_in_their_tenant(h: &dyn Harness) {
    let s = h.store();
    let (a, ua) = h.tenant().await;
    let (b, ub) = h.tenant().await;
    let na = h.app_node(a, ua).await;
    let nb = h.app_node(b, ub).await;
    let row = s
        .insert_app(&ctx(a, ua), app(na, "planner"))
        .await
        .unwrap()
        .expect("the slug is free");
    assert_eq!(
        (row.tenant_id, row.node_id, row.slug.as_str()),
        (a, na, "planner")
    );
    assert_eq!(row.settings, AppSettings::default());
    assert!(row.live_deployment_id.is_none() && row.slug_fixed_at.is_none());
    assert_eq!(s.get_app(a, na).await.unwrap().unwrap(), row);
    assert!(s.get_app(b, na).await.unwrap().is_none());
    // The app slug space is per tenant: B has its own `planner`.
    assert_eq!(
        s.slug_state(b, "planner", nb).await.unwrap(),
        SlugState::Free
    );
    assert!(
        s.insert_app(&ctx(b, ub), app(nb, "planner"))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(s.count_apps(a).await.unwrap(), 1);
    assert_eq!(s.count_apps(b).await.unwrap(), 1);
    // B cannot change A's settings.
    let changed = AppSettings {
        raw_copy_days: 7,
        ..AppSettings::default()
    };
    let data = settings_changed_data(&["raw_copy_days"], &changed);
    let e = s
        .update_settings(&ctx(b, ub), na, &changed, data.clone())
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 404);
    assert_eq!(
        s.get_app(a, na)
            .await
            .unwrap()
            .unwrap()
            .settings
            .raw_copy_days,
        0
    );
    let (after, change) = s
        .update_settings(&ctx(a, ua), na, &changed, data)
        .await
        .unwrap();
    assert_eq!(after.settings.raw_copy_days, 7);
    assert!(change.seq.is_some(), "the change has a journal entry");
    assert_eq!(change.actor, Actor::user(ua));
    // B's removal of A's node changes nothing.
    s.remove_app(b, na).await.unwrap();
    assert!(s.get_app(a, na).await.unwrap().is_some());
    // A deployment of A is not reachable from B.
    let d = s
        .insert_deployment(
            &ctx(a, ua),
            deployment(na, Environment::Preview, DeploymentState::Ready),
        )
        .await
        .unwrap();
    assert!(s.get_deployment(b, na, d.id).await.unwrap().is_none());
    assert!(s.get_deployment(b, nb, d.id).await.unwrap().is_none());
    assert!(
        s.list_deployments(b, na, None, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        !s.set_deployment_state(b, d.id, DeploymentState::Failed)
            .await
            .unwrap()
    );
    assert_eq!(
        s.get_deployment(a, na, d.id).await.unwrap().unwrap().state,
        DeploymentState::Ready
    );
    // B cannot deploy to A's app.
    let e = s
        .insert_deployment(
            &ctx(b, ub),
            deployment(na, Environment::Preview, DeploymentState::Pending),
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 404);
}

/// A slug given once is never given to another app of the tenant, also after that app is gone;
/// the same app node gets it back.
pub(crate) async fn used_slugs_are_never_given_again(h: &dyn Harness) {
    let s = h.store();
    let (a, ua) = h.tenant().await;
    let x = h.app_node(a, ua).await;
    let y = h.app_node(a, ua).await;
    s.insert_app(&ctx(a, ua), app(x, "planner"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        s.slug_state(a, "planner", y).await.unwrap(),
        SlugState::InUse(x)
    );
    assert_eq!(
        s.slug_state(a, "planner", x).await.unwrap(),
        SlugState::Free
    );
    // Taken now: nothing is written.
    assert!(
        s.insert_app(&ctx(a, ua), app(y, "planner"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(s.get_app(a, y).await.unwrap().is_none());
    // X is purged: its slug stays used.
    s.insert_deployment(
        &ctx(a, ua),
        deployment(x, Environment::Live, DeploymentState::Live),
    )
    .await
    .unwrap();
    s.remove_app(a, x).await.unwrap();
    assert!(s.get_app(a, x).await.unwrap().is_none());
    assert!(
        s.list_deployments(a, x, None, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        s.slug_state(a, "planner", y).await.unwrap(),
        SlugState::Retired
    );
    assert!(
        s.insert_app(&ctx(a, ua), app(y, "planner"))
            .await
            .unwrap()
            .is_none()
    );
    // The same node may have it again (a restore).
    assert_eq!(
        s.slug_state(a, "planner", x).await.unwrap(),
        SlugState::Free
    );
    // Another slug is fine.
    assert!(
        s.insert_app(&ctx(a, ua), app(y, "planner-2"))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(s.count_apps(a).await.unwrap(), 1);
}

/// Deployments: newest first, the environment filter, paging, and a release that becomes the
/// live deployment with a version and fixes the slug.
pub(crate) async fn deployments_list_and_releases(h: &dyn Harness) {
    let s = h.store();
    let (a, ua) = h.tenant().await;
    let n = h.app_node(a, ua).await;
    let c = ctx(a, ua);
    s.insert_app(&c, app(n, "planner")).await.unwrap().unwrap();
    let p1 = s
        .insert_deployment(
            &c,
            deployment(n, Environment::Preview, DeploymentState::Pending),
        )
        .await
        .unwrap();
    assert!(p1.version_id.is_none() && p1.published_at.is_none());
    assert_eq!(p1.created_by, Actor::user(ua));
    let l1 = s
        .insert_deployment(&c, deployment(n, Environment::Live, DeploymentState::Live))
        .await
        .unwrap();
    assert!(l1.version_id.is_some() && l1.published_at.is_some());
    let row = s.get_app(a, n).await.unwrap().unwrap();
    assert_eq!(row.live_deployment_id, Some(l1.id));
    assert!(row.slug_fixed_at.is_some());
    let l2 = s
        .insert_deployment(&c, deployment(n, Environment::Live, DeploymentState::Live))
        .await
        .unwrap();
    assert_ne!(l1.version_id, l2.version_id);
    assert_eq!(
        s.get_app(a, n).await.unwrap().unwrap().live_deployment_id,
        Some(l2.id)
    );
    assert_eq!(
        s.get_deployment(a, n, l1.id).await.unwrap().unwrap().state,
        DeploymentState::Superseded
    );
    let all = s
        .list_deployments(a, n, None, PageRequest::default())
        .await
        .unwrap();
    let ids: Vec<Uuid> = all.items.iter().map(|d| d.id).collect();
    assert_eq!(ids, [l2.id, l1.id, p1.id]);
    assert!(all.next_cursor.is_none());
    let live = s
        .list_deployments(a, n, Some(Environment::Live), PageRequest::default())
        .await
        .unwrap();
    assert_eq!(live.items.len(), 2);
    let first = s
        .list_deployments(
            a,
            n,
            None,
            PageRequest {
                cursor: None,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    let rest = s
        .list_deployments(
            a,
            n,
            None,
            PageRequest {
                cursor: first.next_cursor.clone(),
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(rest.items.iter().map(|d| d.id).collect::<Vec<_>>(), [p1.id]);
    assert!(rest.next_cursor.is_none());
    // A deployment is read under its own app only.
    let other = h.app_node(a, ua).await;
    assert!(s.get_deployment(a, other, p1.id).await.unwrap().is_none());
    // The state changes.
    assert!(
        s.set_deployment_state(a, p1.id, DeploymentState::Running)
            .await
            .unwrap()
    );
    let got = s.get_deployment(a, n, p1.id).await.unwrap().unwrap();
    assert_eq!(got.state, DeploymentState::Running);
    assert_eq!(got.source, p1.source);
    assert_eq!(got.archive, p1.archive);
    assert_eq!(got.manifest, p1.manifest);
    // A bad cursor is a bad request.
    let e = s
        .list_deployments(
            a,
            n,
            None,
            PageRequest {
                cursor: Some(Cursor("nope".into())),
                limit: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 400);
}

/// Runs every case.
pub(crate) async fn run_all(h: &dyn Harness) {
    tenant_slugs_span_tenants(h).await;
    apps_stay_in_their_tenant(h).await;
    used_slugs_are_never_given_again(h).await;
    deployments_list_and_releases(h).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::store::memory::MemoryAppStore;
    use crate::apps::store::postgres::PgAppStore;
    use crate::journal::memory::MemoryJournal;
    use std::sync::Arc;

    struct Mem(MemoryAppStore);

    #[async_trait]
    impl Harness for Mem {
        fn store(&self) -> &dyn AppStore {
            &self.0
        }
        async fn tenant(&self) -> (TenantId, UserId) {
            (TenantId::new(), UserId::new())
        }
        async fn app_node(&self, _: TenantId, _: UserId) -> NodeId {
            NodeId::new()
        }
    }

    #[tokio::test]
    async fn memory_store_conforms() {
        run_all(&Mem(MemoryAppStore::new(Some(Arc::new(
            MemoryJournal::default(),
        )))))
        .await;
    }

    pub(crate) struct Pg {
        pub pool: sqlx::PgPool,
        pub store: PgAppStore,
    }

    /// A tenant with a root folder and a user, written as the system.
    pub(crate) async fn pg_tenant(pool: &sqlx::PgPool) -> (TenantId, UserId) {
        let (t, u, root) = (TenantId::new(), UserId::new(), NodeId::new());
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
        sqlx::query(
            "insert into nodes (id, tenant_id, parent_id, name, kind, owner_id, created_kind, \
             created_by, updated_kind, updated_by) \
             values ($1, $2, null, 'root', 'folder', $3, 'user', $3, 'user', $3)",
        )
        .bind(root)
        .bind(t)
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (t, u)
    }

    /// A typed `webapp` node under the tenant root, with its manifest version and its
    /// `typed_nodes` row, written as the system.
    pub(crate) async fn pg_app_node(pool: &sqlx::PgPool, t: TenantId, u: UserId) -> NodeId {
        let (node, v) = (NodeId::new(), VersionId::new());
        let (bytes, hash) = crate::nodes::typed_manifest_bytes(&serde_json::json!({}));
        let mut tx = pool.begin().await.unwrap();
        for sql in [
            "set local ruum.system = 'on'",
            "set constraints all deferred",
        ] {
            sqlx::query(sql).execute(&mut *tx).await.unwrap();
        }
        sqlx::query(
            "insert into nodes (id, tenant_id, parent_id, name, kind, type, head_version, \
             owner_id, created_kind, created_by, updated_kind, updated_by) \
             select $1, $2, root_node_id, $3, 'typed', 'webapp', $4, $5, 'user', $5, 'user', $5 \
             from tenants where id = $2",
        )
        .bind(node)
        .bind(t)
        .bind(node.to_string())
        .bind(v)
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
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
                provider: crate::apps::PROVIDER_ID.into(),
                type_name: crate::apps::NODE_TYPE.into(),
                desired_version: v,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        node
    }

    #[async_trait]
    impl Harness for Pg {
        fn store(&self) -> &dyn AppStore {
            &self.store
        }
        async fn tenant(&self) -> (TenantId, UserId) {
            pg_tenant(&self.pool).await
        }
        async fn app_node(&self, t: TenantId, u: UserId) -> NodeId {
            pg_app_node(&self.pool, t, u).await
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store_conforms(pool: sqlx::PgPool) {
        let h = Pg {
            store: PgAppStore::new(pool.clone()),
            pool,
        };
        run_all(&h).await;
    }

    /// The Postgres store keeps every row in its tenant with row-level security off, so only
    /// the tenant filters of its statements hold (the mutation `apps-tenant-filter-postgres`
    /// drops one).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store_filters_by_tenant_without_rls(pool: sqlx::PgPool) {
        for table in [
            "tenant_app_settings",
            "apps",
            "retired_app_slugs",
            "app_deployments",
            "typed_nodes",
            "versions",
            "nodes",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "alter table {table} no force row level security"
            )))
            .execute(&pool)
            .await
            .unwrap();
        }
        let h = Pg {
            store: PgAppStore::new(pool.clone()),
            pool,
        };
        apps_stay_in_their_tenant(&h).await;
        used_slugs_are_never_given_again(&h).await;
    }
}
